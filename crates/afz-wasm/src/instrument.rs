//! Post-hoc coverage instrumentation for a wasm module.
//!
//! A probe compiled into the target would make coverage a property of how the artifact was built,
//! and the thing under test is the artifact a team already ships. So the harness rewrites the
//! module it is handed: one counter byte per structured block, written by three instructions into a
//! memory the harness owns.
//!
//! The counters live in a memory the instrumentation *adds*, appended after every memory the
//! module already declares. That keeps every existing memory index - each load, store, data
//! segment, and the module's own `memory` export - pointing at what it pointed at, and it keeps the
//! counters out of reach of the target's allocator, because they are a different memory rather than
//! an unused range of the target's. The host reads them as its own bytes.
//!
//! A block is a function entry or the head of a structured block: `block`, `loop`, `if`, `else`.
//! Branches land on a block head, so every path through the function passes a guard, and a guard
//! costs two `i32.const` and one `i32.store8` - no call, so an instrumented module stays fast enough
//! to be worth fuzzing.
//!
//! Only the module's own functions get counters. An artifact built from Rust also carries the panic
//! machinery, the allocator, and the compiler's abort trampoline; every artifact of that language
//! carries the same functions, and no exported entry point is one of them. Counting them would put
//! the runtime in the denominator of `coverageRatio`, so a scan that reached every branch of the
//! target would report a small ratio because it did not spend its budget inside `core::panicking`.
//! `plumbing_functions` is the rule that leaves them alone, and it is the wasm counterpart of the
//! JavaScript harness keeping files outside the scan project root out of its coverage.

use std::collections::{HashMap, HashSet};
use thiserror::Error;
use wasm_encoder::reencode::{Reencode as _, RoundtripReencoder};
use wasm_encoder::{
    CodeSection, CustomSection, DataCountSection, DataSection, Encode, ElementSection, ExportKind, ExportSection,
    FunctionSection, GlobalSection, ImportSection, Instruction, MemArg, MemArgTarget, MemorySection, MemoryType,
    Module, RawSection, StartSection, TableSection, TypeSection, ValType,
};
use wasmparser::{Encoding, Operator, Parser, Payload, Validator};

/// The export name the counters memory is given, so the runner can find it by name.
pub const COUNTERS: &str = "__afz_counters";

/// `i32.const <slot>`, `i32.const 1`, `i32.store8` into the counters memory. `i32.store8` rather
/// than a wider store because one byte answers "was this block executed", which is all a coverage
/// digest carries, and a byte keeps the region to one byte per block however many blocks the module
/// has.
fn guard(body: &mut Vec<u8>, counters_memory: u32, edge: u32) {
    Instruction::I32Const(i32::try_from(edge).unwrap_or(0)).encode(body);
    Instruction::I32Const(1).encode(body);
    Instruction::I32Store8(MemArg {
        offset: 0,
        align: 0,
        target: MemArgTarget::Memory(counters_memory),
        ordering: None,
    })
    .encode(body);
}

/// One instrumented block, so `coverageMap` can say what an edge id stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
    pub edge: u32,
    /// The function index whose body holds the guard.
    pub function: u32,
    /// Offset of the guard within that function body. A stripped wasm module carries no source
    /// positions, so the code offset is the only range there is to report.
    pub offset: u32,
    /// Bytes the guard occupies at that offset, so an edge names a range rather than a point.
    pub length: u32,
}

#[derive(Debug)]
pub struct Instrumented {
    pub bytes: Vec<u8>,
    pub blocks: Vec<Block>,
    /// Functions left uninstrumented because their names say they are the language's own plumbing.
    pub skipped_functions: usize,
    /// Names from the module's name section, for the functions that were instrumented, so
    /// `coverageMap` can say which function an edge belongs to.
    pub function_names: HashMap<u32, String>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("the module could not be read: {0}")]
    Wasm(#[from] wasmparser::BinaryReaderError),
    #[error("only plain wasm modules are instrumented; a nested module or component is not supported")]
    NotAModule,
    #[error("the module already exports `{COUNTERS}`, which the harness needs for its counters")]
    CountersNameTaken,
    #[error("the module could not be re-encoded: {0}")]
    Reencode(String),
    #[error("the instrumented module is not valid wasm: {0}")]
    Invalid(String),
}

type Mapped<T> = Result<T, wasm_encoder::reencode::Error<std::convert::Infallible>>;

trait Context {
    fn context(self) -> Result<(), Error>;
}

impl Context for Mapped<()> {
    fn context(self) -> Result<(), Error> {
        self.map_err(|e| Error::Reencode(e.to_string()))
    }
}

trait ContextValue<T> {
    fn value(self) -> Result<T, Error>;
}

impl<T> ContextValue<T> for Mapped<T> {
    fn value(self) -> Result<T, Error> {
        self.map_err(|e| Error::Reencode(e.to_string()))
    }
}

fn check(bytes: &[u8], input: bool) -> Result<(), Error> {
    match Validator::new().validate_all(bytes) {
        Ok(_) => Ok(()),
        Err(e) if input => Err(Error::Wasm(e)),
        Err(e) => Err(Error::Invalid(e.to_string())),
    }
}

/// The blocks instrumentation would guard: one per function entry plus one per structured block
/// head. Counted in a first pass because the counters memory's size has to be written into the
/// memory section, which comes before the code section. Functions in `skip` are excluded here for
/// the same reason they are excluded in the rewrite pass: the counters region and the coverage map
/// have to agree about how many guards there are.
fn count_blocks(bytes: &[u8], skip: &HashSet<u32>) -> Result<usize, Error> {
    let mut total = 0usize;
    let mut function = 0u32;
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::CodeSectionEntry(body) => {
                if !skip.contains(&function) {
                    total += 1;
                    for op in body.get_operators_reader()? {
                        if matches!(op?, Operator::Block { .. } | Operator::Loop { .. } | Operator::If { .. } | Operator::Else) {
                            total += 1;
                        }
                    }
                }
                function += 1;
            }
            Payload::ComponentSection { .. } | Payload::ModuleSection { .. } => return Err(Error::NotAModule),
            _ => {}
        }
    }
    Ok(total)
}

/// The name a Rust toolchain writes into a module's `name` section is a mangled symbol, as in
/// `_RNvNtCs4NIexwV1FGX_4core3fmt5write`, while the same module's backtrace prints it demangled. Both
/// the plumbing rule and `coverageMap` read the demangled form, so a map entry and a backtrace frame
/// name the same function the same way. A name that is not a mangled symbol - a `#[no_mangle]` export,
/// or anything a C toolchain emitted - comes back unchanged.
pub fn demangled(name: &str) -> String {
    rustc_demangle::try_demangle(name)
        .map(|symbol| symbol.to_string())
        .unwrap_or_else(|_| name.to_string())
}

/// True when a function is the language's plumbing rather than the target's code: the panic path, the
/// allocator behind it, the backtrace reader that the panic handler pulls in, and the shims the
/// compiler emits by hand. The crate is what precedes the first `::` and any hash a demangler attaches
/// to it, as in `core[37ee0ce0c4cd57d5]::panicking::panic_fmt`.
///
/// This is a denylist, so a crate that is not listed gets instrumented. The error it makes is counting
/// runtime blocks in the denominator, which is what a module with no names at all already does; it is
/// never the error of leaving the target's own code out of the map.
pub fn is_plumbing(name: &str) -> bool {
    let name = demangled(name);
    if name.starts_with("__rust")
        || name.starts_with("rust_begin_unwind")
        || name.starts_with("__abort")
        || name.starts_with("__eh_personality")
    {
        return true;
    }
    // A method of a generic type or a trait impl demangles to a qualified path, as in
    // `<dlmalloc::Dlmalloc<S>::malloc` or `<u32 as core::fmt::Display>::fmt`, so the crate can sit
    // behind an opening bracket, and behind ` as ` when the impl is of a foreign trait.
    let stripped = name.trim_start_matches(['<', '&', '*', ' ']);
    let head = stripped.split("::").next().unwrap_or(stripped);
    let head = head.rsplit(" as ").next().unwrap_or(head);
    let crate_name = head.split('[').next().unwrap_or(head);
    matches!(
        crate_name,
        "core"
            | "std"
            | "alloc"
            | "test"
            | "panic_unwind"
            | "panic_abort"
            | "unwind"
            | "std_detect"
            | "compiler_builtins"
            | "profiler_builtins"
            | "libc"
            | "libm"
            | "dlmalloc"
            | "hashbrown"
            | "rustc_demangle"
            | "rustc_std_workspace_alloc"
            | "rustc_std_workspace_core"
            | "rustc_std_workspace_std"
            | "addr2line"
            | "gimli"
            | "object"
            | "adler"
            | "adler2"
            | "miniz_oxide"
            | "memchr"
    )
}

/// The functions instrumentation leaves alone, as code-section indices, and the module's function
/// names. A module stripped of its name section has nothing to classify, so everything in it is
/// instrumented: the rule can only be as good as the names the artifact ships.
fn plumbing_functions(bytes: &[u8]) -> Result<(HashSet<u32>, HashMap<u32, String>), Error> {
    let mut imported = 0u32;
    let mut named: Vec<(u32, String)> = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::ImportSection(reader) => {
                for group in reader {
                    imported += match group? {
                        wasmparser::Imports::Single(_, import) => {
                            u32::from(matches!(import.ty, wasmparser::TypeRef::Func(_)))
                        }
                        wasmparser::Imports::Compact1 { items, .. } => {
                            let mut grouped = 0u32;
                            for item in items {
                                if matches!(item?.ty, wasmparser::TypeRef::Func(_)) {
                                    grouped += 1;
                                }
                            }
                            grouped
                        }
                        wasmparser::Imports::Compact2 { ty, names, .. } => {
                            if matches!(ty, wasmparser::TypeRef::Func(_)) {
                                names.count()
                            } else {
                                0
                            }
                        }
                    };
                }
            }
            // The names ride in a custom section, and a module whose names cannot be parsed is
            // still instrumented: the section only decides what to leave alone, so a parse failure
            // must not stop a scan.
            Payload::CustomSection(section) => {
                if let wasmparser::KnownCustom::Name(reader) = section.as_known() {
                    for record in reader.flatten() {
                        if let wasmparser::Name::Function(map) = record {
                            named.extend(map.flatten().map(|naming| (naming.index, naming.name.to_string())));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // The name section indexes the function index space, which begins with the imports; the code
    // section counts only its own entries, and that is the index a `Block` records.
    let mut skip = HashSet::new();
    let mut names = HashMap::new();
    for (index, name) in named {
        let Some(entry) = index.checked_sub(imported) else { continue };
        if is_plumbing(&name) {
            skip.insert(entry);
        } else {
            names.insert(entry, demangled(&name));
        }
    }
    Ok((skip, names))
}

/// What a first pass over the module has to answer before anything is encoded: how many memories
/// the module declares (imported and defined), which fixes the index the counters memory will get,
/// and whether the module already exports the name the harness needs.
fn scan_layout(bytes: &[u8]) -> Result<(u32, bool), Error> {
    let mut memories = 0u32;
    let mut counters_name_taken = false;
    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::ImportSection(reader) => {
                for group in reader {
                    memories += match group? {
                        wasmparser::Imports::Single(_, import) => {
                            u32::from(matches!(import.ty, wasmparser::TypeRef::Memory(_)))
                        }
                        wasmparser::Imports::Compact1 { items, .. } => {
                            let mut grouped = 0u32;
                            for item in items {
                                if matches!(item?.ty, wasmparser::TypeRef::Memory(_)) {
                                    grouped += 1;
                                }
                            }
                            grouped
                        }
                        wasmparser::Imports::Compact2 { ty, names, .. } => {
                            if matches!(ty, wasmparser::TypeRef::Memory(_)) {
                                names.count()
                            } else {
                                0
                            }
                        }
                    };
                }
            }
            Payload::MemorySection(reader) => memories += reader.count(),
            Payload::ExportSection(reader) => {
                for export in reader {
                    if export?.name == COUNTERS {
                        counters_name_taken = true;
                    }
                }
            }
            _ => {}
        }
    }
    Ok((memories, counters_name_taken))
}

fn counters_type(pages: u64) -> MemoryType {
    MemoryType {
        minimum: pages,
        // The counters never grow, so the maximum is the minimum: the module cannot be made to run
        // out of memory by a scan, and the host knows the region's size exactly.
        maximum: Some(pages),
        memory64: false,
        shared: false,
        page_size_log2: None,
    }
}

/// Add coverage counters to `bytes`.
pub fn instrument(bytes: &[u8]) -> Result<Instrumented, Error> {
    check(bytes, true)?;
    let (skip, names) = plumbing_functions(bytes)?;
    let guards = count_blocks(bytes, &skip)?.max(1);
    let pages = (guards as u64 + 65_535) / 65_536;
    let (declared, counters_name_taken) = scan_layout(bytes)?;
    if counters_name_taken {
        return Err(Error::CountersNameTaken);
    }
    // The code section is only complete once every body has been rewritten, and a data section has
    // to follow it, so the code section is emitted at the first payload that must come after it.
    let mut code_emitted = false;

    let mut re = RoundtripReencoder;
    let mut module = Module::new();
    let mut code = CodeSection::new();
    let mut blocks = Vec::new();
    // The counters memory is appended after every memory the module declares, so its index is the
    // number of memories the first pass counted.
    let memories = declared;
    let mut counters_emitted = false;
    let mut edge: u32 = 0;
    let mut function: u32 = 0;

    // A module with no memory section gets one synthesized at the first position that must follow
    // the memory section, because section order is fixed by the format.
    let emit_counters_section = |module: &mut Module, emitted: &mut bool| {
        if *emitted {
            return;
        }
        let mut section = MemorySection::new();
        section.memory(counters_type(pages));
        module.section(&section);
        *emitted = true;
    };

    for payload in Parser::new(0).parse_all(bytes) {
        match payload? {
            Payload::Version { encoding: Encoding::Module, .. } | Payload::End(_) => {}
            Payload::TypeSection(reader) => {
                let mut section = TypeSection::new();
                re.parse_type_section(&mut section, reader).context()?;
                module.section(&section);
            }
            Payload::ImportSection(reader) => {
                let mut section = ImportSection::new();
                re.parse_import_section(&mut section, reader).context()?;
                module.section(&section);
            }
            Payload::FunctionSection(reader) => {
                let mut section = FunctionSection::new();
                re.parse_function_section(&mut section, reader).context()?;
                module.section(&section);
            }
            Payload::TableSection(reader) => {
                let mut section = TableSection::new();
                re.parse_table_section(&mut section, reader).context()?;
                module.section(&section);
            }
            Payload::MemorySection(reader) => {
                let mut section = MemorySection::new();
                for memory in reader {
                    section.memory(re.memory_type(memory?).value()?);
                }
                section.memory(counters_type(pages));
                module.section(&section);
                counters_emitted = true;
            }
            Payload::GlobalSection(reader) => {
                emit_counters_section(&mut module, &mut counters_emitted);
                let mut section = GlobalSection::new();
                re.parse_global_section(&mut section, reader).context()?;
                module.section(&section);
            }
            Payload::ExportSection(reader) => {
                emit_counters_section(&mut module, &mut counters_emitted);
                let mut section = ExportSection::new();
                re.parse_export_section(&mut section, reader).context()?;
                section.export(COUNTERS, ExportKind::Memory, memories);
                module.section(&section);
            }
            Payload::StartSection { func, .. } => {
                emit_counters_section(&mut module, &mut counters_emitted);
                module.section(&StartSection { function_index: func });
            }
            Payload::ElementSection(reader) => {
                emit_counters_section(&mut module, &mut counters_emitted);
                let mut section = ElementSection::new();
                re.parse_element_section(&mut section, reader).context()?;
                module.section(&section);
            }
            Payload::DataCountSection { count, .. } => {
                emit_counters_section(&mut module, &mut counters_emitted);
                module.section(&DataCountSection { count });
            }
            Payload::DataSection(reader) => {
                emit_counters_section(&mut module, &mut counters_emitted);
                if !code_emitted {
                    module.section(&code);
                    code_emitted = true;
                }
                let mut section = DataSection::new();
                re.parse_data_section(&mut section, reader).context()?;
                module.section(&section);
            }
            Payload::CodeSectionStart { .. } => {
                emit_counters_section(&mut module, &mut counters_emitted);
            }
            Payload::CodeSectionEntry(body) => {
                emit_counters_section(&mut module, &mut counters_emitted);
                // Plumbing keeps its body byte for byte. A counter in the panic path would charge a
                // crash to coverage, and the abort trampoline runs once per crash whatever the input
                // was, so it can only ever report one edge as covered.
                if skip.contains(&function) {
                    code.raw(body.as_bytes());
                } else {
                    rewrite_function(&mut re, body, memories, function, &mut edge, &mut blocks, &mut code)?;
                }
                function += 1;
            }
            Payload::CustomSection(section) => {
                {
                    module.section(&CustomSection { name: section.name().to_string().into(), data: section.data().into() });
                }
            }
            Payload::UnknownSection { id, contents, .. } => {
                module.section(&RawSection { id, data: contents });
            }
            _ => return Err(Error::NotAModule),
        }
    }

    emit_counters_section(&mut module, &mut counters_emitted);
    if !code_emitted {
        module.section(&code);
    }
    let bytes = module.finish();
    check(&bytes, false)?;
    Ok(Instrumented { bytes, blocks, skipped_functions: skip.len(), function_names: names })
}

/// Copy one function body, inserting a guard at every block head.
///
/// The body is assembled as bytes and handed to the code section raw, because the encoder's
/// instruction sink exposes a builder method per instruction and no way to append an instruction
/// that was mapped from the input. Everything else about the body goes through the reencoder, so an
/// instruction the harness has never heard of survives the rewrite unchanged.
fn rewrite_function(
    re: &mut RoundtripReencoder,
    body: wasmparser::FunctionBody<'_>,
    counters_memory: u32,
    function: u32,
    edge: &mut u32,
    blocks: &mut Vec<Block>,
    code: &mut CodeSection,
) -> Result<(), Error> {
    let mut locals: Vec<(u32, ValType)> = Vec::new();
    for entry in body.get_locals_reader()? {
        let (count, ty) = entry?;
        locals.push((count, re.val_type(ty).value()?));
    }

    let mut out: Vec<u8> = Vec::new();
    (locals.len() as u32).encode(&mut out);
    for (count, ty) in &locals {
        count.encode(&mut out);
        ty.encode(&mut out);
    }

    let mut hit = |out: &mut Vec<u8>, edge: &mut u32| {
        let start = out.len() as u32;
        guard(out, counters_memory, *edge);
        blocks.push(Block { edge: *edge, function, offset: start, length: out.len() as u32 - start });
        *edge += 1;
    };
    hit(&mut out, edge);

    for op in body.get_operators_reader()? {
        let op = op?;
        let opens = matches!(op, Operator::Block { .. } | Operator::Loop { .. } | Operator::If { .. } | Operator::Else);
        re.instruction(op).value()?.encode(&mut out);
        if opens {
            hit(&mut out, edge);
        }
    }
    // The operators reader yields the body's terminating `end`, so the copy needs none of its own.
    code.raw(&out);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_encoder::{BlockType, ConstExpr, EntityType, Function, NameMap, NameSection, TypeSection};

    /// A module with two exported functions - one that calls the imported function, one with a
    /// structured block - plus a memory and a data segment: the shapes whose indices the rewrite
    /// must not disturb.
    fn sample() -> Vec<u8> {
        let mut module = Module::new();
        let mut types = TypeSection::new();
        types.ty().function([], [ValType::I32]);
        types.ty().function([ValType::I32], []);
        module.section(&types);

        let mut imports = ImportSection::new();
        imports.import("env", "log", EntityType::Function(0));
        module.section(&imports);

        let mut funcs = FunctionSection::new();
        funcs.function(0);
        funcs.function(0);
        module.section(&funcs);

        let mut memories = MemorySection::new();
        memories.memory(MemoryType { minimum: 1, maximum: None, memory64: false, shared: false, page_size_log2: None });
        module.section(&memories);

        let mut exports = ExportSection::new();
        exports.export("compute", ExportKind::Func, 1);
        exports.export("nested", ExportKind::Func, 2);
        exports.export("memory", ExportKind::Memory, 0);
        module.section(&exports);

        module.section(&DataCountSection { count: 1 });

        let mut code = CodeSection::new();
        let mut plain = Function::new([]);
        plain.instructions().call(0).end();
        code.function(&plain);

        let mut nested = Function::new([(1, ValType::I32)]);
        {
            let mut body = nested.instructions();
            body.block(BlockType::Empty).nop().end().i32_const(9).end();
        }
        code.function(&nested);
        module.section(&code);

        let mut data = DataSection::new();
        data.active(0, &ConstExpr::i32_const(8), vec![1, 2, 3, 4]);
        module.section(&data);
        module.finish()
    }

    #[test]
    fn every_block_head_gets_a_guard_and_the_result_validates() {
        let instrumented = instrument(&sample()).expect("the sample instruments");
        // One guard per function entry, plus one for the `block` head in the second function.
        let edges: Vec<u32> = instrumented.blocks.iter().map(|b| b.edge).collect();
        assert_eq!(edges, vec![0, 1, 2], "guards landed on {edges:?}");
        assert_eq!(instrumented.blocks.iter().map(|b| b.function).collect::<Vec<_>>(), vec![0, 1, 1]);
        assert!(instrumented.blocks.iter().all(|b| b.length > 0), "a guard was recorded as zero bytes");
        check(&instrumented.bytes, false).expect("the instrumented module validates");
    }

    #[test]
    fn a_guard_is_placed_in_the_counters_memory_and_the_target_keeps_its_own() {
        let instrumented = instrument(&sample()).expect("the sample instruments");
        let mut sizes = Vec::new();
        let mut counters_index = None;
        let mut target_memory_index = None;
        for payload in Parser::new(0).parse_all(&instrumented.bytes) {
            match payload.expect("the instrumented module parses") {
                Payload::MemorySection(section) => {
                    for memory in section {
                        sizes.push(memory.expect("memory type").initial);
                    }
                }
                Payload::ExportSection(section) => {
                    for export in section {
                        let export = export.expect("export");
                        if export.name == COUNTERS {
                            counters_index = Some(export.index);
                        }
                        if export.name == "memory" {
                            target_memory_index = Some(export.index);
                        }
                    }
                }
                _ => {}
            }
        }
        assert_eq!(sizes, vec![1, 1], "the target's memory kept its declared size and counters were appended");
        assert_eq!(target_memory_index, Some(0), "the target's memory kept index 0");
        assert_eq!(counters_index, Some(1), "the counters memory was appended after it");
    }

    #[test]
    fn a_module_that_already_uses_the_counters_name_is_refused() {
        let mut module = Module::new();
        let mut memories = MemorySection::new();
        memories.memory(counters_type(1));
        module.section(&memories);
        let mut exports = ExportSection::new();
        exports.export(COUNTERS, ExportKind::Memory, 0);
        module.section(&exports);
        let error = instrument(&module.finish()).expect_err("a name clash is refused");
        assert!(matches!(error, Error::CountersNameTaken), "got {error}");
    }

    #[test]
    fn a_module_without_any_memory_still_gets_counters() {
        let mut module = Module::new();
        let mut types = TypeSection::new();
        types.ty().function([], []);
        module.section(&types);
        let mut funcs = FunctionSection::new();
        funcs.function(0);
        module.section(&funcs);
        let mut code = CodeSection::new();
        let mut f = Function::new([]);
        f.instructions().nop().end();
        code.function(&f);
        module.section(&code);

        let instrumented = instrument(&module.finish()).expect("a memory-less module instruments");
        assert_eq!(instrumented.blocks.len(), 1);
        check(&instrumented.bytes, false).expect("the instrumented module validates");
    }

    /// Two functions with the names section a Rust toolchain ships: the plain symbol a `no_mangle`
    /// export carries, and one mangled `core` symbol. The first code-section function is index 1 in the
    /// function index space because `env.log` is imported, and that offset is what this test pins.
    fn named_module() -> Vec<u8> {
        let mut module = Module::new();
        let mut types = TypeSection::new();
        types.ty().function([], []);
        module.section(&types);

        let mut imports = ImportSection::new();
        imports.import("env", "log", EntityType::Function(0));
        module.section(&imports);

        let mut funcs = FunctionSection::new();
        funcs.function(0);
        funcs.function(0);
        module.section(&funcs);

        let mut exports = ExportSection::new();
        exports.export("probe", ExportKind::Func, 1);
        module.section(&exports);

        let mut code = CodeSection::new();
        let mut target = Function::new([]);
        target.instructions().block(BlockType::Empty).nop().end().end();
        code.function(&target);
        let mut runtime = Function::new([]);
        runtime.instructions().block(BlockType::Empty).nop().end().end();
        code.function(&runtime);
        module.section(&code);

        let mut names = NameMap::new();
        names.append(1, "probe");
        names.append(2, "_RNvNtCs4NIexwV1FGX_4core3fmt5write");
        let mut section = NameSection::new();
        section.functions(&names);
        module.section(&section);
        module.finish()
    }

    /// A function the language put there is not part of the target: it gets no counters, and it is
    /// not in the map a reader uses to work out what a coverage ratio was measured over.
    #[test]
    fn a_runtime_function_is_left_out_of_the_counters_and_the_map() {
        let instrumented = instrument(&named_module()).expect("the module instruments");
        assert_eq!(instrumented.skipped_functions, 1, "the panic function should have been recognised");
        assert_eq!(
            instrumented.blocks.iter().map(|b| b.function).collect::<Vec<_>>(),
            vec![0, 0],
            "counters belong to the target function alone: {:?}",
            instrumented.blocks,
        );
        assert_eq!(instrumented.function_names.get(&0).map(String::as_str), Some("probe"));
        assert!(!instrumented.function_names.contains_key(&1), "a plumbing function is not in the map");
        check(&instrumented.bytes, false).expect("the instrumented module validates");
    }

    /// The rule has to match what a toolchain actually emits. Every name here is copied from the probe
    /// module: mangled symbols as the `name` section carries them, demangled names as a backtrace
    /// prints them, and the plain symbol a `no_mangle` export carries.
    #[test]
    fn the_plumbing_rule_matches_emitted_names() {
        for name in [
            "_RNvCs5fGHjzWXRav_7___rustc12___rust_alloc",
            "_RNvNtCs4NIexwV1FGX_4core3fmt5write",
            "_RNvMs0_NtCsbw64Iw7W9Hn_8dlmalloc8dlmallocINtB5_8DlmallocNtNtB7_3sys6SystemE6mallocCsiuSwmoQr08c_3std",
            "__rustc[3d2f79520d6fa809]::__rust_abort",
            "std[d775520e4a0009e6]::panicking::panic_with_hook",
            "core[37ee0ce0c4cd57d5]::panicking::panic_const::panic_const_mul_overflow",
            "<u32 as core[37ee0ce0c4cd57d5]::fmt::Display>::fmt",
            "<&str as core[37ee0ce0c4cd57d5]::any::Any>::type_id",
            "__rust_alloc",
            "rust_begin_unwind",
        ] {
            assert!(is_plumbing(name), "{name} is the runtime's own code");
        }
        for name in ["probe", "wasm_probe::probe", "std_logic::parse", "core_utils::sum", ""] {
            assert!(!is_plumbing(name), "{name} is the target's own code");
        }
        // An impl of a runtime trait on the target's own type is the target's code: the impl block is
        // what the author wrote, and the generator that emits it is the crate that owns the type.
        assert!(!is_plumbing("<wasm_probe::Table as core[37ee0ce0c4cd57d5]::fmt::Debug>::fmt"));
        // A name that is not a mangled symbol is left as it is, so a C module's symbols are readable.
        assert_eq!(demangled("probe"), "probe");
        assert!(demangled("_RNvNtCs4NIexwV1FGX_4core3fmt5write").starts_with("core"));
    }

}

//! Running one instrumented module: instance, deadline, trap classification, coverage read.
//!
//! The three operations the design asks of a wasm runner are first-class wasmtime calls rather
//! than approximations. A deadline is epoch interruption: a ticker thread advances the engine's
//! epoch and the store stops the call at the next loop backedge, which is what lets a `loop` that
//! never returns still produce an outcome inside the plugin's own budget instead of leaving the
//! engine to kill the process. A trap is read from the error, not parsed from a message. Coverage
//! is a byte slice of a memory the host owns.
//!
//! Every invocation gets a fresh `Store` and a fresh instance, for two measured reasons. A store
//! whose epoch deadline has fired keeps its interrupt: wasmtime 50 exposes no way to clear it, so
//! the next thing done with that store - instantiating the module into it included - traps with
//! `interrupt`. One deadline finding therefore turned into a plugin that failed every later call.
//! And a trapped call can leave linear memory half-written with the guest's allocator metadata
//! among it. Instantiating a module this size costs microseconds next to the invocation it follows,
//! and a fresh instance also arrives with its counters region zeroed, so nothing needs resetting.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use afz_protocol::{FaultFrame, FaultSignature};
use wasmtime::{Config, Engine, ExternType, Func, Instance, Linker, Module, Store, Trap, Val, ValType};

use crate::instrument;

/// Epoch tick. The smallest deadline a scan configures is whole milliseconds, and a tick an order
/// of magnitude below it keeps the cut close to the deadline the engine asked for.
const TICK: Duration = Duration::from_millis(5);

/// How a call ended.
#[derive(Debug)]
pub enum Stopped {
    /// The export returned. The values are the wasm results, reported as they arrived.
    Returned(Vec<Val>),
    /// The epoch deadline cut the call. The target never returned, so the harness reports the
    /// duration and lets the deadline oracle decide: calling a slow call a trap would report one
    /// event as two findings.
    Interrupted,
    /// The export trapped. The signature carries the trap code as its class.
    Trapped(FaultSignature),
}

pub struct Executed {
    pub duration: Duration,
    pub edges: Vec<u32>,
    pub memory_bytes: u64,
    pub stopped: Stopped,
}

/// The host state. Nothing: a wasm export is a function of its arguments, the harness declares no
/// `stateful` and no `sideEffects` capability, and there is no receiver to keep alive between calls.
struct Host;

pub struct Runner {
    engine: Engine,
    module: Module,
    path: PathBuf,
    counters: usize,
}

impl Runner {
    /// Load the instrumented bytes. The ticker thread lives for the process: one plugin process is
    /// one worker, so one ticker drives every store that process creates.
    pub fn new(path: &Path, bytes: &[u8], blocks: &[instrument::Block]) -> Result<Runner, String> {
        let mut config = Config::new();
        // Epoch interruption is what cuts a call that runs past the deadline. Wasm backtraces are
        // collected by default, and that is where a fault's frames come from.
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| format!("could not build a wasm engine: {e}"))?;
        let ticker = engine.clone();
        std::thread::Builder::new()
            .name("afz-epoch".into())
            .spawn(move || loop {
                std::thread::sleep(TICK);
                ticker.increment_epoch();
            })
            .map_err(|e| format!("could not start the deadline ticker: {e}"))?;
        let module = Module::new(&engine, bytes).map_err(|e| format!("{}: could not compile: {e}", path.display()))?;
        // The counters region is one byte per guard, and its size is the number of guards the
        // instrumentation placed.
        let counters = blocks.len().max(1);
        Ok(Runner { engine, module, path: path.to_path_buf(), counters })
    }

    /// The module's exports, which is the whole inventory a wasm module can offer.
    pub fn exports(&self) -> impl Iterator<Item = (&str, ExternType)> {
        self.module.exports().map(|e| (e.name(), e.ty()))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Call one export. `deadline` bounds the call; the call is cut, not abandoned, so an
    /// invocation always produces an outcome inside the plugin's own budget.
    pub fn invoke(&mut self, export: &str, args: &[Val], deadline: Duration, collect_coverage: bool) -> Result<Executed, String> {
        let ticks = deadline.as_millis().saturating_sub(1) as u64 / TICK.as_millis() as u64 + 1;
        let mut call = Call::new(&self.engine, &self.module, &self.path, export, ticks.max(1), self.counters)?;

        let started = Instant::now();
        let called = call.call(args);
        let duration = started.elapsed();
        // The counters and the memory high-water are read after a trap too: the edges reached
        // before the trap are the ones a finding is reported for, and the high-water is what the
        // memory oracle was asked about.
        let edges = if collect_coverage { call.edges()? } else { Vec::new() };
        let memory_bytes = call.memory_bytes();
        drop(call);
        let stopped = match called {
            Ok(results) => Stopped::Returned(results),
            Err(error) => classify(&self.path, export, &error, duration, deadline),
        };
        Ok(Executed { duration, edges, memory_bytes, stopped })
    }
}

/// The wasm state one invocation owns: a store, one instance of it, and the export being called.
struct Call<'a> {
    store: Store<Host>,
    instance: Instance,
    func: Func,
    module: &'a Module,
    path: &'a Path,
    counters: usize,
}

impl<'a> Call<'a> {
    fn new(
        engine: &Engine,
        module: &'a Module,
        path: &'a Path,
        export: &str,
        ticks: u64,
        counters: usize,
    ) -> Result<Call<'a>, String> {
        let mut store = Store::new(engine, Host);
        store.set_epoch_deadline(ticks);
        let instance = Linker::new(engine)
            .instantiate(&mut store, module)
            .map_err(|e| format!("{}: could not instantiate: {e}", path.display()))?;
        let func = instance
            .get_func(&mut store, export)
            .ok_or_else(|| format!("{}: no export named {export}", path.display()))?;
        Ok(Call { store, instance, func, module, path, counters })
    }

    fn call(&mut self, args: &[Val]) -> Result<Vec<Val>, wasmtime::Error> {
        let mut results = vec![Val::I32(0); self.func.ty(&self.store).results().len()];
        self.func.call(&mut self.store, args, &mut results)?;
        Ok(results)
    }

    /// The counters memory the instrumentation appended, by the name it exported it under.
    fn counters_memory(&mut self) -> Result<wasmtime::Memory, String> {
        self.instance
            .get_memory(&mut self.store, instrument::COUNTERS)
            .ok_or_else(|| format!("{}: the counters memory export is missing", self.path.display()))
    }

    /// The edge ids whose counter byte is non-zero.
    fn edges(&mut self) -> Result<Vec<u32>, String> {
        let memory = self.counters_memory()?;
        let span = self.counters.min(memory.data_size(&self.store));
        let bytes = memory.data(&self.store)[..span].to_vec();
        Ok(bytes.iter().enumerate().filter(|(_, hit)| **hit != 0).map(|(edge, _)| edge as u32).collect())
    }

    /// Guest memory high-water, reported as `memoryBytes`. The counters region is excluded: it is
    /// the harness's, and counting it would make every invocation look like it allocated a page.
    fn memory_bytes(&mut self) -> u64 {
        let mut total = 0u64;
        for export in self.module.exports() {
            if !matches!(export.ty(), ExternType::Memory(_)) || export.name() == instrument::COUNTERS {
                continue;
            }
            let name = export.name().to_string();
            if let Some(memory) = self.instance.get_memory(&mut self.store, &name) {
                total += memory.data_size(&self.store) as u64;
            }
        }
        total
    }
}

/// Sort a failed call into "the deadline cut it" and "the target broke".
///
/// Epoch interruption raises `Trap::Interrupt`, and the elapsed time is checked as well because the
/// cut lands on the next loop backedge after the tick, so a call can report a trap a hair past its
/// deadline without being the target's fault.
fn classify(path: &Path, export: &str, error: &wasmtime::Error, duration: Duration, deadline: Duration) -> Stopped {
    let code = error.downcast_ref::<Trap>().copied();
    if matches!(code, Some(Trap::Interrupt)) || duration >= deadline {
        return Stopped::Interrupted;
    }
    let class = match code {
        Some(trap) => trap_name(trap),
        None => "wasm-error",
    };
    let mut frames = Vec::new();
    if let Some(backtrace) = error.downcast_ref::<wasmtime::WasmBacktrace>() {
        // A Rust wasm module aborts every panic through one trampoline instruction, so the frame a
        // backtrace starts with is the same file at the same offset for every panic the module can
        // raise, and the panic path above it differs only in the message the trap carries. Keyed on
        // that frame, two distinct bugs are filed as one finding. The frames that say where the target
        // broke are the target's own, and the name section says which frames belong to the runtime, so
        // those are left out here for the same reason they are left out of the coverage map.
        let all: Vec<&wasmtime::FrameInfo> = backtrace.frames().iter().collect();
        let own: Vec<&wasmtime::FrameInfo> =
            all.iter().copied().filter(|frame| !frame.func_name().is_some_and(instrument::is_plumbing)).collect();
        // A module whose every frame is runtime code has nothing else to name the trap with.
        let reported: &[&wasmtime::FrameInfo] = if own.is_empty() { &all } else { &own };
        for frame in reported {
            frames.push(FaultFrame {
                // A stripped wasm module carries no source positions: the module is the file, and the
                // instruction offset is the closest thing to a line. The offset is also what keeps two
                // traps in one function apart once the runtime's own frames are out of the way.
                file: path.display().to_string(),
                line: frame
                    .module_offset()
                    .map(|offset| offset as u32)
                    .unwrap_or_else(|| frame.func_index()),
                column: None,
                fn_name: Some(frame.func_name().unwrap_or(export).to_string()),
            });
        }
    }
    Stopped::Trapped(FaultSignature {
        class: class.to_string(),
        message: error.to_string(),
        frames,
    })
}

/// The trap's own name for itself, so a finding's `class` says which check in the WebAssembly
/// spec fired rather than repeating the message text.
fn trap_name(trap: Trap) -> &'static str {
    match trap {
        Trap::StackOverflow => "stack-overflow",
        Trap::MemoryOutOfBounds => "memory-out-of-bounds",
        Trap::HeapMisaligned => "heap-misaligned",
        Trap::TableOutOfBounds => "table-out-of-bounds",
        Trap::IndirectCallToNull => "indirect-call-to-null",
        Trap::BadSignature => "bad-signature",
        Trap::IntegerOverflow => "integer-overflow",
        Trap::IntegerDivisionByZero => "integer-division-by-zero",
        Trap::BadConversionToInteger => "bad-conversion-to-integer",
        Trap::UnreachableCodeReached => "unreachable-code-reached",
        Trap::Interrupt => "interrupt",
        Trap::OutOfFuel => "out-of-fuel",
        Trap::AtomicWaitNonSharedMemory => "atomic-wait-non-shared-memory",
        Trap::NullReference => "null-reference",
        Trap::ArrayOutOfBounds => "array-out-of-bounds",
        Trap::AllocationTooLarge => "allocation-too-large",
        Trap::CastFailure => "cast-failure",
        _ => "wasm-trap",
    }
}

/// The wasm type a parameter or result of this shape maps to, for discovery.
pub fn lowered(ty: ValType) -> Option<afz_protocol::TypeNode> {
    use afz_protocol::{NumberBounds, ScalarKind, TypeNode};
    match ty {
        ValType::I32 => Some(TypeNode::Scalar {
            scalar: ScalarKind::Number,
            bounds: Some(NumberBounds {
                int: true,
                finite_only: true,
                min: Some(i32::MIN as f64),
                max: Some(i32::MAX as f64),
            }),
            length: None,
        }),
        ValType::I64 => Some(TypeNode::Scalar { scalar: ScalarKind::BigInt, bounds: None, length: None }),
        ValType::F32 | ValType::F64 => Some(TypeNode::Scalar {
            scalar: ScalarKind::Number,
            bounds: Some(NumberBounds { int: false, finite_only: false, min: None, max: None }),
            length: None,
        }),
        // Reference parameters would need the harness to hand the module an object it owns, which
        // is the host-side marshalling the wasm harness does not do.
        ValType::Ref(_) => None,
        _ => None,
    }
}

/// A generated value as a wasm argument. `None` means the value has no wasm representation, and
/// `prepare` reports that rather than passing something the target never declares.
pub fn argument(ty: &ValType, node: &afz_protocol::ValueNode) -> Option<Val> {
    use afz_protocol::ValueNode;
    let number = |node: &afz_protocol::NumberValue| Some(node.as_f64());
    match (ty, node) {
        (ValType::I32, afz_protocol::ValueNode::Number { value }) => Some(Val::I32(number(value)? as i32)),
        (ValType::I32, afz_protocol::ValueNode::BigInt { value }) => value.parse::<i32>().ok().map(Val::I32),
        (ValType::I32, ValueNode::Boolean { value }) => Some(Val::I32(i32::from(*value))),
        (ValType::I64, afz_protocol::ValueNode::BigInt { value }) => value.parse::<i64>().ok().map(Val::I64),
        (ValType::I64, afz_protocol::ValueNode::Number { value }) => Some(Val::I64(number(value)? as i64)),
        (ValType::F32, afz_protocol::ValueNode::Number { value }) => Some(Val::F32(ieee(number(value)? as f32))),
        (ValType::F64, afz_protocol::ValueNode::Number { value }) => Some(Val::F64(ieee64(number(value)?))),
        _ => None,
    }
}

fn ieee(v: f32) -> u32 {
    if v.is_nan() { f32::NAN.to_bits() } else { v.to_bits() }
}

fn ieee64(v: f64) -> u64 {
    if v.is_nan() { f64::NAN.to_bits() } else { v.to_bits() }
}

/// A wasm result as a value the engine can put in a report. A wasm `i32` is signed on the wire, and
/// that is what is reported: the harness does not guess whether the target meant `u32`.
pub fn returned(value: &Val) -> Option<afz_protocol::ValueTree> {
    use afz_protocol::ValueTree;
    match value {
        Val::I32(v) => Some(ValueTree::number(f64::from(*v))),
        Val::I64(v) => Some(ValueTree::new(afz_protocol::ValueNode::BigInt { value: v.to_string() })),
        Val::F32(bits) => Some(ValueTree::number(f32::from_bits(*bits) as f64)),
        Val::F64(bits) => Some(ValueTree::number(f64::from_bits(*bits))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_encoder::{
        BlockType, CodeSection, Encode, ExportKind, ExportSection, FunctionSection, Instruction, NameMap, NameSection,
        Module as WasmModule, TypeSection, ValType as WasmValType,
    };

    /// A module with five exports: one that returns a number, one that traps, one that never returns,
    /// one that traps at either of two sites, and one that traps inside a function the name section
    /// says belongs to the runtime. The fourth is the case that matters, because cutting it is what
    /// used to leave the runner unable to do anything afterwards, including instantiating again.
    fn module() -> Vec<u8> {
        let mut m = WasmModule::new();
        let mut types = TypeSection::new();
        types.ty().function([], [WasmValType::I32]);
        types.ty().function([], []);
        types.ty().function([WasmValType::I32], []);
        m.section(&types);

        let mut funcs = FunctionSection::new();
        funcs.function(0);
        funcs.function(1);
        funcs.function(1);
        funcs.function(2);
        funcs.function(1);
        funcs.function(1);
        m.section(&funcs);

        let mut exports = ExportSection::new();
        exports.export("answer", ExportKind::Func, 0);
        exports.export("trap", ExportKind::Func, 1);
        exports.export("spin", ExportKind::Func, 2);
        exports.export("boom", ExportKind::Func, 3);
        exports.export("through", ExportKind::Func, 5);
        m.section(&exports);

        let mut code = CodeSection::new();
        let mut body = Vec::new();
        (0u32).encode(&mut body); // the locals section, empty
        Instruction::I32Const(7).encode(&mut body);
        Instruction::End.encode(&mut body);
        code.raw(&body);

        let mut body = Vec::new();
        (0u32).encode(&mut body);
        Instruction::Unreachable.encode(&mut body);
        Instruction::End.encode(&mut body);
        code.raw(&body);

        // `loop { br 0 }`. The backedge is where the epoch check runs, so a loop with none would
        // never be cut and the test would pass for the wrong reason.
        let mut body = Vec::new();
        (0u32).encode(&mut body);
        Instruction::Loop(BlockType::Empty).encode(&mut body);
        Instruction::Br(0).encode(&mut body);
        Instruction::End.encode(&mut body);
        Instruction::End.encode(&mut body); // closes the function, the one above closes the loop
        code.raw(&body);

        // Two `unreachable` instructions at different offsets inside one function, which is what a
        // module with more than one panic site looks like once it is compiled: every panic aborts
        // through the same trampoline, so the function alone does not say which site fired.
        let mut body = Vec::new();
        (0u32).encode(&mut body);
        Instruction::LocalGet(0).encode(&mut body);
        Instruction::If(BlockType::Empty).encode(&mut body);
        Instruction::Unreachable.encode(&mut body);
        Instruction::Else.encode(&mut body);
        Instruction::Unreachable.encode(&mut body);
        Instruction::End.encode(&mut body); // closes the if
        Instruction::End.encode(&mut body); // closes the function
        code.raw(&body);

        // The runtime's own trap site: the abort trampoline a Rust panic ends in.
        let mut body = Vec::new();
        (0u32).encode(&mut body);
        Instruction::Unreachable.encode(&mut body);
        Instruction::End.encode(&mut body);
        code.raw(&body);

        // The target's frame: it calls the runtime function above, so a backtrace from here starts
        // with the runtime and only names the target one frame deeper.
        let mut body = Vec::new();
        (0u32).encode(&mut body);
        Instruction::Call(4).encode(&mut body);
        Instruction::End.encode(&mut body);
        code.raw(&body);

        m.section(&code);

        let mut names = NameMap::new();
        names.append(4, "core[37ee0ce0c4cd57d5]::panicking::panic_fmt");
        let mut section = NameSection::new();
        section.functions(&names);
        m.section(&section);
        m.finish()
    }

    fn runner() -> Runner {
        Runner::new(Path::new("test-module.wasm"), &module(), &[]).expect("the test module compiles")
    }

    /// The regression: an epoch cut leaves no interrupt behind for the invocation that follows.
    #[test]
    fn a_cut_call_does_not_poison_the_next_one() {
        let mut runner = runner();
        let cut = runner
            .invoke("spin", &[], Duration::from_millis(80), false)
            .expect("a cut is an outcome, not a plugin error");
        assert!(matches!(cut.stopped, Stopped::Interrupted), "the spin export should be cut, got {:?}", cut.stopped);

        let next = runner
            .invoke("answer", &[], Duration::from_millis(500), false)
            .expect("the invocation after a cut must still run");
        match &next.stopped {
            Stopped::Returned(values) => {
                assert!(matches!(values.as_slice(), [Val::I32(7)]), "the export returns 7, got {values:?}")
            }
            other => panic!("expected the export to return, got {other:?}"),
        }
    }

    /// A trapped call is the other way a guest can end badly, and it used to leave half-written
    /// memory in front of the next call.
    #[test]
    fn a_trapped_call_does_not_poison_the_next_one() {
        let mut runner = runner();
        let trapped = runner
            .invoke("trap", &[], Duration::from_millis(500), false)
            .expect("a trap is an outcome, not a plugin error");
        match &trapped.stopped {
            Stopped::Trapped(fault) => assert_eq!(fault.class, "unreachable-code-reached"),
            other => panic!("expected a trap, got {other:?}"),
        }

        let next = runner
            .invoke("answer", &[], Duration::from_millis(500), false)
            .expect("the invocation after a trap must still run");
        assert!(matches!(next.stopped, Stopped::Returned(_)), "expected a return, got {:?}", next.stopped);
    }

    /// Two traps in one function are two bugs. A Rust wasm module aborts every panic through the
    /// same trampoline, so a frame keyed on the function index gives every trap in that function
    /// the same finding identity, and the second distinct bug is filed as a repeat of the first.
    #[test]
    fn two_traps_in_one_function_get_different_frame_positions() {
        let mut runner = runner();
        let left = runner
            .invoke("boom", &[Val::I32(0)], Duration::from_millis(500), false)
            .expect("a trap is an outcome");
        let right = runner
            .invoke("boom", &[Val::I32(1)], Duration::from_millis(500), false)
            .expect("a trap is an outcome");

        let top = |executed: &Executed| match &executed.stopped {
            Stopped::Trapped(fault) => fault.frames.first().map(|frame| frame.line),
            other => panic!("expected a trap, got {other:?}"),
        };
        let (Some(left_line), Some(right_line)) = (top(&left), top(&right)) else {
            panic!("both calls must trap with a frame that says where: {:?} {:?}", top(&left), top(&right));
        };
        assert_ne!(left_line, right_line, "the two traps sit at different instruction offsets");
    }

    /// A Rust wasm module aborts every panic through one trampoline instruction, so the frame a
    /// backtrace starts with is the same file at the same offset for every panic the module raises, and
    /// two distinct bugs would be filed as one finding. The frame that names the bug is the target's.
    #[test]
    fn a_trap_inside_the_runtimes_own_code_is_not_the_frame_that_names_the_bug() {
        let mut runner = runner();
        let executed = runner
            .invoke("through", &[], Duration::from_millis(500), false)
            .expect("a trap is an outcome");
        let Stopped::Trapped(fault) = executed.stopped else { panic!("expected a trap, got {:?}", executed.stopped) };
        let names: Vec<&str> = fault.frames.iter().map(|frame| frame.fn_name.as_deref().unwrap_or_default()).collect();
        assert_eq!(names, vec!["through"], "the runtime's frame must not be what a finding is keyed on: {names:?}");
    }

}

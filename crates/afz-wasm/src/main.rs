//! The wasm harness plugin: a JSON-RPC server on stdio that calls the exported functions of one
//! module.
//!
//! It is a harness plugin like `js-ts` is, and it speaks the same six methods, which is the point:
//! the engine drives a wasm target with the same scheduler, corpus, oracles, and report it uses for
//! a package on disk, and nothing in the engine learns that the target is WebAssembly. The module is
//! chosen by the scan project - the plugin's `command` names this binary and the module path - and
//! every export of function type becomes one target whose parameter types come from the module's
//! own signature, because that is the only type information a wasm module carries.
//!
//! Discovery does not run the module, so the handshake says `discoverRequiresExecution: false` and
//! the engine reads the inventory without applying an invocation deadline to it.

mod instrument;
mod runner;

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use afz_protocol::rpc::{self, Method, Request, Response};
use afz_protocol::{
    Capability, CoverageEntry, CoverageMap, DiscoverReq, Discovery, Effects, FailedOutcome, Handshake, InvokeReq,
    Materialization, MaterializedCall, OkOutcome, Origin, Parameter, PrepareReq, PrepareResp, PROTOCOL_VERSION,
    RejectedOutcome, SkipReason, SkippedTarget, TargetClass, TargetDescriptor, ValueNode, ValueTree,
};
use sha2::{Digest as _, Sha256};
use wasmtime::ExternType;

/// A parameter the plugin cannot build. The engine counts the invocation as unattempted rather than
/// calling the target with a value it never declares.
const INVALID_PARAMS: i64 = -32602;
/// A request the plugin cannot answer at all: an unknown target, or a method this harness does not
/// implement.
const INVALID_REQUEST: i64 = -32600;

/// One discovered export, kept so `prepare` and `invoke` can resolve a target id back to the
/// signature discovery read.
#[derive(Clone)]
struct Target {
    export: String,
    arguments: Vec<wasmtime::ValType>,
}

struct Plugin {
    runner: runner::Runner,
    blocks: Vec<instrument::Block>,
    names: std::collections::HashMap<u32, String>,
    targets: Vec<Target>,
    /// The scan project's root, which include and exclude patterns are written relative to.
    root: PathBuf,
}

fn main() {
    if let Err(error) = serve() {
        eprintln!("afz-wasm: {error}");
        std::process::exit(1);
    }
}

fn serve() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let root = std::env::var("AFZ_ROOT").unwrap_or_else(|_| std::env::current_dir().map(|d| d.display().to_string()).unwrap_or_default());
    let path = module_path(&argv, Path::new(&root))?;
    let raw = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let instrumented = instrument::instrument(&raw).map_err(|e| format!("{}: {e}", path.display()))?;
    // The engine drains a plugin's stderr and keeps the lines, so this is where a reader of a scan
    // can see what the coverage numbers were measured over.
    eprintln!(
        "afz-wasm: {} instrumented blocks, {} runtime functions left alone",
        instrumented.blocks.len(),
        instrumented.skipped_functions,
    );
    let runner = runner::Runner::new(&path, &instrumented.bytes, &instrumented.blocks)?;
    let mut plugin = Plugin {
        runner,
        blocks: instrumented.blocks,
        names: instrumented.function_names,
        targets: Vec::new(),
        root: PathBuf::from(&root),
    };

    let mut reader = rpc::buffered(io::stdin());
    let mut writer = rpc::buffered_writer(io::stdout());
    let mut line = String::new();
    while let Some(text) = rpc::read_line(&mut reader, &mut line).map_err(|e| format!("reading requests: {e}"))? {
        let response = match rpc::parse_request(&text) {
            Ok(request) => handle(&mut plugin, &request),
            Err(error) => Response::err(0, INVALID_REQUEST, error.to_string()),
        };
        rpc::write_line(&mut writer, &response).map_err(|e| format!("writing responses: {e}"))?;
        writer.flush().map_err(|e| format!("writing responses: {e}"))?;
    }
    Ok(())
}

/// The module under test: the path the scan project's `command` names, or the first module in the
/// scan project if the command names none.
fn module_path(argv: &[String], root: &Path) -> Result<PathBuf, String> {
    if let Some(named) = argv.iter().find(|arg| !arg.starts_with('-')) {
        let path = Path::new(named);
        let path = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
        return path.canonicalize().map_err(|e| format!("{}: {e}", path.display()));
    }
    let mut found = Vec::new();
    collect(Path::new(root), 0, &mut found);
    found.sort();
    found.into_iter().next().ok_or_else(|| format!("no .wasm module under {}", root.display()))
}

/// Modules a scan would not want: build caches and dependency trees carry hundreds of modules that
/// are not the project's own artifact.
const SKIPPED_DIRS: &[&str] = &["node_modules", ".git", ".autofuzz", "debug", "build"];
const MAX_DEPTH: u32 = 6;

fn collect(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_ref()) {
                collect(&path, depth + 1, out);
            }
        } else if name.ends_with(".wasm") && !name.ends_with(".wasm.gz") {
            out.push(path);
        }
    }
}

fn handle(plugin: &mut Plugin, request: &Request) -> Response {
    let id = request.id;
    let params = request.params.clone();
    let Some(method) = Method::from_wire(&request.method) else {
        return Response::err(id, rpc::METHOD_NOT_FOUND, format!("unknown method \"{}\"", request.method));
    };
    let result: Result<serde_json::Value, (i64, String)> = match method {
        Method::Handshake => serde_json::to_value(handshake()).map_err(|e| (INVALID_REQUEST, e.to_string())),
        Method::Discover => match parse::<DiscoverReq>(&params) {
            Ok(req) => serde_json::to_value(plugin.discover(&req)).map_err(|e| (INVALID_REQUEST, e.to_string())),
            Err(e) => Err(e),
        },
        Method::Prepare => match parse::<PrepareReq>(&params) {
            Ok(req) => match plugin.prepare(&req) {
                Ok(resp) => serde_json::to_value(resp).map_err(|e| (INVALID_REQUEST, e.to_string())),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        },
        Method::Invoke => match parse::<InvokeReq>(&params) {
            Ok(req) => match plugin.invoke(&req) {
                Ok(outcome) => serde_json::to_value(outcome).map_err(|e| (INVALID_REQUEST, e.to_string())),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        },
        // Shrinkage in target terms needs a target-specific notion of what to keep; the engine's
        // delta-debugging minimizer works on the value tree, and the handshake does not claim the
        // capability.
        Method::Minimize => Err((INVALID_REQUEST, "the wasm harness does not minimize in target terms".into())),
        Method::CoverageMap => serde_json::to_value(plugin.coverage_map()).map_err(|e| (INVALID_REQUEST, e.to_string())),
    };
    match result {
        Ok(value) => Response::ok(id, value),
        Err((code, message)) => Response::err(id, code, message),
    }
}

/// Request params are optional on the wire. A missing `params` is read as an empty object, which
/// is what a request type whose fields all have defaults expects; `null` would fail to deserialize.
fn parse<T: serde::de::DeserializeOwned>(params: &Option<serde_json::Value>) -> Result<T, (i64, String)> {
    let params = params.clone().unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    serde_json::from_value(params).map_err(|e| (INVALID_PARAMS, format!("malformed request: {e}")))
}

fn handshake() -> Handshake {
    Handshake {
        protocol_version: PROTOCOL_VERSION.to_string(),
        name: "wasm".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        target_class: TargetClass::DesignTime,
        // `coverage`: every invocation carries a digest. `concurrent`: the plugin holds no state
        // outside the process the engine gave it, so the engine may run one per worker. Not
        // `stateful` - there is no receiver to build - and not `sideEffects`: a wasm export cannot
        // reach outside its instance.
        capabilities: vec![Capability::Coverage, Capability::Concurrent],
        languages: vec!["wasm".into()],
        discover_requires_execution: false,
    }
}

/// Match a path against a glob, segment by segment: `*` matches within one path segment and `**`
/// matches any number of segments. A wasm scan project has one file, so an include pattern that
/// matches nothing silently turns the scan into a scan of nothing, which is why the shapes a person
/// writes - `**/*.wasm`, `*.wasm`, `dist/app.wasm` - are the ones tested.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    let pattern = pattern.replace('\\', "/");
    let path = path.replace('\\', "/");
    let pat = glob_segments(&pattern);
    let way = glob_segments(&path);
    // A pattern with no separator names a file rather than a place: `*.wasm` means every module at
    // any depth, which is how it is written when the project keeps one module in a build tree.
    let way: &[&str] = match (pattern.contains('/'), way.split_last()) {
        (true, _) => &way,
        (false, Some((last, _))) => std::slice::from_ref(last),
        (false, None) => &[],
    };
    matches_segments(&pat, way)
}

fn glob_segments(text: &str) -> Vec<&str> {
    text.split('/').filter(|part| !part.is_empty() && *part != ".").collect()
}

fn matches_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.first().copied() {
        None => path.is_empty(),
        // `**` either stands for no segments here or consumes one, which is what makes it any depth.
        Some("**") => matches_segments(&pattern[1..], path) || (!path.is_empty() && matches_segments(pattern, &path[1..])),
        Some(head) => !path.is_empty() && matches_segment(head, path[0]) && matches_segments(&pattern[1..], &path[1..]),
    }
}

fn matches_segment(pattern: &str, text: &str) -> bool {
    let mut rest = text;
    for (index, part) in pattern.split('*').enumerate() {
        if index == 0 {
            if !rest.starts_with(part) {
                return false;
            }
            rest = &rest[part.len()..];
            continue;
        }
        let Some(at) = rest.find(part) else { return false };
        rest = &rest[at + part.len()..];
    }
    rest.is_empty() || pattern.ends_with('*')
}

impl Plugin {
    fn discover(&mut self, req: &DiscoverReq) -> Discovery {
        let module = self.runner.path().display().to_string();
        // Patterns are written relative to the scan project and the module's path is absolute, so
        // the match runs against the relative form.
        let relative = self
            .runner
            .path()
            .strip_prefix(&self.root)
            .unwrap_or(self.runner.path())
            .display()
            .to_string();
        let excluded = req.exclude.as_ref().is_some_and(|p| p.iter().any(|g| matches_pattern(g, &relative)));
        let included = req.include.as_ref().is_none_or(|p| p.iter().any(|g| matches_pattern(g, &relative)));
        let mut targets = Vec::new();
        let mut skipped = Vec::new();
        self.targets.clear();

        for (name, ty) in self.runner.exports() {
            // `__wasm_call_ctors` and its neighbours are the artifact's own plumbing, and the
            // counters memory belongs to the harness.
            if name == instrument::COUNTERS || name.starts_with("__") {
                continue;
            }
            let id = target_id(&module, name);
            if excluded || !included {
                skipped.push(skip(id, name, SkipReason::Excluded, None, None, Some(module.clone())));
                continue;
            }
            let kind = kind_of(&ty);
            let ExternType::Func(signature) = ty else {
                skipped.push(skip(
                    id,
                    name,
                    SkipReason::UnsupportedSignature,
                    None,
                    None,
                    Some(format!("{name} is a {kind} export, not a function")),
                ));
                continue;
            };
            if signature.results().len() > 1 {
                skipped.push(skip(
                    id,
                    name,
                    SkipReason::UnsupportedSignature,
                    None,
                    None,
                    Some(format!("{name} returns {} values, which one call result cannot carry", signature.results().len())),
                ));
                continue;
            }
            let mut parameters = Vec::new();
            let mut blocker = None;
            for (index, parameter) in signature.params().enumerate() {
                // `ValType` is not `Copy`, and the block below reports the wasm type by name when it
                // cannot be lowered, so the name is taken before the type is moved.
                let wire = format!("{parameter:?}");
                match runner::lowered(parameter) {
                    Some(ty) => parameters.push(Parameter { name: format!("arg{index}"), ty, required: true }),
                    None => {
                        blocker = Some(afz_protocol::Blocker {
                            type_: wire,
                            module: Some(module.clone()),
                            path: format!("arg{index}"),
                        })
                    }
                }
            }
            if let Some(blocked_by) = blocker {
                skipped.push(skip(
                    id,
                    name,
                    SkipReason::UnconstructibleParameter,
                    Some(blocked_by),
                    Some(Materialization::Opaque),
                    Some("a reference parameter needs the host to hand the module an object".into()),
                ));
                continue;
            }
            self.targets.push(Target { export: name.to_string(), arguments: signature.params().collect() });
            targets.push(TargetDescriptor {
                id,
                target_class: TargetClass::DesignTime,
                origin: Origin { file: Some(module.clone()), line: None, module: Some(module.clone()), http: None },
                name: name.to_string(),
                parameters,
                returns: signature.results().next().and_then(runner::lowered),
                // A wasm export cannot be async, cannot take a callback, cannot stream, and has no
                // `throw`: the traps it can raise are faults, recorded on the outcome, not effects
                // declared at discovery.
                effects: Effects::default(),
                guard_sites: Vec::new(),
                materialization: Materialization::Constructible,
            });
        }
        Discovery { targets, skipped }
    }

    fn prepare(&self, req: &PrepareReq) -> Result<PrepareResp, (i64, String)> {
        let Some(target) = self.target(&req.target_id) else {
            return Err((INVALID_REQUEST, format!("no target with id {}", req.target_id)));
        };
        if req.values.len() != target.arguments.len() {
            return Ok(PrepareResp::Skip { reason: SkipReason::UnsupportedSignature });
        }
        for (ty, value) in target.arguments.iter().zip(&req.values) {
            if runner::argument(ty, &value.node).is_none() {
                return Ok(PrepareResp::Skip { reason: SkipReason::UnconstructibleParameter });
            }
        }
        Ok(PrepareResp::Ok {
            materialized: MaterializedCall { handle: req.target_id.clone(), summary: summarize(&target.export, &req.values) },
        })
    }

    fn invoke(&mut self, req: &InvokeReq) -> Result<afz_protocol::Outcome, (i64, String)> {
        let target = self.target(&req.target_id).cloned().ok_or((INVALID_REQUEST, format!("no target with id {}", req.target_id)))?;
        let mut arguments = Vec::new();
        for (ty, value) in target.arguments.iter().zip(&req.values) {
            match runner::argument(ty, &value.node) {
                Some(argument) => arguments.push(argument),
                None => {
                    return Ok(afz_protocol::Outcome::Rejected(RejectedOutcome {
                        duration_ms: 0.0,
                        coverage: None,
                        via: "unmaterializable".into(),
                        detail: format!("{} has no wasm representation for {}", target.export, describe(&value.node)),
                    }))
                }
            }
        }
        let deadline = std::time::Duration::from_millis(req.deadline_ms);
        let executed = self
            .runner
            .invoke(&target.export, &arguments, deadline, req.collect_coverage)
            .map_err(|e| (rpc::PLUGIN_ERROR, e))?;
        let duration_ms = executed.duration.as_secs_f64() * 1000.0;
        let coverage = req.collect_coverage.then_some(executed.edges);
        let memory_bytes = Some(executed.memory_bytes);
        Ok(match executed.stopped {
            runner::Stopped::Returned(values) => afz_protocol::Outcome::Ok(OkOutcome {
                duration_ms,
                memory_bytes,
                coverage,
                value: values.first().and_then(runner::returned),
            }),
            runner::Stopped::Interrupted => afz_protocol::Outcome::Ok(OkOutcome { duration_ms, memory_bytes, coverage, value: None }),
            runner::Stopped::Trapped(fault) => {
                afz_protocol::Outcome::Failed(FailedOutcome { duration_ms, coverage, via: "wasm-trap".into(), fault })
            }
        })
    }

    fn coverage_map(&self) -> CoverageMap {
        CoverageMap {
            entries: self
                .blocks
                .iter()
                .map(|block| CoverageEntry {
                    edge: block.edge,
                    // The name section is what says which function an edge belongs to. A module
                    // stripped of names leaves the index, which is the most a map can say.
                    url: match self.names.get(&block.function) {
                        Some(name) => format!("{}#{}", self.runner.path().display(), name),
                        None => format!("{}#f{}", self.runner.path().display(), block.function),
                    },
                    start: u64::from(block.offset),
                    end: u64::from(block.offset) + u64::from(block.length),
                })
                .collect(),
        }
    }

    fn target(&self, id: &str) -> Option<&Target> {
        self.targets.iter().find(|t| target_id(&self.module_name(), &t.export) == id)
    }

    fn module_name(&self) -> String {
        self.runner.path().display().to_string()
    }
}

fn skip(
    target_id: String,
    name: &str,
    reason: SkipReason,
    blocked_by: Option<afz_protocol::Blocker>,
    materialization: Option<Materialization>,
    detail: Option<String>,
) -> SkippedTarget {
    SkippedTarget { target_id, name: name.to_string(), reason, blocked_by, unlocks_targets: None, materialization, detail }
}

fn kind_of(ty: &ExternType) -> &'static str {
    match ty {
        ExternType::Func(_) => "function",
        ExternType::Memory(_) => "memory",
        ExternType::Table(_) => "table",
        ExternType::Global(_) => "global",
        _ => "other",
    }
}

/// The target id: a hash of the module and the export name, so the same export of the same module
/// gets the same id across runs and a resumed scan recognises its findings.
fn target_id(module: &str, export: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(module.as_bytes());
    digest.update([0]);
    digest.update(export.as_bytes());
    let bytes = digest.finalize();
    let mut hex = String::with_capacity(12);
    for byte in &bytes[..6] {
        hex.push_str(&format!("{byte:02x}"));
    }
    format!("afz:t:{hex}")
}

fn summarize(export: &str, values: &[ValueTree]) -> String {
    let arguments: Vec<String> = values.iter().map(|v| describe(&v.node)).collect();
    format!("{export}({})", arguments.join(", "))
}

/// A value in one phrase, for a call summary and for the message of an unmaterializable argument.
fn describe(node: &ValueNode) -> String {
    match node {
        ValueNode::Number { value } => value.as_f64().to_string(),
        ValueNode::BigInt { value } => format!("{value}n"),
        ValueNode::String { value } => {
            if value.chars().count() > 24 {
                format!("\"{}…\"", value.chars().take(24).collect::<String>())
            } else {
                format!("{value:?}")
            }
        }
        ValueNode::Boolean { value } => value.to_string(),
        ValueNode::Null => "null".into(),
        ValueNode::Undefined => "undefined".into(),
        other => format!("{other:?}").split('{').next().unwrap_or("value").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::matches_pattern;

    /// The include list decides whether the scan sees its only module at all. A pattern that
    /// silently matched nothing reports an empty inventory instead of a target, so the shapes a
    /// person writes are pinned here, including the two that must not match.
    #[test]
    fn include_patterns_match_the_module_a_scan_names() {
        assert!(matches_pattern("**/*.wasm", "target/wasm32-unknown-unknown/release/wasm_probe.wasm"));
        assert!(matches_pattern("*.wasm", "target/release/app.wasm"), "a pattern with no separator is matched against the file name");
        assert!(matches_pattern("dist/app.wasm", "dist/app.wasm"));
        assert!(matches_pattern("**", "any/depth/at/all.wasm"));
        assert!(!matches_pattern("**/*.wasm", "src/main.ts"));
        assert!(!matches_pattern("*.wasm", "app.wasm.gz"), "a gzipped module is not the module");
        assert!(!matches_pattern("dist/*.wasm", "build/app.wasm"), "a pattern with a separator is anchored at the scan root");
    }
}


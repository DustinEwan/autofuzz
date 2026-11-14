//! A harness plugin with no target language in it. It answers the seven protocol methods and
//! takes its `invoke` behavior from a script, which is how the engine's deadline, restart,
//! and process-exit tests work without a real target.
//!
//! Script format: a JSON array of directives, passed as `AFZ_FAKE_SCRIPT` (inline JSON) or
//! `AFZ_FAKE_SCRIPT_FILE` (path). Directives apply in array order; `at` is the 1-based
//! invocation index, and `for` optionally restricts a directive to one target id.

use std::io::Write;

use afz_protocol::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const PLUGIN_NAME: &str = "afz-fakeplugin";
pub const PLUGIN_VERSION: &str = "0.1.0";

/// One scripted behavior for a range of invocations.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Directive {
    /// First invocation index this directive governs, 1-based. Defaults to the first index
    /// no earlier directive claimed.
    pub at: Option<u64>,
    /// Last invocation index this directive governs, inclusive. Defaults to `at`.
    pub to: Option<u64>,
    /// Restrict to one target id.
    pub r#for: Option<String>,
    /// Sleep this many milliseconds before answering.
    pub sleep_ms: Option<u64>,
    /// Exit the process with this code instead of answering.
    pub exit: Option<i32>,
    /// Write to stderr, which the engine forwards to the scan log.
    pub stderr: Option<String>,
    /// Answer with this outcome verbatim.
    pub outcome: Option<Outcome>,
    /// Coverage edges to attach to the answer.
    pub coverage: Option<Vec<EdgeId>>,
    /// Leave `coverage` off the answer instead of filling it, so a plugin that declares the
    /// `coverage` capability and does not honor it can be modelled.
    pub drop_coverage: Option<bool>,
    /// Fail the method with a JSON-RPC error instead of an outcome.
    pub rpc_error: Option<String>,
}

/// The whole script.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Script {
    pub directives: Vec<Directive>,
    /// Overrides the fixed inventory, for tests that need a particular target set.
    pub discovery: Option<Discovery>,
    /// Answers `handshake` with this document instead of the conforming one, for tests that
    /// need a plugin reporting a different protocol revision or a capability it does not honor.
    pub handshake: Option<Handshake>,
    /// Refuses `prepare` and `invoke` until a `discover` has been served on this connection,
    /// which is what both shipped harnesses do: they resolve a `targetId` only against the
    /// inventory the same process discovered. Off by default, because the engine's transport
    /// tests drive `prepare` and `invoke` on a connection that never discovered.
    pub require_discover: Option<bool>,
    /// Answers an unparseable request with nothing at all, modelling the plugin that drops the
    /// bad line instead of writing a -32700 frame; `framing-intact` must catch it.
    pub swallow_parse_error: Option<bool>,
    /// Answers `ready` with this document instead of the always-ready default, for tests
    /// that need a plugin reporting its environment is down.
    pub ready: Option<ReadyResp>,
}

impl Script {
    /// Load from the environment. An absent or unreadable script is not an error: the plugin
    /// then answers every invocation with `ok`. Both shapes are accepted, so a test can write
    /// the short form `[{"exit":1}]` instead of `{"directives":[{"exit":1}]}`.
    pub fn from_env() -> Script {
        let text = std::env::var("AFZ_FAKE_SCRIPT")
            .ok()
            .or_else(|| std::env::var("AFZ_FAKE_SCRIPT_FILE").ok().and_then(|p| std::fs::read_to_string(p).ok()));
        let Some(text) = text else { return Script::default() };
        match serde_json::from_str::<Script>(&text) {
            Ok(script) => script,
            Err(struct_err) => match serde_json::from_str::<Vec<Directive>>(&text) {
                Ok(directives) => Script { directives, discovery: None, handshake: None, require_discover: None, swallow_parse_error: None, ready: None },
                Err(_) => {
                    eprintln!("{PLUGIN_NAME}: ignoring malformed script: {struct_err}");
                    Script::default()
                }
            },
        }
    }

    /// The directive governing invocation `index` of `target_id`, if any.
    pub fn directive_for(&self, index: u64, target_id: &str) -> Option<&Directive> {
        let mut next_free = 1u64;
        for d in &self.directives {
            let at = d.at.unwrap_or(next_free);
            let to = d.to.unwrap_or(at);
            next_free = next_free.max(to + 1);
            if index >= at && index <= to && d.r#for.as_deref().is_none_or(|t| t == target_id) {
                return Some(d);
            }
        }
        None
    }
}

/// Serve the plugin over the given pipes until the engine closes stdin.
pub fn serve<R: std::io::Read, W: Write>(read: R, mut write: W) {
    let script = Script::from_env();
    let mut reader = buffered(read);
    let mut buf = String::new();
    // Script indices count `invoke` requests only: `at` is an invocation index, and a test
    // must not shift when the engine adds a handshake or a discover.
    let mut invocations = 0u64;
    // `requireDiscover` models what both shipped harnesses do: they resolve a `targetId` only
    // against the inventory this same process discovered, so `prepare` before `discover` is a
    // refusal rather than an answer.
    let mut discovered = false;

    loop {
        let line = match read_line(&mut reader, &mut buf) {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(e) => {
                eprintln!("{PLUGIN_NAME}: {e}");
                return;
            }
        };
        let req = match parse_request(&line) {
            Ok(req) => req,
            Err(_) => {
                if !script.swallow_parse_error.unwrap_or(false) {
                    let _ = write_line(&mut write, &Response::err(0, PARSE_ERROR, "malformed request"));
                }
                continue;
            }
        };
        let Some(method) = Method::from_wire(&req.method) else {
            let _ = write_line(
                &mut write,
                &Response::err(req.id, METHOD_NOT_FOUND, format!("unknown method {}", req.method)),
            );
            continue;
        };

        let gated = script.require_discover.unwrap_or(false) && !discovered;
        let refused = Response::err(req.id, INVALID_REQUEST, "discover has not run on this connection");
        let resp = match method {
            Method::Handshake => Response::ok(req.id, script.handshake.clone().unwrap_or_else(handshake)),
            Method::Discover => {
                discovered = true;
                Response::ok(req.id, script.discovery.clone().unwrap_or_else(inventory))
            }
            Method::Prepare if gated => refused,
            Method::Invoke if gated => refused,
            Method::Prepare => Response::ok(req.id, prepare_resp(&req)),
            Method::CoverageMap => Response::ok(req.id, coverage_map()),
            Method::Invoke => {
                invocations += 1;
                invoke_resp(req.id, &req, &script, invocations)
            }
            Method::Minimize => Response::ok(req.id, minimize_resp(&req)),
            Method::Reproducer => Response::ok(req.id, reproducer_resp()),
            Method::Ready => Response::ok(
                req.id,
                script.ready.clone().unwrap_or(ReadyResp { ready: true, detail: None }),
            ),
        };
        if write_line(&mut write, &resp).is_err() {
            return;
        }
    }
}

fn handshake() -> Handshake {
    Handshake {
        protocol_version: PROTOCOL_VERSION.to_string(),
        name: PLUGIN_NAME.to_string(),
        version: PLUGIN_VERSION.to_string(),
        target_class: TargetClass::DesignTime,
        capabilities: vec![Capability::Coverage, Capability::Stateful, Capability::Minimize, Capability::Reproducer, Capability::Concurrent],
        languages: vec!["fake".to_string()],
        discover_requires_execution: false,
    }
}

fn param(name: &str, ty: TypeNode) -> Parameter {
    Parameter { name: name.to_string(), ty, required: true }
}

fn target(id: &str, name: &str, file: &str, line: u32, parameters: Vec<Parameter>, materialization: Materialization, guard_sites: Vec<u32>) -> TargetDescriptor {
    TargetDescriptor {
        id: id.to_string(),
        target_class: TargetClass::DesignTime,
        origin: Origin {
            file: Some(file.to_string()),
            line: Some(line),
            module: Some(file.to_string()),
            http: None,
        },
        name: name.to_string(),
        parameters,
        returns: Some(TypeNode::number()),
        effects: Effects::default(),
        guard_sites,
        materialization,
    }
}

/// The fixed six-target inventory.
pub fn inventory() -> Discovery {
    let money = TypeNode::Object {
        name: Some("Money".to_string()),
        id: None,
        fields: vec![
            Field { name: "v".into(), ty: TypeNode::number(), required: true },
            Field {
                name: "to".into(),
                ty: TypeNode::Literal { values: vec!["USD".into(), "EUR".into(), "ZZZ".into()] },
                required: true,
            },
        ],
    };
    Discovery {
        targets: vec![
            target(
                "afz:t:0001",
                "plain",
                "src/fake.ts",
                3,
                vec![param("amount", TypeNode::number())],
                Materialization::Constructible,
                Vec::new(),
            ),
            target(
                "afz:t:0002",
                "guarded",
                "src/fake.ts",
                11,
                vec![param("amount", TypeNode::number()), param("to", TypeNode::string())],
                Materialization::Constructible,
                vec![5],
            ),
            target(
                "afz:t:0003",
                "bulk",
                "src/fake.ts",
                24,
                vec![param("items", TypeNode::array(money)), param("rate", TypeNode::number())],
                Materialization::Constructible,
                Vec::new(),
            ),
            target(
                "afz:t:0004",
                "asyncThing",
                "src/fake.ts",
                40,
                vec![param("input", TypeNode::string())],
                Materialization::Constructible,
                Vec::new(),
            ),
            target(
                "afz:t:0005",
                "needsClock",
                "src/fake.ts",
                55,
                vec![param("clock", TypeNode::Opaque { ty: "Clock".into(), module: Some("src/clock.ts".into()) })],
                Materialization::FixtureRequired,
                Vec::new(),
            ),
            target(
                "afz:t:0006",
                "needsDb",
                "src/repo.ts",
                8,
                vec![param("db", TypeNode::Opaque { ty: "Db".into(), module: Some("src/db.ts".into()) })],
                Materialization::Opaque,
                Vec::new(),
            ),
        ],
        skipped: vec![
            SkippedTarget {
                target_id: "afz:t:0006".to_string(),
                name: "needsDb".to_string(),
                reason: SkipReason::UnconstructibleParameter,
                blocked_by: Some(Blocker {
                    type_: "Db".to_string(),
                    module: Some("src/db.ts".to_string()),
                    path: "receiver.db".to_string(),
                }),
                unlocks_targets: Some(14),
                materialization: Some(Materialization::FixtureRequired),
                detail: None,
            },
            SkippedTarget {
                target_id: "afz:t:0005".to_string(),
                name: "needsClock".to_string(),
                reason: SkipReason::UnconstructibleParameter,
                blocked_by: Some(Blocker {
                    type_: "Clock".to_string(),
                    module: Some("src/clock.ts".to_string()),
                    path: "parameters[0]".to_string(),
                }),
                unlocks_targets: Some(1),
                materialization: Some(Materialization::FixtureRequired),
                detail: None,
            },
            SkippedTarget {
                target_id: "afz:t:1001".to_string(),
                name: "brokenModule".to_string(),
                reason: SkipReason::DiscoveryError,
                blocked_by: None,
                unlocks_targets: None,
                materialization: None,
                detail: Some("syntax error at src/broken.ts:2".to_string()),
            },
            SkippedTarget {
                target_id: "afz:t:1002".to_string(),
                name: "internalHelper".to_string(),
                reason: SkipReason::NotPublic,
                blocked_by: None,
                unlocks_targets: None,
                materialization: None,
                detail: None,
            },
            SkippedTarget {
                target_id: "afz:t:1003".to_string(),
                name: "legacyShim".to_string(),
                reason: SkipReason::Excluded,
                blocked_by: None,
                unlocks_targets: None,
                materialization: None,
                detail: None,
            },
            SkippedTarget {
                target_id: "afz:t:1004".to_string(),
                name: "variadicJoin".to_string(),
                reason: SkipReason::UnsupportedSignature,
                blocked_by: None,
                unlocks_targets: None,
                materialization: None,
                detail: Some("rest parameter with no declared element type".to_string()),
            },
            SkippedTarget {
                target_id: "afz:t:0003".to_string(),
                name: "bulk".to_string(),
                reason: SkipReason::TargetDisabled,
                blocked_by: None,
                unlocks_targets: None,
                materialization: None,
                detail: Some("50 consecutive plugin losses".to_string()),
            },
        ],
    }
}

fn prepare_resp(req: &Request) -> PrepareResp {
    let req: PrepareReq = serde_json::from_value(req.params.clone().unwrap_or_default())
        .unwrap_or(PrepareReq { target_id: String::new(), values: Vec::new() });
    if req.target_id == "afz:t:0006" {
        return PrepareResp::Skip { reason: SkipReason::UnconstructibleParameter };
    }
    PrepareResp::Ok {
        materialized: MaterializedCall {
            handle: format!("h-{}", req.target_id),
            summary: format!("{} args", req.values.len()),
        },
    }
}

/// The block table this plugin can name. It must cover every edge [`default_edges`] can put in
/// a digest: the map is the denominator of `coverageRatio` and the table `coverage.json` is
/// written from, so a digest edge the map does not name is reported as covered code no reader
/// can locate.
fn coverage_map() -> CoverageMap {
    CoverageMap {
        entries: (1..=DIGEST_EDGES)
            .map(|i| CoverageEntry {
                edge: i,
                url: "src/fake.ts".to_string(),
                start: u64::from(i) * 40,
                end: u64::from(i) * 40 + 20,
            })
            .collect(),
    }
}

fn minimize_resp(req: &Request) -> MinimizeResp {
    let req: MinimizeReq = serde_json::from_value(req.params.clone().unwrap_or_default())
        .unwrap_or(MinimizeReq {
            target_id: String::new(),
            values: Vec::new(),
            fault: FaultSignature { class: String::new(), message: String::new(), frames: Vec::new() },
            budget_ms: 0,
        });
    // Shrink to the first argument only, which is enough for the engine's acceptance tests.
    let values = req.values.into_iter().take(1).collect();
    MinimizeResp { values, steps: 1 }
}

/// The fake plugin has no target language, so its script is a placeholder: the conformance
/// suite checks the response shape, and the engine checks the extension and stores the bytes.
fn reproducer_resp() -> ReproducerResp {
    ReproducerResp {
        extension: "txt".to_string(),
        script: "afz fake reproducer: replay the finding's inputs/afz-f-*.json through the plugin.\n".to_string(),
    }
}

fn invoke_resp(id: u64, req: &Request, script: &Script, index: u64) -> Response {
    let Ok(req) = serde_json::from_value::<InvokeReq>(req.params.clone().unwrap_or_default()) else {
        return Response::err(id, INVALID_REQUEST, "malformed invoke params");
    };
    let Some(d) = script.directive_for(index, &req.target_id) else {
        return Response::ok(id, default_ok(index, &req, None));
    };

    if let Some(msg) = &d.stderr {
        eprintln!("{msg}");
    }
    // The slept time is reported as part of the call's duration, so a scripted slow call looks
    // slow to the deadline oracle the way a real slow target would.
    let started = std::time::Instant::now();
    if let Some(ms) = d.sleep_ms {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    if let Some(code) = d.exit {
        std::process::exit(code);
    }
    if let Some(message) = &d.rpc_error {
        return Response::err(id, PLUGIN_ERROR, message.clone());
    }
    let slept = started.elapsed().as_secs_f64() * 1000.0;
    let timed = |outcome: Outcome| -> Outcome {
        match outcome {
            Outcome::Ok(mut o) => {
                o.duration_ms += slept;
                Outcome::Ok(o)
            }
            Outcome::Rejected(mut o) => {
                o.duration_ms += slept;
                Outcome::Rejected(o)
            }
            Outcome::Failed(mut o) => {
                o.duration_ms += slept;
                Outcome::Failed(o)
            }
        }
    };
    let mut outcome = match d.outcome.clone() {
        Some(Outcome::Ok(mut o)) => {
            if o.coverage.is_none() {
                o.coverage = d.coverage.clone().or_else(|| Some(default_edges(index)));
            }
            Outcome::Ok(o)
        }
        Some(Outcome::Rejected(mut o)) => {
            if o.coverage.is_none() {
                o.coverage = d.coverage.clone().or_else(|| Some(default_edges(index)));
            }
            Outcome::Rejected(o)
        }
        Some(outcome) => outcome,
        None => default_ok(index, &req, d.coverage.clone()),
    };
    if d.drop_coverage.unwrap_or(false) {
        match &mut outcome {
            Outcome::Ok(o) => o.coverage = None,
            Outcome::Rejected(o) => o.coverage = None,
            Outcome::Failed(o) => o.coverage = None,
        }
    }
    Response::ok(id, timed(outcome))
}

/// How many distinct edges a digest can carry, which is also how many the coverage map names.
const DIGEST_EDGES: u32 = 64;

fn default_edges(index: u64) -> Vec<EdgeId> {
    // Deterministic and index-dependent, so the corpus sees novelty grow as invocations do.
    vec![(index % u64::from(DIGEST_EDGES)) as u32 + 1]
}

/// The `ok` a call answers with when nothing was scripted for it, or when the directive named
/// edges but no whole outcome.
fn default_ok(index: u64, req: &InvokeReq, edges: Option<Vec<EdgeId>>) -> Outcome {
    Outcome::Ok(OkOutcome {
        duration_ms: 0.5,
        memory_bytes: Some(1024),
        coverage: Some(edges.unwrap_or_else(|| default_edges(index))),
        value: Some(ValueTree::number(f64::from(req.values.len() as u32))),
    })
}

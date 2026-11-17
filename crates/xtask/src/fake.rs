//! A harness plugin with no target language in it. It answers the seven protocol methods and
//! takes its `invoke` behavior from a script, which is how the engine's deadline, restart,
//! and process-exit tests work without a real target.
//!
//! Script format: a JSON array of directives, passed as `AFZ_FAKE_SCRIPT` (inline JSON) or
//! `AFZ_FAKE_SCRIPT_FILE` (path). Directives apply in array order; `at` is the 1-based
//! invocation index, and `for` optionally restricts a directive to one target id.
//!
//! The object form carries whole-script keys beside `directives`: `raw` answers a named method
//! with the script's own bytes, `discoverSecond` answers the second `discover` on a connection,
//! `refusePrepare` and `dropCoverageMap` refuse a method, and `secondInstance` makes the second of
//! two plugin processes answer differently. Each one exists so a profile rule has the failure it
//! is written to catch, and can be shown catching it.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    /// Answers these methods with this document verbatim, keyed by the method's wire name.
    ///
    /// Every other answer this plugin gives is built from an `afz_protocol` type, so it satisfies
    /// its committed schema by construction and a rule written to catch an off-schema answer has
    /// nothing to catch. This is the one place a script writes bytes the plugin does not type.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub raw: BTreeMap<String, serde_json::Value>,
    /// Answers the second and later `discover` on this connection with this inventory, modelling
    /// the plugin whose enumeration depends on something outside the request.
    pub discover_second: Option<Discovery>,
    /// Answers `prepare` with this JSON-RPC error, modelling the plugin that reports a target in
    /// its inventory and then cannot materialize it.
    pub refuse_prepare: Option<String>,
    /// Answers `coverageMap` with this JSON-RPC error instead of the table, modelling the plugin
    /// that declares the `coverage` capability and does not honor the method.
    pub drop_coverage_map: Option<String>,
    /// Makes the instance that finds `marker` already present answer differently. Two plugin
    /// processes started from one launch share an environment, so a file is the only thing that
    /// tells them apart, which is what the rules that compare them need.
    pub second_instance: Option<SecondInstance>,
}

/// How the second plugin process to start differs from the first.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct SecondInstance {
    /// Path whose presence says an instance has already started. The instance that finds it absent
    /// creates it and answers as the first.
    pub marker: PathBuf,
    /// Report this many targets fewer than the first instance reports.
    pub drop_targets: Option<usize>,
    /// Refuse `prepare` and `invoke` with this message.
    pub refuse: Option<String>,
}

impl Script {
    /// Load from the environment. An absent or unreadable script is not an error: the plugin
    /// then answers every invocation with `ok`. Both shapes are accepted, so a test can write
    /// the short form `[{"exit":1}]` instead of `{"directives":[{"exit":1}]}`.
    pub fn from_env() -> Script {
        let text = std::env::var("AFZ_FAKE_SCRIPT").ok().or_else(|| {
            std::env::var("AFZ_FAKE_SCRIPT_FILE")
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
        });
        let Some(text) = text else {
            return Script::default();
        };
        match serde_json::from_str::<Script>(&text) {
            Ok(script) => script,
            Err(struct_err) => match serde_json::from_str::<Vec<Directive>>(&text) {
                Ok(directives) => Script {
                    directives,
                    ..Script::default()
                },
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
    // `discover` calls served on this connection, so a script can answer a later one differently.
    let mut discovers = 0u64;
    // Whether an instance of this plugin has already started, for the rules that compare two of
    // them. The marker says so, and the marker is named after the process that started the
    // instances, so a later run finds none and cannot be told it is not the first.
    let second = script
        .second_instance
        .as_ref()
        .map(|si| {
            let marker = instance_marker(&si.marker);
            let started_before = marker.is_file();
            if !started_before {
                if let Some(parent) = marker.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&marker, b"");
            }
            started_before
        })
        .unwrap_or(false);
    let fewer = script
        .second_instance
        .as_ref()
        .filter(|_| second)
        .and_then(|si| si.drop_targets)
        .unwrap_or(0);
    let second_refusal = second
        .then(|| {
            script
                .second_instance
                .as_ref()
                .and_then(|si| si.refuse.clone())
        })
        .flatten();

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
                    let _ = write_line(
                        &mut write,
                        &Response::uncorrelated(PARSE_ERROR, "malformed request"),
                    );
                }
                continue;
            }
        };
        let Some(method) = Method::from_wire(&req.method) else {
            let _ = write_line(
                &mut write,
                &Response::refusal(
                    req.id,
                    METHOD_NOT_FOUND,
                    format!("unknown method {}", req.method),
                ),
            );
            continue;
        };

        let gated = script.require_discover.unwrap_or(false) && !discovered;
        let refused = Response::refusal(
            req.id,
            INVALID_REQUEST,
            "discover has not run on this connection",
        );
        // A script that names a method here gets those bytes back as the result, so a rule about
        // an off-schema answer has an off-schema answer to catch; the typed arms below cannot
        // produce one, because their values are built from the types the schemas were drawn from.
        let resp = match script.raw.get(req.method.as_str()) {
            Some(value) => Response::result(req.id, value.clone()),
            None => match method {
                Method::Handshake => {
                    Response::result(req.id, script.handshake.clone().unwrap_or_else(handshake))
                }
                Method::Discover => {
                    discovered = true;
                    discovers += 1;
                    let scripted = if discovers > 1 {
                        script
                            .discover_second
                            .clone()
                            .or_else(|| script.discovery.clone())
                    } else {
                        script.discovery.clone()
                    };
                    let mut answered = scripted.unwrap_or_else(inventory);
                    answered.targets.truncate(answered.targets.len().saturating_sub(fewer));
                    Response::result(req.id, answered)
                }
                Method::Prepare if gated => refused,
                Method::Invoke if gated => refused,
                Method::Prepare if script.refuse_prepare.is_some() => Response::refusal(
                    req.id,
                    INVALID_REQUEST,
                    script.refuse_prepare.as_deref().unwrap_or_default(),
                ),
                Method::Prepare if second_refusal.is_some() => Response::refusal(
                    req.id,
                    INVALID_REQUEST,
                    second_refusal.as_deref().unwrap_or_default(),
                ),
                Method::Prepare => Response::result(req.id, prepare_resp(&req)),
                Method::CoverageMap if script.drop_coverage_map.is_some() => Response::refusal(
                    req.id,
                    PLUGIN_ERROR,
                    script.drop_coverage_map.as_deref().unwrap_or_default(),
                ),
                Method::CoverageMap => Response::result(req.id, coverage_map()),
                Method::Invoke if second_refusal.is_some() => Response::refusal(
                    req.id,
                    INVALID_REQUEST,
                    second_refusal.as_deref().unwrap_or_default(),
                ),
                Method::Invoke => {
                    invocations += 1;
                    invoke_resp(req.id, &req, &script, invocations)
                }
                Method::Minimize => Response::result(req.id, minimize_resp(&req)),
                Method::Reproducer => Response::result(req.id, reproducer_resp()),
                Method::Ready => Response::result(
                    req.id,
                    script.ready.clone().unwrap_or(ReadyResp {
                        ready: true,
                        detail: None,
                    }),
                ),
            },
        };
        if write_line(&mut write, &resp).is_err() {
            return;
        }
    }
}

/// `marker` with the id of the process that started this plugin appended, so the instances one run
/// starts share a marker and a later run, started by a different process, finds none. Without a
/// parent process id every run shares the marker, and the key that reads it is used on Linux only.
#[cfg(unix)]
fn instance_marker(marker: &Path) -> PathBuf {
    marker.with_extension(format!("run{}", std::os::unix::process::parent_id()))
}

#[cfg(not(unix))]
fn instance_marker(marker: &Path) -> PathBuf {
    marker.to_path_buf()
}

fn handshake() -> Handshake {
    Handshake {
        protocol_version: PROTOCOL_VERSION.to_string(),
        name: PLUGIN_NAME.to_string(),
        version: PLUGIN_VERSION.to_string(),
        target_class: TargetClass::DesignTime,
        capabilities: vec![
            Capability::Coverage,
            Capability::Stateful,
            Capability::Minimize,
            Capability::Reproducer,
            Capability::Concurrent,
        ],
        languages: vec!["fake".to_string()],
        discover_requires_execution: false,
    }
}

fn param(name: &str, ty: TypeNode) -> Parameter {
    Parameter {
        name: name.to_string(),
        ty,
        required: true,
    }
}

fn target(
    id: &str,
    name: &str,
    file: &str,
    line: u32,
    parameters: Vec<Parameter>,
    materialization: Materialization,
    guard_sites: Vec<u32>,
) -> TargetDescriptor {
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
            Field {
                name: "v".into(),
                ty: TypeNode::number(),
                required: true,
            },
            Field {
                name: "to".into(),
                ty: TypeNode::Literal {
                    values: vec!["USD".into(), "EUR".into(), "ZZZ".into()],
                },
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
                vec![
                    param("amount", TypeNode::number()),
                    param("to", TypeNode::string()),
                ],
                Materialization::Constructible,
                vec![5],
            ),
            target(
                "afz:t:0003",
                "bulk",
                "src/fake.ts",
                24,
                vec![
                    param("items", TypeNode::array(money)),
                    param("rate", TypeNode::number()),
                ],
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
                vec![param(
                    "clock",
                    TypeNode::Opaque {
                        ty: "Clock".into(),
                        module: Some("src/clock.ts".into()),
                    },
                )],
                Materialization::FixtureRequired,
                Vec::new(),
            ),
            target(
                "afz:t:0006",
                "needsDb",
                "src/repo.ts",
                8,
                vec![param(
                    "db",
                    TypeNode::Opaque {
                        ty: "Db".into(),
                        module: Some("src/db.ts".into()),
                    },
                )],
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
    let req: PrepareReq =
        serde_json::from_value(req.params.clone().unwrap_or_default()).unwrap_or(PrepareReq {
            target_id: String::new(),
            values: Vec::new(),
        });
    if req.target_id == "afz:t:0006" {
        return PrepareResp::Skip {
            reason: SkipReason::UnconstructibleParameter,
        };
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
            fault: FaultSignature {
                class: String::new(),
                message: String::new(),
                frames: Vec::new(),
            },
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
        script:
            "afz fake reproducer: replay the finding's inputs/afz-f-*.json through the plugin.\n"
                .to_string(),
    }
}

fn invoke_resp(id: u64, req: &Request, script: &Script, index: u64) -> Response {
    let Ok(req) = serde_json::from_value::<InvokeReq>(req.params.clone().unwrap_or_default())
    else {
        return Response::refusal(id, INVALID_REQUEST, "malformed invoke params");
    };
    let Some(d) = script.directive_for(index, &req.target_id) else {
        return Response::result(id, default_ok(index, &req, None));
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
        return Response::refusal(id, PLUGIN_ERROR, message.clone());
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
    Response::result(id, timed(outcome))
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

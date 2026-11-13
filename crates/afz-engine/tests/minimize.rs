//! Step 8 acceptance: the committed 1842-byte input minimizes below 200 bytes against the real
//! plugin, and the reproducer the plugin renders for the minimized input reproduces the fault
//! when Node runs it on its own, with no engine and no plugin process in sight.
//!
//! This needs the `js-ts` plugin installed: run `afz install-harness
//! https://github.com/DustinEwan/autofuzz-harness-js-ts.git` first.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use afz_engine::minimize::{minimize, FaultMatch};
use afz_engine::rpc::client::{Launch, PluginClient};
use afz_protocol::{DiscoverReq, InvokeReq, Outcome, ReproducerReq, ValueTree};

fn target_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/targets/minimize")
}

fn launch() -> Launch {
    afz_engine::harnesses::launch("js-ts")
        .expect("install the js-ts harness first: afz install-harness https://github.com/DustinEwan/autofuzz-harness-js-ts.git")
        .with_cwd(target_root().display().to_string())
}

/// The runtime the plugin ran on: the reproducer must run on the same Node build, not on
/// whatever `node` happens to be on the PATH.
fn plugin_program() -> String {
    launch().argv.first().expect("the recorded launch has a program").clone()
}

#[test]
fn the_planted_input_minimizes_below_200_bytes_and_still_reproduces() {
    let input_path = target_root().join("order-input.json");
    let text = std::fs::read_to_string(&input_path).expect("the committed target input");
    let values: Vec<ValueTree> = serde_json::from_str(&text).expect("the target input is a JSON array of ValueTree");
    let bytes_on_disk = text.len();
    assert_eq!(bytes_on_disk, 1842, "the plan names the target input's size exactly");
    // `bytesBefore` sums the arguments' own wire size; the file adds the two-byte JSON array
    // wrapper around them, so the two numbers differ by exactly that.
    let bytes_before: usize = values.iter().map(ValueTree::wire_bytes).sum();
    assert_eq!(bytes_before + 2, bytes_on_disk, "the file is the canonical wire form of the input");
    assert_eq!(serde_json::to_string(&values).expect("reserialize"), text);
    let launch = launch();
    let client = PluginClient::spawn(&launch).expect("spawn the js-ts plugin");
    let discovery = client
        .discover(
            &DiscoverReq {
                root: target_root().display().to_string(),
                include: Some(vec!["**/*.ts".to_string()]),
                exclude: None,
                config_path: None,
            },
            Duration::from_secs(120),
        )
        .expect("discover the minimize target");
    let target = discovery
        .targets
        .iter()
        .find(|t| t.name == "describe")
        .unwrap_or_else(|| panic!("discovery reported no `describe`; got {:?}", discovery.targets.iter().map(|t| &t.name).collect::<Vec<_>>()));

    let prepared = client
        .prepare(&afz_protocol::PrepareReq { target_id: target.id.clone(), values: values.clone() }, Duration::from_secs(30))
        .expect("prepare the target");
    assert!(matches!(prepared, afz_protocol::PrepareResp::Ok { .. }), "the target must be buildable: {prepared:?}");

    let invoke = |args: &[ValueTree]| -> Outcome {
        client
            .invoke(
                &InvokeReq {
                    invocation_id: 1,
                    target_id: target.id.clone(),
                    values: args.to_vec(),
                    deadline_ms: 5000,
                    collect_coverage: false,
                },
                Duration::from_secs(30),
            )
            .unwrap_or_else(|reason| Outcome::plugin_lost(0.0, reason.detail()))
    };

    let original = invoke(&values);
    let fault = original.fault().expect("the target input must fail");
    assert_eq!(fault.class, "TypeError", "{original:?}");
    let top = fault.top_frame().expect("a TypeError from the target carries a frame");
    let (file, line) = (top.file.clone(), top.line);
    assert!(file.ends_with("src/order.ts"), "{file}");

    let result = minimize(&values, 2000, Duration::MAX, 4, &mut |batch| batch.iter().map(|candidate| invoke(candidate)).collect());
    assert!(result.reproduces, "the original input must reproduce before anything is shrunk");
    // The plan's bound. The empty list is what makes `lines[0]` undefined, so a search that stops
    // dropping fields shows up here as a byte count that stays near the original, rather than in a
    // scan report where nobody compares it to anything.
    assert!(result.bytes_after < 200, "minimized to {} bytes after {} attempts", result.bytes_after, result.attempts);
    assert!(result.bytes_after < bytes_before / 4, "the shrink must be substantial, not cosmetic");

    let after = invoke(&result.values);
    let matched = FaultMatch::from_outcome(&original).expect("the original fault");
    assert!(matched.reproduces(&after), "the minimized input reports {after:?}");
    let after_top = after.fault().and_then(|f| f.top_frame().cloned()).expect("the minimized fault keeps its frame");
    assert_eq!((after_top.file.as_str(), after_top.line), (file.as_str(), line), "the bug moved to another line");

    // The reproducer is the artifact a triager runs. The plugin renders it — the engine holds
    // no target-language code — and it has to fail the same way on its own.
    let resp = client
        .reproducer(&ReproducerReq { target_id: target.id.clone(), values: result.values.clone() }, Duration::from_secs(10))
        .expect("the js-ts plugin renders a reproducer");
    assert_eq!(resp.extension, "mjs", "the js-ts plugin renders a Node-runnable module");
    let script = resp.script;
    let run_dir = std::env::temp_dir().join(format!("afz-minimize-{}", std::process::id()));
    std::fs::create_dir_all(&run_dir).expect("temp dir for the reproducer");
    let script_path = run_dir.join(format!("repro.{}", resp.extension));
    std::fs::write(&script_path, &script).expect("write the reproducer");
    let output = Command::new(plugin_program())
        .arg(&script_path)
        .output()
        .expect("run the reproducer");
    let _ = std::fs::remove_dir_all(&run_dir);

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(!output.status.success(), "the reproducer must fail the way the finding did; stdout: {}", String::from_utf8_lossy(&output.stdout));
    assert!(stderr.contains("TypeError"), "{stderr}");
    assert!(stderr.contains(&format!("order.ts:{line}")), "the reproducer failed somewhere else:\n{stderr}");
}

#[test]
fn minimization_refuses_a_candidate_that_changes_the_fault() {
    // No plugin here: the closure is a table, so this pins the rule itself rather than the
    // transport. The target breaks on the *second* element, so shrinking the list to the first
    // element must be rejected even though it is smaller.
    let values = vec![ValueTree::new(afz_protocol::ValueNode::Array {
        items: vec![ValueTree::string("padding"), ValueTree::string("")],
    })];
    let run = |args: &[ValueTree]| -> Outcome {
        let items = match &args[0].node {
            afz_protocol::ValueNode::Array { items } => items,
            _ => return Outcome::plugin_lost(0.0, "shape changed"),
        };
        let hits_empty = items.iter().any(|item| matches!(&item.node, afz_protocol::ValueNode::String { value } if value.is_empty()));
        if hits_empty {
            Outcome::Failed(afz_protocol::FailedOutcome {
                duration_ms: 1.0,
                coverage: None,
                via: "engine-error-outside-declared-channel".to_string(),
                fault: afz_protocol::FaultSignature {
                    class: "TypeError".to_string(),
                    message: "Cannot read properties of undefined (reading 'join')".to_string(),
                    frames: vec![afz_protocol::FaultFrame {
                        file: "/target/src/order.ts".to_string(),
                        line: 19,
                        column: None,
                        fn_name: None,
                    }],
                },
            })
        } else {
            Outcome::Ok(afz_protocol::OkOutcome { duration_ms: 1.0, memory_bytes: None, coverage: None, value: None })
        }
    };

    let result = minimize(&values, 200, Duration::MAX, 4, &mut |batch| batch.iter().map(|candidate| run(candidate)).collect());
    assert!(result.reproduces);
    let kept = match &result.values[0].node {
        afz_protocol::ValueNode::Array { items } => items,
        other => panic!("the container shape changed: {other:?}"),
    };
    assert!(
        kept.iter().any(|item| matches!(&item.node, afz_protocol::ValueNode::String { value } if value.is_empty())),
        "the element the fault depends on survived: {:?}",
        result.values[0]
    );
}

#[test]
fn minimization_drops_the_lines_before_the_one_that_breaks() {
    // The fault depends on one line near the end of a long document, which is the usual shape for a
    // parser input. A shrink that can only empty a string or keep its first half cannot express
    // "delete what came before", so every irrelevant line would stay in the reported input.
    let filler = (0..200).map(|at| format!("filler{at}\n")).collect::<String>();
    let values = vec![ValueTree::string(format!("{filler}boom\n"))];
    let run = |args: &[ValueTree]| -> Outcome {
        let text = match &args[0].node {
            afz_protocol::ValueNode::String { value } => value.clone(),
            other => return Outcome::plugin_lost(0.0, format!("the argument shape changed: {other:?}")),
        };
        if text.contains("boom") {
            broken()
        } else {
            accepted()
        }
    };

    let result = minimize(&values, 2000, Duration::MAX, 6, &mut |batch| batch.iter().map(|candidate| run(candidate)).collect());
    assert!(result.reproduces);
    let shrunk = match &result.values[0].node {
        afz_protocol::ValueNode::String { value } => value.clone(),
        other => panic!("the argument shape changed: {other:?}"),
    };
    assert!(shrunk.contains("boom"), "the line the fault depends on did not survive: {shrunk:?}");
    assert!(!shrunk.starts_with("filler0"), "the minimized input still carries the document's first line: {shrunk:?}");
    assert!(result.bytes_after * 10 < result.bytes_before, "shrunk {} bytes to {}", result.bytes_before, result.bytes_after);
    // Delta debugging halves what it rejects, so a return to one trial per line shows up here.
    assert!(result.attempts < 60, "the search cost {} trials for a 201-line document", result.attempts);
}

/// The fault every candidate has to reproduce: one class, one frame.
fn broken() -> Outcome {
    Outcome::Failed(afz_protocol::FailedOutcome {
        duration_ms: 1.0,
        coverage: None,
        via: "engine-error-outside-declared-channel".to_string(),
        fault: afz_protocol::FaultSignature {
            class: "TypeError".to_string(),
            message: "Cannot read properties of undefined".to_string(),
            frames: vec![afz_protocol::FaultFrame {
                file: "/target/src/document.ts".to_string(),
                line: 41,
                column: None,
                fn_name: None,
            }],
        },
    })
}

fn accepted() -> Outcome {
    Outcome::Ok(afz_protocol::OkOutcome { duration_ms: 1.0, memory_bytes: None, coverage: None, value: None })
}

#[test]
fn one_round_offers_candidates_for_every_node_of_the_input() {
    // Two independent containers, neither of which the fault depends on. A search that shrank one
    // node at a time could only offer candidates for one of them per round, which leaves the other
    // shrink connections with nothing to answer. One round now covers both, so the width the caller
    // asked for is actually reachable.
    let list = |prefix: &str| {
        ValueTree::new(afz_protocol::ValueNode::Array {
            items: (0..8).map(|at| ValueTree::string(format!("{prefix}{at}"))).collect(),
        })
    };
    let values = vec![list("a"), list("b")];
    let mut widest = 0usize;
    let mut spans_both = false;
    let result = minimize(&values, 500, Duration::MAX, 6, &mut |batch| {
        widest = widest.max(batch.len());
        for (at, left) in batch.iter().enumerate() {
            for right in &batch[at + 1..] {
                // Two candidates that differ in both argument slots changed different arguments.
                let differing = (0..left.len()).filter(|slot| left[*slot].wire_bytes() != right[*slot].wire_bytes()).count();
                if differing == 2 {
                    spans_both = true;
                }
            }
        }
        batch.iter().map(|_| broken()).collect()
    });
    assert!(result.reproduces);
    assert!(widest >= 6, "the widest round offered {widest} candidates to a width of 6");
    assert!(spans_both, "no round offered candidates for both arguments at once");
}

#[test]
fn minimization_never_returns_a_larger_input_than_it_was_given() {
    // `[null]` is 40 bytes on the wire and `[undefined]` is 45, so dropping the argument is a
    // candidate that both reproduces and grows the input. A real scan accepted it, then reported no
    // minimized input because the bytes had not shrunk, while writing the reproducer from the
    // larger tree.
    let values = vec![ValueTree::new(afz_protocol::ValueNode::Null)];
    let run = |args: &[ValueTree]| -> Outcome {
        match &args[0].node {
            afz_protocol::ValueNode::Null | afz_protocol::ValueNode::Undefined => broken(),
            _ => accepted(),
        }
    };

    let result = minimize(&values, 50, Duration::MAX, 4, &mut |batch| batch.iter().map(|candidate| run(candidate)).collect());
    assert!(result.reproduces, "the original input reproduces, so the finding is real");
    assert_eq!(result.bytes_after, result.bytes_before, "the input grew from {} to {} bytes", result.bytes_before, result.bytes_after);
    assert!(matches!(result.values[0].node, afz_protocol::ValueNode::Null), "the grown candidate was kept: {:?}", result.values[0]);
}

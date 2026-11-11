//! Step 8 acceptance: the committed 1842-byte input minimizes below 200 bytes against the real
//! plugin, and the `.ts` reproducer written for the minimized input reproduces the fault when
//! Node runs it on its own, with no engine and no plugin in sight.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use afz_engine::minimize::{minimize, FaultMatch};
use afz_engine::report::repro::{reproducer, ReproTarget};
use afz_engine::rpc::client::{Launch, PluginClient};
use afz_protocol::{DiscoverReq, InvokeReq, Outcome, ValueTree};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn fixture_root() -> PathBuf {
    repo_root().join("packages/harness-js/fixtures/minimize")
}

fn harness_package() -> PathBuf {
    repo_root().join("packages/harness-js")
}

fn node() -> String {
    std::env::var("AFZ_NODE").unwrap_or_else(|_| "node".to_string())
}

fn launch() -> Launch {
    let server = harness_package().join("dist/server.js");
    assert!(
        server.is_file(),
        "build the harness plugin first: npm --prefix packages/harness-js run build (looked for {})",
        server.display()
    );
    let mut launch = Launch::new([node(), server.display().to_string()]);
    launch.cwd = Some(harness_package().display().to_string());
    launch
}

#[test]
fn the_fixture_input_minimizes_below_200_bytes_and_still_reproduces() {
    let input_path = fixture_root().join("order-input.json");
    let text = std::fs::read_to_string(&input_path).expect("the committed fixture input");
    let values: Vec<ValueTree> = serde_json::from_str(&text).expect("the fixture input is a JSON array of ValueTree");
    let bytes_on_disk = text.len();
    assert_eq!(bytes_on_disk, 1842, "the plan names the fixture input's size exactly");
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
                root: fixture_root().display().to_string(),
                include: Some(vec!["**/*.ts".to_string()]),
                exclude: None,
                config_path: None,
            },
            Duration::from_secs(120),
        )
        .expect("discover the minimize fixture");
    let target = discovery
        .targets
        .iter()
        .find(|t| t.name == "describe")
        .unwrap_or_else(|| panic!("discovery reported no `describe`; got {:?}", discovery.targets.iter().map(|t| &t.name).collect::<Vec<_>>()));
    let origin = target.origin.file.clone().expect("a design-time target reports its source file");

    let prepared = client
        .prepare(&afz_protocol::PrepareReq { target_id: target.id.clone(), values: values.clone() }, Duration::from_secs(30))
        .expect("prepare the target");
    assert!(matches!(prepared, afz_protocol::PrepareResp::Ok { .. }), "the fixture target must be buildable: {prepared:?}");

    let mut invoke = |args: &[ValueTree]| -> Outcome {
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
    let fault = original.fault().expect("the fixture input must fail");
    assert_eq!(fault.class, "TypeError", "{original:?}");
    let top = fault.top_frame().expect("a TypeError from the target carries a frame");
    let (file, line) = (top.file.clone(), top.line);
    assert!(file.ends_with("src/order.ts"), "{file}");

    let result = minimize(&values, 2000, &mut invoke);
    assert!(result.reproduces, "the original input must reproduce before anything is shrunk");
    // The plan's bound. The run reaches 116 bytes in 13 attempts, leaving `{ lines: [] }`: the
    // empty list is what makes `lines[0]` undefined, and everything else was noise.
    // 116 bytes is well under the bound, so a minimizer that stops dropping fields is caught
    // here rather than in a scan report.
    assert!(result.bytes_after < 200, "minimized to {} bytes after {} attempts", result.bytes_after, result.attempts);
    assert!(result.bytes_after < bytes_before / 4, "the shrink must be substantial, not cosmetic");

    let after = invoke(&result.values);
    let matched = FaultMatch::from_outcome(&original).expect("the original fault");
    assert!(matched.reproduces(&after), "the minimized input reports {after:?}");
    let after_top = after.fault().and_then(|f| f.top_frame().cloned()).expect("the minimized fault keeps its frame");
    assert_eq!((after_top.file.as_str(), after_top.line), (file.as_str(), line), "the bug moved to another line");

    // The reproducer is the artifact a triager runs. It has to fail the same way on its own.
    let script = reproducer(
        &ReproTarget { target_id: &target.id, file: &origin, symbol: &target.name },
        &result.values,
    );
    let run_dir = std::env::temp_dir().join(format!("afz-minimize-{}", std::process::id()));
    std::fs::create_dir_all(&run_dir).expect("temp dir for the reproducer");
    let script_path = run_dir.join("repro.ts");
    std::fs::write(&script_path, &script).expect("write the reproducer");
    let output = Command::new(node())
        .arg("--experimental-strip-types")
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
    let mut run = |args: &[ValueTree]| -> Outcome {
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
                        file: "/fixture/src/order.ts".to_string(),
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

    let result = minimize(&values, 200, &mut run);
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

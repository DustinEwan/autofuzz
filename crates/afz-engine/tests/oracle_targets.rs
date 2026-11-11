//! Step 7 acceptance: four JavaScript test targets, the real `js-ts` plugin, the real supervisor.
//!
//! Each target is one exported function and one input that drives it down a different channel.
//! Nothing here is stubbed: the plugin classifies the call, the engine enforces the deadline by
//! killing and restarting a process, and the oracles decide what becomes a finding. The expected
//! result is the plan's own list — 0 findings, then one each of `failed`, `deadline`, and
//! `memoryGrowth`.
//!
//! This needs the built plugin, so run `npm --prefix packages/harness-js run build` first. Set
//! `AFZ_NODE` when `node` on the PATH is not a Linux Node 24.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use afz_engine::config::OraclesConfig;
use afz_engine::oracles::{Finding, Oracles};
use afz_engine::rpc::client::{Launch, PluginClient};
use afz_engine::supervise::{Job, Supervisor, DISABLE_AFTER};
use afz_protocol::{DiscoverReq, Outcome, ValueNode, ValueTree};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn harness_package() -> PathBuf {
    repo_root().join("packages/harness-js")
}

fn target_root() -> PathBuf {
    harness_package().join("test-targets/oracles")
}

fn launch() -> Launch {
    let server = harness_package().join("dist/server.js");
    assert!(
        server.is_file(),
        "build the harness plugin first: npm --prefix packages/harness-js run build (looked for {})",
        server.display()
    );
    let node = std::env::var("AFZ_NODE").unwrap_or_else(|_| "node".to_string());
    let mut launch = Launch::new([node, server.display().to_string()]);
    launch.cwd = Some(harness_package().display().to_string());
    launch
}

/// The scope every plugin process is started with, so `prepare` can resolve the ids below.
fn discovery_req() -> DiscoverReq {
    DiscoverReq {
        root: target_root().display().to_string(),
        include: Some(vec!["**/*.ts".to_string()]),
        exclude: None,
        config_path: None,
    }
}

/// The four test targets by exported name, which is how the test says which input belongs to
/// which channel without hard-coding the ids discovery derives from the file contents.
fn discover_targets() -> BTreeMap<String, String> {
    let launch = launch();
    let client = PluginClient::spawn(&launch).expect("spawn the js-ts plugin");
    let discovery = client
        .discover(
            &discovery_req(),
            Duration::from_secs(120),
        )
        .expect("discover the oracle targets");
    let mut names: Vec<&str> = discovery.targets.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["drain", "expand", "normalize", "priced"], "unexpected inventory: {names:?}");
    discovery.targets.into_iter().map(|t| (t.name, t.id)).collect()
}

fn job(invocation_id: u64, target_id: String, values: Vec<ValueTree>) -> Job {
    Job { invocation_id, target_id, values, collect_coverage: false }
}

#[test]
fn the_four_targets_produce_the_four_verdicts() {
    let targets = discover_targets();
    let id = |name: &str| targets.get(name).unwrap_or_else(|| panic!("discovery reported no {name}; got {targets:?}")).clone();

    let jobs = vec![
        job(1, id("priced"), vec![ValueTree::string("widget"), ValueTree::number(0.0)]),
        job(2, id("normalize"), vec![ValueTree::new(ValueNode::Object { fields: vec![] })]),
        job(3, id("expand"), vec![ValueTree::number(300.0)]),
        job(4, id("drain"), vec![ValueTree::new(ValueNode::Array { items: vec![ValueTree::number(13.0)] })]),
    ];

    let deadline = Oracles::new(&OraclesConfig::default()).invocation_deadline().expect("the deadline oracle is on by default");
    let mut supervisor = Supervisor::new(launch(), 1, deadline, DISABLE_AFTER, discovery_req());
    let report = supervisor.run(jobs, Duration::from_secs(240));

    assert_eq!(report.completed.len(), 4, "every invocation must be accounted for: {:?}", report.completed.iter().map(|c| (c.invocation_id, c.outcome.kind())).collect::<Vec<_>>());
    // The spinning target is stopped by the engine, which costs one plugin process.
    assert!(report.restarts >= 1, "the deadline must restart a process, restarts={}", report.restarts);

    let engine = Oracles::new(&OraclesConfig::default());
    let mut by_invocation: BTreeMap<u64, Vec<Finding>> = BTreeMap::new();
    for completed in &report.completed {
        for finding in engine.evaluate(&completed.target_id, completed.invocation_id, &completed.outcome) {
            by_invocation.entry(completed.invocation_id).or_default().push(finding);
        }
    }

    let outcome_of = |invocation_id: u64| {
        report
            .completed
            .iter()
            .find(|c| c.invocation_id == invocation_id)
            .unwrap_or_else(|| panic!("invocation {invocation_id} was not completed"))
            .outcome
            .clone()
    };
    let findings_of = |invocation_id: u64| by_invocation.get(&invocation_id).cloned().unwrap_or_default();
    let oracle_names = |invocation_id: u64| {
        findings_of(invocation_id)
            .iter()
            .map(|f| f.oracle.as_str().to_string())
            .collect::<Vec<String>>()
    };

    // 1. A `RangeError` at a guard site is the target refusing the input. No finding, in any
    //    oracle, with the default configuration.
    let guard = outcome_of(1);
    assert_eq!(guard.kind(), "rejected", "{guard:?}");
    assert_eq!(guard.via(), Some("guard-site"), "{guard:?}");
    assert!(findings_of(1).is_empty(), "a guard-site rejection must be silent, got {:?}", oracle_names(1));

    // 2. `undefined.map` outside any guard is the target breaking.
    let deep = outcome_of(2);
    assert_eq!(deep.kind(), "failed", "{deep:?}");
    assert_eq!(deep.via(), Some("engine-error-outside-declared-channel"), "{deep:?}");
    assert_eq!(deep.fault().expect("fault").class, "TypeError");
    let findings = findings_of(2);
    assert_eq!(oracle_names(2), vec!["failed".to_string()], "{findings:?}");
    assert_eq!(findings[0].severity, afz_engine::config::Severity::High);

    // 3. A call that never returns is stopped by the engine and reported once, as a deadline.
    let spin = outcome_of(4);
    assert_eq!(spin.kind(), "failed", "{spin:?}");
    assert_eq!(spin.via(), Some("deadline"), "{spin:?}");
    assert_eq!(oracle_names(4), vec!["deadline".to_string()]);
    assert_eq!(findings_of(4)[0].severity, afz_engine::config::Severity::Medium);

    // 4. An allocation past the oracle's megabytes is a finding even though the call returned.
    let grow = outcome_of(3);
    assert_eq!(grow.kind(), "ok", "{grow:?}");
    let memory = match &grow {
        Outcome::Ok(ok) => ok.memory_bytes.expect("the plugin reports a heap delta"),
        other => panic!("expected an ok outcome, got {other:?}"),
    };
    assert!(memory > 256 * 1024 * 1024, "the target allocated {memory} bytes");
    assert_eq!(oracle_names(3), vec!["memoryGrowth".to_string()]);

    // One finding per channel, and no oracle reporting twice.
    assert_eq!(by_invocation.values().map(Vec::len).sum::<usize>(), 3, "{by_invocation:?}");
}

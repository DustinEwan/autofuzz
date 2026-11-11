//! The transport contract: a plugin that dies is a result, not an engine error.
//!
//! These tests drive the real `afz-fakeplugin` binary over real pipes, so the framing, the
//! request-id correlation, and the loss classification are all exercised as the supervisor
//! will see them from Step 5 onward.

use std::path::PathBuf;
use std::time::Duration;

use afz_engine::rpc::client::{Launch, LostReason, PluginClient};
use afz_protocol::*;

/// The fake plugin is a binary of `afz-xtask`, so this crate's tests cannot use
/// `CARGO_BIN_EXE_…`, which only covers the current package. A test binary sits in
/// `<target>/<profile>/deps/`, one level below the plugin, which is a stable relationship.
fn fakeplugin_path() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable");
    let profile_dir = exe.parent().and_then(|d| d.parent()).expect("target profile dir");
    let candidate = profile_dir.join("afz-fakeplugin");
    assert!(candidate.is_file(), "build the fake plugin: `cargo build --workspace` (looked in {})", candidate.display());
    candidate
}

fn fake(script: Option<&str>) -> PluginClient {
    let mut launch = Launch::new([fakeplugin_path().display().to_string()]);
    if let Some(script) = script {
        launch = launch.with_env("AFZ_FAKE_SCRIPT", script);
    }
    PluginClient::spawn(&launch).expect("fake plugin starts")
}

fn invoke_req(index: u64, target: &str) -> InvokeReq {
    InvokeReq {
        invocation_id: index,
        target_id: target.to_string(),
        values: vec![ValueTree::number(1.0)],
        deadline_ms: 2000,
        collect_coverage: true,
    }
}

#[test]
fn handshake_discover_and_invoke_over_real_pipes() {
    let client = fake(None);
    let handshake = client.handshake().expect("handshake");
    assert_eq!(handshake.protocol_version, PROTOCOL_VERSION);
    assert_eq!(handshake.name, "afz-fakeplugin");
    assert!(handshake.capabilities.contains(&Capability::Coverage));

    let discovery = client
        .discover(&DiscoverReq { root: ".".into(), ..Default::default() }, Duration::from_secs(30))
        .expect("discover");
    assert_eq!(discovery.targets.len(), 6, "the fixed inventory has six targets");
    assert_eq!(discovery.targets.iter().filter(|t| t.is_fuzzable()).count(), 4);
    assert_eq!(discovery.skipped.len(), 7);

    let outcome = client.invoke(&invoke_req(1, "afz:t:0001"), Duration::from_secs(5)).expect("invoke");
    assert_eq!(outcome.kind(), "ok");
    assert_eq!(outcome.coverage().map(|c| c.to_vec()), Some(vec![2]), "coverage follows the invocation index");

    let prepared = client
        .prepare(&PrepareReq { target_id: "afz:t:0001".into(), values: vec![ValueTree::number(1.0)] }, Duration::from_secs(5))
        .expect("prepare");
    assert!(prepared.ok());
    let skipped = client
        .prepare(&PrepareReq { target_id: "afz:t:0006".into(), values: Vec::new() }, Duration::from_secs(5))
        .expect("prepare");
    assert_eq!(skipped, PrepareResp::Skip { reason: SkipReason::UnconstructibleParameter });

    let minimized = client
        .minimize(
            &MinimizeReq {
                target_id: "afz:t:0001".into(),
                values: vec![ValueTree::number(1.0), ValueTree::string("x")],
                fault: FaultSignature { class: "TypeError".into(), message: "m".into(), frames: Vec::new() },
                budget_ms: 1000,
            },
            Duration::from_secs(5),
        )
        .expect("minimize");
    assert_eq!(minimized.values.len(), 1, "the fake plugin shrinks to the first argument");

    let map = client.coverage_map(Duration::from_secs(5)).expect("coverageMap");
    assert_eq!(map.entries.len(), 8);
}

#[test]
fn invocation_ids_are_attributable_across_calls() {
    // Out-of-order responses from parallel plugin processes must stay attributable, so the
    // client must not reuse an id and must not deliver a response to the wrong caller.
    let client = fake(None);
    let mut ids = Vec::new();
    for i in 0..5 {
        let outcome = client.invoke(&invoke_req(i, "afz:t:0001"), Duration::from_secs(5)).expect("invoke");
        // The fake plugin's coverage edge is a function of its own invocation counter, which
        // tracks the engine's request order one-for-one.
        ids.push(outcome.coverage().unwrap().to_vec());
    }
    assert_eq!(ids, vec![vec![2u32], vec![3], vec![4], vec![5], vec![6]], "responses arrived misattributed");
}

#[test]
fn plugin_exit_becomes_plugin_lost_not_an_error() {
    let client = fake(Some(r#"[{"at":1,"exit":1}]"#));
    let outcome = client.invoke_or_lose(&invoke_req(1, "afz:t:0001"), Duration::from_secs(5));
    let Outcome::Failed(f) = &outcome else { panic!("a dead plugin is a failure, got {outcome:?}") };
    assert_eq!(f.via, "plugin-lost");
    assert_eq!(f.fault.class, "PluginLost");
    assert!(f.fault.message.contains("code 1"), "message was {:?}", f.fault.message);
    assert_eq!(client.lost(), Some(LostReason::Exited { code: Some(1), signal: None }));
}

#[test]
fn engine_kill_is_attributable_to_the_engine() {
    // A plugin that sleeps past the deadline is killed. The engine must know it asked, so the
    // supervisor can call the invocation a deadline rather than a crash.
    let client = fake(Some(r#"[{"at":1,"sleepMs":5000}]"#));
    let slow = client.invoke(&invoke_req(1, "afz:t:0001"), Duration::from_millis(200));
    assert_eq!(slow, Err(LostReason::NoAnswer));
    let started = std::time::Instant::now();
    client.kill();
    assert!(started.elapsed() < Duration::from_secs(5), "kill waited for the sleeping plugin");
    assert_eq!(client.lost(), Some(LostReason::Killed));
    assert!(client.lost().unwrap().engine_requested());
    // A later call fails fast with the recorded reason instead of hanging.
    assert_eq!(client.invoke(&invoke_req(2, "afz:t:0001"), Duration::from_secs(5)), Err(LostReason::Killed));
}

#[test]
fn slow_answer_is_no_answer_not_a_hang() {
    let client = fake(Some(r#"[{"at":1,"sleepMs":1500}]"#));
    let started = std::time::Instant::now();
    let slow = client.invoke(&invoke_req(1, "afz:t:0001"), Duration::from_millis(150));
    assert_eq!(slow, Err(LostReason::NoAnswer));
    assert!(started.elapsed() < Duration::from_millis(1200), "the call outlived its own deadline");
    // The plugin recovers and answers the next call, so the worker keeps working.
    let next = client.invoke(&invoke_req(2, "afz:t:0001"), Duration::from_secs(5)).expect("next invoke");
    assert_eq!(next.kind(), "ok");
    drop(client);
}

#[test]
fn rpc_error_is_reported_as_a_failure_with_the_plugin_message() {
    let client = fake(Some(r#"[{"at":1,"rpcError":"target not found"}]"#));
    let err = client.invoke(&invoke_req(1, "afz:t:0001"), Duration::from_secs(5)).unwrap_err();
    let LostReason::Spawn(message) = err else { panic!("expected a plugin error, got {err:?}") };
    assert!(message.contains("target not found"), "message was {message}");
    assert_eq!(client.lost(), None, "a plugin that answers an error is still alive");
}

#[test]
fn stderr_reaches_the_engine_log() {
    let client = fake(Some(r#"[{"at":1,"stderr":"target said hello"}]"#));
    client.invoke(&invoke_req(1, "afz:t:0001"), Duration::from_secs(5)).expect("invoke");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        {
            let lines = client.stderr_lines.lock().expect("stderr lock");
            if lines.iter().any(|l| l.contains("target said hello")) {
                break;
            }
        }
        assert!(std::time::Instant::now() < deadline, "stderr never drained");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn spawn_failure_is_a_loss_not_a_panic() {
    let launch = Launch::new(["/nonexistent/afz-plugin-that-does-not-exist"]);
    let err = PluginClient::spawn(&launch).unwrap_err();
    assert!(matches!(err, LostReason::Spawn(_)), "got {err:?}");
    assert!(err.detail().contains("afz-plugin-that-does-not-exist"), "detail was {}", err.detail());
}

#[test]
fn dropping_the_client_leaves_no_plugin_behind() {
    let client = fake(None);
    let pid = client.pid();
    assert!(std::path::Path::new(&format!("/proc/{pid}")).exists(), "the plugin should be alive while the client is");
    client.invoke(&invoke_req(1, "afz:t:0001"), Duration::from_secs(5)).expect("invoke");
    drop(client);
    // `/proc/<pid>` disappears once the reaper has waited the child, so this also proves the
    // engine reaped it rather than leaving a zombie.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        assert!(std::time::Instant::now() < deadline, "plugin {pid} outlived its client");
        std::thread::sleep(Duration::from_millis(10));
    }
}

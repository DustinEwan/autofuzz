//! Supervision against a real plugin process: a missed deadline and a plugin that exits are
//! results the scan keeps going after, not engine errors.
//!
//! These drive the `afz-fakeplugin` binary, so the deadline is enforced by the engine against a
//! process that genuinely does not answer, and the restart genuinely brings a new process up.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use afz_engine::supervise::{Completed, DISABLE_AFTER, Job, RunReport, Supervisor};
use afz_engine::config::OraclesConfig;
use afz_engine::oracles::{Oracle, Oracles};
use afz_engine::rpc::client::Launch;
use afz_protocol::{DiscoverReq, SkipReason, ValueTree};

/// The fake plugin is a binary of the `xtask` package, so `CARGO_BIN_EXE_…` does not cover it
/// from here.
fn fakeplugin_path() -> PathBuf {
    // cargo places a test executable under `<target>/<profile>/deps/` or under
    // `<target>/<profile>/build/<package>/<hash>/out/` depending on its version: walk every
    // ancestor for the plugin instead of assuming the depth.
    let exe = std::env::current_exe().expect("test executable");
    let mut dir = exe.parent().expect("test executable in a directory");
    loop {
        let candidate = dir.join("afz-fakeplugin");
        if candidate.is_file() {
            return candidate;
        }
        let Some(parent) = dir.parent() else {
            panic!("build the fake plugin: `cargo build --workspace` (no afz-fakeplugin above {})", exe.display());
        };
        dir = parent;
    }
}

fn supervisor(script: &str, workers: usize, deadline_ms: u64) -> Supervisor {
    let launch = Launch::new([fakeplugin_path().display().to_string()]).with_env("AFZ_FAKE_SCRIPT", script);
    // The fake plugin answers `discover` from a fixed inventory, so the scope here is only
    // what a real plugin would need to resolve a `targetId`.
    let discovery = DiscoverReq { root: ".".to_string(), ..Default::default() };
    Supervisor::new(launch, workers, Duration::from_millis(deadline_ms), DISABLE_AFTER, discovery)
}

fn jobs(target: &str, count: u64) -> Vec<Job> {
    (0..count)
        .map(|i| Job {
            invocation_id: i + 1,
            target_id: target.to_string(),
            values: vec![ValueTree::number(i as f64)],
            collect_coverage: true,
        })
        .collect()
}

/// Join two target groups so the ids interleave, which is what a real scan's queue looks like.
fn mixed(groups: &[(&str, u64)]) -> Vec<Job> {
    let mut out = Vec::new();
    let mut id = 0;
    let mut round = 0;
    loop {
        let mut added = false;
        for (target, count) in groups {
            if round < *count {
                id += 1;
                out.push(Job {
                    invocation_id: id,
                    target_id: target.to_string(),
                    values: vec![ValueTree::number(id as f64)],
                    collect_coverage: true,
                });
                added = true;
            }
        }
        round += 1;
        if !added {
            break;
        }
    }
    out
}

#[test]
fn a_plugin_that_misses_the_deadline_yields_deadline_findings_and_the_run_finishes() {
    // 5000 ms of sleep against a 300 ms deadline: the plugin is genuinely still running when
    // the engine gives up on it. `to` is explicit because a directive without it governs only
    // the single invocation index named by `at`.
    let script = r#"[{"sleepMs":5000,"at":1,"to":100000}]"#;
    let mut supervisor = supervisor(script, 1, 300);
    let started = Instant::now();
    let report = supervisor.run(jobs("afz:t:0001", 4), Duration::from_millis(4_000));
    let elapsed = started.elapsed();

    assert_eq!(report.completed.len(), 4, "every invocation still produced a result");
    for completed in &report.completed {
        assert_eq!(completed.outcome.kind(), "failed", "{:?}", completed.outcome);
        assert_eq!(completed.outcome.via(), Some("deadline"), "expected a deadline finding, got {:?}", completed.outcome);
        assert!(completed.outcome.coverage().unwrap_or(&[]).is_empty(), "a killed invocation reports no coverage");
        assert!(completed.restart.is_some(), "a deadline kills and restarts the plugin process");
    }
    assert_eq!(report.restarts, 4, "one restart per missed deadline");
    // The budget, not the plugin's 5000 ms, bounds the run.
    assert!(elapsed < Duration::from_millis(4_000 + 1_000), "the run took {elapsed:?}, past its budget plus slack");
}

#[test]
fn a_plugin_that_exits_yields_process_exit_findings_and_restarts() {
    let script = r#"[{"exit":1,"at":1,"to":100000}]"#;
    let mut supervisor = supervisor(script, 2, 2_000);
    let report = supervisor.run(jobs("afz:t:0002", 6), Duration::from_millis(20_000));

    assert_eq!(report.completed.len(), 6);
    for completed in &report.completed {
        assert_eq!(completed.outcome.kind(), "failed");
        assert_eq!(completed.outcome.via(), Some("processExit"), "expected a processExit finding, got {:?}", completed.outcome);
        let message = completed.outcome.fault().map(|f| f.message.clone()).unwrap_or_default();
        assert!(message.contains("code 1"), "the exit code should reach the finding: {message}");
    }
    assert!(report.restarts >= 6, "each exit must restart the process, saw {}", report.restarts);
    assert!(report.disabled.is_empty(), "six restarts must not disable a target");
}

#[test]
fn a_run_stops_at_its_wall_clock_and_accounts_for_every_job() {
    let script = r#"[{"sleepMs":400,"at":1,"to":100000}]"#;
    let mut supervisor = supervisor(script, 1, 1_000);
    let started = Instant::now();
    let report = supervisor.run(jobs("afz:t:0001", 1_000), Duration::from_millis(1_200));
    let elapsed = started.elapsed();

    assert!(!report.completed.is_empty(), "some invocations should have run");
    assert!(!report.unattempted.is_empty(), "a 1.2 s budget cannot run 1000 invocations");
    let attempted: Vec<u64> = report.completed.iter().map(|c| c.invocation_id).collect();
    let accounted: Vec<u64> = report.unattempted.to_vec();
    for id in 1..=1_000 {
        assert!(attempted.contains(&id) || accounted.contains(&id), "invocation {id} is unaccounted for");
    }
    assert!(elapsed < Duration::from_millis(1_200 + 1_500), "the run overran its budget: {elapsed:?}");
}

#[test]
fn fifty_consecutive_restarts_disable_that_target_and_leave_the_others_running() {
    let script = r#"[{"exit":1,"for":"afz:t:0003","at":1,"to":100000}]"#;
    let mut supervisor = supervisor(script, 1, 2_000);
    let report = supervisor.run(mixed(&[("afz:t:0003", DISABLE_AFTER + 12), ("afz:t:0001", 5)]), Duration::from_millis(60_000));

    assert_eq!(report.disabled, vec!["afz:t:0003".to_string()], "the bad target must be disabled");
    assert!(report.restarts >= DISABLE_AFTER, "restarts are counted: {}", report.restarts);
    let good: Vec<&Completed> = report.completed.iter().filter(|c| c.target_id == "afz:t:0001").collect();
    assert_eq!(good.len(), 5, "the healthy target keeps running");
    assert!(good.iter().all(|c| c.outcome.kind() == "ok"), "healthy invocations still report ok");
    // The jobs left in the queue for the disabled target were never attempted.
    let bad_attempted = report.completed.iter().filter(|c| c.target_id == "afz:t:0003").count();
    assert!(bad_attempted <= (DISABLE_AFTER as usize) + 1, "the engine kept invoking a target it had disabled: {bad_attempted}");
}

#[test]
fn a_target_the_plugin_refuses_to_build_is_reported_once_and_never_invoked() {
    // The fake plugin's sixth inventory entry has an unconstructible parameter.
    let mut supervisor = supervisor("[]", 1, 2_000);
    let report = supervisor.run(jobs("afz:t:0006", 4), Duration::from_millis(20_000));

    assert!(report.completed.is_empty(), "an unbuildable target must not be invoked");
    assert_eq!(report.unbuildable.len(), 1, "one report per target, not per invocation: {:?}", report.unbuildable);
    assert_eq!(report.unbuildable[0].target_id, "afz:t:0006");
    assert_eq!(report.unbuildable[0].reason, SkipReason::UnconstructibleParameter);
    assert_eq!(report.unattempted.len(), 4, "all four invocations are accounted for as unattempted");
    assert_eq!(report.restarts, 0, "a refusal is not a crash");
}

#[test]
fn parallel_workers_answer_every_invocation_exactly_once() {
    let mut supervisor = supervisor("[]", 4, 5_000);
    let report = supervisor.run(jobs("afz:t:0001", 40), Duration::from_millis(60_000));

    assert_eq!(report.completed.len(), 40, "every invocation answers once");
    let mut ids: Vec<u64> = report.completed.iter().map(|c| c.invocation_id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 40, "an invocation was answered twice");
    assert!(report.completed.iter().all(|c| c.outcome.kind() == "ok"));
    // Out-of-order answers are the point of parallel workers; the report is sorted anyway.
    assert!(report.completed.windows(2).all(|w| w[0].invocation_id < w[1].invocation_id));
    assert_eq!(report.restarts, 0, "a healthy plugin is not restarted");
}

/// `RunReport::restarts` describes the batch that produced it. The scan loop adds each batch's
/// number into the run total, so a report that repeated earlier batches inflated what a reader
/// sees: one wasm run printed 904328 restarts against a 20000-invocation budget.
#[test]
fn a_second_batch_reports_only_its_own_restarts() {
    let script = r#"[{"exit":1,"at":1,"to":100000}]"#;
    let mut supervisor = supervisor(script, 1, 2_000);
    let first = supervisor.run(jobs("afz:t:0002", 3), Duration::from_millis(20_000));
    let second = supervisor.run(jobs("afz:t:0002", 3), Duration::from_millis(20_000));

    let restarted = |report: &RunReport| report.completed.iter().filter(|c| c.restart.is_some()).count() as u64;
    assert_eq!(first.restarts, restarted(&first), "the first batch reports its own cost");
    assert_eq!(
        second.restarts,
        restarted(&second),
        "a batch must report what it cost, not the run total: {} reported, {} invocations restarted",
        second.restarts,
        restarted(&second)
    );
}

/// A harness that cuts a slow call itself answers a little after the deadline. The engine has to
/// let it answer: the answer carries that call's coverage, and killing the process instead costs a
/// restart on every slow invocation. The `deadline` finding is not lost, because the oracle reads
/// the duration the harness reports.
#[test]
fn a_plugin_that_answers_past_the_deadline_but_inside_the_grace_keeps_its_process() {
    // 400 ms against a 300 ms deadline: past the deadline, inside `KILL_GRACE`.
    let script = r#"[{"sleepMs":400,"at":1,"to":100000}]"#;
    let mut supervisor = supervisor(script, 1, 300);
    let report = supervisor.run(jobs("afz:t:0001", 3), Duration::from_millis(20_000));

    assert_eq!(report.restarts, 0, "a late answer must not cost a process: {} restarts", report.restarts);
    assert_eq!(report.completed.len(), 3, "every invocation is answered by the plugin");

    let mut config = OraclesConfig::default();
    config.deadline.ms = Some(300);
    let engine = Oracles::new(&config);
    for completed in &report.completed {
        assert_eq!(completed.outcome.kind(), "ok", "the plugin answered: {:?}", completed.outcome);
        assert!(
            completed.outcome.duration_ms() >= 300.0,
            "the call must show that it ran past the deadline: {} ms",
            completed.outcome.duration_ms()
        );
        let findings = engine.evaluate("afz:t:0001", completed.invocation_id, &completed.outcome);
        assert!(
            findings.iter().any(|f| f.oracle == Oracle::Deadline),
            "the deadline oracle must still fire on the reported duration: {findings:?}"
        );
    }
}

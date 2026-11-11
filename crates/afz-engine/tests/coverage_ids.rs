//! Coverage accounting against a plugin that dies mid-scan. `coverageEdges` is reported as a
//! fraction of the plugin's `coverageMap`, so it may only ever contain ids that map can name.

use std::path::PathBuf;
use std::time::Duration;

use afz_engine::config::Strategy;
use afz_engine::scan::{self, Options};

/// The fake plugin is a binary of the `xtask` package, so `CARGO_BIN_EXE_…` does not cover it from
/// here. A test binary sits in `<target>/<profile>/deps/`, one level below the plugin.
fn fakeplugin_path() -> PathBuf {
    let exe = std::env::current_exe().expect("test executable");
    let profile = exe.parent().and_then(|dir| dir.parent()).expect("target profile dir");
    let candidate = profile.join("afz-fakeplugin");
    assert!(candidate.is_file(), "build the fake plugin: `cargo build --workspace` (looked in {})", candidate.display());
    candidate
}

/// A scratch scan project whose plugin is scripted through the environment. The directory name is
/// unique per test so two runs cannot share an output directory.
fn project(name: &str, script: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("afz-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(&root).expect("scratch directory");
    let config = serde_json::json!({
        "project": name,
        "codebase": {
            "harness": "afz-fakeplugin",
            "command": [fakeplugin_path().display().to_string()],
            "env": { "AFZ_FAKE_SCRIPT": script },
        },
        "budget": { "invocations": 5, "wallClockMs": 20_000, "workers": 1 },
        "output": { "dir": ".autofuzz", "renderers": ["json"] },
    });
    std::fs::write(root.join("autofuzz.json"), config.to_string()).expect("scan project");
    root
}

/// A plugin that dies mid-call answers `plugin-lost`, which carries no coverage. The scan holds the
/// `coverage` capability from the handshake, so the outcome tuple must not stand in for edges: the ids
/// the tuple hashes are not in the coverage map, and counting them pushed `coverageEdges` past the map
/// it is reported against, which is how a wasm scan came to report 11 of 9 edges.
#[test]
fn an_invocation_that_never_reached_the_target_adds_no_coverage_edge() {
    // Each plugin process answers its first invocation with the fake's default digest - one edge, the
    // same id in every process - and answers its second by exiting, which the engine restarts.
    let root = project("coverage-ids", r#"[{"exit":1,"at":2}]"#);
    let loaded = afz_engine::config::load(&root.join("autofuzz.json")).expect("the scan project validates");
    let mut options = Options::new(loaded, root.clone());
    options.invocations = 5;
    options.workers = 1;
    options.seed = 0x5e3a_91c4;
    options.strategy = Strategy::Random;
    options.wall_clock = Duration::from_secs(20);

    let report = scan::run(&options, &mut |_| {}).expect("the scan completes");
    std::fs::remove_dir_all(&root).ok();

    let execution = &report.document.execution;
    assert!(execution.restarts >= 1, "the script has to kill a worker: {execution:?}");
    assert_eq!(execution.coverage_edges, 1, "the answered invocations report one edge between them: {execution:?}");
    assert_eq!(execution.corpus_size, 1, "an invocation that never reached the target teaches nothing: {execution:?}");
    assert!(execution.coverage_ratio <= 1.0, "coverage cannot exceed the map it is measured over: {execution:?}");
}

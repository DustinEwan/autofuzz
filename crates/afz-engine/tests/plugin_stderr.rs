//! What a plugin writes to stderr has to reach `engine.log`. The engine captures it at spawn and
//! reported it only when a plugin was already lost, so a scan whose plugin died on its own recorded
//! `plugin exited with code 1` and nothing else, and the reason was gone.

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
        "budget": { "invocations": 4, "wallClockMs": 20_000, "workers": 1 },
        "output": { "dir": ".autofuzz", "renderers": ["json"] },
    });
    std::fs::write(root.join("autofuzz.json"), config.to_string()).expect("scan project");
    root
}

fn scan(root: &PathBuf) -> scan::ScanReport {
    let loaded = afz_engine::config::load(&root.join("autofuzz.json")).expect("the scan project validates");
    let mut options = Options::new(loaded, root.clone());
    options.invocations = 4;
    options.workers = 1;
    options.seed = 0x5e3a_91c4;
    options.strategy = Strategy::Random;
    options.wall_clock = Duration::from_secs(20);
    scan::run(&options, &mut |_| {}).expect("the scan completes")
}

#[test]
fn a_plugin_diagnostic_appears_in_the_engine_log_once() {
    // The same line is written by every invocation, which is how a `prepare` refusal looks: twelve
    // workers repeat it, and a reader needs it once.
    let root = project("plugin-stderr", r#"[{"stderr":"target said hello"}]"#);
    let report = scan(&root);
    let log = std::fs::read_to_string(report.dir.join("engine.log")).expect("the scan writes its log");
    std::fs::remove_dir_all(&root).ok();

    assert!(log.contains("plugin said: target said hello"), "the diagnostic never reached the log:\n{log}");
    let repeats = log.matches("plugin said: target said hello").count();
    assert_eq!(repeats, 1, "the repeated line was reported {repeats} times:\n{log}");
}

#[test]
fn the_reason_a_plugin_gave_survives_its_death() {
    // The death itself is reported as an exit code. The line the plugin wrote before exiting is the
    // only thing in the artifact that says why.
    let root = project("plugin-died", r#"[{"at":1,"stderr":"the realm is out of memory"},{"at":2,"exit":1}]"#);
    let report = scan(&root);
    let log = std::fs::read_to_string(report.dir.join("engine.log")).expect("the scan writes its log");
    std::fs::remove_dir_all(&root).ok();

    assert!(log.contains("plugin said: the realm is out of memory"), "the last words of a dead plugin were dropped:\n{log}");
    assert!(report.document.execution.restarts >= 1, "the script has to kill a worker: {:?}", report.document.execution);
}

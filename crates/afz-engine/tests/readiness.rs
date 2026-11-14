//! The readiness gate: a harness that declares `readiness` is asked before any worker
//! spawns, and a not-ready answer aborts the scan with the harness's own reason.
//!
//! Both tests run the fake plugin with the same scripted inventory and the same
//! readiness-declaring handshake; only the `ready` answer differs. That pairing is the
//! point: the abort must be attributable to the answer, not to the declaration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use afz_engine::config::Strategy;
use afz_engine::scan::{self, ScanError, ScanReport};

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

fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("afz-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("src")).expect("scratch directory");
    root
}

/// The handshake the script answers: the conforming fake handshake, plus the capability.
fn declaring_handshake() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": afz_protocol::PROTOCOL_VERSION,
        "name": "afz-fakeplugin",
        "version": "0.0.0",
        "targetClass": "design-time",
        "capabilities": ["readiness"],
        "languages": ["fake"],
    })
}

/// One constructible target, so the scan that passes the gate has something to drive.
fn inventory() -> serde_json::Value {
    serde_json::json!({
        "targets": [{
            "id": "afz:t:0001",
            "targetClass": "design-time",
            "origin": { "file": "src/lib.js", "line": 1, "module": "src/lib.js" },
            "name": "entry1",
            "parameters": [{
                "name": "n",
                "type": { "kind": "scalar", "scalar": "number" },
                "required": true,
            }],
            "effects": {},
            "materialization": "constructible",
        }],
        "skipped": [],
    })
}

/// A scan project whose fake plugin declares `readiness`; `ready` is the scripted answer,
/// or absent to let the fake answer its always-ready default.
fn project(name: &str, ready: Option<serde_json::Value>) -> PathBuf {
    let root = scratch(name);
    let mut script = serde_json::json!({
        "handshake": declaring_handshake(),
        "discovery": inventory(),
    });
    if let Some(ready) = ready {
        script["ready"] = ready;
    }
    let config = serde_json::json!({
        "project": name,
        "harness": {
            "name": "afz-fakeplugin",
            "command": [fakeplugin_path().display().to_string()],
            "env": { "AFZ_FAKE_SCRIPT": script.to_string() },
        },
        "budget": { "invocations": 4, "wallClockMs": 20_000, "workers": 1 },
        "output": { "dir": ".autofuzz", "renderers": ["json"] },
    });
    std::fs::write(root.join("autofuzz.json"), config.to_string()).expect("scan project");
    root
}

fn scan(root: &Path) -> Result<ScanReport, ScanError> {
    let loaded = afz_engine::config::load(&root.join("autofuzz.json")).expect("the scan project validates");
    let mut options = scan::Options::new(loaded.clone(), root.to_path_buf());
    options.out_dir = root.join(".autofuzz");
    options.invocations = 4;
    options.workers = 1;
    options.seed = 0x5e3a_91c4;
    options.strategy = Strategy::Random;
    options.wall_clock = Duration::from_secs(20);
    scan::run(&options, &mut |_| {})
}

#[test]
fn a_not_ready_answer_aborts_the_scan_with_the_harness_reason() {
    let root = project(
        "readiness-abort",
        Some(serde_json::json!({ "ready": false, "detail": "no service is listening on http://127.0.0.1:8731" })),
    );
    let error = scan(&root).expect_err("a harness that says not ready must stop the scan");
    let ScanError::NotReady { detail } = &error else {
        panic!("the abort must be NotReady, not another error: {error}");
    };
    assert!(detail.contains("127.0.0.1:8731"), "the harness's reason must survive verbatim: {detail}");
    assert!(error.to_string().contains("not ready"), "the message must say what stopped the scan: {error}");
    // The scan stopped before writing: no scan directory was opened under the output dir.
    let scans = std::fs::read_dir(root.join(".autofuzz")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(scans, 0, "a scan aborted on readiness must not write a scan result");
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_ready_answer_lets_the_same_scan_run() {
    // Same handshake, same inventory: only the `ready` answer changed, so this passing is
    // attributable to the answer and not to the declaration. The fake answers its default
    // `{ready: true}` when the script carries no override.
    let root = project("readiness-pass", None);
    let report = scan(&root).unwrap_or_else(|e| panic!("a ready environment must run the scan: {e}"));
    assert_eq!(report.document.inventory.discovered, 1, "the scan ran the scripted inventory");
    std::fs::remove_dir_all(&root).ok();
}

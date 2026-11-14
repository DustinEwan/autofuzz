// Shared over `tests/mcp_wire.rs` (`mod fixture;`). Standalone, this file compiles to an
// empty test crate, so the helpers look unused there; allow that rather than duplicate them.
#![allow(dead_code)]

//! Scratch scan projects driven by the fake plugin. The `scratch`, `write`,
//! `fakeplugin_path`, `project`, and `scripted` helpers are the ones from
//! `crates/afz-engine/tests/git_scope.rs`, which are private to that test binary.

use std::path::{Path, PathBuf};

/// The fake plugin is a binary of the `xtask` package, so `CARGO_BIN_EXE_…` does not cover it
/// from here.
pub fn fakeplugin_path() -> PathBuf {
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

/// A scratch directory, removed by the caller's last statement, as the other engine tests do.
pub fn scratch(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("afz-mcp-{name}-{}", std::process::id()));
    std::fs::remove_dir_all(&root).ok();
    std::fs::create_dir_all(root.join("src")).expect("scratch directory");
    root
}

pub fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent directory");
    }
    std::fs::write(path, text).expect("write a source file");
}

/// One constructible target per listed source file, the fake plugin's `discover` answer.
fn targets(files: &[&str]) -> Vec<serde_json::Value> {
    files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            serde_json::json!({
                "id": format!("afz:t:{index:04}"),
                "targetClass": "design-time",
                "origin": { "file": file, "line": 1, "module": file },
                "name": format!("entry{index}"),
                "parameters": [{
                    "name": "n",
                    "type": { "kind": "scalar", "scalar": "number" },
                    "required": true,
                }],
                "effects": {},
                "materialization": "constructible",
            })
        })
        .collect()
}

/// A `discover` answer with one constructible target per listed source file.
pub fn scripted(files: &[&str]) -> String {
    serde_json::json!({ "discovery": { "targets": targets(files), "skipped": [] } }).to_string()
}

/// The same inventory, plus a first invocation that fails through the channel the oracle
/// table turns into a `failed` finding at `Severity::High`
/// (`crates/afz-engine/src/oracles/mod.rs`).
pub fn faulted(files: &[&str]) -> String {
    serde_json::json!({
        "directives": [{
            "at": 1,
            "outcome": {
                "kind": "failed",
                "durationMs": 5.0,
                "via": "engine-error-outside-declared-channel",
                "fault": {"class": "TypeError", "message": "rate must be positive", "frames": []}
            }
        }],
        "discovery": { "targets": targets(files), "skipped": [] },
    })
    .to_string()
}

/// A scan project whose plugin is the fake one, with the discovery it answers scripted inline.
pub fn project(name: &str, script: &str) -> PathBuf {
    let root = scratch(name);
    let config = serde_json::json!({
        "project": name,
        "harness": {
            "name": "afz-fakeplugin",
            "command": [fakeplugin_path().display().to_string()],
            "env": { "AFZ_FAKE_SCRIPT": script },
        },
        "budget": { "invocations": 4, "wallClockMs": 20_000, "workers": 1 },
        "output": { "dir": ".autofuzz", "renderers": ["json"] },
    });
    std::fs::write(root.join("autofuzz.json"), config.to_string()).expect("scan project");
    root
}

//! The conformance fixture for this repository's own scripted plugin.
//!
//! `afz validate-harness` renders a harness's fixture with that harness's emitter, and the
//! mechanism's runner reaches the same rules through the manifest's `conformance.emitter`, so the
//! scripted plugin needs an emitter of its own to be validated by either. It answers `discover`
//! from its `AFZ_FAKE_SCRIPT` rather than from the tree it is pointed at, so the fixture is one
//! scan project that names the plugin and nothing else: what the suite reads from it is a root, a
//! config path, and a discovery scope, which are the three things every rule that drives `prepare`
//! and `invoke` needs.

use std::path::{Path, PathBuf};

use afz_engine::config::ScanProject;

/// Write the fixture into `dir` and print nothing but the directory on success, which is the
/// convention `afz_conformance::harness::drive` expects of an emitter.
pub fn write_fixture(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let project = serde_json::json!({
        "project": "afz-fakeplugin",
        "harness": { "name": "afz-fakeplugin" },
        "oracles": {},
        "budget": {},
        "output": {},
    });
    let text = serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?;
    // Read back what was just written: a scan project this engine stopped loading fails here,
    // naming the file, rather than as an unreadable fixture deep inside the suite.
    let path = dir.join(afz_engine::config::CONFIG_FILE_NAME);
    std::fs::write(&path, text.clone() + "\n")
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    serde_json::from_str::<ScanProject>(&text).map_err(|e| {
        format!(
            "{} is not a scan project this engine loads: {e}",
            path.display()
        )
    })?;
    Ok(dir.to_path_buf())
}

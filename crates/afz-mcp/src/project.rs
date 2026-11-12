//! Scan-project resolution, the same steps `afz scan` takes (crates/afz-cli/src/scan.rs),
//! with the printing removed.
//!
//! The CLI's `run` is eight arguments and a terminal summary, so it cannot be called from a
//! tool; this is the resolution order it performs, in the same sequence, so `afz_probe` and
//! `afz_scan` see the same project `afz scan` would see for the same arguments.

use std::path::{Path, PathBuf};

use afz_engine::config::{self, ConfigError, LoadedConfig};

/// The resolved project: what to load, where to scan, and where results go.
pub struct Resolved {
    pub loaded: LoadedConfig,
    pub scan_root: PathBuf,
    pub out_dir: PathBuf,
}

/// `path` defaults to the scan project's own discovery root; `config` to the nearest
/// autofuzz.json above it.
pub fn resolve(path: Option<&str>, config_path: Option<&str>) -> Result<Resolved, String> {
    // Absolute before it reaches the plugin: the plugin's working directory is the scan
    // project's, not the harness's, and a relative path would resolve against the wrong place.
    let requested = std::path::absolute(path.unwrap_or(".")).map_err(|e| format!("cannot resolve the scan path: {e}"))?;
    let config_path = match config_path {
        Some(explicit) => PathBuf::from(explicit),
        None => config::discover(&requested).map_err(|e| match e {
            ConfigError::Message(message) => format!(
                "{message}\ncreate {} there, or pass the `config` argument with an absolute path; a relative `path` resolves against the server's working directory, which the client sets",
                config::CONFIG_FILE_NAME
            ),
            other => other.to_string(),
        })?,
    };
    let loaded = config::load(&config_path).map_err(|e| e.to_string())?;
    // With an explicit path the scan runs against that directory; without one, against the
    // project's own discovery root, exactly as `afz scan` decides.
    let scan_root = match path {
        Some(_) => requested,
        None => loaded.discovery_scope().map_err(|e| e.to_string())?.root,
    };
    let out_dir = loaded.root.join(loaded.project.output.dir.as_str());
    Ok(Resolved { loaded, scan_root, out_dir })
}

/// Whether a directory holds a readable scan result, used by `afz_findings` when it picks
/// the newest scan under an output directory.
pub fn has_scan_file(dir: &Path) -> bool {
    dir.join(afz_engine::report::scanresult::SCAN_FILE).is_file()
}

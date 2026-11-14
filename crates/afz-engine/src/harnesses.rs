//! The installed-harness registry: one directory per harness, written by `afz install-harness`.
//!
//! A scan project may name a harness (`harness.name`) without a `command`; the launch then
//! comes from the record this module owns. The engine never invents a runtime and never guesses
//! a plugin path: an uninstalled name is still an error, and `harness.command` stays the way
//! to run a plugin nobody installed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::absolutized;
use crate::rpc::client::Launch;

/// The `afz-harness.json` at the root of a harness repository. Flat, not an array: a
/// repository holds one harness.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct HarnessManifest {
    pub name: String,
    /// Dependency fetch: `npm ci` and anything else slow that a checkout needs once. Only
    /// `afz install-harness` runs these.
    pub install: Vec<Vec<String>>,
    /// Compilation of the plugin and the emitter. `afz install-harness` and
    /// `afz validate-harness` both run these, so a validation never tests a stale build.
    pub build: Vec<Vec<String>>,
    pub command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conformance: Option<Conformance>,
}

/// How this harness is validated. `emitter` is a program the driver runs as `<argv…> --out <dir>`:
/// it renders the whole conformance scan project — source tree, `autofuzz.json`, and
/// `afz-conformance-manifest.json`. That rendered project is the only project validation ever
/// drives, which is why `afz validate-harness` takes no scan project of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Conformance {
    pub emitter: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

/// `<AFZ_HOME or XDG_DATA_HOME/autofuzz or HOME/.local/share/autofuzz>/harnesses`.
pub fn root() -> PathBuf {
    let unset = |v: Option<std::ffi::OsString>| v.filter(|v| !v.is_empty()).map(PathBuf::from);
    let base = unset(std::env::var_os("AFZ_HOME"))
        .or_else(|| unset(std::env::var_os("XDG_DATA_HOME")).map(|d| d.join("autofuzz")))
        .unwrap_or_else(|| {
            unset(std::env::var_os("HOME")).unwrap_or_else(|| PathBuf::from(".")).join(".local/share/autofuzz")
        });
    base.join("harnesses")
}

/// Read `<dir>/afz-harness.json`.
pub fn read_manifest(dir: &Path) -> Result<HarnessManifest, String> {
    let text = std::fs::read_to_string(dir.join("afz-harness.json"))
        .map_err(|_| format!("no afz-harness.json in {}", dir.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("afz-harness.json: {e}"))
}

/// Resolve a manifest argv against the harness repository root: every element that names an
/// existing file under `root` becomes absolute, so the argv runs from any working directory.
/// Program names and flags stay as written, exactly as [`absolutized`] rules them.
pub fn resolve_argv(root: &Path, argv: &[String]) -> Vec<String> {
    argv.iter()
        .enumerate()
        .map(|(index, argument)| absolutized(index, argument.clone(), root, |path| root.join(path).exists()))
        .collect()
}

/// One harness recorded by `afz install-harness`, at `<registry>/<name>/harness.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledHarness {
    pub name: String,
    pub url: String,
    pub commit: String,
    /// The installed repository root, absolute: `<registry>/<name>/checkout`.
    pub root: PathBuf,
    /// The plugin argv, absolute.
    pub argv: Vec<String>,
    /// The manifest's `conformance` block with its emitter argv resolved against `root`, so a
    /// caller never has to guess where a harness repository keeps its emitter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conformance: Option<InstalledConformance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledConformance {
    /// The emitter argv from the manifest, each element that names an existing file resolved
    /// against `root`, so a caller can run it as it stands with `--out <temp>` appended.
    pub emitter: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

/// The record at `<root()>/<name>/harness.json`, or `None` when absent or unparseable. An
/// unreadable record must not break an unrelated scan, so this never returns an error.
pub fn lookup(name: &str) -> Option<InstalledHarness> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let text = std::fs::read_to_string(root().join(name).join("harness.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Every record under `root()`, sorted by name.
pub fn all() -> Vec<InstalledHarness> {
    let mut harnesses = Vec::new();
    let Ok(entries) = std::fs::read_dir(root()) else { return harnesses };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(harness) = lookup(&name) {
            harnesses.push(harness);
        }
    }
    harnesses.sort_by(|a, b| a.name.cmp(&b.name));
    harnesses
}

/// Write `<root()>/<name>/harness.json`.
pub fn record(harness: &InstalledHarness) -> Result<PathBuf, String> {
    let dir = root().join(&harness.name);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = dir.join("harness.json");
    let text = serde_json::to_string_pretty(harness)
        .map_err(|e| format!("cannot serialize the record for {}: {e}", harness.name))?;
    std::fs::write(&path, text.as_bytes()).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path)
}

/// A `Launch` carrying the recorded argv, with no cwd and no environment: the caller sets
/// both, exactly as `LoadedConfig::launch` does.
pub fn launch(name: &str) -> Option<Launch> {
    lookup(name).map(|harness| Launch::new(harness.argv))
}

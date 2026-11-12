//! `afz validate-harness`: drive a harness plugin through the conformance suite and report,
//! rule by rule, whether it conforms to the plugin contract (`docs/harness-authoring.md`).
//!
//! The plugin argv is required: unlike the repository-side `check-harness`, the product CLI has
//! no reference plugin to fall back to. `--path` optionally names a scan project to discover
//! against; without it the suite drives the plugin over the working directory.

use std::path::PathBuf;

use afz_conformance::{render, Check};
use afz_engine::config::{self, LoadedConfig};
use afz_engine::rpc::client::Launch;

pub fn run(command: Vec<String>, path: Option<String>, targets: usize, deadline: u64, json: bool) -> Result<(), String> {
    if command.is_empty() {
        return Err("--command needs at least the plugin's program name".to_string());
    }
    let scope = scope_for(path.as_deref())?;
    let mut check = Check::new(Launch::new(command), scope.root.display().to_string());
    check.include = scope.include.unwrap_or_default();
    check.exclude = scope.exclude.unwrap_or_default();
    check.config_path = scope.config.as_ref().map(|loaded| loaded.path.display().to_string());
    // A rendered conformance project ships its manifest beside the config: template mode turns
    // on automatically, and a manifest naming a template version this tool does not embed is a
    // usage error reported before the plugin is even spawned.
    let manifest = scope.root.join("afz-conformance-manifest.json");
    if manifest.is_file() {
        check.template = Some(afz_conformance::template::load_manifest(&manifest)?);
    }
    check.drive_targets = targets;
    check.deadline_ms = deadline;

    let report = afz_conformance::run(&check);
    if json {
        println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
    } else {
        print!("{}", render(&report));
    }
    // Notes are advice about cost profile; only a violated rule fails the command.
    if !report.violations.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

/// Where to discover, and the scan project that said so, if `--path` named one. Same resolution
/// as `probe-plugin`, so both commands point a plugin at the same tree.
struct Scope {
    root: PathBuf,
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
    config: Option<LoadedConfig>,
}

fn scope_for(path: Option<&str>) -> Result<Scope, String> {
    let Some(given) = path.map(PathBuf::from) else {
        return Ok(Scope { root: PathBuf::from("."), include: None, exclude: None, config: None });
    };
    // The path may name the scan project's directory or the config file itself, as `afz scan`
    // accepts either.
    let file = if given.join(config::CONFIG_FILE_NAME).is_file() {
        given.join(config::CONFIG_FILE_NAME)
    } else if given.is_file() {
        given
    } else {
        return Err(format!(
            "no {} under {}; pass --path a scan project, or omit it to validate over the working directory",
            config::CONFIG_FILE_NAME,
            given.display(),
        ));
    };
    let loaded = config::load(&file).map_err(|e| e.to_string())?;
    let declared = loaded.discovery_scope().map_err(|e| e.to_string())?;
    Ok(Scope { root: declared.root, include: declared.include, exclude: declared.exclude, config: Some(loaded) })
}

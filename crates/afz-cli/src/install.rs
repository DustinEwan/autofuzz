//! `afz install-harness`: clone a harness repository, build its plugin, drive the conformance
//! suite against it, and record the launch argv under the harness name. One repository holds
//! one harness, so one URL installs exactly one plugin and no selector flag exists.
//!
//! Validation happens before the record is written: an install that does not conform leaves
//! nothing behind, and a working install of the same name survives a failed replacement.
//! `afz harnesses` lists what the registry holds.

use std::path::{Path, PathBuf};

use afz_engine::git::git;
use afz_engine::harnesses::{self, HarnessManifest, InstalledConformance, InstalledHarness};
use afz_engine::rpc::client::{Launch, PluginClient};

use crate::probe;
use crate::validate;

/// `afz install-harness <url>`.
pub fn run(url: &str, reference: Option<&str>, no_validate: bool, json: bool) -> Result<(), String> {
    let registry = harnesses::root();
    std::fs::create_dir_all(&registry).map_err(|e| format!("cannot create {}: {e}", registry.display()))?;
    let staging = registry.join(format!(".staging-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);

    // A local path is a valid clone source, so verification and offline installs need no push.
    // Absolutize it: git resolves a relative source against the `-C` directory, not the shell's.
    let source = match std::path::absolute(url) {
        Ok(absolute) if Path::new(url).exists() => absolute.display().to_string(),
        _ => url.to_string(),
    };
    // Clone through manifest: a failure here has only dirtied the staging directory, so it is
    // removed here rather than left for the next install with the same pid to find.
    let (commit, manifest) = match preflight(&registry, &source, url, reference, &staging) {
        Ok(ok) => ok,
        Err(reason) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(reason);
        }
    };
    let name = manifest.name.clone();
    let dest = registry.join(&name);
    let previous = registry.join(format!(".{name}.previous"));
    let _ = std::fs::remove_dir_all(&previous);
    // Rename the working install aside rather than deleting it: this is what keeps it alive
    // when the new one fails.
    if dest.exists() {
        std::fs::rename(&dest, &previous).map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            format!("cannot move the existing {} aside: {e}", dest.display())
        })?;
    }

    match install(&manifest, &staging, &dest, url, &commit, no_validate, json) {
        Ok(record_only) => {
            let _ = std::fs::remove_dir_all(&previous);
            let _ = std::fs::remove_dir_all(&staging);
            if !json {
                if let Some(argv) = record_only {
                    println!("installed {name}  <- {}", argv.join(" "));
                }
            }
            Ok(())
        }
        Err(reason) => {
            // Nothing new is installed and the previous install still resolves.
            let _ = std::fs::remove_dir_all(&dest);
            if previous.exists() {
                let _ = std::fs::rename(&previous, &dest);
            }
            let _ = std::fs::remove_dir_all(&staging);
            Err(reason)
        }
    }
}

/// Clone, pin the reference, and read the manifest. Nothing outside `staging` is touched, so
/// the caller owns the whole cleanup on the error path.
fn preflight(registry: &Path, source: &str, url: &str, reference: Option<&str>, staging: &Path) -> Result<(String, HarnessManifest), String> {
    git(registry, &["clone", source, &staging.display().to_string()], &[]).map_err(|e| format!("clone {url} failed: {e}"))?;
    if let Some(reference) = reference {
        git(staging, &["checkout", reference], &[]).map_err(|e| format!("checkout {reference} failed: {e}"))?;
    }
    let commit = git(staging, &["rev-parse", "HEAD"], &[]).map_err(|e| e.to_string())?;
    let manifest = harnesses::read_manifest(staging)?;
    Ok((commit, manifest))
}

/// Build, validate, and record. `Ok(Some(argv))` is the success line's argv; `Ok(None)` means
/// the machine-readable report was the whole output.
fn install(
    manifest: &HarnessManifest,
    staging: &Path,
    dest: &Path,
    url: &str,
    commit: &str,
    no_validate: bool,
    json: bool,
) -> Result<Option<Vec<String>>, String> {
    let name = &manifest.name;
    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let checkout = dest.join("checkout");
    std::fs::rename(staging, &checkout).map_err(|e| format!("cannot move the checkout into place: {e}"))?;

    // Only an install runs the `install` argv: fetching dependencies is a once-per-checkout
    // act, and compiling is what keeps a validation honest about current source.
    for argv in &manifest.install {
        validate::run_stage("install", name, &checkout, argv, json)?;
    }
    for argv in &manifest.build {
        validate::run_stage("build", name, &checkout, argv, json)?;
    }
    // A manifest path is relative to the harness repository root, not to the directory `afz`
    // was run from, so every file-naming element is resolved against the checkout.
    let argv = harnesses::resolve_argv(&checkout, &manifest.command);
    let conformance = manifest
        .conformance
        .as_ref()
        .map(|conformance| InstalledConformance {
            emitter: harnesses::resolve_argv(&checkout, &conformance.emitter).into_iter().map(PathBuf::from).collect(),
            deadline_ms: conformance.deadline_ms,
        });

    if !no_validate {
        match &conformance {
            Some(conformance) => {
                let target = validate::Target { argv: argv.clone(), root: checkout.clone(), conformance: conformance.clone() };
                let out = dest.parent().map(Path::to_path_buf).unwrap_or_default().join(format!(".validate-{name}"));
                let report = validate::drive(&target, &out, 8, conformance.deadline_ms.unwrap_or(1000))?;
                // The handshake name is what the record is keyed on: this is what catches a
                // manifest naming the wrong harness.
                if !report.plugin.starts_with(&format!("{name} ")) {
                    return Err(format!(
                        "harness \"{}\" reported plugin \"{}\": afz-harness.json and the handshake must name the same harness",
                        name, report.plugin
                    ));
                }
                if json {
                    println!("{}", serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?);
                } else {
                    print!("{}", afz_conformance::render(&report));
                }
                if !report.violations.is_empty() {
                    return Err(format!("harness \"{name}\" did not conform: {} rule(s) violated", report.violations.len()));
                }
            }
            // Nothing to render, so validate what can be checked without a project: the
            // handshake, with the same protocol-version rule `probe-plugin` prints.
            None => {
                let client = PluginClient::spawn(&Launch::new(argv.clone()).with_cwd(checkout.display().to_string()))
                    .map_err(|reason| format!("cannot start harness \"{name}\": {}", reason.detail()))?;
                let handshake = client.handshake().map_err(|reason| format!("handshake with \"{name}\" failed: {}", reason.detail()))?;
                probe::check_protocol(&handshake)?;
                if handshake.name != *name {
                    return Err(format!(
                        "harness \"{name}\" reported name \"{}\": afz-harness.json and the handshake must name the same harness",
                        handshake.name
                    ));
                }
                if json {
                    println!(
                        "{}",
                        serde_json::json!({ "name": name, "validated": "handshake only", "version": handshake.version })
                    );
                } else {
                    println!("validated {name} (handshake only)");
                }
            }
        }
    }

    // Record it last, once validation has passed: nothing in validation reads the record, so a
    // failed install never leaves a record behind and a good one is written exactly once.
    harnesses::record(&InstalledHarness {
        name: name.clone(),
        url: url.to_string(),
        commit: commit.to_string(),
        root: checkout,
        argv: argv.clone(),
        conformance,
    })?;
    Ok(Some(argv))
}

/// `afz harnesses`: one row per installed harness — name, where it came from, and the argv a
/// scan resolves for it.
pub fn list(json: bool) -> Result<(), String> {
    let installed = harnesses::all();
    if json {
        println!("{}", serde_json::to_string_pretty(&installed).map_err(|e| e.to_string())?);
        return Ok(());
    }
    if installed.is_empty() {
        println!("no harnesses installed; run `afz install-harness <git-url>`");
        return Ok(());
    }
    for harness in &installed {
        println!("{:<10} {} @ {}  {}", harness.name, harness.url, harness.commit, harness.argv.join(" "));
    }
    println!("\n{} harness(es) under {}", installed.len(), harnesses::root().display());
    Ok(())
}

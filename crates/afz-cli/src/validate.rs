//! `afz validate-harness [harness]`: drive a harness plugin through the conformance suite and
//! report, rule by rule, whether it conforms to the plugin contract
//! (`docs/harness-authoring.md` in the engine repository).
//!
//! Validation names a harness and nothing else. The plugin's entrypoint is that harness's
//! `afz-harness.json` `command`, and the scan project is whatever that harness's emitter
//! renders: `afz validate-harness` takes no `--command` and no `--path`, because the harness
//! already carries both. Pointing a plugin at an arbitrary project is a different job and
//! belongs to `afz probe-plugin` and `afz scan`, which keep `--command`.
//!
//! This module is also the validation engine `afz install-harness` calls, so the two commands
//! cannot drift: an install validates the plugin it is about to record through the exact code
//! path an author reaches with `afz validate-harness`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use afz_conformance::{render, Check};
use afz_engine::config::ScanProject;
use afz_engine::harnesses::{self, InstalledConformance};
use afz_engine::rpc::client::Launch;

/// A plugin to validate, and the harness that says how to run and validate it.
pub struct Target {
    /// The plugin argv, absolute.
    pub argv: Vec<String>,
    /// The directory the plugin runs in: the installed checkout, or an author's own.
    pub root: PathBuf,
    pub conformance: InstalledConformance,
}

/// `afz validate-harness [harness]`.
pub fn run(harness: Option<&str>, targets: Option<usize>, deadline: Option<u64>, json: bool) -> Result<(), String> {
    let target = resolve(harness, json)?;
    let out = std::env::temp_dir().join(format!("afz-conformance-{}", std::process::id()));
    let deadline = deadline.or(target.conformance.deadline_ms).unwrap_or(1000);
    let report = drive(&target, &out, targets.unwrap_or(8), deadline)?;
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

/// An installed name first, then a directory holding `afz-harness.json`. `None` means the
/// working directory, which is what makes `afz validate-harness` inside a checkout validate
/// that checkout. A name wins over a same-named directory, because a user who typed the name
/// means the installed plugin; an author who means a working tree types nothing or `.`.
pub fn resolve(harness: Option<&str>, json: bool) -> Result<Target, String> {
    let arg = harness.unwrap_or(".");
    if let Some(record) = harnesses::lookup(arg) {
        let conformance = record.conformance.clone().ok_or_else(|| no_emitter(&record.name))?;
        return Ok(Target { argv: record.argv, root: record.root, conformance });
    }
    let dir = std::path::absolute(PathBuf::from(arg)).map_err(|e| format!("{arg}: {e}"))?;
    if !dir.join("afz-harness.json").is_file() {
        return Err(format!("no installed harness \"{arg}\" and no afz-harness.json in {arg}"));
    }
    let manifest = harnesses::read_manifest(&dir)?;
    // `build` and never `install`: an author's inner loop costs one compile and never a
    // dependency reinstall, and `build` recompiles from current source on every run, which
    // is what keeps a pass honest.
    for argv in &manifest.build {
        run_stage("build", &manifest.name, &dir, argv, json)?;
    }
    // Name the unbuilt plugin here rather than letting it die inside `PluginClient::spawn`
    // with an OS error: at least one command element must name a file that now exists.
    if !manifest.command.iter().any(|argument| !argument.starts_with('-') && dir.join(argument).exists()) {
        return Err(format!(
            "afz-harness.json \"command\" names no existing file under {}: run the build steps in afz-harness.json and try again",
            dir.display()
        ));
    }
    let conformance = manifest.conformance.as_ref().ok_or_else(|| no_emitter(&manifest.name))?;
    let emitter = harnesses::resolve_argv(&dir, &conformance.emitter).into_iter().map(PathBuf::from).collect();
    Ok(Target {
        argv: harnesses::resolve_argv(&dir, &manifest.command),
        root: dir,
        conformance: InstalledConformance { emitter, deadline_ms: conformance.deadline_ms },
    })
}

fn no_emitter(name: &str) -> String {
    format!(
        "harness \"{name}\" declares no conformance emitter: the conformance project ships with the harness, so add \"conformance\": {{\"emitter\": […], …}} to its afz-harness.json"
    )
}

/// Run one manifest argv in `dir`. Its stdout goes where the caller's output channel allows:
/// a `--json` run must leave stdout for the report alone, so a build tool's progress (npm
/// prints its banner on stdout) is re-pointed at stderr there. stderr is always inherited.
/// `install` uses this for both its stages; `resolve` uses it for `build` alone.
pub(crate) fn run_stage(stage: &str, name: &str, dir: &Path, argv: &[String], json: bool) -> Result<(), String> {
    let Some((program, args)) = argv.split_first() else {
        return Err(format!("{stage} argv for harness \"{name}\" is empty"));
    };
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdout(if json { Stdio::from(std::io::stderr()) } else { Stdio::inherit() })
        .status()
        .map_err(|e| format!("{stage} failed for harness \"{name}\": {program}: {e}"))?;
    if !status.success() {
        return Err(format!("{stage} failed for harness \"{name}\": {program} exited {status}"));
    }
    Ok(())
}

/// Render the conformance project with the harness's emitter and drive the suite over it.
pub fn drive(target: &Target, out: &Path, targets: usize, deadline_ms: u64) -> Result<afz_conformance::Report, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("cannot create {}: {e}", out.display()))?;
    let report = drive_inner(target, out, targets, deadline_ms);
    // The staged project belongs to this run: nothing outside this function reads it, and a
    // leftover tree would be mistaken for a fixture by the next scan of the temp directory.
    let _ = std::fs::remove_dir_all(out);
    report
}

fn drive_inner(target: &Target, out: &Path, targets: usize, deadline_ms: u64) -> Result<afz_conformance::Report, String> {
    // 1. The emitter renders the whole project. It prints the directory it rendered; the
    //    driver ignores that and uses `out`, because the driver chose it. The line is
    //    captured, not inherited: `--json` promises stdout carries the report alone.
    let Some((program, args)) = target.conformance.emitter.split_first() else {
        return Err("the harness's conformance emitter argv is empty".to_string());
    };
    let output = Command::new(program)
        .args(args)
        .arg("--out")
        .arg(out)
        .current_dir(&target.root)
        .output()
        .map_err(|e| format!("the emitter {} did not start: {e}", join_paths(&target.conformance.emitter)))?;
    if !output.status.success() {
        return Err(format!(
            "the emitter {} exited {}; stderr: {}",
            join_paths(&target.conformance.emitter),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    // 2. The rendered project, read without `config::load`: that calls `ScanProject::validate`,
    //    which demands a `command` or an already-installed harness name, and a project being
    //    validated in order to decide whether to install a harness has neither.
    let config_path = out.join(afz_engine::config::CONFIG_FILE_NAME);
    let text = std::fs::read_to_string(&config_path).map_err(|e| format!("cannot read {}: {e}", config_path.display()))?;
    let project: ScanProject =
        serde_json::from_str(&text).map_err(|e| format!("{} is not a scan project: {e}", config_path.display()))?;
    let scope = project.discovery_scope(out).map_err(|e| e.to_string())?;

    // 3. The check: the plugin under test is the one the target names, never a registry
    //    record. `AFZ_ROOT` and `AFZ_CONFIG` point at the staged project exactly as
    //    `launch_for` points them at a real scan root, because a plugin reads its module or
    //    its harness settings from them before any request arrives.
    let launch = Launch::new(target.argv.clone())
        .with_cwd(target.root.display().to_string())
        .with_env("AFZ_ROOT", out.display().to_string())
        .with_env("AFZ_CONFIG", config_path.display().to_string());
    let mut check = Check::new(launch, scope.root.display().to_string());
    check.include = scope.include.unwrap_or_default();
    check.exclude = scope.exclude.unwrap_or_default();
    check.config_path = Some(config_path.display().to_string());
    check.deadline_ms = deadline_ms;
    check.drive_targets = targets;
    // A manifest-free project stays in schema-only mode, which is what keeps
    // `xtask check-harness`'s manifest-free roots working.
    let manifest_path = out.join("afz-conformance-manifest.json");
    if manifest_path.is_file() {
        let manifest = afz_conformance::template::load_manifest(&manifest_path)?;
        // 3b. The project's own fixture validator runs before the plugin starts: a template
        //     section whose ground truth could not be produced is not a pass.
        if !manifest.validator.is_empty() {
            let argv = harnesses::resolve_argv(out, &manifest.validator).into_iter().map(PathBuf::from).collect::<Vec<_>>();
            check.validator = Some(afz_conformance::template::run_validator(&argv, out, deadline_ms)?);
        }
        check.template = Some(manifest);
    }

    // 4. The suite itself.
    Ok(afz_conformance::run(&check))
}

/// The argv as the error messages name it.
fn join_paths(argv: &[PathBuf]) -> String {
    argv.iter().map(|argument| argument.display().to_string()).collect::<Vec<_>>().join(" ")
}

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
//! The driver lives in [`afz_conformance::harness`], which `afz install-harness` and the
//! `afz_validate_harness` MCP tool call through the same entry points this command does, so the
//! three cannot drift: an install validates the plugin it is about to record through the exact
//! code path an author reaches with `afz validate-harness`.

use std::path::PathBuf;

use afz_conformance::harness::{drive, resolve};
use afz_conformance::render;
use afz_conformance::suite;

/// `afz validate-harness [harness]`.
pub fn run(
    harness: Option<&str>,
    targets: Option<usize>,
    deadline: Option<u64>,
    json: bool,
    out: Option<PathBuf>,
) -> Result<(), String> {
    let targets = targets.unwrap_or(8);
    if let Some(dir) = out {
        // The mechanism reads `verdicts.json` and decides conformance from it, so this writes the
        // file, keeps stdout clean, and exits zero. A suite error returns `Err` and writes no
        // file, which is how §12 item 5 keeps the suite's own failure off the plugin.
        let verdicts = suite::run(&dir, targets, deadline)?;
        eprintln!(
            "wrote {} verdicts to {}",
            verdicts.len(),
            dir.join("verdicts.json").display()
        );
        return Ok(());
    }
    let target = resolve(harness, json)?;
    let staged = std::env::temp_dir().join(format!("afz-conformance-{}", std::process::id()));
    let deadline = deadline.or(target.conformance.deadline_ms).unwrap_or(1000);
    let report = drive(&target, &staged, targets, deadline)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", render(&report));
    }
    // Notes are advice about cost profile; only a violated rule fails the command.
    if !report.violations.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}

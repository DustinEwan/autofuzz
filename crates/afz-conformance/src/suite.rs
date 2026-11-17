//! `afz validate-harness --out <dir>`: this suite in the role the pluginrpc mechanism calls its
//! suite program (`spec/spec.md` §12 item 6). The mechanism writes `<dir>`'s `profile.json`,
//! `manifest.json`, and every schema the profile names, runs `<argv> --out <dir>`, and reads
//! `<dir>/verdicts.json`.
//!
//! The rule set comes from the profile spec rather than from [`crate::RULES`], because the
//! mechanism requires exactly one verdict for every rule the profile declares and none it does
//! not. A rule this build does not know is therefore a rule the profile should not have declared,
//! and it surfaces as a pass the author will see, not as a dropped verdict.

use std::path::{Path, PathBuf};

use afz_engine::harnesses::{self, InstalledConformance};
use plugin_rpc::conformance::{Manifest, Outcome, ProfileSpec, SuiteVerdict};

use crate::harness::{self, Target};
use crate::Report;

/// Drive the plugin the mechanism's manifest names, then write `<dir>/verdicts.json`. An `Err`
/// here is a suite error (§12 item 5): no file is written, so the mechanism charges the failure
/// to this suite and not to the plugin.
pub fn run(dir: &Path, targets: usize, deadline: Option<u64>) -> Result<Vec<SuiteVerdict>, String> {
    let profile_path = dir.join("profile.json");
    let profile = ProfileSpec::load(&profile_path)
        .map_err(|e| format!("{} is unreadable: {e}", profile_path.display()))?;
    let manifest_path = dir.join("manifest.json");
    let manifest = Manifest::load(&manifest_path)
        .map_err(|e| format!("{} is unreadable: {e}", manifest_path.display()))?;
    // §12 item 6 starts this program in the mechanism's own working directory, which is the
    // harness checkout the author ran the runner from. The manifest the mechanism copied holds
    // its paths exactly as that checkout wrote them, so they resolve beside the checkout and not
    // beside the copy: `"command": ["build/afz-cpp", "--stdio"]` names the binary the author
    // just built there.
    let root = std::env::current_dir()
        .map_err(|e| format!("the working directory is unreadable: {e}"))?;
    let target = target(&root, &manifest)?;
    let manifest_deadline = manifest.conformance.as_ref().and_then(|c| c.deadline_ms);
    let deadline_ms = deadline
        .or(manifest_deadline)
        .or(profile.conformance.deadline_ms)
        .unwrap_or(1000);
    // The fixture is staged outside the mechanism's directory: `drive` removes the tree it
    // stages, and the mechanism's directory holds the spec, the manifest, and the schemas it
    // will read back when this program exits.
    let staged = std::env::temp_dir().join(format!("afz-suite-{}", std::process::id()));
    let report = harness::drive(&target, &staged, targets, deadline_ms)?;
    let verdicts = verdicts(&profile, &report);
    let path = dir.join("verdicts.json");
    let text = serde_json::to_string_pretty(&verdicts).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(verdicts)
}

/// The plugin the manifest names, rooted at the harness checkout. The emitter and the plugin
/// argv are the strings that checkout wrote, resolved beside it the way [`harness::resolve`]
/// resolves them beside the directory it was given.
fn target(root: &Path, manifest: &Manifest) -> Result<Target, String> {
    let missing = || {
        format!(
            "the manifest for \"{}\" names no conformance emitter, so there is no fixture to drive it against",
            manifest.name
        )
    };
    let conformance = manifest.conformance.as_ref().ok_or_else(missing)?;
    let emitter = conformance
        .emitter
        .clone()
        .filter(|e| !e.is_empty())
        .ok_or_else(missing)?;
    Ok(Target {
        argv: harnesses::resolve_argv(root, &manifest.command),
        root: root.to_path_buf(),
        conformance: InstalledConformance {
            emitter: emitter.into_iter().map(PathBuf::from).collect(),
            deadline_ms: conformance.deadline_ms,
        },
    })
}

/// One verdict per rule the profile declares, in the order it declares them.
fn verdicts(profile: &ProfileSpec, report: &Report) -> Vec<SuiteVerdict> {
    for violation in &report.violations {
        if !profile
            .conformance
            .rules
            .iter()
            .any(|rule| rule.id == violation.rule)
        {
            // The mechanism rejects a verdict for a rule it does not declare, so this is said
            // aloud rather than written into a file the mechanism would refuse.
            eprintln!(
                "note  {}: {} (not a rule this profile declares)",
                violation.rule, violation.detail
            );
        }
    }
    profile
        .conformance
        .rules
        .iter()
        .map(|rule| verdict(report, &rule.id))
        .collect()
}

fn verdict(report: &Report, rule: &str) -> SuiteVerdict {
    if let Some(violation) = report.violations.iter().find(|v| v.rule == rule) {
        return SuiteVerdict {
            rule: rule.to_string(),
            outcome: Outcome::Violation,
            note: Some(violation.detail.clone()),
            reason: None,
        };
    }
    if let Some(skipped) = report.not_applicable.iter().find(|n| n.rule == rule) {
        return SuiteVerdict {
            rule: rule.to_string(),
            outcome: Outcome::NotApplicable,
            note: None,
            reason: Some(skipped.reason.clone()),
        };
    }
    SuiteVerdict {
        rule: rule.to_string(),
        outcome: Outcome::Pass,
        note: None,
        reason: None,
    }
}

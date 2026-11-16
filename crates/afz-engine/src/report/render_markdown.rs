//! Markdown for a scan result.
//!
//! This module reads a scan directory's result and nothing else. It receives a path, hands the
//! directory to [`crate::report::scanresult::read`], and formats the finished record. It has no
//! access to the corpus, the coverage map, or the engine log, and it does not ask for them: a
//! report that needed the corpus could not be handed to someone who only has the report
//! directory.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use crate::report::scanresult::{self, ScanDocument};

/// Render the scan result of the directory holding `path`. [`scanresult::read`] chooses the
/// file: the journal's finished record, or a legacy `scan.json`.
pub fn render_file(path: &Path) -> Result<String, io::Error> {
    let document = scanresult::read(path.parent().unwrap_or(Path::new(".")))?;
    Ok(render(&document))
}

/// Render a scan result the caller already holds.
pub fn render(document: &ScanDocument) -> String {
    let mut out = String::new();
    out.push_str(&format!("# autofuzz scan {}\n\n", document.scan_id));

    let revision = document
        .target
        .revision
        .as_deref()
        .map(|r| format!(" at {r}"))
        .unwrap_or_default();
    out.push_str(&format!("`{}`{revision}\n", document.target.root));
    // A scoped scan has to say so in its first lines: 12 clean targets inside a change set is not
    // the same claim as a clean package, and the report is what a triager reads first.
    if let Some(git) = &document.git {
        out.push_str(&format!(
            "scoped to `{}` against {} ({} changed file(s))\n",
            git.selector, git.base, git.changed_files
        ));
    }
    out.push_str(&format!(
        "harness {} {} ({}; {})\n\n",
        document.target.harness.name,
        document.target.harness.version,
        serde_json::to_value(document.target.harness.target_class)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default(),
        document
            .target
            .harness
            .capabilities
            .iter()
            .map(|c| serde_json::to_value(c)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!(
        "Started {}. Finished {}. Stopped by {}.\n\n",
        document.started_at,
        document.finished_at,
        keyword(&document.execution.stopped_reason)
    ));
    out.push_str(&format!(
        "Engine {} with seed {}, config {}.\n\n",
        document.engine.version, document.engine.seed, document.engine.config_hash
    ));

    out.push_str("## Summary\n\n");
    out.push_str("| | |\n|---|---|\n");
    row(
        &mut out,
        "targets discovered",
        &document.inventory.discovered.to_string(),
    );
    row(
        &mut out,
        "fuzzable",
        &document.inventory.fuzzable.to_string(),
    );
    row(
        &mut out,
        "skipped",
        &document.inventory.skipped.len().to_string(),
    );
    row(
        &mut out,
        "invocations",
        &document.execution.invocations.to_string(),
    );
    row(
        &mut out,
        "corpus entries",
        &document.execution.corpus_size.to_string(),
    );
    if let Some(bytes) = &document.execution.corpus_bytes {
        row(&mut out, "corpus bytes", &bytes.to_string());
    }
    row(
        &mut out,
        "coverage",
        &format!(
            "{} edges ({:.1}%)",
            document.execution.coverage_edges,
            document.execution.coverage_ratio * 100.0
        ),
    );
    row(
        &mut out,
        "invocations past deadline",
        &document.execution.deadline_exceeded.to_string(),
    );
    row(
        &mut out,
        "invocations cut at deadline",
        &document.execution.deadline_cuts.to_string(),
    );
    row(
        &mut out,
        "plugin restarts",
        &document.execution.restarts.to_string(),
    );
    row(
        &mut out,
        "unattempted invocations",
        &document.execution.unattempted.to_string(),
    );
    if !document.execution.disabled_targets.is_empty() {
        row(
            &mut out,
            "disabled targets",
            &document.execution.disabled_targets.join(", "),
        );
    }
    row(&mut out, "findings", &document.findings.len().to_string());
    if let Some(suppressed) = &document.findings_suppressed {
        if *suppressed > 0 {
            row(&mut out, "findings suppressed", &suppressed.to_string());
        }
    }
    out.push('\n');

    if !document.inventory.materialization_counts.is_empty() {
        out.push_str("#### Materialization\n\n");
        out.push_str(
            "These counts cover the fuzzable targets and the skipped symbols that report a materialization.\n\n",
        );
        out.push_str("| | |\n|---|---|\n");
        for (materialization, count) in &document.inventory.materialization_counts {
            row(&mut out, &keyword(materialization), &count.to_string());
        }
        out.push('\n');
    }

    if !document.inventory.skipped.is_empty() {
        out.push_str("## Skipped targets\n\n");
        out.push_str("| target | reason | blocked by | unlocks |\n|---|---|---|---|\n");
        for skipped in &document.inventory.skipped {
            let blocked = skipped
                .blocked_by
                .as_ref()
                .map(|b| match &b.module {
                    Some(module) => format!("`{}` in `{module}` at `{}`", b.type_, b.path),
                    None => format!("`{}` at `{}`", b.type_, b.path),
                })
                .unwrap_or_else(|| "-".to_string());
            out.push_str(&format!(
                "| `{}` | {} | {blocked} | {} |\n",
                skipped.target_id,
                keyword(&skipped.reason),
                skipped
                    .unlocks_targets
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string())
            ));
        }
        out.push('\n');
    }

    // One row per target: where the budget went, what each target taught the corpus, and which
    // targets spent invocations without adding anything.
    out.push_str("## Targets\n\n");
    out.push_str(
        "| target | invocations | findings | novel edges | deadline cuts | total ms | max ms |\n|---|---|---|---|---|---|---|\n",
    );
    for target in &document.execution.targets {
        out.push_str(&format!(
            "| `{}`{} | {} | {} | {} | {} | {:.1} | {:.1} |\n",
            target.target_id,
            if target.disabled {
                " (disabled)"
            } else {
                ""
            },
            target.invocations,
            target.findings,
            target.novel_edges,
            target.deadline_cuts,
            target.total_ms,
            target.max_ms
        ));
    }
    out.push('\n');

    // Two findings that share a target and the invocation that first saw them are one call
    // classified by two oracles; naming the siblings keeps identical snippets readable as one
    // event that failed in more than one way.
    let mut sibling_groups: BTreeMap<(&str, u64), Vec<&scanresult::FindingRecord>> = BTreeMap::new();
    for finding in &document.findings {
        sibling_groups.entry((finding.target_id.as_str(), finding.first_seen_invocation)).or_default().push(finding);
    }
    out.push_str(&format!("## Findings ({})\n\n", document.findings.len()));
    if document.findings.is_empty() {
        out.push_str("No finding met the configured oracles.\n");
        return out;
    }
    for finding in &document.findings {
        out.push_str(&format!(
            "### {} — {} ({})\n\n",
            finding.finding_id,
            finding.oracle,
            keyword(&finding.severity)
        ));
        out.push_str(&format!(
            "`{}`: {} via `{}`\n\n",
            finding.target_id, finding.fault.message, finding.via
        ));
        out.push_str(&format!("- Fault class: `{}`\n", finding.fault.class));
        if let Some(group) = sibling_groups.get(&(finding.target_id.as_str(), finding.first_seen_invocation)) {
            if group.len() > 1 {
                let others: Vec<String> = group
                    .iter()
                    .filter(|other| other.finding_id != finding.finding_id)
                    .map(|other| format!("`{}` (class `{}`)", other.finding_id, other.fault.class))
                    .collect();
                out.push_str(&format!("- The same call also produced: {}\n", others.join(", ")));
            }
        }
        for frame in &finding.fault.frames {
            let column = frame.column.map(|c| format!(":{c}")).unwrap_or_default();
            let function = frame
                .fn_name
                .as_deref()
                .map(|f| format!(" in `{f}`"))
                .unwrap_or_default();
            out.push_str(&format!(
                "  - {}:{}{column}{function}\n",
                frame.file, frame.line
            ));
        }
        out.push_str(&format!(
            "- Input: `{}` ({})\n",
            finding.input.artifact,
            keyword(&finding.input.encoding)
        ));
        match &finding.minimized_input {
            Some(minimized) => out.push_str(&format!(
                "- Minimized input: `{}` ({} bytes, from {})\n",
                minimized.artifact, minimized.bytes_after, minimized.bytes_before
            )),
            None if finding.seen_before => {
                out.push_str("- Minimized input: none. The engine does not minimize a repeat.\n")
            }
            None => {
                out.push_str("- Minimized input: none. No smaller input reproduced the fault.\n")
            }
        }
        match &finding.reproducer {
            Some(reproducer) => out.push_str(&format!("- Reproducer: `{}`\n", reproducer.artifact)),
            None => out.push_str("- Reproducer: none emitted.\n"),
        }
        out.push_str(&format!(
            "- Reproduced: {}; first seen at invocation {}\n",
            if finding.reproduced { "yes" } else { "no" },
            finding.first_seen_invocation
        ));
        if finding.seen_before {
            out.push_str("- Seen before: this project reported this identity in an earlier scan\n");
        }
        out.push_str(&format!("- Identity: `{}`\n\n", finding.identity));
    }

    out.push_str("## Artifacts\n\n");
    out.push_str(&format!("- Corpus: `{}`\n", document.artifacts.corpus));
    if let Some(coverage) = &document.artifacts.coverage {
        out.push_str(&format!("- Coverage: `{coverage}`\n"));
    }
    if let Some(log) = &document.artifacts.log {
        out.push_str(&format!("- Engine log: `{log}`\n"));
    }
    out.push('\n');
    out
}

fn row(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("| {label} | {value} |\n"));
}

/// The kebab-case or camelCase spelling a serde enum variant has on the wire, which is the
/// spelling a reader should see. Serializing the value and reading the string back keeps this in
/// step with the types instead of restating their renames here.
fn keyword<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "?".to_string())
}

/// The file a renderer is given: `<scan dir>/scan.json`.
pub fn scan_file_in(scan_dir: &Path) -> std::path::PathBuf {
    scanresult::result_file_in(scan_dir)
}

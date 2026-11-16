//! The scan result document, and the files that stand beside it.
//!
//! One append-only JSON Lines file per scan: `scan.jsonl`. Every line is fsynced the moment it
//! lands, and the last line — the finished record — is the complete result document. A journal
//! that ends without that line is an unfinished run. Renderers read the finished record and
//! nothing else, which is why the document carries the numbers a triager needs and points at
//! artifacts for everything large.
//!
//! The shape is Part I's. Five fields are additions the plan requires and Part I's example does
//! not show, each documented at its declaration: `execution.stoppedReason` (the verification
//! list requires a scan to report why it ended), `execution.restarts`, `execution.unattempted`,
//! `execution.disabledTargets` (the supervisor's accounting, which the scan result is the only
//! place it can appear), `findings[].severity` (the renderer prints it and may read nothing but
//! this document), and `findings[].seenBefore` (a second scan of the same tree marks the finding
//! rather than listing it twice).

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use afz_protocol::{Capability, EdgeId, Materialization, SkippedTarget, TargetClass, ValueTree};

use crate::config::Severity;

/// The directory names beside the result.
pub const INPUTS_DIR: &str = "inputs";
pub const REPROS_DIR: &str = "repros";
pub const CORPUS_DIR: &str = "corpus";
/// The result file of engines that ended a scan with a single document. New scans write
/// [`JOURNAL_FILE`]; this file is still read, so old scan directories stay inspectable.
pub const SCAN_FILE: &str = "scan.json";
/// The scan result: one JSON record per line, fsynced as it is written. The durable lines
/// (`started`, `corpus`, `finding`, `progress`) keep every fact a `kill -9` would otherwise
/// lose, and the final `finished` line carries the complete document. A journal without that
/// line is an unfinished run: `--resume` restarts from the lines that landed.
pub const JOURNAL_FILE: &str = "scan.jsonl";

/// One scan's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScanDocument {
    pub schema_version: ScanSchemaVersion,
    /// The run's directory name: eight Crockford-base32 characters of the start time, then two
    /// of randomness, so ids sort by time and two scans of the same tree still get two ids.
    pub scan_id: String,
    pub started_at: String,
    pub finished_at: String,
    pub engine: EngineInfo,
    pub target: TargetInfo,
    /// The git change set the scan was limited to. Absent for a whole-tree scan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git: Option<GitScope>,
    pub inventory: Inventory,
    pub execution: Execution,
    pub findings: Vec<FindingRecord>,
    /// Findings this scan did not report because an earlier scan had reported the same fault
    /// identity. Without it, `findings: []` cannot tell "found nothing" from "suppressed
    /// everything". Absent in scans recorded before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub findings_suppressed: Option<u32>,
    pub artifacts: Artifacts,
}

/// The document's version tag, pinned so a validator rejects anything else rather than accepting
/// a string it cannot interpret.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ScanSchemaVersion {
    #[serde(rename = "afz-scan/1")]
    #[default]
    AfzScan1,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfo {
    pub version: String,
    /// The seed as the user typed it, `0x`-prefixed, because that is what `--seed` accepts back.
    pub seed: String,
    pub config_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    pub root: String,
    /// `git:<short sha>` when the scan root is a repository, absent when it is not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub harness: HarnessInfo,
}

/// The change set a scoped scan was limited to, and the two states it was computed between.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GitScope {
    /// What was named after `--git`, verbatim.
    pub selector: String,
    /// The state the change set was computed against: a sha, or `index`.
    pub base: String,
    /// The state that was scanned: a sha, or `worktree`.
    pub tree: String,
    pub changed_files: u64,
    /// The materialized tree the scan ran in. Absent for `unstaged`, which scans the working tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessInfo {
    /// The protocol revision the handshake agreed on, so an old scan says which contract its
    /// bytes were written under.
    pub protocol_version: String,
    pub name: String,
    pub version: String,
    pub target_class: TargetClass,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub discovered: u64,
    pub fuzzable: u64,
    pub skipped: Vec<SkippedTarget>,
    pub materialization_counts: BTreeMap<Materialization, u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Execution {
    pub invocations: u64,
    pub corpus_size: u64,
    /// The total size in bytes of the compact JSON documents the scan wrote into `corpus/`:
    /// the byte count of the inputs a resume or a seed file replays. Absent in scans recorded
    /// before this field existed, which is why it is optional: a missing count must not read
    /// as a zero-byte corpus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corpus_bytes: Option<u64>,
    /// Inputs the caller supplied and the engine ran before the guided loop started. Zero for
    /// every scan that was not seeded.
    pub seeded: u64,
    pub coverage_edges: u64,
    pub coverage_ratio: f64,
    pub deadline_exceeded: u64,
    /// Invocations whose reported duration reached the deadline the engine gave the plugin. A
    /// harness that cuts a call itself answers at the deadline with an `ok` outcome carrying no
    /// value, so these appear nowhere else: `deadlineExceeded` counts only calls the engine
    /// stopped waiting for. A scan can spend most of its wall clock inside these and report a
    /// clean run.
    pub deadline_cuts: u64,
    pub stopped_reason: StoppedReason,
    /// Plugin processes the engine restarted, and the targets it stopped using because they
    /// caused too many of them. Both are the supervisor's, and a scan that says nothing about
    /// them looks like a clean run that was actually fighting its harness.
    pub restarts: u64,
    /// Planned invocations that never started: their target was disabled or proved unbuildable,
    /// or the wall clock ended first.
    pub unattempted: u64,
    pub disabled_targets: Vec<String>,
    /// Coverage at fixed invocation intervals, taken every `SAMPLE_EVERY` invocations. The final
    /// `coverageRatio` cannot tell a search that converged from one that stalled at invocation
    /// 6,000 and then spent the rest of the budget repeating itself, and the acceptance check for
    /// a guided scan is about the series rather than its endpoint.
    pub samples: Vec<ProgressSample>,
    /// One row per discovered target, in target-id order, including targets the scan never
    /// invoked. Where this run's budget went and what each target gave back.
    pub targets: Vec<TargetStat>,
}

/// One point of the coverage series. `coverageRatio` is restated against the reachable set known
/// when the scan finished, so the series is comparable across samples; `coverageEdges` keeps the
/// count the run observed at that moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressSample {
    pub invocations: u64,
    pub coverage_edges: u64,
    pub coverage_ratio: f64,
}

/// What one target cost and what it gave back. `meanMs` is deliberately absent: it is
/// `totalMs / invocations`, and two numbers for one fact disagree when one is rounded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TargetStat {
    pub target_id: String,
    pub invocations: u64,
    /// Distinct finding identities recorded for this target, including ones an earlier scan
    /// also reported (`seenBefore`), because a target that keeps hitting a known bug is still
    /// spending budget.
    pub findings: u64,
    pub total_ms: f64,
    pub max_ms: f64,
    /// Edges this target's inputs added to the corpus. A target at zero over thousands of
    /// invocations has stopped teaching the search.
    pub novel_edges: u64,
    /// Invocations of this target whose reported duration reached the deadline the engine gave
    /// the plugin. The scan-level `deadlineCuts` is the sum of this column. `default` keeps
    /// target rows written before this column existed readable, at zero.
    #[serde(default)]
    pub deadline_cuts: u64,
    pub disabled: bool,
}

impl TargetStat {
    /// A zeroed row for `target_id`, for a target the scan never invoked.
    pub fn new(target_id: String) -> TargetStat {
        TargetStat {
            target_id,
            invocations: 0,
            findings: 0,
            total_ms: 0.0,
            max_ms: 0.0,
            novel_edges: 0,
            deadline_cuts: 0,
            disabled: false,
        }
    }
}

/// Why the run ended. `budget` covers both limits, because the user reads the same sentence for
/// either: the scan stopped before it finished planning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StoppedReason {
    /// A budget limit ended the run: `budget.invocations` or `budget.wallClockMs`.
    #[default]
    Budget,
    /// Every planned invocation ran.
    Complete,
    /// The engine was asked to stop before the budget.
    Interrupted,
}

/// One reported problem, with pointers to the files that hold its details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindingRecord {
    pub finding_id: String,
    pub identity: String,
    pub target_id: String,
    pub oracle: String,
    pub severity: Severity,
    /// The channel the plugin classified the outcome through.
    pub via: String,
    pub fault: ScanFault,
    pub input: InputArtifact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimized_input: Option<MinimizedArtifact>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproducer: Option<Artifact>,
    pub first_seen_invocation: u64,
    /// Whether the minimized input still produced the fault when the engine re-ran it.
    pub reproduced: bool,
    /// The engine has reported this identity for this project before. Skipped when false, so a
    /// first scan's document reads exactly like Part I's example.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seen_before: bool,
}

/// The fault, with paths made relative to the scan root so the document says the same thing on
/// the machine that produced it and on the one reading it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScanFault {
    pub class: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<ScanFrame>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScanFrame {
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Renamed from `fnName`: Part I spells this one `fn`.
    #[serde(rename = "fn", skip_serializing_if = "Option::is_none")]
    pub fn_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InputArtifact {
    pub encoding: InputEncoding,
    pub artifact: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum InputEncoding {
    /// A JSON array of `ValueTree`, the same shape `invoke` carries.
    #[serde(rename = "value-tree+json")]
    #[default]
    ValueTreeJson,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MinimizedArtifact {
    pub artifact: String,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub artifact: String,
}

/// Where the run's other files live, relative to the scan directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Artifacts {
    pub corpus: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
}

/// A finding's files, which the writer places at the paths the document names.
#[derive(Debug, Clone)]
pub struct FindingFiles {
    pub finding_id: String,
    pub input: Vec<ValueTree>,
    pub minimized: Option<MinimizedInput>,
    /// The rendered script and the exact document path it must land at, so the writer cannot
    /// drift from the path `scan.rs` recorded in the document.
    pub reproducer: Option<ReproFile>,
}

/// A rendered reproducer script and the relative artifact path the document names for it.
#[derive(Debug, Clone)]
pub struct ReproFile {
    pub artifact: String,
    pub content: String,
}

/// The minimized input, with the sizes the document reports for it.
#[derive(Debug, Clone)]
pub struct MinimizedInput {
    pub values: Vec<ValueTree>,
    pub bytes_before: usize,
    pub bytes_after: usize,
}

/// One corpus entry, written so a later scan can replay it.
///
/// The file names the target and the edge set beside the values, because a resumed scan needs
/// both: an input is only useful when the engine knows which target it belongs to and which edges
/// it earned, and rediscovering that by re-running every retained entry would spend a scan's
/// whole budget before the first new invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CorpusFile {
    /// The file stem beside `corpus/`, normally the corpus entry's own id.
    pub name: String,
    pub target_id: String,
    pub values: Vec<ValueTree>,
    pub edges: Vec<EdgeId>,
}

/// Everything the document points at.
#[derive(Debug, Clone, Default)]
pub struct ScanFiles<'a> {
    pub findings: Vec<FindingFiles>,
    pub corpus: Vec<CorpusFile>,
    pub coverage: Option<&'a str>,
    pub log: Option<&'a str>,
}

/// The file name a finding's artifacts share, with the id's separators replaced by the ones file
/// systems allow everywhere.
pub fn artifact_stem(finding_id: &str) -> String {
    finding_id.replace(':', "-")
}

pub fn input_artifact_path(finding_id: &str) -> String {
    format!("{INPUTS_DIR}/{}.json", artifact_stem(finding_id))
}

pub fn minimized_artifact_path(finding_id: &str) -> String {
    format!("{INPUTS_DIR}/{}.min.json", artifact_stem(finding_id))
}

pub fn reproducer_artifact_path(finding_id: &str, extension: &str) -> String {
    format!("{REPROS_DIR}/{}.{extension}", artifact_stem(finding_id))
}

/// A short sortable run id: the millisecond in Crockford base 32, then two characters that keep
/// two scans started in the same millisecond apart.
pub fn scan_id(now: SystemTime) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let millis = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut stamp = String::with_capacity(8);
    for shift in (0..8).rev() {
        stamp.push(ALPHABET[((millis >> (shift * 5)) & 0x1f) as usize] as char);
    }
    let nonce: u16 = rand::random();
    format!(
        "{stamp}{}{}",
        ALPHABET[((nonce >> 5) & 0x1f) as usize] as char,
        ALPHABET[(nonce & 0x1f) as usize] as char
    )
}

/// Write one scan: the files the document names, then the `finished` line that lands the
/// result. Returns the scan's directory.
///
/// The directory is created under `out_dir` named by `document.scanId`. If that name is taken —
/// two scans in the same millisecond and the same nonce — the writer appends `-2`, `-3` rather
/// than overwriting a finished scan's output.
pub fn write(
    out_dir: &Path,
    document: &ScanDocument,
    files: &ScanFiles<'_>,
) -> io::Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let dir = unique_dir(out_dir, &document.scan_id)?;
    write_into(&dir, document, files)?;
    Ok(dir)
}

/// Claim the scan's directory before the first invocation, so the journal has somewhere to live
/// from the first line. Naming is reserved once and for all: the finished document lands in the
/// same directory the journal started in.
pub fn reserve_dir(out_dir: &Path, scan_id: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let dir = unique_dir(out_dir, scan_id)?;
    std::fs::create_dir_all(dir.join(INPUTS_DIR))?;
    std::fs::create_dir_all(dir.join(REPROS_DIR))?;
    std::fs::create_dir_all(dir.join(CORPUS_DIR))?;
    Ok(dir)
}

/// Land a finished scan in a directory [`reserve_dir`] already claimed: the artifact files
/// first, then the `finished` line that completes the journal. Split from [`write`] because a
/// journalled scan's directory predates its document.
pub fn write_into(dir: &Path, document: &ScanDocument, files: &ScanFiles<'_>) -> io::Result<()> {
    std::fs::create_dir_all(dir.join(INPUTS_DIR))?;
    std::fs::create_dir_all(dir.join(REPROS_DIR))?;
    std::fs::create_dir_all(dir.join(CORPUS_DIR))?;

    for finding in &files.findings {
        write_json(
            &dir.join(input_artifact_path(&finding.finding_id)),
            &finding.input,
        )?;
        if let Some(minimized) = &finding.minimized {
            write_json(
                &dir.join(minimized_artifact_path(&finding.finding_id)),
                &minimized.values,
            )?;
        }
        if let Some(repro) = &finding.reproducer {
            std::fs::write(dir.join(&repro.artifact), &repro.content)?;
        }
    }

    for entry in &files.corpus {
        write_json(
            &dir.join(CORPUS_DIR)
                .join(format!("{}.json", artifact_stem(&entry.name))),
            entry,
        )?;
    }
    if let Some(coverage) = files.coverage {
        std::fs::write(dir.join("coverage.json"), coverage)?;
    }
    if let Some(log) = files.log {
        std::fs::write(dir.join("engine.log"), log)?;
    }

    // The completion of the scan: the finished line rides the journal last, fsynced like every
    // line before it, so a reader that finds it never sees a scan directory whose artifacts are
    // still being written.
    let mut journal = Journal::create(dir)?;
    journal.append(&JournalLine::Finished(Box::new(document.clone())))
}

/// Read a scan result from the directory that holds it. Renderers go through here and open
/// nothing else.
///
/// The journal is the result: the newest `finished` line of `scan.jsonl` is the document, and
/// a journal without one is an unfinished run, reported as such rather than rendered as a
/// partial report. Directories written by engines that ended the scan with `scan.json` are
/// still read, so old scan directories stay inspectable and resumable.
pub fn read(dir: &Path) -> Result<ScanDocument, io::Error> {
    if dir.join(JOURNAL_FILE).is_file() {
        let contents = read_journal(dir)?;
        for line in contents.lines.into_iter().rev() {
            if let JournalLine::Finished(document) = line {
                return Ok(*document);
            }
        }
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "{}: no finished record: the scan did not complete",
                dir.display()
            ),
        ));
    }
    let path = dir.join(SCAN_FILE);
    let text = std::fs::read_to_string(&path)?;
    serde_json::from_str(&text).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", path.display()),
        )
    })
}

/// The file holding a directory's result: the journal, or the legacy document for a directory
/// written before the journal was the result.
pub fn result_file_in(dir: &Path) -> PathBuf {
    if dir.join(JOURNAL_FILE).is_file() {
        dir.join(JOURNAL_FILE)
    } else {
        dir.join(SCAN_FILE)
    }
}

/// Whether a directory holds a finished scan result: a journal with a `finished` line, or a
/// legacy `scan.json`.
pub fn has_result(dir: &Path) -> bool {
    if dir.join(JOURNAL_FILE).is_file() {
        return read_journal(dir)
            .map(|contents| {
                contents
                    .lines
                    .iter()
                    .any(|line| matches!(line, JournalLine::Finished(_)))
            })
            .unwrap_or(false);
    }
    dir.join(SCAN_FILE).is_file()
}

/// The JSON Schema for this document, the same bytes `docs/schema/afz-1/scan-result.json` holds.
pub fn schema_json() -> String {
    let schema = schemars::schema_for!(ScanDocument);
    serde_json::to_string_pretty(&schema).expect("a schema is always serializable")
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text =
        serde_json::to_string(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut handle = std::fs::File::create(path)?;
    handle.write_all(text.as_bytes())?;
    handle.flush()
}

fn unique_dir(out_dir: &Path, scan_id: &str) -> io::Result<PathBuf> {
    let first = out_dir.join(scan_id);
    if !first.exists() {
        return Ok(first);
    }
    for suffix in 2u32.. {
        let candidate = out_dir.join(format!("{scan_id}-{suffix}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    unreachable!("the suffix loop always returns")
}

/// Read the corpus a previous scan left behind, oldest name first. Entries the file cannot be
/// read as are reported rather than skipped: a resumed scan that silently lost half its corpus
/// would look like a scan that found less.
pub fn read_corpus(scan_dir: &Path) -> io::Result<Vec<CorpusFile>> {
    let dir = scan_dir.join(CORPUS_DIR);
    if !dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no {} under {}", CORPUS_DIR, scan_dir.display()),
        ));
    }
    let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .collect();
    names.sort();
    let mut loaded = Vec::with_capacity(names.len());
    for path in names {
        let text = std::fs::read_to_string(&path)?;
        let entry: CorpusFile = serde_json::from_str(&text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })?;
        loaded.push(entry);
    }
    Ok(loaded)
}

/// Make a path safe to publish: the scan result reports paths relative to the scan root, so the
/// same directory read on another machine says the same thing.
pub fn relativize(root: &str, path: &str) -> String {
    let trimmed = path.trim_start_matches("file://");
    let root = root.trim_end_matches('/');
    match trimmed.strip_prefix(root) {
        Some(rest) => rest.trim_start_matches('/').to_string(),
        None => trimmed.to_string(),
    }
}

/// One durable fact of a scan, one line of `scan.jsonl`, the scan result.
///
/// The durable lines exist for the one failure no signal handler catches: power loss, the OOM
/// killer, `kill -9`. Every line that landed is durable the moment it was appended, so a scan
/// that died mid-flight still left its corpus and findings behind for `--resume` to restart
/// from. The `finished` line is what makes the journal complete: it carries the whole result
/// document, and a journal without it marks an unfinished run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum JournalLine {
    /// The scan claimed its directory and began.
    #[serde(rename = "started")]
    Started(JournalStarted),
    /// The corpus retained this input; a resumed scan replays it without rediscovering it.
    #[serde(rename = "corpus")]
    Corpus(CorpusFile),
    /// The scan found something. The record is the finished document's, and the values and
    /// script ride inline because the artifact files they name do not exist yet.
    #[serde(rename = "finding")]
    Finding(Box<JournalFinding>),
    /// One progress sample, so a crashed run's trajectory survives it.
    #[serde(rename = "progress")]
    Progress(JournalProgress),
    /// The scan is complete, and this is its result. Appended last and fsynced like every
    /// line before it: the scan is finished exactly when this line lands.
    #[serde(rename = "finished")]
    Finished(Box<ScanDocument>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalStarted {
    pub scan_id: String,
    pub started_at: String,
    pub seed: u64,
    pub invocations: u64,
    pub wall_clock_ms: u64,
    pub workers: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalFinding {
    pub record: FindingRecord,
    /// The input that produced the finding, the same shape `inputs/<id>.json` carries.
    pub input: Vec<ValueTree>,
    /// The shrunk input, if shrinking had finished when the scan died.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimized: Option<Vec<ValueTree>>,
    /// The rendered reproducer script, if the harness had handed one back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reproducer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalProgress {
    pub invocations: u64,
    pub coverage_edges: u64,
    pub coverage_total: u64,
    pub findings: usize,
}

/// The scan's open journal. Every append flushes and fsyncs before returning: a durability
/// promise that lives in the page cache is no promise at all, and journal lines are rare
/// (one per retained corpus entry, per finding, per progress sample) next to the batch that
/// produced them.
#[derive(Debug)]
pub struct Journal {
    file: std::fs::File,
}

impl Journal {
    pub fn create(dir: &Path) -> io::Result<Journal> {
        Ok(Journal {
            file: std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(JOURNAL_FILE))?,
        })
    }

    pub fn append(&mut self, line: &JournalLine) -> io::Result<()> {
        let text = serde_json::to_string(line)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
            + "\n";
        self.file.write_all(text.as_bytes())?;
        self.file.flush()?;
        self.file.sync_all()
    }
}

/// What a journal held when it was read back.
#[derive(Debug, Clone)]
pub struct JournalContents {
    pub lines: Vec<JournalLine>,
    /// The last line was not complete JSON: the machine died mid-append. Everything before
    /// it is intact, because every landed line was fsynced.
    pub torn_tail: bool,
}

/// Read a scan journal. A torn last line is expected after a hard kill and is skipped; a
/// corrupt line in the middle means the file was not written by a journal, and that is
/// reported rather than skipped in silence.
pub fn read_journal(dir: &Path) -> io::Result<JournalContents> {
    let text = std::fs::read_to_string(dir.join(JOURNAL_FILE))?;
    let mut lines = Vec::new();
    let mut torn_tail = false;
    let raw: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    for (index, line) in raw.iter().enumerate() {
        match serde_json::from_str::<JournalLine>(line) {
            Ok(parsed) => lines.push(parsed),
            Err(e) => {
                if index + 1 == raw.len() {
                    torn_tail = true;
                } else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("{} line {}: {e}", JOURNAL_FILE, index + 1),
                    ));
                }
            }
        }
    }
    Ok(JournalContents { lines, torn_tail })
}

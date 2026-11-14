//! The scan result document, and the files that stand beside it.
//!
//! One JSON document per scan, written once and never edited in place. Renderers read it and
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

/// The directory names beside `scan.json`.
pub const INPUTS_DIR: &str = "inputs";
pub const REPROS_DIR: &str = "repros";
pub const CORPUS_DIR: &str = "corpus";
/// The file every renderer opens and the only one it may open.
pub const SCAN_FILE: &str = "scan.json";

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    let millis = now.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let mut stamp = String::with_capacity(8);
    for shift in (0..8).rev() {
        stamp.push(ALPHABET[((millis >> (shift * 5)) & 0x1f) as usize] as char);
    }
    let nonce: u16 = rand::random();
    format!("{stamp}{}{}", ALPHABET[((nonce >> 5) & 0x1f) as usize] as char, ALPHABET[(nonce & 0x1f) as usize] as char)
}

/// Write one scan: the document, then the files it names. Returns the scan's directory.
///
/// The directory is created under `out_dir` named by `document.scanId`. If that name is taken —
/// two scans in the same millisecond and the same nonce — the writer appends `-2`, `-3` rather
/// than overwriting a finished scan's output.
pub fn write(out_dir: &Path, document: &ScanDocument, files: &ScanFiles<'_>) -> io::Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let dir = unique_dir(out_dir, &document.scan_id)?;
    std::fs::create_dir_all(dir.join(INPUTS_DIR))?;
    std::fs::create_dir_all(dir.join(REPROS_DIR))?;
    std::fs::create_dir_all(dir.join(CORPUS_DIR))?;

    for finding in &files.findings {
        write_json(&dir.join(input_artifact_path(&finding.finding_id)), &finding.input)?;
        if let Some(minimized) = &finding.minimized {
            write_json(&dir.join(minimized_artifact_path(&finding.finding_id)), &minimized.values)?;
        }
        if let Some(repro) = &finding.reproducer {
            std::fs::write(dir.join(&repro.artifact), &repro.content)?;
        }
    }

    for entry in &files.corpus {
        write_json(&dir.join(CORPUS_DIR).join(format!("{}.json", artifact_stem(&entry.name))), entry)?;
    }
    if let Some(coverage) = files.coverage {
        std::fs::write(dir.join("coverage.json"), coverage)?;
    }
    if let Some(log) = files.log {
        std::fs::write(dir.join("engine.log"), log)?;
    }

    // Last, so a reader that finds `scan.json` never sees a scan directory whose artifacts are
    // still being written.
    write_json(&dir.join(SCAN_FILE), document)?;
    Ok(dir)
}

/// Read a scan result. Renderers go through here and open nothing else.
pub fn read(path: &Path) -> Result<ScanDocument, io::Error> {
    let text = std::fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))
}

/// The JSON Schema for this document, the same bytes `docs/schema/scan-result.json` holds.
pub fn schema_json() -> String {
    let schema = schemars::schema_for!(ScanDocument);
    serde_json::to_string_pretty(&schema).expect("a schema is always serializable")
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
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
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("no {} under {}", CORPUS_DIR, scan_dir.display())));
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
        let entry: CorpusFile = serde_json::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {e}", path.display())))?;
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

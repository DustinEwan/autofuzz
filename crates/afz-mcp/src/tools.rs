//! The six tools a coding harness calls, and the waiting loop `await_scan` runs.
//!
//! Every tool takes `Parameters<T>` and answers with a [`CallToolResult`]. Success is
//! `structured` content built from a serialized result; a failure the caller can act on (bad
//! path, no scan project, nothing in scope, plugin lost) is `Ok(CallToolResult::error(..))`,
//! never `Err(ErrorData)` - `Err` reaches the model as an opaque protocol error, while the
//! error text here is the sentence the harness acts on. `Err(ErrorData)` is reserved for a
//! poisoned runtime: a failed `spawn_blocking` join.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ProgressNotificationParam};
use rmcp::service::RequestContext;
use rmcp::{tool, tool_router, ErrorData, RoleServer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use afz_engine::config::Severity;
use afz_engine::report::scanresult::{self, FindingRecord, GitScope, SCAN_FILE};
use afz_engine::rpc::client::PluginClient;
use afz_engine::scan::{self, Options, Progress};
use afz_protocol::{CoverageMap, DiscoverReq, Handshake, SkippedTarget, TargetDescriptor, SUPPORTED_VERSIONS, is_deprecated};

use crate::jobs::{self, Job, JobState, Shared};
use crate::project::{self, Resolved};
use crate::server::Afz;

/// Defaults the tool table fixes: 25 items, never more than 200.
const DEFAULT_LIMIT: u32 = 25;
const MAX_LIMIT: u32 = 200;
/// How often a blocking call re-reads the registry while it waits for a scan to end.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A failure the caller can act on, as the tool error the plan requires: the message is the
/// sentence the harness reads, not an opaque protocol error.
fn action_error(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// A failure of the server itself, not of the call.
fn internal(e: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(e.to_string(), None)
}

fn structured<T: Serialize>(value: &T) -> Result<CallToolResult, ErrorData> {
    let json = serde_json::to_value(value).map_err(internal)?;
    Ok(CallToolResult::structured(json))
}

/// `90s`, `1500ms`, `10m`, `1h`, or a bare number of seconds - the CLI's grammar
/// (`crates/afz-cli/src/scan.rs`), restated for a tool argument.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let invalid = || format!("budget.wall {text} is not a duration; use 1500ms, 90s, 10m, or 1h");
    let digits = |unit: &str| -> Option<u64> { text.strip_suffix(unit)?.trim().parse::<u64>().ok() };
    if let Some(ms) = digits("ms") {
        return Ok(Duration::from_millis(ms));
    }
    if let Some(seconds) = digits("s") {
        return Ok(Duration::from_secs(seconds));
    }
    if let Some(minutes) = digits("m") {
        return Ok(Duration::from_secs(minutes * 60));
    }
    if let Some(hours) = digits("h") {
        return Ok(Duration::from_secs(hours * 3_600));
    }
    if let Ok(seconds) = text.parse::<u64>() {
        return Ok(Duration::from_secs(seconds));
    }
    Err(invalid())
}

/// `0x`-prefixed hex or decimal; the CLI's `parse_seed` rule.
fn parse_seed(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let (radix, digits) = match trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")) {
        Some(rest) => (16, rest),
        None => (10, trimmed),
    };
    u64::from_str_radix(digits, radix).map_err(|_| format!("seed {text} is not a number, decimal or 0x-prefixed"))
}

fn parse_strategy(name: &str) -> Result<afz_engine::config::Strategy, String> {
    match name {
        "guided" => Ok(afz_engine::config::Strategy::Guided),
        "random" => Ok(afz_engine::config::Strategy::Random),
        other => Err(format!("unknown strategy {other}; use guided or random")),
    }
}

/// The severity floor for `afz_findings`; `any` is `None`.
fn parse_severity(text: &str) -> Result<Option<Severity>, String> {
    match text {
        "any" => Ok(None),
        "low" => Ok(Some(Severity::Low)),
        "medium" => Ok(Some(Severity::Medium)),
        "high" => Ok(Some(Severity::High)),
        "critical" => Ok(Some(Severity::Critical)),
        other => Err(format!("severity {other} is not one of any, low, medium, high, critical")),
    }
}

fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Off => 0,
        Severity::Low => 1,
        Severity::Medium => 2,
        Severity::High => 3,
        Severity::Critical => 4,
    }
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ProbeParams {
    /// Directory to probe. Defaults to the scan project's own discovery root.
    pub path: Option<String>,
    /// Scan project file. Defaults to the nearest autofuzz.json above the directory.
    pub config: Option<String>,
    /// Maximum targets to return. Default 25, at most 200.
    pub target_limit: Option<u32>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeInventory {
    pub discovered: u64,
    pub fuzzable: u64,
    pub skipped: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    /// The directory discovery ran against.
    pub root: String,
    /// The scan project file that named the plugin.
    pub config: String,
    /// The plugin's handshake, verbatim.
    pub plugin: Handshake,
    pub inventory: ProbeInventory,
    /// The fuzzable targets, at most `targetLimit` of them.
    pub targets: Vec<TargetDescriptor>,
    /// Every non-fuzzed symbol the plugin reported, with its reason.
    pub skipped: Vec<SkippedTarget>,
    /// The plugin's coverage map, when it reports one.
    pub coverage: Option<CoverageMap>,
    /// Whether more targets existed than the limit returned.
    pub truncated: bool,
}

#[derive(Debug, Clone, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanBudget {
    /// Invocations to plan before the scan stops. Defaults to the scan project's budget.
    pub invocations: Option<u64>,
    /// Wall clock: 1500ms, 90s, 10m, 1h, or a bare number of seconds.
    pub wall: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScanParams {
    /// Directory to scan. Defaults to the scan project's own discovery root.
    pub path: Option<String>,
    /// Scan project file. Defaults to the nearest autofuzz.json above the directory.
    pub config: Option<String>,
    /// Git selector the scan is limited to: staged, unstaged, stash, a revision, or a span such as A..B.
    pub git: Option<String>,
    /// Budget overrides; absent fields keep the scan project's own budget.
    pub budget: Option<ScanBudget>,
    /// Seed: decimal or 0x-prefixed hex. Absent means 0.
    pub seed: Option<String>,
    /// guided (default) or random.
    pub strategy: Option<String>,
    /// Plugin processes; default is one per available core.
    pub workers: Option<u32>,
    /// A previous scan directory: its corpus is replayed and its findings are marked seen.
    pub resume: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct StatusParams {
    /// Scan to report. Defaults to the newest scan this server knows.
    pub job_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AwaitParams {
    /// Scan to wait for. Defaults to the newest scan this server knows.
    pub job_id: Option<String>,
    /// Seconds to wait before answering with the state as it is then. Absent means wait until the
    /// scan ends, however long that takes.
    pub timeout_seconds: Option<f64>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CancelParams {
    /// Scan to stop. Defaults to the running scan.
    pub job_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct FindingsParams {
    /// Scan to read, by the jobId afz_scan returned. Wins over scanId.
    pub job_id: Option<String>,
    /// Scan to read, by the scanId in a job's result or a directory under the output dir.
    pub scan_id: Option<String>,
    /// Directory whose scan project names the output dir to search. Defaults to the scan project's own.
    pub path: Option<String>,
    /// Scan project file for the search. Defaults to the nearest autofuzz.json.
    pub config: Option<String>,
    /// Keep findings at or above this severity: any (default), low, medium, high, critical.
    pub severity: Option<String>,
    /// Maximum findings to return. Default 25, at most 200.
    pub limit: Option<u32>,
    /// Include the scan's report.md text as `markdown`.
    pub markdown: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BudgetView {
    pub invocations: u64,
    pub wall_seconds: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProgressView {
    pub invocations: u64,
    pub coverage_edges: u64,
    pub coverage_total: u64,
    pub coverage_ratio: f64,
    pub findings: usize,
}

impl From<&Progress> for ProgressView {
    fn from(p: &Progress) -> ProgressView {
        ProgressView {
            invocations: p.invocations,
            coverage_edges: p.coverage_edges,
            coverage_total: p.coverage_total,
            coverage_ratio: p.coverage_ratio,
            findings: p.findings,
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub job_id: String,
    pub state: JobState,
    pub root: String,
    pub out_dir: String,
    pub elapsed_seconds: f64,
    /// The change set the scan was limited to; null for a whole-tree scan or a running one.
    pub git: Option<GitScope>,
    pub budget: BudgetView,
    pub progress: Option<ProgressView>,
    pub scan_id: Option<String>,
    pub dir: Option<String>,
    pub stopped_reason: Option<String>,
    pub error: Option<String>,
}

impl JobView {
    fn of(job: &Job) -> JobView {
        JobView {
            job_id: job.id.clone(),
            state: job.state,
            root: job.root.display().to_string(),
            out_dir: job.out_dir.display().to_string(),
            elapsed_seconds: job.started.elapsed().as_secs_f64(),
            git: job.git.clone(),
            budget: BudgetView {
                invocations: job.budget,
                wall_seconds: job.wall_seconds,
            },
            progress: job.progress.as_ref().map(ProgressView::from),
            scan_id: job.scan_id.clone(),
            dir: job.dir.as_ref().map(|dir| dir.display().to_string()),
            stopped_reason: job.stopped_reason.clone(),
            error: job.error.clone(),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CancelResult {
    pub job_id: String,
    pub state: JobState,
    pub cancel_requested: bool,
    pub message: String,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindingsInventory {
    pub discovered: u64,
    pub fuzzable: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindingsExecution {
    pub invocations: u64,
    pub corpus_size: u64,
    pub coverage_edges: u64,
    pub coverage_ratio: f64,
    pub stopped_reason: String,
    pub unattempted: u64,
    pub restarts: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FindingsResult {
    pub scan_id: String,
    pub dir: String,
    pub root: String,
    pub revision: Option<String>,
    pub git: Option<GitScope>,
    pub inventory: FindingsInventory,
    pub execution: FindingsExecution,
    /// The findings in document order (first seen first), filtered and limited.
    pub findings: Vec<FindingRecord>,
    pub truncated: bool,
    /// The scan's `report.md`, when the scan rendered one.
    pub report_path: Option<String>,
    pub markdown: Option<String>,
}

/// Start a scan from validated parameters, refusing anything the CLI would refuse.
fn build_scan_options(resolved: &Resolved, params: &ScanParams) -> Result<Options, String> {
    let seed = match params.seed.as_deref() {
        Some(text) => parse_seed(text)?,
        None => 0,
    };
    let strategy = params.strategy.as_deref().map(parse_strategy).transpose()?;
    let budget = params.budget.clone().unwrap_or_default();
    let wall = budget.wall.as_deref().map(parse_duration).transpose()?;
    let resume = match params.resume.as_deref() {
        Some(dir) => {
            let path = PathBuf::from(dir);
            if !path.is_dir() {
                return Err(format!("resume {dir} is not a directory"));
            }
            Some(path)
        }
        None => None,
    };
    // Parsed here so a bad selector is the tool's error text, and so preflight can see the
    // scope it needs to check.
    let scope = params.git.as_deref().map(afz_engine::git::parse).transpose().map_err(|e| e.0)?;
    let workers = params.workers.map(|count| count.max(1) as usize);
    Ok(jobs::build(resolved, |options| {
        options.seed = seed;
        if let Some(strategy) = strategy {
            options.strategy = strategy;
        }
        if let Some(invocations) = budget.invocations {
            options.invocations = invocations;
        }
        if let Some(wall) = wall {
            options.wall_clock = wall;
        }
        if let Some(workers) = workers {
            options.workers = workers;
        }
        options.resume = resume;
        options.scope = scope;
    }))
}

/// A job's final report: a failed scan is the actionable sentence, everything else is the
/// structured view the tool table defines.
fn job_result(view: JobView) -> Result<CallToolResult, ErrorData> {
    if view.state == JobState::Failed {
        let error = view.error.unwrap_or_else(|| "the scan failed".to_string());
        return Ok(action_error(error));
    }
    structured(&view)
}

/// Block until the job leaves `running`, a caller-set timeout expires, or the request is
/// cancelled, forwarding live progress to the caller when the request carried a progress token.
///
/// With no `timeoutSeconds` there is no deadline, so a caller waiting on a twenty-minute scan is
/// never told partway through that it is still running. The other two exits are there because an
/// unbounded block needs them: `timeoutSeconds` answers with the state as it is then, and a
/// `notifications/cancelled` for this request id cancels `ctx.ct`
/// (`rmcp::service::RequestContext`), which ends the wait. Neither stops the scan:
/// `afz_scan_cancel` is the only thing that stops a scan.
async fn await_job(shared: &Shared, id: &str, timeout: Option<Duration>, ctx: &RequestContext<RoleServer>) -> JobView {
    let token = ctx.meta.get_progress_token();
    let deadline = timeout.map(|wait| Instant::now() + wait);
    let mut last_sent: Option<u64> = None;
    loop {
        let view = {
            let registry = jobs::lock(shared);
            let job = registry.jobs.get(id).expect("the job is registered before its wait begins");
            JobView::of(job)
        };
        if view.state != JobState::Running {
            return view;
        }
        if let (Some(token), Some(progress)) = (token.as_ref(), view.progress.as_ref()) {
            if last_sent.is_none_or(|sent| progress.invocations > sent) {
                last_sent = Some(progress.invocations);
                let message = format!(
                    "{} invocations, coverage {}/{} ({}%), {} finding(s)",
                    progress.invocations,
                    progress.coverage_edges,
                    progress.coverage_total,
                    (progress.coverage_ratio * 100.0).round(),
                    progress.findings
                );
                let total = view.budget.invocations.max(1) as f64;
                let param = ProgressNotificationParam::new(token.clone(), progress.invocations as f64)
                    .with_total(total)
                    .with_message(message);
                // A client that hung up must not fail the scan's waiter.
                let _ = ctx.peer.notify_progress(param).await;
            }
        }
        // The three exits race: the scan finishing wins on the next poll, and a cancelled caller
        // gets the current view back, which the SDK discards. What matters is that the wait stops.
        tokio::select! {
            _ = ctx.ct.cancelled() => return view,
            _ = until_deadline(deadline) => return view,
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

/// Sleep until the caller's deadline, or forever when they asked to wait for the whole scan, so
/// the `select!` above has one arm per way a wait can end.
async fn until_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending::<()>().await,
    }
}

/// The probe sequence of `afz probe-plugin` (crates/afz-cli/src/probe.rs), on a blocking
/// thread: launch, handshake, protocol check, discover, coverage map. The client kills the
/// plugin process when it drops.
fn probe_blocking(resolved: &Resolved, limit: usize) -> Result<ProbeResult, String> {
    let launch = resolved.loaded.launch().map_err(|e| e.to_string())?;
    let client = PluginClient::spawn(&launch).map_err(|reason| reason.detail())?;
    let handshake = client.handshake().map_err(|reason| reason.detail())?;
    if !SUPPORTED_VERSIONS.contains(&handshake.protocol_version.as_str()) {
        return Err(format!(
            "plugin speaks protocol {}, this engine speaks {}",
            handshake.protocol_version,
            SUPPORTED_VERSIONS.join(" or ")
        ));
    }
    if is_deprecated(&handshake.protocol_version) {
        eprintln!("warning: protocol {} is deprecated and a future engine will refuse it", handshake.protocol_version);
    }
    let scope = resolved.loaded.discovery_scope().map_err(|e| e.to_string())?;
    let discovery = client
        .discover(
            &DiscoverReq {
                root: resolved.scan_root.display().to_string(),
                include: scope.include,
                exclude: scope.exclude,
                config_path: Some(resolved.loaded.path.display().to_string()),
            },
            Duration::from_secs(120),
        )
        .map_err(|reason| reason.detail())?;
    let coverage = client.coverage_map(Duration::from_secs(30)).ok();
    let discovered = discovery.targets.len() as u64;
    let fuzzable = discovery.targets.iter().filter(|target| target.is_fuzzable()).count() as u64;
    let truncated = discovery.targets.len() > limit;
    let mut targets = discovery.targets;
    targets.truncate(limit);
    Ok(ProbeResult {
        root: resolved.scan_root.display().to_string(),
        config: resolved.loaded.path.display().to_string(),
        plugin: handshake,
        inventory: ProbeInventory {
            discovered,
            fuzzable,
            skipped: discovery.skipped.len() as u64,
        },
        targets,
        skipped: discovery.skipped,
        coverage,
        truncated,
    })
}

/// The scan directory `afz_findings` reads: `jobId` first, then `scanId`, then the newest
/// scan under the resolved output directory.
fn findings_dir(shared: &Shared, params: &FindingsParams) -> Result<(PathBuf, String), String> {
    if let Some(job_id) = params.job_id.as_deref() {
        let registry = jobs::lock(shared);
        let job = jobs::find(&registry, Some(job_id)).ok_or_else(|| format!("no job {job_id} in this server; afz only knows the scans it started"))?;
        return job_dir(job);
    }
    if let Some(scan_id) = params.scan_id.as_deref() {
        let registry = jobs::lock(shared);
        if let Some(job) = jobs::find(&registry, Some(scan_id)) {
            return job_dir(job);
        }
        let resolved = project::resolve(params.path.as_deref(), params.config.as_deref())?;
        let candidate = resolved.out_dir.join(scan_id);
        if project::has_scan_file(&candidate) {
            return Ok((candidate, resolved.scan_root.display().to_string()));
        }
        return Err(format!("no scan {scan_id} under {}; run afz_scan first", resolved.out_dir.display()));
    }
    let resolved = project::resolve(params.path.as_deref(), params.config.as_deref())?;
    let newest = newest_scan_dir(&resolved.out_dir).ok_or_else(|| format!("no scan found under {}; run afz_scan first", resolved.out_dir.display()))?;
    Ok((newest, resolved.scan_root.display().to_string()))
}

fn job_dir(job: &Job) -> Result<(PathBuf, String), String> {
    match &job.dir {
        Some(dir) => Ok((dir.clone(), job.root.display().to_string())),
        None if job.state == JobState::Running => Err(format!("job {} is still running; afz_findings reads finished scans", job.id)),
        None => Err(format!("job {} wrote no result: {}", job.id, job.error.as_deref().unwrap_or("unknown reason"))),
    }
}

/// The newest subdirectory of `out_dir` holding a readable `scan.json`. Scan ids sort by
/// their start time, so the lexicographically last name is the most recent scan.
fn newest_scan_dir(out_dir: &std::path::Path) -> Option<PathBuf> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(out_dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| project::has_scan_file(path))
        .collect();
    names.sort();
    names.pop()
}

#[tool_router(vis = "pub(crate)")]
impl Afz {
    #[tool(
        description = "Report the harness plugin a directory uses and the targets it would fuzz. Cheap: starts the plugin, handshakes, discovers, and stops.",
        annotations(title = "Probe a scan project", read_only_hint = true)
    )]
    async fn afz_probe(&self, Parameters(params): Parameters<ProbeParams>) -> Result<CallToolResult, ErrorData> {
        let resolved = match project::resolve(params.path.as_deref(), params.config.as_deref()) {
            Ok(resolved) => resolved,
            Err(message) => return Ok(action_error(message)),
        };
        let limit = params.target_limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
        let outcome = tokio::task::spawn_blocking(move || probe_blocking(&resolved, limit))
            .await
            .map_err(internal)?;
        match outcome {
            Ok(result) => structured(&result),
            Err(message) => Ok(action_error(message)),
        }
    }

    #[tool(
        description = "Start a coverage-guided scan and answer at once with its jobId. Use await_scan to block until it ends and afz_scan_status for progress. One scan runs at a time.",
        annotations(title = "Scan a tree", destructive_hint = false, idempotent_hint = false)
    )]
    async fn afz_scan(&self, Parameters(params): Parameters<ScanParams>) -> Result<CallToolResult, ErrorData> {
        let resolved = match project::resolve(params.path.as_deref(), params.config.as_deref()) {
            Ok(resolved) => resolved,
            Err(message) => return Ok(action_error(message)),
        };
        let options = match build_scan_options(&resolved, &params) {
            Ok(options) => options,
            Err(message) => return Ok(action_error(message)),
        };
        // Preflight before the job exists, so "--git {selector} needs a git repository"
        // answers the call instead of failing a job.
        let preflighted = tokio::task::spawn_blocking(move || scan::preflight(&options).map(|()| options))
            .await
            .map_err(internal)?;
        let options = match preflighted {
            Ok(options) => options,
            Err(e) => return Ok(action_error(e.to_string())),
        };
        let id = match jobs::start(&self.shared, options) {
            Ok(id) => id,
            Err(message) => return Ok(action_error(message)),
        };
        let view = {
            let registry = jobs::lock(&self.shared);
            JobView::of(jobs::find(&registry, Some(&id)).expect("the job was just started"))
        };
        job_result(view)
    }

    #[tool(
        description = "Report one scan's state and live progress right now. Answers immediately; use await_scan to block until the scan ends.",
        annotations(title = "Scan status", read_only_hint = true)
    )]
    async fn afz_scan_status(&self, Parameters(params): Parameters<StatusParams>) -> Result<CallToolResult, ErrorData> {
        let id = {
            let registry = jobs::lock(&self.shared);
            jobs::find(&registry, params.job_id.as_deref()).map(|job| job.id.clone())
        };
        let Some(id) = id else {
            return Ok(action_error("no scan to report; run afz_scan first"));
        };
        let view = {
            let registry = jobs::lock(&self.shared);
            JobView::of(jobs::find(&registry, Some(&id)).expect("the id came from this registry moments ago"))
        };
        job_result(view)
    }

    #[tool(
        description = "Block until a scan ends and answer with its final state. Run it as a background task to be woken when the scan finishes; afz_scan_status still answers instantly for progress meanwhile. Pass timeoutSeconds to answer with the state as it is then instead. Cancelling this call stops the wait, not the scan - afz_scan_cancel stops a scan.",
        annotations(title = "Wait for a scan", read_only_hint = true)
    )]
    async fn await_scan(&self, Parameters(params): Parameters<AwaitParams>, ctx: RequestContext<RoleServer>) -> Result<CallToolResult, ErrorData> {
        let id = {
            let registry = jobs::lock(&self.shared);
            jobs::find(&registry, params.job_id.as_deref()).map(|job| job.id.clone())
        };
        let Some(id) = id else {
            return Ok(action_error("no scan to wait for; run afz_scan first"));
        };
        let timeout = params.timeout_seconds.map(|seconds| Duration::from_secs_f64(seconds.max(0.0)));
        let view = await_job(&self.shared, &id, timeout, &ctx).await;
        job_result(view)
    }

    #[tool(
        description = "Ask a running scan to stop between batches. The scan still writes its result, with stoppedReason interrupted.",
        annotations(title = "Stop a scan", destructive_hint = false)
    )]
    async fn afz_scan_cancel(&self, Parameters(params): Parameters<CancelParams>) -> Result<CallToolResult, ErrorData> {
        let mut registry = jobs::lock(&self.shared);
        let Some(job) = jobs::find_mut(&mut registry, params.job_id.as_deref()) else {
            return Ok(action_error("no scan to stop; run afz_scan first"));
        };
        let id = job.id.clone();
        let state = job.state;
        let running = state == JobState::Running;
        jobs::request_stop(job);
        let message = if running {
            "stopping after the current batch; await_scan reports the result".to_string()
        } else {
            format!("job {id} already {}; nothing to stop", state.as_str())
        };
        let result = CancelResult {
            job_id: id,
            state,
            cancel_requested: job.cancel_requested,
            message,
        };
        structured(&result)
    }

    #[tool(
        description = "Read the findings of a finished scan, newest first, with the artifact path of each input.",
        annotations(title = "Read findings", read_only_hint = true)
    )
    ]
    async fn afz_findings(&self, Parameters(params): Parameters<FindingsParams>) -> Result<CallToolResult, ErrorData> {
        let (dir, root) = match findings_dir(&self.shared, &params) {
            Ok(found) => found,
            Err(message) => return Ok(action_error(message)),
        };
        let path = dir.join(SCAN_FILE);
        let document = match scanresult::read(&path) {
            Ok(document) => document,
            Err(e) => return Ok(action_error(format!("cannot read {}: {e}", path.display()))),
        };
        let floor = match params.severity.as_deref().map(parse_severity).transpose() {
            Ok(floor) => floor.flatten(),
            Err(message) => return Ok(action_error(message)),
        };
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT) as usize;
        let matched: Vec<&FindingRecord> = document
            .findings
            .iter()
            .filter(|finding| floor.is_none_or(|min| severity_rank(finding.severity) >= severity_rank(min)))
            .collect();
        let truncated = matched.len() > limit;
        let findings: Vec<FindingRecord> = matched.into_iter().take(limit).cloned().collect();
        let report_path = dir.join("report.md");
        let rendered = report_path.is_file();
        let markdown = if params.markdown && rendered {
            std::fs::read_to_string(&report_path).ok()
        } else {
            None
        };
        let result = FindingsResult {
            scan_id: document.scan_id,
            dir: dir.display().to_string(),
            root,
            revision: document.target.revision,
            git: document.git,
            inventory: FindingsInventory {
                discovered: document.inventory.discovered,
                fuzzable: document.inventory.fuzzable,
            },
            execution: FindingsExecution {
                invocations: document.execution.invocations,
                corpus_size: document.execution.corpus_size,
                coverage_edges: document.execution.coverage_edges,
                coverage_ratio: document.execution.coverage_ratio,
                stopped_reason: jobs::stopped_reason(&document.execution.stopped_reason).to_string(),
                unattempted: document.execution.unattempted,
                restarts: document.execution.restarts,
            },
            findings,
            truncated,
            report_path: rendered.then(|| report_path.display().to_string()),
            markdown,
        };
        structured(&result)
    }
}

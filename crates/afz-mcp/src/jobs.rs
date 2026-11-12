//! The job registry and the bridge to the blocking engine call.
//!
//! `afz_engine::scan::run` blocks for the whole scan and names its output directory only
//! when it writes it at the end, so a scan runs on `tokio::task::spawn_blocking` and the tool
//! call returns a server-minted handle: `job-<scan-id>`. The only in-flight control the engine
//! offers is `Options::stop`, an `Arc<AtomicBool>` it polls between batches
//! (`crates/afz-engine/src/scan.rs`, `loop_until_budget`), so `afz_scan_cancel` sets that flag
//! and the scan still writes its result, with `stoppedReason: "interrupted"`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use afz_engine::report::scanresult::{self, GitScope};
use afz_engine::scan::{self, Options, Progress};

use crate::project::Resolved;

/// Where a job is. `running` until the engine returns; then the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Running => "running",
            JobState::Completed => "completed",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

/// One scan this server started, and everything a status call can report about it without
/// re-reading the output directory.
#[derive(Debug)]
pub struct Job {
    pub id: String,
    pub root: PathBuf,
    pub out_dir: PathBuf,
    pub started: Instant,
    pub stop: Arc<AtomicBool>,
    /// Set by `afz_scan_cancel`; decides whether a finished scan reports `cancelled` or
    /// `completed`, because the engine's own `stoppedReason` cannot tell a cancel from any
    /// other early stop.
    pub cancel_requested: bool,
    pub state: JobState,
    pub progress: Option<Progress>,
    /// `options.invocations`, the progress denominator.
    pub budget: u64,
    /// The wall clock the scan was given, so a status call can restate the budget the same
    /// way `afz_scan` did.
    pub wall_seconds: f64,
    pub scan_id: Option<String>,
    pub dir: Option<PathBuf>,
    /// The change set the scan was limited to, taken from the written document when the scan
    /// ends; a running scan has not computed it yet.
    pub git: Option<GitScope>,
    /// `document.execution.stoppedReason` as the document spells it.
    pub stopped_reason: Option<String>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Registry {
    pub jobs: BTreeMap<String, Job>,
    /// Insertion order, so "the newest job" does not depend on the id's sort order.
    pub order: Vec<String>,
    /// The one scan allowed to run at a time. `budget.workers` defaults to the machine's
    /// core count (ADR 0011), so two concurrent scans would only halve each other; a harness
    /// that wants both starts two servers.
    pub active: Option<String>,
}

/// The registry every tool call shares. A `std::sync::Mutex`, never held across an `.await`.
pub type Shared = Arc<Mutex<Registry>>;

/// Lock the registry, recovering the guard if a previous holder panicked: a poisoned lock
/// still holds a consistent `Registry`, and dropping in-flight jobs would be worse.
pub fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Registry> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Build the `Options` for one scan: the scan project's defaults, the resolved paths, and
/// the caller's overrides. The stop flag is installed here so [`start`] can hand the same
/// `Arc` to the registry.
pub fn build(resolved: &Resolved, configure: impl FnOnce(&mut Options)) -> Options {
    let mut options = Options::new(resolved.loaded.clone(), resolved.scan_root.clone());
    options.out_dir = resolved.out_dir.clone();
    configure(&mut options);
    // This assignment is the whole cancellation mechanism: the engine reads
    // `self.options.stop` between batches and ends the run with `stoppedReason: interrupted`.
    options.stop = Arc::new(AtomicBool::new(false));
    options
}

/// Start one built scan and return its job id. Fails when another scan is still running:
/// `budget.workers` defaults to the machine's core count (ADR 0011), so two concurrent
/// scans would only halve each other.
pub fn start(shared: &Shared, options: Options) -> Result<String, String> {
    let stop = Arc::clone(&options.stop);
    let id = format!("job-{}", scanresult::scan_id(SystemTime::now()));
    let mut registry = lock(shared);
    if let Some(active) = registry.active.clone() {
        return Err(format!("a scan is already running: {active}; wait for it with afz_scan_status or stop it with afz_scan_cancel"));
    }
    registry.jobs.insert(
        id.clone(),
        Job {
            id: id.clone(),
            root: options.root.clone(),
            out_dir: options.out_dir.clone(),
            started: Instant::now(),
            stop,
            cancel_requested: false,
            state: JobState::Running,
            progress: None,
            budget: options.invocations,
            wall_seconds: options.wall_clock.as_secs_f64(),
            scan_id: None,
            dir: None,
            git: None,
            stopped_reason: None,
            error: None,
        },
    );
    registry.order.push(id.clone());
    registry.active = Some(id.clone());
    drop(registry);
    let id_for_task = id.clone();
    // The task outlives this call, so it holds its own handle to the registry.
    let shared = Arc::clone(shared);
    tokio::task::spawn_blocking(move || {
        let id = id_for_task;
        // `scan::run` is blocking; every sample it reports goes straight into the registry so
        // a status call sees live progress. `catch_unwind` keeps a panicking engine from
        // leaving the job `running` forever and refusing every later scan.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scan::run(&options, &mut |sample: &Progress| {
                if let Some(job) = lock(&shared).jobs.get_mut(&id) {
                    job.progress = Some(sample.clone());
                }
            })
        }));
        let report = match outcome {
            Ok(report) => report,
            Err(_) => {
                let mut registry = lock(&shared);
                if let Some(job) = registry.jobs.get_mut(&id) {
                    job.state = JobState::Failed;
                    job.error = Some("the scan thread panicked; see the server's stderr".to_string());
                }
                registry.active = None;
                return;
            }
        };
        let mut registry = lock(&shared);
        if let Some(job) = registry.jobs.get_mut(&id) {
            match report {
                Ok(report) => {
                    job.scan_id = report.dir.file_name().map(|name| name.to_string_lossy().into_owned());
                    job.dir = Some(report.dir.clone());
                    job.git = report.document.git.clone();
                    job.stopped_reason = Some(stopped_reason(&report.document.execution.stopped_reason).to_string());
                    job.state = if job.cancel_requested { JobState::Cancelled } else { JobState::Completed };
                }
                Err(e) => {
                    job.state = JobState::Failed;
                    job.error = Some(e.to_string());
                }
            }
        }
        registry.active = None;
    });
    Ok(id)
}

/// The newest job the server knows, running or not.
pub fn newest(registry: &Registry) -> Option<&Job> {
    registry.order.last().and_then(|id| registry.jobs.get(id))
}

/// Resolve a job by id, accepting both the `job-…` handle and the bare engine scan id the
/// result directory is named after; `None` means the newest job.
pub fn find<'a>(registry: &'a Registry, id: Option<&str>) -> Option<&'a Job> {
    let Some(id) = id else { return newest(registry) };
    if let Some(job) = registry.jobs.get(id) {
        return Some(job);
    }
    registry.jobs.values().find(|job| job.scan_id.as_deref() == Some(id))
}

/// The same lookup as [`find`], for the caller that must set the stop flag.
pub fn find_mut<'a>(registry: &'a mut Registry, id: Option<&str>) -> Option<&'a mut Job> {
    let key = match id {
        Some(id) if registry.jobs.contains_key(id) => Some(id.to_string()),
        Some(id) => registry.jobs.values().find(|job| job.scan_id.as_deref() == Some(id)).map(|job| job.id.clone()),
        None => registry.order.last().cloned(),
    };
    key.and_then(|key| registry.jobs.get_mut(&key))
}

/// Ask a running job to stop between batches. Returns whether the flag was newly set; a
/// finished job ignores the request.
pub fn request_stop(job: &mut Job) -> bool {
    if job.state != JobState::Running {
        return false;
    }
    job.cancel_requested = true;
    let was_stopped = job.stop.swap(true, Ordering::Relaxed);
    !was_stopped
}

/// Set `stop` on every running job; returns how many were asked to stop. Used at shutdown so
/// the engine reaps its plugin processes before the process exits.
pub fn stop_everything(shared: &Shared) -> usize {
    let mut stopped = 0;
    for job in lock(shared).jobs.values_mut() {
        if request_stop(job) {
            stopped += 1;
        }
    }
    stopped
}

/// The written document's spelling of why the run ended.
pub fn stopped_reason(reason: &scanresult::StoppedReason) -> &'static str {
    match reason {
        scanresult::StoppedReason::Budget => "budget",
        scanresult::StoppedReason::Complete => "complete",
        scanresult::StoppedReason::Interrupted => "interrupted",
    }
}

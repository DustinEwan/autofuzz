//! Execution supervision: one plugin process per worker, engine-side deadlines, and the
//! restart accounting that decides when a target is doing more harm than good.
//!
//! The engine, not the plugin, enforces `deadlineMs`. A plugin that is still computing at the
//! deadline cannot be trusted to stop itself, and its answer is useless by then, so the engine
//! kills that process, restarts it, and records the invocation's coverage as lost.
//!
//! Results come back out of order because the workers run in parallel. Every [`Completed`]
//! carries its `invocation_id` and the report is sorted by it, so the scan loop consumes
//! invocations in the order the seed produced them and a replay records the same corpus.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use afz_protocol::{DiscoverReq, FaultSignature, InvokeReq, Outcome, ValueTree};

use crate::rpc::client::{Launch, LostReason, PluginClient};

/// Restarts attributed to one target before the engine disables it. The design's number.
pub const DISABLE_AFTER: u64 = 50;

/// How long a plugin may take to answer `discover` before its process is treated as broken.
/// The same allowance `afz probe-plugin` gives, because discovery walks the whole scan tree and
/// is the one request whose cost is the size of the project rather than of one input.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(120);

/// One invocation the supervisor must run.
#[derive(Debug, Clone)]
pub struct Job {
    pub invocation_id: u64,
    pub target_id: String,
    pub values: Vec<ValueTree>,
    pub collect_coverage: bool,
}

/// A plugin process was restarted, and what it was doing when it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restart {
    pub worker: usize,
    pub reason: LostReason,
}

/// One finished invocation.
#[derive(Debug, Clone)]
pub struct Completed {
    pub invocation_id: u64,
    pub target_id: String,
    pub outcome: Outcome,
    /// The engine restarted a plugin process on this invocation's account.
    pub restart: Option<Restart>,
}

impl Completed {
    /// Build the invocation request the plugin sees.
    pub fn request_for(job: &Job, deadline: Duration) -> InvokeReq {
        InvokeReq {
            invocation_id: job.invocation_id,
            target_id: job.target_id.clone(),
            values: job.values.clone(),
            deadline_ms: deadline.as_millis() as u64,
            collect_coverage: job.collect_coverage,
        }
    }
}

/// A target the plugin says it cannot build, reported once per target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unbuildable {
    pub target_id: String,
    pub reason: afz_protocol::SkipReason,
}

/// What one worker did with one job.
#[derive(Debug, Clone)]
pub enum Event {
    Ran(Completed),
    /// `prepare` reported that this target cannot be built, so the invocation never ran.
    Unbuildable { invocation_id: u64, unbuildable: Unbuildable },
}

/// What a run produced.
#[derive(Debug, Default)]
pub struct RunReport {
    /// Attempted invocations, sorted by `invocation_id`.
    pub completed: Vec<Completed>,
    /// Jobs never attempted because their target was disabled, proved unbuildable, or the wall
    /// clock ran out, by invocation id. The scan result must account for every invocation it
    /// planned.
    pub unattempted: Vec<u64>,
    pub restarts: u64,
    /// Targets the engine disabled during this run.
    pub disabled: Vec<String>,
    /// Targets the plugin refused to build, with the reason it gave.
    pub unbuildable: Vec<Unbuildable>,
}

/// Restart accounting and the disable rule.
#[derive(Debug, Clone)]
pub struct Watchdog {
    disable_after: u64,
    consecutive: BTreeMap<String, u64>,
    disabled: BTreeSet<String>,
    restarts: u64,
}

impl Watchdog {
    pub fn new(disable_after: u64) -> Watchdog {
        Watchdog { disable_after, consecutive: BTreeMap::new(), disabled: BTreeSet::new(), restarts: 0 }
    }

    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    pub fn disabled(&self) -> Vec<String> {
        self.disabled.iter().cloned().collect()
    }

    pub fn is_disabled(&self, target_id: &str) -> bool {
        self.disabled.contains(target_id)
    }

    /// Record a restart against the target that was running. Returns `true` when this restart
    /// crossed the disable threshold.
    pub fn note_restart(&mut self, target_id: &str) -> bool {
        self.restarts += 1;
        let count = self.consecutive.entry(target_id.to_string()).or_insert(0);
        *count += 1;
        if *count >= self.disable_after && self.disabled.insert(target_id.to_string()) {
            return true;
        }
        false
    }

    /// An answered invocation breaks the losing streak for that target.
    pub fn note_answered(&mut self, target_id: &str) {
        self.consecutive.insert(target_id.to_string(), 0);
    }

    pub fn streak(&self, target_id: &str) -> u64 {
        self.consecutive.get(target_id).copied().unwrap_or(0)
    }
}

/// The supervisor owns the plugin processes and the wall clock for one run.
pub struct Supervisor {
    launch: Launch,
    discovery: DiscoverReq,
    workers: usize,
    deadline: Duration,
    watchdog: Watchdog,
}

impl Supervisor {
    /// `discovery` is replayed into every plugin process this supervisor starts. `prepare`
    /// resolves a `targetId` only against an inventory that process discovered itself, so a
    /// restart has to discover again: the new process knows nothing.
    pub fn new(launch: Launch, workers: usize, deadline: Duration, disable_after: u64, discovery: DiscoverReq) -> Supervisor {
        Supervisor { launch, discovery, workers: workers.max(1), deadline, watchdog: Watchdog::new(disable_after) }
    }

    pub fn watchdog(&self) -> &Watchdog {
        &self.watchdog
    }

    /// Run `jobs` across the worker pool, stopping at `wall_clock` and never later.
    ///
    /// A plugin that will not answer is killed and restarted inside the worker thread, so one
    /// hung target cannot stall the pool.
    pub fn run(&mut self, jobs: Vec<Job>, wall_clock: Duration) -> RunReport {
        let started = Instant::now();
        let budget = wall_clock.min(Duration::from_secs(3600 * 24));
        let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
        let results: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        // Workers share one watchdog so the disable threshold counts restarts across the pool,
        // and it comes back to `self` when the last thread finishes.
        let watchdog = Arc::new(Mutex::new(self.watchdog.clone()));

        let mut threads = Vec::with_capacity(self.workers);
        for index in 0..self.workers {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            let watchdog = Arc::clone(&watchdog);
            let launch = self.launch.clone();
            let deadline = self.deadline;
            let discovery = self.discovery.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(format!("afz-worker-{index}"))
                    .spawn(move || {
                        let mut worker = Worker::spawn(index, &launch, &discovery);
                        loop {
                            // Leave room for one more deadline, so a run stops on its budget
                            // rather than on the last invocation that happened to start.
                            if started.elapsed() + deadline >= budget {
                                return;
                            }
                            let Some(job) = Self::take(&queue, &watchdog) else { return };
                            let completed = worker.invoke(job, deadline, &mut watchdog.lock().expect("watchdog lock"));

                            results.lock().expect("results lock").push(completed);
                        }
                    })
                    .expect("spawn worker"),
            );
        }

        for thread in threads {
            let _ = thread.join();
        }

        self.watchdog = Arc::try_unwrap(watchdog).expect("watchdog still shared").into_inner().expect("watchdog lock");
        let events = Arc::try_unwrap(results).expect("results still shared").into_inner().expect("results lock");
        let mut completed: Vec<Completed> = events.iter().filter_map(|e| match e { Event::Ran(c) => Some(c.clone()), _ => None }).collect();
        completed.sort_by_key(|c| c.invocation_id);
        let mut unbuildable: Vec<Unbuildable> = events
            .iter()
            .filter_map(|e| match e { Event::Unbuildable { unbuildable, .. } => Some(unbuildable.clone()), _ => None })
            .collect();
        unbuildable.sort_by(|a, b| a.target_id.cmp(&b.target_id));
        unbuildable.dedup_by(|a, b| a.target_id == b.target_id);
        let mut unattempted: Vec<u64> = events
            .iter()
            .filter_map(|e| match e { Event::Unbuildable { invocation_id, .. } => Some(*invocation_id), _ => None })
            .collect();
        unattempted.extend(queue.lock().expect("queue lock").iter().map(|j| j.invocation_id));
        unattempted.sort_unstable();
        let disabled = self.watchdog.disabled();
        RunReport { completed, unattempted, restarts: self.watchdog.restarts(), disabled, unbuildable }
    }

    /// Pull the next job that is worth running. A job whose target has been disabled is dropped
    /// here rather than reported as an invocation: the design records it as a skipped target.
    fn take(queue: &Arc<Mutex<VecDeque<Job>>>, watchdog: &Arc<Mutex<Watchdog>>) -> Option<Job> {
        let disabled = watchdog.lock().expect("watchdog lock").disabled();
        let mut queue = queue.lock().expect("queue lock");
        if disabled.is_empty() {
            return queue.pop_front();
        }
        queue.retain(|job| !disabled.contains(&job.target_id));
        queue.pop_front()
    }
}

/// One plugin process, plus the per-target `prepare` state it holds.
pub struct Worker {
    index: usize,
    launch: Launch,
    discovery: DiscoverReq,
    client: Option<PluginClient>,
    prepared: BTreeSet<String>,
    unbuildable: BTreeSet<String>,
    restarts: u64,
}

impl Worker {
    fn spawn(index: usize, launch: &Launch, discovery: &DiscoverReq) -> Worker {
        Worker {
            index,
            launch: launch.clone(),
            discovery: discovery.clone(),
            client: Worker::start(index, launch, discovery),
            prepared: BTreeSet::new(),
            unbuildable: BTreeSet::new(),
            restarts: 0,
        }
    }

    /// Bring a process up and give it the inventory it needs to answer `prepare`. A plugin that
    /// fails discovery stays up: every `prepare` then reports the plugin's own reason, which is
    /// the diagnostic the scan result should carry rather than a swallowed error here.
    fn start(index: usize, launch: &Launch, discovery: &DiscoverReq) -> Option<PluginClient> {
        let client = PluginClient::spawn(launch).ok()?;
        if let Err(reason) = client.discover(discovery, DISCOVERY_TIMEOUT) {
            eprintln!("afz: worker {index} discovery failed: {}", reason.detail());
        }
        Some(client)
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// The process is gone or was never startable; bring up a fresh one and forget what it had
    /// prepared, because the new process has prepared nothing.
    fn restart(&mut self) {
        if let Some(client) = self.client.take() {
            client.kill();
        }
        self.client = Worker::start(self.index, &self.launch, &self.discovery);
        self.prepared.clear();
        self.restarts += 1;
    }

    /// Run one invocation and classify whatever came back.
    ///
    /// The plugin call happens while `self` is borrowed immutably, so the attempt is collected
    /// first and the restart that follows it runs on a free `self`.
    fn invoke(&mut self, job: Job, deadline: Duration, watchdog: &mut Watchdog) -> Event {
        let attempt = match self.client.as_ref() {
            None => Attempt::Absent,
            Some(client) => {
                // `prepare` is asked once per target per process: it builds the receiver, and
                // the plugin keeps it so `invoke` does not rebuild it. A refusal is a fact about
                // the target rather than about this input, so the invocation is not run and the
                // target is reported instead.
                if self.prepared.insert(job.target_id.clone()) {
                    match client.prepare(&afz_protocol::PrepareReq { target_id: job.target_id.clone(), values: job.values.clone() }, deadline) {
                        Ok(afz_protocol::PrepareResp::Ok { .. }) => {}
                        Ok(afz_protocol::PrepareResp::Skip { reason }) => {
                            self.prepared.remove(&job.target_id);
                            return Event::Unbuildable {
                                invocation_id: job.invocation_id,
                                unbuildable: Unbuildable { target_id: job.target_id, reason },
                            };
                        }
                        Err(reason) => return self.classify_loss(job, LostReason::Spawn(format!("prepare: {}", reason.detail())), watchdog),
                    }
                }
                match client.invoke(&Completed::request_for(&job, deadline), deadline) {
                    Ok(outcome) => Attempt::Answered(outcome),
                    Err(reason) => Attempt::Lost(reason),
                }
            }
        };

        match attempt {
            Attempt::Answered(outcome) => {
                watchdog.note_answered(&job.target_id);
                Event::Ran(Completed {
                    invocation_id: job.invocation_id,
                    target_id: job.target_id,
                    outcome,
                    restart: None,
                })
            }
            Attempt::Lost(reason) => self.classify_loss(job, reason, watchdog),
            // The process is gone or was never startable. That is a process exit as far as the
            // scan is concerned, and the next invocation gets a fresh process.
            Attempt::Absent => self.classify_loss(job, LostReason::Spawn("plugin is not running".to_string()), watchdog),
        }
    }

    /// Turn a call that got no answer into the finding the design names, restart the process,
    /// and account for the restart against the target that was running.
    fn classify_loss(&mut self, job: Job, reason: LostReason, watchdog: &mut Watchdog) -> Event {
        let outcome = match &reason {
            // The engine waited for the answer until the deadline and none arrived.
            LostReason::NoAnswer => engine_fault("deadline", "Deadline", reason.detail()),
            // The plugin ended itself with a call in flight and left no explanation.
            LostReason::Exited { .. } => engine_fault("processExit", "ProcessExit", reason.detail()),
            other => engine_fault(other.via(), "PluginLost", other.detail()),
        };
        self.restart();
        let disabled = watchdog.note_restart(&job.target_id);
        if disabled {
            eprintln!("afz: target {} disabled after {} consecutive restarts", job.target_id, watchdog.streak(&job.target_id));
        }
        Event::Ran(Completed {
            invocation_id: job.invocation_id,
            target_id: job.target_id,
            outcome,
            restart: Some(Restart { worker: self.index, reason }),
        })
    }
}

/// What one plugin process did with an attempt, before the engine classifies it.
enum Attempt {
    Answered(Outcome),
    Lost(LostReason),
    /// The process is gone: it exited between invocations or never started.
    Absent,
}

impl Worker {
    /// Targets this process refused to build. They are reported once, not per invocation.
    pub fn unbuildable(&self) -> &BTreeSet<String> {
        &self.unbuildable
    }
}

/// An outcome the engine produced because the plugin could not.
fn engine_fault(via: &str, class: &str, detail: String) -> Outcome {
    Outcome::Failed(afz_protocol::FailedOutcome {
        duration_ms: 0.0,
        coverage: None,
        via: via.to_string(),
        fault: FaultSignature { class: class.to_string(), message: detail, frames: Vec::new() },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifty_consecutive_restarts_disable_one_target_only() {
        let mut watchdog = Watchdog::new(DISABLE_AFTER);
        for _ in 0..DISABLE_AFTER - 1 {
            assert!(!watchdog.note_restart("afz:t:bad"));
        }
        assert!(watchdog.note_restart("afz:t:bad"));
        assert!(watchdog.is_disabled("afz:t:bad"));
        assert!(!watchdog.is_disabled("afz:t:good"));
        assert_eq!(watchdog.restarts(), DISABLE_AFTER);
        // A further restart does not report a second disable.
        assert!(!watchdog.note_restart("afz:t:bad"));
    }

    #[test]
    fn an_answered_invocation_breaks_the_streak() {
        let mut watchdog = Watchdog::new(3);
        assert!(!watchdog.note_restart("t"));
        assert!(!watchdog.note_restart("t"));
        watchdog.note_answered("t");
        assert_eq!(watchdog.streak("t"), 0);
        assert!(!watchdog.note_restart("t"));
        assert!(!watchdog.note_restart("t"));
        assert!(!watchdog.is_disabled("t"), "the streak never reached three in a row");
    }
}

//! Execution supervision: one plugin process per worker, engine-side deadlines, and the
//! restart accounting that decides when a target is doing more harm than good.
//!
//! The engine, not the plugin, owns the wall clock. The plugin is handed a `deadlineMs` strictly
//! shorter than the time the engine will wait, so a harness that can cut its own call answers
//! inside the engine's wait; a plugin that is still computing when that wait ends cannot be
//! trusted to stop itself, and its answer is useless by then, so the engine kills that process,
//! restarts it, and records the invocation's coverage as lost.
//!
//! Results come back out of order because the workers run in parallel. Every [`Completed`]
//! carries its `invocation_id` and the report is sorted by it, so the scan loop consumes
//! invocations in the order the seed produced them and a replay records the same corpus.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use afz_protocol::{
    Call, CoverageEntry, CoverageMap, DiscoverReq, EdgeId, ErrorObject, FaultSignature, InvokeReq,
    Loss, Outcome, PluginConfig, Timeouts, ValueTree,
};

use crate::rpc::client::{spawn_discovered, via, PluginClient};
use crate::scan::PREPARE_TIMEOUT;

/// Attempts that did not run, attributed to one target before the engine disables it: a restart
/// after a lost process, or a refusal from a live one. The design's number.
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
    pub reason: Loss,
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
    /// Build the invocation request the plugin sees. `deadline` is the configured invocation
    /// deadline, which is this profile's `deadlineMs` and goes on the wire unchanged; the engine's
    /// own wait is derived from it by [`Timeouts::host_wait`] at the call site - never a second
    /// number written beside it.
    pub fn request_for(job: &Job, deadline: Duration) -> InvokeReq {
        InvokeReq {
            invocation_id: job.invocation_id,
            target_id: job.target_id.clone(),
            values: job.values.clone(),
            deadline_ms: deadline.as_millis().try_into().unwrap_or(u64::MAX),
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
    Unbuildable {
        invocation_id: u64,
        unbuildable: Unbuildable,
    },
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
    /// Plugin processes this `run` call restarted. The watchdog carries its count across batches,
    /// so the report says what this batch cost, which is what the scan loop adds up.
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
        Watchdog {
            disable_after,
            consecutive: BTreeMap::new(),
            disabled: BTreeSet::new(),
            restarts: 0,
        }
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
        *count >= self.disable_after && self.disabled.insert(target_id.to_string())
    }

    /// Record an attempt that produced no result because the plugin declined the work. It counts
    /// toward the same disable rule as a restart, because the protection that rule buys is "stop
    /// invoking a target that never runs", and a plugin that refuses every call needs it as much
    /// as a plugin that dies on every call. It is not a restart, so it does not raise the run's
    /// restart count. Returns `true` when this crossed the disable threshold.
    pub fn note_refusal(&mut self, target_id: &str) -> bool {
        let count = self.consecutive.entry(target_id.to_string()).or_insert(0);
        *count += 1;
        *count >= self.disable_after && self.disabled.insert(target_id.to_string())
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
    launch: PluginConfig,
    discovery: DiscoverReq,
    /// One plugin process per worker. They outlive a batch: the next batch starts against
    /// processes that have already discovered the inventory and prepared their targets, and only
    /// a process that has run invocations knows the target's coverage blocks.
    workers: Vec<Worker>,
    deadline: Duration,
    watchdog: Watchdog,
}

impl Supervisor {
    /// `discovery` is replayed into every plugin process this supervisor starts. `prepare`
    /// resolves a `targetId` only against an inventory that process discovered itself, so a
    /// restart has to discover again: the new process knows nothing.
    pub fn new(
        launch: PluginConfig,
        workers: usize,
        deadline: Duration,
        disable_after: u64,
        discovery: DiscoverReq,
    ) -> Supervisor {
        // `Worker::spawn` blocks on that process's `discover`, so mapping over the indices serially
        // would make a default scan pay `(workers + 1)` discoveries before its first invocation and
        // again on every restart. They overlap instead.
        //
        // Joining in index order is load-bearing, not tidiness: `Vec<Worker>` order decides which
        // process pulls which job, and `docs/decisions/0011-….md` records that changing the worker
        // count changed which findings a seed produced. The trajectory must stay a function of the
        // seed and the worker count, never of which process answered first.
        let pool = {
            let (launch, discovery) = (&launch, &discovery);
            std::thread::scope(|scope| {
                let starting: Vec<_> = (0..workers.max(1))
                    .map(|index| scope.spawn(move || Worker::spawn(index, launch, discovery)))
                    .collect();
                starting
                    .into_iter()
                    .map(|thread| thread.join().expect("worker spawn thread"))
                    .collect()
            })
        };
        Supervisor {
            launch,
            discovery,
            workers: pool,
            deadline,
            watchdog: Watchdog::new(disable_after),
        }
    }

    /// The blocks every worker has reported, unioned. Only a process that has run an invocation
    /// knows a module's blocks, and each worker has loaded only the modules of the targets it ran,
    /// so the reachable set of the run is the pool's union. The engine's own plugin client only
    /// prepares and minimizes without collecting coverage, so it is not asked.
    pub fn coverage_map(&self, timeout: Duration) -> Option<CoverageMap> {
        let mut entries: BTreeMap<EdgeId, CoverageEntry> = BTreeMap::new();
        for worker in &self.workers {
            if let Some(map) = worker.coverage_map(timeout) {
                for entry in map.entries {
                    entries.entry(entry.edge).or_insert(entry);
                }
            }
        }
        (!entries.is_empty()).then(|| CoverageMap {
            entries: entries.into_values().collect(),
        })
    }

    /// What the plugin processes wrote to stderr, deduplicated across the pool. Twelve workers repeat
    /// the same `prepare` refusal twelve times, and a crash text is worth one line.
    pub fn take_stderr(&mut self) -> Vec<String> {
        const CAP: usize = 200;
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out: Vec<String> = Vec::new();
        let mut suppressed = 0_usize;
        for worker in &mut self.workers {
            for line in worker.take_stderr() {
                if line.trim().is_empty() || seen.contains(&line) {
                    continue;
                }
                if seen.len() >= CAP {
                    suppressed += 1;
                    continue;
                }
                seen.insert(line.clone());
                out.push(line);
            }
        }
        if suppressed > 0 {
            out.push(format!(
                "... {suppressed} more distinct plugin stderr lines suppressed"
            ));
        }
        out
    }

    /// The `id` 0 refusals the pool's plugins reported against the connection itself (spec §4).
    /// They answer no particular call, so no finding carries them and the scan log has to.
    pub fn connection_faults(&mut self) -> Vec<String> {
        self.workers
            .iter_mut()
            .flat_map(Worker::connection_faults)
            .collect()
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
        let restarts_before = self.watchdog.restarts();
        let budget = wall_clock.min(Duration::from_secs(3600 * 24));
        let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
        let results: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        // Workers share one watchdog so the disable threshold counts restarts across the pool,
        // and it comes back to `self` when the last thread finishes.
        let watchdog = Arc::new(Mutex::new(self.watchdog.clone()));

        let deadline = self.deadline;
        // Each thread borrows one worker for the batch and hands it back afterwards, so the pool
        // outlives the batch. The slot is behind a lock because a thread can panic on a bad job.
        let slots: Vec<Arc<Mutex<Option<Worker>>>> = self
            .workers
            .drain(..)
            .map(|worker| Arc::new(Mutex::new(Some(worker))))
            .collect();
        let mut threads = Vec::with_capacity(slots.len());
        for (index, slot) in slots.iter().enumerate() {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            let watchdog = Arc::clone(&watchdog);
            let slot = Arc::clone(slot);
            threads.push(
                std::thread::Builder::new()
                    .name(format!("afz-worker-{index}"))
                    .spawn(move || {
                        let Some(mut worker) = slot.lock().expect("worker slot lock").take() else { return };
                        loop {
                            // Leave room for one more deadline, so a run stops on its budget
                            // rather than on the last invocation that happened to start. No kill
                            // grace is counted here because there is no second number to count:
                            // the plugin's `deadlineMs` is derived from this same wait, so an
                            // answer that arrives at all arrives before the deadline ends.
                            if started.elapsed() + deadline >= budget {
                                break;
                            }
                            let Some(job) = Self::take(&queue, &watchdog) else { break };
                            // The plugin call runs with no lock held. The watchdog is shared by
                            // the whole pool, and a guard across a call that can wait out a
                            // deadline would run every worker one invocation at a time.
                            let (event, watch) = worker.invoke(job, deadline);
                            {
                                let mut guard = watchdog.lock().expect("watchdog lock");
                                match watch {
                                    Watch::Answered(target) => guard.note_answered(&target),
                                    Watch::Restarted(target) => {
                                        if guard.note_restart(&target) {
                                            eprintln!(
                                                "afz: target {target} disabled after {} consecutive restarts",
                                                guard.streak(&target)
                                            );
                                        }
                                    }
                                    Watch::Refused(target) => {
                                        if guard.note_refusal(&target) {
                                            eprintln!(
                                                "afz: target {target} disabled after {} consecutive refusals",
                                                guard.streak(&target)
                                            );
                                        }
                                    }
                                    Watch::Nothing => {}
                                }
                            }
                            results.lock().expect("results lock").push(event);
                        }
                        *slot.lock().expect("worker slot lock") = Some(worker);
                    })
                    .expect("spawn worker"),
            );
        }

        for thread in threads {
            let _ = thread.join();
        }

        // Hand the pool back. A thread that panicked never returned its worker, so that slot is
        // refilled with a fresh process and the next batch still runs at the full worker count.
        self.workers = slots
            .into_iter()
            .enumerate()
            .map(|(index, slot)| {
                // A panicked thread leaves its slot poisoned as well as empty, and a poisoned
                // mutex cannot yield the process inside it, so both cases start a fresh one.
                Arc::into_inner(slot)
                    .and_then(|slot| slot.into_inner().ok())
                    .flatten()
                    .unwrap_or_else(|| Worker::spawn(index, &self.launch, &self.discovery))
            })
            .collect();

        self.watchdog = Arc::try_unwrap(watchdog)
            .expect("watchdog still shared")
            .into_inner()
            .expect("watchdog lock");
        let events = Arc::try_unwrap(results)
            .expect("results still shared")
            .into_inner()
            .expect("results lock");
        let mut completed: Vec<Completed> = events
            .iter()
            .filter_map(|e| match e {
                Event::Ran(c) => Some(c.clone()),
                _ => None,
            })
            .collect();
        completed.sort_by_key(|c| c.invocation_id);
        let mut unbuildable: Vec<Unbuildable> = events
            .iter()
            .filter_map(|e| match e {
                Event::Unbuildable { unbuildable, .. } => Some(unbuildable.clone()),
                _ => None,
            })
            .collect();
        unbuildable.sort_by(|a, b| a.target_id.cmp(&b.target_id));
        unbuildable.dedup_by(|a, b| a.target_id == b.target_id);
        let mut unattempted: Vec<u64> = events
            .iter()
            .filter_map(|e| match e {
                Event::Unbuildable { invocation_id, .. } => Some(*invocation_id),
                _ => None,
            })
            .collect();
        unattempted.extend(
            queue
                .lock()
                .expect("queue lock")
                .iter()
                .map(|j| j.invocation_id),
        );
        unattempted.sort_unstable();
        let disabled = self.watchdog.disabled();
        RunReport {
            completed,
            unattempted,
            restarts: self.watchdog.restarts() - restarts_before,
            disabled,
            unbuildable,
        }
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
    launch: PluginConfig,
    discovery: DiscoverReq,
    client: Option<PluginClient>,
    prepared: BTreeSet<String>,
    unbuildable: BTreeSet<String>,
    restarts: u64,
    /// Stderr buffers of the processes this worker has replaced. A plugin's last lines are lost with
    /// its client unless the worker holds on to them.
    stderr_history: Vec<Arc<Mutex<Vec<String>>>>,
    /// The connection's own faults, as sentences. A restart drops the client that reported one, so
    /// the reason outlives the process that gave it.
    faults: Vec<String>,
}

impl Worker {
    fn spawn(index: usize, launch: &PluginConfig, discovery: &DiscoverReq) -> Worker {
        Worker {
            index,
            launch: launch.clone(),
            discovery: discovery.clone(),
            client: Worker::start(index, launch, discovery),
            prepared: BTreeSet::new(),
            unbuildable: BTreeSet::new(),
            restarts: 0,
            stderr_history: Vec::new(),
            faults: Vec::new(),
        }
    }

    /// Bring a process up and give it the inventory it needs to answer `prepare`. A plugin that
    /// fails discovery stays up: every `prepare` then reports the plugin's own reason, which is
    /// the diagnostic the scan result should carry rather than a swallowed error here.
    fn start(index: usize, launch: &PluginConfig, discovery: &DiscoverReq) -> Option<PluginClient> {
        let (client, lost) = match spawn_discovered(launch, discovery) {
            Ok(started) => started,
            // `Lost` is a process that never came up; `Refused` is a process that is alive and
            // declined to be a plugin here. There is nothing to invoke either way, but the line
            // the operator reads has to say which of the two it is.
            Err(error) => {
                eprintln!("afz: worker {index} could not start the plugin: {error}");
                return None;
            }
        };
        if let Some(reason) = lost {
            eprintln!("afz: worker {index} discovery failed: {reason}");
        }
        Some(client)
    }

    pub fn index(&self) -> usize {
        self.index
    }

    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// The coverage blocks this process has reported, or nothing when it has not run an
    /// invocation. V8 lists a function's blocks only once that function has run under coverage,
    /// so a process that has only discovered and prepared answers with an empty table.
    pub fn coverage_map(&self, timeout: Duration) -> Option<CoverageMap> {
        let client = self.client.as_ref()?;
        client
            .coverage_map(timeout)
            .payload()
            .filter(|map| !map.entries.is_empty())
    }

    /// Lines this plugin process wrote to stderr, taken so the scan reports each one once. A plugin
    /// that exits on its own leaves the reason here and nowhere else: the engine records the exit
    /// code, which says nothing about why.
    pub fn take_stderr(&mut self) -> Vec<String> {
        let mut buffers: Vec<Arc<_>> = self.stderr_history.iter().map(Arc::clone).collect();
        if let Some(client) = self.client.as_ref() {
            buffers.push(client.stderr_lines());
        }
        self.stderr_history.clear();
        buffers
            .into_iter()
            .flat_map(|lines| std::mem::take(&mut *lines.lock().expect("stderr lock")))
            .collect()
    }

    /// The connection faults this worker's processes reported, taken so the scan reports each once.
    pub fn connection_faults(&mut self) -> Vec<String> {
        if let Some(client) = self.client.as_ref() {
            if let Some(fault) = client.connection_fault() {
                self.faults
                    .push(format!("worker {}: {} {}", self.index, fault.code, fault.message));
            }
        }
        self.faults.sort();
        self.faults.dedup();
        std::mem::take(&mut self.faults)
    }

    /// The process is gone or was never startable; bring up a fresh one and forget what it had
    /// prepared, because the new process has prepared nothing.
    fn restart(&mut self) {
        if let Some(client) = self.client.take() {
            // The process is gone; its stderr buffer and its connection fault are kept. The drain
            // thread is still reading the closed pipe, and what the plugin wrote before it died is
            // the only reason the engine has.
            self.stderr_history.push(client.stderr_lines());
            if let Some(fault) = client.connection_fault() {
                self.faults
                    .push(format!("worker {}: {} {}", self.index, fault.code, fault.message));
            }
            client.kill();
        }
        self.client = Worker::start(self.index, &self.launch, &self.discovery);
        self.prepared.clear();
        self.restarts += 1;
    }

    /// Run one invocation and classify whatever came back.
    ///
    /// The plugin call happens while `self` is borrowed immutably, so the attempt is collected
    /// first and the restart that follows it runs on a free `self`. What the attempt means for
    /// the shared watchdog comes back as a `Watch` for the caller to apply under the lock.
    fn invoke(&mut self, job: Job, deadline: Duration) -> (Event, Watch) {
        let attempt = match self.client.as_ref() {
            None => Attempt::Absent,
            Some(client) => {
                // `prepare` is asked once per target per process: it builds the receiver, and
                // the plugin keeps it so `invoke` does not rebuild it. A refusal is a fact about
                // the target rather than about this input, so the invocation is not run and the
                // target is reported instead. Its bound is the transport allowance for `prepare`,
                // never the invocation deadline: preparing imports the target's modules, which is
                // the harness's work rather than the target's, and a package with a deep module
                // graph costs more than a target's own call should. Holding it to the invocation
                // deadline killed a plugin on every first call into such a package, and the scan
                // reported those kills as `processExit` findings.
                if self.prepared.insert(job.target_id.clone()) {
                    match client.prepare(
                        &afz_protocol::PrepareReq {
                            target_id: job.target_id.clone(),
                            values: job.values.clone(),
                        },
                        PREPARE_TIMEOUT,
                    ) {
                        Call::Result(afz_protocol::PrepareResp::Ok { .. }) => {}
                        Call::Result(afz_protocol::PrepareResp::Skip { reason }) => {
                            self.prepared.remove(&job.target_id);
                            return (
                                Event::Unbuildable {
                                    invocation_id: job.invocation_id,
                                    unbuildable: Unbuildable {
                                        target_id: job.target_id,
                                        reason,
                                    },
                                },
                                Watch::Nothing,
                            );
                        }
                        Call::Refusal(error) => {
                            // The plugin declined this target and is still alive. Restarting it
                            // costs a process and a full `discover` to get the same answer back,
                            // so the process is kept and the target is left unprepared, to be
                            // tried again if the scan reaches it later.
                            self.prepared.remove(&job.target_id);
                            return (
                                Event::Ran(Completed {
                                    invocation_id: job.invocation_id,
                                    target_id: job.target_id.clone(),
                                    outcome: engine_fault(
                                        "plugin-refused",
                                        "PluginRefused",
                                        format!(
                                            "prepare declined: {}: {}",
                                            error.code, error.message
                                        ),
                                    ),
                                    restart: None,
                                }),
                                Watch::Refused(job.target_id),
                            );
                        }
                        Call::Lost(loss) => {
                            return self.classify_loss(
                                job,
                                Loss::Spawn {
                                    detail: format!("prepare: {loss}"),
                                },
                            )
                        }
                    }
                }
                // The configured deadline is the plugin's budget, and the engine's wait is derived
                // from it by `Timeouts::host_wait`, so a harness that cuts its own call answers
                // before this wait ends and the kill is left for the plugin that will not stop.
                match client.invoke(
                    &Completed::request_for(&job, deadline),
                    Timeouts::host_wait(deadline),
                ) {
                    Call::Result(outcome) => Attempt::Answered(outcome),
                    Call::Refusal(error) => Attempt::Refused(error),
                    Call::Lost(loss) => Attempt::Lost(loss),
                }
            }
        };

        match attempt {
            Attempt::Answered(outcome) => {
                let target_id = job.target_id;
                (
                    Event::Ran(Completed {
                        invocation_id: job.invocation_id,
                        target_id: target_id.clone(),
                        outcome,
                        restart: None,
                    }),
                    Watch::Answered(target_id),
                )
            }
            Attempt::Lost(loss) => self.classify_loss(job, loss),
            // The plugin answered, and what it said was that it cannot do this work. The process
            // is healthy, so it is kept: restarting it would throw away a warm process to get the
            // same refusal back.
            Attempt::Refused(error) => {
                let target_id = job.target_id;
                (
                    Event::Ran(Completed {
                        invocation_id: job.invocation_id,
                        target_id: target_id.clone(),
                        outcome: engine_fault(
                            "plugin-refused",
                            "PluginRefused",
                            format!("{}: {}", error.code, error.message),
                        ),
                        restart: None,
                    }),
                    Watch::Refused(target_id),
                )
            }
            // The process is gone or was never startable. That is a process exit as far as the
            // scan is concerned, and the next invocation gets a fresh process.
            Attempt::Absent => self.classify_loss(
                job,
                Loss::Spawn {
                    detail: "plugin is not running".to_string(),
                },
            ),
        }
    }

    /// Turn a call that got no answer into the finding the design names and restart the process.
    /// Accounting the restart against the target is the caller's, because the watchdog belongs to
    /// the pool and only the caller can hold it while the plugin call is in flight.
    fn classify_loss(&mut self, job: Job, reason: Loss) -> (Event, Watch) {
        let outcome = match &reason {
            // The engine waited for the answer until the deadline and none arrived.
            Loss::NoAnswer => engine_fault("deadline", "Deadline", reason.to_string()),
            // The plugin ended itself with a call in flight and left no explanation.
            Loss::Exited { .. } => engine_fault("processExit", "ProcessExit", reason.to_string()),
            other => engine_fault(via(other), "PluginLost", other.to_string()),
        };
        self.restart();
        let target_id = job.target_id;
        (
            Event::Ran(Completed {
                invocation_id: job.invocation_id,
                target_id: target_id.clone(),
                outcome,
                restart: Some(Restart {
                    worker: self.index,
                    reason,
                }),
            }),
            Watch::Restarted(target_id),
        )
    }
}

/// What one attempt tells the pool's watchdog.
enum Watch {
    /// The target answered, so its losing streak is over.
    Answered(String),
    /// The process was lost and restarted while this target was running.
    Restarted(String),
    /// The plugin declined the work while this target was running. No restart happened, but the
    /// target still did not run.
    Refused(String),
    /// Nothing happened that the watchdog counts.
    Nothing,
}

/// What one plugin process did with an attempt, before the engine classifies it.
enum Attempt {
    Answered(Outcome),
    /// The plugin said it cannot do the work. The process is alive, so this is not a loss.
    Refused(ErrorObject),
    Lost(Loss),
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
        fault: FaultSignature {
            class: class.to_string(),
            message: detail,
            frames: Vec::new(),
        },
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
        assert!(
            !watchdog.is_disabled("t"),
            "the streak never reached three in a row"
        );
    }
}

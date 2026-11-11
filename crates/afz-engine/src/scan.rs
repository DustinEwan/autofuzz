//! The scan: discovery, generation, scheduling, supervision, oracles, minimization, writing.
//!
//! One pass of the loop is one batch of invocations. The batch is planned from the corpus and the
//! target list, run by [`crate::supervise::Supervisor`] in parallel plugin processes, and fed back
//! into the corpus before the next batch is planned. That feedback is the whole design: an input
//! that reached a new edge is kept, and the next batch mutates it.
//!
//! The loop accounts for every invocation it plans. `execution.unattempted` is the budget minus
//! what ran, so a scan that disabled targets or ran out of wall clock says so in its own numbers
//! instead of reporting a smaller run as a clean one.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use afz_protocol::{
    Capability, CoverageMap, DiscoverReq, FaultSignature, Handshake, InvokeReq, Materialization, Outcome, PrepareReq,
    TargetDescriptor, ValueTree,
};

use crate::config::{Block, ConfigError, LoadedConfig, Strategy};
use crate::corpus::{self, Corpus, CoverageDigest, Recorded};
use crate::finding::{self, Seen};
use crate::generate::generate_arguments;
use crate::minimize::{self, Minimized};
use crate::mutate::mutate_with_donor;
use crate::oracles::{self, Finding, Oracles};
use crate::report::repro::{self, ReproTarget};
use crate::report::render_markdown;
use crate::report::scanresult::*;
use crate::rng::Rng;
use crate::rpc::client::{Launch, LostReason, PluginClient};
use crate::schedule::{Plan, Scheduler};
use crate::supervise::{Completed, DISABLE_AFTER, DISCOVERY_TIMEOUT, Job, RunReport, Supervisor};

/// Invocations between coverage samples. The verification list samples at this spacing.
pub const SAMPLE_EVERY: u64 = 5_000;
/// Identities a project has already reported, under the output directory.
pub const SEEN_FILE: &str = "seen.json";
/// Jobs handed to the supervisor per worker at a time. A batch that is too large delays the
/// feedback from the first invocation of the batch; one that is too small pays the process
/// bookkeeping too often.
const BATCH_PER_WORKER: usize = 8;
/// Shrink attempts per finding, so one finding cannot spend the whole scan shrinking itself.
const MINIMIZE_ATTEMPTS: u64 = 2_000;
/// Wall clock one finding may spend being shrunk. An attempt against a deadline fault costs the
/// whole deadline, so the attempt count alone would let one slow finding hold the finish phase
/// for the better part of an hour.
const MINIMIZE_BUDGET: Duration = Duration::from_secs(300);
/// How long one minimization call may take over the wire. The plugin enforces the invocation
/// deadline; this is the transport bound on top of it.
const MINIMIZE_SLACK: Duration = Duration::from_secs(10);
/// How long one `coverageMap` call may take. The plugin answers from the table it already holds,
/// so this only has to cover writing a few thousand entries over a pipe; it is asked after every
/// batch, and a worker that cannot answer would cost this much per batch.
const COVERAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one `prepare` may take: the plugin imports the target module here.
const PREPARE_TIMEOUT: Duration = Duration::from_secs(120);
/// Planning gives up after this many consecutive draws whose target is no longer usable, so a
/// corpus full of disabled targets ends the run instead of spinning.
const MAX_PLAN_SKIPS: usize = 64;

/// Everything the loop needs. The scan project supplies the defaults; the command line overrides
/// the fields a user would change between runs.
#[derive(Debug, Clone)]
pub struct Options {
    pub config: LoadedConfig,
    /// The directory discovery walks. The scan project's `codebase.root`, or the path the user
    /// named on the command line.
    pub root: PathBuf,
    pub seed: u64,
    pub strategy: Strategy,
    pub invocations: u64,
    pub wall_clock: Duration,
    pub workers: usize,
    pub max_corpus: u64,
    /// A previous scan directory: its corpus is loaded and its identities are suppressed.
    pub resume: Option<PathBuf>,
    pub out_dir: PathBuf,
    pub renderers: Vec<String>,
    /// Set from a signal handler to end the run early. The scan result is still written.
    pub stop: Arc<AtomicBool>,
}

impl Options {
    /// Defaults straight from a validated scan project.
    pub fn new(config: LoadedConfig, root: PathBuf) -> Options {
        let budget = &config.project.budget;
        Options {
            root,
            invocations: budget.invocations,
            wall_clock: Duration::from_millis(budget.wall_clock_ms),
            workers: budget.workers_or_default() as usize,
            max_corpus: u64::from(budget.max_corpus),
            strategy: budget.strategy,
            config,
            seed: 0,
            resume: None,
            out_dir: PathBuf::from(".autofuzz"),
            renderers: vec!["json".to_string(), "markdown".to_string()],
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// One coverage sample, reported as the run goes and returned in [`ScanReport::samples`].
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub invocations: u64,
    pub coverage_edges: u64,
    /// Reachable edges, as the harness reported them.
    pub coverage_total: u64,
    /// Covered edges over the edges the harness says are reachable. Zero when the harness reports
    /// no coverage at all.
    pub coverage_ratio: f64,
    pub findings: usize,
}

/// What a finished scan wrote.
#[derive(Debug, Clone)]
pub struct ScanReport {
    /// `<output.dir>/<scanId>`.
    pub dir: PathBuf,
    pub document: ScanDocument,
    pub samples: Vec<Progress>,
    /// The rendered markdown, when `output.renderers` asks for it.
    pub markdown: Option<String>,
}

#[derive(Debug)]
pub enum ScanError {
    Config(ConfigError),
    Plugin(LostReason),
    Io(io::Error),
    /// Discovery reported nothing the engine can call. A scan of zero targets is a configuration
    /// mistake more often than it is a fact about the code, so it is reported rather than run.
    NothingFuzzable { skipped: usize },
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::Config(e) => write!(f, "{e}"),
            ScanError::Plugin(e) => write!(f, "plugin: {}", e.detail()),
            ScanError::Io(e) => write!(f, "{e}"),
            ScanError::NothingFuzzable { skipped } => write!(
                f,
                "discovery reported no fuzzable target ({skipped} symbol(s) skipped); \
                 the scan project's `include` may not match the code"
            ),
        }
    }
}

impl std::error::Error for ScanError {}

impl From<ConfigError> for ScanError {
    fn from(e: ConfigError) -> Self {
        ScanError::Config(e)
    }
}

impl From<LostReason> for ScanError {
    fn from(e: LostReason) -> Self {
        ScanError::Plugin(e)
    }
}

impl From<io::Error> for ScanError {
    fn from(e: io::Error) -> Self {
        ScanError::Io(e)
    }
}

/// Run one scan and write its result. `on_progress` is called at every [`SAMPLE_EVERY`]
/// invocations, which is how a caller watches coverage without reading the output directory while
/// the run is still going.
pub fn run(options: &Options, on_progress: &mut impl FnMut(&Progress)) -> Result<ScanReport, ScanError> {
    let launch = options.config.launch_for(Block::Codebase)?;
    let codebase = options
        .config
        .project
        .codebase
        .as_ref()
        .ok_or_else(|| ConfigError::msg("this scan project has no `codebase` block"))?;
    let request = DiscoverReq {
        root: options.root.display().to_string(),
        include: codebase.include.clone(),
        exclude: codebase.exclude.clone(),
        config_path: Some(options.config.path.display().to_string()),
    };

    let started_at = SystemTime::now();
    let probe = PluginClient::spawn(&launch)?;
    let handshake = probe.handshake()?;
    let discovery = probe.discover(&request, DISCOVERY_TIMEOUT)?;
    let mut scanner = Scanner::open(options, launch, handshake, discovery, request, probe)?;
    scanner.loop_until_budget(on_progress)?;
    scanner.finish(started_at)
}

/// The loop's state, so the helpers below take arguments instead of reaching into a caller.
struct Scanner {
    options: Options,
    handshake: Handshake,
    discovery: afz_protocol::Discovery,
    targets: Vec<TargetDescriptor>,
    oracles: Oracles,
    scheduler: Scheduler,
    corpus: Corpus,
    seen: Seen,
    rng: Rng,
    minimizer: Minimizer,
    /// The plugin processes that run invocations. They stay warm between batches, and only they
    /// can answer `coverageMap`: a process learns a module's blocks by running it.
    supervisor: Supervisor,
    /// Ids of targets this scan has been given at least one invocation.
    attempted: BTreeSet<String>,
    planned: u64,
    ran: u64,
    restarts: u64,
    deadline_exceeded: u64,
    /// Invocations whose reported duration reached `deadline`. See `Execution::deadline_cuts`.
    deadline_cuts: u64,
    /// The deadline every invocation is asked to finish inside, taken from the `deadline` oracle.
    deadline: Duration,
    disabled: BTreeSet<String>,
    log: Vec<String>,
    records: Vec<(FindingRecord, FindingFiles)>,
    by_identity: BTreeMap<String, usize>,
    samples: Vec<Progress>,
    next_sample: u64,
    coverage_total: u64,
    coverage_map: Option<CoverageMap>,
    stopped: StoppedReason,
}

/// The plugin connection minimization runs against, with the targets it has prepared.
struct Minimizer {
    client: PluginClient,
    prepared: BTreeSet<String>,
    deadline: Duration,
}

impl Minimizer {
    fn run(&mut self, target_id: &str, values: &[ValueTree]) -> Outcome {
        if self.client.lost().is_some() {
            // A shrink step that killed the plugin is a candidate like any other: the next call
            // starts a fresh process, and the candidate is kept only if the fault comes back.
            match PluginClient::spawn(self.launch()) {
                Ok(fresh) => self.client = fresh,
                Err(_) => return Outcome::plugin_lost(0.0, "the minimizer plugin could not be restarted"),
            }
            self.prepared.clear();
        }
        if !self.prepared.contains(target_id) {
            let prepare = PrepareReq { target_id: target_id.to_string(), values: values.to_vec() };
            if self.client.prepare(&prepare, PREPARE_TIMEOUT).is_ok() {
                self.prepared.insert(target_id.to_string());
            }
        }
        let request = InvokeReq {
            invocation_id: 0,
            target_id: target_id.to_string(),
            values: values.to_vec(),
            deadline_ms: self.deadline.as_millis() as u64,
            collect_coverage: false,
        };
        self.client.invoke_or_lose(&request, self.deadline + MINIMIZE_SLACK)
    }

    fn launch(&self) -> &Launch {
        self.client.launch()
    }
}

impl Scanner {
    fn open(
        options: &Options,
        launch: Launch,
        handshake: Handshake,
        discovery: afz_protocol::Discovery,
        discovery_request: DiscoverReq,
        probe: PluginClient,
    ) -> Result<Scanner, ScanError> {
        let mut targets = discovery.targets.clone();
        targets.sort_by(|a, b| a.id.cmp(&b.id));
        if targets.is_empty() {
            return Err(ScanError::NothingFuzzable { skipped: discovery.skipped.len() });
        }
        let oracles = Oracles::new(&options.config.project.oracles);
        let deadline = oracles
            .invocation_deadline()
            .unwrap_or_else(|| Duration::from_millis(oracles::DEFAULT_DEADLINE_MS));
        let seen = Seen::open(&options.out_dir.join(SEEN_FILE))?;
        let scheduler = match options.strategy {
            Strategy::Guided => Scheduler::guided(),
            Strategy::Random => Scheduler::random(),
        };
        let supervisor = Supervisor::new(launch, options.workers, deadline, DISABLE_AFTER, discovery_request);
        let mut scanner = Scanner {
            options: options.clone(),
            handshake,
            discovery,
            targets,
            oracles,
            scheduler,
            corpus: Corpus::new(options.max_corpus),
            seen,
            rng: Rng::seeded_u64(options.seed),
            minimizer: Minimizer { client: probe, prepared: BTreeSet::new(), deadline },
            supervisor,
            attempted: BTreeSet::new(),
            planned: 0,
            ran: 0,
            restarts: 0,
            deadline_exceeded: 0,
            deadline_cuts: 0,
            deadline,
            disabled: BTreeSet::new(),
            log: Vec::new(),
            records: Vec::new(),
            by_identity: BTreeMap::new(),
            samples: Vec::new(),
            next_sample: SAMPLE_EVERY,
            coverage_total: 0,
            coverage_map: None,
            stopped: StoppedReason::Complete,
        };
        scanner.log.push(format!(
            "harness {} {} ({}), strategy {:?}, seed 0x{:08x}",
            scanner.handshake.name,
            scanner.handshake.version,
            scanner.handshake.target_class.as_str(),
            scanner.options.strategy,
            scanner.options.seed
        ));
        scanner.load_resume()?;
        Ok(scanner)
    }

    /// A resumed scan starts with the previous corpus and the previous findings' identities.
    ///
    /// A finding that comes back is reported again, marked `seenBefore`: a report that hid a bug
    /// still present in the tree would be worse than one that repeats itself.
    fn load_resume(&mut self) -> Result<(), ScanError> {
        let Some(dir) = self.options.resume.clone() else { return Ok(()) };
        let entries = read_corpus(&dir)?;
        let mut loaded = 0usize;
        for entry in entries {
            let edges: CoverageDigest = entry.edges.into_iter().collect();
            if matches!(self.corpus.record(&entry.target_id, &entry.values, &edges, 0), Recorded::Retained { .. }) {
                loaded += 1;
            }
        }
        let document = read(&dir.join(SCAN_FILE))?;
        let suppressed = document.findings.iter().filter(|f| self.seen.record(&f.identity)).count();
        self.log.push(format!(
            "resumed {}: {loaded} corpus entr(ies) reloaded, {suppressed} identity/ies suppressed",
            dir.display()
        ));
        Ok(())
    }

    /// Plan, run, and learn from batches until a budget limit or the stop flag ends the run.
    fn loop_until_budget(&mut self, on_progress: &mut impl FnMut(&Progress)) -> Result<(), ScanError> {
        let clock = Instant::now();
        loop {
            if self.options.stop.load(Ordering::Relaxed) {
                self.stopped = StoppedReason::Interrupted;
                self.log.push("stopped on request".to_string());
                break;
            }
            let elapsed = clock.elapsed();
            if elapsed >= self.options.wall_clock {
                self.stopped = StoppedReason::Budget;
                self.log.push(format!("wall clock budget of {} ms reached", self.options.wall_clock.as_millis()));
                break;
            }
            if self.planned >= self.options.invocations {
                self.stopped = StoppedReason::Budget;
                break;
            }
            let jobs = self.plan_batch();
            if jobs.is_empty() {
                // Nothing left to try: every target is disabled or proved unbuildable.
                self.log.push("no target left to invoke".to_string());
                break;
            }
            let values_by_invocation: HashMap<u64, Vec<ValueTree>> =
                jobs.iter().map(|job| (job.invocation_id, job.values.clone())).collect();
            let remaining = self.options.wall_clock.saturating_sub(elapsed);
            let report = self.supervisor.run(jobs, remaining);
            self.account(report, &values_by_invocation, on_progress);
        }
        Ok(())
    }

    /// Build the next batch. Every fuzzable target gets its first attempt before any target gets
    /// a second, which is what makes a small budget still tell the reader about every entry point.
    fn plan_batch(&mut self) -> Vec<Job> {
        let room = self.options.invocations - self.planned;
        let want = room.min((self.options.workers.max(1) * BATCH_PER_WORKER) as u64) as usize;
        let collect_coverage = self.handshake.capabilities.contains(&Capability::Coverage);
        let mut jobs = Vec::with_capacity(want);
        let mut skips = 0usize;
        while jobs.len() < want {
            let invocation = self.planned + 1;
            let selection = self.scheduler.pick(&self.corpus, &mut self.rng, invocation);
            let (target_id, values) = match selection.resolve(&self.corpus) {
                Plan::Fresh => match self.next_target_index() {
                    Some(index) => {
                        let target_id = self.targets[index].id.clone();
                        let values = generate_arguments(&self.targets[index].parameters, &mut self.rng);
                        (target_id, values)
                    }
                    None => break,
                },
                Plan::Mutate { entry_id, values } => {
                    let Some(entry) = self.corpus.get(&entry_id).cloned() else { continue };
                    self.corpus.note_use(&entry_id, invocation);
                    let donor = self.donor(&entry.target_id, &entry_id);
                    let mutated = self.mutate_arguments(&entry.target_id, &values, donor.as_ref());
                    (entry.target_id, mutated)
                }
            };
            if self.disabled.contains(&target_id) {
                skips += 1;
                if skips >= MAX_PLAN_SKIPS {
                    self.log.push(format!("stopped planning after {skips} draws onto disabled targets"));
                    break;
                }
                continue;
            }
            self.attempted.insert(target_id.clone());
            self.planned += 1;
            jobs.push(Job { invocation_id: invocation, target_id, values, collect_coverage });
        }
        jobs
    }

    /// The next target for a freshly generated input: an untouched one first, then any target the
    /// scan has not disabled. An index, so the caller can read the target and draw from the
    /// generator's random state without holding one borrow across the other.
    fn next_target_index(&mut self) -> Option<usize> {
        if let Some(index) = self
            .targets
            .iter()
            .position(|t| !self.attempted.contains(&t.id) && !self.disabled.contains(&t.id))
        {
            return Some(index);
        }
        let live: Vec<usize> = self
            .targets
            .iter()
            .enumerate()
            .filter(|(_, t)| !self.disabled.contains(&t.id))
            .map(|(index, _)| index)
            .collect();
        self.rng.pick(&live).copied()
    }

    /// Another retained input for the same target, to donate a subtree during mutation.
    fn donor(&mut self, target_id: &str, except: &str) -> Option<Vec<ValueTree>> {
        let ids: Vec<String> = self
            .corpus
            .entries()
            .filter(|e| e.target_id == target_id && e.id != except)
            .map(|e| e.id.clone())
            .collect();
        let id = self.rng.pick(&ids)?.clone();
        self.corpus.get(&id).map(|e| e.values.clone())
    }

    /// Mutate one argument of a retained input, leaving the others alone. Mutating every argument
    /// at once loses whatever combination the retained input had found.
    fn mutate_arguments(&mut self, target_id: &str, values: &[ValueTree], donor: Option<&Vec<ValueTree>>) -> Vec<ValueTree> {
        let Some(position) = self.targets.iter().position(|t| t.id == target_id) else { return values.to_vec() };
        if values.is_empty() || self.targets[position].parameters.is_empty() {
            return values.to_vec();
        }
        let index = self.rng.below(values.len() as u64) as usize;
        let Some(parameter) = self.targets[position].parameters.get(index) else { return values.to_vec() };
        let donor_value = donor.and_then(|d| d.get(index));
        let mut next = values.to_vec();
        let mutated = mutate_with_donor(&next[index], &parameter.ty, &mut self.rng, donor_value, target_id);
        next[index] = mutated;
        next
    }

    /// Feed one batch back: coverage into the corpus, outcomes into the oracles, and the accounting.
    fn account(
        &mut self,
        report: RunReport,
        values: &HashMap<u64, Vec<ValueTree>>,
        on_progress: &mut impl FnMut(&Progress),
    ) {
        self.planned += report.unattempted.len() as u64;
        self.restarts += report.restarts;
        for id in &report.disabled {
            if self.disabled.insert(id.clone()) {
                self.log.push(format!("disabled {} after repeated plugin losses", id));
            }
        }
        for unbuildable in &report.unbuildable {
            self.log.push(format!("{} cannot be built: {}", unbuildable.target_id, unbuildable.reason.as_str()));
        }
        for completed in &report.completed {
            if let Some(restart) = &completed.restart {
                self.log.push(format!(
                    "invocation {} lost worker {}: {}",
                    completed.invocation_id, restart.worker, restart.reason.detail()
                ));
            }
            self.ran += 1;
            let input = values.get(&completed.invocation_id).cloned().unwrap_or_default();
            self.learn(completed, &input);
            if completed.invocation_id >= self.next_sample {
                while self.next_sample <= completed.invocation_id {
                    self.next_sample += SAMPLE_EVERY;
                }
                let sample = self.progress(completed.invocation_id);
                self.samples.push(sample.clone());
                on_progress(&sample);
            }
        }
        // Reachable edges come from the processes that have run targets, and each worker has
        // loaded only the modules of the targets it invoked, so the pool's union is re-read after
        // every batch and only grows. `finish` restates each sample against the final count.
        self.refresh_coverage();
    }

    fn learn(&mut self, completed: &Completed, values: &[ValueTree]) {
        let edges: CoverageDigest = match completed.outcome.coverage() {
            Some(edges) => edges.iter().copied().collect(),
            // The outcome tuple stands in for edges only where the harness has no coverage to give.
            // A coverage harness can also answer with no digest - a plugin process that died
            // mid-call never reached the target, and there is nothing to learn from that - and
            // filing the tuple as edges would put ids the coverage map cannot name into
            // `coverageEdges`, which is reported as a fraction of that map.
            None if !self.handshake.capabilities.contains(&Capability::Coverage) => corpus::outcome_digest(&completed.outcome),
            None => CoverageDigest::new(),
        };
        if completed.outcome.via() == Some(oracles::DEADLINE_VIA) {
            self.deadline_exceeded += 1;
        }
        if completed.outcome.duration_ms() >= self.deadline.as_millis() as f64 {
            self.deadline_cuts += 1;
        }
        self.corpus.record(&completed.target_id, values, &edges, completed.invocation_id);
        for finding in self.oracles.evaluate(&completed.target_id, completed.invocation_id, &completed.outcome) {
            self.record_finding(finding, values);
        }
    }

    /// One finding: deduplicate on identity, then minimize and emit a reproducer for a new one.
    fn record_finding(&mut self, finding: Finding, values: &[ValueTree]) {
        let fault = finding.fault.clone().unwrap_or_else(|| FaultSignature {
            // A deadline or memory finding has no fault of its own; the oracle and its sentence
            // are what distinguishes it, so they become the class and message identity reads.
            class: finding.oracle.as_str().to_string(),
            message: finding.summary.clone(),
            frames: vec![],
        });
        let identity = finding::identity(&finding.target_id, &fault);
        if self.by_identity.contains_key(&identity) {
            // The same fault again in this run: one finding, one reproducer.
            return;
        }
        // `seen_before` is a statement about earlier scans, not about this one: the identity was
        // already in the history this scan opened. Such a finding is listed so the reader sees
        // that the fault is still being hit, and is not shrunk again, because an earlier scan
        // already did that work and wrote it up.
        let seen_before = !self.seen.record(&identity);
        let id = finding::finding_id(&identity);
        let minimized = (!seen_before).then(|| self.shrink(&finding.target_id, values));
        let target = self.target(&finding.target_id);
        let reproducer = minimized.as_ref().filter(|m| m.reproduces).map(|m| {
            let script_target = ReproTarget {
                target_id: &finding.target_id,
                file: target.and_then(|t| t.origin.file.as_deref()).unwrap_or(""),
                symbol: target.map(|t| t.name.as_str()).unwrap_or(""),
            };
            repro::reproducer(&script_target, &m.values)
        });
        let shrunk = minimized.as_ref().filter(|m| m.reproduces && m.bytes_after < m.bytes_before);
        let record = FindingRecord {
            finding_id: id.clone(),
            identity: identity.clone(),
            target_id: finding.target_id.clone(),
            oracle: finding.oracle.as_str().to_string(),
            severity: finding.severity,
            via: finding.via.clone(),
            fault: self.scan_fault(&fault),
            input: InputArtifact { encoding: InputEncoding::ValueTreeJson, artifact: input_artifact_path(&id) },
            minimized_input: shrunk.map(|m| MinimizedArtifact {
                artifact: minimized_artifact_path(&id),
                bytes_before: m.bytes_before,
                bytes_after: m.bytes_after,
            }),
            reproducer: reproducer.as_ref().map(|_| Artifact { artifact: reproducer_artifact_path(&id) }),
            first_seen_invocation: finding.invocation_id,
            reproduced: minimized.as_ref().is_some_and(|m| m.reproduces),
            seen_before,
        };
        self.log.push(match &minimized {
            Some(m) => format!(
                "finding {id} at invocation {}: {} {} ({} bytes, {} attempts, reproduced {})",
                finding.invocation_id,
                finding.oracle.as_str(),
                fault.message,
                m.bytes_after,
                m.attempts,
                m.reproduces
            ),
            None => format!(
                "finding {id} at invocation {}: {} {} (an earlier scan reported this identity, so it is not shrunk again)",
                finding.invocation_id,
                finding.oracle.as_str(),
                fault.message
            ),
        });
        self.by_identity.insert(identity, self.records.len());
        self.records.push((
            record,
            FindingFiles {
                finding_id: id,
                input: values.to_vec(),
                minimized: shrunk.map(|m| MinimizedInput {
                    values: m.values.clone(),
                    bytes_before: m.bytes_before,
                    bytes_after: m.bytes_after,
                }),
                reproducer,
            },
        ));
    }

    /// Shrink a finding's input against the same fault. The minimizer runs on its own plugin
    /// process, so shrinking never displaces the guided workers.
    fn shrink(&mut self, target_id: &str, values: &[ValueTree]) -> Minimized {
        minimize::minimize(values, MINIMIZE_ATTEMPTS, MINIMIZE_BUDGET, &mut |candidate| self.minimizer.run(target_id, candidate))
    }

    fn scan_fault(&self, fault: &FaultSignature) -> ScanFault {
        ScanFault {
            class: fault.class.clone(),
            message: fault.message.clone(),
            frames: fault
                .frames
                .iter()
                .map(|frame| ScanFrame {
                    file: relativize(&self.options.root.display().to_string(), &frame.file),
                    line: frame.line,
                    column: frame.column,
                    fn_name: frame.fn_name.clone(),
                })
                .collect(),
        }
    }

    fn target(&self, target_id: &str) -> Option<&TargetDescriptor> {
        self.targets.iter().find(|t| t.id == target_id)
    }

    fn progress(&self, invocations: u64) -> Progress {
        Progress {
            invocations,
            coverage_edges: self.corpus.covered_count(),
            coverage_total: self.coverage_total,
            coverage_ratio: self.ratio(),
            findings: self.records.len(),
        }
    }

    /// Covered edges over the reachable count the harness reported. Zero when the harness has
    /// reported nothing, so a run with no coverage says so instead of claiming a full one.
    fn ratio_of(covered: u64, total: u64) -> f64 {
        if total == 0 {
            return 0.0;
        }
        covered as f64 / total as f64
    }

    fn ratio(&self) -> f64 {
        Self::ratio_of(self.corpus.covered_count(), self.coverage_total)
    }

    /// The pool's reachable-edge table. Only a process that has run an invocation knows a
    /// module's blocks, so the answer comes from the workers; a harness without the `coverage`
    /// capability is not asked.
    fn refresh_coverage(&mut self) {
        if !self.handshake.capabilities.contains(&Capability::Coverage) {
            return;
        }
        if let Some(map) = self.supervisor.coverage_map(COVERAGE_TIMEOUT) {
            self.coverage_total = map.entries.len() as u64;
            self.coverage_map = Some(map);
        }
    }

    /// Write the document, the files it names, and the identities this scan reported.
    fn finish(&mut self, started_at: SystemTime) -> Result<ScanReport, ScanError> {
        self.refresh_coverage();
        // The reachable set is only complete now, so the samples taken during the run are restated
        // against it; each keeps the covered-edge count it had at the time it was taken.
        let total = self.coverage_total;
        for sample in &mut self.samples {
            sample.coverage_ratio = Self::ratio_of(sample.coverage_edges, total);
        }
        let finished_at = SystemTime::now();
        let document = self.document(started_at, finished_at);
        let coverage = self
            .coverage_map
            .as_ref()
            .map(|map| serde_json::to_string_pretty(map).expect("a coverage map is serializable"));
        let log = self.log.join("\n") + "\n";
        let corpus_files: Vec<CorpusFile> = self
            .corpus
            .entries()
            .map(|entry| CorpusFile {
                name: entry.id.clone(),
                target_id: entry.target_id.clone(),
                values: entry.values.clone(),
                edges: entry.edges.iter().copied().collect(),
            })
            .collect();
        let files = ScanFiles {
            findings: self.records.iter().map(|(_, files)| files.clone()).collect(),
            corpus: corpus_files,
            coverage: coverage.as_deref(),
            log: Some(&log),
        };
        let dir = write(&self.options.out_dir, &document, &files)?;
        let markdown = if self.options.renderers.iter().any(|r| r == "markdown") {
            let text = render_markdown::render(&document);
            std::fs::write(dir.join("report.md"), &text)?;
            Some(text)
        } else {
            None
        };
        self.seen.save()?;
        Ok(ScanReport { dir, document, samples: self.samples.clone(), markdown })
    }

    fn document(&self, started_at: SystemTime, finished_at: SystemTime) -> ScanDocument {
        let mut counts: BTreeMap<Materialization, u64> = BTreeMap::new();
        for target in &self.targets {
            *counts.entry(target.materialization).or_insert(0) += 1;
        }
        for skipped in &self.discovery.skipped {
            if let Some(materialization) = skipped.materialization {
                *counts.entry(materialization).or_insert(0) += 1;
            }
        }
        let root = self.options.root.display().to_string();
        ScanDocument {
            schema_version: ScanSchemaVersion::AfzScan1,
            scan_id: scan_id(started_at),
            started_at: rfc3339(started_at),
            finished_at: rfc3339(finished_at),
            engine: EngineInfo {
                version: env!("CARGO_PKG_VERSION").to_string(),
                seed: format!("0x{:08x}", self.options.seed),
                config_hash: self.options.config.config_hash().unwrap_or_else(|_| "unavailable".to_string()),
            },
            target: TargetInfo {
                root: root.clone(),
                revision: git_revision(&root),
                harness: HarnessInfo {
                    name: self.handshake.name.clone(),
                    version: self.handshake.version.clone(),
                    target_class: self.handshake.target_class,
                    capabilities: self.handshake.capabilities.clone(),
                },
            },
            inventory: Inventory {
                discovered: (self.targets.len() + self.discovery.skipped.len()) as u64,
                fuzzable: self.targets.len() as u64,
                skipped: self.discovery.skipped.clone(),
                materialization_counts: counts,
            },
            execution: Execution {
                invocations: self.ran,
                corpus_size: self.corpus.len() as u64,
                coverage_edges: self.corpus.covered_count(),
                coverage_ratio: self.ratio(),
                deadline_exceeded: self.deadline_exceeded,
                deadline_cuts: self.deadline_cuts,
                stopped_reason: self.stopped,
                restarts: self.restarts,
                unattempted: self.options.invocations.saturating_sub(self.ran),
                disabled_targets: self.disabled.iter().cloned().collect(),
            },
            findings: self.records.iter().map(|(record, _)| record.clone()).collect(),
            artifacts: Artifacts {
                corpus: format!("{CORPUS_DIR}/"),
                coverage: self.coverage_map.as_ref().map(|_| "coverage.json".to_string()),
                log: Some("engine.log".to_string()),
            },
        }
    }
}

/// `git:<short sha>` when the scan root is in a repository, `None` when it is not.
fn git_revision(root: &str) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(root).args(["rev-parse", "--short", "HEAD"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if sha.is_empty() {
        None
    } else {
        Some(format!("git:{sha}"))
    }
}

/// UTC `YYYY-MM-DDThh:mm:ssZ`, the format Part I's example shows. Written here so the engine
/// carries no date library for one string.
fn rfc3339(at: SystemTime) -> String {
    let Ok(since) = at.duration_since(UNIX_EPOCH) else { return "1970-01-01T00:00:00Z".to_string() };
    let secs = since.as_secs();
    let days = (secs / 86_400) as i64;
    let time_of_day = secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60
    )
}

/// Days since the epoch to a civil date, Howard Hinnant's algorithm, which is defined for the
/// whole range a `SystemTime` can hold.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

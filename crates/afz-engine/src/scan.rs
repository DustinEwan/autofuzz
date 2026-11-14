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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use afz_protocol::{
    Capability, CoverageMap, DiscoverReq, FaultSignature, Handshake, InvokeReq, Materialization, Outcome, PrepareReq,
    ReproducerReq, TargetDescriptor, ValueTree,
};

use crate::config::{ConfigError, LoadedConfig, Strategy};
use crate::corpus::{self, Corpus, CoverageDigest, Recorded};
use crate::finding::{self, Seen};
use crate::generate::generate_arguments;
use crate::minimize::{self, Minimized};
use crate::mutate::mutate_with_donor;
use crate::oracles::{self, Finding, Oracles};
use crate::report::render_markdown;
use crate::report::scanresult::*;
use crate::rng::Rng;
use crate::rpc::client::{spawn_discovered, Launch, LostReason, PluginClient};
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
/// Wall clock a scan may overrun its budget by, covering the write of the result. Minimization
/// stops at the budget plus this, summed across every finding, so a run reports the inputs that
/// failed at the time its operator expects a document instead of spending one more per-finding
/// allowance on each finding it found late.
const SHRINK_GRACE: Duration = Duration::from_secs(30);

/// How much wall clock one minimization may use: the smaller of the per-finding allowance and what
/// is left of the scan's budget plus the write grace. Zero means the finding is reported unshrunk.
fn shrink_budget(elapsed: Duration, wall_clock: Duration) -> Duration {
    (wall_clock + SHRINK_GRACE).saturating_sub(elapsed).min(MINIMIZE_BUDGET)
}

/// How long one minimization call may take over the wire. The plugin enforces the invocation
/// deadline; this is the transport bound on top of it.
const MINIMIZE_SLACK: Duration = Duration::from_secs(10);
/// How long one `coverageMap` call may take. The plugin answers from the table it already holds,
/// so this only has to cover writing a few thousand entries over a pipe; it is asked after every
/// batch, and a worker that cannot answer would cost this much per batch.
const COVERAGE_TIMEOUT: Duration = Duration::from_secs(10);
/// The `reproducer` call answers from string concatenation in the plugin, so a slow answer is
/// a stuck plugin, not work.
const REPRODUCER_TIMEOUT: Duration = Duration::from_secs(10);
/// The `ready` probe is a bounded network dialogue the plugin runs on the engine's
/// behalf; a plugin that cannot answer within this is a stuck plugin.
const READY_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest script a plugin may hand back. A reproducer is a dozen lines; a megabyte is a bug.
const MAX_REPRO_SCRIPT_BYTES: usize = 1 << 20;

/// Whether a plugin-named extension is a safe file suffix: short and alphanumeric, so the
/// artifact cannot name a path outside `repros/`.
fn valid_extension(extension: &str) -> bool {
    !extension.is_empty() && extension.len() <= 8 && extension.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// How long one `prepare` may take: the plugin imports the target module here, so the cost is the
/// target's module graph rather than one call. Both the guided workers and the shrink pool use this
/// bound. The invocation deadline does not apply to `prepare`, because preparing is not the target
/// executing; holding it there killed a plugin on the first call into any package with a deep
/// module graph, and the scan reported those kills as `processExit` findings.
pub(crate) const PREPARE_TIMEOUT: Duration = Duration::from_secs(120);
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
    /// The git selector the scan is limited to. `None` scans the whole tree.
    pub scope: Option<crate::git::Selector>,
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
            scope: None,
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
    /// Reachable edges, as the harness reported them across the whole worker pool. A harness
    /// learns an edge only when the runtime reveals it, so this count grows as a scan reaches code
    /// it had not executed. The ratio is therefore comparable between two runs of the same seed and
    /// strategy, and the covered-edge count is the number to compare across strategies.
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
    /// A `--git` scope left no target. Its own variant, because the advice that belongs with
    /// `NothingFuzzable` - check your `include` - is the wrong advice here.
    EmptyScope { selector: String, changed: usize, discovered: usize },
    /// The harness declares `readiness` and answered `ready` with no. The scan aborts
    /// before any worker spawns, because every invocation would fail the way the probe
    /// did, and the harness's own reason names the fix.
    NotReady { detail: String },
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
            ScanError::EmptyScope { selector, changed, discovered } => write!(
                f,
                "git scope \"{selector}\" touches no fuzzable target: {changed} changed file(s), \
                 {discovered} target(s) discovered"
            ),
            ScanError::NotReady { detail } => write!(f, "the harness reports the target environment is not ready: {detail}"),
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

/// Reject a scan the engine cannot run, before any work starts, so a caller can report the reason
/// as its first line instead of after announcing a scan.
pub fn preflight(options: &Options) -> Result<(), ScanError> {
    // A scope is a statement about a repository, so a directory that is not one is refused here
    // rather than after the plugin has been spawned and a tree has been checked out.
    if let Some(selector) = &options.scope {
        if !crate::git::is_repo(&options.root) {
            return Err(ConfigError::msg(format!(
                "--git {} needs a git repository: {} is not one",
                crate::git::render(selector),
                options.root.display()
            ))
            .into());
        }
    }
    Ok(())
}

/// Run one scan and write its result. `on_progress` is called at every [`SAMPLE_EVERY`]
/// invocations, which is how a caller watches coverage without reading the output directory while
/// the run is still going.
pub fn run(options: &Options, on_progress: &mut impl FnMut(&Progress)) -> Result<ScanReport, ScanError> {
    preflight(options)?;
    // The scope decides which tree the scan runs against, so it is resolved before anything is
    // launched. `resolved` lives until the run ends: dropping it removes the materialized tree.
    let resolved = match &options.scope {
        Some(selector) => {
            Some(crate::git::resolve(&options.root, &options.out_dir, selector).map_err(|e| ConfigError::msg(e.0))?)
        }
        None => None,
    };
    let scoped = match &resolved {
        Some(resolved) if resolved.worktree.is_some() => Some(scoped_options(options, resolved)?),
        _ => None,
    };
    let options = scoped.as_ref().unwrap_or(options);
    let launch = options.config.launch()?;
    let harness = options.config.project.harness.as_ref().ok_or_else(|| {
        ConfigError::msg("a scan project must declare a `harness` block: it names the plugin that drives the scan")
    })?;
    let request = DiscoverReq {
        root: options.root.display().to_string(),
        include: harness.include.clone(),
        exclude: harness.exclude.clone(),
        config_path: Some(options.config.path.display().to_string()),
    };

    let started_at = SystemTime::now();
    let setup_at = Instant::now();
    let probe = PluginClient::spawn(&launch)?;
    let handshake = probe.handshake()?;
    let mut discovery = probe.discover(&request, DISCOVERY_TIMEOUT)?;
    // A harness that declares `readiness` vouches for the environment its targets live
    // in. Ask before any worker spawns: a service nobody started would otherwise file
    // one deadline finding per target, and the harness's reason is the fixable fact.
    if handshake.capabilities.contains(&Capability::Readiness) {
        let ready = probe.ready(READY_TIMEOUT)?;
        if !ready.ready {
            return Err(ScanError::NotReady {
                detail: ready.detail.unwrap_or_else(|| "the harness gave no reason".to_string()),
            });
        }
    }
    let mut notes = Vec::new();
    if let Some(resolved) = &resolved {
        let discovered = discovery.targets.len();
        discovery.targets.retain(|target| in_scope(&target.origin, &resolved.changed, &options.root));
        discovery.skipped.retain(|skipped| skipped_in_scope(skipped, &resolved.changed, &options.root));
        let kept = discovery.targets.len();
        notes.push(format!(
            "git scope \"{}\": {} changed file(s), {kept} of {discovered} target(s) in scope",
            resolved.selector,
            resolved.changed.len()
        ));
        if kept == 0 {
            return Err(ScanError::EmptyScope {
                selector: resolved.selector.clone(),
                changed: resolved.changed.len(),
                discovered,
            });
        }
        // Coverage ids are hashed from the file url, so a corpus carried into a materialized tree
        // reports every edge as new. The run is valid; its ratio is not comparable to the earlier
        // one, and a reader of the log needs to be told before they compare the two.
        if options.resume.is_some() && resolved.worktree.is_some() {
            notes.push("git scope + --resume: the resumed corpus's edge ids were measured in a different tree".to_string());
        }
    }
    let git = resolved.as_ref().map(|resolved| GitScope {
        selector: resolved.selector.clone(),
        base: resolved.base.clone(),
        tree: resolved.tree.clone(),
        changed_files: resolved.changed.len() as u64,
        worktree: resolved.worktree.as_ref().map(|worktree| worktree.path.display().to_string()),
    });
    let mut scanner = Scanner::open(options, launch, handshake, discovery, request, probe)?;
    scanner.git_scope = git;
    scanner.log.append(&mut notes);
    scanner.setup_ms = setup_at.elapsed().as_millis();
    scanner.loop_until_budget(on_progress)?;
    scanner.finish(started_at)
}

/// The options for a scan whose tree was materialized.
///
/// The invoked scan project is copied into that tree and loaded from there, which is what makes
/// `launch.cwd`, `AFZ_ROOT`, `AFZ_CONFIG`, `config_hash`, and the plugin's tsconfig-beside-config
/// lookup all resolve inside the tree being scanned. Without the copy, a scan project that is
/// uncommitted would not exist in the tree at all, and `AFZ_CONFIG` would point at the user's file
/// while `AFZ_ROOT` pointed at the scoped one. The output directory stays the invoked project's,
/// so results never land in the throwaway tree.
fn scoped_options(options: &Options, resolved: &crate::git::Resolved) -> Result<Options, ScanError> {
    let worktree = resolved.worktree.as_ref().expect("only a materialized tree reaches this");
    let name = options.config.path.file_name().ok_or_else(|| ConfigError::msg("the scan project path names no file"))?;
    let destination = match options.config.path.strip_prefix(&options.root) {
        Ok(relative) => worktree.path.join(relative),
        Err(_) => worktree.path.join(name),
    };
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(&options.config.path, &destination)?;
    let mut scoped = options.clone();
    scoped.root = resolved.root.clone();
    scoped.config = crate::config::load(&destination)?;
    Ok(scoped)
}

/// True when the target's source file is one the scope changed. A target with no file origin
/// cannot be placed in a change set, so it is out of scope.
fn in_scope(origin: &afz_protocol::Origin, changed: &BTreeSet<String>, root: &Path) -> bool {
    origin
        .file
        .as_deref()
        .or(origin.module.as_deref())
        .is_some_and(|file| touches(file, changed, root))
}

/// A skipped entry carries no origin, only the module its blocker sits in. An entry whose blocker
/// names no module is a package-level fact that no change set can place, so it is kept: dropping it
/// would understate `inventory.discovered`.
fn skipped_in_scope(skipped: &afz_protocol::SkippedTarget, changed: &BTreeSet<String>, root: &Path) -> bool {
    match skipped.blocked_by.as_ref().and_then(|blocker| blocker.module.as_deref()) {
        Some(module) => touches(module, changed, root),
        None => true,
    }
}

/// Whether `path` names a file the scope changed.
///
/// Both sides are compared as absolute posix strings: the caller absolutizes `root`, the plugin
/// builds its paths with `path.resolve(root, file)`, and the change set is root-relative, so the
/// two share one prefix. Canonicalizing is deliberately not done: it resolves symlinks and fails on
/// a path the change set deleted.
fn touches(path: &str, changed: &BTreeSet<String>, root: &Path) -> bool {
    let absolute = absolute_posix(root, path);
    changed.iter().any(|file| absolute_posix(root, file) == absolute)
}

fn absolute_posix(root: &Path, path: &str) -> String {
    let path = Path::new(path);
    let joined = if path.is_absolute() { path.to_path_buf() } else { root.join(path) };
    joined.display().to_string().replace('\\', "/")
}

/// The loop's state, so the helpers below take arguments instead of reaching into a caller.
struct Scanner {
    options: Options,
    handshake: Handshake,
    discovery: afz_protocol::Discovery,
    /// The `git` object for the scan result: the change set this scan was limited to. `None` for
    /// a whole-tree scan, which is the difference between 12 clean targets and a clean package.
    git_scope: Option<GitScope>,
    targets: Vec<TargetDescriptor>,
    /// Declared argument shape per target id, so a donor can be chosen by signature rather than
    /// by target id. Computed once: every mutation draws a donor, and serializing a `TypeNode`
    /// on each draw would cost more than the batch it is planning.
    signatures: HashMap<String, String>,
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
    /// When the loop started, so minimization is bounded by the scan's wall clock rather than by
    /// its own allowance alone. See [`shrink_budget`].
    started: Instant,
    /// Wall time each phase of the batch loop took across the whole run, written to the engine log
    /// so a scan that is slow says which of its three phases is the reason instead of leaving the
    /// reader to guess between planning, executing and scoring.
    plan_ms: u128,
    run_ms: u128,
    account_ms: u128,
    batches: u64,
    /// Invocations the plugin answered `failed`. Each one retires that plugin's invoke worker
    /// thread, so the count is a cost the finding list hides: identities deduplicate the report,
    /// not the thread restarts.
    failed: u64,
    /// Wall time from the scan entry to the first batch: spawn, handshake, discovery and
    /// scanner construction. No worker is executing targets during this, so it is where a scan's
    /// idle cores usually come from.
    setup_ms: u128,
    /// Wall time spent asking the workers for their reachable-edge tables, at each sample and at the
    /// end of the run. The answer waits for the slowest process while every other worker is idle.
    coverage_ms: u128,
}

/// The plugin connections minimization runs against, with the targets each has prepared.
struct Minimizer {
    launch: Launch,
    /// The inventory every connection this pool grows is given at spawn. `prepare` resolves a
    /// `targetId` only against a `discover` that ran in that same process, so a connection that
    /// never discovered answers every shrink trial with an RPC error, which the scan files as a
    /// `plugin-lost` finding against the target.
    discovery: DiscoverReq,
    /// `pool[0]` is the connection that already did the handshake and discovery; the rest appear
    /// only when a finding is actually being shrunk, so a scan with no findings never pays for them.
    pool: Vec<PluginClient>,
    /// Prepared target ids per connection, index-matched with `pool`.
    prepared: Vec<BTreeSet<String>>,
    deadline: Duration,
    /// How many connections the pool may hold. The guided workers are parked while a finding is
    /// shrunk, so minimization is given the scan's worker count rather than a smaller fixed number.
    /// Every connection is a plugin process with its own V8 heap, which is the only reason for a cap.
    cap: usize,
}

impl Minimizer {
    /// Answer each candidate on its own connection, in the order given. The order is the contract:
    /// the caller accepts the first candidate that reproduces, not the first to answer.
    fn run_batch(&mut self, target_id: &str, candidates: &[Vec<ValueTree>]) -> Vec<Outcome> {
        self.grow(candidates.len());
        let deadline = self.deadline;
        let launch = &self.launch;
        let discovery = &self.discovery;
        let mut outcomes = Vec::with_capacity(candidates.len());
        for chunk in candidates.chunks(self.pool.len()) {
            let answered = std::thread::scope(|scope| {
                let threads = self
                    .pool
                    .iter_mut()
                    .zip(self.prepared.iter_mut())
                    .zip(chunk)
                    .map(|((client, prepared), values)| {
                        scope.spawn(move || run_trial(client, prepared, launch, discovery, target_id, values, deadline))
                    })
                    .collect::<Vec<_>>();
                threads.into_iter().map(|thread| thread.join().unwrap_or_else(|_| Outcome::plugin_lost(0.0, "the minimizer trial thread panicked"))).collect::<Vec<_>>()
            });
            outcomes.extend(answered);
        }
        outcomes
    }

    /// Bring the pool up to `want` connections, capped at the scan's worker count, and stop at the
    /// first spawn failure: a machine that cannot start another plugin process shrinks on whatever
    /// connections it already has rather than failing the scan.
    fn grow(&mut self, want: usize) {
        while self.pool.len() < want.min(self.cap) {
            match spawn_discovered(&self.launch, &self.discovery) {
                Ok((client, Some(reason))) => {
                    eprintln!("afz: minimizer discovery failed: {}", reason.detail());
                    self.pool.push(client);
                    self.prepared.push(BTreeSet::new());
                }
                Ok((client, None)) => {
                    self.pool.push(client);
                    self.prepared.push(BTreeSet::new());
                }
                Err(_) => break,
            }
        }
    }

    /// Give back the connections a shrink grew, keeping the one that did the handshake and
    /// discovery. A scan finds few findings and the gap between them is minutes, so a second full set
    /// of plugin processes - each with a warm V8 heap and the target's modules loaded - would
    /// otherwise sit idle for the rest of the run. A measured yaml scan carried 24 plugin processes
    /// while 12 did the work, the extra twelve having grown for one shrink in its ninth minute.
    fn release(&mut self) {
        while self.pool.len() > 1 {
            if let Some(client) = self.pool.pop() {
                client.kill();
            }
            self.prepared.pop();
        }
    }
}

/// One shrink trial. A trial that killed the plugin is a candidate like any other: the next call
/// starts a fresh process, and the candidate is kept only if the fault comes back.
fn run_trial(
    client: &mut PluginClient,
    prepared: &mut BTreeSet<String>,
    launch: &Launch,
    discovery: &DiscoverReq,
    target_id: &str,
    values: &[ValueTree],
    deadline: Duration,
) -> Outcome {
    if client.lost().is_some() {
        match spawn_discovered(launch, discovery) {
            Ok((fresh, _)) => *client = fresh,
            Err(_) => return Outcome::plugin_lost(0.0, "the minimizer plugin could not be restarted"),
        }
        prepared.clear();
    }
    if !prepared.contains(target_id) {
        let prepare = PrepareReq { target_id: target_id.to_string(), values: values.to_vec() };
        if client.prepare(&prepare, PREPARE_TIMEOUT).is_ok() {
            prepared.insert(target_id.to_string());
        }
    }
    let request = InvokeReq {
        invocation_id: 0,
        target_id: target_id.to_string(),
        values: values.to_vec(),
        deadline_ms: deadline.as_millis() as u64,
        collect_coverage: false,
    };
    client.invoke_or_lose(&request, deadline + MINIMIZE_SLACK)
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
        let supervisor = Supervisor::new(launch.clone(), options.workers, deadline, DISABLE_AFTER, discovery_request.clone());
        let mut scanner = Scanner {
            options: options.clone(),
            handshake,
            discovery,
            git_scope: None,
            signatures: signature_index(&targets),
            targets,
            oracles,
            scheduler,
            corpus: Corpus::new(options.max_corpus),
            seen,
            rng: Rng::seeded_u64(options.seed),
            minimizer: Minimizer {
                launch,
                discovery: discovery_request,
                pool: vec![probe],
                prepared: vec![BTreeSet::new()],
                deadline,
                cap: options.workers.max(1),
            },
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
            started: Instant::now(),
            plan_ms: 0,
            run_ms: 0,
            account_ms: 0,
            batches: 0,
            failed: 0,
            setup_ms: 0,
            coverage_ms: 0,
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
            let planned_at = Instant::now();
            let jobs = self.plan_batch();
            if jobs.is_empty() {
                // Nothing left to try: every target is disabled or proved unbuildable.
                self.log.push("no target left to invoke".to_string());
                break;
            }
            let values_by_invocation: HashMap<u64, Vec<ValueTree>> =
                jobs.iter().map(|job| (job.invocation_id, job.values.clone())).collect();
            self.plan_ms += planned_at.elapsed().as_millis();
            let remaining = self.options.wall_clock.saturating_sub(elapsed);
            let ran_at = Instant::now();
            let report = self.supervisor.run(jobs, remaining);
            self.run_ms += ran_at.elapsed().as_millis();
            let accounted_at = Instant::now();
            self.account(report, &values_by_invocation, on_progress);
            self.account_ms += accounted_at.elapsed().as_millis();
            self.batches += 1;
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
                    let (donor_values, donor_id) = match donor.as_ref() {
                        Some((id, values)) => (Some(values), id.as_str()),
                        None => (None, ""),
                    };
                    let mutated = self.mutate_arguments(&entry.target_id, &values, donor_values, donor_id);
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

    /// Another retained input to donate a subtree during mutation, as its corpus entry id and its
    /// argument values. The entry id is what the transplant provenance records, so a finding names
    /// the corpus entry a grafted value came from and a triager can open that entry.
    fn donor(&mut self, target_id: &str, except: &str) -> Option<(String, Vec<ValueTree>)> {
        let signature = self.signatures.get(target_id)?.clone();
        let entries: Vec<(String, String)> =
            self.corpus.entries().map(|entry| (entry.id.clone(), entry.target_id.clone())).collect();
        let pool = donor_pool(&entries, target_id, except, &self.signatures, &signature);
        let id = self.rng.pick(&pool)?.clone();
        self.corpus.get(&id).map(|entry| (id.clone(), entry.values.clone()))
    }

    /// Mutate one argument of a retained input, leaving the others alone. Mutating every argument
    /// at once loses whatever combination the retained input had found.
    fn mutate_arguments(
        &mut self,
        target_id: &str,
        values: &[ValueTree],
        donor: Option<&Vec<ValueTree>>,
        donor_id: &str,
    ) -> Vec<ValueTree> {
        let Some(position) = self.targets.iter().position(|t| t.id == target_id) else { return values.to_vec() };
        if values.is_empty() || self.targets[position].parameters.is_empty() {
            return values.to_vec();
        }
        let index = self.rng.below(values.len() as u64) as usize;
        let Some(parameter) = self.targets[position].parameters.get(index) else { return values.to_vec() };
        let donor_value = donor.and_then(|d| d.get(index));
        let mut next = values.to_vec();
        let mutated = mutate_with_donor(&next[index], &parameter.ty, &mut self.rng, donor_value, donor_id);
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
            if matches!(completed.outcome, Outcome::Failed { .. }) {
                self.failed += 1;
            }
            let input = values.get(&completed.invocation_id).cloned().unwrap_or_default();
            self.learn(completed, &input);
            // Keyed on invocations that ran, not on the id the planner handed out. A scan stopped by
            // the wall clock mid-batch has planned ids it never executed, and a sample carrying one
            // of those numbers reads as more work done than `execution.invocations` reports.
            if self.ran >= self.next_sample {
                while self.next_sample <= self.ran {
                    self.next_sample += SAMPLE_EVERY;
                }
                // Reachable edges come from the processes that have run targets, and each worker
                // loads only the modules of the targets it invoked, so the pool's union has to be
                // re-read. It is asked for at a sample boundary and at `finish`, not after every batch:
                // the walk waits for the slowest process to serialize its whole map, and every other
                // worker is idle while it does, and `finish` restates each sample against the final
                // count anyway. Between samples the number is a display value.
                self.refresh_coverage();
                let sample = self.progress(self.ran);
                self.samples.push(sample.clone());
                on_progress(&sample);
            }
        }
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
        // `seen_before` is a statement about earlier scans, not about this one: `by_identity` above has
        // already dropped a repeat inside this run, so reaching here means the identity is new to this
        // scan and its absence from the history is the whole question. Such a finding is listed so the
        // reader sees that the fault is still being hit, and is not shrunk again, because an earlier
        // scan already did that work and wrote it up.
        let seen_before = !self.seen.record(&identity);
        let id = finding::finding_id(&identity);
        let minimized = (!seen_before).then(|| self.shrink(&finding.target_id, values));
        // The plugin renders the reproducer. The engine owns no target-language knowledge: it
        // stores the plugin's bytes verbatim, under an extension the plugin names and the
        // engine sanitizes. A plugin that does not declare `reproducer`, fails the call, or
        // answers unsafely gets no script; the finding's `inputs/` artifact is replayable
        // either way.
        let repro_attempt = minimized.as_ref().filter(|m| m.reproduces).and_then(|m| {
            if !self.handshake.capabilities.contains(&Capability::Reproducer) {
                return None;
            }
            let client = self.minimizer.pool.first()?;
            let req = ReproducerReq { target_id: finding.target_id.clone(), values: m.values.clone() };
            Some(client.reproducer(&req, REPRODUCER_TIMEOUT))
        });
        let repro = match repro_attempt {
            Some(Ok(resp)) => {
                if resp.script.is_empty() || resp.script.len() > MAX_REPRO_SCRIPT_BYTES || !valid_extension(&resp.extension) {
                    self.log.push(format!("finding {id} gets no reproducer: the plugin answered an empty or oversized script, or the unsafe extension {:?}", resp.extension));
                    None
                } else {
                    Some(ReproFile { artifact: reproducer_artifact_path(&id, &resp.extension), content: resp.script })
                }
            }
            Some(Err(reason)) => {
                self.log.push(format!("finding {id} gets no reproducer: {}", reason.detail()));
                None
            }
            None => None,
        };
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
            reproducer: repro.as_ref().map(|r| Artifact { artifact: r.artifact.clone() }),
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
            None if seen_before => format!(
                "finding {id} at invocation {}: {} {} (an earlier scan reported this identity, so it is not shrunk again)",
                finding.invocation_id,
                finding.oracle.as_str(),
                fault.message
            ),
            // Shrinking gave no result: the wall clock line above says when the budget was spent, and
            // a shrink that could not reproduce the fault at all reports `reproduced false` instead.
            None => format!(
                "finding {id} at invocation {}: {} {} (not shrunk)",
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
                reproducer: repro,
            },
        ));
    }

    /// Shrink a finding's input against the same fault. Trials run on the minimizer's own plugin
    /// connections, so shrinking never displaces the guided workers, and one round's candidates are
    /// answered on all of them at once because the workers that found the bug have nothing to do
    /// meanwhile.
    ///
    /// The work is bounded by the scan's clock rather than by its own allowance alone. A round needs
    /// the previous round's answer, so a finding whose rounds stay narrow - one long string, few
    /// parts to drop at a time - cannot keep every connection busy, and an unbounded sum over
    /// findings held one run in this phase for half an hour after its invocations had finished.
    fn shrink(&mut self, target_id: &str, values: &[ValueTree]) -> Minimized {
        let left = shrink_budget(self.started.elapsed(), self.options.wall_clock);
        if left.is_zero() {
            self.log.push(format!(
                "finding on {target_id} not shrunk: the wall clock budget of {} ms is spent",
                self.options.wall_clock.as_millis()
            ));
            let bytes = values.iter().map(ValueTree::wire_bytes).sum();
            return Minimized { values: values.to_vec(), attempts: 0, bytes_before: bytes, bytes_after: bytes, reproduces: false };
        }
        let started_at = Instant::now();
        let width = self.minimizer.cap;
        let result = minimize::minimize(values, MINIMIZE_ATTEMPTS, left, width, &mut |candidates| self.minimizer.run_batch(target_id, candidates));
        self.minimizer.release();
        self.log.push(format!(
            "shrunk {target_id}: {} attempts, {} -> {} bytes in {:.1} s",
            result.attempts,
            result.bytes_before,
            result.bytes_after,
            started_at.elapsed().as_secs_f64()
        ));
        result
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
        let asked_at = Instant::now();
        if let Some(map) = self.supervisor.coverage_map(COVERAGE_TIMEOUT) {
            self.coverage_total = map.entries.len() as u64;
            self.coverage_map = Some(map);
        }
        self.coverage_ms += asked_at.elapsed().as_millis();
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
        self.log.push(format!(
            "{} batches over {} ms: plan {} ms, run {} ms, account {} ms, setup {} ms, coverage map {} ms",
            self.batches,
            started_at.elapsed().unwrap_or_default().as_millis(),
            self.plan_ms,
            self.run_ms,
            self.account_ms,
            self.setup_ms,
            self.coverage_ms,
        ));
        self.log.push(format!("{} invocations answered failed, each retiring the plugin's invoke worker", self.failed));
        // A plugin that dies on its own leaves the reason in its stderr, which the engine captures at
        // spawn. Without this the log said only `plugin exited with code 1`, and 119 plugin deaths in
        // one scan carried no explanation at all. Shrink connections replaced mid-minimization are not
        // covered here: their buffer went with the client that held it.
        let shrink_stderr: Vec<String> = self
            .minimizer
            .pool
            .iter()
            .flat_map(|client| std::mem::take(&mut *client.stderr_lines.lock().expect("stderr lock")))
            .collect();
        let worker_stderr = self.supervisor.take_stderr();
        let mut reported: BTreeSet<&str> = BTreeSet::new();
        for line in worker_stderr.iter().chain(shrink_stderr.iter()) {
            let text = line.trim();
            if text.is_empty() || !reported.insert(text) {
                continue;
            }
            self.log.push(format!("plugin said: {text}"));
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
                    protocol_version: self.handshake.protocol_version.clone(),
                    name: self.handshake.name.clone(),
                    version: self.handshake.version.clone(),
                    target_class: self.handshake.target_class,
                    capabilities: self.handshake.capabilities.clone(),
                },
            },
            git: self.git_scope.clone(),
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
                samples: self
                    .samples
                    .iter()
                    .map(|sample| ProgressSample {
                        invocations: sample.invocations,
                        coverage_edges: sample.coverage_edges,
                        coverage_ratio: sample.coverage_ratio,
                    })
                    .collect(),
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

/// The declared argument shape of a target: its parameter types in order. Parameter names are
/// left out because they say nothing about whether a value is legal at a position.
fn signature_of(target: &TargetDescriptor) -> String {
    target
        .parameters
        .iter()
        .map(|parameter| serde_json::to_string(&parameter.ty).unwrap_or_else(|_| "?".to_string()))
        .collect::<Vec<_>>()
        .join("|")
}

fn signature_index(targets: &[TargetDescriptor]) -> HashMap<String, String> {
    targets.iter().map(|target| (target.id.clone(), signature_of(target))).collect()
}

/// Which retained entries may donate to `target_id`: another entry for the same target first,
/// and otherwise any entry whose declared parameter types are the same.
///
/// Entry points that take the same argument types solve the same argument problem, so an input
/// that satisfied one of them is a head start on the others. On the billing test target that sharing
/// is the difference between finding the bug and not finding it: `creditSummary` is the only entry
/// point that calls `isReissue`, it retained a single input that failed the admission test, and
/// the two siblings taking the same `ReportRequest` were the ones whose entries got past
/// `isCreditWindow`. `mutate::transplant` still rejects any node the destination declaration does
/// not allow, so a wider pool cannot graft a value the target would read as a type error.
fn donor_pool(
    entries: &[(String, String)],
    target_id: &str,
    except: &str,
    signatures: &HashMap<String, String>,
    signature: &str,
) -> Vec<String> {
    let mut same_target = Vec::new();
    let mut same_shape = Vec::new();
    for (id, owner) in entries {
        if id == except {
            continue;
        }
        if owner == target_id {
            same_target.push(id.clone());
        } else if signatures.get(owner).map(String::as_str) == Some(signature) {
            same_shape.push(id.clone());
        }
    }
    if same_target.is_empty() { same_shape } else { same_target }
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

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn shrink_work_stops_when_the_scan_budget_is_spent() {
        // A finding found late must not spend its own per-finding allowance after the run is over.
        // The bound is the scan's clock, so the sum across findings cannot overrun it by more than
        // the grace the result write needs.
        assert_eq!(shrink_budget(Duration::ZERO, Duration::from_secs(600)), MINIMIZE_BUDGET);
        assert_eq!(shrink_budget(Duration::from_secs(500), Duration::from_secs(600)), Duration::from_secs(130));
        assert_eq!(shrink_budget(Duration::from_secs(631), Duration::from_secs(600)), Duration::ZERO);
    }

    fn signatures() -> HashMap<String, String> {
        HashMap::from([
            ("summary".to_string(), "REPORT".to_string()),
            ("manifest".to_string(), "REPORT".to_string()),
            ("digest".to_string(), "NUMBER".to_string()),
        ])
    }

    #[test]
    fn a_donor_comes_from_a_sibling_with_the_same_argument_types() {
        // The entry being mutated is the only one for its target, so the same-target pool is
        // empty and the sibling taking the same request type has to serve.
        let entries = vec![
            ("c-summary".to_string(), "summary".to_string()),
            ("c-manifest".to_string(), "manifest".to_string()),
            ("c-digest".to_string(), "digest".to_string()),
        ];
        assert_eq!(donor_pool(&entries, "summary", "c-summary", &signatures(), "REPORT"), vec!["c-manifest".to_string()]);
    }

    #[test]
    fn same_target_entries_are_preferred_and_an_entry_is_never_its_own_donor() {
        let entries = vec![
            ("c-summary".to_string(), "summary".to_string()),
            ("c-summary-2".to_string(), "summary".to_string()),
            ("c-manifest".to_string(), "manifest".to_string()),
        ];
        assert_eq!(donor_pool(&entries, "summary", "c-summary", &signatures(), "REPORT"), vec!["c-summary-2".to_string()]);
    }

    #[test]
    fn a_different_argument_shape_is_not_a_donor() {
        let entries = vec![("c-digest".to_string(), "digest".to_string())];
        assert!(donor_pool(&entries, "summary", "c-summary", &signatures(), "REPORT").is_empty());
    }
}

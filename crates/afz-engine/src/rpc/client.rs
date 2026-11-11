//! Engine-side plugin client: one plugin process per worker, JSON-RPC over its stdio.
//!
//! The rule this file exists to enforce is that a plugin is a hostile, fallible peer. A slow
//! answer, EOF, a panic, a `process.exit`, and an engine kill are all *results*, never engine
//! errors: the caller gets a [`LostReason`] it maps onto an `Outcome`, so a scan of a target
//! that crashes the plugin still finishes and still reports.
//!
//! Ownership matters here. The reaper thread owns the `Child`, because only one place can
//! call `wait()`. The kill path asks the reaper for a kill over a channel and waits for the
//! confirmed exit status, so a restart never races a half-dead process. One mutex guards both
//! the loss reason and the in-flight request slots, so waking waiters and recording why the
//! process died cannot interleave.

use std::collections::BTreeMap;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

/// The signal that ended the plugin, when there was one. Windows has no signal numbers, so
/// the engine classifies an exit there by code alone.
fn exit_signal(status: &ExitStatus) -> Option<i32> {
    #[cfg(unix)]
    {
        status.signal()
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        None
    }
}

use afz_protocol::*;

/// Why a call did not get an answer. The supervisor maps these onto outcomes:
/// `NoAnswer` becomes a `deadline`, an unrequested `Exited` becomes `processExit`, and
/// anything else becomes `failed` with `via: "plugin-lost"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LostReason {
    /// The plugin is alive but did not answer within the timeout.
    NoAnswer,
    /// The plugin closed stdout without an explanation.
    Eof,
    /// The plugin process exited on its own.
    Exited { code: Option<i32>, signal: Option<i32> },
    /// The engine killed it, because the call crossed its deadline or the scan ended.
    Killed,
    /// The engine could not start the process, or the plugin replied unparseably.
    Spawn(String),
}

impl LostReason {
    pub fn detail(&self) -> String {
        match self {
            LostReason::NoAnswer => "plugin did not answer before the deadline".to_string(),
            LostReason::Eof => "plugin closed the pipe".to_string(),
            LostReason::Exited { code: Some(c), signal: None } => format!("plugin exited with code {c}"),
            LostReason::Exited { code: None, signal: Some(s) } => format!("plugin killed by signal {s}"),
            LostReason::Exited { code: Some(c), signal: Some(s) } => {
                format!("plugin exited with code {c} after signal {s}")
            }
            LostReason::Exited { .. } => "plugin exited".to_string(),
            LostReason::Killed => "engine killed the plugin".to_string(),
            LostReason::Spawn(e) => e.clone(),
        }
    }

    /// The `via` string a finding should carry for this loss.
    pub fn via(&self) -> &'static str {
        match self {
            LostReason::NoAnswer => "deadline",
            LostReason::Exited { .. } => "processExit",
            LostReason::Killed => "plugin-killed",
            LostReason::Eof | LostReason::Spawn(_) => "plugin-lost",
        }
    }

    /// Whether the engine asked for this. A `Killed` loss with a call in flight is a deadline
    /// or a shutdown; anything else is the plugin's own doing, and the supervisor classifies
    /// the two differently.
    pub fn engine_requested(&self) -> bool {
        matches!(self, LostReason::Killed)
    }
}

/// Either a decoded answer, or the reason there is none.
pub type Call<T> = Result<T, LostReason>;

/// How the engine launches a plugin: argv, plus the environment and directory it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub cwd: Option<String>,
}

impl Launch {
    pub fn new(argv: impl IntoIterator<Item = impl Into<String>>) -> Launch {
        Launch { argv: argv.into_iter().map(Into::into).collect(), env: BTreeMap::new(), cwd: None }
    }

    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Launch {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn with_cwd(mut self, cwd: impl Into<String>) -> Launch {
        self.cwd = Some(cwd.into());
        self
    }

    /// A human-facing rendering for the scan log.
    pub fn display(&self) -> String {
        self.argv.join(" ")
    }
}

/// The loss reason plus the requests still in flight, under one lock so a waiter is woken
/// with the reason already recorded.
struct State {
    lost: Option<LostReason>,
    slots: BTreeMap<u64, mpsc::Sender<Response>>,
}

impl State {
    /// Record `reason` unless a more specific one is already recorded, then wake waiters.
    fn lose(&mut self, reason: LostReason) {
        let replace = match &self.lost {
            None => true,
            // The engine's own kill outranks the exit status that kill produced, and a real
            // exit status outranks the EOF the reader noticed first.
            Some(LostReason::Killed) => false,
            Some(LostReason::Eof) => !matches!(reason, LostReason::Eof),
            Some(_) => false,
        };
        if replace {
            self.lost = Some(reason);
        }
        self.slots.clear();
    }
}

/// One live plugin process plus the pipe machinery around it.
pub struct PluginClient {
    launch: Launch,
    next_id: AtomicU64,
    stdin: Mutex<ChildStdin>,
    state: Arc<Mutex<State>>,
    kill: mpsc::SyncSender<mpsc::Sender<()>>,
    reaper: Mutex<Option<mpsc::Receiver<()>>>,
    kill_on_drop: AtomicBool,
    pub stderr_lines: Arc<Mutex<Vec<String>>>,
    pid: u32,
}

impl PluginClient {
    /// Start the plugin. A spawn failure is reported as `LostReason::Spawn`, not as an error,
    /// so the supervisor counts it like any other plugin loss.
    pub fn spawn(launch: &Launch) -> Result<PluginClient, LostReason> {
        let Some(program) = launch.argv.first() else {
            return Err(LostReason::Spawn("empty harness.command".to_string()));
        };
        let mut cmd = Command::new(program);
        cmd.args(launch.argv.iter().skip(1))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .envs(&launch.env);
        if let Some(cwd) = &launch.cwd {
            cmd.current_dir(cwd);
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return Err(LostReason::Spawn(format!("{}: {e}", launch.display()))),
        };
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let pid = child.id();

        let state = Arc::new(Mutex::new(State { lost: None, slots: BTreeMap::new() }));
        let stderr_lines = Arc::new(Mutex::new(Vec::new()));
        let (kill_tx, kill_rx) = mpsc::sync_channel::<mpsc::Sender<()>>(1);
        let (reaper_done_tx, reaper_done_rx) = mpsc::channel();

        spawn_reader(stdout, Arc::clone(&state));
        spawn_reaper(child, Arc::clone(&state), kill_rx, reaper_done_tx);
        spawn_stderr_drain(stderr, Arc::clone(&stderr_lines));

        Ok(PluginClient {
            launch: launch.clone(),
            next_id: AtomicU64::new(1),
            stdin: Mutex::new(stdin),
            state,
            kill: kill_tx,
            reaper: Mutex::new(Some(reaper_done_rx)),
            kill_on_drop: AtomicBool::new(true),
            stderr_lines,
            pid,
        })
    }

    pub fn launch(&self) -> &Launch {
        &self.launch
    }

    /// The plugin's process id, for a lifecycle check that the engine left nothing behind.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The recorded loss, if the process is gone. Requests made after this fail immediately.
    pub fn lost(&self) -> Option<LostReason> {
        self.state.lock().expect("state lock").lost.clone()
    }

    /// Kill the plugin now and wait for the exit. Any call in flight resolves as
    /// `LostReason::Killed`.
    pub fn kill(&self) {
        // Record the request first, so a call in flight is attributed to the engine's kill
        // rather than to the exit status the kill produces.
        self.state.lock().expect("state lock").lose(LostReason::Killed);
        let (ack_tx, ack_rx) = mpsc::channel();
        // A full or closed channel means the reaper already finished: the process is gone.
        if self.kill.send(ack_tx).is_ok() {
            let _ = ack_rx.recv_timeout(Duration::from_secs(10));
        }
    }

    /// Send one request and wait for the matching response.
    pub fn request<T>(&self, method: Method, params: Option<serde_json::Value>, timeout: Duration) -> Call<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let deadline = Instant::now() + timeout;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        {
            let mut state = self.state.lock().expect("state lock");
            if let Some(reason) = state.lost.clone() {
                return Err(reason);
            }
            state.slots.insert(id, tx);
        }

        let req = match params {
            Some(params) => Request::new(id, method, params),
            None => Request::empty(id, method),
        };
        let write = {
            let mut stdin = self.stdin.lock().expect("stdin lock");
            write_line(&mut *stdin, &req)
        };
        if let Err(e) = write {
            self.drop_slot(id);
            // The write failed because the peer is gone. Give the reaper its poll interval to
            // record the exit status, so the finding says `exited with code 1` rather than the
            // reader's earlier `closed the pipe`.
            let reason = self
                .await_exit_reason(Duration::from_millis(250))
                .unwrap_or_else(|| LostReason::Spawn(format!("pipe to {} closed: {e}", self.launch.display())));
            return Err(reason);
        }

        let response = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining) {
                Ok(resp) => break resp,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.drop_slot(id);
                    // A recorded loss is more specific than "slow": the plugin died while we
                    // waited, and the supervisor should classify the death, not the timeout.
                    return Err(self.lost().unwrap_or(LostReason::NoAnswer));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.drop_slot(id);
                    return Err(self
                        .await_exit_reason(Duration::from_millis(250))
                        .unwrap_or(LostReason::Eof));
                }
            }
        };

        match response {
            Response::Success(s) => serde_json::from_value(s.result).map_err(|e| {
                LostReason::Spawn(format!(
                    "{} replied malformed json to {}: {e}",
                    self.launch.display(),
                    method.name()
                ))
            }),
            Response::Failure(f) => Err(LostReason::Spawn(format!(
                "{} failed {} ({}): {}",
                self.launch.display(),
                method.name(),
                f.error.code,
                f.error.message
            ))),
        }
    }

    fn drop_slot(&self, id: u64) {
        self.state.lock().expect("state lock").slots.remove(&id);
    }

    /// The reaper records the exit status a poll interval after the reader notices a closed
    /// pipe. Wait that long for the specific reason before settling for a generic one.
    fn await_exit_reason(&self, grace: Duration) -> Option<LostReason> {
        let until = Instant::now() + grace;
        loop {
            match self.lost() {
                // EOF is the reader's placeholder; the reaper's exit status is the truth.
                Some(LostReason::Eof) if Instant::now() < until => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                reason => return reason,
            }
        }
    }

    pub fn handshake(&self) -> Call<Handshake> {
        self.request(Method::Handshake, None, Duration::from_secs(30))
    }

    pub fn discover(&self, req: &DiscoverReq, timeout: Duration) -> Call<Discovery> {
        self.request(Method::Discover, Some(serde_json::to_value(req).expect("DiscoverReq")), timeout)
    }

    pub fn prepare(&self, req: &PrepareReq, timeout: Duration) -> Call<PrepareResp> {
        self.request(Method::Prepare, Some(serde_json::to_value(req).expect("PrepareReq")), timeout)
    }

    /// One invocation. The reason is returned rather than folded away, because the supervisor
    /// distinguishes a deadline from a process exit.
    pub fn invoke(&self, req: &InvokeReq, timeout: Duration) -> Call<Outcome> {
        self.request(Method::Invoke, Some(serde_json::to_value(req).expect("InvokeReq")), timeout)
    }

    /// One invocation with the default mapping: any loss becomes `failed` with
    /// `via: "plugin-lost"`, which is what a caller that does not supervise wants.
    pub fn invoke_or_lose(&self, req: &InvokeReq, timeout: Duration) -> Outcome {
        match self.invoke(req, timeout) {
            Ok(outcome) => outcome,
            Err(reason) => Outcome::plugin_lost(req.deadline_ms as f64, reason.detail()),
        }
    }

    pub fn minimize(&self, req: &MinimizeReq, timeout: Duration) -> Call<MinimizeResp> {
        self.request(Method::Minimize, Some(serde_json::to_value(req).expect("MinimizeReq")), timeout)
    }

    pub fn coverage_map(&self, timeout: Duration) -> Call<CoverageMap> {
        self.request(Method::CoverageMap, None, timeout)
    }
}

/// A plugin client is mostly OS handles, so `Debug` reports what identifies it and nothing else.
impl std::fmt::Debug for PluginClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginClient")
            .field("launch", &self.launch.display())
            .field("pid", &self.pid)
            .field("lost", &self.lost())
            .finish_non_exhaustive()
    }
}

impl Drop for PluginClient {
    fn drop(&mut self) {
        if self.kill_on_drop.load(Ordering::Relaxed) {
            self.kill();
        }
        if let Some(rx) = self.reaper.lock().expect("reaper lock").take() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }
}

fn spawn_reader(stdout: ChildStdout, state: Arc<Mutex<State>>) {
    std::thread::Builder::new()
        .name("afz-rpc-reader".to_string())
        .spawn(move || {
            let mut reader = buffered(stdout);
            let mut buf = String::new();
            loop {
                match read_line(&mut reader, &mut buf) {
                    Ok(Some(line)) => {
                        let Ok(resp) = parse_response(&line) else { continue };
                        let slot = {
                            let mut state = state.lock().expect("state lock");
                            state.slots.remove(&resp.id())
                        };
                        if let Some(tx) = slot {
                            let _ = tx.send(resp);
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            // A closed pipe is how a plugin dies when no exit status is readable yet.
            state.lock().expect("state lock").lose(LostReason::Eof);
        })
        .expect("spawn reader");
}

/// Owns the `Child`: waits for exit, records the reason, and answers kill requests.
fn spawn_reaper(
    mut child: Child,
    state: Arc<Mutex<State>>,
    kill_rx: mpsc::Receiver<mpsc::Sender<()>>,
    done_tx: mpsc::Sender<()>,
) {
    std::thread::Builder::new()
        .name("afz-rpc-reaper".to_string())
        .spawn(move || {
            let status = loop {
                if let Ok(ack) = kill_rx.try_recv() {
                    let _ = child.kill();
                    let status = child.wait();
                    let _ = ack.send(());
                    break status;
                }
                match child.try_wait() {
                    Ok(Some(status)) => break Ok(status),
                    Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                    Err(e) => {
                        eprintln!("afz: cannot wait on plugin: {e}");
                        break Err(e);
                    }
                }
            };
            let reason = match status {
                Ok(status) => LostReason::Exited { code: status.code(), signal: exit_signal(&status) },
                Err(_) => LostReason::Eof,
            };
            state.lock().expect("state lock").lose(reason);
            let _ = done_tx.send(());
        })
        .expect("spawn reaper");
}

fn spawn_stderr_drain(stderr: std::process::ChildStderr, lines: Arc<Mutex<Vec<String>>>) {
    std::thread::Builder::new()
        .name("afz-rpc-stderr".to_string())
        .spawn(move || {
            let mut reader = buffered(stderr);
            let mut buf = String::new();
            while let Ok(Some(line)) = read_line(&mut reader, &mut buf) {
                let mut lines = lines.lock().expect("stderr lock");
                if lines.len() < 4096 {
                    lines.push(line);
                }
            }
        })
        .expect("spawn stderr drain");
}

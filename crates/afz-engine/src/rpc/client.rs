//! Engine-side plugin client: one plugin process per worker, JSON-RPC over its stdio.
//!
//! The transport belongs to [`plugin_rpc::process`]. It owns the spawn, the stdout reader, the
//! exit watcher, the stderr drain, the request ids, the in-flight slots, the handshake-first rule,
//! and the kill. What lives here is the profile's half of the boundary: the method names and
//! payload types the engine calls, the handshake read in the profile's own shape, and the one
//! place the mechanism's [`Loss`] becomes this profile's `via` vocabulary.
//!
//! The rule the file serves is that a plugin is a hostile, fallible peer. A slow answer, EOF, a
//! panic, a `process.exit`, and an engine kill are all *results*, never engine errors: the caller
//! gets a [`Call`] it maps onto an `Outcome`, so a scan of a target that crashes the plugin still
//! finishes and still reports.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use afz_protocol::*;
use plugin_rpc::process::{Plugin, PluginConfig, StartError};

/// The `via` string a finding carries for a loss the mechanism reported. The engine keeps no loss
/// enum of its own - it had written one, veridoc had written a byte-identical one, and
/// `plugin_rpc::Loss` owns both now - but `via` is this profile's own vocabulary, so this is the
/// one place the mechanism's reason becomes the finding's word.
pub fn via(loss: &Loss) -> &'static str {
    match loss {
        Loss::NoAnswer => "deadline",
        Loss::Exited { .. } => "processExit",
        Loss::Killed => "plugin-killed",
        Loss::Eof | Loss::Spawn { .. } | Loss::Profile { .. } => "plugin-lost",
    }
}

/// One plugin call as a sentence, for a caller whose error channel is a string rather than a
/// `Call`. A refusal and a loss both mean there is no payload, and they are different fixes: the
/// plugin answered that it will not do the work, or the plugin did not answer at all. Reporting the
/// first as the second is what made a refusal look like a dead process.
pub fn sentence<T>(method: &str, call: Call<T>) -> Result<T, String> {
    match call {
        Call::Result(value) => Ok(value),
        Call::Refusal(error) => Err(format!(
            "the plugin refused {method} ({}): {}",
            error.code, error.message
        )),
        Call::Lost(loss) => Err(loss.to_string()),
    }
}

/// Why there is no client. The mechanism's two reasons, plus the one this profile owns.
#[derive(Debug)]
pub enum ClientError {
    /// What [`Plugin::start`] returned: `Lost` is no process, `Refused` is a live plugin that
    /// declined to be a plugin for this engine.
    Start(StartError),
    /// The plugin answered the handshake with a document the `afz/1` handshake does not describe.
    /// It claimed a protocol revision it does not implement, which is autofuzz's judgement to
    /// name - not a process that went missing, and not a refusal either.
    Profile(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Start(error) => write!(f, "{error}"),
            ClientError::Profile(detail) => write!(
                f,
                "the plugin answered `handshake` in a shape the {PROTOCOL_VERSION} profile does \
                 not accept: {detail}"
            ),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<StartError> for ClientError {
    fn from(error: StartError) -> ClientError {
        ClientError::Start(error)
    }
}

/// One live plugin process, driven through the mechanism's transport.
pub struct PluginClient {
    plugin: Plugin,
    /// The handshake as this profile reads it, and the document it was read from. Both come from
    /// the one `handshake` request `Plugin::start` sent; see [`PluginClient::handshake`].
    handshake: Handshake,
    handshake_wire: serde_json::Value,
}

impl PluginClient {
    /// Start the plugin and shake hands with it. The handshake is the first request, which
    /// [`Plugin::start`] enforces, so a client that exists has already been introduced.
    ///
    /// [`ClientError::Start(StartError::Lost)`] is the spawn failure or silence the supervisor
    /// counts like any other plugin loss. [`ClientError::Start(StartError::Refused)`] is a
    /// different fact - the plugin is alive and declined to be a plugin for this engine - and no
    /// caller may fold it into the first.
    pub fn spawn(config: &PluginConfig) -> Result<PluginClient, ClientError> {
        let (plugin, handshake) = Plugin::start(config)?;
        // The mechanism hands over the whole `result` object, because the envelope is open and
        // this profile's handshake carries more than the four fields the mechanism reads:
        // `targetClass`, `languages`, `discoverRequiresExecution`, and the capabilities as this
        // profile's own enum. Reading them here is what keeps `handshake` one request.
        let handshake_wire = handshake.result;
        let profile = serde_json::from_value(handshake_wire.clone())
            .map_err(|error| ClientError::Profile(error.to_string()))?;
        Ok(PluginClient {
            plugin,
            handshake: profile,
            handshake_wire,
        })
    }

    /// What this plugin was started with: argv, environment, working directory, timeouts.
    pub fn config(&self) -> &PluginConfig {
        self.plugin.config()
    }

    /// The plugin's process id, for a lifecycle check that the engine left nothing behind.
    pub fn pid(&self) -> u32 {
        self.plugin.pid()
    }

    /// The recorded loss, if the process is gone. Requests made after this fail immediately.
    pub fn lost(&self) -> Option<Loss> {
        self.plugin.lost()
    }

    /// The `id` 0 refusal the plugin reported and could not attribute to any request. It answers
    /// no particular call, so a caller that discarded it would lose the only account of why the
    /// connection went bad.
    pub fn connection_fault(&self) -> Option<ErrorObject> {
        self.plugin.connection_fault()
    }

    /// Lines this plugin process wrote to stderr, as the mechanism's own drain collects them.
    pub fn stderr_lines(&self) -> Arc<Mutex<Vec<String>>> {
        Arc::clone(&self.plugin.stderr_lines)
    }

    /// Kill the plugin now and wait for it to be gone. A call in flight is attributed to this
    /// kill, not to the exit status the kill produced.
    pub fn kill(&self) {
        self.plugin.kill();
    }

    /// Send one request and wait for the response whose id matches.
    pub fn request<T>(
        &self,
        method: Method,
        params: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Call<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.plugin.request(method, params, timeout)
    }

    /// The handshake in the profile's own shape, as the plugin answered it to
    /// [`PluginClient::spawn`]. `handshake` is one request: the mechanism handed the whole `result`
    /// to the host, so nothing here asks the plugin a second time, and a client that exists has an
    /// answer - there is no failure left for this to report.
    pub fn handshake(&self) -> &Handshake {
        &self.handshake
    }

    /// The handshake document exactly as the plugin wrote it, for a caller that validates the
    /// document before reading it - the conformance suite's `handshake-schema` rule.
    pub fn handshake_document(&self) -> &serde_json::Value {
        &self.handshake_wire
    }

    pub fn discover(&self, req: &DiscoverReq, timeout: Duration) -> Call<Discovery> {
        self.plugin.call(Method::Discover, req, timeout)
    }

    pub fn prepare(&self, req: &PrepareReq, timeout: Duration) -> Call<PrepareResp> {
        self.plugin.call(Method::Prepare, req, timeout)
    }

    /// One invocation. The reason is returned rather than folded away, because the supervisor
    /// distinguishes a deadline from a process exit.
    pub fn invoke(&self, req: &InvokeReq, timeout: Duration) -> Call<Outcome> {
        self.plugin.call(Method::Invoke, req, timeout)
    }

    /// One invocation with the default mapping: a payload is returned as-is, a refusal becomes
    /// `failed` with `via: "plugin-refused"` carrying the plugin's own message, and a loss becomes
    /// `failed` with `via: "plugin-lost"`. That is what a caller that does not supervise wants.
    pub fn invoke_or_lose(&self, req: &InvokeReq, timeout: Duration) -> Outcome {
        match self.invoke(req, timeout) {
            Call::Result(outcome) => outcome,
            Call::Refusal(error) => Outcome::plugin_refused(
                req.deadline_ms as f64,
                format!("{}: {}", error.code, error.message),
            ),
            Call::Lost(loss) => Outcome::plugin_lost(req.deadline_ms as f64, loss.to_string()),
        }
    }

    pub fn minimize(&self, req: &MinimizeReq, timeout: Duration) -> Call<MinimizeResp> {
        self.plugin.call(Method::Minimize, req, timeout)
    }

    pub fn reproducer(&self, req: &ReproducerReq, timeout: Duration) -> Call<ReproducerResp> {
        self.plugin.call(Method::Reproducer, req, timeout)
    }

    pub fn coverage_map(&self, timeout: Duration) -> Call<CoverageMap> {
        self.plugin.request(Method::CoverageMap, None, timeout)
    }

    /// The readiness probe. The engine calls it once, on the probe connection, only when
    /// the handshake declares `readiness`.
    pub fn ready(&self, timeout: Duration) -> Call<ReadyResp> {
        self.plugin.request(Method::Ready, None, timeout)
    }
}

/// Start a plugin and give it the inventory it needs to answer `prepare`. Every connection the
/// engine runs `prepare` or `invoke` on goes through here, because `prepare` resolves a
/// `targetId` only against a `discover` that ran in that same process, and no protocol method
/// carries an inventory from one process to another.
///
/// The `Err` arm is the one case that leaves no usable process: it never started, or it answered
/// the handshake without a plugin behind it. A plugin that starts and then fails `discover` comes
/// back in the `Ok` arm with the reason beside it and the client still usable, the way
/// `Worker::start` keeps such a process: every `prepare` then reports the plugin's own reason,
/// which is the diagnostic a scan result should carry rather than a spawn error that hides it.
pub fn spawn_discovered(
    config: &PluginConfig,
    discovery: &DiscoverReq,
) -> Result<(PluginClient, Option<String>), ClientError> {
    let client = PluginClient::spawn(config)?;
    let failed = match client.discover(discovery, crate::supervise::DISCOVERY_TIMEOUT) {
        Call::Result(_) => None,
        // A refusal says the plugin declined to inventory itself; a loss says it could not answer.
        // Both leave the process up, and `prepare` reports the same reason again later, so this is
        // the wording for the scan log and not a decision.
        Call::Refusal(error) => Some(format!(
            "declined discover ({}): {}",
            error.code, error.message
        )),
        Call::Lost(loss) => Some(loss.to_string()),
    };
    Ok((client, failed))
}

/// A plugin client is mostly OS handles, so `Debug` reports what identifies it and nothing else.
impl std::fmt::Debug for PluginClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginClient")
            .field("command", &self.plugin.config().command.join(" "))
            .field("pid", &self.pid())
            .field("lost", &self.lost())
            .finish_non_exhaustive()
    }
}

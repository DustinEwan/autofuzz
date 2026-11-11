//! Scan project: the unit of configuration and the unit of one scan.
//!
//! `autofuzz.json` at the target project root is found without arguments by `afz scan`;
//! `--config <path>` selects a different file. A run names exactly one block, `codebase` or
//! `runtime`, and one scan writes one scan result with one `harness` object, which is why
//! the config holds at most one harness per file.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::rpc::client::Launch;

pub const CONFIG_FILE_NAME: &str = "autofuzz.json";

/// Validation and user-facing failure messages share these strings, so a test can assert the
/// exact wording the plan requires.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Message(String),
    #[error("cannot read {}: {source}", path.display())]
    Read { path: PathBuf, source: std::io::Error },
    #[error("cannot parse {}: {source}", path.display())]
    Parse { path: PathBuf, source: serde_json::Error },
}

impl ConfigError {
    fn msg(m: impl Into<String>) -> ConfigError {
        ConfigError::Message(m.into())
    }
}

/// Which block a scan run targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    Codebase,
    Runtime,
}

/// How the plugin is launched. `command` is argv, so a scan project can name
/// `["node", "server.js"]` without the engine guessing a runtime.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct HarnessConfig {
    /// Harness selector: `js-ts` today, `wasm` and `http` later.
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
}

/// Fixture module for `fixture-required` parameters. Reserved in v1: the block parses, and
/// the inventory still reports those targets as skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct FixturesConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// A design-time target: "look at this code and fuzz its public boundaries".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct CodebaseBlock {
    pub harness: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixtures: Option<FixturesConfig>,
    /// Error classes the user declares as the target's rejection channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_classes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
}

/// A runtime target: an environment, a service to stand up, and an API definition to fuzz
/// against. Reserved in v1.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct RuntimeBlock {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openapi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_mutating: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requests_per_second: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
}

/// Oracle severities. `off` disables an oracle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    Off,
    Low,
    #[default]
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn enabled(self) -> bool {
        self != Severity::Off
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Off => "off",
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

/// One oracle's configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct OracleConfig {
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mb: Option<u64>,
}

/// Reclassify an outcome by the rule that produced it, for a target whose declared channel
/// is documented as internal-failure-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OracleOverride {
    #[serde(default)]
    pub target: Option<String>,
    pub via: String,
    /// `ok`, `rejected`, or `failed`.
    pub treat_as: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct OraclesConfig {
    pub failed: OracleConfig,
    pub rejected: OracleConfig,
    pub deadline: OracleConfig,
    #[serde(rename = "memoryGrowth")]
    pub memory_growth: OracleConfig,
    #[serde(rename = "processExit")]
    pub process_exit: OracleConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overrides: Vec<OracleOverride>,
    /// Any oracle name the engine does not implement lands here: `differential`, a property
    /// oracle, or a typo. Naming one is a configuration error, never a silent no-op. This is
    /// a `flatten` catch-all, which is why `OraclesConfig` does not deny unknown fields;
    /// `validate` reads every key.
    #[serde(flatten, default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub unimplemented: std::collections::BTreeMap<String, serde_json::Value>,
}

impl Default for OraclesConfig {
    /// Enabled by default: `failed` high, `deadline` 2000 ms medium, `memoryGrowth` 256 MB
    /// medium, `processExit` high. `rejected` is off and can be turned on per target.
    fn default() -> Self {
        OraclesConfig {
            failed: OracleConfig { severity: Severity::High, ms: None, mb: None },
            rejected: OracleConfig { severity: Severity::Off, ms: None, mb: None },
            deadline: OracleConfig { severity: Severity::Medium, ms: Some(2000), mb: None },
            memory_growth: OracleConfig { severity: Severity::Medium, ms: None, mb: Some(256) },
            process_exit: OracleConfig { severity: Severity::High, ms: None, mb: None },
            overrides: Vec::new(),
            unimplemented: Default::default(),
        }
    }
}

/// What bounds a scan. A scan stopped by budget still writes a complete scan result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct BudgetConfig {
    pub invocations: u64,
    pub wall_clock_ms: u64,
    /// 0 means `min(num_cpus, 8)`.
    pub workers: u32,
    pub max_corpus: u32,
    /// `guided` runs the coverage-guided loop; `random` disables corpus guidance.
    pub strategy: Strategy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    #[default]
    Guided,
    Random,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        BudgetConfig {
            invocations: 100_000,
            wall_clock_ms: 900_000,
            workers: 0,
            max_corpus: 2048,
            strategy: Strategy::Guided,
        }
    }
}

impl BudgetConfig {
    pub fn workers_or_default(&self) -> u32 {
        if self.workers > 0 {
            self.workers
        } else {
            num_cpus::get().min(8) as u32
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct OutputConfig {
    pub dir: String,
    pub renderers: Vec<String>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig { dir: ".autofuzz".to_string(), renderers: vec!["json".to_string(), "markdown".to_string()] }
    }
}

/// The scan project.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct ScanProject {
    pub project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codebase: Option<CodebaseBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeBlock>,
    pub oracles: OraclesConfig,
    pub budget: BudgetConfig,
    pub output: OutputConfig,
}

/// Where a loaded config came from.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub path: PathBuf,
    pub root: PathBuf,
    pub project: ScanProject,
}

impl LoadedConfig {
    /// `sha256:<hex>` over the config file bytes, recorded in the scan result so a replay is
    /// attributable to the exact configuration.
    pub fn config_hash(&self) -> Result<String, ConfigError> {
        let bytes = std::fs::read(&self.path).map_err(|e| ConfigError::Read { path: self.path.clone(), source: e })?;
        let digest = Sha256::digest(bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        Ok(format!("sha256:{hex}"))
    }

    /// The plugin launch for a block, resolved against the project root.
    pub fn launch_for(&self, block: Block) -> Result<Launch, ConfigError> {
        let (harness, command, env) = match block {
            Block::Codebase => {
                let codebase = self
                    .project
                    .codebase
                    .as_ref()
                    .ok_or_else(|| ConfigError::msg("this scan project has no `codebase` block"))?;
                (codebase.harness.as_str(), codebase.command.clone(), codebase.env.clone())
            }
            Block::Runtime => {
                let runtime = self
                    .project
                    .runtime
                    .as_ref()
                    .ok_or_else(|| ConfigError::msg("this scan project has no `runtime` block"))?;
                if runtime.base_url.is_none() {
                    return Err(ConfigError::msg(
                        "`runtime.baseUrl` is required: requests to a host the user did not name are refused",
                    ));
                }
                (
                    runtime.harness.as_deref().unwrap_or("http"),
                    runtime.command.clone(),
                    None,
                )
            }
        };
        let argv = match command {
            Some(argv) if !argv.is_empty() => argv,
            _ => default_command(harness).ok_or_else(|| {
                ConfigError::msg(format!(
                    "harness \"{harness}\" has no built-in command; set `command` to the plugin argv"
                ))
            })?,
        };
        let mut launch = Launch::new(argv);
        if let Some(env) = env {
            for (k, v) in env {
                launch.env.insert(k, v);
            }
        }
        launch.env.insert("AFZ_ROOT".to_string(), self.root.display().to_string());
        launch.env.insert("AFZ_CONFIG".to_string(), self.path.display().to_string());
        launch.cwd = Some(self.root.display().to_string());
        Ok(launch)
    }

    /// The declared harness name for a block, for the scan result's `target.harness`.
    pub fn harness_name(&self, block: Block) -> String {
        match block {
            Block::Codebase => self
                .project
                .codebase
                .as_ref()
                .map(|c| c.harness.clone())
                .unwrap_or_else(|| "js-ts".to_string()),
            Block::Runtime => self
                .project
                .runtime
                .as_ref()
                .and_then(|r| r.harness.clone())
                .unwrap_or_else(|| "http".to_string()),
        }
    }

    /// Discovery scope for a codebase block: root plus include and exclude globs.
    pub fn discovery_scope(&self) -> Result<(PathBuf, Option<Vec<String>>, Option<Vec<String>>), ConfigError> {
        let codebase = self
            .project
            .codebase
            .as_ref()
            .ok_or_else(|| ConfigError::msg("this scan project has no `codebase` block"))?;
        let root = match &codebase.root {
            Some(root) => self.root.join(root),
            None => self.root.clone(),
        };
        Ok((root, codebase.include.clone(), codebase.exclude.clone()))
    }
}

/// The built-in plugin command for a known harness, relative to the repository or project
/// root. An unknown harness must declare `command`.
fn default_command(harness: &str) -> Option<Vec<String>> {
    match harness {
        "js-ts" => Some(vec![
            "node".to_string(),
            "packages/harness-js/dist/server.js".to_string(),
        ]),
        _ => None,
    }
}

impl ScanProject {
    /// Reject a configuration the engine cannot honor. Every message is user-facing.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.codebase.is_none() && self.runtime.is_none() {
            return Err(ConfigError::msg(
                "at least one of `codebase` or `runtime` must be present",
            ));
        }
        if let Some(codebase) = &self.codebase {
            if codebase.harness.is_empty() {
                return Err(ConfigError::msg("`codebase.harness` must name a harness, e.g. \"js-ts\""));
            }
            // Fail at load time rather than at the first spawn: an unknown harness with no
            // declared argv cannot run, whatever the scan does later.
            if codebase.command.as_deref().unwrap_or(&[]).is_empty() && default_command(&codebase.harness).is_none() {
                return Err(ConfigError::msg(format!(
                    "harness \"{}\" has no built-in command; set `command` to the plugin argv",
                    codebase.harness
                )));
            }
        }
        if let Some(runtime) = &self.runtime {
            if runtime.base_url.as_deref().unwrap_or("").is_empty() {
                return Err(ConfigError::msg(
                    "`runtime.baseUrl` is required: requests to a host the user did not name are refused",
                ));
            }
        }
        for name in self.oracles.unimplemented.keys() {
            return Err(ConfigError::msg(format!(
                "oracle \"{name}\" is not implemented in this version"
            )));
        }
        if self.budget.invocations == 0 {
            return Err(ConfigError::msg("`budget.invocations` must be greater than 0"));
        }
        if self.budget.wall_clock_ms == 0 {
            return Err(ConfigError::msg("`budget.wallClockMs` must be greater than 0"));
        }
        if self.budget.max_corpus == 0 {
            return Err(ConfigError::msg("`budget.maxCorpus` must be greater than 0"));
        }
        if let Some(ms) = self.oracles.deadline.ms {
            if ms == 0 {
                return Err(ConfigError::msg("`oracles.deadline.ms` must be greater than 0"));
            }
        }
        for o in &self.oracles.overrides {
            if !matches!(o.treat_as.as_str(), "ok" | "rejected" | "failed") {
                return Err(ConfigError::msg(format!(
                    "`oracles.overrides[].treatAs` must be \"ok\", \"rejected\", or \"failed\", got \"{}\"",
                    o.treat_as
                )));
            }
        }
        for renderer in &self.output.renderers {
            if !matches!(renderer.as_str(), "json" | "markdown" | "sarif") {
                return Err(ConfigError::msg(format!("renderer \"{renderer}\" is not implemented in this version")));
            }
        }
        if self.output.dir.is_empty() {
            return Err(ConfigError::msg("`output.dir` must not be empty"));
        }
        Ok(())
    }
}

/// Load and validate a scan project file.
pub fn load(path: &Path) -> Result<LoadedConfig, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read { path: path.to_path_buf(), source: e })?;
    let project: ScanProject =
        serde_json::from_str(&text).map_err(|e| ConfigError::Parse { path: path.to_path_buf(), source: e })?;
    project.validate()?;
    let abs = absolute(path)?;
    let root = abs.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    Ok(LoadedConfig { path: abs, root, project })
}

/// Find `autofuzz.json`. A directory argument is searched for the file, then upward; a bare
/// `afz scan` starts at the current directory.
pub fn discover(start: &Path) -> Result<PathBuf, ConfigError> {
    let start = absolute(start).unwrap_or_else(|_| start.to_path_buf());
    let mut dir = if start.is_file() {
        return Ok(start);
    } else {
        start.clone()
    };
    loop {
        let candidate = dir.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !dir.pop() {
            return Err(ConfigError::msg(format!(
                "no {CONFIG_FILE_NAME} found from {}; pass --config <path>",
                start.display()
            )));
        }
    }
}

fn absolute(path: &Path) -> Result<PathBuf, ConfigError> {
    std::path::absolute(path).map_err(|e| ConfigError::Read { path: path.to_path_buf(), source: e })
}

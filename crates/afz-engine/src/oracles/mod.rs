//! Oracles: the only place that decides whether an outcome is a finding.
//!
//! An oracle reads one [`Outcome`] and the scan project's severity for that oracle, and nothing
//! else. It never sees a JavaScript error, a Rust `Result`, or an HTTP status, because the
//! plugin already reduced the invocation to `ok`, `rejected`, or `failed` and named the rule
//! that decided it in `via`. That name is the only input an oracle has about *why* the plugin
//! chose a channel, which is also what makes an override expressible: `oracles.overrides`
//! reclassifies by `via`.
//!
//! # Which oracle owns which `via`
//!
//! One outcome produces at most one finding per oracle, so the channels are partitioned instead
//! of each oracle matching on whatever it likes:
//!
//! | Oracle | Fires on |
//! | --- | --- |
//! | `failed` | a `failed` outcome whose `via` names the target itself breaking, such as `engine-error-outside-declared-channel` or `unhandled-rejection`, and no channel another oracle owns |
//! | `deadline` | `via: "deadline"`, or an outcome slower than `oracles.deadline.ms` |
//! | `memoryGrowth` | an `ok` outcome whose reported `memoryBytes` passes `oracles.memoryGrowth.mb` |
//! | `processExit` | a `failed` outcome whose `via` is in [`ENVIRONMENT_VIA`]: the environment died, so the target never got to answer |
//! | `rejected` | a `rejected` outcome, off by default |
//!
//! `memoryBytes` is plugin-reported and never engine-measured, so a harness that does not report
//! it simply never trips `memoryGrowth`; the engine does not guess a number by sampling the
//! process.
//!
//! Channels in [`HARNESS_VIA`] produce no finding at all. They say the harness could not build
//! or load the input, which is a property of the run rather than of the input, so the scan result
//! lists them once as run errors instead of once per invocation.

use std::borrow::Cow;
use std::time::Duration;

use afz_protocol::{FaultSignature, Outcome};

use crate::config::{OracleConfig, OraclesConfig};

/// The `via` the engine raises when a call outlives `oracles.deadline.ms`. The `deadline`
/// oracle owns it, so `failed` says nothing about it: one outcome produces one finding, and a
/// hung target is reported as a hang rather than as a crash it did not have.
pub const DEADLINE_VIA: &str = "deadline";

/// The channels that mean "the environment around the target stopped working". The target did
/// not break; the process hosting it did, so `processExit` owns them and `failed` ignores them.
pub const ENVIRONMENT_VIA: &[&str] = &["processExit", "plugin-lost", "plugin-killed", "worker-dead"];

/// The channels that mean "the harness could not deliver this input to the target". They are
/// reported as run errors, not as findings against the target.
pub const HARNESS_VIA: &[&str] = &["module-load-failed", "materialization-blocked", "malformed-value", "stub-call"];

/// The fault class an override supplies when it turns a rejection into a failure. Identity is
/// computed from the fault class, so an overridden finding must not inherit the rejection's
/// class: the same input reaching the same guard would otherwise deduplicate with a rejection
/// that was never reported.
pub const OVERRIDE_CLASS: &str = "TreatedAsFailed";

/// Used when a scan sets `oracles.deadline.severity` but leaves `ms` out. Part I's default.
pub const DEFAULT_DEADLINE_MS: u64 = 2000;

/// One oracle, named the way the configuration and the scan result name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Oracle {
    Failed,
    Deadline,
    MemoryGrowth,
    ProcessExit,
    Rejected,
}

impl Oracle {
    /// Every oracle, in the order `Oracles::evaluate` reports them.
    pub const ALL: [Oracle; 5] = [Oracle::Failed, Oracle::Deadline, Oracle::MemoryGrowth, Oracle::ProcessExit, Oracle::Rejected];

    pub fn as_str(self) -> &'static str {
        match self {
            Oracle::Failed => "failed",
            Oracle::Deadline => "deadline",
            Oracle::MemoryGrowth => "memoryGrowth",
            Oracle::ProcessExit => "processExit",
            Oracle::Rejected => "rejected",
        }
    }
}

/// One reported problem, carrying what the later steps need: minimization re-runs `outcome`'s
/// input, identity hashes the fault, and the renderer prints `summary`.
#[derive(Debug, Clone)]
pub struct Finding {
    pub oracle: Oracle,
    pub severity: crate::config::Severity,
    pub target_id: String,
    pub invocation_id: u64,
    /// The channel the outcome was classified through, after any override.
    pub via: String,
    /// The one line a triager reads before opening the input.
    pub summary: String,
    pub fault: Option<FaultSignature>,
    pub outcome: Outcome,
}

/// The oracles as one scan run uses them.
#[derive(Debug, Clone)]
pub struct Oracles {
    config: OraclesConfig,
}

impl Oracles {
    pub fn new(config: &OraclesConfig) -> Oracles {
        Oracles { config: config.clone() }
    }

    pub fn config(&self) -> &OraclesConfig {
        &self.config
    }

    /// The per-invocation deadline the supervisor should enforce. `None` when the `deadline`
    /// oracle is off, which is the engine's signal to let a call run as long as it likes.
    pub fn invocation_deadline(&self) -> Option<Duration> {
        let config = self.config_for(Oracle::Deadline);
        if !config.severity.enabled() {
            return None;
        }
        Some(Duration::from_millis(config.ms.unwrap_or(DEFAULT_DEADLINE_MS)))
    }

    pub fn config_for(&self, oracle: Oracle) -> OracleConfig {
        match oracle {
            Oracle::Failed => self.config.failed,
            Oracle::Deadline => self.config.deadline,
            Oracle::MemoryGrowth => self.config.memory_growth,
            Oracle::ProcessExit => self.config.process_exit,
            Oracle::Rejected => self.config.rejected,
        }
    }

    /// Judge one finished invocation. An empty vector means the run keeps the input in silence.
    pub fn evaluate(&self, target_id: &str, invocation_id: u64, outcome: &Outcome) -> Vec<Finding> {
        let classified = classify_with_overrides(&self.config, target_id, outcome);
        let mut findings = Vec::new();
        for oracle in Oracle::ALL {
            let config = self.config_for(oracle);
            if !config.severity.enabled() {
                continue;
            }
            if let Some(finding) = oracle.check(config, &classified, target_id, invocation_id, outcome) {
                findings.push(finding);
            }
        }
        findings
    }
}

/// The outcome as the oracles see it, after any override rewrote its kind.
struct Classified<'a> {
    kind: Kind,
    via: Option<String>,
    detail: Option<String>,
    fault: Option<Cow<'a, FaultSignature>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ok,
    Rejected,
    Failed,
}

/// Apply the first `oracles.overrides` entry that names this target (or no target) and this `via`.
fn classify_with_overrides<'a>(config: &OraclesConfig, target_id: &str, outcome: &'a Outcome) -> Classified<'a> {
    let mut classified = match outcome {
        Outcome::Ok(_) => Classified { kind: Kind::Ok, via: None, detail: None, fault: None },
        Outcome::Rejected(rejected) => Classified {
            kind: Kind::Rejected,
            via: Some(rejected.via.clone()),
            detail: Some(rejected.detail.clone()),
            fault: None,
        },
        Outcome::Failed(failed) => Classified {
            kind: Kind::Failed,
            via: Some(failed.via.clone()),
            detail: Some(format!("{}: {}", failed.fault.class, failed.fault.message)),
            fault: Some(Cow::Borrowed(&failed.fault)),
        },
    };

    // Only a classified channel can be overridden, so an `ok` outcome is never rewritten.
    let Some(via) = classified.via.clone() else { return classified };
    let Some(override_) = config
        .overrides
        .iter()
        .find(|o| o.via == via && o.target.as_deref().is_none_or(|t| t == target_id))
    else {
        return classified;
    };

    match override_.treat_as.as_str() {
        "failed" if classified.kind != Kind::Failed => {
            classified.kind = Kind::Failed;
            classified.fault = Some(Cow::Owned(FaultSignature {
                class: OVERRIDE_CLASS.to_string(),
                message: format!("{via} treated as a failure by `oracles.overrides`: {}", classified.detail.clone().unwrap_or_default()),
                frames: Vec::new(),
            }));
        }
        "rejected" if classified.kind == Kind::Failed => {
            classified.kind = Kind::Rejected;
            if let Some(fault) = classified.fault.take() {
                classified.detail = Some(format!("{}: {}", fault.class, fault.message));
            }
        }
        "ok" => {
            classified.kind = Kind::Ok;
            classified.fault = None;
        }
        // Every other request asks for the kind the outcome already has.
        _ => {}
    }
    classified
}

impl Oracle {
    /// This oracle's one decision. `None` means no finding from this oracle.
    fn check(
        self,
        config: OracleConfig,
        classified: &Classified<'_>,
        target_id: &str,
        invocation_id: u64,
        outcome: &Outcome,
    ) -> Option<Finding> {
        let via = classified.via.clone().unwrap_or_default();
        let (summary, fault) = match self {
            Oracle::Failed => {
                if classified.kind != Kind::Failed
                    || via == DEADLINE_VIA
                    || ENVIRONMENT_VIA.contains(&via.as_str())
                    || HARNESS_VIA.contains(&via.as_str())
                {
                    return None;
                }
                let fault = classified.fault.as_ref()?;
                (format!("{}: {}", fault.class, fault.message), Some(fault.as_ref()))
            }
            Oracle::Deadline => {
                if via == DEADLINE_VIA {
                    let detail = classified.detail.clone().unwrap_or_else(|| "no answer".to_string());
                    (format!("exceeded the deadline ({detail})"), classified.fault.as_deref())
                } else {
                    let limit = config.ms? as f64;
                    if outcome.duration_ms() < limit {
                        return None;
                    }
                    (format!("took {:.0} ms against a {limit:.0} ms deadline", outcome.duration_ms()), None)
                }
            }
            Oracle::MemoryGrowth => {
                let Outcome::Ok(ok) = outcome else { return None };
                let bytes = ok.memory_bytes?;
                let limit = config.mb?.saturating_mul(1024 * 1024);
                if bytes < limit {
                    return None;
                }
                (format!("allocated {} MB against a {} MB oracle", bytes / (1024 * 1024), config.mb?), None)
            }
            Oracle::ProcessExit => {
                if classified.kind != Kind::Failed || !ENVIRONMENT_VIA.contains(&via.as_str()) {
                    return None;
                }
                let detail = classified.detail.clone().unwrap_or_else(|| "plugin process is gone".to_string());
                (format!("the plugin process stopped during the call ({detail})"), classified.fault.as_deref())
            }
            Oracle::Rejected => {
                if classified.kind != Kind::Rejected {
                    return None;
                }
                (classified.detail.clone().unwrap_or_else(|| "rejected".to_string()), None)
            }
        };
        Some(Finding {
            oracle: self,
            severity: config.severity,
            target_id: target_id.to_string(),
            invocation_id,
            via,
            summary,
            fault: fault.cloned(),
            outcome: outcome.clone(),
        })
    }
}

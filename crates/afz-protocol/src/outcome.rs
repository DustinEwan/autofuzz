//! `Outcome` is the language-neutral seam. A harness plugin reduces one invocation to
//! exactly one of `ok`, `rejected`, or `failed`, and every oracle operates on that value
//! alone. Nothing in the engine inspects a JavaScript error, a Rust `Result`, a Go `error`,
//! or an HTTP status class.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::value::ValueTree;

/// A stable edge identifier produced by a coverage digest.
pub type EdgeId = u32;

/// `ok`: the target accepted the input and returned normally.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct OkOutcome {
    pub duration_ms: f64,
    /// Plugin-reported, never engine-measured: the JS plugin reports the `heapUsed` delta
    /// around the call, the wasm harness reports the guest memory high-water mark.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<EdgeId>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<ValueTree>,
}

/// `rejected`: the target examined the input, decided it was not acceptable, and said so
/// through the channel its own signature declares. `via` names the rule that decided it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RejectedOutcome {
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<EdgeId>>,
    pub via: String,
    pub detail: String,
}

/// `failed`: the target tried to accept the input and broke.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FailedOutcome {
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coverage: Option<Vec<EdgeId>>,
    pub via: String,
    pub fault: FaultSignature,
}

/// The fault a `failed` outcome carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FaultSignature {
    pub class: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<FaultFrame>,
}

impl FaultSignature {
    /// The frame identity is deduplicated on. A fault with no frames deduplicates on the
    /// class alone.
    pub fn top_frame(&self) -> Option<&FaultFrame> {
        self.frames.first()
    }
}

/// One stack frame inside a fault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct FaultFrame {
    pub file: String,
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fn_name: Option<String>,
}

impl Default for FaultFrame {
    fn default() -> Self {
        FaultFrame { file: String::new(), line: 0, column: None, fn_name: None }
    }
}

/// The three outcomes. Exactly one is produced per invocation, by the plugin, always.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Outcome {
    Ok(OkOutcome),
    Rejected(RejectedOutcome),
    Failed(FailedOutcome),
}

impl Outcome {
    pub fn kind(&self) -> &'static str {
        match self {
            Outcome::Ok(_) => "ok",
            Outcome::Rejected(_) => "rejected",
            Outcome::Failed(_) => "failed",
        }
    }

    pub fn duration_ms(&self) -> f64 {
        match self {
            Outcome::Ok(o) => o.duration_ms,
            Outcome::Rejected(o) => o.duration_ms,
            Outcome::Failed(o) => o.duration_ms,
        }
    }

    pub fn coverage(&self) -> Option<&[EdgeId]> {
        match self {
            Outcome::Ok(o) => o.coverage.as_deref(),
            Outcome::Rejected(o) => o.coverage.as_deref(),
            Outcome::Failed(o) => o.coverage.as_deref(),
        }
    }

    pub fn via(&self) -> Option<&str> {
        match self {
            Outcome::Ok(_) => None,
            Outcome::Rejected(o) => Some(&o.via),
            Outcome::Failed(o) => Some(&o.via),
        }
    }

    pub fn fault(&self) -> Option<&FaultSignature> {
        match self {
            Outcome::Failed(o) => Some(&o.fault),
            _ => None,
        }
    }

    /// The outcome the engine substitutes when the plugin process is gone and no response
    /// arrived. A lost plugin is a fault of the run, never an engine error.
    pub fn plugin_lost(duration_ms: f64, detail: impl Into<String>) -> Self {
        Outcome::Failed(FailedOutcome {
            duration_ms,
            coverage: None,
            via: "plugin-lost".to_string(),
            fault: FaultSignature {
                class: "PluginLost".to_string(),
                message: detail.into(),
                frames: Vec::new(),
            },
        })
    }
}

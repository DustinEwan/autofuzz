//! Per-method request and response payloads. The method names live in [`crate::rpc::Method`];
//! these are the shapes behind them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::outcome::Outcome;
use crate::targets::{CoverageMap, Discovery, Handshake, SkipReason};
use crate::outcome::FaultSignature;
use crate::value::{MaterializedCall, ValueTree};

/// `discover` request. Discovery must not execute target code unless the handshake said
/// it has to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct DiscoverReq {
    pub root: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Path to the scan project file, so a plugin can read harness-specific settings such
    /// as `fixtures` and `rejectClasses` without the engine interpreting them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
}

/// `discover` response.
pub type DiscoverResp = Discovery;

/// `prepare` request: build the receiver plus arguments for one call, or report why that
/// is impossible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PrepareReq {
    pub target_id: String,
    pub values: Vec<ValueTree>,
}

/// `prepare` response. The wire form is `{"ok": true, "materialized": …}` or
/// `{"ok": false, "reason": …}`: `ok` is a JSON boolean, which serde's internally-tagged
/// enums cannot produce, so the impls below are hand-written.
#[derive(Debug, Clone, PartialEq)]
pub enum PrepareResp {
    Ok { materialized: MaterializedCall },
    Skip { reason: SkipReason },
}

impl PrepareResp {
    pub fn ok(&self) -> bool {
        matches!(self, PrepareResp::Ok { .. })
    }
}

impl Serialize for PrepareResp {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap as _;
        let mut map = s.serialize_map(Some(2))?;
        match self {
            PrepareResp::Ok { materialized } => {
                map.serialize_entry("ok", &true)?;
                map.serialize_entry("materialized", materialized)?;
            }
            PrepareResp::Skip { reason } => {
                map.serialize_entry("ok", &false)?;
                map.serialize_entry("reason", reason)?;
            }
        }
        map.end()
    }
}

/// The intermediate shape: `ok` decides which payload is required.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareRespWire {
    ok: bool,
    #[serde(default)]
    materialized: Option<MaterializedCall>,
    #[serde(default)]
    reason: Option<SkipReason>,
}

impl<'de> Deserialize<'de> for PrepareResp {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let wire = PrepareRespWire::deserialize(d)?;
        match (wire.ok, wire.materialized, wire.reason) {
            (true, Some(materialized), None) => Ok(PrepareResp::Ok { materialized }),
            (false, None, Some(reason)) => Ok(PrepareResp::Skip { reason }),
            (true, None, _) => Err(serde::de::Error::custom("`ok: true` requires `materialized`")),
            (true, _, Some(_)) => {
                Err(serde::de::Error::custom("`ok: true` must not carry `reason`"))
            }
            (false, Some(_), _) => Err(serde::de::Error::custom("`ok: false` must not carry `materialized`")),
            (false, None, None) => Err(serde::de::Error::custom("`ok: false` requires `reason`")),
        }
    }
}

/// `ok` is a boolean on the wire, so the generated schema states the two shapes directly.
impl JsonSchema for PrepareResp {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("PrepareResp")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::PrepareResp"))
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let ok = serde_json::json!({
            "type": "object",
            "properties": {
                "ok": {"const": true, "type": "boolean"},
                "materialized": generator.subschema_for::<MaterializedCall>()
            },
            "required": ["ok", "materialized"],
            "additionalProperties": false
        });
        let skip = serde_json::json!({
            "type": "object",
            "properties": {
                "ok": {"const": false, "type": "boolean"},
                "reason": generator.subschema_for::<SkipReason>()
            },
            "required": ["ok", "reason"],
            "additionalProperties": false
        });
        serde_json::from_value(serde_json::json!({ "oneOf": [ok, skip] })).expect("static schema")
    }
}

/// `invoke` request. One invocation. A plugin never throws over the wire: faults are
/// `Outcome` values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InvokeReq {
    /// Assigned by the engine, echoed on the outcome, so out-of-order responses from
    /// parallel plugin processes stay attributable.
    pub invocation_id: u64,
    pub target_id: String,
    pub values: Vec<ValueTree>,
    pub deadline_ms: u64,
    pub collect_coverage: bool,
}

/// `invoke` response.
pub type InvokeResp = Outcome;

/// `reproducer` request: render a standalone script that replays the call with these values.
/// The engine has minimized the input and confirmed it reproduces; the plugin, which owns the
/// target language, renders the call in it. The engine stores the script verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReproducerReq {
    pub target_id: String,
    pub values: Vec<ValueTree>,
}

/// `reproducer` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReproducerResp {
    /// File extension for the artifact, without a dot: `ts`, `py`. The engine accepts one to
    /// eight ASCII alphanumeric characters and rejects anything else, so a plugin cannot name
    /// a path outside `repros/`.
    pub extension: String,
    /// The complete script. The engine writes these bytes untouched.
    pub script: String,
}

/// `minimize` request: shrink a failing input while it still reproduces the same fault.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MinimizeReq {
    pub target_id: String,
    pub values: Vec<ValueTree>,
    pub fault: FaultSignature,
    pub budget_ms: u64,
}

/// `minimize` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MinimizeResp {
    pub values: Vec<ValueTree>,
    pub steps: u32,
}

/// `handshake` request has no parameters; the response is the handshake itself.
pub type HandshakeResp = Handshake;

/// `coverageMap` request has no parameters.
pub type CoverageMapResp = CoverageMap;

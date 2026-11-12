//! The inventory side of the contract: what a plugin reports it can do, and what it found.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::TypeNode;

pub const PROTOCOL_VERSION: &str = "afz/1";

/// The two target classes. Design-time targets are symbols we call in a process we control;
/// runtime targets are boundaries a live process exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum TargetClass {
    DesignTime,
    Runtime,
}

impl TargetClass {
    /// The kebab-case spelling this enum serializes to, for log lines.
    pub fn as_str(self) -> &'static str {
        match self {
            TargetClass::DesignTime => "design-time",
            TargetClass::Runtime => "runtime",
        }
    }
}

/// What a plugin declares at handshake. The engine adapts instead of assuming, and an
/// absent capability has a defined absent behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    /// Per-invocation coverage digest is available. Absent: the scheduler uses
    /// schema-directed generation and outcome novelty only.
    Coverage,
    /// The plugin can render a standalone reproducer script in the target language. Absent:
    /// findings carry no script, and the `inputs/` JSON stays the replayable artifact.
    Reproducer,
    /// The plugin can build receiver state. Absent: only entry points with fully
    /// constructible parameter graphs are fuzzed.
    Stateful,
    /// The plugin can shrink an input in target terms. Absent: the engine minimizes by
    /// removing arguments and reducing structure only.
    Minimize,
    /// Invocations may change external state. Present: the side-effect policy applies.
    SideEffects,
    /// The plugin tolerates parallel instances. Absent: the engine runs one instance.
    Concurrent,
}

/// `handshake` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Handshake {
    pub protocol_version: String,
    pub name: String,
    pub version: String,
    pub target_class: TargetClass,
    pub capabilities: Vec<Capability>,
    pub languages: Vec<String>,
    /// Set when enumerating targets requires importing the target. The engine then runs
    /// that discovery under the same isolation and deadline it applies to invocations.
    #[serde(default)]
    pub discover_requires_execution: bool,
}

/// Where a target came from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Origin {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpOrigin>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HttpOrigin {
    pub method: String,
    pub path: String,
}

/// Effects a target can have, recorded during discovery, never guessed at invoke time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Effects {
    pub async_: bool,
    pub throws: bool,
    pub streams: bool,
    pub callbacks: bool,
    pub mutates_state: bool,
}

/// The honest accounting of what the tool can actually reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Materialization {
    /// The whole parameter graph is buildable from generated values.
    Constructible,
    /// A user-supplied fixture can fill the gap.
    FixtureRequired,
    /// Unreachable without a live system.
    Opaque,
}

/// One callable, normalized. Every plugin produces this shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TargetDescriptor {
    /// Stable content hash of module path + symbol + signature.
    pub id: String,
    pub target_class: TargetClass,
    pub origin: Origin,
    /// Human-facing name: `parseConfig`, `POST /v1/tokens`.
    pub name: String,
    pub parameters: Vec<Parameter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub returns: Option<TypeNode>,
    pub effects: Effects,
    /// Source lines inside this target that hold a `throw` the target author wrote. The
    /// classification rule reads this list; the engine never sees it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guard_sites: Vec<u32>,
    pub materialization: Materialization,
}

impl TargetDescriptor {
    pub fn is_fuzzable(&self) -> bool {
        self.materialization == Materialization::Constructible
    }
}

/// One parameter of a target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Parameter {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeNode,
    pub required: bool,
}

/// Why a parameter graph cannot be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    /// A parameter or receiver field has no constructible representation.
    UnconstructibleParameter,
    /// Discovery could not analyze the declaring file.
    DiscoveryError,
    /// The symbol is not public.
    NotPublic,
    /// The scan project excluded it.
    Excluded,
    /// The plugin cannot call it: varargs-only, generator, or accessor.
    UnsupportedSignature,
    /// The engine disabled the target after repeated plugin losses.
    TargetDisabled,
}

impl SkipReason {
    /// The kebab-case spelling this enum serializes to, for log lines.
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::UnconstructibleParameter => "unconstructible-parameter",
            SkipReason::DiscoveryError => "discovery-error",
            SkipReason::NotPublic => "not-public",
            SkipReason::Excluded => "excluded",
            SkipReason::UnsupportedSignature => "unsupported-signature",
            SkipReason::TargetDisabled => "target-disabled",
        }
    }
}

/// The type that blocks a target, and where it sits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Blocker {
    pub type_: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Parameter or receiver path, e.g. `receiver.db`.
    pub path: String,
}

/// One non-fuzzed symbol, with a reason. Reported, never silently dropped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkippedTarget {
    pub target_id: String,
    pub name: String,
    pub reason: SkipReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_by: Option<Blocker>,
    /// Entry points that share the blocker, which turns one missing type into one task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unlocks_targets: Option<u32>,
    /// How far the plugin got in building a call for this symbol. The scan result's
    /// `materializationCounts` reads it: a symbol skipped for a reason that is not about
    /// materialization carries nothing, and is listed without being counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<Materialization>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// `discover` response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub targets: Vec<TargetDescriptor>,
    /// Every non-fuzzed symbol, with a reason.
    pub skipped: Vec<SkippedTarget>,
}

/// One entry of `coverageMap`: a stable edge id and the source range it stands for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoverageEntry {
    pub edge: EdgeIdAlias,
    pub url: String,
    pub start: u64,
    pub end: u64,
}

/// Re-export so `coverageMap` types read without an import cycle.
pub type EdgeIdAlias = crate::outcome::EdgeId;

/// `coverageMap` response: the mapping from digest ids to source ranges.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CoverageMap {
    pub entries: Vec<CoverageEntry>,
}

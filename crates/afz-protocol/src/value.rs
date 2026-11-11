//! `ValueTree`: the structured input the generator produces, the mutator rewrites, the
//! minimizer shrinks, and the plugin materializes.
//!
//! Every node carries the type it was generated against and the provenance that made it,
//! which is what lets mutation and minimization work in target terms instead of bytes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why a node exists. `Provenance::Boundary` marks a value drawn from the shared boundary
/// table, which is how a triager can tell a boundary hit from an arbitrary value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "provenance", rename_all = "camelCase")]
pub enum Provenance {
    Generated,
    Boundary,
    Mutated { op: String, from: String },
    Transplanted { from_entry: String },
    Minimized { step: u32 },
    User,
}

/// A number that survives JSON round-trip exactly: finite values as `f64`, everything else
/// as a name. `NaN`, `Infinity`, and `-Infinity` have no JSON number token, so they travel
/// as strings and the plugin re-materializes them.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum NumberValue {
    Finite(f64),
    Special(SpecialNumber),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SpecialNumber {
    NaN,
    Infinity,
    NegativeInfinity,
}

impl NumberValue {
    pub fn as_f64(&self) -> f64 {
        match self {
            NumberValue::Finite(v) => *v,
            NumberValue::Special(SpecialNumber::NaN) => f64::NAN,
            NumberValue::Special(SpecialNumber::Infinity) => f64::INFINITY,
            NumberValue::Special(SpecialNumber::NegativeInfinity) => f64::NEG_INFINITY,
        }
    }
}

/// One node of a structured input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "node", rename_all = "camelCase")]
pub enum ValueTree {
    Null,
    Undefined,
    Boolean { value: bool },
    Number { value: NumberValue },
    BigInt { value: String },
    String { value: String },
    Literal { value: serde_json::Value },
    Array { items: Vec<ValueTree> },
    Set { items: Vec<ValueTree> },
    Map { entries: Vec<(ValueTree, ValueTree)> },
    Object { fields: Vec<(String, ValueTree)> },
    Tuple { items: Vec<ValueTree> },
    Buffer { bytes: Vec<u8> },
    /// A callable stub. `behavior` selects what the stub does when the target calls it.
    Function {
        behavior: crate::types::FunctionBehavior,
        returns: Option<Box<ValueTree>>,
    },
    /// A stream or iterable source, generated as an in-memory item list.
    Stream { iterable: bool, items: Vec<ValueTree> },
    /// A node the plugin must supply from a fixture. The engine never invents one.
    Opaque { ty: String },
}

impl ValueTree {
    pub fn null() -> Self {
        ValueTree::Null
    }

    pub fn string(v: impl Into<String>) -> Self {
        ValueTree::String { value: v.into() }
    }

    pub fn number(v: f64) -> Self {
        ValueTree::Number {
            value: if v.is_finite() {
                NumberValue::Finite(v)
            } else if v.is_nan() {
                NumberValue::Special(SpecialNumber::NaN)
            } else if v > 0.0 {
                NumberValue::Special(SpecialNumber::Infinity)
            } else {
                NumberValue::Special(SpecialNumber::NegativeInfinity)
            },
        }
    }

    pub fn bool(v: bool) -> Self {
        ValueTree::Boolean { value: v }
    }

    /// Rough serialized size, used for `bytesBefore`/`bytesAfter` in a finding.
    pub fn wire_bytes(&self) -> usize {
        serde_json::to_string(self).map(|s| s.len()).unwrap_or(0)
    }
}

/// A complete call input: the argument list, plus the receiver the plugin should build
/// state on when the target is a method.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallInput {
    pub arguments: Vec<ValueTree>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver: Option<ValueTree>,
}

/// What the plugin built for one call. Present only when `prepare` reports `ok`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaterializedCall {
    /// Plugin-side handle naming the built receiver and arguments. `invoke` may reference
    /// it so the plugin does not rebuild state it already built during `prepare`.
    pub handle: String,
    pub summary: String,
}

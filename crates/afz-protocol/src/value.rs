//! `ValueTree`: the structured input the generator produces, the mutator rewrites, the
//! minimizer shrinks, and the plugin materializes.
//!
//! A `ValueTree` is a [`ValueNode`] plus the provenance that made it. Provenance travels with
//! every node, which is what lets mutation and minimization reason about a value in target
//! terms instead of bytes, and lets a triager tell a boundary hit from an arbitrary value.
//!
//! The wire form is flat: the node's own tag and fields sit beside `provenance`, so
//! `{"node":"string","value":"x","provenance":"generated"}`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::types::FunctionBehavior;

/// Why a node exists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Provenance {
    /// Drawn from the type by the generator.
    #[default]
    Generated,
    /// Drawn from the shared boundary table in `crates/afz-engine/src/boundaries.rs`.
    Boundary,
    /// Rewritten by one mutation operator. `op` names it, `from` names the target or corpus
    /// entry the operation is attributed to.
    Mutated { op: String, from: String },
    /// Copied whole from another corpus entry.
    ///
    /// Renamed explicitly: serde's container-level `rename_all` covers variant names, not the
    /// fields of a struct variant, and the wire format is camelCase everywhere.
    Transplanted { #[serde(rename = "fromEntry")] from_entry: String },
    /// Produced by the minimizer at this step count.
    Minimized { step: u32 },
    /// Supplied by a user fixture.
    User,
}

/// A number that survives JSON round-trip exactly: finite values as `f64`, everything else as
/// a name. JSON has no token for `NaN` or `Infinity`, so they travel as names and the plugin
/// re-materializes them.
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

    pub fn from_f64(v: f64) -> NumberValue {
        if v.is_nan() {
            NumberValue::Special(SpecialNumber::NaN)
        } else if v == f64::INFINITY {
            NumberValue::Special(SpecialNumber::Infinity)
        } else if v == f64::NEG_INFINITY {
            NumberValue::Special(SpecialNumber::NegativeInfinity)
        } else {
            NumberValue::Finite(v)
        }
    }
}

/// The shape of one node. The variants mirror [`crate::Kind`], so a plugin can materialize a
/// node by reading its tag alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "node", rename_all = "camelCase")]
pub enum ValueNode {
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
        behavior: FunctionBehavior,
        #[serde(skip_serializing_if = "Option::is_none")]
        returns: Option<Box<ValueTree>>,
    },
    /// A stream or iterable source, generated as an in-memory item list.
    Stream { iterable: bool, items: Vec<ValueTree> },
    /// A node the plugin must supply from a fixture. The engine never invents one.
    Opaque { ty: String },
}

impl ValueNode {
    /// A number node from an `f64`, mapping the non-finite values to their names.
    pub fn number(v: f64) -> ValueNode {
        ValueNode::Number { value: NumberValue::from_f64(v) }
    }

    /// A string node.
    pub fn string(value: impl Into<String>) -> ValueNode {
        ValueNode::String { value: value.into() }
    }

    /// The [`crate::types::Kind`] this node's tag corresponds to, so a mutator can check a
    /// node against the type it was generated for.
    pub fn kind(&self) -> crate::types::Kind {
        use crate::types::Kind;
        match self {
            ValueNode::Null
            | ValueNode::Undefined
            | ValueNode::Boolean { .. }
            | ValueNode::Number { .. }
            | ValueNode::BigInt { .. }
            | ValueNode::String { .. } => Kind::Scalar,
            ValueNode::Literal { .. } => Kind::Literal,
            ValueNode::Array { .. } | ValueNode::Set { .. } | ValueNode::Map { .. } => Kind::Collection,
            ValueNode::Object { .. } => Kind::Object,
            ValueNode::Tuple { .. } => Kind::Tuple,
            ValueNode::Buffer { .. } => Kind::Buffer,
            ValueNode::Function { .. } => Kind::Function,
            ValueNode::Stream { .. } => Kind::Stream,
            ValueNode::Opaque { .. } => Kind::Opaque,
        }
    }
}

/// A node plus the provenance that made it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ValueTree {
    #[serde(flatten)]
    pub node: ValueNode,
    #[serde(default)]
    pub provenance: Provenance,
}

impl ValueTree {
    pub fn new(node: ValueNode) -> ValueTree {
        ValueTree { node, provenance: Provenance::Generated }
    }

    pub fn with_provenance(node: ValueNode, provenance: Provenance) -> ValueTree {
        ValueTree { node, provenance }
    }

    pub fn null() -> ValueTree {
        ValueTree::new(ValueNode::Null)
    }

    pub fn undefined() -> ValueTree {
        ValueTree::new(ValueNode::Undefined)
    }

    pub fn bool(value: bool) -> ValueTree {
        ValueTree::new(ValueNode::Boolean { value })
    }

    pub fn number(v: f64) -> ValueTree {
        ValueTree::new(ValueNode::Number { value: NumberValue::from_f64(v) })
    }

    pub fn string(value: impl Into<String>) -> ValueTree {
        ValueTree::new(ValueNode::String { value: value.into() })
    }

    pub fn boundary(node: ValueNode) -> ValueTree {
        ValueTree::with_provenance(node, Provenance::Boundary)
    }

    /// Rough serialized size, used for `bytesBefore` and `bytesAfter` in a finding.
    pub fn wire_bytes(&self) -> usize {
        serde_json::to_string(self).map(|s| s.len()).unwrap_or(0)
    }

    /// Every node below and including this one, in a deterministic pre-order. The mutator and
    /// the minimizer address nodes through this list rather than through an index scheme of
    /// their own.
    pub fn paths(&self) -> Vec<NodePath> {
        let mut out = Vec::new();
        self.collect_paths(&mut Vec::new(), &mut out);
        out
    }

    fn collect_paths(&self, trail: &mut Vec<Step>, out: &mut Vec<NodePath>) {
        out.push(NodePath { steps: trail.clone() });
        match &self.node {
            ValueNode::Array { items } | ValueNode::Set { items } | ValueNode::Tuple { items } | ValueNode::Stream { items, .. } => {
                for (i, item) in items.iter().enumerate() {
                    trail.push(Step::Index(i));
                    item.collect_paths(trail, out);
                    trail.pop();
                }
            }
            ValueNode::Map { entries } => {
                for (i, (k, v)) in entries.iter().enumerate() {
                    trail.push(Step::Key(i, KeySide::Key));
                    k.collect_paths(trail, out);
                    trail.pop();
                    trail.push(Step::Key(i, KeySide::Value));
                    v.collect_paths(trail, out);
                    trail.pop();
                }
            }
            ValueNode::Object { fields } => {
                for (name, value) in fields {
                    trail.push(Step::Field(name.clone()));
                    value.collect_paths(trail, out);
                    trail.pop();
                }
            }
            ValueNode::Function { returns: Some(inner), .. } => {
                trail.push(Step::Return);
                inner.collect_paths(trail, out);
                trail.pop();
            }
            _ => {}
        }
    }
}

/// One edge on the way to a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Index(usize),
    Key(usize, KeySide),
    Field(String),
    Return,
}

/// Which half of a map entry a path goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySide {
    Key,
    Value,
}

/// The address of a node inside a `ValueTree`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NodePath {
    pub steps: Vec<Step>,
}

impl NodePath {
    pub fn root() -> NodePath {
        NodePath { steps: Vec::new() }
    }

    /// The node this path addresses, or `None` when the path does not exist.
    pub fn get<'a>(&self, root: &'a ValueTree) -> Option<&'a ValueTree> {
        let mut node = root;
        for step in &self.steps {
            node = match (step, &node.node) {
                (Step::Index(i), ValueNode::Array { items } | ValueNode::Set { items } | ValueNode::Tuple { items } | ValueNode::Stream { items, .. }) => {
                    items.get(*i)?
                }
                (Step::Key(i, side), ValueNode::Map { entries }) => {
                    let (k, v) = entries.get(*i)?;
                    match side {
                        KeySide::Key => k,
                        KeySide::Value => v,
                    }
                }
                (Step::Field(name), ValueNode::Object { fields }) => {
                    fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)?
                }
                (Step::Return, ValueNode::Function { returns: Some(inner), .. }) => inner,
                _ => return None,
            };
        }
        Some(node)
    }

    /// Replace the node this path addresses. Used by mutation and minimization.
    pub fn replace(&self, root: &mut ValueTree, next: ValueTree) -> bool {
        let mut steps = self.steps.iter().peekable();
        Self::replace_inner(&mut steps, root, next)
    }

    fn replace_inner<'a>(
        steps: &mut std::iter::Peekable<std::slice::Iter<'a, Step>>,
        node: &mut ValueTree,
        next: ValueTree,
    ) -> bool {
        let Some(step) = steps.next() else {
            *node = next;
            return true;
        };
        match (step.clone(), &mut node.node) {
            (Step::Index(i), ValueNode::Array { items } | ValueNode::Set { items } | ValueNode::Tuple { items } | ValueNode::Stream { items, .. }) => {
                match items.get_mut(i) {
                    Some(child) => Self::replace_inner(steps, child, next),
                    None => false,
                }
            }
            (Step::Key(i, side), ValueNode::Map { entries }) => match entries.get_mut(i) {
                Some((k, v)) => match side {
                    KeySide::Key => Self::replace_inner(steps, k, next),
                    KeySide::Value => Self::replace_inner(steps, v, next),
                },
                None => false,
            },
            (Step::Field(name), ValueNode::Object { fields }) => {
                match fields.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, child)) => Self::replace_inner(steps, child, next),
                    None => false,
                }
            }
            (Step::Return, ValueNode::Function { returns: Some(inner), .. }) => {
                Self::replace_inner(steps, inner, next)
            }
            _ => false,
        }
    }
}

/// A complete call input: the argument list, plus the receiver the plugin should build state
/// on when the target is a method.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallInput {
    pub arguments: Vec<ValueTree>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<ValueTree>,
}

/// What the plugin built for one call. Present only when `prepare` reports `ok`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MaterializedCall {
    /// Plugin-side handle naming the built receiver and arguments. `invoke` may reference it
    /// so the plugin does not rebuild state it already built during `prepare`.
    pub handle: String,
    pub summary: String,
}

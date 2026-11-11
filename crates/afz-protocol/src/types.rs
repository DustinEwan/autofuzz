//! `TypeNode`: the generator's vocabulary, and the normalized type form every harness
//! plugin lowers its language's types into.
//!
//! The thirteen wire kinds map one-to-one onto the rows of the type table in
//! `docs/architecture.md`: `scalar` covers `null`, `undefined`, `boolean`, `number`,
//! `bigint`, and `string`; `collection` covers `array`, `set`, and `map`;
//! `composite` covers `union` and `intersection`; `stream` covers `stream` and `iterable`.
//! The sub-selector travels in the variant payload, so no design vocabulary word is lost.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Scalar sub-kinds carried by [`Kind::Scalar`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ScalarKind {
    Null,
    Undefined,
    Boolean,
    Number,
    BigInt,
    String,
}

/// Collection sub-kinds carried by [`Kind::Collection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CollectionKind {
    Array,
    Set,
    Map,
}

/// Composite sub-kinds carried by [`Kind::Composite`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CompositeKind {
    Union,
    Intersection,
}

/// The thirteen wire kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Scalar,
    StringFormat,
    Literal,
    Collection,
    Object,
    Tuple,
    Composite,
    Buffer,
    Promise,
    Function,
    Stream,
    Opaque,
    Ref,
}

/// Declared numeric bounds, present only when the source declares them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct NumberBounds {
    pub int: bool,
    pub finite_only: bool,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// Declared length bounds for a collection, a string, or a buffer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct LengthBounds {
    pub min: Option<u64>,
    pub max: Option<u64>,
}

/// One named field of an object type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Field {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeNode,
    pub required: bool,
}

/// A `function` node's configurable stub behavior. The generator emits a callable; this
/// says what the callable does when the target calls it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum FunctionBehavior {
    #[default]
    ReturnFirst,
    Throw,
    CaptureArguments,
}

/// A named field of a `ref` node: the receiver property or parameter path that a
/// recursive reference re-enters through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeNode,
}

/// A normalized type. Every plugin produces this shape; the generator consumes nothing else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TypeNode {
    /// `null`, `undefined`, `boolean`, `number`, `bigint`, `string`.
    Scalar {
        scalar: ScalarKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bounds: Option<NumberBounds>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<LengthBounds>,
    },
    /// `email`, `uuid`, `date-time`, `hostname`, `ipv4`, `uri`, plus plugin-defined formats.
    StringFormat { format: String },
    /// Enum members and string unions collapse to a literal set.
    Literal { values: Vec<serde_json::Value> },
    /// `array`, `set`, `map`. A map carries `key` and `element`; the others carry `element`.
    Collection {
        collection: CollectionKind,
        #[serde(rename = "type")]
        element: Box<TypeNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<Box<TypeNode>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<LengthBounds>,
    },
    Object {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        fields: Vec<Field>,
        /// Set when the object type is recursive; children may carry [`Kind::Ref`] back to it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },
    Tuple {
        items: Vec<TypeNode>,
    },
    /// `union` or `intersection`. A union with a literal discriminant field generates
    /// discriminant-aware members.
    Composite {
        composite: CompositeKind,
        members: Vec<TypeNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        discriminant: Option<String>,
    },
    /// `Uint8Array`, `ArrayBuffer`, `Buffer`.
    Buffer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<LengthBounds>,
    },
    /// Marks an async effect. The harness awaits it under the invocation deadline.
    Promise {
        #[serde(rename = "type")]
        inner: Box<TypeNode>,
    },
    /// A callable parameter, generated as a stub.
    Function {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        behavior: Option<FunctionBehavior>,
        #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
        returns: Option<Box<TypeNode>>,
    },
    /// `stream` or `iterable`, generated as an in-memory source with a bounded item count.
    Stream {
        iterable: bool,
        #[serde(rename = "type")]
        element: Box<TypeNode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_items: Option<u64>,
    },
    /// A type the plugin cannot build: class instance, DOM node, socket. Drives
    /// [`crate::TargetDescriptor::materialization`].
    Opaque {
        ty: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        module: Option<String>,
    },
    /// A named recursive reference. Generation enforces a per-path depth budget.
    Ref {
        name: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fields: Vec<RefField>,
    },
}

impl TypeNode {
    pub fn kind(&self) -> Kind {
        match self {
            TypeNode::Scalar { .. } => Kind::Scalar,
            TypeNode::StringFormat { .. } => Kind::StringFormat,
            TypeNode::Literal { .. } => Kind::Literal,
            TypeNode::Collection { .. } => Kind::Collection,
            TypeNode::Object { .. } => Kind::Object,
            TypeNode::Tuple { .. } => Kind::Tuple,
            TypeNode::Composite { .. } => Kind::Composite,
            TypeNode::Buffer { .. } => Kind::Buffer,
            TypeNode::Promise { .. } => Kind::Promise,
            TypeNode::Function { .. } => Kind::Function,
            TypeNode::Stream { .. } => Kind::Stream,
            TypeNode::Opaque { .. } => Kind::Opaque,
            TypeNode::Ref { .. } => Kind::Ref,
        }
    }

    pub fn null() -> Self {
        TypeNode::Scalar { scalar: ScalarKind::Null, bounds: None, length: None }
    }

    pub fn boolean() -> Self {
        TypeNode::Scalar { scalar: ScalarKind::Boolean, bounds: None, length: None }
    }

    pub fn number() -> Self {
        TypeNode::Scalar {
            scalar: ScalarKind::Number,
            bounds: Some(NumberBounds::default()),
            length: None,
        }
    }

    pub fn string() -> Self {
        TypeNode::Scalar {
            scalar: ScalarKind::String,
            bounds: None,
            length: Some(LengthBounds::default()),
        }
    }

    pub fn array(element: TypeNode) -> Self {
        TypeNode::Collection {
            collection: CollectionKind::Array,
            element: Box::new(element),
            key: None,
            length: Some(LengthBounds::default()),
        }
    }

    pub fn promise(inner: TypeNode) -> Self {
        TypeNode::Promise { inner: Box::new(inner) }
    }
}

//! Value generation: `TypeNode` in, `ValueTree` out.
//!
//! Two rules hold everywhere here. Every draw comes from [`Rng`], so a seed reproduces a tree
//! exactly. And the depth budget plus the per-path `ref` counter terminate recursion, so a
//! recursive type cannot make generation loop.

use std::collections::BTreeMap;

use afz_protocol::{
    CollectionKind, CompositeKind, Field, FunctionBehavior, Kind, LengthBounds, NumberBounds,
    Parameter, RefField, ScalarKind, TypeNode, ValueNode, ValueTree,
};

use crate::boundaries;
use crate::rng::Rng;

/// Default depth budget. A tree generated with this budget nests at most this many type nodes
/// deep, which bounds both the artifact size and the materialization cost in the plugin.
pub const DEFAULT_DEPTH: u32 = 6;

/// Longest string the generator invents when the type declares no bound.
const DEFAULT_STRING_MAX: u64 = 32;
/// Longest collection the generator invents when the type declares no bound.
const DEFAULT_LENGTH_MAX: u64 = 8;
/// Items in a generated stream or iterable.
const DEFAULT_STREAM_MAX: u64 = 4;
/// Longest buffer the generator invents when the type declares no bound.
const DEFAULT_BUFFER_MAX: u64 = 64;
/// How many times one `ref` name may expand along a single path.
const REF_EXPANSIONS_PER_PATH: u32 = 1;

/// Generate a value for `ty`. `depth` is the remaining budget; use [`DEFAULT_DEPTH`] at the
/// top level.
pub fn generate(ty: &TypeNode, depth: u32, rng: &mut Rng) -> ValueTree {
    generate_with_refs(ty, depth, &mut BTreeMap::new(), rng)
}

/// One value per parameter. An optional parameter is omitted a third of the time, which is how
/// a target with `undefined` handling gets exercised.
pub fn generate_arguments(parameters: &[Parameter], rng: &mut Rng) -> Vec<ValueTree> {
    let mut out = Vec::with_capacity(parameters.len());
    for p in parameters {
        if !p.required && !rng.chance(2, 3) {
            continue;
        }
        out.push(generate(&p.ty, DEFAULT_DEPTH, rng));
    }
    out
}

fn generate_with_refs(ty: &TypeNode, depth: u32, refs: &mut BTreeMap<String, u32>, rng: &mut Rng) -> ValueTree {
    if depth == 0 {
        return boundaries::boundary(terminate(ty));
    }
    let node = match ty {
        TypeNode::Scalar { scalar, bounds, length } => scalar_node(*scalar, *bounds, *length, rng),
        TypeNode::StringFormat { format } => ValueNode::string(format_value(format, rng)),
        TypeNode::Literal { values } => {
            if values.is_empty() {
                ValueNode::Null
            } else {
                ValueNode::Literal { value: rng.pick(values).cloned().unwrap_or(serde_json::Value::Null) }
            }
        }
        TypeNode::Collection { collection, element, key, length } => {
            collection_node(*collection, element, key.as_deref(), *length, depth, refs, rng)
        }
        TypeNode::Object { id, fields, name: _ } => object_node(id.as_deref(), fields, depth, refs, rng),
        TypeNode::Tuple { items } => ValueNode::Tuple {
            items: items.iter().map(|t| generate_with_refs(t, depth - 1, refs, rng)).collect(),
        },
        TypeNode::Composite { composite, members, discriminant } => {
            composite_node(*composite, members, discriminant.as_deref(), depth, refs, rng)
        }
        TypeNode::Buffer { length } => ValueNode::Buffer {
            bytes: (0..buffer_length(*length, rng)).map(|_| rng.next_u32() as u8).collect(),
        },
        // A promise is the async marker on a return type; as an *input* it is the awaited value.
        TypeNode::Promise { inner } => return generate_with_refs(inner, depth - 1, refs, rng),
        TypeNode::Function { behavior, returns } => ValueNode::Function {
            behavior: behavior.unwrap_or(FunctionBehavior::ReturnFirst),
            returns: returns.as_ref().map(|t| Box::new(generate_with_refs(t, depth - 1, refs, rng))),
        },
        TypeNode::Stream { iterable, element, max_items } => ValueNode::Stream {
            iterable: *iterable,
            items: (0..max_items.unwrap_or(DEFAULT_STREAM_MAX))
                .map(|_| generate_with_refs(element, depth - 1, refs, rng))
                .collect(),
        },
        TypeNode::Opaque { ty, module: _ } => ValueNode::Opaque { ty: ty.clone() },
        TypeNode::Ref { name, fields } => ref_node(name, fields, depth, refs, rng),
    };
    ValueTree::new(node)
}

fn scalar_node(scalar: ScalarKind, bounds: Option<NumberBounds>, length: Option<LengthBounds>, rng: &mut Rng) -> ValueNode {
    match scalar {
        ScalarKind::Null => ValueNode::Null,
        ScalarKind::Undefined => ValueNode::Undefined,
        ScalarKind::Boolean => ValueNode::Boolean { value: rng.boolean() },
        ScalarKind::Number => number_scalar(bounds.unwrap_or_default(), rng),
        ScalarKind::BigInt => {
            if rng.chance(1, 3) {
                boundaries::bigint_node(rng)
            } else {
                ValueNode::BigInt { value: rng.int_in(-1_000_000, 1_000_000).to_string() }
            }
        }
        ScalarKind::String => {
            if rng.chance(1, 4) {
                boundaries::string_node(rng, length)
            } else {
                let min = length.and_then(|l| l.min).unwrap_or(0);
                let max = length.and_then(|l| l.max).unwrap_or(DEFAULT_STRING_MAX).max(min);
                let len = rng.between(min, max);
                ValueNode::string((0..len).map(|_| rng.ascii_char()).collect::<String>())
            }
        }
    }
}

fn number_scalar(bounds: NumberBounds, rng: &mut Rng) -> ValueNode {
    if rng.chance(1, 3) {
        return boundaries::number_node(rng, bounds);
    }
    if bounds.int {
        return ValueNode::number(boundaries::int_in_declared(rng, bounds) as f64);
    }
    let mut v = rng.float_in(bounds.min.unwrap_or(-boundaries::DEFAULT_RANGE), bounds.max.unwrap_or(boundaries::DEFAULT_RANGE));
    if bounds.finite_only && !v.is_finite() {
        v = 0.0;
    }
    ValueNode::number(v)
}

fn collection_node(
    collection: CollectionKind,
    element: &TypeNode,
    key: Option<&TypeNode>,
    length: Option<LengthBounds>,
    depth: u32,
    refs: &mut BTreeMap<String, u32>,
    rng: &mut Rng,
) -> ValueNode {
    let len = boundaries::length(
        rng,
        length.and_then(|l| l.min),
        length.and_then(|l| l.max).or(Some(DEFAULT_LENGTH_MAX)),
    );
    match collection {
        CollectionKind::Array => ValueNode::Array {
            items: (0..len).map(|_| generate_with_refs(element, depth - 1, refs, rng)).collect(),
        },
        // A set is a list on the wire; the plugin deduplicates on materialization, so the
        // generator emits whatever the element type produces and lets collisions happen.
        CollectionKind::Set => ValueNode::Set {
            items: (0..len).map(|_| generate_with_refs(element, depth - 1, refs, rng)).collect(),
        },
        CollectionKind::Map => {
            let key_ty = key.cloned().unwrap_or_else(TypeNode::string);
            ValueNode::Map {
                entries: (0..len)
                    .map(|_| {
                        (
                            generate_with_refs(&key_ty, depth - 1, refs, rng),
                            generate_with_refs(element, depth - 1, refs, rng),
                        )
                    })
                    .collect(),
            }
        }
    }
}

fn buffer_length(length: Option<LengthBounds>, rng: &mut Rng) -> u64 {
    boundaries::length(rng, length.and_then(|l| l.min), length.and_then(|l| l.max).or(Some(DEFAULT_BUFFER_MAX)))
}

fn object_node(id: Option<&str>, fields: &[Field], depth: u32, refs: &mut BTreeMap<String, u32>, rng: &mut Rng) -> ValueNode {
    if let Some(id) = id {
        *refs.entry(id.to_string()).or_insert(0) += 1;
    }
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        if field.required || rng.chance(2, 3) {
            out.push((field.name.clone(), generate_with_refs(&field.ty, depth - 1, refs, rng)));
        }
    }
    if let Some(id) = id {
        if let Some(count) = refs.get_mut(id) {
            *count = count.saturating_sub(1);
        }
    }
    ValueNode::Object { fields: out }
}

fn composite_node(
    composite: CompositeKind,
    members: &[TypeNode],
    discriminant: Option<&str>,
    depth: u32,
    refs: &mut BTreeMap<String, u32>,
    rng: &mut Rng,
) -> ValueNode {
    if members.is_empty() {
        return ValueNode::Null;
    }
    match composite {
        CompositeKind::Union => {
            let chosen = rng.pick(members).cloned().unwrap_or_else(TypeNode::null);
            let mut tree = generate_with_refs(&chosen, depth - 1, refs, rng);
            // Discriminant-aware generation: an object arm of a discriminated union gets its
            // discriminant set, so a `switch` on it reaches each arm instead of only the first.
            if let Some(discriminant) = discriminant {
                if let ValueNode::Object { fields } = &mut tree.node {
                    if !fields.iter().any(|(n, _)| n == discriminant) {
                        fields.push((discriminant.to_string(), ValueTree::new(discriminant_node(&chosen, discriminant, rng))));
                    }
                }
            }
            tree.node
        }
        CompositeKind::Intersection => {
            // An intersection is satisfiable only by a value that satisfies every member, so
            // the generator merges the members' fields into one object.
            let mut fields = Vec::new();
            for member in members {
                if let ValueNode::Object { fields: mine } = generate_with_refs(member, depth - 1, refs, rng).node {
                    fields.extend(mine);
                }
            }
            if fields.is_empty() {
                ValueNode::Null
            } else {
                ValueNode::Object { fields }
            }
        }
    }
}

/// The literal a discriminant field takes: the member's own literal when it declares one,
/// otherwise a fresh short tag.
fn discriminant_node(member: &TypeNode, discriminant: &str, rng: &mut Rng) -> ValueNode {
    if let TypeNode::Object { fields, .. } = member {
        if let Some(field) = fields.iter().find(|f| f.name == discriminant) {
            if let TypeNode::Literal { values } = &field.ty {
                if let Some(v) = rng.pick(values) {
                    return ValueNode::Literal { value: v.clone() };
                }
            }
        }
    }
    ValueNode::string((0..4).map(|_| rng.lowercase()).collect::<String>())
}

fn ref_node(name: &str, fields: &[RefField], depth: u32, refs: &mut BTreeMap<String, u32>, rng: &mut Rng) -> ValueNode {
    let seen = refs.get(name).copied().unwrap_or(0);
    if seen >= REF_EXPANSIONS_PER_PATH {
        // The recursion terminates here rather than at the depth budget, so a self-referential
        // type produces a finite tree at any budget.
        return ValueNode::Null;
    }
    *refs.entry(name.to_string()).or_insert(0) += 1;
    let mut out = Vec::with_capacity(fields.len());
    for field in fields {
        // A `ref` field carries no required flag, so each appears on a coin flip and the
        // terminating case leaves them all out.
        if rng.chance(1, 2) {
            out.push((field.name.clone(), generate_with_refs(&field.ty, depth - 1, refs, rng)));
        }
    }
    if let Some(count) = refs.get_mut(name) {
        *count = count.saturating_sub(1);
    }
    if out.is_empty() {
        return ValueNode::Object { fields: Vec::new() };
    }
    ValueNode::Object { fields: out }
}

/// A plausible value for a declared string format. A format the engine does not know still
/// gets a non-empty string, so an unknown format never blocks a scan.
fn format_value(format: &str, rng: &mut Rng) -> String {
    let word = |rng: &mut Rng, n: u64| (0..n).map(|_| rng.lowercase()).collect::<String>();
    match format {
        "email" => format!("{}@{}.com", word(rng, 6), word(rng, 4)),
        "uuid" => {
            let hex = |rng: &mut Rng, n: u64| (0..n).map(|_| format!("{:x}", rng.below(16))).collect::<String>();
            format!("{}-{}-4{}-{}-{}", hex(rng, 8), hex(rng, 4), &hex(rng, 3)[..3], hex(rng, 4), hex(rng, 12))
        }
        "date-time" => format!("20{}-{}-{}T00:00:00.000Z", rng.between(19, 26), rng.between(1, 12), rng.between(1, 28)),
        "date" => format!("20{}-{}-{}", rng.between(19, 26), rng.between(1, 12), rng.between(1, 28)),
        "hostname" => format!("{}.example.com", word(rng, 5)),
        "ipv4" => format!("{}.{}.{}.{}", rng.below(256), rng.below(256), rng.below(256), rng.below(256)),
        "ipv6" => "2001:db8::1".to_string(),
        "uri" | "url" => format!("https://{}/{}", word(rng, 5), word(rng, 3)),
        "time" => format!("{:02}:{:02}:00", rng.below(24), rng.below(60)),
        "duration" => format!("{}s", rng.below(3600)),
        "byte" => "AAECAwQ=".to_string(),
        "password" => format!("P{}!", rng.int_in(0, 999_999)),
        _ => word(rng, 8),
    }
}

/// The value generation emits when the budget is spent: the simplest value the type admits.
pub fn terminate(ty: &TypeNode) -> ValueNode {
    match ty {
        TypeNode::Scalar { scalar, .. } => match scalar {
            ScalarKind::Null => ValueNode::Null,
            ScalarKind::Undefined => ValueNode::Undefined,
            ScalarKind::Boolean => ValueNode::Boolean { value: false },
            ScalarKind::Number => ValueNode::number(0.0),
            ScalarKind::BigInt => ValueNode::BigInt { value: "0".into() },
            ScalarKind::String => ValueNode::string(String::new()),
        },
        TypeNode::StringFormat { .. } => ValueNode::string(String::new()),
        TypeNode::Literal { values } => ValueNode::Literal {
            value: values.first().cloned().unwrap_or(serde_json::Value::Null),
        },
        TypeNode::Collection { collection, .. } => match collection {
            CollectionKind::Array => ValueNode::Array { items: Vec::new() },
            CollectionKind::Set => ValueNode::Set { items: Vec::new() },
            CollectionKind::Map => ValueNode::Map { entries: Vec::new() },
        },
        TypeNode::Object { fields, .. } => ValueNode::Object {
            // Required fields still appear at the budget floor, so a terminated object remains
            // something the target can destructure.
            fields: fields
                .iter()
                .filter(|f| f.required)
                .map(|f| (f.name.clone(), boundaries::boundary(terminate(&f.ty))))
                .collect(),
        },
        TypeNode::Tuple { items } => ValueNode::Tuple {
            items: items.iter().map(|t| boundaries::boundary(terminate(t))).collect(),
        },
        TypeNode::Composite { members, .. } => members.first().map(terminate).unwrap_or(ValueNode::Null),
        TypeNode::Buffer { .. } => ValueNode::Buffer { bytes: Vec::new() },
        TypeNode::Promise { inner } => terminate(inner),
        TypeNode::Function { behavior, .. } => ValueNode::Function {
            behavior: behavior.unwrap_or(FunctionBehavior::ReturnFirst),
            returns: None,
        },
        TypeNode::Stream { iterable, .. } => ValueNode::Stream { iterable: *iterable, items: Vec::new() },
        TypeNode::Opaque { ty, .. } => ValueNode::Opaque { ty: ty.clone() },
        TypeNode::Ref { fields, .. } => ValueNode::Object {
            fields: fields
                .iter()
                .map(|f| (f.name.clone(), boundaries::boundary(terminate(&f.ty))))
                .collect(),
        },
    }
}

/// Whether a kind can contain another node, which is what the mutator uses to decide whether a
/// subtree is worth resizing.
pub fn is_recursive(kind: Kind) -> bool {
    matches!(kind, Kind::Object | Kind::Collection | Kind::Composite | Kind::Ref | Kind::Tuple | Kind::Promise | Kind::Stream | Kind::Function)
}

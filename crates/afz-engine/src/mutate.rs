//! Structured mutation: rewrite a `ValueTree` in place at one node, keeping the rest of the
//! input intact.
//!
//! Every operator is node-level, which is what lets a mutated input stay type-conformant and
//! lets the minimizer reason about the result. The operator that ran is recorded on the changed
//! node's provenance, so a finding says which operation reached the fault.

use afz_protocol::{Kind, NodePath, Provenance, Step, TypeNode, ValueNode, ValueTree};

use crate::boundaries;
use crate::generate;
use crate::rng::Rng;

/// The operators, named on the wire so a finding can say which one produced a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// Replace a node with a freshly generated sibling of the same type.
    Sibling,
    /// Add or remove an optional object field.
    ToggleOptional,
    /// Grow, shrink, or empty a collection.
    Resize,
    /// Replace a node with a boundary value of its kind.
    Boundary,
    /// Copy a subtree from another corpus entry.
    Transplant,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Op::Sibling => "sibling",
            Op::ToggleOptional => "toggle-optional",
            Op::Resize => "resize",
            Op::Boundary => "boundary",
            Op::Transplant => "transplant",
        }
    }
}

/// Mutate `value`, which was generated against `ty`. `mutate` is the entry point the scheduler
/// uses; it performs no transplant, because that needs a donor.
pub fn mutate(value: &ValueTree, ty: &TypeNode, rng: &mut Rng) -> ValueTree {
    mutate_with_donor(value, ty, rng, None, "")
}

/// Mutate with a donor entry, which enables [`Op::Transplant`]. `donor_id` names the corpus
/// entry the subtree came from and lands in the provenance.
pub fn mutate_with_donor(value: &ValueTree, ty: &TypeNode, rng: &mut Rng, donor: Option<&ValueTree>, donor_id: &str) -> ValueTree {
    let mut out = value.clone();
    // The root path is a valid target: replacing the whole argument is a legal mutation, and a
    // scheduler that only ever mutated children could never leave an input's shape.
    let candidates = selectable_paths(&out);
    if candidates.is_empty() {
        return out;
    }
    let Some(path) = rng.pick(&candidates).cloned() else { return out };
    let Some(current) = path.get(&out).cloned() else { return out };
    let node_ty = type_at(ty, &path);

    let Some(op) = choose_op(rng, &current.node, donor) else { return out };
    let Some(replacement) = apply(op, &current, &node_ty, rng, donor, donor_id) else {
        return out;
    };
    if replacement.node == current.node {
        return out;
    }
    if path.replace(&mut out, replacement) {
        out
    } else {
        // The path went stale between selection and replacement; the input is returned whole
        // rather than half-mutated.
        value.clone()
    }
}

/// Every node the mutator may rewrite: the root and all descendants.
fn selectable_paths(value: &ValueTree) -> Vec<NodePath> {
    value.paths()
}

/// Pick an operator that the selected node can actually accept.
fn choose_op(rng: &mut Rng, node: &ValueNode, donor: Option<&ValueTree>) -> Option<Op> {
    let mut ops = vec![Op::Sibling, Op::Boundary];
    match node {
        ValueNode::Array { .. } | ValueNode::Set { .. } | ValueNode::Map { .. } | ValueNode::Tuple { .. } | ValueNode::Stream { .. } | ValueNode::Buffer { .. } => {
            ops.push(Op::Resize);
        }
        ValueNode::Object { .. } => ops.push(Op::ToggleOptional),
        _ => {}
    }
    if donor.is_some() {
        ops.push(Op::Transplant);
    }
    rng.pick(&ops).copied()
}

fn apply(
    op: Op,
    current: &ValueTree,
    ty: &TypeNode,
    rng: &mut Rng,
    donor: Option<&ValueTree>,
    donor_id: &str,
) -> Option<ValueTree> {
    // `op` names the operator and `from` names what it acted on, so a finding can be traced
    // back through the corpus without replaying the run.
    let (node, provenance) = match op {
        Op::Sibling => (
            generate::generate(ty, generate::DEFAULT_DEPTH, rng).node,
            Provenance::Mutated { op: op.name().into(), from: ty_label(ty) },
        ),
        Op::Boundary => {
            let (node, source) = boundary_for(&current.node, ty, rng)?;
            (node, Provenance::Mutated { op: op.name().into(), from: source.into() })
        }
        Op::Resize => (
            resize(&current.node, ty, rng)?,
            Provenance::Mutated { op: op.name().into(), from: kind_label(&current.node) },
        ),
        Op::ToggleOptional => {
            let (node, field) = toggle_optional(&current.node, ty, rng)?;
            (node, Provenance::Mutated { op: op.name().into(), from: field })
        }
        Op::Transplant => {
            let donor = donor?;
            let paths = donor.paths();
            let path = rng.pick(&paths)?;
            let subtree = path.get(donor)?.clone();
            (subtree.node, Provenance::Transplanted { from_entry: donor_id.to_string() })
        }
    };
    Some(ValueTree::with_provenance(node, provenance))
}

/// A boundary value of the same node kind, and what it was drawn from.
///
/// A composite boundary comes from the declared type, not the table: an empty object would drop
/// required fields and an all-null tuple would break arity, so the boundary of a composite is
/// its declared type's terminated form.
fn boundary_for(node: &ValueNode, ty: &TypeNode, rng: &mut Rng) -> Option<(ValueNode, &'static str)> {
    let wrapped = |n: ValueNode, s: &'static str| Some((n, s));
    if node.kind() == ty.kind() && matches!(node, ValueNode::Object { .. } | ValueNode::Tuple { .. }) {
        return wrapped(generate::terminate(ty), "declared-type");
    }
    match node {
        ValueNode::Number { .. } => wrapped(boundaries::number_node(rng, declared_number_bounds(ty)), "boundary-table"),
        ValueNode::BigInt { .. } => wrapped(boundaries::bigint_node(rng), "boundary-table"),
        ValueNode::String { .. } => wrapped(boundaries::string_node(rng, declared_string_length(ty)), "boundary-table"),
        ValueNode::Boolean { .. } => wrapped(ValueNode::Boolean { value: rng.boolean() }, "boundary-table"),
        ValueNode::Array { .. } => wrapped(ValueNode::Array { items: Vec::new() }, "boundary-table"),
        ValueNode::Set { .. } => wrapped(ValueNode::Set { items: Vec::new() }, "boundary-table"),
        ValueNode::Map { .. } => wrapped(ValueNode::Map { entries: Vec::new() }, "boundary-table"),
        ValueNode::Buffer { .. } => wrapped(ValueNode::Buffer { bytes: Vec::new() }, "boundary-table"),
        ValueNode::Stream { iterable, .. } => wrapped(ValueNode::Stream { iterable: *iterable, items: Vec::new() }, "boundary-table"),
        // Without a declared object type the mutator cannot know which fields are required, so
        // it declines rather than emit an object the target cannot read.
        ValueNode::Object { .. } => None,
        // A literal is already its type's only boundary, so there is nothing to swap in.
        ValueNode::Literal { .. } => None,
        _ => None,
    }
}

fn resize(node: &ValueNode, ty: &TypeNode, rng: &mut Rng) -> Option<ValueNode> {
    let grow = rng.chance(1, 2);
    match node {
        ValueNode::Array { items } => Some(ValueNode::Array { items: resize_items(items, grow, rng) }),
        ValueNode::Set { items } => Some(ValueNode::Set { items: resize_items(items, grow, rng) }),
        ValueNode::Stream { iterable, items } => Some(ValueNode::Stream { iterable: *iterable, items: resize_items(items, grow, rng) }),
        ValueNode::Map { entries } => {
            let mut entries = entries.clone();
            if entries.is_empty() || (grow && !entries.is_empty()) {
                let (k, v) = entries.last().cloned().unwrap_or_else(|| (ValueTree::string("k"), ValueTree::null()));
                entries.push((k, v));
            } else {
                entries.remove(rng.below(entries.len() as u64) as usize);
            }
            Some(ValueNode::Map { entries })
        }
        ValueNode::Buffer { bytes } => {
            let mut bytes = bytes.clone();
            // An empty buffer has nothing to remove, so the first byte arrives on either draw.
            if bytes.is_empty() || grow {
                bytes.push(rng.next_u32() as u8);
            } else {
                bytes.remove(rng.below(bytes.len() as u64) as usize);
            }
            Some(ValueNode::Buffer { bytes })
        }
        ValueNode::Tuple { items } => {
            // A tuple is fixed-arity, so a resize changes one element instead of the length,
            // and only when the declaration says what that element's type is.
            let TypeNode::Tuple { items: declared } = ty else { return None };
            if items.is_empty() || items.len() != declared.len() {
                return None;
            }
            let mut items = items.clone();
            let at = rng.below(items.len() as u64) as usize;
            items[at] = boundaries::boundary(generate::terminate(&declared[at]));
            Some(ValueNode::Tuple { items })
        }
        _ => None,
    }
}

fn resize_items(items: &[ValueTree], grow: bool, rng: &mut Rng) -> Vec<ValueTree> {
    let mut items = items.to_vec();
    if items.is_empty() || grow {
        let seed = items.last().cloned().unwrap_or_else(|| ValueTree::number(0.0));
        items.push(seed);
    } else {
        items.remove(rng.below(items.len() as u64) as usize);
    }
    items
}

fn toggle_optional(node: &ValueNode, ty: &TypeNode, rng: &mut Rng) -> Option<(ValueNode, String)> {
    let ValueNode::Object { fields } = node else { return None };
    let TypeNode::Object { fields: declared, .. } = ty else {
        // Without a declaration the mutator cannot tell which field is optional, so it drops a
        // field it is allowed to drop: any of them.
        if fields.is_empty() {
            return None;
        }
        let mut fields = fields.clone();
        let at = rng.below(fields.len() as u64) as usize;
        let removed = fields[at].0.clone();
        fields.remove(at);
        return Some((ValueNode::Object { fields }, format!("-{removed}")));
    };

    // Remove a field that is not required, or add a declared field that is absent.
    let removable: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, (name, _))| declared.iter().find(|f| &f.name == name).is_none_or(|f| !f.required))
        .map(|(i, _)| i)
        .collect();
    let missing: Vec<&afz_protocol::Field> = declared
        .iter()
        .filter(|f| !fields.iter().any(|(n, _)| n == &f.name))
        .collect();

    if !removable.is_empty() && (missing.is_empty() || rng.chance(1, 2)) {
        let mut fields = fields.clone();
        let at = *rng.pick(&removable)?;
        let removed = fields[at].0.clone();
        fields.remove(at);
        return Some((ValueNode::Object { fields }, format!("-{removed}")));
    }
    let field = *rng.pick(&missing)?;
    let mut fields = fields.clone();
    fields.push((field.name.clone(), generate::generate(&field.ty, generate::DEFAULT_DEPTH, rng)));
    Some((ValueNode::Object { fields }, format!("+{}", field.name)))
}

/// The declared type at a path, so a mutation can generate a conformant replacement. Falls back
/// to `null` when the path does not correspond to a declared type, which happens after a
/// transplant has already changed the shape.
pub fn type_at(ty: &TypeNode, path: &NodePath) -> TypeNode {
    let mut current = ty.clone();
    for step in &path.steps {
        current = match (step, &current) {
            (Step::Field(name), TypeNode::Object { fields, .. }) => fields
                .iter()
                .find(|f| &f.name == name)
                .map(|f| f.ty.clone())
                .unwrap_or_else(TypeNode::null),
            // A map key is a distinct type from the element, so it is matched before the
            // element arm that would otherwise swallow every `Key` step.
            (Step::Key(_, afz_protocol::KeySide::Key), TypeNode::Collection { key: Some(key), .. }) => (**key).clone(),
            (Step::Index(_) | Step::Key(_, afz_protocol::KeySide::Value), TypeNode::Collection { element, .. }) => (**element).clone(),
            (Step::Return, TypeNode::Function { returns: Some(inner), .. }) => (**inner).clone(),
            (Step::Index(i), TypeNode::Tuple { items }) => items.get(*i).cloned().unwrap_or_else(TypeNode::null),
            (Step::Index(_), TypeNode::Stream { element, .. }) => (**element).clone(),
            _ => TypeNode::null(),
        };
    }
    current
}

/// A short label for the type a mutation was drawn against, recorded in provenance.
fn ty_label(ty: &TypeNode) -> String {
    let kind: Kind = ty.kind();
    format!("{kind:?}").to_lowercase()
}

/// A short label for the node an operator acted on.
fn kind_label(node: &ValueNode) -> String {
    format!("{:?}", node.kind()).to_lowercase()
}

/// The declared numeric bounds of a type, so the boundary swap respects them.
fn declared_number_bounds(ty: &TypeNode) -> afz_protocol::NumberBounds {
    match ty {
        TypeNode::Scalar { bounds, .. } => bounds.unwrap_or_default(),
        _ => afz_protocol::NumberBounds::default(),
    }
}

/// The declared length bound of a string type, so the boundary swap respects it.
fn declared_string_length(ty: &TypeNode) -> Option<afz_protocol::LengthBounds> {
    match ty {
        TypeNode::Scalar { length, .. } => *length,
        _ => None,
    }
}

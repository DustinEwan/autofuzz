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
            // A graft has to be something the declaration allows at this position. A `LineItem`
            // grafted onto a parameter declared `Invoice` reaches the target as
            // `inv.lines === undefined`, and a number grafted onto a field declared
            // `region: string` reaches it as `region.toUpperCase is not a function`. Both faults
            // report that a caller ignored the types, which is not the target's logic.
            let compatible: Vec<NodePath> =
                donor.paths().into_iter().filter(|path| path.get(donor).is_some_and(|sub| matches(ty, &sub.node))).collect();
            let path = rng.pick(&compatible).cloned()?;
            let subtree = path.get(donor)?.clone();
            (subtree.node, Provenance::Transplanted { from_entry: donor_id.to_string() })
        }
    };
    Some(ValueTree::with_provenance(node, provenance))
}

/// Whether a value is something the declaration allows at this position.
///
/// Transplant is the only operator that moves a value between positions, and the donor was built
/// against a different declared type, so the check is structural rather than by [`Kind`]:
/// [`Kind::Scalar`] covers `number` and `string` alike. Comparison stops at a [`TypeNode::Ref`],
/// whose shape the wire type does not carry beyond its own fields.
fn matches(ty: &TypeNode, node: &ValueNode) -> bool {
    match (ty, node) {
        (TypeNode::Composite { composite, members, .. }, _) => match composite {
            afz_protocol::CompositeKind::Union => members.iter().any(|m| matches(m, node)),
            afz_protocol::CompositeKind::Intersection => members.iter().all(|m| matches(m, node)),
        },
        (TypeNode::Ref { fields, .. }, ValueNode::Object { fields: have }) => fields.iter().all(|f| {
            have.iter().find(|(n, _)| n == &f.name).is_none_or(|(_, v)| matches(&f.ty, &v.node))
        }),
        (TypeNode::Scalar { scalar, .. }, other) => match (scalar, other) {
            (afz_protocol::ScalarKind::Null, ValueNode::Null) => true,
            (afz_protocol::ScalarKind::Undefined, ValueNode::Undefined) => true,
            (afz_protocol::ScalarKind::Boolean, ValueNode::Boolean { .. }) => true,
            (afz_protocol::ScalarKind::Number, ValueNode::Number { .. }) => true,
            (afz_protocol::ScalarKind::BigInt, ValueNode::BigInt { .. }) => true,
            (afz_protocol::ScalarKind::String, ValueNode::String { .. }) => true,
            _ => false,
        },
        (TypeNode::StringFormat { .. }, ValueNode::String { .. }) => true,
        (TypeNode::Literal { values }, ValueNode::Literal { value }) => values.contains(value),
        (TypeNode::Collection { collection, element, key, .. }, other) => match (collection, other) {
            (afz_protocol::CollectionKind::Array, ValueNode::Array { items }) => {
                items.iter().all(|i| matches(element, &i.node))
            }
            (afz_protocol::CollectionKind::Set, ValueNode::Set { items }) => items.iter().all(|i| matches(element, &i.node)),
            (afz_protocol::CollectionKind::Map, ValueNode::Map { entries }) => entries.iter().all(|(k, v)| {
                key.as_ref().is_some_and(|kt| matches(kt, &k.node)) && matches(element, &v.node)
            }),
            _ => false,
        },
        (TypeNode::Object { fields, .. }, ValueNode::Object { fields: have }) => {
            fields.iter().all(|f| match have.iter().find(|(n, _)| n == &f.name) {
                Some((_, v)) => matches(&f.ty, &v.node),
                None => !f.required,
            }) && have.iter().all(|(n, v)| fields.iter().any(|f| &f.name == n && matches(&f.ty, &v.node)))
        }
        (TypeNode::Tuple { items }, ValueNode::Tuple { items: have }) => {
            items.len() == have.len() && items.iter().zip(have).all(|(t, v)| matches(t, &v.node))
        }
        (TypeNode::Buffer { .. }, ValueNode::Buffer { .. }) => true,
        (TypeNode::Promise { .. }, ValueNode::Function { .. }) => true,
        (TypeNode::Function { .. }, ValueNode::Function { .. }) => true,
        (TypeNode::Stream { iterable, element, .. }, ValueNode::Stream { iterable: is_iterable, items }) => {
            iterable == is_iterable && items.iter().all(|i| matches(element, &i.node))
        }
        (TypeNode::Opaque { ty: name, .. }, ValueNode::Opaque { ty }) => name == ty,
        _ => false,
    }
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

/// The length ceiling a resize may cross when the declared type states no maximum. Without it,
/// repeated growth produces an artifact no plugin could materialize.
const ABSOLUTE_MAX: u64 = 256;

fn resize(node: &ValueNode, ty: &TypeNode, rng: &mut Rng) -> Option<ValueNode> {
    let (min, max) = declared_length(ty);
    let element = match ty {
        TypeNode::Collection { element, .. } | TypeNode::Stream { element, .. } => Some(element.as_ref()),
        _ => None,
    };
    match node {
        ValueNode::Array { items } => {
            let len = step(items.len() as u64, min, max, rng)?;
            let mut items = items.clone();
            resize_to(&mut items, len, element, rng)?;
            Some(ValueNode::Array { items })
        }
        ValueNode::Set { items } => {
            let len = step(items.len() as u64, min, max, rng)?;
            let mut items = items.clone();
            resize_to(&mut items, len, element, rng)?;
            Some(ValueNode::Set { items })
        }
        ValueNode::Stream { iterable, items } => {
            let len = step(items.len() as u64, min, max, rng)?;
            let mut items = items.clone();
            resize_to(&mut items, len, element, rng)?;
            Some(ValueNode::Stream { iterable: *iterable, items })
        }
        ValueNode::Map { entries } => {
            let TypeNode::Collection { element, key, .. } = ty else { return None };
            let len = step(entries.len() as u64, min, max, rng)?;
            let mut entries = entries.clone();
            if entries.len() < len {
                let (k, v) = match entries.last().cloned() {
                    Some(pair) => pair,
                    None => (
                        generate::generate(key.as_ref()?.as_ref(), generate::DEFAULT_DEPTH, rng),
                        generate::generate(element, generate::DEFAULT_DEPTH, rng),
                    ),
                };
                entries.push((k, v));
            } else {
                entries.remove(rng.below(entries.len() as u64) as usize);
            }
            Some(ValueNode::Map { entries })
        }
        ValueNode::Buffer { bytes } => {
            let len = step(bytes.len() as u64, min, max, rng)?;
            let mut bytes = bytes.clone();
            if bytes.len() < len {
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

/// The length after one resize step of exactly one element.
///
/// The declared bounds decide the direction, so a parameter declared `maxItems: 8` never
/// receives nine items: a value the type rules out is a type error, not a finding. When both
/// directions are open the coin flip is the only draw, which keeps the ladder's walk one step at
/// a time.
fn step(len: u64, min: u64, max: Option<u64>, rng: &mut Rng) -> Option<usize> {
    let can_grow = len < max.unwrap_or(ABSOLUTE_MAX);
    let can_shrink = len > min;
    match (can_grow, can_shrink) {
        (true, true) => Some(if rng.chance(1, 2) { (len + 1) as usize } else { (len - 1) as usize }),
        (true, false) => Some((len + 1) as usize),
        (false, true) => Some((len - 1) as usize),
        (false, false) => None,
    }
}

/// The declared length bounds of a collection, buffer, or stream type.
fn declared_length(ty: &TypeNode) -> (u64, Option<u64>) {
    match ty {
        TypeNode::Collection { length, .. } | TypeNode::Buffer { length } => {
            (length.and_then(|l| l.min).unwrap_or(0), length.and_then(|l| l.max))
        }
        TypeNode::Stream { max_items, .. } => (0, *max_items),
        _ => (0, None),
    }
}

/// Grow or shrink a collection by one element.
///
/// The new element is a value of the declared element type: cloning the last item is only
/// possible when there is one, and a `number` pushed into a `Row[]` is a type error the target
/// never sees rather than logic it has to handle, so an element with no declaration and no
/// sibling to copy leaves the resize undone.
fn resize_to(items: &mut Vec<ValueTree>, len: usize, element: Option<&TypeNode>, rng: &mut Rng) -> Option<()> {
    if items.len() < len {
        let seed = match items.last() {
            Some(last) => last.clone(),
            None => generate::generate(element?, generate::DEFAULT_DEPTH, rng),
        };
        items.push(seed);
    } else if items.len() > len {
        items.remove(rng.below(items.len() as u64) as usize);
    }
    Some(())
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

//! Mutation: node-level rewrites that stay type-conformant and carry provenance.

use afz_engine::generate::{self, DEFAULT_DEPTH};
use afz_engine::mutate::{self, Op};
use afz_engine::rng::Rng;
use afz_protocol::{Field, LengthBounds, NumberBounds, Provenance, ScalarKind, TypeNode, ValueNode, ValueTree};

fn bounded_number() -> TypeNode {
    TypeNode::Scalar {
        scalar: ScalarKind::Number,
        bounds: Some(NumberBounds { int: true, finite_only: true, min: Some(0.0), max: Some(10.0) }),
        length: None,
    }
}

fn object_type() -> TypeNode {
    TypeNode::Object {
        name: Some("Config".into()),
        id: None,
        fields: vec![
            Field { name: "id".into(), ty: TypeNode::number(), required: true },
            Field { name: "label".into(), ty: TypeNode::string(), required: false },
            Field { name: "tags".into(), ty: TypeNode::array(TypeNode::string()), required: false },
        ],
    }
}

#[test]
fn mutation_is_deterministic_for_one_seed() {
    let ty = object_type();
    let base = generate::generate(&ty, DEFAULT_DEPTH, &mut Rng::seeded(7));
    let once = mutate::mutate(&base, &ty, &mut Rng::seeded(99));
    let twice = mutate::mutate(&base, &ty, &mut Rng::seeded(99));
    assert_eq!(serde_json::to_string(&once).unwrap(), serde_json::to_string(&twice).unwrap());
}

#[test]
fn a_mutated_bounded_number_stays_in_range() {
    let ty = bounded_number();
    let mut rng = Rng::seeded(0x1234);
    let mut saw_change = 0;
    for _ in 0..500 {
        let base = generate::generate(&ty, DEFAULT_DEPTH, &mut rng);
        let after = mutate::mutate(&base, &ty, &mut rng);
        let ValueNode::Number { value } = after.node else { panic!("a number type mutated into {:?}", after.node) };
        let v = value.as_f64();
        assert!(v.fract() == 0.0 && (0.0..=10.0).contains(&v), "mutation produced {v} outside 0..=10");
        if after != base {
            saw_change += 1;
        }
    }
    assert!(saw_change > 50, "mutation changed nothing across 500 draws");
}

#[test]
fn a_mutated_node_records_which_operator_ran() {
    let ty = object_type();
    let mut rng = Rng::seeded(5);
    let mut seen = Vec::new();
    for _ in 0..400 {
        let base = generate::generate(&ty, DEFAULT_DEPTH, &mut rng);
        let after = mutate::mutate(&base, &ty, &mut rng);
        for path in after.paths() {
            let node = path.get(&after).unwrap();
            match &node.provenance {
                Provenance::Mutated { op, .. } => {
                    if !seen.contains(op) {
                        seen.push(op.clone());
                    }
                }
                Provenance::Generated | Provenance::Boundary => {}
                other => panic!("generation produced provenance {other:?}"),
            }
        }
    }
    for expected in [Op::Sibling.name(), Op::Boundary.name(), Op::Resize.name(), Op::ToggleOptional.name()] {
        assert!(seen.contains(&expected.to_string()), "operator {expected} never ran across 400 mutations: {seen:?}");
    }
}

#[test]
fn a_transplant_records_the_entry_it_came_from() {
    let ty = object_type();
    let mut rng = Rng::seeded(11);
    let base = generate::generate(&ty, DEFAULT_DEPTH, &mut rng);
    let donor = generate::generate(&ty, DEFAULT_DEPTH, &mut Rng::seeded(12));
    let mut saw_transplant = false;
    for i in 0..400 {
        let after = mutate::mutate_with_donor(&base, &ty, &mut rng, Some(&donor), "afz:t:donor");
        for path in after.paths() {
            if let Provenance::Transplanted { from_entry } = &path.get(&after).unwrap().provenance {
                assert_eq!(from_entry, "afz:t:donor");
                saw_transplant = true;
            }
        }
        let _ = i;
    }
    assert!(saw_transplant, "transplant never ran with a donor present");
}

/// A graft that violates the declaration tests what happens when a caller ignores the types, and
/// the fault it reports is not the target's logic. The donor below comes from a different
/// signature: its `region` is a number and its row items carry no `sku`, so the only places any
/// part of it may stand are the number positions.
#[test]
fn a_transplant_never_violates_the_declared_type() {
    let row = TypeNode::Object {
        name: Some("Row".into()),
        id: None,
        fields: vec![
            Field { name: "sku".into(), ty: TypeNode::string(), required: true },
            Field { name: "qty".into(), ty: TypeNode::number(), required: true },
        ],
    };
    let ty = TypeNode::Object {
        name: Some("Report".into()),
        id: None,
        fields: vec![
            Field { name: "region".into(), ty: TypeNode::string(), required: true },
            Field { name: "rows".into(), ty: TypeNode::array(row), required: true },
        ],
    };
    let donor = ValueTree::new(ValueNode::Object {
        fields: vec![
            ("region".into(), ValueTree::number(7.0)),
            (
                "rows".into(),
                ValueTree::new(ValueNode::Array {
                    items: vec![ValueTree::new(ValueNode::Object { fields: vec![("qty".into(), ValueTree::number(3.0))] })],
                }),
            ),
        ],
    });

    let mut rng = Rng::seeded(0x51);
    let mut saw_transplant = false;
    for _ in 0..600 {
        let base = generate::generate(&ty, DEFAULT_DEPTH, &mut rng);
        let after = mutate::mutate_with_donor(&base, &ty, &mut rng, Some(&donor), "afz:t:other");
        let ValueNode::Object { fields } = &after.node else { panic!("the root of a Report stayed no object") };
        for (name, value) in fields {
            match name.as_str() {
                "region" => {
                    assert!(matches!(value.node, ValueNode::String { .. }), "`region: string` holds {:?}", value.node);
                }
                "rows" => {
                    let ValueNode::Array { items } = &value.node else { panic!("`rows` stayed no array") };
                    for item in items {
                        let ValueNode::Object { fields: cells } = &item.node else { panic!("a row stayed no object") };
                        for (field, cell) in cells {
                            let declared =
                                matches!((field.as_str(), &cell.node), ("sku", ValueNode::String { .. }) | ("qty", ValueNode::Number { .. }));
                            assert!(declared, "`{field}` holds {:?}, which the row type does not declare", cell.node);
                        }
                    }
                }
                other => panic!("generation invented a field {other}"),
            }
        }
        saw_transplant |= after
            .paths()
            .iter()
            .any(|p| matches!(p.get(&after).unwrap().provenance, Provenance::Transplanted { .. }));
    }
    assert!(saw_transplant, "the filter left no legal graft, so transplant can never fire");
}

#[test]
fn toggling_an_optional_never_removes_a_required_field() {
    let ty = object_type();
    let mut rng = Rng::seeded(21);
    for _ in 0..500 {
        let base = generate::generate(&ty, DEFAULT_DEPTH, &mut rng);
        let after = mutate::mutate(&base, &ty, &mut rng);
        if let ValueNode::Object { fields } = &after.node {
            assert!(fields.iter().any(|(n, _)| n == "id"), "mutation dropped the required field `id`");
        }
    }
}

#[test]
fn a_mutated_tree_still_addresses_every_node_by_path() {
    // The minimizer in Step 8 replaces nodes by path, so a mutated tree must keep `paths`,
    // `get`, and `replace` consistent with each other.
    let ty = object_type();
    let mut rng = Rng::seeded(31);
    let base = generate::generate(&ty, DEFAULT_DEPTH, &mut rng);
    let after = mutate::mutate(&base, &ty, &mut rng);
    let paths = after.paths();
    assert!(!paths.is_empty());
    for path in &paths {
        let found = path.get(&after).expect("every reported path resolves");
        let mut rebuilt = after.clone();
        assert!(path.replace(&mut rebuilt, ValueTree::number(3.0)), "path {path:?} cannot be replaced");
        let replaced = path.get(&rebuilt).unwrap();
        assert!(matches!(replaced.node, ValueNode::Number { .. }));
        let _ = found;
    }
}

#[test]
fn mutation_of_a_bounded_string_respects_its_length() {
    let ty = TypeNode::Scalar {
        scalar: ScalarKind::String,
        bounds: None,
        length: Some(LengthBounds { min: Some(1), max: Some(3) }),
    };
    let mut rng = Rng::seeded(41);
    for _ in 0..300 {
        let base = generate::generate(&ty, DEFAULT_DEPTH, &mut rng);
        let after = mutate::mutate(&base, &ty, &mut rng);
        let ValueNode::String { value } = after.node else { panic!("string type mutated into {:?}", after.node) };
        assert!((1..=3).contains(&value.chars().count()), "mutated string {:?} ignores length 1..=3", value);
    }
}

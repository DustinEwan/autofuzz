//! Generation golden: the same seed must produce the same bytes.
//!
//! The fixture type set at `tests/golden/types.json` covers every wire kind. Values are
//! generated from one `Rng` in a fixed order, so the whole file is one draw sequence: any change
//! to the generator, the boundary table, or the seed shifts a byte.

use std::path::{Path, PathBuf};

use afz_engine::generate::{self, DEFAULT_DEPTH};
use afz_engine::rng::Rng;
use afz_protocol::{LengthBounds, NumberBounds, TypeNode, ValueNode};
use serde::{Deserialize, Serialize};

const SEED: u32 = 0x5e3a_91c4;
const PER_TYPE: usize = 8;

#[derive(Deserialize)]
struct Fixture {
    name: String,
    #[serde(rename = "type")]
    ty: TypeNode,
}

#[derive(Serialize)]
struct Line<'a> {
    r#type: &'a str,
    index: usize,
    value: &'a afz_protocol::ValueTree,
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden")
}

fn fixtures() -> Vec<Fixture> {
    let path = golden_dir().join("types.json");
    let raw = std::fs::read_to_string(&path).expect("committed fixture type set tests/golden/types.json");
    serde_json::from_str(&raw).expect("fixture type set parses as TypeNode JSON")
}

fn jsonl(seed: u32, fixtures: &[Fixture]) -> String {
    let mut rng = Rng::seeded(seed);
    let mut out = String::new();
    for fixture in fixtures {
        for index in 0..PER_TYPE {
            let value = generate::generate(&fixture.ty, DEFAULT_DEPTH, &mut rng);
            let line = Line { r#type: &fixture.name, index, value: &value };
            out.push_str(&serde_json::to_string(&line).expect("value tree serializes"));
            out.push('\n');
        }
    }
    out
}

#[test]
fn generation_matches_the_committed_golden_byte_for_byte() {
    let fixtures = fixtures();
    assert!(fixtures.len() >= 20, "the fixture set must cover every wire kind");
    let path = golden_dir().join(format!("seed-{SEED:08x}.jsonl"));
    let actual = jsonl(SEED, &fixtures);
    if !path.exists() {
        std::fs::write(&path, &actual).expect("write golden");
        panic!("golden created at {}; rerun the test to check it byte-for-byte", path.display());
    }
    let expected = std::fs::read_to_string(&path).expect("read golden");
    assert_eq!(actual, expected, "generation drifted from the committed golden");
}

#[test]
fn the_same_seed_reproduces_and_a_different_seed_does_not() {
    let fixtures = fixtures();
    assert_eq!(jsonl(SEED, &fixtures), jsonl(SEED, &fixtures), "one seed must replay exactly");
    assert_ne!(jsonl(SEED, &fixtures), jsonl(SEED ^ 0x0000_ffff, &fixtures), "a different seed must explore elsewhere");
}

#[test]
fn every_wire_kind_appears_in_the_fixture_set() {
    let kinds: Vec<String> = fixtures().iter().map(|f| format!("{:?}", f.ty.kind())).collect();
    for expected in [
        "Scalar", "StringFormat", "Literal", "Collection", "Object", "Tuple", "Composite", "Buffer", "Promise", "Function",
        "Stream", "Opaque", "Ref",
    ] {
        assert!(kinds.iter().any(|k| k == expected), "fixture set has no {expected} type: {kinds:?}");
    }
}

#[test]
fn a_recursive_ref_terminates_at_any_budget() {
    // A self-referential node: `Node { children: Node[] }`. Without the per-path ref counter
    // this recursion runs to the depth budget at every level, which is exponential work.
    let inner = TypeNode::Ref { name: "Node".into(), fields: Vec::new() };
    let ty = TypeNode::Object {
        name: Some("Node".into()),
        id: Some("Node".into()),
        fields: vec![
            afz_protocol::Field { name: "id".into(), ty: TypeNode::number(), required: true },
            afz_protocol::Field { name: "children".into(), ty: TypeNode::array(inner), required: true },
        ],
    };
    let mut rng = Rng::seeded(SEED);
    for _ in 0..200 {
        let tree = generate::generate(&ty, 64, &mut rng);
        assert!(tree.paths().len() < 5_000, "a recursive type produced {} nodes at budget 64", tree.paths().len());
    }
}

#[test]
fn generation_respects_declared_bounds() {
    let ty = TypeNode::Scalar {
        scalar: afz_protocol::ScalarKind::Number,
        bounds: Some(NumberBounds { int: true, finite_only: true, min: Some(0.0), max: Some(10.0) }),
        length: None,
    };
    let mut rng = Rng::seeded(SEED);
    for _ in 0..500 {
        let value = generate::generate(&ty, DEFAULT_DEPTH, &mut rng);
        let ValueNode::Number { value } = value.node else { panic!("a number type generated {:?}", value.node) };
        let v = value.as_f64();
        assert!(v.fract() == 0.0, "an int type generated {v}");
        assert!((0.0..=10.0).contains(&v), "declared range 0..=10 produced {v}");
    }

    let short = TypeNode::Scalar {
        scalar: afz_protocol::ScalarKind::String,
        bounds: None,
        length: Some(LengthBounds { min: Some(2), max: Some(4) }),
    };
    for _ in 0..500 {
        let value = generate::generate(&short, DEFAULT_DEPTH, &mut rng);
        let ValueNode::String { value } = value.node else { panic!("a string type generated {:?}", value.node) };
        assert!((2..=4).contains(&value.chars().count()), "declared length 2..=4 produced {:?}", value);
    }
}

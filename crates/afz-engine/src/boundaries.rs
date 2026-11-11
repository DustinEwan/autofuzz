//! The boundary values generation and mutation share.
//!
//! One table, read by both, because a value that appears during generation must also be
//! reachable during mutation for replay to hold: `--seed` reproduces a run only if the same
//! draws come from the same table. A boundary is selected by index, never by a distribution,
//! so a table edit changes the sequence in exactly one place.

use afz_protocol::{LengthBounds, NumberBounds, NumberValue, ValueNode, ValueTree};

use crate::rng::Rng;

/// Numeric boundaries. `i64::MIN` and `i64::MAX` are the edges a JavaScript number still
/// represents exactly, and `NAN` and `INFINITY` are the two values a target most often forgets
/// to guard.
pub const NUMBERS: &[f64] = &[
    0.0,
    1.0,
    -1.0,
    i64::MIN as f64,
    i64::MAX as f64,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
    0.5,
    -0.0,
];

/// Integer boundaries, for a parameter declared `int`.
pub const INTEGERS: &[i64] = &[0, 1, -1, i64::MIN, i64::MAX, 2, 255, 256, 65_535, 65_536];

/// Every integer below 128, which generation and mutation treat as boundaries alongside
/// [`INTEGERS`]. A `u32` or `i32` parameter is usually a length, an index, or a small tag, and a
/// draw uniform over the declared range - the whole `i32` range for a wasm export, which is all
/// the module declares - never produces one. The band is where the branch conditions are.
const fn small_integers() -> [i64; 128] {
    let mut out = [0i64; 128];
    let mut i = 0;
    while i < 128 {
        out[i] = i as i64;
        i += 1;
    }
    out
}

pub static SMALL_INTEGERS: [i64; 128] = small_integers();

/// `bigint` boundaries. They travel as strings, because JSON has no token for them.
pub const BIGINTS: &[&str] = &[
    "0",
    "1",
    "-1",
    "-9223372036854775808",
    "9223372036854775807",
    "340282366920938463463374607431768211456",
];

/// String boundaries. The empty string, the values a parser confuses with a number, the
/// prototype-pollution keys, and the characters that break a naive concatenation.
pub const STRINGS: &[&str] = &[
    "",
    " ",
    "0",
    "-1",
    "1e309",
    "null",
    "undefined",
    "true",
    "__proto__",
    "constructor",
    "prototype",
    "\n",
    "\u{0}",
    "\u{feff}",
    "😀",
    "../../etc/passwd",
    "' OR 1=1 --",
    "1e-323",
    "9007199254740993",
];

/// The long-string boundary: one MiB, which is where a target that copies its input into a
/// buffer per character starts to differ from one that does not.
pub const LONG_STRING_LEN: usize = 1 << 20;

/// Collection length boundaries: empty, one, and the two sizes around a typical power-of-two
/// growth step.
pub const LENGTHS: &[u64] = &[0, 1, 2, 7, 8];

/// One MiB of `a`. Built on demand so a scan that never asks for it pays nothing.
pub fn long_string() -> String {
    "a".repeat(LONG_STRING_LEN)
}

/// A number range wide enough to cross a guard that compares against a small constant, used
/// when the declared type states no range.
pub const DEFAULT_RANGE: f64 = 1_000_000.0;

/// A number boundary as a node.
///
/// The declared range filters the table: a parameter declared `0..=10` never receives
/// `i64::MAX`, because a value the type rules out is a type error rather than a finding. When
/// the range excludes every boundary, a value is drawn from inside the range instead.
pub fn number_node(rng: &mut Rng, bounds: NumberBounds) -> ValueNode {
    if bounds.int {
        let mut in_range: Vec<f64> = INTEGERS.iter().copied().map(|v| v as f64).filter(|v| in_declared(*v, bounds)).collect();
        for v in SMALL_INTEGERS {
            let v = v as f64;
            if in_declared(v, bounds) && !in_range.contains(&v) {
                in_range.push(v);
            }
        }
        let v = match rng.pick(&in_range) {
            Some(v) => *v,
            None => int_in_declared(rng, bounds) as f64,
        };
        return ValueNode::number(v);
    }
    let in_range: Vec<f64> = NUMBERS
        .iter()
        .copied()
        .filter(|v| in_declared(*v, bounds) && (!bounds.finite_only || v.is_finite()))
        .collect();
    let v = match rng.pick(&in_range) {
        Some(v) => *v,
        None => {
            let mut v = rng.float_in(bounds.min.unwrap_or(-DEFAULT_RANGE), bounds.max.unwrap_or(DEFAULT_RANGE));
            if bounds.finite_only && !v.is_finite() {
                v = 0.0;
            }
            v
        }
    };
    ValueNode::number(v)
}

/// Whether a candidate lies inside the declared range. A NaN fails every comparison, so it is
/// reachable only when the type states no range at all.
fn in_declared(v: f64, bounds: NumberBounds) -> bool {
    bounds.min.is_none_or(|min| v >= min) && bounds.max.is_none_or(|max| v <= max)
}

/// A number boundary drawn from inside the declared integer range.
pub fn int_in_declared(rng: &mut Rng, bounds: NumberBounds) -> i64 {
    let lo = clamp_to_i64(bounds.min.unwrap_or(-DEFAULT_RANGE));
    let hi = clamp_to_i64(bounds.max.unwrap_or(DEFAULT_RANGE));
    if lo <= hi {
        rng.int_in(lo, hi)
    } else {
        hi
    }
}

/// Clamp a declared float bound into the integer range without wrapping.
pub fn clamp_to_i64(v: f64) -> i64 {
    if v.is_nan() {
        0
    } else if v <= i64::MIN as f64 {
        i64::MIN
    } else if v >= i64::MAX as f64 {
        i64::MAX
    } else {
        v as i64
    }
}

/// A string boundary as a node.
///
/// The declared length bound filters the table, so a parameter declared `maxLength: 4` never
/// receives a 19-character probe. The one MiB entry is reachable only when the declared maximum
/// permits it: a type that states no bound is not treated as unbounded.
pub fn string_node(rng: &mut Rng, length: Option<LengthBounds>) -> ValueNode {
    let min = length.and_then(|l| l.min).unwrap_or(0);
    let max = length.and_then(|l| l.max).unwrap_or(u64::from(u32::MAX));
    if permits_long_string(length.and_then(|l| l.max)) && rng.chance(1, 16) {
        return ValueNode::string(long_string());
    }
    let in_range: Vec<&str> = STRINGS
        .iter()
        .copied()
        .filter(|s| {
            let len = s.chars().count() as u64;
            len >= min && len <= max
        })
        .collect();
    match rng.pick(&in_range) {
        Some(s) => ValueNode::string(*s),
        None => {
            let len = rng.between(min, min.max(max));
            ValueNode::string((0..len).map(|_| rng.ascii_char()).collect::<String>())
        }
    }
}

/// Whether a declared length bound permits the one MiB string boundary.
pub fn permits_long_string(declared_max: Option<u64>) -> bool {
    declared_max.is_some_and(|max| max >= LONG_STRING_LEN as u64)
}

/// A `bigint` boundary as a node.
pub fn bigint_node(rng: &mut Rng) -> ValueNode {
    ValueNode::BigInt { value: (*rng.pick(BIGINTS).unwrap_or(&"0")).to_string() }
}

/// A collection length boundary, clamped to a declared bound when there is one.
pub fn length(rng: &mut Rng, declared_min: Option<u64>, declared_max: Option<u64>) -> u64 {
    let mut picks: Vec<u64> = LENGTHS
        .iter()
        .copied()
        .filter(|n| declared_min.is_none_or(|m| *n >= m) && declared_max.is_none_or(|x| *n <= x))
        .collect();
    if picks.is_empty() {
        picks.push(declared_min.unwrap_or(0));
    }
    *rng.pick(&picks).unwrap_or(&0)
}

/// Wrap a boundary node so its provenance says where it came from.
pub fn boundary(node: ValueNode) -> ValueTree {
    ValueTree::with_provenance(node, afz_protocol::Provenance::Boundary)
}

/// The `NumberValue` for a boundary, used by code that needs the value rather than the node.
pub fn number_value(v: f64) -> NumberValue {
    NumberValue::from_f64(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use afz_protocol::Provenance;

    #[test]
    fn the_table_holds_the_values_the_design_names() {
        assert!(NUMBERS.contains(&0.0) && NUMBERS.contains(&1.0) && NUMBERS.contains(&-1.0));
        // NaN is never equal to itself, so `contains` cannot find it.
        assert!(NUMBERS.iter().any(|v| v.is_nan()) && NUMBERS.contains(&f64::INFINITY));
        assert!(NUMBERS.contains(&(i64::MIN as f64)) && NUMBERS.contains(&(i64::MAX as f64)));
        assert!(STRINGS.contains(&""));
        assert_eq!(long_string().len(), LONG_STRING_LEN);
        assert!(LENGTHS.contains(&0) && LENGTHS.contains(&1));
        assert!(INTEGERS.contains(&i64::MIN) && INTEGERS.contains(&i64::MAX));
    }

    #[test]
    fn an_int_declaration_never_draws_a_non_integer() {
        let mut rng = Rng::seeded(3);
        for _ in 0..200 {
            let ValueNode::Number { value } = number_node(&mut rng, NumberBounds { int: true, ..NumberBounds::default() }) else {
                panic!("int boundary must be a number")
            };
            let v = value.as_f64();
            assert!(v.fract() == 0.0, "{v} is not an integer");
        }
    }

    #[test]
    fn finite_only_skips_nan_and_infinity() {
        let mut rng = Rng::seeded(4);
        for _ in 0..400 {
            let ValueNode::Number { value } = number_node(&mut rng, NumberBounds { finite_only: true, ..NumberBounds::default() }) else { panic!() };
            assert!(value.as_f64().is_finite(), "{} drew a non-finite value", value.as_f64());
        }
    }

    #[test]
    fn a_declared_number_range_filters_the_table() {
        let mut rng = Rng::seeded(12);
        let small = NumberBounds { int: true, finite_only: true, min: Some(0.0), max: Some(10.0) };
        for _ in 0..500 {
            let ValueNode::Number { value } = number_node(&mut rng, small) else { panic!("not a number") };
            let v = value.as_f64();
            assert!((0.0..=10.0).contains(&v), "i64::MAX reached a 0..=10 parameter");
        }
        // A range that excludes every table entry still draws from inside the range.
        let tight = NumberBounds { int: true, finite_only: true, min: Some(3.0), max: Some(4.0) };
        for _ in 0..100 {
            let ValueNode::Number { value } = number_node(&mut rng, tight) else { panic!("not a number") };
            assert!((3.0..=4.0).contains(&value.as_f64()));
        }
    }

    #[test]
    fn a_declared_string_length_filters_the_table() {
        let mut rng = Rng::seeded(13);
        let short = Some(LengthBounds { min: Some(1), max: Some(3) });
        for _ in 0..500 {
            let ValueNode::String { value } = string_node(&mut rng, short) else { panic!("not a string") };
            assert!((1..=3).contains(&value.chars().count()), "{value:?} ignores the declared length");
        }
        // The one MiB entry needs a declared maximum that permits it.
        assert!(!permits_long_string(None));
        assert!(!permits_long_string(Some(64)));
        assert!(permits_long_string(Some(LONG_STRING_LEN as u64)));
        let roomy = Some(LengthBounds { min: None, max: Some(2 * LONG_STRING_LEN as u64) });
        let mut saw_long = false;
        for _ in 0..4000 {
            if let ValueNode::String { value } = string_node(&mut rng, roomy) {
                saw_long |= value.len() == LONG_STRING_LEN;
            }
        }
        assert!(saw_long, "the one MiB boundary is unreachable from a type that permits it");
    }

    #[test]
    fn a_declared_bound_clamps_the_drawn_length() {
        let mut rng = Rng::seeded(5);
        for _ in 0..200 {
            let n = length(&mut rng, Some(3), Some(4));
            assert!((3..=4).contains(&n), "{n} escaped the declared range");
        }
        // An impossible range still returns a length rather than panicking.
        assert_eq!(length(&mut rng, Some(9), Some(2)), 9);
    }

    #[test]
    fn a_boundary_node_carries_its_provenance() {
        let tree = boundary(ValueNode::string(""));
        assert_eq!(tree.provenance, Provenance::Boundary);
        assert!(number_value(f64::NAN).as_f64().is_nan());
    }
}

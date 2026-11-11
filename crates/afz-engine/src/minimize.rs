//! Delta-debugging over a [`ValueTree`].
//!
//! Minimization runs the same call with a smaller input and keeps the smaller input only when
//! the target still breaks the same way. "The same way" is the fault class plus the top frame,
//! which is exactly what a finding's identity is computed from, so a candidate cannot move the
//! bug to a different line and still be accepted.
//!
//! The search is greedy with restarts. Each pass walks every node of every argument in a stable
//! pre-order and tries a short ladder of smaller values for it, most aggressive first. A pass
//! that changes nothing ends the search. Nothing here is random, so the same input against the
//! same target minimizes to the same bytes on every machine.
//!
//! The caller supplies the re-run as a closure, which keeps this module away from the transport:
//! production passes a plugin call, a test passes a table.

use afz_protocol::{NodePath, Outcome, ValueNode, ValueTree};

/// The fault a candidate must still produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultMatch {
    class: String,
    /// The top frame's file and line. A fault with no frames — a wasm trap, an engine-raised
    /// deadline — is matched on its class alone, because there is no frame to agree on.
    frame: Option<(String, u32)>,
}

impl FaultMatch {
    /// `None` when the outcome is not a failure: there is then nothing for a candidate to
    /// reproduce, and minimization has no business running.
    pub fn from_outcome(outcome: &Outcome) -> Option<FaultMatch> {
        let fault = outcome.fault()?;
        let frame = fault.top_frame().map(|f| (f.file.clone(), f.line));
        Some(FaultMatch { class: fault.class.clone(), frame })
    }

    pub fn reproduces(&self, outcome: &Outcome) -> bool {
        let Some(fault) = outcome.fault() else { return false };
        if fault.class != self.class {
            return false;
        }
        match (&self.frame, fault.top_frame()) {
            (None, _) => true,
            (Some((file, line)), Some(top)) => top.file == *file && top.line == *line,
            (Some(_), None) => false,
        }
    }
}

/// The result of one minimization run.
#[derive(Debug, Clone)]
pub struct Minimized {
    pub values: Vec<ValueTree>,
    /// Calls the closure needed, rejected candidates included.
    pub attempts: u64,
    pub bytes_before: usize,
    pub bytes_after: usize,
    /// False when the original input did not reproduce on the first re-run. The finding is then
    /// reported as not reproduced rather than minimized against a fault that is not there.
    pub reproduces: bool,
}

/// Shrink `values` as far as it goes while `run` keeps reporting the same fault.
///
/// `max_attempts` bounds the work: a scan should not spend more shrinking one finding than it
/// spent finding it.
pub fn minimize<F>(values: &[ValueTree], max_attempts: u64, run: &mut F) -> Minimized
where
    F: FnMut(&[ValueTree]) -> Outcome,
{
    let bytes_before = total_bytes(values);
    let mut current = values.to_vec();
    let mut attempts = 0u64;

    let Some(target) = (attempts < max_attempts).then(|| {
        attempts += 1;
        FaultMatch::from_outcome(&run(&current))
    }).flatten() else {
        let bytes_after = total_bytes(&current);
        return Minimized { values: current, attempts, bytes_before, bytes_after, reproduces: false };
    };

    'passes: loop {
        let mut changed = false;

        // An argument the target never needed can go entirely: `undefined` in its position is
        // what a caller who passed fewer arguments would produce.
        for index in 0..current.len() {
            if attempts >= max_attempts {
                break 'passes;
            }
            attempts += 1;
            let mut trial = current.clone();
            trial[index] = ValueTree::undefined();
            if target.reproduces(&run(&trial)) {
                current = trial;
                changed = true;
            }
        }

        // Then every node inside the arguments, in the order `paths` reports them.
        for index in 0..current.len() {
            for path in current[index].paths() {
                let Some(node) = node_at(&current[index], &path) else { continue };
                for candidate in ladder(node) {
                    if attempts >= max_attempts {
                        break 'passes;
                    }
                    attempts += 1;
                    let mut trial = current.clone();
                    if !path.replace(&mut trial[index], ValueTree::new(candidate)) {
                        continue;
                    }
                    if target.reproduces(&run(&trial)) {
                        current = trial;
                        changed = true;
                        break;
                    }
                }
            }
        }

        if !changed {
            break;
        }
    }

    Minimized { bytes_after: total_bytes(&current), values: current, attempts, bytes_before, reproduces: true }
}

fn total_bytes(values: &[ValueTree]) -> usize {
    values.iter().map(ValueTree::wire_bytes).sum()
}

/// The node a path addresses. Paths come from a snapshot of the tree, so a shrink accepted
/// earlier in the same pass can leave a later path pointing at nothing.
fn node_at<'a>(tree: &'a ValueTree, path: &NodePath) -> Option<&'a ValueNode> {
    path.get(tree).map(|node| &node.node)
}

/// The smaller values to try for a node, most aggressive first. Every candidate is a node the
/// generator could itself have produced, so a minimized input stays a legal `ValueTree`.
fn ladder(node: &ValueNode) -> Vec<ValueNode> {
    let candidates = match node {
        // A stub's behavior is part of what the input means: turning `throw` into `returnFirst`
        // changes the call rather than its size. An opaque node can only come from a fixture.
        ValueNode::Function { .. } | ValueNode::Opaque { .. } | ValueNode::Undefined | ValueNode::Null => return Vec::new(),
        ValueNode::Boolean { value } => {
            if *value {
                vec![ValueNode::Boolean { value: false }]
            } else {
                Vec::new()
            }
        }
        ValueNode::String { value } => {
            let chars: Vec<char> = value.chars().collect();
            let mut out = vec![ValueNode::string("")];
            if chars.len() > 1 {
                out.push(ValueNode::string(chars[..chars.len() / 2].iter().collect::<String>()));
            }
            out
        }
        ValueNode::Number { value } => {
            let mut out = vec![ValueNode::number(0.0)];
            let half = value.as_f64() / 2.0;
            if half != value.as_f64() && half != 0.0 {
                out.push(ValueNode::number(half));
            }
            out
        }
        ValueNode::BigInt { value } => {
            if value == "0" {
                Vec::new()
            } else {
                vec![ValueNode::BigInt { value: "0".to_string() }]
            }
        }
        ValueNode::Literal { value } => [
            serde_json::Value::Null,
            serde_json::Value::Bool(false),
            serde_json::Value::from(0),
            serde_json::Value::String(String::new()),
        ]
        .into_iter()
        .filter(|candidate| candidate != value)
        .map(|value| ValueNode::Literal { value })
        .collect(),
        ValueNode::Buffer { bytes } => {
            let mut out = vec![ValueNode::Buffer { bytes: Vec::new() }];
            if bytes.len() > 1 {
                out.push(ValueNode::Buffer { bytes: bytes[..bytes.len() / 2].to_vec() });
            }
            out
        }
        ValueNode::Array { items } => shrink(items, |kept| ValueNode::Array { items: kept }),
        ValueNode::Set { items } => shrink(items, |kept| ValueNode::Set { items: kept }),
        ValueNode::Tuple { items } => shrink(items, |kept| ValueNode::Tuple { items: kept }),
        ValueNode::Stream { iterable, items } => {
            let iterable = *iterable;
            shrink(items, move |kept| ValueNode::Stream { iterable, items: kept })
        }
        ValueNode::Map { entries } => shrink(entries, |kept| ValueNode::Map { entries: kept }),
        ValueNode::Object { fields } => shrink(fields, |kept| ValueNode::Object { fields: kept }),
    };
    candidates.into_iter().filter(|candidate| candidate != node).collect()
}

/// `[]`, then the first element alone, then the list with one element removed at a time.
fn shrink<T: Clone>(items: &[T], build: impl Fn(Vec<T>) -> ValueNode) -> Vec<ValueNode> {
    let mut out = vec![build(Vec::new())];
    if items.len() > 1 {
        out.push(build(items[..1].to_vec()));
        for skip in 0..items.len() {
            out.push(build(items.iter().enumerate().filter(|(i, _)| *i != skip).map(|(_, v)| v.clone()).collect()));
        }
    }
    out
}

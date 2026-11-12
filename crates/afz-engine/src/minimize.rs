//! Delta-debugging over a [`ValueTree`].
//!
//! Minimization runs the same call with a smaller input and keeps the smaller input only when
//! the target still breaks the same way. "The same way" is the fault class plus the top frame,
//! which is exactly what a finding's identity is computed from, so a candidate cannot move the
//! bug to a different line and still be accepted.
//!
//! The search is delta-debugging over the parts of each node, plus a pass that replaces scalars
//! with smaller ones. For one node it tries the empty node first, then drops chunks of its parts -
//! children of a collection, lines of a string, runs of a buffer - and halves the chunk size only
//! when no chunk can go, so a long input sheds most of itself in a handful of trials rather than one
//! trial per part. A successful drop restarts the walk from the shrunken tree, so a path is never
//! carried across a change that moved what it addressed.
//!
//! Dropping whole lines and whole byte runs is what lets a defect near the end of a long string shed
//! everything before it. Halving from the front cannot express that, and the reported input kept the
//! irrelevant prefix.
//!
//! Candidates within one round all derive from the same base, so they are handed to the runner in
//! chunks and answered in parallel. A candidate is accepted by its position in the round, never by
//! when its answer arrives, so the same input against the same target minimizes to the same bytes on
//! every machine.
//!
//! The caller supplies the re-run as a closure over a chunk of candidates, which keeps this module
//! away from the transport: production runs a chunk across a pool of plugin connections, a test
//! passes a table.

use std::time::{Duration, Instant};

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

/// How many runs a single-line string or a buffer is cut into when it has no natural boundary.
/// Without a cap a one-megabyte string would become a million candidates.
const MAX_RUNS: usize = 32;

/// A node whose contents can be made smaller by dropping parts, addressed by argument and path.
struct Hole {
    index: usize,
    path: NodePath,
}

fn total_bytes(values: &[ValueTree]) -> usize {
    values.iter().map(ValueTree::wire_bytes).sum()
}

/// The line ranges of a string, or equal runs when it has no line break.
fn ranges(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    for (at, c) in chars.iter().enumerate() {
        if *c == '\n' {
            out.push((start, at + 1));
            start = at + 1;
        }
    }
    if start < chars.len() {
        out.push((start, chars.len()));
    }
    if out.len() > 1 {
        return out;
    }
    runs_of(chars.len())
}

/// Equal byte or char runs, capped at `MAX_RUNS`.
fn runs_of(len: usize) -> Vec<(usize, usize)> {
    let step = len.div_ceil(MAX_RUNS).max(1);
    let mut out = Vec::new();
    let mut start = 0;
    while start < len {
        let end = (start + step).min(len);
        out.push((start, end));
        start = end;
    }
    out
}

/// How many parts a node offers the search. Zero means there is nothing to drop.
fn part_count(node: &ValueNode) -> usize {
    match node {
        ValueNode::Array { items } | ValueNode::Set { items } | ValueNode::Tuple { items } | ValueNode::Stream { items, .. } => items.len(),
        ValueNode::Map { entries } => entries.len(),
        ValueNode::Object { fields } => fields.len(),
        ValueNode::String { value } => ranges(value).len(),
        ValueNode::Buffer { bytes } => runs_of(bytes.len()).len(),
        _ => 0,
    }
}

/// The same node with only the named parts kept. `parts` indexes the node as `part_count` saw it,
/// so a caller that drops several chunks of one node still holds valid indices.
fn keep(node: &ValueNode, parts: &[usize]) -> Option<ValueNode> {
    let trees = |src: &[ValueTree]| -> Vec<ValueTree> {
        src.iter().enumerate().filter(|(at, _)| parts.contains(at)).map(|(_, value)| value.clone()).collect()
    };
    let pairs = |src: &[(ValueTree, ValueTree)]| -> Vec<(ValueTree, ValueTree)> {
        src.iter().enumerate().filter(|(at, _)| parts.contains(at)).map(|(_, entry)| entry.clone()).collect()
    };
    let named = |src: &[(String, ValueTree)]| -> Vec<(String, ValueTree)> {
        src.iter().enumerate().filter(|(at, _)| parts.contains(at)).map(|(_, field)| field.clone()).collect()
    };
    match node {
        ValueNode::Array { items } => Some(ValueNode::Array { items: trees(items) }),
        ValueNode::Set { items } => Some(ValueNode::Set { items: trees(items) }),
        ValueNode::Tuple { items } => Some(ValueNode::Tuple { items: trees(items) }),
        ValueNode::Stream { iterable, items } => Some(ValueNode::Stream { iterable: *iterable, items: trees(items) }),
        ValueNode::Map { entries } => Some(ValueNode::Map { entries: pairs(entries) }),
        ValueNode::Object { fields } => Some(ValueNode::Object { fields: named(fields) }),
        ValueNode::String { value } => {
            let chars: Vec<char> = value.chars().collect();
            let lines = ranges(value);
            let text = parts
                .iter()
                .filter_map(|at| lines.get(*at))
                .map(|(from, to)| chars[*from..*to].iter().collect::<String>())
                .collect();
            Some(ValueNode::String { value: text })
        }
        ValueNode::Buffer { bytes } => {
            let runs = runs_of(bytes.len());
            let cut = parts
                .iter()
                .filter_map(|at| runs.get(*at))
                .flat_map(|(from, to)| bytes[*from..*to].iter().copied())
                .collect();
            Some(ValueNode::Buffer { bytes: cut })
        }
        _ => None,
    }
}

/// Every node with parts to drop, in the pre-order `paths` reports.
fn holes(base: &[ValueTree]) -> Vec<Hole> {
    let mut out = Vec::new();
    for (index, tree) in base.iter().enumerate() {
        for path in tree.paths() {
            let Some(node) = path.get(tree).map(|node| &node.node) else { continue };
            if part_count(node) > 0 {
                out.push(Hole { index, path });
            }
        }
    }
    out
}

/// `base` with the node at the hole's path rebuilt from `original`, keeping only `parts`.
fn with_kept(original: &ValueNode, base: &[ValueTree], hole: &Hole, parts: &[usize]) -> Option<Vec<ValueTree>> {
    let mut trial = base.to_vec();
    hole.path.replace(&mut trial[hole.index], ValueTree::new(keep(original, parts)?)).then_some(trial)
}

/// The parts in `n` near-equal chunks.
fn split<T: Clone>(items: &[T], n: usize) -> Vec<Vec<T>> {
    let size = items.len().div_ceil(n.max(1)).max(1);
    items.chunks(size).map(|chunk| chunk.to_vec()).collect()
}

/// The smaller scalar to try for a node, most aggressive first. Collections, strings and buffers are
/// not here: dropping their parts is the hole search's job, and a candidate the generator could not
/// have produced is not a legal input.
fn scalar_ladder(node: &ValueNode) -> Vec<ValueNode> {
    let candidates = match node {
        ValueNode::Boolean { value } if *value => vec![ValueNode::Boolean { value: false }],
        ValueNode::Number { value } => {
            let mut out = vec![ValueNode::number(0.0)];
            let half = value.as_f64() / 2.0;
            if half != value.as_f64() && half != 0.0 {
                out.push(ValueNode::number(half));
            }
            out
        }
        ValueNode::BigInt { value } if value != "0" => vec![ValueNode::BigInt { value: "0".to_string() }],
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
        _ => Vec::new(),
    };
    candidates.into_iter().filter(|candidate| candidate != node).collect()
}

/// The candidate plans of one round, answered in chunks under the attempt and time bounds.
struct Search<'r, F> {
    run: &'r mut F,
    width: usize,
    max_attempts: u64,
    budget: Duration,
    started: Instant,
    attempts: u64,
}

impl<F> Search<'_, F>
where
    F: FnMut(&[Vec<ValueTree>]) -> Vec<Outcome>,
{
    fn out_of_work(&self) -> bool {
        self.attempts >= self.max_attempts || self.started.elapsed() >= self.budget
    }

    /// The index of the first plan that reproduces `target`, or `None`. Plans are answered in
    /// `width`-sized chunks, so the answer is the same whether the runner is serial or parallel.
    fn first_hit(&mut self, target: &FaultMatch, plans: &[Vec<ValueTree>]) -> Option<usize> {
        let mut base = 0usize;
        while base < plans.len() {
            if self.out_of_work() {
                break;
            }
            let room = (self.max_attempts - self.attempts) as usize;
            let end = (base + self.width.min(room)).min(plans.len());
            let chunk = &plans[base..end];
            self.attempts += chunk.len() as u64;
            let outcomes = (self.run)(chunk);
            debug_assert_eq!(outcomes.len(), chunk.len(), "the runner answers every candidate it was given");
            if let Some(offset) = outcomes.iter().position(|outcome| target.reproduces(outcome)) {
                return Some(base + offset);
            }
            base = end;
        }
        None
    }
}

/// Shrink `values` as far as it goes while `run` keeps reporting the same fault.
///
/// `run` receives a chunk of candidate inputs and answers each one. `width` is how many candidates
/// one call receives; 1 makes the search serial.
///
/// `max_attempts` and `budget` bound the work: a scan should not spend more shrinking one
/// finding than it spent finding it. The time bound is the one that bites when the fault is a
/// deadline, because an attempt that reproduces it costs the whole deadline every time.
pub fn minimize<F>(values: &[ValueTree], max_attempts: u64, budget: Duration, width: usize, run: &mut F) -> Minimized
where
    F: FnMut(&[Vec<ValueTree>]) -> Vec<Outcome>,
{
    let bytes_before = total_bytes(values);
    let mut current = values.to_vec();
    let mut search = Search { run, width: width.max(1), max_attempts, budget, started: Instant::now(), attempts: 0 };

    let Some(target) = (search.attempts < max_attempts)
        .then(|| {
            search.attempts += 1;
            (search.run)(&[current.clone()]).first().and_then(FaultMatch::from_outcome)
        })
        .flatten()
    else {
        let bytes_after = total_bytes(&current);
        return Minimized { values: current, attempts: search.attempts, bytes_before, bytes_after, reproduces: false };
    };

    // One round is every smaller input that can be built from the current tree without waiting for
    // another trial's answer: each argument dropped, the empty node at each node that has parts, a
    // chunk of parts dropped at each such node, and every smaller scalar. The candidates are
    // independent, so they are handed to the runner `width` at a time and answered on as many
    // connections as there are. The dependency between rounds is real; the work inside one is not.
    //
    // A round that reproduces becomes the new base and the walk starts again, because a path is
    // never carried across a change that moved what it addressed. A round that does not doubles the
    // chunk count, so the search converges at one-part granularity, where every surviving part has
    // been seen to be necessary. Attempts only ever grow, so even a candidate that changes nothing
    // ends at `max_attempts` instead of looping.
    let mut chunks = MIN_CHUNKS;
    loop {
        if search.out_of_work() {
            break;
        }
        let found = holes(&current);
        let widest = found.iter().map(|hole| parts_at(&current, hole)).max().unwrap_or(0);
        // A slot that is already `undefined` offers no candidate: dropping that argument is the
        // input it already is, and a candidate identical to its base is accepted by every fault, so
        // the search would re-accept it every round until the attempt bound ended the run. One such
        // candidate cost a real scan 2000 trials and 189 s to shed nothing.
        let mut plans: Vec<Vec<ValueTree>> = (0..current.len())
            .filter(|index| !matches!(current[*index].node, ValueNode::Undefined))
            .map(|index| {
                let mut trial = current.clone();
                trial[index] = ValueTree::undefined();
                trial
            })
            .collect();
        for hole in &found {
            let Some(original) = hole.path.get(&current[hole.index]).map(|node| node.node.clone()) else { continue };
            let total = part_count(&original);
            if total == 0 {
                continue;
            }
            // The empty node first: the most aggressive cut is also the cheapest to ask for.
            if let Some(empty) = with_kept(&original, &current, hole, &[]) {
                plans.push(empty);
            }
            for drop in split(&(0..total).collect::<Vec<usize>>(), chunks) {
                let parts: Vec<usize> = (0..total).filter(|part| !drop.contains(part)).collect();
                if let Some(trial) = with_kept(&original, &current, hole, &parts) {
                    plans.push(trial);
                }
            }
        }
        for (index, tree) in current.iter().enumerate() {
            for path in tree.paths() {
                let Some(node) = path.get(tree).map(|node| &node.node) else { continue };
                for candidate in scalar_ladder(node) {
                    let mut trial = current.clone();
                    if path.replace(&mut trial[index], ValueTree::new(candidate)) {
                        plans.push(trial);
                    }
                }
            }
        }
        match search.first_hit(&target, &plans) {
            Some(hit) => {
                current = plans[hit].clone();
                chunks = MIN_CHUNKS;
            }
            None => {
                if chunks >= widest {
                    break;
                }
                chunks = (chunks * 2).min(widest);
            }
        }
    }

    Minimized { bytes_after: total_bytes(&current), values: current, attempts: search.attempts, bytes_before, reproduces: true }
}

/// The chunk count a round starts at, and what it returns to after a round is accepted. A round is
/// already wide, because it spans every node of the input, and the runner answers a whole round in
/// one batch - so widening this only spends more trials on the same wall time. It doubles each time
/// a round fails, which is what drives the search to one-part granularity.
const MIN_CHUNKS: usize = 2;

/// How many parts the node at `hole` offers in `base`.
fn parts_at(base: &[ValueTree], hole: &Hole) -> usize {
    hole.path.get(&base[hole.index]).map(|node| part_count(&node.node)).unwrap_or(0)
}

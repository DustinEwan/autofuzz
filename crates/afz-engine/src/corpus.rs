//! The corpus: the inputs the engine keeps, and the novelty rule that decides what is kept.
//!
//! An edge set is a `BTreeSet`, never a `HashSet`: the scan must replay byte-for-byte from a
//! seed, and a `HashSet` iterates in an order that depends on insertion history and hasher
//! state.
//!
//! The corpus does not know what an edge *is*. With the `coverage` capability the ids come from
//! the plugin's stable edge identities; without it, [`outcome_digest`] builds ids from the
//! observable outcome tuple. The retention rule below is identical in both cases, which is what
//! the design's "the loop is unchanged; only the novelty function differs" requires.

use std::collections::{BTreeMap, BTreeSet};

use afz_protocol::{EdgeId, Outcome, ValueTree};
use sha2::{Digest as _, Sha256};

/// The edge set of one invocation. Sorted by construction.
pub type CoverageDigest = BTreeSet<EdgeId>;

/// A retained input and the coverage it earned.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Content-addressed: the same input for the same target always produces the same id, so a
    /// resumed scan recognises an entry it has already seen.
    pub id: String,
    pub target_id: String,
    pub values: Vec<ValueTree>,
    pub edges: CoverageDigest,
    /// Times this entry was selected for mutation.
    pub executions: u64,
    /// Edges this entry contributed to the corpus when it was retained.
    pub new_edges: u32,
    /// Invocation index at which this entry was last selected.
    pub last_used: u64,
}

/// What the corpus did with an invocation's edge set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// Retained, with the edge count it added.
    Retained { id: String, new_edges: u32 },
    /// Every edge was already covered, so the input was dropped.
    NotNovel,
    /// The corpus was at its cap and nothing could be culled, so the input was refused.
    Refused,
}

/// The retained inputs, their union of edges, and the cap.
#[derive(Debug, Clone)]
pub struct Corpus {
    cap: u64,
    /// Keyed by id so iteration is deterministic.
    entries: BTreeMap<String, Entry>,
    covered: BTreeSet<EdgeId>,
    /// Insertions the cap turned away. Reported in the scan result: a corpus that stopped
    /// growing is information the reader needs, not a silent condition.
    refused: u64,
}

impl Corpus {
    pub fn new(cap: u64) -> Corpus {
        Corpus { cap, entries: BTreeMap::new(), covered: BTreeSet::new(), refused: 0 }
    }

    pub fn cap(&self) -> u64 {
        self.cap
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn refused(&self) -> u64 {
        self.refused
    }

    /// Every edge id the corpus has produced. Monotonic: edges are only ever added.
    pub fn covered(&self) -> &BTreeSet<EdgeId> {
        &self.covered
    }

    pub fn covered_count(&self) -> u64 {
        self.covered.len() as u64
    }

    pub fn get(&self, id: &str) -> Option<&Entry> {
        self.entries.get(id)
    }

    /// Entries in id order, which is the order the scheduler walks for its visit guarantee.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    /// Whether `values` was already retained for `target_id`.
    pub fn contains(&self, target_id: &str, values: &[ValueTree]) -> bool {
        self.entries.contains_key(&entry_id(target_id, values))
    }

    /// Record one invocation. Retention follows the design: keep the input when its edge set
    /// contains an edge id the corpus has not produced.
    pub fn record(&mut self, target_id: &str, values: &[ValueTree], edges: &CoverageDigest, invocation: u64) -> Recorded {
        let id = entry_id(target_id, values);
        let new_edges: Vec<EdgeId> = edges.difference(&self.covered).copied().collect();
        if let Some(entry) = self.entries.get_mut(&id) {
            // The same input again. Its edge set is merged rather than ignored: a target can
            // report an edge it did not report on an earlier run of the same input, and an edge
            // added to the union without a holding entry would break the invariant that every
            // covered edge has a live holder.
            entry.edges.extend(edges.iter().copied());
            self.covered.extend(edges.iter().copied());
            return if new_edges.is_empty() {
                Recorded::NotNovel
            } else {
                entry.new_edges += new_edges.len() as u32;
                Recorded::Retained { id, new_edges: new_edges.len() as u32 }
            };
        }
        if new_edges.is_empty() {
            return Recorded::NotNovel;
        }
        if self.entries.len() as u64 >= self.cap && !self.make_room() {
            self.refused += 1;
            return Recorded::Refused;
        }
        self.covered.extend(edges.iter().copied());
        self.entries.insert(
            id.clone(),
            Entry {
                id: id.clone(),
                target_id: target_id.to_string(),
                values: values.to_vec(),
                edges: edges.clone(),
                executions: 0,
                new_edges: new_edges.len() as u32,
                last_used: invocation,
            },
        );
        Recorded::Retained { id, new_edges: new_edges.len() as u32 }
    }

    /// Count a selection against an entry.
    pub fn note_use(&mut self, id: &str, invocation: u64) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.executions += 1;
            entry.last_used = invocation;
        }
    }

    /// Free one slot, following the design's narrow cull rule, and report whether it worked.
    ///
    /// An entry is cullable only when another entry's edge set is a superset of it *and* that
    /// other entry was used more recently, and never when it is the sole holder of an edge. When
    /// nothing qualifies the caller refuses the new input rather than dropping something the
    /// design protects: the cap may then be reached exactly, but no coverage is ever thrown away
    /// to make room.
    fn make_room(&mut self) -> bool {
        let Some(victim) = self.cullable().into_iter().next() else { return false };
        let Some(entry) = self.entries.remove(&victim) else { return false };
        // The union only shrinks when an edge has no other holder, which the cull rule forbids,
        // so `covered` stays as it is.
        debug_assert!(
            entry.edges.iter().all(|e| self.entries.values().any(|other| other.edges.contains(e))),
            "a sole-edge holder was culled"
        );
        true
    }

    /// Cullable entries, least useful first: fewest edges, then id, so the choice is
    /// deterministic and does not depend on map order.
    fn cullable(&self) -> Vec<String> {
        let mut candidates: Vec<(usize, String)> = self
            .entries
            .values()
            .filter(|entry| self.dominated(entry))
            .map(|entry| (entry.edges.len(), entry.id.clone()))
            .collect();
        candidates.sort();
        candidates.into_iter().map(|(_, id)| id).collect()
    }

    /// Whether `entry` is dominated: some other entry covers everything it covers, was used at
    /// least as recently, and `entry` holds no edge on its own.
    fn dominated(&self, entry: &Entry) -> bool {
        if !self.sole_edges(entry).is_empty() {
            return false;
        }
        self.entries
            .values()
            .any(|other| other.id != entry.id && other.last_used >= entry.last_used && other.edges.is_superset(&entry.edges))
    }

    /// Edges only this entry covers. Such an entry is never culled.
    pub fn sole_edges(&self, entry: &Entry) -> Vec<EdgeId> {
        entry
            .edges
            .iter()
            .copied()
            .filter(|edge| {
                !self
                    .entries
                    .values()
                    .any(|other| other.id != entry.id && other.edges.contains(edge))
            })
            .collect()
    }

    /// Every edge in `covered` must still be held by a live entry. The scan asserts this at the
    /// end of a run; the test asserts it after every step.
    pub fn uncovered_edges(&self) -> Vec<EdgeId> {
        self.covered
            .iter()
            .filter(|edge| !self.entries.values().any(|entry| entry.edges.contains(edge)))
            .copied()
            .collect()
    }
}

/// The content address of an input for a target.
///
/// The hash covers the serialized tree, provenance included. Excluding provenance looks like a
/// cleanliness win and is not one. Retention requires an edge set the corpus has not produced, and
/// two trees that differ only in provenance make the same call and therefore report the same
/// edges, so the second is never retained and no corpus capacity is wasted: a measured 85-entry
/// scan of the billing test target held zero entries sharing a target and a value. `Corpus::contains`
/// is the only other reader of the address and has no callers. What the address does drive is
/// ordering, because the corpus is keyed by it and the donor pool draws from that order, so
/// changing it re-numbers every entry and moves the search trajectory. Treat it as frozen.
pub fn entry_id(target_id: &str, values: &[ValueTree]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(target_id.as_bytes());
    hasher.update([0]);
    let canonical = serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string());
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(12);
    out.push_str("afz:c:");
    for byte in &digest[..3] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The novelty id space when the plugin has no `coverage` capability.
///
/// The design names the tuple: status class or error class, response shape, latency bucket, and
/// whether the invocation crossed a deadline. Each component hashes into a wide id space, so two
/// invocations collide only when their whole tuple matches.
pub fn outcome_digest(outcome: &Outcome) -> CoverageDigest {
    let mut digest = CoverageDigest::new();
    digest.insert(component(&status_class(outcome)));
    digest.insert(component(&format!("latency:{}", latency_bucket(outcome.duration_ms()))));
    if let Some(shape) = response_shape(outcome) {
        digest.insert(component(&shape));
    }
    digest
}

/// `ok`, `rejected:<via>`, `failed:<via>:<fault class>`. The `via` channel is part of novelty
/// because a target that rejects through a different rule took a different path.
fn status_class(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ok(_) => "ok".to_string(),
        Outcome::Rejected(r) => format!("rejected:{}", r.via),
        Outcome::Failed(f) => format!("failed:{}:{}", f.via, f.fault.class),
    }
}

/// The value's node kind, for an `ok` that returned a value. A target that returns a different
/// shape for the same input reached a different branch.
fn response_shape(outcome: &Outcome) -> Option<String> {
    match outcome {
        Outcome::Ok(ok) => ok.value.as_ref().map(|v| format!("shape:{:?}", v.node.kind())),
        _ => None,
    }
}

/// A power-of-two latency bucket, so 12 ms and 13 ms are one observation and 12 ms and 40 ms
/// are not.
fn latency_bucket(ms: f64) -> u32 {
    if !ms.is_finite() || ms < 1.0 {
        0
    } else {
        64 - (ms as u64).leading_zeros()
    }
}

/// FNV-1a over the component text. Fixed arithmetic, so a novelty id is stable across runs and
/// platforms, which `DefaultHasher` does not promise.
fn component(text: &str) -> EdgeId {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    // Keep ids in the low 31 bits so they stay readable and cannot collide with a plugin's own
    // edge numbering, which starts at zero in a fresh module.
    (hash & 0x7fff_ffff) as EdgeId
}

#[cfg(test)]
mod tests {
    use super::*;
    use afz_protocol::{FaultSignature, ValueNode};

    fn outcome_ok(ms: f64) -> Outcome {
        Outcome::Ok(afz_protocol::OkOutcome { duration_ms: ms, memory_bytes: None, coverage: None, value: None })
    }

    #[test]
    fn an_input_is_retained_only_for_an_edge_the_corpus_has_not_seen() {
        let mut corpus = Corpus::new(10);
        let values = vec![ValueTree::number(1.0)];
        let mut first = CoverageDigest::new();
        first.insert(1);
        assert!(matches!(corpus.record("t", &values, &first, 0), Recorded::Retained { new_edges: 1, .. }));
        assert!(matches!(corpus.record("t", &values, &first, 1), Recorded::NotNovel), "the same input twice is not novelty");

        let mut same_edge = CoverageDigest::new();
        same_edge.insert(1);
        let other = vec![ValueTree::number(2.0)];
        assert_eq!(corpus.record("t", &other, &same_edge, 2), Recorded::NotNovel);

        let novel: CoverageDigest = [1, 2].into_iter().collect();
        let mut grown = novel.clone();
        assert!(matches!(corpus.record("t", &other, &novel, 3), Recorded::Retained { new_edges: 1, .. }));
        assert_eq!(corpus.covered_count(), 2);
        grown.insert(3);
        assert!(matches!(corpus.record("t", &other, &grown, 4), Recorded::Retained { .. }));
    }

    #[test]
    fn the_same_input_for_another_target_is_a_different_entry() {
        let mut corpus = Corpus::new(10);
        let values = vec![ValueTree::number(1.0)];
        let edges: CoverageDigest = [7].into_iter().collect();
        assert!(matches!(corpus.record("a", &values, &edges, 0), Recorded::Retained { .. }));
        assert!(matches!(corpus.record("b", &values, &edges, 1), Recorded::NotNovel));
        let other: CoverageDigest = [8].into_iter().collect();
        assert!(matches!(corpus.record("b", &values, &other, 2), Recorded::Retained { .. }));
    }

    #[test]
    fn a_cap_refuses_rather_than_dropping_a_sole_edge_holder() {
        let mut corpus = Corpus::new(2);
        for (i, edge) in [1u32, 2, 3].iter().enumerate() {
            let values = vec![ValueTree::number(i as f64)];
            let edges: CoverageDigest = [*edge].into_iter().collect();
            corpus.record("t", &values, &edges, i as u64);
        }
        assert_eq!(corpus.len(), 2, "each entry holds an edge no other holds");
        assert_eq!(corpus.refused(), 1);
        assert!(corpus.uncovered_edges().is_empty());
    }

    #[test]
    fn a_dominated_entry_is_culled_and_its_superset_survives() {
        let mut corpus = Corpus::new(2);
        let wide: CoverageDigest = [1, 2].into_iter().collect();
        let narrow: CoverageDigest = [1].into_iter().collect();
        corpus.record("t", &[ValueTree::number(1.0)], &narrow, 0);
        corpus.record("t", &[ValueTree::number(2.0)], &wide, 1);
        // The third input forces a cull. The narrow entry is dominated by the wide one and was
        // used earlier, so it goes and no edge is lost.
        let third: CoverageDigest = [3].into_iter().collect();
        assert!(matches!(corpus.record("t", &[ValueTree::number(3.0)], &third, 2), Recorded::Retained { .. }));
        assert_eq!(corpus.len(), 2);
        assert!(corpus.uncovered_edges().is_empty(), "an edge lost its only holder");
        assert!(corpus.covered.contains(&1) && corpus.covered.contains(&2) && corpus.covered.contains(&3));
    }

    #[test]
    fn an_outcome_digest_separates_the_tuple_the_design_names() {
        assert_eq!(outcome_digest(&outcome_ok(12.0)), outcome_digest(&outcome_ok(13.0)), "12 ms and 13 ms are one bucket");
        assert_ne!(outcome_digest(&outcome_ok(1.0)), outcome_digest(&outcome_ok(5_000.0)));
        let rejected = Outcome::Rejected(afz_protocol::RejectedOutcome {
            duration_ms: 5.0,
            coverage: None,
            via: "throws:validation".into(),
            detail: "bad".into(),
        });
        assert_ne!(outcome_digest(&outcome_ok(5.0)), outcome_digest(&rejected));
        let failed = Outcome::Failed(afz_protocol::FailedOutcome {
            duration_ms: 5.0,
            coverage: None,
            via: "throws:unguarded".into(),
            fault: FaultSignature { class: "TypeError".into(), message: "m".into(), frames: vec![] },
        });
        assert_ne!(outcome_digest(&rejected), outcome_digest(&failed));
        let with_value = Outcome::Ok(afz_protocol::OkOutcome {
            duration_ms: 5.0,
            memory_bytes: None,
            coverage: None,
            value: Some(ValueTree::new(ValueNode::Boolean { value: true })),
        });
        assert_ne!(outcome_digest(&outcome_ok(5.0)), outcome_digest(&with_value), "a different return shape is a different path");
    }

    #[test]
    fn an_entry_id_is_stable_and_content_sensitive() {
        let a = entry_id("t", &[ValueTree::number(1.0)]);
        assert_eq!(a, entry_id("t", &[ValueTree::number(1.0)]));
        assert_ne!(a, entry_id("t", &[ValueTree::number(2.0)]));
        assert_ne!(a, entry_id("u", &[ValueTree::number(1.0)]));
        assert!(a.starts_with("afz:c:") && a.len() == 12, "id was {a}");
    }
}

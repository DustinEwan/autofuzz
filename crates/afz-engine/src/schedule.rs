//! Scheduling: which corpus entry gets mutated next, and when to generate instead.
//!
//! Two rules from the design. Weight by new-edge yield and by time since last selection, so an
//! entry that keeps finding new edges is drawn often and an unexplored entry is still revisited.
//! And spend a slice of invocations on fresh generation, so a scan whose corpus has stalled
//! cannot stall the loop.

use afz_protocol::ValueTree;

use crate::corpus::Corpus;
use crate::rng::Rng;

/// What the next invocation works on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Generate a new value from the target's declared types.
    Fresh,
    /// Mutate this corpus entry.
    Mutate { entry_id: String },
}

/// Fresh generation gets this share of invocations, in percent. The design's number is 10.
const FRESH_PERCENT: u64 = 10;
/// How many invocations an entry may go unselected before the visit guarantee promotes it, as a
/// multiple of the corpus size.
const VISIT_WINDOW: usize = 4;
/// Yield weight: each edge an entry contributed buys this many extra shares.
const YIELD_WEIGHT: u64 = 4;
/// Waiting weight: this many invocations unselected add one share, so an entry that has been
/// left alone climbs even when its yield is low.
const WAIT_STEP: u64 = 2;

/// The scheduler holds no state of its own: the corpus records `executions` and `last_used`, so
/// a scheduler can be rebuilt from the corpus after a resume.
#[derive(Debug, Clone, Copy)]
pub struct Scheduler {
    /// `Strategy::Random` disables corpus guidance: every invocation generates.
    guided: bool,
}

impl Scheduler {
    pub fn guided() -> Scheduler {
        Scheduler { guided: true }
    }

    pub fn random() -> Scheduler {
        Scheduler { guided: false }
    }

    pub fn is_guided(&self) -> bool {
        self.guided
    }

    /// Choose the next input source. `corpus.note_use` must follow for a `Mutate` selection, or
    /// the visit guarantee will keep promoting the same entry.
    pub fn pick(&mut self, corpus: &Corpus, rng: &mut Rng, invocation: u64) -> Selection {
        if !self.guided {
            return Selection::Fresh;
        }
        if corpus.is_empty() {
            // Nothing to mutate yet; the first inputs have to come from generation.
            return Selection::Fresh;
        }
        if rng.chance(FRESH_PERCENT, 100) {
            return Selection::Fresh;
        }
        if let Some(id) = self.overdue(corpus, invocation) {
            return Selection::Mutate { entry_id: id };
        }
        let entries: Vec<&str> = corpus.entries().map(|e| e.id.as_str()).collect();
        let weights: Vec<u64> = corpus.entries().map(|entry| weight(entry, invocation)).collect();
        let Some(index) = weighted(rng, &weights) else { return Selection::Fresh };
        Selection::Mutate { entry_id: entries[index].to_string() }
    }

    /// The entry that has waited longest past the visit window, if any. Ties break on id, so a
    /// resumed scan promotes entries in the same order.
    fn overdue(&self, corpus: &Corpus, invocation: u64) -> Option<String> {
        let window = (corpus.len().max(1) * VISIT_WINDOW) as u64;
        let mut overdue: Vec<(u64, &str)> = corpus
            .entries()
            .filter(|entry| invocation.saturating_sub(entry.last_used) >= window)
            .map(|entry| (entry.last_used, entry.id.as_str()))
            .collect();
        overdue.sort();
        overdue.first().map(|(_, id)| id.to_string())
    }
}

/// Weight in shares: one for being in the corpus, four per edge the entry contributed, and one
/// per `WAIT_STEP` invocations it has waited.
///
/// `executions` is deliberately not a penalty. Both counters are cumulative, so a term that
/// subtracts executions eventually erases the yield difference between a productive entry and an
/// unproductive one, and every entry converges on the same weight. Waiting is the honest
/// fatigue signal: an entry that keeps being drawn earns no waiting shares, while its idle
/// rivals climb until they are selected.
fn weight(entry: &crate::corpus::Entry, invocation: u64) -> u64 {
    1 + u64::from(entry.new_edges) * YIELD_WEIGHT + invocation.saturating_sub(entry.last_used) / WAIT_STEP
}

/// Draw an index with the given weights. Deterministic for a seed, and `None` when every weight
/// is zero or the list is empty.
fn weighted(rng: &mut Rng, weights: &[u64]) -> Option<usize> {
    let total: u64 = weights.iter().sum();
    if total == 0 {
        return None;
    }
    let mut draw = rng.below(total);
    for (index, weight) in weights.iter().enumerate() {
        if draw < *weight {
            return Some(index);
        }
        draw -= *weight;
    }
    Some(weights.len() - 1)
}

/// The input the next invocation runs: a fresh value, or a mutation of a corpus entry.
pub enum Plan {
    Fresh,
    Mutate { entry_id: String, values: Vec<ValueTree> },
}

impl Selection {
    /// Resolve a selection against the corpus. A `Mutate` whose entry has since been culled
    /// degrades to fresh generation rather than failing the invocation.
    pub fn resolve(self, corpus: &Corpus) -> Plan {
        match self {
            Selection::Fresh => Plan::Fresh,
            Selection::Mutate { entry_id } => match corpus.get(&entry_id) {
                Some(entry) => Plan::Mutate { entry_id, values: entry.values.clone() },
                None => Plan::Fresh,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::{CoverageDigest, Corpus};

    fn corpus_with(edges: &[&[u32]]) -> Corpus {
        let mut corpus = Corpus::new(100);
        for (index, set) in edges.iter().enumerate() {
            let digest: CoverageDigest = set.iter().copied().collect();
            corpus.record("t", &[ValueTree::number(index as f64)], &digest, index as u64);
        }
        corpus
    }

    #[test]
    fn an_empty_corpus_always_generates() {
        let mut scheduler = Scheduler::guided();
        let corpus = Corpus::new(10);
        let mut rng = Rng::seeded(1);
        for i in 0..50 {
            assert_eq!(scheduler.pick(&corpus, &mut rng, i), Selection::Fresh);
        }
    }

    #[test]
    fn ten_percent_of_invocations_generate() {
        let mut scheduler = Scheduler::guided();
        let corpus = corpus_with(&[&[1], &[2], &[3]]);
        let mut rng = Rng::seeded(0x5e3a91c4);
        let fresh = (0..10_000)
            .filter(|i| scheduler.pick(&corpus, &mut rng, *i) == Selection::Fresh)
            .count();
        // The visit guarantee can only convert a mutation into another mutation, so the fresh
        // share sits at its configured rate.
        assert!((800..1_300).contains(&fresh), "fresh share was {fresh} of 10000");
    }

    #[test]
    fn every_entry_is_visited_inside_the_window() {
        let mut scheduler = Scheduler::guided();
        let mut corpus = corpus_with(&[&[1], &[2], &[3], &[4], &[5]]);
        let mut rng = Rng::seeded(7);
        let window = (corpus.len() * VISIT_WINDOW) as u64;
        let mut visited: Vec<u64> = vec![0; corpus.len()];
        let ids: Vec<String> = corpus.entries().map(|e| e.id.clone()).collect();
        for invocation in 0..(window * 3) {
            let Selection::Mutate { entry_id } = scheduler.pick(&corpus, &mut rng, invocation) else { continue };
            let at = ids.iter().position(|id| *id == entry_id).unwrap();
            visited[at] += 1;
            corpus.note_use(&entry_id, invocation);
        }
        assert!(visited.iter().all(|v| *v > 0), "an entry was never visited: {visited:?}");
    }

    #[test]
    fn a_higher_yield_entry_is_drawn_more_often() {
        let mut scheduler = Scheduler::guided();
        let mut corpus = corpus_with(&[&[1], &[2, 3, 4, 5, 6]]);
        let ids: Vec<String> = corpus.entries().map(|e| e.id.clone()).collect();
        let rich = ids[1].clone();
        let mut rng = Rng::seeded(3);
        let (mut rich_draws, mut mutated) = (0, 0);
        for invocation in 0..4_000 {
            if let Selection::Mutate { entry_id } = scheduler.pick(&corpus, &mut rng, invocation) {
                // Selections are recorded, so recency stays comparable and the difference below
                // is yield.
                corpus.note_use(&entry_id, invocation);
                mutated += 1;
                if entry_id == rich {
                    rich_draws += 1;
                }
            }
        }
        // Weight is 1 + 5*4 against 1 + 1*4, so the rich entry should draw most of the
        // mutations; the visit guarantee only caps how far ahead of its rival it can get.
        assert!(rich_draws * 10 > mutated * 6, "the higher-yield entry drew {rich_draws} of {mutated} mutations");
    }

    #[test]
    fn an_unvisited_entry_outranks_a_high_yield_one() {
        let mut scheduler = Scheduler::guided();
        let mut corpus = corpus_with(&[&[1, 2, 3, 4, 5], &[9]]);
        let rich = corpus.entries().next().unwrap().id.clone();
        corpus.note_use(&rich, 10_000);
        let poor = corpus.entries().find(|e| e.id != rich).unwrap().id.clone();
        let mut rng = Rng::seeded(4);
        // Far past the window, the starved entry is promoted even against a 5-edge rival.
        let picked = scheduler.pick(&corpus, &mut rng, 10_000 + (corpus.len() * VISIT_WINDOW) as u64);
        assert_eq!(picked, Selection::Mutate { entry_id: poor });
    }

    #[test]
    fn a_random_strategy_never_mutates() {
        let mut scheduler = Scheduler::random();
        let corpus = corpus_with(&[&[1], &[2]]);
        let mut rng = Rng::seeded(9);
        for i in 0..200 {
            assert_eq!(scheduler.pick(&corpus, &mut rng, i), Selection::Fresh);
        }
    }

    #[test]
    fn a_culled_entry_degrades_to_fresh_generation() {
        let mut corpus = Corpus::new(4);
        let digest: CoverageDigest = [1].into_iter().collect();
        corpus.record("t", &[ValueTree::number(1.0)], &digest, 0);
        let stale = Selection::Mutate { entry_id: "afz:c:gone".into() };
        assert!(matches!(stale.resolve(&corpus), Plan::Fresh));
        let live = Selection::Mutate { entry_id: corpus.entries().next().unwrap().id.clone() };
        assert!(matches!(live.resolve(&corpus), Plan::Mutate { values, .. } if values.len() == 1));
    }

    #[test]
    fn the_weighted_draw_follows_the_weights() {
        let mut rng = Rng::seeded(15);
        let counts = [0u64; 3];
        let mut counts = counts;
        for _ in 0..9_000 {
            counts[weighted(&mut rng, &[1, 2, 3]).unwrap()] += 1;
        }
        assert!(counts[0] < counts[1] && counts[1] < counts[2], "{counts:?} did not follow 1:2:3");
        assert_eq!(weighted(&mut rng, &[]), None);
        assert_eq!(weighted(&mut rng, &[0, 0]), None);
    }
}

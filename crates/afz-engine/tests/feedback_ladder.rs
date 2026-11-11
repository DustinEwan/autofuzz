//! The feedback ladder: generation seeds a corpus, mutation grows coverage, and the corpus
//! keeps every edge it has ever claimed.
//!
//! The synthetic target is an array of one literal whose edge set is `{length + 1}`. Fresh
//! generation can only draw the lengths in the boundary length table `{0, 1, 2, 7, 8}`, so edges
//! `4`, `5`, `6`, `7` are reachable only by the resize operator, which changes a length by one.
//! That is what makes this a test of the loop rather than of the generator: a scan that never
//! mutates a retained input cannot reach full coverage.

use std::collections::BTreeSet;

use afz_engine::corpus::{Corpus, CoverageDigest, Recorded};
use afz_engine::generate::{self, DEFAULT_DEPTH};
use afz_engine::mutate;
use afz_engine::rng::Rng;
use afz_engine::schedule::{Plan, Scheduler, Selection};
use afz_protocol::{CollectionKind, LengthBounds, TypeNode, ValueNode, ValueTree};

/// The longest array the target declares, and therefore the number of distinct edges.
const MAX_LEN: u64 = 8;
/// Every edge the target can report: lengths 0..=MAX_LEN map to edges 1..=MAX_LEN+1.
const EDGES: u32 = MAX_LEN as u32 + 1;
/// Edges fresh generation cannot produce.
const MUTATION_ONLY: [u32; 4] = [4, 5, 6, 7];

fn target_type() -> TypeNode {
    TypeNode::Collection {
        collection: CollectionKind::Array,
        element: Box::new(TypeNode::Literal { values: vec![serde_json::Value::String("a".into())] }),
        key: None,
        length: Some(LengthBounds { min: Some(0), max: Some(MAX_LEN) }),
    }
}

/// The synthetic target: its edge set is one edge, chosen by array length.
fn invoke(values: &[ValueTree]) -> CoverageDigest {
    let length = match values.first().map(|v| &v.node) {
        Some(ValueNode::Array { items }) => items.len(),
        _ => 0,
    };
    let mut digest = CoverageDigest::new();
    digest.insert(length as u32 + 1);
    digest
}

struct Run {
    corpus: Corpus,
    /// Covered-edge count after each invocation, for the monotonicity assertion.
    growth: Vec<u64>,
    mutated: u64,
    generated: u64,
}

/// One guided run: pick, produce, invoke, record.
fn run(seed: u32, invocations: u64, cap: u64) -> Run {
    let ty = target_type();
    let mut scheduler = Scheduler::guided();
    let mut corpus = Corpus::new(cap);
    let mut rng = Rng::seeded(seed);
    let mut growth = Vec::new();
    let (mut mutated, mut generated) = (0, 0);

    for invocation in 0..invocations {
        let selection = scheduler.pick(&corpus, &mut rng, invocation);
        let (values, is_mutation) = match selection.resolve(&corpus) {
            Plan::Fresh => (vec![generate::generate(&ty, DEFAULT_DEPTH, &mut rng)], false),
            Plan::Mutate { entry_id, values } => {
                corpus.note_use(&entry_id, invocation);
                (vec![mutate::mutate(&values[0], &ty, &mut rng)], true)
            }
        };
        let edges = invoke(&values);
        corpus.record("afz:t:ladder", &values, &edges, invocation);

        // The three invariants the design puts on the corpus, checked after every step rather
        // than at the end, so a violation names the invocation that caused it.
        assert!(corpus.len() as u64 <= cap, "the corpus outgrew its cap at invocation {invocation}");
        assert!(
            growth.last().is_none_or(|previous| corpus.covered_count() >= *previous),
            "coverage shrank at invocation {invocation}"
        );
        assert!(
            corpus.uncovered_edges().is_empty(),
            "a covered edge lost its only holder at invocation {invocation}: {:?}",
            corpus.uncovered_edges()
        );

        growth.push(corpus.covered_count());
        if is_mutation {
            mutated += 1;
        } else {
            generated += 1;
        }
    }

    Run { corpus, growth, mutated, generated }
}

#[test]
fn an_empty_corpus_becomes_non_empty_from_generated_input() {
    let ty = target_type();
    let mut corpus = Corpus::new(64);
    let mut rng = Rng::seeded(0x5e3a_91c4);
    assert!(corpus.is_empty());
    let mut retained = 0;
    for invocation in 0..8 {
        let values = vec![generate::generate(&ty, DEFAULT_DEPTH, &mut rng)];
        let edges = invoke(&values);
        assert!(!edges.is_empty(), "the target reported no edges");
        if matches!(corpus.record("afz:t:ladder", &values, &edges, invocation), Recorded::Retained { .. }) {
            retained += 1;
        }
    }
    assert_eq!(retained, corpus.len());
    assert!(!corpus.is_empty(), "generation alone left the corpus empty");
}

#[test]
fn mutating_a_retained_input_reaches_an_edge_generation_cannot() {
    let ty = target_type();
    let mut corpus = Corpus::new(64);
    let mut rng = Rng::seeded(0x1234_5678);
    // Seed the corpus with what generation produces, then mutate until the edge set grows.
    let mut seeds = Vec::new();
    for invocation in 0..40 {
        let values = vec![generate::generate(&ty, DEFAULT_DEPTH, &mut rng)];
        let edges = invoke(&values);
        if matches!(corpus.record("afz:t:seed", &values, &edges, invocation), Recorded::Retained { .. }) {
            seeds.push(values);
        }
    }
    let before = corpus.covered_count();
    assert!(
        !corpus.covered().iter().any(|e| MUTATION_ONLY.contains(e)),
        "generation alone already reached a mutation-only edge: {:?}",
        corpus.covered()
    );

    let mut grew = false;
    'outer: for (round, seed_values) in seeds.iter().enumerate() {
        let mut current = seed_values.clone();
        for step in 0..64 {
            let mutated = vec![mutate::mutate(&current[0], &ty, &mut rng)];
            let edges = invoke(&mutated);
            if let Recorded::Retained { .. } = corpus.record("afz:t:mut", &mutated, &edges, (round * 64 + step) as u64) {
                assert!(edges.len() as u64 >= 1);
                grew = true;
                break 'outer;
            }
            current = mutated;
        }
    }
    assert!(grew, "no chain of mutations produced a new digest");
    assert!(corpus.covered_count() > before);
    assert!(corpus.covered().iter().any(|e| MUTATION_ONLY.contains(e)), "the new edge was not a mutation-only edge");
}

#[test]
fn a_digest_already_in_the_corpus_yields_a_new_digest_after_mutation() {
    let ty = target_type();
    let mut corpus = Corpus::new(64);
    let mut rng = Rng::seeded(0xabcd);
    let values = vec![generate::generate(&ty, DEFAULT_DEPTH, &mut rng)];
    let first = invoke(&values);
    assert!(matches!(corpus.record("afz:t:same", &values, &first, 0), Recorded::Retained { .. }));
    // The identical input again is not novelty.
    assert_eq!(corpus.record("afz:t:same", &values, &first, 1), Recorded::NotNovel);
    assert_eq!(corpus.len(), 1);

    // Walking mutations from that same input must eventually report a digest the corpus lacks.
    let mut current = values.clone();
    let mut saw_new = false;
    for step in 2..200 {
        let mutated = vec![mutate::mutate(&current[0], &ty, &mut rng)];
        let edges = invoke(&mutated);
        if corpus.covered().is_superset(&edges) {
            current = mutated;
            continue;
        }
        assert!(matches!(corpus.record("afz:t:same", &mutated, &edges, step), Recorded::Retained { .. }));
        saw_new = true;
        break;
    }
    assert!(saw_new, "mutations of a retained input never left its digest");
}

#[test]
fn a_guided_run_reaches_every_edge_and_holds_the_invariants() {
    let run = run(0x5e3a_91c4, 4_000, 64);
    let expected: BTreeSet<u32> = (1..=EDGES).collect();
    assert_eq!(run.corpus.covered(), &expected, "coverage stopped short of the full edge set");
    assert!(run.mutated > run.generated / 2, "the loop generated {} and mutated {}", run.generated, run.mutated);
    assert!(run.growth.windows(2).all(|w| w[0] <= w[1]), "coverage was not monotonic");
    assert!(run.corpus.len() <= 64);
    assert!(run.corpus.uncovered_edges().is_empty());
}

#[test]
fn a_tight_cap_holds_and_never_loses_an_edge() {
    // With a cap of 3 and 9 edges to hold, the corpus must refuse rather than evict a sole-edge
    // holder, and every edge it claimed must still have a holder.
    let run = run(0x0bad_c0de, 2_000, 3);
    assert!(run.corpus.len() <= 3, "the corpus held {} entries against a cap of 3", run.corpus.len());
    assert!(run.corpus.uncovered_edges().is_empty());
    for entry in run.corpus.entries() {
        assert!(!run.corpus.sole_edges(entry).is_empty(), "a retained entry holds no edge of its own");
    }
    assert!(run.corpus.refused() > 0, "a cap of 3 never refused an input");
}

#[test]
fn the_loop_replays_from_its_seed() {
    let a = run(0x5e3a_91c4, 1_000, 64);
    let b = run(0x5e3a_91c4, 1_000, 64);
    assert_eq!(a.growth, b.growth, "the same seed produced a different coverage curve");
    assert_eq!(a.corpus.len(), b.corpus.len());
    let a_ids: Vec<&str> = a.corpus.entries().map(|e| e.id.as_str()).collect();
    let b_ids: Vec<&str> = b.corpus.entries().map(|e| e.id.as_str()).collect();
    assert_eq!(a_ids, b_ids, "the same seed retained a different corpus");
}

#[test]
fn a_random_strategy_cannot_reach_the_mutation_only_edges() {
    // The control for the ladder claim: with guidance off, the same number of invocations
    // generates the same lengths it always could and never walks into the middle of the range.
    let ty = target_type();
    let mut corpus = Corpus::new(64);
    let mut scheduler = Scheduler::random();
    let mut rng = Rng::seeded(0x5e3a_91c4);
    for invocation in 0..4_000 {
        let Selection::Fresh = scheduler.pick(&corpus, &mut rng, invocation) else { panic!("unguided run mutated") };
        let values = vec![generate::generate(&ty, DEFAULT_DEPTH, &mut rng)];
        let edges = invoke(&values);
        corpus.record("afz:t:ladder", &values, &edges, invocation);
    }
    assert!(
        corpus.covered().iter().all(|e| !MUTATION_ONLY.contains(e)),
        "an unguided run reached a mutation-only edge: {:?}",
        corpus.covered()
    );
    assert!(corpus.covered().len() < EDGES as usize);
}

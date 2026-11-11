# Decision 2 - a run of operators per execution, and a copy within the input

Status: accepted. 2026-11-11.
Amends: `docs/architecture.md` "Feedback and scheduling", which says mutation operates on
`ValueTree` and leaves the operator set and its per-execution arity unstated.

## Context

Step 10's acceptance test target hides a bug behind a gate with five conditions, each of which opens a
new block, and a scan that found it had to cross a sixth change that opens nothing: the crashing
read of a nullable array slot. Three properties of the mutator as designed made that unreachable
within the budget, measured by running the engine's real scheduler, corpus, generator, and
mutation operators against a model of the target's block structure:

- One operator per execution. A run that needs two changes to reach new coverage is discarded,
  because the intermediate earned no edge and so was never retained to be mutated again. A gate
  two changes deep is then unreachable however long the scan runs.
- No operator copies a value that already exists in the input. `sibling` generates afresh and
  `transplant` brings a subtree from a different corpus entry, so two fields the target compares
  for equality - a reissue whose memo repeats its own identifier - are never equal: each string is
  drawn independently, and no operator can make one look like the other.
- Integer boundaries were `0, 1, -1, 2, 255, 256, 65535, 65536` and the extremes. A `u32` or `i32`
  parameter - which is every parameter a wasm export has - is usually a length, an index, or a
  small tag, and a draw uniform over the declared range never produces a value in between.

## Decision

1. **A mutated input takes a run of `1..=8` operator applications**, drawn from the scan's own
   `rng`, re-selecting the node each time. Node-level granularity is unchanged, so the minimizer
   and the finding's provenance still work on single nodes; what changes is that one execution can
   land two rungs away, and a collection can cross a length range in which no intermediate length
   opens a block.
2. **A tenth operator, `paste`, copies a node over another node of the same input.** Candidates are
   filtered by the same declaration check `transplant` uses, and a node that contains the
   destination is excluded, because copying an ancestor over its own descendant doubles the input
   on every application. This is AFL++'s copy-paste operator, applied to a typed tree.
3. **The integer boundary table carries every integer below 128** alongside the existing entries.
   The band is contiguous rather than a list of notable values, so it is a rule rather than a set of
   constants picked to suit one target.

## Consequences

- The committed generation golden changes byte for byte, because a boundary table edit shifts the
  draw sequence in exactly one place. It was regenerated and the rerun is still identical.
- A run costs more per invocation than one operator did, and inputs land further from their base.
  The corpus's retention rule is what keeps that harmless: an input that opens no new block is
  dropped whatever the run length was.
- Determinism is untouched. Every draw comes from the scan's single seed, so `--seed` still replays
  the same invocation sequence.
- Measured on the Step 10 target model: guided reached the hidden bug in 20 of 20 runs of 50000
  invocations, first arrival between 198 and 13778 invocations; before the change, 9 of 20. The
  random strategy - which never mutates, so it cannot paste - reached it in 0 of 20.

## Reconsidered

- **More corpus entries, or a bigger budget.** Both cost invocations to work around a mutator that
  cannot express the change. Rejected.
- **A resize that jumps to a random length.** Fixes the one plateau shape this target showed and
  weakens the one-step walk that makes a coverage ladder climbable. The run length already crosses
  that plateau. Rejected.
- **Per-operator weights** (run `sibling` more often than `paste`). Unmeasured, and a weighting is a
  second random stream to keep deterministic. Deferred until a scan shows an operator that never
  pays for itself.

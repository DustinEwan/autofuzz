# Decision 5 - coverage ids and outcome-tuple ids are separate id spaces

Status: accepted. 2026-11-11.

## Context

`execution.coverageRatio` is `coverageEdges` divided by the size of the plugin's coverage map, and
two different things were being counted into `coverageEdges`.

A harness that declares the `coverage` capability reports edge ids the map can name. A harness that
declares nothing has no edges to report, so the corpus records a digest of the answer's shape
instead - status class, latency bucket, response shape, from `corpus::outcome_digest` - which is what
gives a coverage-free scan any novelty signal at all.

One wasm run reported `coverageRatio` above 1.0: 11 edges against a map of 9. A plugin process that
dies mid-call answers nothing, and the engine synthesizes an outcome for it. That outcome is real,
but the harness was coverage-capable and had simply never reached the module, so the corpus took the
coverage-free fallback and filed ids that the coverage map cannot name into the numerator of a
fraction whose denominator is that map.

## Decision

The outcome-tuple digest stands in for edges only where the handshake declares no `coverage`
capability. A coverage-capable plugin that answers with no digest contributes an empty digest: there
is nothing to learn from an invocation that never reached the target.

## Consequences

- `coverageRatio` cannot exceed 1.0 for a coverage-capable harness, because every id in the numerator
  comes from the map that is the denominator.
- An invocation lost to a dead worker records no novelty, so it cannot pull the corpus toward a repeat
  of itself. It still counts in `invocations`, and it still produces its finding through the
  `processExit` oracle.
- The rule reads the handshake, not the outcome, so a harness keeps one identity for its feedback
  across the whole run rather than switching identity when a process dies.

## Reconsidered

- **Capping `coverageRatio` at 1.0.** Rejected: it hides the mixing that produced the number, and a
  ratio above 1.0 is the only symptom a reader would otherwise never see.
- **Naming outcome tuples in the coverage map.** Rejected: the map is the set of blocks the harness
  can point at, and an id that stands for an answer is not a block. A reader told there are 9 blocks
  should not be shown 11 of them.
- **Dropping the outcome-tuple fallback entirely.** Rejected: it is the only novelty signal a
  coverage-free harness has, and the design keeps that harness class.

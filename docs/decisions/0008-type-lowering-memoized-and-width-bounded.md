# Decision 8 - Type lowering is memoized by type and depth, and its width is bounded

## Context

Discovery over this repository's root never answered. The plugin process died with
`FATAL ERROR: Ineffective mark-compacts near heap limit Allocation failed - JavaScript heap out of
memory` after 119 s on a 4 GB ceiling, which is why it exceeded `DISCOVERY_TIMEOUT`
(`crates/afz-engine/src/supervise.rs:34`, 120 s): the symptom was a timeout, the cause was a crash.

Building the compiler program was not the cost. 48 sources and 118 files took 401 ms. Attributing
the work per exported parameter, every symbol that finished had lowered at most 7 composite types,
and the process died inside `packages/harness-js/src/lower.ts#promisedTypeOf(checker)`, whose
declared type is `ts.TypeChecker`.

`MAX_TYPE_DEPTH = 12` bounds the height of a type graph and nothing else. `lowerObject` resolved
every property of every type it reached, and the recursion guard in `Ctx.active` only short-circuits
a type that is an *ancestor* of the current path. A declaration file whose interfaces reference each
other is a DAG rather than a cycle, and a DAG walked once per path is exponential in depth. The
TypeScript compiler's own declaration file is the largest such graph in the repository, and it sits
in a parameter position because the harness's public API takes `ts.TypeChecker` and `ts.Type`: the
scan that failed was the harness discovering itself.

## Decision

1. **Composite shapes are memoized by (`ts.Type`, depth).** A union, intersection, or object type
   already lowered at the same depth is reused rather than re-walked. Blockers and the
   materialization floor are *replayed* on a hit, with blocker paths re-based on the path the shape
   is being replayed at, because a memo that dropped blockers would turn a reported
   `fixture-required` parameter into a constructible one, and that record is the whole mechanism by
   which an unreachable entry point is reported. A shape is stored only when its subtree emitted no
   `ref`: a `ref` names a type that has to sit above the position for the shape it points at to be
   registered there, so a subtree that emitted one is still path-dependent.
2. **Width is bounded per `lower()` call.** `MAX_EXPANSIONS = 200_000`, charged one per composite
   type lowered and one per property resolved. Past the budget a position lowers to `opaque` with
   the type name `expansion-budget-exceeded:<name>`, so the inventory reports a shape that was too
   large instead of claiming a shape it never built.

Both live in `packages/harness-js/src/lower.ts`. Traversal order is file order and property order,
so the memo is deterministic and a seed-free discovery run stays reproducible.

## Alternatives rejected

- **Set `lib` explicitly and refuse `node_modules` reads in the compiler host.** The default lib set
  pulls `lib.dom.d.ts` (2.35 MB) into every program, and blocking `node_modules` reads measured 387
  ms against 34 ms for the program build. Against a 1.78 s discovery that is a rounding error, and
  refusing `node_modules` reads would resolve every dependency type to `any`, which loses the
  `buffer` mapping and the `opaque` accounting for exactly the types a caller needs named. Rejected.
- **Raise `DISCOVERY_TIMEOUT`.** The work was unbounded, not the budget. A longer timeout turns a
  crash at 119 s into a crash later. Rejected.
- **Skip lowering types declared in a `.d.ts`.** A dependency's options interface is precisely what
  a caller wants fuzzed, and it is declared in a `.d.ts`. Rejected.
- **Lower only the first N properties of a wide type.** Produces a shape that looks complete and is
  not. The budget reports instead of guessing. Rejected.

## Measured outcome

Over this repository's root with `include: ["**/*.ts"]`:

| | before | after |
| --- | --- | --- |
| outcome | heap OOM, no answer | completes |
| wall clock | 119 s to crash | 1.78 s |
| heap | 4 GB ceiling reached | 165 MiB peak |
| `promisedTypeOf(checker)`, typed `ts.TypeChecker` | crash | 28,844 expansions, 612 ms, 1,069 blockers, floor `opaque` |

Total composite expansions across every exported parameter of all 48 sources: 61,938. The largest
single parameter used 29,233 of the 200,000 budget, so no shape was given up on this input; the
budget is a backstop, not the mechanism that fixed it.

## Consequences

- A codebase whose public API exposes a very large declaration-file type is now scanned and reported
  as `fixture-required` or `opaque` with a named reason, instead of ending the scan.
- `expansion-budget-exceeded:` is a new prefix inside `skipped[].blockedBy.type`. Renderers and
  triage tooling read that field as text, so no schema change follows.
- The repository can now be scanned by its own harness, which makes it the largest available
  discovery test rather than an input the tool cannot survive.

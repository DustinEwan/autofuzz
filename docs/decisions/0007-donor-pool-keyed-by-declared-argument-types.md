# Decision 7 - A mutation donor may come from any entry point with the same declared argument types

Status: accepted, 2026-11-11.

## Context

The billing test target hides a crash behind a conjunction: `currency == "BRL"`, `kind == "credit"`,
`batchTags.length === 3`, a non-empty first tag, `locale` present, and `locale.region` equal to that
first tag apart from case. Two runs of 250 000 invocations at the same seed found nothing, guided
reaching 206 of 328 blocks and random 198 of 317.

The mutator was not the limit. Measured over 200 000 mutations each, against the `TypeNode` the
plugin actually reports for `creditSummary`:

| starting point | reached all five structural conditions | also reached the equality |
|---|---|---|
| an input that already satisfies the gates | 96 862 | **51 271** |
| fresh generation plus one mutation | 16 | 2 |
| fresh generation alone | — | 0 |

So from a usable parent the bug is easy to reach: the copy-within-input operator lands on
`locale.region` 12 800 times and the equality follows about a quarter of the time. The engine simply
never obtained that parent, and the corpus said why. `creditSummary` is the only entry point in the
target that calls `isReissue`, and it retained exactly one entry, an input that failed the admission
test. Its siblings `creditManifest` and `exportCreditLedger` take the same `ReportRequest` and were
the entries that got past `isCreditWindow` into `validation.ts:917`. The engine had solved the hard
part on one entry point and could not hand it to the neighbour, because donor selection filtered
corpus entries by `target_id`.

## Decision

1. **A donor may come from any retained entry whose declared parameter types equal the target's.**
   Entries for the target itself are still preferred when any exist, so a target with its own history
   keeps behaving as before, and the wider pool only fires where the old rule left the mutator
   without a donor at all.
2. **The signature is the ordered list of parameter types.** Parameter names are excluded: they say
   nothing about whether a value is legal at a position. The index is built once when the scan opens.
3. **The per-node declaration check stays where it is.** `mutate::transplant` already rejects any
   donor subtree the destination type does not allow, so widening the pool cannot graft a value the
   target would read as a type error.

## Consequences

- Knowledge one entry point earns is a head start for every entry point with the same signature,
  which is the common shape of real code: a request type read by several exported functions.
- A seed's trajectory changes across this edit, because the number of random draws changes when the
  pool changes. Determinism still holds per seed; a golden comparison that spans this commit is not
  meaningful.
- Cost is one hash lookup and one pass over the retained entries per mutation, bounded by
  `maxCorpus`, plus a signature index built once per scan.
- Two entry points with equal parameter types but different semantics now share material. That is
  sound because legality is a property of the type, and it is bounded by the same check that
  constrains a donor inside one input.

## Alternatives rejected

- **A global donor pool with no type check.** Cheapest to write. Rejected for the reason the
  transplant filter already gives: grafting a `LineItem` subtree into a parameter declared `Invoice`
  produces a fault in how the target reads a malformed object, which is a finding about the harness
  rather than about the target.
- **Seeding the corpus with a hand-written admitted input.** It would satisfy the acceptance
  demonstration trivially and prove nothing about guidance.
- **Raising the budget.** From fresh generation plus one mutation the conjunction appears twice in
  200 000 tries, so the expected wait is around 10^5 invocations on the target that needs it, or a
  few million across the target's 37 entry points. That is a cost of minutes at 4.4 ms per
  invocation and it buys no capability.
- **Teaching the mutator about comparisons in the target's source.** The engine does not read target
  source, and by Decision 1 that knowledge belongs to a harness that wants to provide it.

## Measured outcome

Before this change, guided runs of 250,000 invocations on the billing test target at seed
`0x5e3a91c4` reported no finding, reaching 206 of 328 revealed edges. With it, a guided run at the
same seed reported the target's defect at invocation 3,916 of a 50,000 budget: `oracle: failed`,
`via: engine-error-outside-declared-channel`, `TypeError: Cannot read properties of undefined
(reading 'toUpperCase')` at `src/reports.ts:72` in `legacyCreditHeader`, reproduced from the
minimized input.

The minimized input shows the mechanism this decision is about. `batchTags[0]` and `locale.region`
carry the same string, `lGy'`!:5uKQcS|{`uTvC`, and each node carries
`provenance: { transplanted: { fromEntry: ... } }`: the value that satisfies the cross-field
equality arrived from another retained input, not from the generator.

`--strategy random` reports no finding at 50,000 invocations and none at 250,000, twice at the
latter with byte-identical execution records (250,000 invocations, 198 covered edges, corpus 83,
`stoppedReason: budget`). That half of the result is not seed-luck: random never mutates, so it has
no copy-within-input operator, and it can satisfy `locale.region === batchTags[0].toUpperCase()`
only by drawing the same string twice, which it did zero times across half a million invocations.

## Caution recorded while measuring this

Whether a guided run finds this defect within a given budget is trajectory-dependent, and a guided
trajectory is sensitive to the corpus's entry ordering. Three builds carrying this decision,
differing only in what the transplant provenance string contains and in what the content address
hashes, reached 217, 206, and 197 covered edges at 50,000 invocations on the same seed; only the
first reported the finding. Corpus entry ids are the keys of the map the donor pool draws from, so
anything that renumbers them re-draws the search. An acceptance test that pins one seed and one
budget is therefore a weak instrument on a target whose path needs a six-condition conjunction,
and should be read across seeds rather than as a single pass.


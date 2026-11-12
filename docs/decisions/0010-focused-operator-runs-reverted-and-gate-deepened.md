# Decision 10 - Focused operator runs are reverted; the acceptance gate needs two copies in one subtree

Status: accepted, 2026-11-11.

## Context

Two things were measured against each other, and both had to be fixed before either measurement
meant anything.

The scheduling idea under test was this: a mutation run picks one node of the input as a focus and
confines its rewrites to that subtree, while paste sources stay global. The argument for it was
Decision 2's argument, one level up - a run that scatters its one to eight applications over every
node of a large input rarely puts two of them in the same neighbourhood, so a gate that needs two
nearby edits is unreachable for the same reason a gate two mutations deep used to be.

The measurement instrument was broken twice. The billing acceptance defect first sat behind an
equality between two free-text fields, and `--strategy random` reached the crash on its own: the
minimized inputs carried the same boundary-table value on both sides, `"1e309"` and
`"../../etc/passwd"`, each with `provenance: { generated: {} }`. The boundary table is shared by
every string field, so a free-text equality is not a measure of depth. The gate was then rebuilt
around `issuedBy: string` versus `raisedBy: Operator`, a three-member literal union, which
generation cannot make agree. That exposed a separate engine bug, recorded as Decision 9.

## Measurement 1 - focused runs against scattered runs, one-copy gate

Twenty-five scans of the billing test target at 50 000 invocations each, 35 minutes 20 seconds of
wall clock, same ten seeds in both arms, the arms separated by one `git stash` of `mutate.rs` and
its test file so the control differed from the treatment by the focus mechanism alone.

| arm | seeds that found the defect | median first arrival | mean first arrival |
|---|---|---|---|
| `--strategy random` (5 seeds) | 0 | - | - |
| focused runs | 10 of 10 | 4 255 | 5 101 |
| scattered whole-tree draw | 10 of 10 | 2 190 | 5 013 |

Pairing the ten seeds head to head, the scattered draw arrived first on six seeds and the focused
runs on four. Coverage finished in the same 206-to-231 edge band in both arms. Hit rate was
identical.

## Measurement 2 - the shape the mechanism was built for

A null result on a gate that needs one copy does not indict a mechanism meant for two copies in one
subtree, so the gate was deepened: `issuedBy` must equal `raisedBy` **and** the same coupon's
`value` must equal the request's `expectedMinor`. Both destinations sit inside one element of one
array. This was measured offline over 100 000 mutation runs per arm rather than by another 35
minutes of scans.

| starting point | focused runs that reached the crash | scattered runs that reached the crash |
|---|---|---|
| an input already holding the login copy | 2 870 of 100 000 | 2 957 of 100 000 |
| fresh generation | 56 of 100 000 | 86 of 100 000 |

The mechanism is behind in both rows. The likely reason is that the scarce event is not the
destination: it is drawing the paste operator at all and drawing a source whose value is legal at
that position. Concentrating destinations raises the chance of hitting the two interesting nodes and
lowers the chance of the rewrites elsewhere that make the input eligible in the first place, and
those two effects cancel. That explanation is inference; the two null results are measurement.

## Decision

1. **The focus mechanism is reverted.** `mutate_with_donor` plans a run over the whole input, as it
   did before the experiment, and `mutate_focused` and `pick_focus` are gone. Two independent null
   results, on the one-copy gate and on the two-copy gate, are not a basis for keeping 57 lines of
   search policy that change every seed's trajectory.
2. **The acceptance gate keeps two copies inside one subtree.** `reissueSelfRedeemed` honours a
   reissue only for a coupon that is `fixed`, whose `issuedBy` equals `raisedBy`, and whose `value`
   equals `expectedMinor`, and then writes the reissue onto the coupon that follows the match. The
   six-condition `creditSummary` path stays in the fixture as a red herring that must return without
   breaking, because a hard path that leads nowhere would make a finding there a wrong report.
3. **A depth device in a test target must have at least one literal-typed side.** An equality
   between two free-text fields is satisfied by the shared boundary table, so it measures nothing.
   This is written into the header of `packages/harness-js/test/billing.test.mjs` as well, because
   that file is where the next person will be tempted to deepen a gate.

## Consequences

- Seeds that ran through a build carrying the mechanism do not replay on the reverted code. The
  numbers in this record are the evidence for the revert and are not reproducible after it; the
  acceptance numbers below are.
- The engine loses no capability it had before the experiment. What it gains is that a run's node
  draw is uniform over the input again, which is what Decision 2's tests assume.
- The claim the plan actually requires is now carried by the corpus, the donor pool of Decision 7,
  and the paste operator of Decision 2 alone: guided runs find the defect on every seed tried, and
  random runs, which never mutate and therefore have no copy-within-input operator, found it on
  none.

## Measured outcome after the revert

Three guided runs of 50 000 invocations on the two-copy gate: all three reported the defect, at
invocations 3 644, 7 131 and 8 599, `oracle: failed`, `via: engine-error-outside-declared-channel`,
`TypeError: Cannot read properties of undefined (reading 'code')` at `src/reissue.ts` in
`reissueSelfRedeemed`. Three random runs at the same seeds and budget reported none, reaching 205 to
206 covered edges against the guided arms' 208 to 209.

## Cost measured while investigating, recorded because it contradicts a plausible guess

Input construction on the single planning thread costs 58.7 µs per input for generation and 247.2 µs
per input for generation plus a full mutation run, measured over 20 000 draws of the
`ReissueRequest` type. A 50 000-invocation scan takes 80.1 seconds, which is 1.60 ms of wall time per
invocation. The mutation run is therefore about 15% of wall, so the clone traffic inside
`mutate_once` and the per-candidate `wire_bytes` calls inside the paste operator are not what leaves
the machine half idle. The remaining candidates are the batch barrier at
`BATCH_PER_WORKER = 8` (`crates/afz-engine/src/scan.rs:48`), the `refresh_coverage()` round trip to
every plugin process after every batch (`crates/afz-engine/src/scan.rs:553`), and single-threaded
scoring in `learn()`. Those are throughput work, not search work, and they get their own record when
measured.

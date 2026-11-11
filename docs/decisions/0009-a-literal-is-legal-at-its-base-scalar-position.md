# Decision 9 - A literal is a legal value at its base scalar position

Status: accepted, 2026-11-11.

## Context

The billing test target hides a crash behind a gate in `reissueSelfRedeemed`: a reissue is honoured
only for a coupon whose `issuedBy` equals the request's `raisedBy`, and the reissue is then written
onto the coupon that follows the match, a slot nothing in the request guarantees.

The first version of that gate compared two free-text fields, and `--strategy random` reached the
crash on its own. The minimized inputs said why: both fields carried the same value
(`"1e309"`, `"../../etc/passwd"`, `"9007199254740993"`) and both carried
`provenance: { generated: {} }`. The boundary table in `boundaries.rs` is shared by every string
field in every input, so an equality between two free-text fields is satisfied by chance and cannot
serve as a measure of depth. Making one side literal-typed is what keeps the gate honest:
`raisedBy: Operator` is drawn from a three-member set and `issuedBy: string` from the generator's
ASCII alphabet, so generation cannot make them agree.

That change exposed a second, larger problem. Seven guided runs of 50 000 invocations, one per seed,
reported no finding, reaching 205 to 228 covered edges; five random runs reported none either. The
gate was not unlikely. It was unreachable.

`mutate::matches` is the legality predicate the mutator consults before it moves a value: paste
filters its candidate sources through it, and transplant filters its donor subtrees through it. It
had arms for `TypeNode::Literal` against `ValueNode::Literal` and for `TypeNode::Scalar` against the
matching `ValueNode` kinds, and no arm pairing a declared scalar with a `ValueNode::Literal`, so a
literal fell through to the catch-all `false`. The only operator that can make two independent
fields agree is the paste, and it had already dropped the raiser's login from its candidate list
before it chose a source.

## Decision

1. **A literal is accepted at a position declared `string`, `number`, or `boolean` when its JSON
   payload is of that kind.** A literal is its base value at runtime; the harness materializes it as
   the primitive, so the declaration permits it.
2. **The widening is one-directional.** A plain string stays refused at a position declared as a
   literal union, because there the declaration names the only legal values.
3. **The rule lives in the legality predicate, not in an operator.** Paste and transplant consult
   `matches`, so both gain the capability from one change and neither carries a special case that
   the other lacks.

## Consequences

- The gate shape "a free-text field compared against an enum-typed field" is now reachable. It is a
  common shape in real code, where one module stores an identity as a login and another declares it
  as a union of known logins.
- Transplant gains the same widening. That is sound for the same reason: the value is legal at that
  position, and the check that refuses an illegal graft is unchanged.
- A scan whose inputs contain a literal-typed field draws a different trajectory across this commit,
  because the paste's candidate list is longer and the number of draws changes. Determinism holds
  per seed; a golden comparison that spans this commit is not meaningful.

## Alternatives rejected

- **Keep the fixture comparing two free-text fields and raise the budget.** Rejected because the
  boundary table makes that equality reachable by generation, which is the one thing the plan's
  acceptance criterion will not tolerate: a target random can solve measures nothing.
- **Coerce inside the paste operator instead of widening the predicate.** Rejected because it leaves
  two sources of truth about legality, and transplant would go on refusing the same value.
- **Accept any scalar at any scalar position.** Rejected for the reason the transplant filter
  already states: a number at a position declared `string` reaches the target as
  `x.toUpperCase is not a function`, which is a finding about how the target reads a malformed
  object rather than about its logic.

## Measured outcome

Before, on the gate as described above: seven guided runs of 50 000 invocations at seeds
`0x5e3a91c4`, `0x12345678`, `0xdeadbeef`, `0xcafef00d`, `0x0badc0de`, `0x5eed1234` and
`0xabcdef01` reported no finding, and five random runs reported no finding.

After, at seed `0x5e3a91c4` with nothing else changed: the finding appears at invocation 10 477 of a
50 000 budget - `oracle: failed`, `via: engine-error-outside-declared-channel`,
`TypeError: Cannot read properties of undefined (reading 'code')` at `src/reissue.ts:49` in
`reissueSelfRedeemed`, `reproduced: true`, minimized from 2 789 to 432 bytes, 207 covered edges and
a corpus of 83. Five random runs on the same target still report no finding, which is the control
the boundary table used to break.

`a_paste_copies_a_literal_into_a_declared_string_field` in `crates/afz-engine/tests/mutation.rs`
pins the rule through the public `mutate` entry point, so the regression it catches is the copy
actually landing rather than a predicate returning `true`.

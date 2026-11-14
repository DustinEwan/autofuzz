# Domains beyond source code

The engine was built for functions in a tree of source files, and the shipped harnesses
reflect that: `js-ts`, `python`, and `wasm` read code, and `starter` shows you how to. The
`openapi` harness is the first one that is not like that. Its targets are HTTP operations, its
execution takes milliseconds instead of microseconds, and its bugs are status codes rather than
exceptions. Nothing in the engine noticed the difference, because the engine reads `targetClass`
for nothing but the record. That is the opening that this page is about: what else fits, what carries
it, and what does not exist yet.

This is a thinking document, not a roadmap. Nothing here is committed work.

## The four mechanisms that generalize

**Structured inputs, not bytes.** The engine generates and mutates `ValueTree` inputs from the
`TypeNode` types a plugin declares, so a target's parameter can be an object with bounds on its
numbers, an array of enums, a recursive tree. A policy graph, a transaction schedule, and a
sequence of UI events are all just objects with that shape. This is where the design parts
company with byte-level fuzzers: the mutation operates on the structure that the harness declared,
so almost every generated input reaches the interesting part of the domain instead of dying in
a parser.

**`failed` is a universal bug channel.** The oracle set is fixed - `failed`, `rejected`,
`deadline`, `memoryGrowth`, `processExit` - and a harness cannot add to it. It does not need
to: an invariant violation is an `Outcome::failed` carrying a `fault.class` that the harness chose,
and finding identity deduplicates on class, message, and top frame, so "invariant
`total_reserves >= user_deposits` violated" becomes exactly one finding however many times the
loop hits it. The `openapi` harness already works this way: an HTTP 500 is not a crash, and it
is still a finding.

**Coverage digests are opaque.** The guided loop diversifies the corpus by digest
(`crates/afz-engine/src/scan.rs`, `collect_coverage`), and the engine never asks what a digest
means. A harness that instruments no code can still report domain state as coverage: a hash of
the evaluated policy graph, the balance vector of a simulated ledger, the topology of a cluster
after a fault schedule. Each novel state earns corpus slots the way a novel branch does today.
A plugin without the `coverage` capability falls back to outcome digests
(`outcome_digest` in the same file), so even a silent harness keeps some guidance.

**The scenario is a function.** `prepare` materializes one receiver and `invoke` performs one
call, so a multi-step world has to live inside the harness: expose
`run_scenario(actors, schedule, faults)` as one target, let the engine mutate the schedule
structure, and let the harness replay it deterministically and return the verdict. The engine
never learns that a "call" was really a thousand-step simulation, and it does not need to.

The premise underneath all four is determinism. The corpus, `--resume`, and `seen.json`
suppression all assume the same input produces the same outcome, so a harness that replays
identically gets the full loop, and a harness wrapping a live system degrades guidance and
duplicate suppression without breaking either.

## The candidate domains

|Domain|Fits today?|What carries it|
|---|---|---|
|IaC and IAM policy|Yes|Deterministic evaluation, canary invariant, state digests|
|DeFi and virtual machines|Yes|Embedded deterministic VM, state digests, invariant-as-`fault.class`|
|Consensus and chaos|Yes, shaped|Whole run inside one invoke|
|Agent and LLM workflows|With mocks|Deterministic replay only; a live model degrades the loop|
|UI state machines|At a cost|Event sequences are `ValueTree`s; live UIs are slow and flaky|
|Differential and round-trip|One plugin only|One scan project names exactly one harness|

**Infrastructure-as-code and IAM policy** is the closest fit. Evaluation is a pure function -
`opa eval`, `terraform plan` against cached providers - so replay is exact. The harness exposes
one target that takes a policy document and a role graph and answers whether a canary principal
reaches a restricted resource; every mutation that grants the canary access is a `failed`
finding with the escalation path as its message. The input that the engine mutates is the entity
graph itself, which is the thing byte fuzzers cannot reach.

**Smart contracts and DeFi economics** fit the same shape through an embedded VM: a harness
that wraps `revm` is deterministic, fast, and instruments nothing, because the state digest
replaces branch coverage. The invariant check is a comparison that the harness runs after each
scenario, and multi-actor transaction interleaving is just an array that the engine shuffles.

**Consensus and network-partition testing** is Jepsen's shape: the harness is a deterministic
simulation controller, and the fault schedule - dropped RPCs, clock skew, a restart mid-commit -
is the input. The cost is the interaction model: the engine's loop is generate, invoke, observe,
so the harness must run the whole schedule inside one `invoke` and return the linearizability
verdict at the end. A harness cannot ask the engine for the next step mid-run, because that
channel does not exist.

**Agent and LLM workflow testing** splits on determinism, not on the protocol. With mocked or
replayed model responses, it is the chaos case again: the harness mutates tool-return payloads
and shared memory through scenario-as-function, and loops or unauthorized tool calls come back
as `failed`. Against a live model, the same scan runs, but repeated inputs produce repeated
findings less reliably, so `seen.json` suppression and corpus replay both weaken.

**UI state-machine testing** is feasible and expensive. Event sequences are `ValueTree`s and
invariants like "no overlapping interactive elements" fold into `failed`, but each invoke drives
a browser, which pushes you toward long deadlines and low throughput, and flaky rendering makes
every finding harder to pin down.

**Differential and round-trip testing** is the one domain that the one-harness rule actually
constrains. A scan project names exactly one plugin, so comparing PostgreSQL against DuckDB
today means one plugin that runs both engines and reports divergence as its bug - which works,
and is how I would build it. A real dual-target scan, where the engine launches two harnesses
and owns the comparison, would need a second launch in the scan project and a comparison oracle
in the engine.

## What the engine does not have

- **Harness-declared oracles.** A plugin folds its invariants into `failed`; the engine accepts
  no other oracle, such as "flag this input when the reserve delta is negative."
- **Feature negotiation.** The seven `Capability` values drive three engine decisions - the
  readiness gate, coverage collection, and reproducer rendering - and nothing else. There is no
  vocabulary in which a plugin declares `time_warping: not-applicable`. The target inventory and
  the `skipped` list do that job today: a capability that a harness cannot offer simply has no target,
  or a skip whose reason says why.
- **Dual-target scans.** One project, one harness, one comparison channel: the plugin's own.
- **Stepwise interactivity.** The engine observes after each invoke, not during.

None of the six domains needs a protocol revision except a true dual-target differential. The
`openapi` harness is the existence proof that the wire is not code-shaped: it ships runtime
targets, no coverage, and status-code bugs over the same `afz/1` the code harnesses speak.

## If you build one, build this one first

An IaC harness is the smallest honest step, because it reuses everything that the `openapi` harness
already proved and needs no service lifecycle at all: `opa eval` is a function you can call in
place. The repository has the same shape as the five that exist - `afz-harness.json`, the
plugin, a conformance emitter and validator, CI that asserts the golden - and its fixture is a
policy document plus a canary that must never be reachable. If that harness lands, the engine
has been proven against two domains that it was never designed for, which is the actual claim this
page is betting on.

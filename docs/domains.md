# Design notes: targets beyond source code

This page records design directions that are not built. It is not a roadmap, and nothing here is
supported. For what works today, read [`harnesses.md`](harnesses.md) and
[`architecture.md`](architecture.md).

The engine was built for functions in a tree of source files. The `openapi` harness is the first one
that is not like that: its targets are HTTP operations, its execution takes milliseconds instead of
microseconds, and its bugs are status codes rather than exceptions. The engine did not need a change,
because it reads `targetClass` only for the record. That is the opening this page explores.

## Four mechanisms that generalize

**Inputs are structured, not raw bytes.** A plugin describes each parameter with a type. The engine
builds and mutates values from that type. A parameter can be an object with numeric bounds, an array
of enums, or a recursive tree. A policy graph, a transaction schedule, and a sequence of UI events
all have that shape. Mutation works on the structure that the plugin described, so most inputs reach the
interesting part of the domain instead of dying in a parser.

**`failed` is a universal bug channel.** The oracle set is fixed: `failed`, `rejected`, `deadline`,
`memoryGrowth`, `processExit`. A harness cannot add to it, and it does not need to. An invariant
violation is an `Outcome::failed` carrying a `fault.class` that the harness supplies. Finding
identity deduplicates on target id, fault class, and top frame, so "invariant `total_reserves >=
user_deposits` violated" becomes one finding however many times the loop hits it. The `openapi`
harness already works this way: an HTTP 500 is not a crash, and it is still a finding.

**Coverage digests are opaque.** The guided loop diversifies the corpus by digest
(`crates/afz-engine/src/scan.rs`, `collect_coverage`), and the engine never asks what a digest means.
A harness that instruments no code can still report domain state as coverage: a hash of the
evaluated policy graph, the balance vector of a simulated ledger, or the topology of a cluster after
a fault schedule. Each novel state earns corpus slots the way that a novel branch does today. A plugin
without the `coverage` capability falls back to outcome digests (`outcome_digest` in the same file),
so even a silent harness keeps some guidance.

**The scenario is a function.** `prepare` materializes one receiver and `invoke` performs one call,
so a multi-step world has to live inside the harness. Expose `run_scenario(actors, schedule, faults)`
as one target, let the engine mutate the schedule structure, and let the harness replay it
deterministically and return the verdict. The engine never learns that a "call" was a thousand-step
simulation.

The premise underneath all four is determinism. The corpus, `--resume`, and `seen.json` suppression
all assume that the same input produces the same outcome. A harness that replays identically gets
the full loop. A harness that wraps a live system degrades guidance and duplicate suppression
without breaking either.

## Candidate domains

| Domain | Fits today? | What carries it |
| --- | --- | --- |
| IaC and IAM policy | Yes | Deterministic evaluation, a canary invariant, state digests |
| DeFi and virtual machines | Yes | An embedded deterministic VM, state digests, an invariant as `fault.class` |
| Consensus and chaos | Yes, with a shaped harness | The whole run inside one `invoke` |
| Agent and LLM workflows | With mocks | Deterministic replay only; a live model degrades the loop |
| UI state machines | At a cost | Event sequences are `ValueTree`s; live UIs are slow and flaky |
| Differential and round-trip | One plugin only | One scan project names exactly one harness |

**Infrastructure-as-code and IAM policy** is the closest fit. Evaluation is a pure function, such as
`opa eval` or `terraform plan` against cached providers, so replay is exact. The harness exposes one
target that takes a policy document and a role graph and answers whether a canary principal reaches
a restricted resource. Every mutation that grants the canary access is a `failed` finding with the
escalation path as its message. The input that the engine mutates is the entity graph itself.

**Smart contracts and DeFi economics** fit the same shape through an embedded VM. A harness that
wraps `revm` is deterministic, fast, and instruments nothing, because the state digest replaces
branch coverage. The invariant check is a comparison that the harness runs after each scenario, and
multi-actor transaction interleaving is an array that the engine shuffles.

**Consensus and network-partition testing** is Jepsen's shape. The harness is a deterministic
simulation controller, and the fault schedule, such as dropped RPCs, clock skew, or a restart
mid-commit, is the input. The cost is the interaction model. The engine generates one input, invokes the target once, and
observes the result, so the harness must run the whole schedule inside one `invoke` and return the
linearizability verdict at the end. A harness cannot ask the engine for the next step mid-run,
because that channel does not exist.

**Agent and LLM workflow testing** splits on determinism, not on the protocol. With mocked or
replayed model responses, it is the chaos case again: the harness mutates tool-return payloads and
shared memory through the scenario-as-function pattern, and loops or unauthorized tool calls come
back as `failed`. Against a live model, the same scan runs, but repeated inputs produce repeated
findings less reliably, so `seen.json` suppression and corpus replay both weaken.

**UI state-machine testing** is feasible and expensive. Event sequences are `ValueTree`s, and
invariants such as "no overlapping interactive elements" fold into `failed`. Each invoke starts a
browser, which pushes you toward long deadlines and low throughput, and flaky rendering makes every
finding harder to identify.

**Differential and round-trip testing** is the one domain that the one-harness rule constrains. A
scan project names exactly one plugin, so comparing PostgreSQL against DuckDB today means one plugin
that runs both engines and reports divergence as its bug. A real dual-target scan, where the engine
launches two harnesses and owns the comparison, would need a second launch in the scan project and a
comparison oracle in the engine.

## What the engine does not have

- **Harness-declared oracles.** A plugin folds its invariants into `failed`. The engine accepts no
  other oracle, such as "flag this input when the reserve delta is negative".
- **Feature negotiation.** The capability values drive the readiness gate, coverage collection, and
  reproducer rendering. There is no vocabulary in which a plugin
  declares `time_warping: not-applicable`. The target inventory and the `skipped` list do that job
  today.
- **Dual-target scans.** One project, one harness, one comparison channel: the plugin's own.
- **Stepwise interactivity.** The engine observes after each invoke, not during.

None of the six domains needs a protocol revision except a true dual-target differential. The
`openapi` harness is the existence proof that the wire is not code-shaped: it ships runtime targets,
no coverage, and status-code bugs over the same `afz/1` that the code harnesses speak.

## If you build one

An IaC harness is the smallest honest step. It reuses what the `openapi` harness already proved and
needs no service lifecycle at all, because `opa eval` is a function that you can call in place. The
repository has the same shape as the recommended harnesses: `afz-harness.json`, the plugin, a
conformance emitter and validator, and CI that asserts the report. Its fixture is a policy document plus a
canary that must never be reachable.

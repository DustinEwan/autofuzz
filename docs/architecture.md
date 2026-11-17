# autofuzz architecture

This document explains the design for someone who will change the engine or decide whether a new
idea belongs in it. It describes the system that is built. For current gaps, see
[`cli.md`](cli.md) and [`harnesses.md`](harnesses.md).

## Contents

- [The problem](#the-problem)
- [The boundary](#the-boundary)
- [Two target classes](#two-target-classes)
- [The protocol](#the-protocol)
- [The scan loop](#the-scan-loop)
- [Outcomes and oracles](#outcomes-and-oracles)
- [The scan result](#the-scan-result)
- [Determinism](#determinism)
- [Why the engine is Rust](#why-the-engine-is-rust)
- [Choices a review can override](#choices-a-review-can-override)

## The problem

Every fuzzer runs a loop: generate an input, run it, look at the result, keep what is interesting,
and repeat. The loop is the same whether the target is a TypeScript function or an HTTP route. The
work around the loop is not. For code, the fuzzer must find the functions, read their types, run
them in a runtime, and measure coverage. For an HTTP service, it must read an API document, send
requests, and accept that coverage is usually absent.

A single program that did all of those would need a parser, a runtime, and an instrumenter for every
language or service. autofuzz keeps the loop in one engine and puts the target-specific work in a
plugin. The engine can fuzz a new kind of target without a parser, a runtime, or an instrumenter for
that kind.

## The boundary

The engine owns the loop, the corpus, scheduling, deadlines, oracles, minimization, and the result.
A plugin owns discovery, argument construction, invocation, and classification.

The boundary is a process boundary. The plugin runs as a child process and speaks the pluginrpc
mechanism (`prpc/1`), which is one JSON message per line over stdio
([the mechanism and its envelope schemas](https://github.com/DustinEwan/pluginrpc)). That gives the
engine three things:

- A plugin can be written in any language that can read a line from stdin.
- A target that hangs or corrupts memory kills its own plugin process, not the scan.
- The engine can run many plugin processes in parallel without a language-specific threading model.

The cost is one process per worker and a pipe between them. That cost is small next to one target
invocation.

The engine accepts the plugin's answers. The plugin says which targets exist, what parameters they
take, whether a call succeeded, and how much memory it used. The engine records those answers and
acts on them. It never reads your source and never imports your code.

That division is deliberate. Only the plugin knows what an exception or a status code means for the
code or service it wraps. If the engine tried to classify a JavaScript `TypeError`, an HTTP 500, or
a Rust `Err`, it would need the language knowledge that the boundary exists to keep out.

## Two target classes

A plugin declares a target class in its handshake: `design-time` or `runtime`. The engine records
the class and does not branch on it. The difference matters to the plugin and to the user:

| | Design-time target | Runtime target |
| --- | --- | --- |
| What it is | A function, method, or exported symbol in source or a compiled artifact | A boundary that a live process exposes: HTTP route, RPC method, CLI command, message consumer |
| Discovery | Declaration analysis | An API document, a route table, or a running application |
| Invocation | Call the symbol in a process the engine controls | Send a request to a process the user runs |
| Coverage | Available when the runtime exposes it | Usually absent |
| Side effects | None by default | Real; a mutating operation can destroy user data |

The `openapi` harness is the first runtime harness. It shows that the engine does not assume source
code: its targets are HTTP operations, and its failures are status codes or contract violations.

## The protocol

A plugin answers eight methods. The full contract is in
[`harness-authoring.md`](harness-authoring.md). This section explains the lifecycle and the data
that the methods carry.

### The methods

The contract in TypeScript. The plugin contract tests in the harness repositories parse this block
and compare its method names with the wire vocabulary, so keep the block and the two-space indent on
each method line.

```ts
export interface HarnessPlugin {
  handshake(): Promise<{
    protocolVersion: string;          // "afz/1"
    name: string;                     // "afz-fakeplugin"
    version: string;
    targetClass: "design-time" | "runtime";
    capabilities: Capability[];
    languages: string[];              // ["typescript", "javascript"]
    discoverRequiresExecution: boolean;
  }>;

  /** Enumerate fuzzable entry points. Must terminate; discovery never executes target code. */
  discover(req: { root: string; include?: string[]; exclude?: string[];
                  configPath?: string }): Promise<{
    targets: TargetDescriptor[];
    skipped: SkippedTarget[];         // every non-fuzzed symbol, with a reason
  }>;

  /** Build the receiver plus arguments for one call, or report why it is impossible. */
  prepare(req: { targetId: string; values: ValueTree[] }):
    Promise<{ ok: true; materialized: MaterializedCall } | { ok: false; reason: SkipReason }>;

  /** One invocation. Never throws over the wire; faults are Outcome values. */
  invoke(req: { invocationId: number; targetId: string; values: ValueTree[];
                deadlineMs: number; collectCoverage: boolean }): Promise<Outcome>;

  /** Shrink a failing input while it still reproduces the same fault class. */
  minimize(req: { targetId: string; values: ValueTree[]; fault: FaultSignature;
                  budgetMs: number }): Promise<{ values: ValueTree[]; steps: number }>;

  /** Render a standalone script that replays the confirmed call in the target language. */
  reproducer(req: { targetId: string; values: ValueTree[] }):
    Promise<{ extension: string; script: string }>;

  /** Map coverage edge ids to the source ranges they stand for. */
  coverageMap(): Promise<CoverageMap>;

  /** Prove the environment the targets live in is up before workers spawn. Asked only
      when the handshake declares `readiness`. */
  ready(): Promise<{ ready: boolean; detail?: string }>;
}
```

The lifecycle matters as much as the method names:

| Method | When the engine calls it | What it gets back |
| --- | --- | --- |
| `handshake` | once on the probe connection | identity and capabilities |
| `discover` | every connection | targets and skipped symbols |
| `prepare` | first invocation of a target in a process | a materialized call or a skip |
| `invoke` | once per call | one outcome |
| `coverageMap` | every 5000 invocations and at the end | edge ids and source ranges |
| `reproducer` | once per finding, after minimization | a script and its extension |
| `ready` | once before workers start, when declared | ready or not ready |

The protocol includes `minimize` for a plugin that can shrink an input in target terms. The engine
shrinks by re-invoking the target with smaller values, because that works for every target and needs
no extra plugin code. The engine does not call `minimize`, and a plugin does not need to answer it.

### What a target is

A target is one thing that the engine can call: a function, a method, an HTTP route, or another entry
point. The plugin reports an id that stays the same across runs, a name, its origin, its parameters
and their types, what it returns, what it can do, and whether the engine can build its arguments. The schema is the authority for the fields.

The argument types are the generator's vocabulary: scalars, string formats, literals, collections,
objects, tuples, unions, buffers, promises, functions, streams, opaque types, and recursive
references. The engine uses the type and its bounds to build values. When a type does not fit, the
plugin reports the target as `unsupported-signature` instead of inventing a shape.

The engine sorts every target into one of three states:

| State | What it means | Example |
| --- | --- | --- |
| `constructible` | The engine can build every argument from generated values. | `parse(text: string)` |
| `fixture-required` | One argument needs an object that the engine cannot build. The target stays in the skipped list. | `findUser(repo: UserRepo)` |
| `opaque` | The target needs a live system that the plugin cannot construct. | `connect(socket: WebSocket)` |

The inventory reports all three counts, so a scan that reached only part of an API says so. For a
skipped target, the record names the blocking type and field, so the plugin author can see what
would unlock it.

### Capabilities

A plugin declares what it can do at handshake. The engine reads three:

| Capability | What the engine does |
| --- | --- |
| `coverage` | Turns on `collectCoverage` and the `coverageMap` walk. Without it, the scheduler uses the outcome instead of edges. |
| `reproducer` | Asks for a script once per finding. Without it, the finding carries no script. |
| `readiness` | Calls `ready` before it starts workers. A not-ready answer stops the scan. |

The other capability names are recorded in the scan result and change no engine behavior. A plugin
should declare what it answers and leave out what it does not.

## The scan loop

A scan runs in batches. Each batch takes inputs from the corpus, mutates them, and sends the results
to the workers. The workers run the calls and return outcomes. The engine adds an input to the
corpus when it produces a coverage edge that the corpus does not have yet.

When the plugin reports coverage, the digest is a sorted list of stable edge identifiers that the
call touched. The engine never asks what an edge means. It only asks whether the set is new. That
rule is what makes the loop work across languages: the plugin can report V8 block ranges, Python
line events, or wasm counters, and the engine treats all three as opaque identifiers.

When the plugin reports no coverage, novelty comes from the observable outcome instead. The loop is
unchanged; only the novelty function differs.

Mutation operates on `ValueTree`, not on bytes. Every node records its type and its generation
provenance, which is what makes type-aware mutation and later minimization possible across
languages. One execution applies a short run of node-level operators. That set includes copying one
node from an input into another, because a gate that compares two independently generated fields is
otherwise unreachable however long the scan runs (Decision 2).

## Outcomes and oracles

A plugin reports one of three outcomes for each call. The engine uses that outcome and never reads
the target's exceptions, return values, signals, or status codes.

```ts
export type Outcome =
  | { kind: "ok";       durationMs: number; memoryBytes?: number; coverage?: EdgeId[]; value?: ValueTree }
  | { kind: "rejected"; durationMs: number; coverage?: EdgeId[]; via: string; detail: string }
  | { kind: "failed";   durationMs: number; coverage?: EdgeId[]; via: string; fault: FaultSignature };
```

| Outcome | Result | What happened | Example |
| --- | --- | --- | --- |
| `ok` | valid | The target accepted the input and returned normally. | `parse("a: 1")` returns a document. |
| `rejected` | valid | The target refused the input on purpose, through its own validation path. | `parsePrice(-1)` throws `RangeError` because the price must be positive. An HTTP handler returns 400 with a validation error. |
| `failed` | invalid | The target tried to accept the input and broke. | `parse(null)` throws `TypeError` because the code assumed a string. A handler panics or returns 500. |

The first two are valid results: the target either accepted the input or refused it as invalid.
`failed` is the only outcome that means the target broke. The difference between `rejected` and
`failed` is intent: a rejection is deliberate; a failure is a break the target did not intend.

The plugin decides which one happened, because only the plugin knows how the target says no: a
throw at a guard site, a returned `Err`, or an HTTP 4xx. A guard site is a line where the target
checks its input and throws if the check fails. The plugin records those lines during discovery. A
throw from a guard site, or an instance of a class that the scan project lists in
`harness.options.rejectClasses`, is a rejection. A runtime-raised `TypeError` outside a guard site,
an unhandled exception, or a panic is a failure.

`via` records the rule that produced the classification, so a triager can audit the decision and a
user can override it.

The engine reads only the outcome kind and the `via` string. That is why a Rust function can reject
with `Err(...)` and a C function can reject with a return code without changing the engine.

Five oracles turn an outcome into a finding:

| Oracle | Turns into a finding when | Example |
| --- | --- | --- |
| `failed` | The target broke. | A panic or an unhandled exception. |
| `rejected` | The target refused the input, when the oracle is enabled. | A 400 response. |
| `deadline` | The call ran past its deadline. | A parser that hangs on a long input. |
| `memoryGrowth` | The call used more than the limit. | A decoder that allocates without a bound. |
| `processExit` | The plugin process died. | A segfault in the target. |

The `rejected` oracle is off by default; the other four are on. The defaults and the override format
are in [`cli.md`](cli.md#the-oracles-block). A name that the engine does not implement is a
configuration error, not a silently disabled check.

`memoryBytes` is reported by the plugin, not measured by the engine. Process sampling would add a
platform-specific dependency for a number that the plugin already reports.

## The scan result

A scan's result is `scan.jsonl`: one JSON record per line, fsynced as it is written. Durable
lines record retained corpus entries, reported findings, and coverage samples as they happen,
and the final `finished` line holds the complete document — the inventory, the execution
counts, the coverage, the findings, and the paths of the artifacts. The document's schema is
`docs/schema/afz-1/scan-result.json`.

The finding identity is a hash over the target id, the fault class, and the top fault frame. It does
not include the input. That is what makes one bug one finding across scans, which a triage queue and
a pull-request bot both need.

The identities that a project has already reported live in `<output.dir>/seen.json`, which every scan
opens and updates. A repeat is still written, with `seenBefore: true`, because a report that hid a
bug still in the tree would be worse than one that repeats itself. The engine does not minimize a
repeat and does not ask for a reproducer for one.

The `finished` line is appended last, after every artifact the document lists, so a reader that
finds it never sees a scan directory whose artifacts are still being written. A journal without
that line is an unfinished run: `--resume` restarts from the lines that landed.

## Determinism

One 32-bit seed per scan drives every random choice: corpus selection, mutation, and value
generation. The result records the seed, the config hash, the target revision, and the plugin
version. The same seed against the same tree and the same configuration replays the same sequence of
calls.

`--resume <scanDir>` loads the corpus that an earlier scan wrote. Suppression of recorded identities
comes from `seen.json`, which the engine reads whether or not you resume.

Coverage ids include the file URL, so a corpus carried into a materialized git tree reports every
edge as new. The run is valid, but its coverage ratio is not comparable to the earlier one.
`engine.log` records that condition when `--resume` and `--git` are both present.

## Why the engine is Rust

The engine is Rust. The `js-ts` plugin is Node with TypeScript, and the `python` plugin is CPython.
The plugin boundary makes the engine language free, so the choice follows from supervision: signals,
`waitpid`, resource limits, and kill timers are direct in Rust, and one static binary has no runtime
to install. The cost is a slower edit-compile loop in generation and scheduling, which are small and
tested with golden files.

## Choices a review can override

1. **The plugin runs out of process.** The process boundary gives language neutrality and fault
   isolation. Override it only if plugin startup cost becomes dominant, in which case add an
   in-process variant behind the same contract.
2. **Coverage is a declared capability.** HTTP targets usually cannot provide it. Override it only
   if every harness can supply per-call edges.
3. **Discovery does not execute target code.** Importing a user module runs user code inside the
   plugin. A plugin that must import the target declares `discoverRequiresExecution: true`, and the
   engine applies the invocation isolation and deadline to it.
4. **Unreachable targets are reported, not dropped.** The inventory stays honest. Override it if the
   product needs one coverage number without the breakdown.
5. **Runtime targets send only non-mutating methods unless the scan project lists the operation.**
   A fuzzer that deletes user data ends the product. The policy lives in the plugin that sends the
   requests.
6. **The scan result is one immutable document with content-addressed artifacts.** Finding identity
   stays stable across scans, so triage and pull-request tooling can match findings.
7. **The plugin classifies the outcome.** Rust rejects through a return value and C rejects through a
   return code, so no engine-level rule can identify a rejection without language knowledge.

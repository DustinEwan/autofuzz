# autofuzz — architecture

This document describes the system that is built. It is for someone about to change the engine, or to
decide whether an idea belongs in the engine at all. Each section states the rule, then the reason.

Two other documents carry what this one does not. [`docs/decisions/`](decisions/) holds the dated
records: what was considered, what was rejected, and what was reverted.
[`docs/harness-authoring.md`](harness-authoring.md) holds the contract that a plugin must satisfy, down to
the wire. The last section here, [Reserved surface](#reserved-surface-what-is-not-built), lists what
the project has designed but not built, so that nothing else in this document has to be read with an
asterisk.

## The problem

Nothing about fuzzing is language-neutral except the loop. Discovery requires the language's type system.
Execution requires the language's runtime. Coverage requires the language's compiler or virtual machine.

So the design question is where to cut. The answer is that the engine owns the loop and the data, and a
harness plugin owns everything that touches one language or runtime.

## The cut

The engine discovers through a plugin, generates inputs, schedules workers, enforces deadlines, applies
oracles, minimizes, and writes the scan result. The plugin enumerates callables, materializes arguments,
runs one call, and reduces that call to one outcome.

One consequence reaches everywhere: coverage feedback is a capability that a plugin declares, not a
requirement that the engine imposes. A harness with no coverage still fuzzes, because the scheduler falls
back to novelty over the outcome that the call returned.

## What is built, and what is not

| | Status |
|---|---|
| Design-time targets: functions, methods, exported symbols | Built, four harnesses |
| Harness plugins `js-ts`, `python`, `wasm`, `starter`, `openapi` | Built. Each is gated in its own repository: install drives the wire rules and that repository's CI asserts the template golden |
| Coverage-guided loop, corpus, scheduler | Built |
| Oracles `failed`, `rejected`, `deadline`, `memoryGrowth`, `processExit` | Built |
| Scan result document, `json` and `markdown` renderers | Built |
| Git change-set scoping (`--git`) | Built |
| MCP server on stdio | Built |
| Runtime targets (HTTP routes, RPC methods, CLI commands) | Engine-side: built. The engine launches any plugin whatever class it drives; `autofuzz-harness-openapi` fuzzes OpenAPI 3 documents against a live service |
| Fixture format for `fixture-required` parameters | Not built. Those targets are reported as skipped |
| `differential` oracle, property-assertion oracles | Not built. Naming one is a configuration error |
| SARIF renderer | Not built |

## Two target classes

Design-time and runtime targets differ in discovery, invocation, feedback, and risk. They share the loop,
the finding model, and the scan result. The plugin declares its class in the handshake, the engine records
it in the scan result, and no engine decision branches on it: what a plugin can drive is the plugin's
business.

| | Design-time target | Runtime target |
|---|---|---|
| What it is | A public function, method, or exported symbol in source or a compiled artifact | A boundary a live process exposes: HTTP route, RPC method, CLI command, message-queue consumer |
| Discovery | Declaration analysis over source, declarations, or module exports | Route table extraction from source or framework metadata, an OpenAPI/proto document, or a running application |
| Invocation | Call the symbol in a process we control | Send a request to a process the user runs |
| Input shape | Typed argument list plus constructed receiver state | Method, path, headers, query, body, auth context |
| Coverage feedback | Available whenever the runtime exposes it | Available only if the target process is instrumented; usually absent |
| Failure signal | Thrown exception, rejection, hang, memory growth, process exit | Status code class, latency, crash of the serving process, contract violation |
| Side effects | None by default; the process dies with the run | Real. A mutating operation can destroy user data |
| Concurrency | The engine sets the worker count | The scan must respect rate limits and target capacity |

## Plugin transport

A plugin runs out of process and speaks JSON-RPC over stdio, one process per plugin instance. This is the
same transport model as LSP and DAP. Three reasons:

- A plugin written in Rust, Python, or Node is equally valid, because all the engine requires is a child
  process with two pipes.
- A target that hangs or corrupts memory kills its own plugin process, not the scan.
- The engine runs N plugin instances in parallel without any language-specific threading story.

In-process plugins are not supported. A single fault in a plugin would take the scan down with it, and the
stdio cost disappears next to the cost of one target invocation.

The engine never guesses how to start a plugin. `harness.command` gives the argv, for every harness, and
`ScanProject::validate` rejects a harness name that has neither a `command` nor an `afz install-harness`
record. The plugin runs with the scan root as its working directory, and the engine passes `AFZ_ROOT` and
`AFZ_CONFIG` in its environment.

## The eight methods

All eight are on the wire, and a plugin must answer every one of them. `crates/afz-protocol/src/rpc.rs`
defines them, and `docs/schema/afz-1/` is the authority for every field, because it is generated
from the Rust
types that the engine compiles against.

The contract in TypeScript, which is how the shape is easiest to read. Both plugin contract tests
parse this block: `test/contract.test.mjs` in `autofuzz-harness-js-ts` and
`test/contract_test.py` in `autofuzz-harness-python` each extract the method names from it and compare them
with the wire vocabulary. Keep the block, and keep the two-space indent on each method line, or those
tests stop finding the interface.

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

| Method | What the plugin must return | Engine timeout |
|---|---|---|
| `handshake` | Protocol version `afz/1`, name, version, target class, capabilities, languages, `discoverRequiresExecution` | none, but a mismatch stops the run |
| `discover` | Every fuzzable target, and every symbol it skipped with a reason | 120 s |
| `prepare` | The materialized call, or a skip reason | 120 s |
| `invoke` | One `Outcome`. It never throws over the wire | the `deadline` oracle's `ms`, and no deadline at all when that oracle is off |
| `minimize` | A smaller input that still reproduces the fault class | not sent today, see below |
| `reproducer` | A standalone script and its file extension | 10 s |
| `coverageMap` | The edge ids and the source ranges they stand for | 10 s |
| `ready` | Whether the environment the targets need is up, and the reason if it is not | 5 s |

`minimize`, `reproducer`, `coverageMap`, and `ready` are optional in one precise sense. A plugin that cannot do the
work leaves the capability out of its handshake, and answers the method with a `-32601` or `-32600` error
frame. Both shipped plugins take that route for `minimize`.

The engine sends `minimize` to no plugin today. Minimization is engine-side delta debugging over repeated
`invoke`, bounded by 2000 attempts and a 300 second budget. A plugin that answered `minimize` would be
answering a request that never arrives.

`discover` must not execute the code that it enumerates. Importing a user module runs that user's code
inside the plugin, so `js-ts` reads declarations statically and `python` parses with `ast`. A plugin that
genuinely must import the target declares `discoverRequiresExecution: true`, and the engine then applies
the same isolation and the same deadline that it applies to invocations.

## Capabilities

A plugin declares what it can do at handshake, and the engine adapts instead of assuming. Three of the
seven names change engine behavior today, and the table says which.

| Capability | Meaning | Does the engine read it today? |
|---|---|---|
| `coverage` | Per-invocation coverage digest is available | Yes. Without it the scheduler scores novelty from the outcome instead of from edges, and `coverage.json` is not written |
| `reproducer` | The plugin can render a standalone replay script in the target language | Yes. Without it a finding carries no script, and the minimized input in `inputs/` is the only replayable artifact |
| `stateful` | The plugin can build receiver state | No. Absent behavior is a property of the plugin's own discovery, not an engine gate |
| `minimize` | The plugin can shrink an input in target terms | No. The engine minimizes through `invoke` |
| `sideEffects` | Invocations may change external state | No. The engine sends whatever the plugin discovered; the side-effect policy lives in the plugin that sends the requests |
| `concurrent` | The plugin tolerates parallel instances | No. The engine starts one process per worker for every harness |
| `readiness` | The plugin can prove the environment its targets live in is up | Yes. The engine calls `ready` after discovery, and a not-ready answer aborts the scan before any worker spawns |

A plugin author who reads this table and then reads `harness-authoring.md` gets the honest version: declare
what you answer, and do not declare what you do not.

## Interface IR

One normalized description of a callable, produced by every plugin. The fields below are the real ones,
from `docs/schema/afz-1/target-descriptor.json`.

| Field | Notes |
|---|---|
| `id` | Stable content hash of module path, symbol, and signature |
| `targetClass` | `design-time` or `runtime` |
| `origin` | `file` and `line` for source, `module` for a compiled artifact, `http` for an operation in an API document |
| `name` | `parseConfig`, `UserRepo.findById`, `POST /v1/tokens` |
| `parameters` | Each with `name`, `type`, `required` |
| `returns` | A `TypeNode`, when the callable has a declared return type |
| `effects` | `async`, `throws`, `streams`, `callbacks`, `mutatesState` |
| `materialization` | `constructible`, `fixture-required`, or `opaque` |
| `guardSites` | The lines inside the target where it raises or throws. This is what lets the plugin call a refusal a refusal |

### TypeNode

`TypeNode` is the generator's vocabulary, and it has 13 variants. The table below lists them; where a
variant carries a sub-kind, that sub-kind is in parentheses.

| Variant | Notes |
|---|---|
| `scalar` | `ScalarKind`: `null`, `undefined`, `boolean`, `number`, `bigint`, `string`. A number carries `int`, `finiteOnly`, `min`, `max` when declared |
| `stringFormat` | `email`, `uuid`, `date-time`, `hostname`, `ipv4`, `uri`, plus plugin-defined formats |
| `literal` | Enum members and string unions become literal sets |
| `collection` | `CollectionKind`: `array`, `set`, `map`, with element and key types and declared length bounds |
| `object` | Named fields, each with `required`. Recursive references are allowed through `ref` |
| `tuple` | Positional and fixed |
| `composite` | `CompositeKind`: `union`, `intersection`. Discriminant-aware generation when a literal discriminant exists |
| `buffer` | `Uint8Array`, `ArrayBuffer`, with byte length bounds |
| `promise` | Marks an async effect. The harness awaits it under the invocation deadline |
| `function` | A callable stub whose behavior is configurable: return-first, throw, or capture arguments |
| `stream` | An in-memory source with a bounded item count |
| `opaque` | A class instance, socket, or any type the plugin cannot build |
| `ref` | A named recursive reference. Generation enforces a depth budget and counts `ref` uses per path, which is what makes recursion terminate |

`materialization` is the honest accounting of what the tool can reach. `constructible` means the whole
parameter graph is buildable from generated values. `fixture-required` means a user-supplied fixture could
fill the gap, and no fixture format exists yet. `opaque` means the entry point is unreachable without a live
system. The inventory reports all three counts, so a scan that reached 5 percent of an API says so.

Unreachable entry points are reported rather than dropped, and that report is the whole mechanism today.
Each skipped record gives the blocking type, the module and the property path of the blocking field, and
how many entry points share the block:

```json
{ "targetId": "afz:t:9c1f", "name": "UserRepo.findById",
  "reason": "unconstructible-parameter",
  "blockedBy": { "type": "Db", "module": "src/db.ts", "path": "receiver.db" },
  "unlocksTargets": 14 }
```

That record is deliberately complete enough for a person, or a coding agent, to act on. Write a constructor
for `Db`, and 14 entry points become fuzzable.

## Generation and feedback

Coverage is a capability. When it is present, every invocation returns a digest: a sorted list of stable
edge identifiers that the call touched. The engine keeps a corpus and retains an input when it produces an
edge set new to the corpus. Scheduling weights corpus entries by new-edge yield and by time since
last selection, so unexplored entries are revisited. This is the novelty rule that makes AFL++ effective,
applied to structured inputs instead of bytes.

When `coverage` is absent, novelty comes from the observable outcome instead. The loop is unchanged; only
the novelty function differs.

Mutation operates on `ValueTree`, not on bytes. Every node records its type and its generation provenance,
which is what makes type-aware mutation and later minimization possible across languages. One execution
applies a short run of node-level operators. That set includes copying one node from an input into another,
because a gate that compares two independently generated fields is otherwise unreachable however long the
scan runs (Decision 2).

## Outcome classification

The engine never reasons about exceptions, `Result` values, signals, or status codes. Every plugin reduces
one invocation to exactly one of three outcomes, and every oracle operates on that value alone.

```ts
export type Outcome =
  | { kind: "ok";       durationMs: number; memoryBytes?: number; coverage?: EdgeId[]; value?: ValueTree }
  | { kind: "rejected"; durationMs: number; coverage?: EdgeId[]; via: string; detail: string }
  | { kind: "failed";   durationMs: number; coverage?: EdgeId[]; via: string; fault: FaultSignature };
```

`rejected` means the target examined the input, decided it was not acceptable, and said so through a channel
that its own signature declares. `failed` means the target tried to accept the input and broke. `via` gives
the rule that produced the classification, so a triager can audit the decision and a user can override it.

Which channel is declared is language knowledge, and it stays inside the plugin. The table below states the
rule per language. Two of these rows are implemented; the rest show how the same decision travels.

| Language or boundary | `rejected` | `failed` | Implemented |
|---|---|---|---|
| TypeScript / JavaScript | `throw` at a source location the target wrote, which discovery records as a guard site, or an error class the scan project lists in `harness.options.rejectClasses` | engine-raised `TypeError`, `RangeError`, or `URIError` at any other location; worker exit; unhandled rejection | yes |
| Python | a raise at a guard site, or an exception class in `harness.options.rejectClasses` | `TypeError`, `AttributeError`, `IndexError`, and any raise outside the declared set | yes |
| Rust | `Err(e)` returned from `Result`, `None` returned from `Option` | `panic!`, `unwrap` on an `Err`, index out of bounds, divide by zero, `unreachable!`, `assert!`, `SIGSEGV`, `SIGABRT` | no plugin answers this yet |
| Go | a returned non-nil `error` | `panic`, write to a nil map, index out of range, race detector report | no |
| Java / Kotlin | an exception type in the method's `throws` clause | unchecked `RuntimeException` or `Error`, JVM assertion, out-of-memory | no |
| C / C++ | a nonzero or negative return code, `errno`, `std::error_code` | signal delivery, ASan or UBSan report, `assert`, `std::terminate` | no |

Rust shows why the rule cannot be "did it throw". `Err(PriceError::UnknownCurrency)` is an ordinary return
value, so the rejection channel carries no exception at all, and the crash channel is a panic. The
TypeScript case and the Rust case are one decision in two machineries.

Two consequences for the wasm harness. A Rust panic or a wasm trap ends the instance, so the harness
re-instantiates after any `failed` outcome and reports that invocation's coverage before it discards the
instance. And Rust checks integer overflow only when `overflow-checks = true`. That is the default in debug
profiles and off in release, so the fuzz build must enable it or an entire bug class reports as `ok`.

## Oracles and defaults

Oracle configuration sits one level above the classification and never reaches into language mechanics.
These are the defaults that the engine applies when a scan project says nothing
(`crates/afz-engine/src/config.rs`):

| Key | Default |
|---|---|
| `oracles.failed` | `severity: "high"` |
| `oracles.rejected` | `severity: "off"`, and you can turn it on per target |
| `oracles.deadline` | `severity: "medium"`, `ms: 2000`. Turning it off removes the deadline from every call |
| `oracles.memoryGrowth` | `severity: "medium"`, `mb: 256` |
| `oracles.processExit` | `severity: "high"` |
| `oracles.overrides` | empty. Each entry names a `target`, a `via`, and a `treatAs` of `ok`, `rejected`, or `failed` |
| `budget.invocations` | 100000 |
| `budget.wallClockMs` | 900000 |
| `budget.workers` | 0, which means one plugin process per logical CPU |
| `budget.maxCorpus` | 2048 |
| `budget.strategy` | `guided`. `random` generates without the corpus |
| `output.dir` | `.autofuzz` |
| `output.renderers` | `["json", "markdown"]`. The list decides only whether `report.md` is rendered |

Severity has five values: `off`, `low`, `medium`, `high`, `critical`.

`memoryBytes` on an `Outcome` is reported by the plugin, not measured by the engine. Engine-side process
sampling would add a platform-specific dependency for a number that the plugin already reports.

The project has one rule about referring to something that is not built, and it applies to oracles and
renderers alike: a name that the engine does not implement is a configuration error, never a silently
disabled check.
`oracles: { "differential": { "against": "v1.4.0" } }` therefore fails at load with
`oracle "differential" is not implemented in this version`.

## Scan result

One JSON document per scan, written once and never edited in place. Renderers read that document and open
nothing else. Here is a real one, from a scan of the `billing` fixture, with the findings and samples
elided:

```json
{
  "schemaVersion": "afz-scan/1",
  "scanId": "M78QEADG9A",
  "startedAt": "2026-11-12T12:00:00.000Z",
  "finishedAt": "2026-11-12T12:00:07.000Z",
  "engine": { "version": "0.1.0", "seed": "0x5e3a91c4", "configHash": "sha256:f366c373…" },
  "target": {
    "root": "/home/me/projects/billing-service",
    "revision": "git:8bbdf07",
    "harness": { "name": "js-ts", "version": "0.1.0", "targetClass": "design-time",
                 "capabilities": ["coverage", "stateful", "concurrent", "reproducer"] }
  },
  "git": { "selector": "HEAD", "base": "1f4a9c0d…", "tree": "8f2c1ab4…",
           "changedFiles": 1, "worktree": "/repo/.autofuzz/git-scope/41-2" },
  "inventory": {
    "discovered": 46, "fuzzable": 38, "skipped": [],
    "materializationCounts": { "constructible": 38, "fixture-required": 8 }
  },
  "execution": { "invocations": 50000, "corpusSize": 85, "coverageEdges": 209,
                 "coverageRatio": 0.6239, "deadlineExceeded": 0, "deadlineCuts": 0,
                 "stoppedReason": "budget", "restarts": 0, "unattempted": 0,
                 "disabledTargets": [], "samples": [] },
  "findings": [],
  "artifacts": { "corpus": "corpus/", "coverage": "coverage.json", "log": "engine.log" }
}
```

Three decisions in that shape are worth pointing out:

- `finding.identity` is a hash over target id, fault class, and the top fault frame, not over the input.
  That is what makes one bug one finding across scans, which a triage queue and a pull-request bot both
  need. Inputs and stacks live in content-addressed files beside the document, so the document stays small
  enough to load whole.
- The identities that this project has already reported live in `<output.dir>/seen.json`, which every scan
  of the project opens and updates. A repeat is still written, with `seenBefore: true`, because a report
  that hid a bug still in the tree would be worse than one that repeats itself. The engine does not
  minimize a repeat and does not ask the plugin for a reproducer for one, so a repeat carries its original
  input and nothing else.
- `git` is present only for a scan run with `--git <selector>`, and `inventory` then counts that change set
  rather than the package. Twelve clean targets inside a change set is a narrower claim than a clean
  package, and the scan result keeps the two claims apart.
- The writer saves `scan.json` last, after every artifact that the document lists. A reader that finds the
  document never sees a scan directory whose artifacts are still being written.

## Determinism

One 32-bit seed per scan drives every random choice: corpus selection, mutation, and value generation. The
scan result records the seed, the config hash, the target revision, and the plugin version. The same
`--seed` against the same tree and the same configuration replays the same sequence of calls.

`--resume <scanDir>` loads the corpus that an earlier scan wrote, so a scan can continue from the inputs
that scan had kept. Suppression of recorded identities is a separate mechanism: it comes from the
project's `seen.json`, which the engine reads whether or not you resume. Every plugin request carries an
`id` that the engine assigns per process, so a response from a parallel worker is still attributable.

One exception is recorded rather than hidden. Coverage ids are hashed from the file url, so a corpus
carried into a materialized git tree reports every edge as new. The run is valid, but its ratio is not
comparable to the earlier one, and `engine.log` says so when both flags are present.

## Harnesses today

| Harness | Discovery | Invocation | Coverage | Capabilities declared |
|---|---|---|---|---|
| `js-ts` | Static declaration analysis. `typescript@7` ships no JavaScript compiler API, so the classic API comes from `@typescript/typescript6@6.0.2`, installed under the `typescript` alias | One warm Node worker thread per plugin process serves many invocations. Each invocation runs in a fresh `vm` realm built from text the plugin transpiled itself, so module-level state resets per call and one call remains a complete reproducer (Decision 6) | Per invocation, from one armed `Profiler.takePreciseCoverage` window per worker, with an invocation's digest scoped to the scripts it first reported. Ranges map back to `.ts` positions through the transpile source map | `coverage`, `stateful`, `concurrent`, `reproducer` |
| `python` | `ast` parse of the modules in the scan project. Nothing is imported, and a def whose name starts with an underscore can be withheld from the inventory | One CPython process per worker. Each invocation calls the function or constructs the class with no arguments, and reads coverage from `sys.monitoring` line events | Per invocation, from `sys.monitoring` line events | `coverage`, `reproducer` |
| `wasm` | The exported functions of a core wasm module. The engine materializes arguments from the same IR and the plugin calls the export directly, so the module's own signature is the only type information involved | One wasmtime instance per worker, in a plugin process per worker as with every other harness | The harness rewrites the module it is handed: one counter byte per function entry and per structured block head, in a memory of its own that it exports as `__afz_counters`. The guard is `i32.const <edge>; i32.store`, and the table is read back after the call, including after a trap. Compile-time pcguard stays available to a target that wants finer edges (Decision 3) | `coverage`, `concurrent` |

`NODE_V8_COVERAGE` is not used: it is inconsistent with worker threads, and that issue is closed as not
planned (nodejs/node#46378).

## Environment

The `js-ts` plugin requires Node 24 or newer and runs with `--experimental-vm-modules`, because each
invocation is a `vm.SourceTextModule` realm. The `python` plugin requires Python 3.12 or newer, because
coverage comes from `sys.monitoring`. Both plugins are stdlib-or-manifest only: the Python plugin imports
nothing outside the standard library, and the Node plugin has one dependency.

The engine requires a stable Rust toolchain, 1.88 or newer. The wasm harness additionally needs the
`wasm32-unknown-unknown` target to build probe modules. It needs no C or C++ toolchain, no Binaryen, no
wasi-sdk, and no wasm-opt. Coverage instrumentation is a post-hoc rewrite of a core wasm module, done with
`wasmparser` and `wasm-encoder`, and both are compiled into the engine. Compile-time pcguard stays
available as an alternative, and a target that selects it brings its own toolchain; the engine never
requires one.

## Implementation language

The engine is Rust. The `js-ts` plugin is Node with TypeScript, and the `python` plugin is CPython. This
split follows from the plugin boundary: because plugins run out of process over stdio, the engine language
is free, so we choose it for supervision rather than for the target's ecosystem.

Four reasons the engine is Rust rather than Node:

- The engine is a long-lived supervisor over hundreds of thousands of invocations, with tight loops over
  coverage digests, deterministic replay, and process lifecycle control. Signals, `waitpid`, resource
  limits, and kill timers are direct in Rust and awkward through Node's child process API.
- The WebAssembly harness is Rust, because reading the guard table from linear memory, enforcing a deadline
  with epoch interruption, and recreating an instance after a trap are all first-class wasmtime operations.
- Distribution is one static binary in CI, with no runtime and no dependency tree to install.
- The wire contract becomes typed structs. `schemars` emits the committed JSON Schema from the same
  types that the engine compiles against, so a protocol drift is a compile error rather than a runtime
  surprise.

The cost is a slower edit-compile loop in the parts that iterate most: generation, mutation, and scheduling
policy. That is accepted, because those modules are small and pure, and their tests are golden-file
comparisons that a single `cargo test` catches.

The exact versions that matter are recorded in `Cargo.toml` and in each harness repository:
`package.json` in `autofuzz-harness-js-ts`, `Cargo.toml` in `autofuzz-harness-wasm`. Two things are
load-bearing enough to call out here. `wasmparser` and `wasm-encoder` carry the wasm instrumentation
pass. `@typescript/typescript6` is aliased to `typescript` in the js-ts harness, because the plugin
transpiles the sources of the targets that it scans, rather than having Node's module system read
them.

Three things are deliberately not used. The `binaryen` crate is one, because its build requires a C++
toolchain. `wasm-instrument` is another, because its last release predates the current wasm proposal set.
The third is LibAFL: the engine is ours by requirement, and the two pcguard callbacks we need are about
thirty lines rather than a framework dependency.

## Assumptions

These are the choices that a design review can override, and each one gives the cost of overriding it.

1. **Plugin transport is out-of-process JSON-RPC over stdio.** Chosen for language neutrality and fault
   isolation. Overridden only if plugin startup cost proves dominant, in which case an in-process variant is
   added behind the same contract.
2. **Coverage is a declared capability, not an engine requirement.** Chosen because HTTP runtime targets
   usually cannot provide it. Overridden only if every harness proves able to supply per-call edges, which
   would also drop the reason `coverage` is optional.
3. **`discover` never executes target code unless the plugin declares otherwise.** Chosen because importing a
   user module runs user code inside our process.
4. **Unreachable entry points are reported, not silently dropped.** Chosen so coverage claims stay honest.
   Overridden if the product requires a single "API coverage" number without the breakdown.
5. **Runtime targets send only non-mutating methods unless the scan project names the operation.** Chosen
   because a fuzzer that deletes user data ends the product. The policy lives in the plugin that sends the
   requests: `autofuzz-harness-openapi` refuses a mutating method unless `harness.options.allowMutating`
   names it.
6. **The scan result is one immutable JSON document plus content-addressed artifacts, with
   `finding.identity` stable across scans.** Chosen so triage and pull-request tooling can match findings
   across scans by identity.
7. **The three-outcome classification belongs to the plugin, and the engine receives only `ok`, `rejected`,
   and `failed`.** Chosen because Rust rejects through a return value and C rejects through a return code, so
   no engine-level rule can identify a rejection. Overridden only by pushing per-language knowledge into the
   engine, which breaks the cut this whole document is about.

## Reserved surface: what is not built

This section exists so that the rest of the document can be read literally. Everything here is designed,
and none of it is implemented.

### Fixtures

No fixture format, loader, or validator exists, so `fixture-required` targets stay in the skipped list. A
harness that can build fixtures for its own targets carries that configuration in `harness.options`.

### Oracles and renderers

`differential` and any property-assertion oracle are unimplemented, and `validate` rejects a scan project
that lists one. The only renderers are `json`, which is the scan result itself, and `markdown`. `validate`
currently also accepts the name `sarif`, and nothing renders it; that is a bug against the rule above, not a
promise.

### Capabilities the engine does not read

`stateful`, `minimize`, `sideEffects`, and `concurrent` are in the handshake vocabulary and in the plugin
handshakes, and no engine code branches on them. A plugin author should read the capability table above
before relying on one.

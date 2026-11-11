# 0006 - The js-ts harness runs each invocation in a fresh realm inside one warm worker

Status: accepted. Corrects an earlier draft of this record, which attributed the per-invocation cost to the TypeScript compiler being loaded on the invoke path. That was wrong: `src/invoke.ts` imports only `protocol.js`, `classify.js`, `coverage.js`, `materialize.js` and `guards.js`, `resolveCallable` is defined in `src/invoke.ts` itself, `signatureOf` in `src/classify.ts`, and only `src/server.ts` imports `discover.js`. The compiler is loaded once in the plugin process for discovery, which is correct, and never in the invoke worker.

## Context

Measured on Linux x86-64 with Node 24, `invoke` of the billing test target's `creditSummary` through an already-warm plugin process, single process, 30 sequential calls: median **90 ms**, min 88 ms, p90 92 ms. Through the engine with 8 workers the same call reports a median of 217 ms, so worker contention roughly doubles per-call cost.

| component | measured |
|---|---|
| worker thread creation, no imports | 24.2 ms |
| `invoke.ts`'s whole import graph in that worker | 3.1 ms |
| `--import tsx` loader registration | 34.0 ms (tsx-mode targets only) |
| target `.ts` graph loaded in the worker | ~3 ms |
| coverage session, end to end | 14.4 ms |
| of which `Profiler.startPreciseCoverage` | 3.79 ms |
| of which `Profiler.disable` plus disconnect | 1.33 ms |
| of which V8 compiling the target inside the coverage window | ~8 ms |
| `vm.createContext` | 0.44 ms |
| fresh realm, link and evaluate a two-module graph | 0.57 ms |
| transpile all 12 target files, once | 125.5 ms |
| `child_process.fork`, child replies | 25.9 ms |

The two largest per-call items are worker-thread creation (24 ms) and the coverage session (14 ms). The loader is either 3 ms in strip mode or 37 ms in tsx mode. At 90 ms single-process and roughly 2x that under 8-worker contention, the Step 10 acceptance test — 50,000 invocations, guided against random, run repeatedly — costs hours per pair, which is why the search's own behaviour could not be evaluated.

## Decision

1. **One warm worker thread per plugin process serves many invocations.** The plugin process is already one per engine worker, so thread creation is amortized across the whole run instead of paid per call.

2. **Each invocation runs in a fresh realm inside that worker.** The harness creates a `vm.createContext` and instantiates the target's module graph with `vm.SourceTextModule`, so module-level state is reset for every call. Linking resolves `./x.js` specifiers to the cached transpiled text of `./x.ts` in the linker function; no Node module hook is registered.

3. **The harness transpiles the target itself and caches the text.** Each `.ts` file in the target's import closure is transpiled once per plugin process with the `typescript` compiler, which the plugin process already loads for discovery. 125.5 ms once per target, then free. This removes both loader modes: strip-types and the `tsx` fallback are no longer needed on the invoke path, because the harness controls compilation. The realm is given the real source URL as its module identifier.

4. **Coverage uses one armed inspector window per worker, and an invocation's digest is the set of scripts that invocation first reported.** Resetting coverage around each call was implemented and rejected: a `Profiler.stopPreciseCoverage` plus `Profiler.startPreciseCoverage` cycle costs 8 to 20 ms and has to finish before the realm compiles its modules, because a script compiled while coverage is off is dropped from the report, and that put the call back at 15 ms. Starting without stopping is cheaper but asymmetric - 13 blocks reported for one input and 3 for another across alternating calls - which is broken scoping. Script identity is what makes a single window safe: every invocation instantiates its own `vm.SourceTextModule`, so every invocation compiles its own scripts, and a script is attributed to the invocation that first reports it. Measured over 600 sequential invocations: no empty digest, and the digest stable at 23 edges for the same input.

5. **An unhandled rejection is attributed to the in-flight invocation, or the worker is terminated.** A warm worker means a rejection raised by call N can surface while call N+1 is running. It is reported against the call that caused it, and if no call is in flight the worker exits so the engine sees a lost plugin rather than a clean result.

6. **A worker is retired after `INVOCATIONS_PER_WORKER = 128` invocations.** A fresh realm per invocation retains roughly 0.2 MB of the worker's heap and V8 does not return it: heap grew from 21 MB to 101 MB over 400 invocations, and a bare `vm` loop showed no plateau over 3000 realms (533 MB heap, 678 MB RSS). A worker thread rejects `--expose-gc` and every heap flag passed through `execArgv`, so termination is the only release. Each retirement boundary costs one call of about 68 ms, which is the price of a fresh thread, and resident set size then holds at 55 MB across 600 calls with median latency flat at 3.9 to 4.3 ms.

## Consequences

- Per-invocation isolation is preserved, which is the property that decides this design. A pooled worker that kept the target's **module graph** alive would not: call N would see state left by call N-1, a finding would depend on the calls before it, and `Finding.input` — one call — would stop reproducing. A fresh realm resets module state while the thread stays warm, which is the half of the zygote pattern that is available in Node.
- Coverage scoping has a trap that must be respected. V8's `detailed: true` block report describes each function's **most recent execution**, not a running total: after `f(1)` the report lists the `else` block as unexecuted, after `f(-1)` it lists the `if` block, and a before-and-after count diff of a single call reported no change at all. So a digest cannot be built by diffing counts across a long-lived session.
- A script compiled while coverage is off is still reported by a **new** inspector session, but a `stopPreciseCoverage` plus `startPreciseCoverage` cycle on the **same** session dropped an already-compiled module from the report entirely. Both facts are measured, and between them they rule out both shortcuts that look cheap: a pooled worker that cached the module graph would silently report empty digests after the first call, which reads as a corpus plateau rather than a broken measurement, and a per-call reset cycle costs more than the call it is measuring.
- Fault frames name realm code by the identifier given to `vm.SourceTextModule`. A realm module created with identifier `file:///…/src/reports.ts` produced `at Module.boom (file:///…/src/reports.ts:1:45)`, so `src/classify.ts` matches guard sites unchanged provided the harness sets the identifier to the real source URL. That identifier is a stability contract, not a label.
- Removing the loader also corrected the coverage map. The `tsx` path reported byte ranges into its own emitted text - `validation.ts` blocks began at offset 114, inside the file's header comment - so an edge id was not a source position. With the harness transpiling and applying the source map, ranges index the `.ts` file exactly, and the same 30 calls of one target now report 38 blocks rather than 35: the three extra are real guard branches that the loader's emit had merged. A count that was previously reported is therefore expected to move, and upward.
- `--experimental-vm-modules` becomes a required flag on the plugin's own launch command.
- Measured cost after this change is **4.15 ms** median per `invoke` through a warm plugin process, against 92.42 ms before, with the per-invocation digest unchanged at 23 edges for the same input. The "roughly 11 ms" estimate in an earlier draft of this record was wrong in both directions: it assumed the per-call coverage reset would survive, and it did not know that the invoke worker imported the transpile helper and therefore loaded the TypeScript compiler per thread, which cost 5.4 ms on top of being deoptimized by the profiler on every arming. `src/sourcemap.ts` exists so the worker never imports the compiler.

## Alternatives rejected

- **A pool of reused worker threads that cache the target module graph.** Cheapest to implement. Rejected twice over: module state persists across calls, which breaks the single-call reproducer, and the coverage trap above turns every digest after the first into an empty set.
- **`fork` per invocation, the AFL forkserver or Android zygote shape.** Measured 25.9 ms against 24.2 ms for a worker thread, and Node's `child_process.fork` execs a new binary, so the child re-imports the target anyway. Node gives no supported way to fork a process that has already initialised V8 and libuv, so the shared-initial-state half of the pattern is unavailable; a fresh realm gives the same property inside one isolate at 0.57 ms.
- **JavaScriptCore.** The measured cost is engine-side setup and coverage instrumentation, not interpretation of the target's code. The guide's only signal is V8's `Profiler.takePreciseCoverage` block report, and `src/classify.ts` decides guard site versus engine error from V8 parse positions. A second engine would have to replace both.
- **Ahead-of-time compilation through V8's module compile cache.** Measured no gain: a fresh worker importing a target `.ts` file cost 0.6 ms cold and 0.6 ms warm with `NODE_COMPILE_CACHE` set. Compilation was never the cost.
- **Resetting precise coverage around each call.** 8 to 20 ms for the cycle, and it must complete before the realm compiles, which put the call back at 15 ms. Superseded by decision 4's script-identity scoping, which is free.
- **Replacing `--import tsx` with `--experimental-strip-types` plus a resolve hook, or with `--experimental-transform-types`.** Measured 52.9 ms against 60.2 ms, and 57.0 ms against 59.4 ms on the enum target. A few milliseconds on one loader mode, and it does not touch the 24 ms of thread creation. Superseded by decision 3, which removes loader modes from the invoke path entirely.

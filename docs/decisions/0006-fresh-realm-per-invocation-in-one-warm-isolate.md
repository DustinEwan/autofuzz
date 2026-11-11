# 0006 - The js-ts harness runs each invocation in a fresh realm inside one warm worker

Status: accepted. Corrects an earlier draft of this record, which attributed the per-invocation cost to the TypeScript compiler being loaded on the invoke path. That was wrong: `src/invoke.ts` imports only `protocol.js`, `classify.js`, `coverage.js`, `materialize.js` and `guards.js`, `resolveCallable` is defined in `src/invoke.ts` itself, `signatureOf` in `src/classify.ts`, and only `src/server.ts` imports `discover.js`. The compiler is loaded once in the plugin process for discovery, which is correct, and never in the invoke worker.

## Context

Measured on Linux x86-64 with Node 24, `invoke` of the billing fixture's `creditSummary` through an already-warm plugin process, single process, 30 sequential calls: median **90 ms**, min 88 ms, p90 92 ms. Through the engine with 8 workers the same call reports a median of 217 ms, so worker contention roughly doubles per-call cost.

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
| transpile all 12 fixture files, once | 125.5 ms |
| `child_process.fork`, child replies | 25.9 ms |

The two largest per-call items are worker-thread creation (24 ms) and the coverage session (14 ms). The loader is either 3 ms in strip mode or 37 ms in tsx mode. At 90 ms single-process and roughly 2x that under 8-worker contention, the Step 10 acceptance test — 50,000 invocations, guided against random, run repeatedly — costs hours per pair, which is why the search's own behaviour could not be evaluated.

## Decision

1. **One warm worker thread per plugin process serves many invocations.** The plugin process is already one per engine worker, so thread creation is amortized across the whole run instead of paid per call.

2. **Each invocation runs in a fresh realm inside that worker.** The harness creates a `vm.createContext` and instantiates the target's module graph with `vm.SourceTextModule`, so module-level state is reset for every call. Linking resolves `./x.js` specifiers to the cached transpiled text of `./x.ts` in the linker function; no Node module hook is registered.

3. **The harness transpiles the target itself and caches the text.** Each `.ts` file in the target's import closure is transpiled once per plugin process with the `typescript` compiler, which the plugin process already loads for discovery. 125.5 ms once per fixture, then free. This removes both loader modes: strip-types and the `tsx` fallback are no longer needed on the invoke path, because the harness controls compilation. The realm is given the real source URL as its module identifier.

4. **Coverage uses one inspector session per worker, reset around each call.** `Profiler.stopPreciseCoverage` followed by `Profiler.startPreciseCoverage` before the call, `Profiler.takePreciseCoverage` after it. This is safe here and only here because every invocation compiles fresh module text while the session is live.

5. **An unhandled rejection is attributed to the in-flight invocation, or the worker is terminated.** A warm worker means a rejection raised by call N can surface while call N+1 is running. It is reported against the call that caused it, and if no call is in flight the worker exits so the engine sees a lost plugin rather than a clean result.

## Consequences

- Per-invocation isolation is preserved, which is the property that decides this design. A pooled worker that kept the target's **module graph** alive would not: call N would see state left by call N-1, a finding would depend on the calls before it, and `Finding.input` — one call — would stop reproducing. A fresh realm resets module state while the thread stays warm, which is the half of the zygote pattern that is available in Node.
- Coverage scoping has a trap that must be respected. V8's `detailed: true` block report describes each function's **most recent execution**, not a running total: after `f(1)` the report lists the `else` block as unexecuted, after `f(-1)` it lists the `if` block, and a before-and-after count diff of a single call reported no change at all. So a digest cannot be built by diffing counts across a long-lived session.
- A script compiled while coverage is off is still reported by a **new** inspector session, but a `stopPreciseCoverage` plus `startPreciseCoverage` cycle on the **same** session dropped an already-compiled module from the report entirely. Both facts are measured. Decision 4 is therefore correct only because realms compile inside the live window; a pooled worker that cached the module graph would silently report empty digests after the first call, which looks like a corpus plateau rather than a broken measurement.
- Fault frames name realm code by the identifier given to `vm.SourceTextModule`. A realm module created with identifier `file:///…/src/reports.ts` produced `at Module.boom (file:///…/src/reports.ts:1:45)`, so `src/classify.ts` matches guard sites unchanged provided the harness sets the identifier to the real source URL. That identifier is a stability contract, not a label.
- `--experimental-vm-modules` becomes a required flag on the plugin's own launch command.
- Expected cost after this change is roughly 11 ms per call: about 5.5 ms of coverage session mechanics, under 1 ms to build a realm, and a few ms to materialize arguments and reply. That is about 8x better than 90 ms, and the same factor applies under contention. It is not the "about 1 ms" an earlier draft of this record promised, because the coverage session and the RPC round trip remain.

## Alternatives rejected

- **A pool of reused worker threads that cache the target module graph.** Cheapest to implement. Rejected twice over: module state persists across calls, which breaks the single-call reproducer, and the coverage trap above turns every digest after the first into an empty set.
- **`fork` per invocation, the AFL forkserver or Android zygote shape.** Measured 25.9 ms against 24.2 ms for a worker thread, and Node's `child_process.fork` execs a new binary, so the child re-imports the target anyway. Node gives no supported way to fork a process that has already initialised V8 and libuv, so the shared-initial-state half of the pattern is unavailable; a fresh realm gives the same property inside one isolate at 0.57 ms.
- **JavaScriptCore.** The measured cost is engine-side setup and coverage instrumentation, not interpretation of the target's code. The guide's only signal is V8's `Profiler.takePreciseCoverage` block report, and `src/classify.ts` decides guard site versus engine error from V8 parse positions. A second engine would have to replace both.
- **Ahead-of-time compilation through V8's module compile cache.** Measured no gain: a fresh worker importing a fixture `.ts` file cost 0.6 ms cold and 0.6 ms warm with `NODE_COMPILE_CACHE` set. Compilation was never the cost.
- **Replacing `--import tsx` with `--experimental-strip-types` plus a resolve hook, or with `--experimental-transform-types`.** Measured 52.9 ms against 60.2 ms, and 57.0 ms against 59.4 ms on the enum fixture. A few milliseconds on one loader mode, and it does not touch the 24 ms of thread creation. Superseded by decision 3, which removes loader modes from the invoke path entirely.

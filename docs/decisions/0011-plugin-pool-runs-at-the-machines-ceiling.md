# 0011 - The plugin pool runs at the machine's ceiling, not the engine's

Status: accepted.

## Context

A 30,000-invocation guided scan of the billing test target with the default 12 workers showed every logical CPU at 52 to 67% and a load average of 5.86, which reads as poor utilisation: twelve plugin processes alive, half a core busy each. The original throughput work (Decision 0006) had seen all twelve CPUs saturated, so the difference looked like a regression in how the engine feeds its pool. The engine's batch loop is plan, run, account, and only the run phase keeps the plugins busy, so the suspicion fell on the two serial phases and on worker-count choice.

Measured on Linux x86-64 (WSL2), AMD Ryzen 5 5500 — 6 physical cores, 2 threads per core, 12 logical — Node 24.22.2, billing test target, strategy `guided`, seed `0x5e3a91c4`, 30,000 invocations.

Phase cost of the batch loop, from counters written to `engine.log`:

| workers | batches | loop wall | plan | run | account | `failed` answers |
|---|---|---|---|---|---|---|
| 12 | 313 | 47.73 s | 3.64 s | 31.24 s | 3.99 s | 4 |
| 6 | 625 | 47.37 s | 3.46 s | 36.62 s | 1.72 s | 0 |

The two serial phases together are 5.4 s of a 47.7 s loop, 11%. `account` is larger at 12 workers because that run found the billing defect and the minimizer's plugin round trips are inside it; the 6-worker run's 1.72 s is the steady-state cost. The `failed` answers each retire the plugin's invoke worker thread at about 68 ms (Decision 0006), so four of them cost 270 ms across the whole scan.

The same target measured in the plugin alone, one warm process, sequential calls, no engine: median 4.07 ms per `invoke` with the coverage digest, 1.60 ms without it. Scaling that path across concurrent plugin processes, still with no engine:

| plugin processes | median per call | throughput | cores consumed |
|---|---|---|---|
| 1 | 3.82 ms | 130 calls/s | 1.33 |
| 2 | 4.18 ms | 243 calls/s | 2.6 |
| 4 | 4.62 ms | 414 calls/s | 4.0 |
| 6 | 5.83 ms | 508 calls/s | 5.0 |
| 8 | 6.50 ms | 590 calls/s | 6.2 |
| 12 | 7.86 ms | 632 calls/s | 8.1 |

One plugin process already consumes 1.33 cores, because it is a main thread, an invoke worker thread, and V8's GC threads. Twelve of them consume 8.1 cores' worth of CPU time and deliver 4.9× the throughput of one. The real scan ran 30,000 invocations in 31.24 s of run phase, 960 calls/s, which is above what this machine delivers for `creditSummary` alone: the scan's mix of billing's 37 entry points is cheaper per call than that one target, and the pool is fed continuously inside the run phase.

## Decision

1. **The worker default stays one plugin process per logical CPU.** Defaulting to physical cores was implemented and measured: 6 workers finished the same 30,000 invocations in 47.37 s against 47.73 s at 12, so the change buys no wall time. It was reverted rather than kept for its halved memory footprint, because it also changed the search trajectory (see consequence 3) and the design states one process per CPU.

2. **The batch loop reports its own phase costs.** Every scan ends with `N batches over T ms: plan P ms, run R ms, account A ms` and the count of invocations answered `failed`, in `engine.log`. A scan that is slow has to say which phase is slow; before this, the only observable was total wall time, which invited three separate wrong guesses about where the time went.

3. **Throughput work stops at the machine.** The remaining per-call cost is the coverage digest, 2.5 ms of a 3.8 ms call and the component that inflates most under concurrency. Reducing it changes what the feedback signal is, which is a harness design question with its own tradeoffs, not a scheduling fix in the engine.

## Consequences

- The utilisation picture is correct, not broken. Six physical cores fully used, drawn across twelve SMT siblings, is 58% per logical CPU and a load average near 6. The original tuning reached 100% on every logical CPU because the work then was dominated by V8 compiling TypeScript on the invoke path, which is compute-dense enough to use both siblings of a core; a fresh realm plus an inspector window is more memory-latency bound and does not.
- Scan results depend on the worker count, because a job reaches a different plugin process depending on how many processes pull from the batch queue. The 6-worker run of the same seed and budget finished without finding the billing defect that the 12-worker run found in the same 30,000 invocations. Acceptance runs that must be reproducible pin `--workers`; the seed fixes the search, not the machine's core count.
- The knee is at four to six processes. `--workers 6` costs 14% of throughput for half the resident memory, which is the right trade on a machine that is doing something else while a scan runs. The default stays the CPU count because a scan left alone should use the machine.
- `INVOCATIONS_PER_WORKER = 128` retirement is not a throughput problem on this workload: four thread restarts per 30,000-invocation scan. It would matter for a target that rejects or fails often, and the `failed` counter in `engine.log` is how that shows up before anyone has to guess.

## Alternatives rejected

- **Defaulting to physical cores.** Measured, no gain, and it moved the trajectory. Rejected above.
- **Planning batch n+1 while batch n executes.** The corpus learns from batch n before n+1 is planned; that ordering is what makes the guided trajectory deterministic, and the two serial phases it would hide are 11% of wall time.
- **Scoring invocations on the worker threads instead of the engine thread.** The corpus update order would follow thread arrival order, which makes a scan's trajectory depend on scheduling.
- **Raising `INVOCATIONS_PER_WORKER`.** It exists to bound heap growth, not to save latency, and the measured restart cost is 270 ms per scan.

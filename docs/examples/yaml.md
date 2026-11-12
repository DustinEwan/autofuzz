# yaml: an example target

`examples/yaml` is the upstream `yaml` package, version 2.9.1 (ISC,
https://github.com/eemeli/yaml), taken from the npm tarball. Only `src/`, the package manifest and
the licence are kept. It is here because it is the input that made three engine defects visible, and
because its findings still exercise the minimizer. It is not a dogfood subject: no verdict here
claims to be a statement about `yaml`'s quality.

Run it from the repository root:

```
cargo run -p afz-cli -- scan examples/yaml --seed 0x5e3a91c4
```

The scan project is `examples/yaml/autofuzz.json`: 20 000 invocations or 600 s, guided, all 12
workers. The scan writes `examples/yaml/.autofuzz/<scanId>/`, which is not committed.

## What the scan reports

Recorded for the run above on 2026-11-12, seed `0x5e3a91c4`, 676 s wall.

| | |
| --- | --- |
| Discovered | 250 symbols |
| Fuzzable | 49 |
| Skipped | 201 |
| Materialization | 49 constructible, 37 fixture-required, 142 opaque |
| Invocations | 11 797 of a 20 000 budget, `stoppedReason: budget` |
| Corpus | 57 entries |
| Coverage | 156 of 262 blocks, ratio 0.5954; samples 149 edges at 5 000 invocations, 155 at 10 003 |
| Restarts | 119 plugin processes |
| Findings | 3 |

Coverage ratio grows between samples, so the guided loop is still finding blocks at the end of the
budget. Only 49 of 250 discovered symbols are fuzzable: 142 are `opaque`, which for a parser package
means the symbol takes a `Node`, `Document` or `Schema` the harness cannot build. That is the honest
reach of the tool on this package, not a scan failure.

### Findings and verdicts

| Finding | Oracle | Input | Verdict |
| --- | --- | --- | --- |
| `afz:f:fb2e26ea` | `processExit`, `via: processExit`, "plugin exited with code 1" | 93 to 45 bytes | Harness defect. The plugin process dies on about 1% of invocations against this package. Nothing in the design says a target input may end the plugin. |
| `afz:f:88173023` | `processExit`, `via: plugin-lost`, "plugin closed the pipe" | 93 to 45 bytes | The same death as the row above. It is a second finding because identity hashes the fault class, and a killed process reports `PluginLost` while an exiting one reports `ProcessExit`. One defect, two identities. |
| `afz:f:5a8cfae3` | `deadline`, 2 000 ms | 401 to 180 bytes | Undiagnosed. The minimized 180-byte input reproduces, so some input costs over two seconds. Candidates are a superlinear parse in the target, which would be a real bug, or harness cost around the call. Not resolved here. |

## Why this target is worth keeping

Three defects were found by scanning it, and none of them were visible on the fixtures.

**`prepare` was held to the invocation deadline.** `supervise.rs` passed the 2 000 ms invocation
deadline to `prepare`, while the shrink pool used the 120 s transport bound. Preparing a yaml target
transpiles and evaluates its module graph, measured at 517 ms for the first target on a fresh process
and 92 ms for a second target on a primed one, so the first call into a target with a deeper graph
crossed the deadline, the engine killed the plugin, and every kill became a `processExit` finding. A
600 s run spent itself on 54 invocations and reported 19 findings that were all the same kill. Fixed
in `2f6f6d5`: the same budget now yields 11 797 invocations and 3 findings.

**The minimizer accepted a candidate identical to its base.** Dropping an argument whose slot was
already `undefined` produces the input it already is, which reproduces every fault, so the round
restarted and offered the same candidate again. Three findings spent 2 000 attempts and about 185 s
each shedding nothing. Fixed in `2f6f6d5`; the same three findings now shrink in 0.5 s, 0.3 s and
0.0 s.

**A shrink round covered one node at a time.** The search shrank one node, then the next, so a round
offered about three candidates while twelve shrink connections existed. A round now offers every
candidate that does not depend on another trial's answer. Fixed in `6ccdb16`.

## Utilization, and what still costs wall time

Sampled every two seconds over 497 s of a guided run: mean plugin CPU 2.41 of 12 cores (20%), mean
3.8 busy plugins out of 17 to 24 processes, engine 0.10 cores. The engine's own phase counters say
run 597 s, setup 75 s, plan 0.9 s, account 1.7 s, coverage map 0.05 s, so planning, corpus
accounting and coverage-map collection are 0.4% of the run.

The remaining wall time is not minimization and not coverage:

- One invocation of a real yaml target costs 74.6 ms with the coverage probe off and 85.1 ms with it
  on, so `Profiler.takePreciseCoverage` is 10.5 ms, 12% of the call. Measured against one plugin
  process and one target, `resolveEnd`.
- The engine spends 605 ms of wall per invocation, and a batch takes 4.56 s while each worker holds
  only about 7.5 jobs of 85 ms, which is 0.63 s of work. The other 3.9 s per batch is fast workers
  waiting at the batch boundary.
- About one plugin process died per batch (119 restarts over 131 batches, and only 2 deadline
  exceeds). A dead worker costs a Node boot, a 517 ms re-prime, and its remaining queued jobs, so
  each death converts a 2-second fault into roughly four seconds of lost parallelism.

The open work this target points at, in the order the numbers rank it: stop the plugin process from
dying on target input, replace the batch barrier with a continuous queue so a straggler costs one
worker rather than eleven, and give a killed process and an exited one the same finding identity.

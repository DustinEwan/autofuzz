# yaml: an example target

`examples/yaml` is the upstream `yaml` package, version 2.9.1 (ISC,
https://github.com/eemeli/yaml), taken from the npm tarball. Only `src/`, the package manifest and
the licence are kept. It is here because it is the input that made four engine defects visible, and
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

That table is the run as it happened. A later rerun of a smaller budget - 4 000 invocations, the same
seed, after the plugin-death fix described below - answered 3 486 invocations in 92 s with `restarts: 0`,
48 corpus entries, 144 covered blocks of 248 (ratio 0.5806), and no findings: both `processExit` rows
above went away with the death they came from.

### Findings and verdicts

| Finding | Oracle | Input | Verdict |
| --- | --- | --- | --- |
| `afz:f:fb2e26ea` | `processExit`, `via: processExit`, "plugin exited with code 1" | 93 to 45 bytes | Harness defect, since fixed. The plugin died on about 1% of invocations because a write to its own stdout ended the process with exit code 1; see the fourth defect below. Nothing in `yaml` was wrong, and the 45-byte input is not a reproducer: `debug(undefined)` invoked on its own answers `ok`. The shrink accepted it because a lost connection matches a `processExit` fault whatever that candidate was doing. |
| `afz:f:88173023` | `processExit`, `via: plugin-lost`, "plugin closed the pipe" | 93 to 45 bytes | The same death as the row above, reported twice because identity hashes the fault class and a killed process answers `PluginLost` where an exiting one answers `ProcessExit`. One defect, two identities. Gone with the death, which is the argument for collapsing the two classes. |
| `afz:f:5a8cfae3` | `deadline`, 2 000 ms | 401 to 180 bytes | Undiagnosed. The minimized 180-byte input reproduces, so some input costs over two seconds. Candidates are a superlinear parse in the target, which would be a real bug, or harness cost around the call. It did not reappear on the 4 000-invocation rerun of this seed after the fix. |

## Why this target is worth keeping

Four defects were found by scanning it, and none of them were visible on the fixtures.

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

**A target's own `console.log` could end the plugin.** The realm gives target code the worker's globals,
`console` and `process` among them, and the plugin's stdout is its JSON-RPC channel. `yaml` exports a
`debug` whose body is `console.log(...messages)`, so calling it writes into the framing. A write to that
stream that fails is an `'error'` event on it, Node ends the process over an unhandled one, and the exit
code is 1 - the exact string the engine carried as the finding's fault. The realm now gets a console bound
to stderr, so target logging reaches `engine.log` as a diagnostic instead of arriving inside the framing,
and the server listens for stream errors rather than dying on them. Two tests in
`test/protocol-channel.test.mjs` fail against the previous build: the log line does not reach stderr, and
a plugin whose stdout reader has gone exits with code 1.

Which write errored inside the scan is not established. What is established is that a failed stdout write
produces that exit code, that the scan's log said only `plugin exited with code 1` until the engine began
writing plugin stderr into it, and that the deaths stopped with these changes: `restarts: 0` over 3 486
invocations where the same budget previously died 15 times.

## Utilization, and what still costs wall time

Sampled every two seconds over 497 s of a guided run: mean plugin CPU 2.41 of 12 cores (20%), mean
3.8 busy plugins out of 17 to 24 processes, engine 0.10 cores. The engine's own phase counters say
run 597 s, setup 75 s, plan 0.9 s, account 1.7 s, coverage map 0.05 s, so planning, corpus
accounting and coverage-map collection are 0.4% of the run.

Setup is a fixed cost rather than a rate. A rerun of the same seed and target set at a 4 000-invocation
budget finished in 173 s and reported `setup 74568 ms`: spawn, handshake, discovery, and the first
`prepare` of each target on each worker, 43% of that run's wall clock, against the 75 s the 497 s run
above spent on the same work. A short budget is therefore mostly setup, and comparing two scans of
different lengths without subtracting it compares the constant rather than the loop.

The remaining wall time is not minimization and not coverage:

- One invocation of a real yaml target costs 74.6 ms with the coverage probe off and 85.1 ms with it
  on, so `Profiler.takePreciseCoverage` is 10.5 ms, 12% of the call. Measured against one plugin
  process and one target, `resolveEnd`.
- The engine spends 605 ms of wall per invocation, and a batch takes 4.56 s while each worker holds
  only about 7.5 jobs of 85 ms, which is 0.63 s of work. The other 3.9 s per batch is fast workers
  waiting at the batch boundary.
- About one plugin process died per batch in that run (119 restarts over 131 batches, and only 2
  deadline exceeds). A dead worker costs a Node boot, a 517 ms re-prime, and its remaining queued jobs,
  so each death converts a 2-second fault into roughly four seconds of lost parallelism. That death is
  fixed, as the fourth defect below describes: the same 4 000-invocation budget that took 173 s, died 15
  times and filed a finding for the death itself now answers 3 486 invocations in 92 s with
  `restarts: 0` and no findings at all.

The open work this target points at, in the order the numbers rank it: replace the batch barrier with a
continuous queue, so a straggler costs one worker rather than eleven; make the realm link a closure that
reaches a sibling module through a `.ts` specifier, which is what leaves 21 of this package's targets
reported unbuildable; give a killed process and an exited one the same finding identity; and stop
reporting a module-load failure under the reason `discovery-error`, which says the wrong thing about it.

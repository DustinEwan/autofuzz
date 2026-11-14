# yaml: an example target

`examples/yaml` is the upstream `yaml` package, version 2.9.1 (ISC,
https://github.com/eemeli/yaml), taken from the npm tarball. Only `src/`, the package manifest, and
the licence are kept. The package is here because its findings need minimization over a long
document, and because it exercises the engine on a real parser.

Run it from the repository root:

```sh
cargo run -p afz-cli -- scan examples/yaml --seed 0x5e3a91c4
```

The scan project is `examples/yaml/autofuzz.json`: 20000 invocations or 600 seconds, guided, all
workers. The scan writes `examples/yaml/.autofuzz/<scanId>/`, which is not committed.

## What the scan saw

Recorded for the run above on 2026-11-12, seed `0x5e3a91c4`, 676 seconds wall.

| | |
| --- | --- |
| Discovered | 250 symbols |
| Fuzzable | 49 |
| Skipped | 201 |
| Materialization | 49 constructible, 37 fixture-required, 142 opaque |
| Invocations | 11797 of a 20000 budget, `stoppedReason: budget` |
| Corpus | 57 entries |
| Coverage | 156 of 262 blocks, ratio 0.5954 |
| Restarts | 119 plugin processes |
| Findings | 3 |

Coverage grew between samples, so the guided loop was still finding blocks at the end of the budget.
Only 49 of 250 discovered symbols are fuzzable. The other 201 are mostly `opaque`, which for a
parser package means that the symbol takes a `Node`, `Document`, or `Schema` that the harness cannot
build. That is the reach of the tool on this package, not a scan failure.

The 119 restarts came from a plugin defect that is now fixed. The same seed and target set at a
4000-invocation budget answered 3486 invocations in 92 seconds with no restarts, 48 corpus entries,
144 covered blocks of 248 (ratio 0.5806), and no findings.

## The findings

| Finding | Oracle | Input | Verdict |
| --- | --- | --- | --- |
| `afz:f:fb2e26ea` | `processExit`, `via: processExit`, "plugin exited with code 1" | 93 to 45 bytes | Harness defect, since fixed. The plugin died on about 1% of invocations because a write to its own stdout ended the process with exit code 1. Nothing in `yaml` was wrong, and the 45-byte input is not a reproducer: `debug(undefined)` invoked on its own answers `ok`. |
| `afz:f:88173023` | `processExit`, `via: plugin-lost`, "plugin closed the pipe" | 93 to 45 bytes | The same death as the row above, reported twice because identity hashes the fault class. One defect produced two identities. |
| `afz:f:5a8cfae3` | `deadline`, 2000 ms | 401 to 180 bytes | Undiagnosed. The minimized 180-byte input reproduces, so some input costs over two seconds. The candidates are a superlinear parse in the target, which would be a real bug, or harness cost around the call. It did not reappear on the 4000-invocation rerun after the fix. |

The first two rows are not `yaml` bugs. They are the reason that a finding names its `via` channel
and its reproducer: a plugin death and a target crash can otherwise look the same.

## What this record shows

The scan reached 49 of 250 symbols, and the rest are reported as skipped with a reason. That
inventory is the honest measure of the tool's reach on this package.

The restarts cost wall time, because each dead worker had to boot Node and re-prime its module
graph. The rerun with the fix shows the difference: 3486 invocations in 92 seconds with no restarts.

The target remains useful as a regression case: its findings need minimization over a long document,
and its inventory shows how far one TypeScript harness can reach into a parser package.

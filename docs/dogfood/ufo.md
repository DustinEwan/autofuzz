# ufo: dogfood record

`afz scan` run against the vendored `ufo` package (`vendor/ufo`, version and upstream recorded in
`vendor/README.md`) on 2026-11-12, seed `0x5e3a91c4`, budget overridden to 20 000 invocations or
300 s:

```
cargo run -p afz-cli -- scan vendor/ufo --seed 0x5e3a91c4 --budget invocations=20000,wall=300s
```

The scan finished in 51 s with `stoppedReason: budget`. The document is
`vendor/ufo/.autofuzz/<scanId>/scan.json`, which is not committed.

## Inventory

| | |
| --- | --- |
| Discovered | 109 symbols |
| Fuzzable | 105 |
| Skipped | 4 |
| Materialization | 105 constructible, 2 fixture-required, 0 opaque |
| Invocations | 16 739 of 20 000, 327 per second |
| Corpus | 20 entries |
| Coverage | 42 of 64 blocks, ratio 0.6562 |
| Restarts | 0 plugin processes, 0 deadline exceeds |
| Findings | 1 |

Every skipped symbol is named in the document. `$URL.append` is skipped twice with
`unconstructible-parameter`: it takes a `$URL` receiver the harness cannot build. `hasProtocol` is
skipped twice with `unsupported-signature`.

Those four rows are two symbols listed twice, and the materialization counts sum to 107 against 105
fuzzable, which is the same duplication. Discovery emits one row per declaration site rather than one
per target id, so a symbol exported from more than one module is enumerated twice and planned twice.
It costs a little throughput and nothing in correctness, because a finding's identity is the target
id plus the fault, not the row.

## Finding

| | |
| --- | --- |
| Finding | `afz:f:4640773b` |
| Target | `afz:t:98d60b875421`, `toASCII (o: composite)` in `src/punycode.ts` |
| Oracle | `failed`, severity `high`, `via: engine-error-outside-declared-channel` |
| Fault | `TypeError: Cannot read properties of null (reading 'split')` at `src/punycode.ts:28` |
| Input | `[null]`, 40 bytes |
| Reproducer | `repros/afz-f-4640773b.ts`, `toASCII(undefined)`, reproduces |

**Verdict: a real crash, but not a `ufo` bug worth reporting upstream.** `src/punycode.ts` is a
minified copy of `punycode@2.1.1` with `@ts-nocheck` at the top, so its parameter has no declared
type and discovery lowers it to a composite that admits `null`. The symbol is not re-exported from
`src/index.ts`; the only caller is `src/encoding.ts:175`, which passes it a hostname it has already
resolved to a string. Nothing in the package's public API can reach line 28 with `null`.

What the finding does say about the tool: discovery treats every module-level export as a fuzzable
target, so internal helpers are counted alongside the package's entry points. On this package 105
symbols were fuzzable, and the one crash came from a helper the package does not export. A
reachability filter - fuzz what the manifest exports, and report the rest as internals - is the
change this record argues for.

## What the run says about the engine

Throughput is not the problem here. 192 batches over 51 s: run 39.9 s, setup 9.5 s, plan 0.86 s,
account 0.55 s, coverage map 38 ms. Ten invocations answered `failed` and retired their plugin's
invoke worker thread, and no plugin process restarted, which is the contrast with
[`examples/yaml`](../examples/yaml.md), where a plugin process died about once a batch. The
unhandled-rejection path that kills the process there is not reachable from `ufo`'s synchronous API.

Coverage plateaued before the first sample: 42 of 64 blocks at 5 000, 10 000, 15 000 and 20 000
planned invocations. Guidance earns nothing on this package, and the honest reading is that the
reachable set is small and every branch that generated values can reach is reached within the first
thousand invocations.

Two engine defects came out of this run, and both are fixed in the change that adds this record:

- The minimizer accepted a candidate larger than its base. The finding's input is `[null]` at 40
  bytes; dropping the argument yields `[undefined]`, which serializes to 45 bytes, reproduced the same
  fault at the same line, and was accepted. The scan then reported no `minimizedInput` because the bytes
  had not shrunk, while the reproducer was written from the accepted 45-byte input. A round now discards
  any candidate whose wire form is larger than the base, and a test pins that a shrink cannot return
  more bytes than it was given.
- Progress samples were keyed on the invocation id the planner assigns, while `execution.invocations`
  counts invocations that ran, so this document carries a sample at 20 000 against 16 739 run. A sample
  is now taken every 5 000 invocations that ran and reports that count.

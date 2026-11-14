# ufo: dogfood record

We scanned the vendored `ufo` package (`vendor/ufo`, version 1.6.4) on 2026-11-12 with seed
`0x5e3a91c4` and a budget of 20000 invocations or 300 seconds:

```sh
cargo run -p afz-cli -- scan vendor/ufo --seed 0x5e3a91c4 --budget invocations=20000,wall=300s
```

The scan finished in 51 seconds with `stoppedReason: budget`. The result is
`vendor/ufo/.autofuzz/<scanId>/scan.json`, which is not committed.

## What the scan saw

| | |
| --- | --- |
| Discovered | 109 symbols |
| Fuzzable | 105 |
| Skipped | 4 |
| Materialization | 105 constructible, 2 fixture-required, 0 opaque |
| Invocations | 16739 of 20000 |
| Corpus | 20 entries |
| Coverage | 42 of 64 blocks, ratio 0.6562 |
| Restarts | 0 |
| Findings | 1 |

Every skipped symbol appears in the document. `$URL.append` is skipped twice with
`unconstructible-parameter`, because it takes a `$URL` receiver that the harness cannot build.
`hasProtocol` is skipped twice with `unsupported-signature`.

Most of the 105 targets are the same function reached from two modules. `decode` is exported by
`src/encoding.ts` and re-exported by `src/index.ts`. Discovery gives each export site its own target
id, so 44 names appear twice in the inventory and are fuzzed twice. That costs throughput and
nothing in correctness, because a finding's identity is a target id plus the fault.

The materialization counts include skipped symbols that report a materialization, so they need not
sum to the fuzzable count. `report.md` prints them under a separate heading for that reason.

## The finding

| | |
| --- | --- |
| Finding | `afz:f:4640773b` |
| Target | `afz:t:98d60b875421`, `toASCII (o: composite)` in `src/punycode.ts` |
| Oracle | `failed`, severity `high`, `via: engine-error-outside-declared-channel` |
| Fault | `TypeError: Cannot read properties of null (reading 'split')` at `src/punycode.ts:28` |
| Input | `[null]`, 40 bytes |
| Reproducer | `repros/afz-f-4640773b.ts`, `toASCII(undefined)`, reproduces |

The crash is real, but it is not a `ufo` bug worth reporting upstream. `src/punycode.ts` is a
minified copy of `punycode@2.1.1` with `@ts-nocheck` at the top, so its parameter has no declared
type and discovery lowers it to a composite that admits `null`. The symbol is not re-exported from
`src/index.ts`. The only caller is `src/encoding.ts:175`, which passes it a hostname that it has
already resolved to a string. Nothing in the package's public API can reach line 28 with `null`.

## What this record shows

The finding says more about the tool than about the package. Discovery treats every module-level
export as a fuzzable target, so internal helpers are counted alongside the package's entry points. On
this package, 105 symbols were fuzzable, and the one crash came from a helper that the package does
not export. A reachability filter, which fuzzes what the manifest exports and reports the rest as
internals, would remove that noise. The engine does not have one today.

Coverage plateaued before the first sample: 42 of 64 blocks at 5000, 10000, 15000, and 20000 planned
invocations. Guidance earned nothing on this package. The reachable set is small, and every branch
that generated values can reach is reached within the first thousand invocations.

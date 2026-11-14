# Example targets

The packages here are copies of upstream projects. They give a scan a real API to run against, so
you can see the engine work end to end without preparing your own target.

`vendor/` serves a different purpose. A package under `vendor/` is scanned to produce a dogfood
record in `docs/dogfood/`, with a verdict on each finding. A package under `examples/` is kept
because it exercises the engine in a way that the small fixtures in the harness repositories do not.

| Package | Version | Upstream | Licence | Why it is here |
| --- | --- | --- | --- | --- |
| `yaml` | 2.9.1 | https://github.com/eemeli/yaml (npm tarball for 2.9.1) | ISC | A finding in a long document needs minimization over that document, which a shrink that only halves a string from the front cannot solve. This package is the regression case for `crates/afz-engine/src/minimize.rs`. |

Only what the scan reads is kept: `src/`, the package manifest, and the upstream licence.
`autofuzz.json` in each directory is the scan project, and the scan is recorded in
[`docs/examples/`](../docs/examples/).

Run a scan from the repository root:

```sh
cargo run -p afz-cli -- scan examples/yaml --seed 0x5e3a91c4
```

The run writes `examples/yaml/.autofuzz/<scanId>/scan.json`, which is not committed. Nothing here is
imported by the engine or by a plugin.

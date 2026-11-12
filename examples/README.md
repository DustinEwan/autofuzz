# Example targets

Sources here are copied from upstream packages so a scan has a real API to run against end to
end. They differ from `vendor/` in what they are for: a package under `vendor/` is scanned to
produce a dogfood record in `docs/dogfood/` with a human verdict per finding, while a package
here is kept because it exercises the engine in a way the fixtures under
`packages/harness-js/test-targets/` cannot. Nothing here is imported by the engine or the
plugin.

| Package | Version | Upstream | Licence | Why it is here |
| --- | --- | --- | --- | --- |
| `yaml` | 2.9.1 | https://github.com/eemeli/yaml (npm tarball for 2.9.1) | ISC | Its findings need minimization over a long document, which is the case a shrink that only halves a string from the front cannot solve. It is the regression case for `crates/afz-engine/src/minimize.rs`. |

Only what the scan reads is kept: `src/`, the package manifest, and the upstream licence.
`autofuzz.json` in each directory is the scan project, and the scan is recorded in
`docs/examples/`.

Rerun a scan from the repository root:

```
cargo run -p afz-cli -- scan examples/yaml --seed 0x5e3a91c4
```

The run writes `examples/yaml/.autofuzz/<scanId>/scan.json`, which is not committed.

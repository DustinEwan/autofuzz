# Contributing

The crate boundaries follow the same line as the design: engine crates never parse a language, and plugin
packages never own the fuzzing loop. Use the layout below to find which side of that line a change
belongs on.

|Path|Contents|
|---|---|
|`crates/afz-protocol/`|Wire types and JSON-RPC framing. No policy.|
|`crates/afz-engine/`|Generator, corpus, scheduler, supervisor, oracles, minimizer, and the scan-result writer.|
|`crates/afz-cli/`|The `afz` binary.|
|`crates/afz-mcp/`|The `afz-mcp` binary: an MCP server on stdio that drives the engine in process.|
|`crates/afz-wasm/`|The WebAssembly harness plugin.|
|`crates/afz-conformance/`|The conformance suite for plugins: 23 wire rules, template v1, and the runtime lifecycle. `afz validate-harness` and `xtask check-harness` share it.|
|`crates/xtask/`|A fake plugin for protocol tests and negative controls, the JSON Schema generator, and the in-repo `check-harness` entry point.|
|`packages/harness-js/`|The `js-ts` plugin and its test targets.|
|`packages/harness-py/`|The `python` plugin and its test targets.|
|`targets/wasm-probe/`|A wasm module with one deliberate panic and one deliberate spin.|
|`docs/schema/`|Generated JSON Schema for every protocol type and for the scan result.|
|`docs/decisions/`|Architecture decision records.|
|`vendor/`, `examples/`|Real packages that were scanned to produce the records under `docs/`.|

## Before you push

Run these four commands:

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix packages/harness-js test
python3 -m unittest discover -s packages/harness-py/test -t packages/harness-py
```

`.github/workflows/ci.yml` runs the same commands on every push to `master` and on every pull request. The
clippy step passes `-D warnings`, so a new warning fails the build instead of scrolling past. That applies
to every target, tests included.

## Generated schemas

`docs/schema/*.json` is generated, so do not hand-edit it. After you change a protocol type or the scan
result, run `cargo run -p afz-xtask -- schema` and commit the regenerated files.

The test in `crates/xtask/tests/schema.rs` enforces that rule. It regenerates the schemas into a temporary
directory and fails when the committed bytes differ, so `cargo test --workspace` catches a drift between
the Rust types and the schema. Without that test, a plugin at runtime would be the one to notice.

## Reading list for a change

- [`docs/architecture.md`](docs/architecture.md) for how a scan works and where the reserved surface is.
- [`docs/cli.md`](docs/cli.md) for the command line, the configuration file, and the MCP tools.
- [`docs/harness-authoring.md`](docs/harness-authoring.md) before you write or change a plugin.
- [`docs/decisions/`](docs/decisions/) for the decisions behind the design. These are dated records, so
  some of them describe a plan that a later record changed.

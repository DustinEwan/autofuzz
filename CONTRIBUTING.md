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
|`crates/afz-engine/src/harnesses.rs`|The install registry: the `afz-harness.json` manifest, the installed record, and the lookup every `command`-less scan project resolves through.|
|`crates/afz-conformance/`|The conformance suite for plugins: 24 wire rules, template v1, the fixture-validator comparison, and the runtime lifecycle. `afz validate-harness`, `afz install-harness`, and `xtask check-harness` share it.|
|`crates/xtask/`|A fake plugin for protocol tests and negative controls, the JSON Schema generator, and the in-repo `check-harness` entry point.|
|`crates/afz-engine/tests/targets/`|Target trees the engine's own tests drive a plugin through: `blocked`, `minimize`, `oracles`.|
|`docs/schema/`|Generated JSON Schema for every protocol type and for the scan result, one directory per protocol revision.|
|`docs/decisions/`|Architecture decision records.|
|`vendor/`, `examples/`|Real packages that were scanned to produce the records under `docs/`.|

## Before you push

Run these two commands:

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The harness suites run in their own repositories (`autofuzz-harness-js-ts`,
`autofuzz-harness-python`, `autofuzz-harness-wasm`, `autofuzz-harness-starter`,
`autofuzz-harness-openapi`): each installs its own checkout with `afz install-harness` and
asserts the conformance report against its own golden. The engine's tests need no published
harness installed; the cross-repository acceptance tests that drive the real `js-ts` plugin are
`#[ignore]`d and run locally, after `afz install-harness <git url>`, with
`cargo test -- --ignored`. `.github/workflows/ci.yml` runs the same commands on every push to
`main` and on every pull request, after installing the repository's own fake plugin - that
install exercises the engine's side of the install contract, not a published harness's
conformance. The clippy step passes `-D warnings`, so a new warning fails the build instead
of scrolling past. That applies to every target, tests included.

## Generated schemas

`docs/schema/<revision>/*.json` (today `docs/schema/afz-1/`) is generated, so do not hand-edit
it. After you change a protocol type or the scan result, run `cargo run -p afz-xtask -- schema`
and commit the regenerated files. A revision's directory freezes once a newer revision exists:
a contract change adds a revision directory, it never edits an old one
([decision 0016](docs/decisions/0016-the-protocol-speaks-a-version-set.md)).

The test in `crates/xtask/tests/schema.rs` enforces that rule. It regenerates the schemas into a temporary
directory and fails when the committed bytes differ, so `cargo test --workspace` catches a drift between
the Rust types and the schema. Without that test, a plugin at runtime would be the one to notice.

## Reading list for a change

- [`docs/architecture.md`](docs/architecture.md) for how a scan works and where the reserved surface is.
- [`docs/cli.md`](docs/cli.md) for the command line, the configuration file, and the MCP tools.
- [`docs/harness-authoring.md`](docs/harness-authoring.md) before you write or change a plugin.
- [`docs/decisions/`](docs/decisions/) for the decisions behind the design. These are dated records, so
  some of them describe a plan that a later record changed.

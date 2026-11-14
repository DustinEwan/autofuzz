# Contributing

This repository holds the engine, the `afz` command, the `afz-mcp` server, and the conformance
suite. The harnesses live in their own repositories. The crates follow the same boundary as the design:
engine crates do not parse a language, and plugin packages do not own the fuzzing loop.

## Layout

| Path | Contents |
| --- | --- |
| `crates/afz-protocol/` | Wire types and JSON-RPC framing. No policy. |
| `crates/afz-engine/` | Generator, corpus, scheduler, supervisor, oracles, minimizer, and the scan-result writer. |
| `crates/afz-cli/` | The `afz` binary. |
| `crates/afz-mcp/` | The `afz-mcp` binary: an MCP server on stdio that drives the engine in process. |
| `crates/afz-conformance/` | The conformance suite: the wire rules, template v1, the fixture-validator comparison, and the runtime lifecycle. |
| `crates/xtask/` | A fake plugin for protocol tests, the JSON Schema generator, and the in-repo `check-harness` entry point. |
| `crates/afz-engine/tests/targets/` | Target trees that the engine's tests drive a plugin through. |
| `docs/schema/` | Generated JSON Schema for every protocol type and for the scan result. |
| `docs/decisions/` | Dated design records. |
| `vendor/`, `examples/` | Real packages that were scanned to produce the records under `docs/`. |

## Before you push

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The clippy step turns warnings into errors, tests included.

The harness suites run in their own repositories. Each one installs its own checkout with
`afz install-harness` and asserts the conformance report against its own golden. The engine's tests
need no published harness. The cross-repository tests that drive the real `js-ts` plugin are
`#[ignore]`d; run them locally after `afz install-harness <git-url>` with `cargo test -- --ignored`.

`.github/workflows/ci.yml` runs the same commands on every push to `main` and on every pull request.
It installs the repository's own fake plugin first, which exercises the engine's side of the install
contract rather than a published harness's conformance.

## Generated schemas

`docs/schema/<revision>/*.json` is generated. The current directory is `docs/schema/afz-1/`.
Do not edit it by hand.
After you change a protocol type or the scan result, run:

```sh
cargo run -p afz-xtask -- schema
```

Then commit the regenerated files. A revision directory never changes after a newer revision
exists. A contract change adds a revision directory and never edits an old one
([decision 0016](docs/decisions/0016-the-protocol-speaks-a-version-set.md)).

The test in `crates/xtask/tests/schema.rs` regenerates the schemas into a temporary directory and
fails when the committed bytes differ, so `cargo test --workspace` catches a drift between the Rust
types and the schema.

## Where to read

- [`docs/architecture.md`](docs/architecture.md): how a scan works, end to end.
- [`docs/cli.md`](docs/cli.md): the command line, the configuration file, and the MCP tools.
- [`docs/harness-authoring.md`](docs/harness-authoring.md): the plugin contract.
- [`docs/decisions/`](docs/decisions/): the decisions behind the design. These are dated records, so
  some of them describe a plan that a later record changed.
- [`docs/domains.md`](docs/domains.md): design notes on targets beyond code-level fuzzing. This is
  not a roadmap.

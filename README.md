# autofuzz

autofuzz is a structural fuzzer for TypeScript and JavaScript. It discovers a package's public
callables, generates type-conformant structured inputs, runs a coverage-guided loop, classifies
every call as `ok`, `rejected`, or `failed`, minimizes what it finds, and writes one
machine-readable scan result that other tools turn into deliverables.

The engine is language-neutral. Everything that touches a specific language or runtime lives in a
harness plugin, which runs out of process and speaks JSON-RPC over stdio. Two more harnesses are
the proof that the boundary holds: WebAssembly and Python — a stdlib-only plugin with no build
step. The same `afz` binary, the same oracle configuration, and the same scan result schema drive
all three, with no engine source difference.

Design-time targets are implemented. The `runtime` target class is reserved: a scan project whose
only block is `runtime` validates, and a scan of it exits with `runtime targets are not implemented`.

## Requirements

- Linux or macOS.
- Rust 1.88 or newer.
- Node 24 or newer, for the `js-ts` harness.
- Python 3.12 or newer, for the `python` harness (it uses `sys.monitoring`).

## Build

```sh
cargo build --release
npm --prefix packages/harness-js ci
npm --prefix packages/harness-js run build
```

The engine binary is `target/release/afz`. The `js-ts` plugin is
`packages/harness-js/dist/server.js`, which the engine starts as one process per worker, so the
plugin build must exist before a scan runs.
The `python` plugin is `packages/harness-py/src/server.py`. It imports nothing outside the
standard library, so there is nothing to build before a scan runs.

## Try it

```sh
cargo run -p afz-cli -- probe-plugin
cargo run -p afz-cli -- scan packages/harness-js/test-targets/billing \
  --budget invocations=50000 --seed 0x5e3a91c4
```

`probe-plugin` starts a plugin, shakes hands, and prints the inventory it discovers: the protocol
version, the declared capabilities, and one line per target with its parameter types and whether
its argument graph is constructible. The `billing` fixture is a package with one unguarded
dereference behind three nested conditionals, which coverage guidance reaches and unguided random
generation does not within the same budget.

## What a scan does

1. **Discovery.** The plugin builds a TypeScript program from the scan project and enumerates
   exported functions, public class methods, and `const` symbols bound to functions. It records
   every `throw` inside a target as a guard site. Discovery never imports target code, because
   importing a user module runs user code inside the plugin.
2. **Generation.** Inputs are `ValueTree` values generated from `TypeNode`, the shared type
   vocabulary with 13 kinds. A depth budget and per-path `ref` counting make recursive types
   terminate.
3. **Execution.** One plugin process per worker calls the target with the materialized arguments
   under an engine-enforced deadline. A worker that dies takes only its own process.
4. **Feedback.** Each invocation returns a coverage digest, a sorted set of stable edge ids. The
   engine keeps an input when its digest contains an edge the corpus has never produced, and the
   scheduler weights entries by new-edge yield and by time since last use.
5. **Classification.** The plugin reduces one call to `ok`, `rejected`, or `failed`. `rejected`
   means the target examined the input and refused it through a channel its own signature declares;
   `failed` means it tried to accept the input and broke. The engine never inspects an exception.
6. **Oracles.** `failed`, `deadline` (2000 ms), and `memoryGrowth` (256 MB) are on by default,
   along with `processExit`. `rejected` is off by default and can be turned on per target.
7. **Minimization.** Delta-debugging over the `ValueTree`, keeping a candidate only while it still
   produces the same fault class at the same top frame, plus a standalone `.ts` reproducer.
8. **Scan result.** One immutable JSON document per scan. Findings are deduplicated by an identity
   hashed over target id, fault class, and top frame, so the same bug is the same finding across
   scans.

One 32-bit seed drives every random choice. `--seed` with the same tree and the same configuration
replays the same invocation sequence. `--resume <scanDir>` reloads a previous corpus and suppresses
identities already recorded.

## Scan project

`autofuzz.json` at the target project root. `afz scan` finds it without arguments.

```jsonc
{
  "project": "billing-service",
  "codebase": {
    "harness": "js-ts",                  // or `command` for a plugin with no built-in argv
    "include": ["src/**/*.ts"],
    "exclude": ["**/*.test.ts"]
  },
  "oracles": { "deadline": { "ms": 2000 } },
  "budget": {
    "invocations": 200000,
    "wallClockMs": 900000,
    "workers": 0,                        // 0 means one plugin process per logical CPU
    "maxCorpus": 2048,
    "strategy": "guided"                 // or `random`, which ignores the corpus
  },
  "output": { "dir": ".autofuzz", "renderers": ["json", "markdown"] }
}
```

## Scope a scan to a change

`--git <selector>` limits a scan to a git change set: it scans the state that selector names and
fuzzes only the targets whose source file that selector changed.

|Selector|Scanned state|Change set computed against|
|---|---|---|
|`staged`|the index|`HEAD`|
|`unstaged`|the working tree|the index, plus the untracked files the ignore rules allow|
|`stash`, `stash@{2}`|that stash entry|the commit the stash was made on|
|`HEAD`, `v1.2.0`, `main~3`, a sha|that commit|its parent, or the empty tree for a root commit|
|`v1..HEAD`, `main~3..main`|the state at the end of the span|the start of the span|
|`v1...feature`|the state at the end of the span|the merge base of the two sides|

```sh
afz scan . --git HEAD --budget invocations=50000   # what the last commit touched
afz scan . --git staged                            # what is about to be committed
afz scan . --git unstaged                          # work in progress, new files included
afz scan . --git v1..HEAD                          # everything since the last release
```

A selector that names anything other than the working tree is checked out into a `git worktree` under
the output directory, so the scan reads the code as that state has it, and the tree is removed when
the scan ends. `scan.json` records the scope in its `git` object and `report.md` prints it in its
first lines: 12 clean targets inside a change set is not a clean package, and `inventory` counts the
scope.

Two boundaries. A wasm scan scopes by the `.wasm` file, not by the Rust sources that produced it, so
a commit that changed only `.rs` files reports that the scope touches no fuzzable target. A change
that breaks an unchanged *importer* of a changed file is outside the scope; omit `--git` to ask the
whole-tree question.

## Drive it from a coding harness

`afz-mcp` speaks the Model Context Protocol on stdio, so an agent in Claude Code, Cursor, VS Code
Copilot, Zed, or Codex CLI can ask "does this change set fuzz clean?" without a shell. It needs no
configuration: every call names the directory it acts on, so the agent reaches a different tree by
calling a tool again, the same way it does with the shell it already has. A scan project's
`codebase.command` is code the engine runs, so scan trees you already trust - the same trust
`afz scan` asks for.

```json
{"mcpServers": {"autofuzz": {"command": "afz-mcp"}}}
```

|Tool|What it answers|
|---|---|
|`afz_probe`|Which harness plugin a directory uses and which targets it would fuzz; cheap, no invocations.|
|`afz_scan`|Starts a coverage-guided scan and answers at once with a `jobId`; `git` scopes it to a change set.|
|`afz_scan_status`|One scan's state and live progress, answered immediately.|
|`await_scan`|Blocks until a scan ends; run it as a background task to be woken when the scan finishes. `timeoutSeconds` answers early with the state as it is then.|
|`afz_scan_cancel`|Ends a running scan early; the scan still writes its result, with `stoppedReason: interrupted`.|
|`afz_findings`|A finished scan's findings, newest scan first, each naming the input and reproducer files it came from.|

## Output

A scan writes `<output.dir>/<scanId>/`:

|Path|Contents|
|---|---|
|`scan.json`|The scan result: inventory, execution counts, coverage, findings, artifact paths, and the `git` change set when the scan was scoped. Validates against `docs/schema/scan-result.json`.|
|`report.md`|Markdown rendered from `scan.json` alone: one section per finding with severity, the minimized input, and the reproducer path.|
|`inputs/`|The original and minimized input for each finding, as `ValueTree` JSON.|
|`repros/`|A standalone reproducer script per finding, rendered by the harness that found it via the `reproducer` method; the extension is the plugin's (`repro-<id>.py`, `repro-<id>.mjs`). A finding whose harness does not declare `reproducer`, or fails the call, ships only its minimized input in `inputs/`, which is the replayable artifact.|
|`corpus/`|The retained inputs, which `--resume` reloads.|
|`coverage.json`|The edge-id map the plugin published.|
|`engine.log`|Per-worker and per-target accounting: restarts, deadline cuts, disabled targets.|

## Harnesses

|Harness|Discovery|Coverage|Status|
|---|---|---|---|
|`js-ts`|TypeScript compiler API over the scan project's declarations.|Per invocation, from `node:inspector` precise coverage in a worker thread.|Implemented, built-in command.|
|`python`|`ast` parse of the scan project's modules; no import.|Per invocation, from `sys.monitoring` line events.|Implemented, `packages/harness-py/`.|
|`wasm`|Exported functions of a core wasm module.|Post-hoc block instrumentation with `wasmparser` and `wasm-encoder`; the host reads the counter table from guest memory.|Implemented, `targets/wasm-probe/`.|

The `wasm` harness has no built-in command, so its scan project names one:

```jsonc
"command": ["target/release/afz-wasm",
            "targets/wasm-probe/target/wasm32-unknown-unknown/release/wasm_probe.wasm"]
```

Build both halves with `cargo build --release -p afz-wasm` and
`cargo build --release --target wasm32-unknown-unknown --manifest-path targets/wasm-probe/Cargo.toml`.

Like `wasm`, the `python` harness has no built-in command, so its scan project names one:
`"command": ["python3", "packages/harness-py/src/server.py"]`.

Adding another harness needs no engine change: `codebase.command` names the plugin's argv.
[`docs/harness-authoring.md`](docs/harness-authoring.md) is the contract a plugin must satisfy, the
measured cost of every operation it is asked to perform, and the traps the shipped harnesses paid
for. Check a plugin against it with `afz validate-harness --path <dir> --command <plugin argv…>`,
which drives the wire rules and the conformance template; inside this repository the same suite
runs as `cargo run -p afz-xtask -- check-harness`. The harnesses this repository maintains, and
what each one declares against the template, are in [`docs/harnesses.md`](docs/harnesses.md).

## Layout

|Path|Contents|
|---|---|
|`crates/afz-protocol/`|Wire types and JSON-RPC framing. No policy.|
|`crates/afz-engine/`|Generator, corpus, scheduler, supervisor, oracles, minimizer, scan-result writer.|
|`crates/afz-cli/`|The `afz` binary.|
|`crates/afz-mcp/`|The `afz-mcp` binary: an MCP server on stdio driving the engine in process.|
|`crates/afz-wasm/`|The WebAssembly harness plugin.|
|`crates/afz-conformance/`|The plugin conformance suite: wire rules, template v1, runtime lifecycle. Shared by `afz validate-harness` and `xtask check-harness`.|
|`crates/xtask/`|Fake plugin for protocol tests and negative controls, the JSON Schema generator, and the in-repo `check-harness` entry point.|
|`packages/harness-js/`|The `js-ts` plugin and its test targets.|
|`packages/harness-py/`|The `python` plugin and its test targets.|
|`targets/wasm-probe/`|A wasm module with one deliberate panic and one deliberate spin.|
|`docs/schema/`|Generated JSON Schema for every protocol type and for the scan result.|
|`docs/decisions/`|Architecture decision records.|
|`vendor/`, `examples/`|Real packages scanned to produce records under `docs/`.|

## Development

```sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm --prefix packages/harness-js test
python3 -m unittest discover -s packages/harness-py/test -t packages/harness-py
```

The same commands run in `.github/workflows/ci.yml` on every push to `master` and on every pull
request. The clippy step passes `-D warnings`, so a new warning in any target, tests included,
fails the build instead of scrolling past.

`docs/schema/*.json` is generated, not hand-written. After changing a protocol type or the scan
result, run `cargo run -p afz-xtask -- schema` and commit the regenerated files. The test in
`crates/xtask/tests/schema.rs` regenerates the schemas into a temporary directory and fails when
the committed bytes differ, so a drift between the Rust types and the schema is caught by
`cargo test --workspace` rather than by a plugin at runtime.

## Documentation

- [`docs/architecture.md`](docs/architecture.md) — the design: the plugin contract, the interface IR,
  the outcome classification rule, and the scan result shape.
- [`docs/harness-authoring.md`](docs/harness-authoring.md) — what a harness plugin must answer, what
  each answer costs, and the traps it can fall into. Read this before writing a plugin.
- [`docs/decisions/`](docs/decisions/) — the decisions behind it, each with what it excludes.
- [`docs/dogfood/ufo.md`](docs/dogfood/ufo.md) — a scan of a real package with a human verdict per
  finding.
- [`docs/examples/yaml.md`](docs/examples/yaml.md) — the same for `yaml`, including where the wall
  time of a large scan goes.

## Third-party sources

Packages under `vendor/` and `examples/` are copies of upstream projects, kept so a scan has a real
API to run against. They keep their upstream licences, which are in their own directories, and
nothing under those paths is compiled into the engine or the plugin.

## Licence

MIT. See [LICENSE](LICENSE).

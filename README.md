# autofuzz

autofuzz is a coverage-guided fuzzer. A plugin tells it what your target exposes and how to call it, so
autofuzz can fuzz anything a plugin can drive. When no plugin covers the kind of target you want, you
write one, and the fuzzer itself does not change.

- **You write no fuzz target.** Unlike libFuzzer, you do not wrap your code in a target function.
  The plugin reports the exported callables, and autofuzz builds the arguments for each call from the
  types that the callable declares.
- **Not tied to a target type.** The target can be a library, a REST API, a GUI, or an ABI. The three
  harnesses you can install today are all code-level: their plugins read a package or a WebAssembly
  module. A `runtime` block parses and validates, and `afz scan` stops in preflight with `runtime targets
  are not implemented`.
- **Coverage-guided, so it finds the buried bugs.** A scan keeps an input only when it reaches code that
  no earlier input reached, so nested conditions get exercised instead of skipped.
- **Findings arrive small.** A broken call is shrunk to the smallest input that still breaks it, with a
  script you can run by hand to see the fault again.
- **One document per scan.** `scan.json` is the whole result, so CI, your triage queue, and your
  pull-request bot all read the same numbers.

## Install and run a first scan

You need Linux or macOS and Rust 1.88 or newer. Beyond that you need the host runtime of the harness you
plan to scan, because no harness comes with the engine: each one lives in its own repository, and you
install it with `afz install-harness`. Three exist today: `js-ts` for TypeScript and JavaScript needs
Node 24 or newer, `python` for Python needs Python 3.12 or newer, and `wasm` scans a WebAssembly module.
[`docs/harnesses.md`](docs/harnesses.md) gives what each one can do and what it still cannot.

Build the engine, which is the `afz` command, then install the plugin for the harness you plan to scan.
The engine never imports your code: it starts a plugin process and calls your target through that
plugin, so the plugin has to be installed before a scan runs.

```sh
cargo build --release
./target/release/afz install-harness https://github.com/DustinEwan/autofuzz-harness-js-ts.git
```

The engine lands at `target/release/afz`. `install-harness` clones the harness repository, builds it,
and drives the plugin through the conformance suite, the checks every plugin must pass, before it
records anything. `afz harnesses` lists what the install recorded.

A scan project is a directory that holds an `autofuzz.json`, and that file names the harness the scan
uses. `vendor/ufo` in this repository is one, which is why the commands below work against it. Your own
project needs the file first, because `probe-plugin` with no `autofuzz.json` above the directory and no
`--command` answers from the fake plugin that the protocol tests drive, and prints that fixture's targets
instead of yours. The minimum is the `codebase` block under `Harness plugins` below, and
[`docs/cli.md`](docs/cli.md) gives every other key.

Start with a look rather than a scan. `probe-plugin` starts the plugin that a scan project names,
completes the handshake, and prints what the plugin discovered:

```sh
afz probe-plugin --path vendor/ufo
```

```text
plugin      js-ts 0.1.0 (node /home/dusti/.local/share/autofuzz/harnesses/js-ts/checkout/dist/server.js)
protocol    afz/1
class       design-time
languages   typescript, javascript
capability  coverage, stateful, concurrent, reproducer
discovery   static

targets     105
  afz:t:0dd364360549 decode                   (text: composite) constructible
  afz:t:3b95560b04c3 decodePath               (text: scalar) constructible
```

The lines above `targets` are what the plugin said about itself in the handshake: its name and version,
the protocol it speaks, the kind of target it handles, the languages it reads, what it can do, and how
it finds callables. The `targets` block is what it found in your package.

Every target line prints the parameter types and marks whether autofuzz can build arguments for them.
A target that it cannot build is reported as skipped with the reason, never dropped in silence.

Then scan the same package:

```sh
afz scan vendor/ufo --budget invocations=50000 --seed 0x5e3a91c4
```

`--budget` sets where the scan stops, and `--seed` fixes every random choice the scan makes, so the
same seed on the same tree replays the same calls.

`vendor/ufo` is a vendored copy of the real `ufo` URL package, and
[`docs/dogfood/ufo.md`](docs/dogfood/ufo.md) is the record of a scan of it: one crash, in a punycode
helper the package does not even export, with a human verdict on that finding.

## Scan output

A scan writes everything under the scanned project, in one directory per scan at
`<output.dir>/<scanId>/`, where `<scanId>` is that scan's id and `output.dir` defaults to `.autofuzz/`.
Open `report.md`. The machine-readable result beside it is `scan.json`, which validates
against [`docs/schema/scan-result.json`](docs/schema/scan-result.json), and it is written last so that
a reader never sees a scan directory whose artifacts are still being written.
[`docs/cli.md`](docs/cli.md) lists every file a scan leaves behind and what it is for.

## Harness plugins

A harness plugin is a program that the engine starts, one process per worker. The engine talks to it over
the plugin's stdin and stdout using JSON-RPC. The engine never parses your code and never imports it, so
a target that hangs or corrupts memory kills its own plugin process instead of your scan.

One consequence reaches your first scan project: **the engine never guesses how to start a plugin**.
Install the harness once with `afz install-harness`, and a scan project names the harness and nothing
else:

```jsonc
{
  "codebase": {
    "harness": "js-ts"   // resolved from the install record
  }
}
```

The `command` key in that same block names a plugin nobody installed - a build you are debugging, say -
and it wins over the record. An uninstalled name with no `command` is an error, never a guess.

`afz scan` reads `autofuzz.json` from the directory you point at, so a scan of a configured project
needs no arguments at all, and a directory with no `autofuzz.json` above it stops with a message that
tells you to create one. [`docs/cli.md`](docs/cli.md) gives every flag and every configuration key,
with the defaults.

## MCP server

`afz-mcp` speaks the Model Context Protocol on stdio, so an agent in Claude Code, Cursor, VS Code
Copilot, Zed, or Codex CLI can ask "does this change set fuzz clean?" without a shell. Configure it
with `{"mcpServers": {"autofuzz": {"command": "afz-mcp"}}}` and read
[`docs/cli.md`](docs/cli.md) for the six tools that it offers.

## Documentation

- [`docs/cli.md`](docs/cli.md) - every command and flag, the configuration file, scanning one git change
  set, the output files, and the MCP tools.
- [`docs/architecture.md`](docs/architecture.md) - how a scan works, end to end, and where the reserved
  surface is. Start here if you are going to change the engine.
- [`docs/harness-authoring.md`](docs/harness-authoring.md) - the contract that a plugin must satisfy, and
  what each answer costs.
- [`docs/harnesses.md`](docs/harnesses.md) - how far you can trust each harness you can install, and
  what each one still cannot do.
- [`docs/decisions/`](docs/decisions/) - the decisions behind the design, each with what it rules out.
- [`docs/dogfood/ufo.md`](docs/dogfood/ufo.md) and [`docs/examples/yaml.md`](docs/examples/yaml.md) -
  scans of real packages, with a human verdict on each finding.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) - the repository layout, and the commands to run before you
  push.

## Third-party sources

Packages under `vendor/` and `examples/` are copies of upstream projects. They are here so that a scan
has a real API to run against. Each one keeps its upstream licence, which you find in its own directory.
Nothing under those paths is compiled into the engine or into a plugin.

## Licence

MIT. See [LICENSE](LICENSE).

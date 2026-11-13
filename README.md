# autofuzz

A universal, coverage-guided fuzzer. Whatever a harness can call, autofuzz can fuzz.

- **No fuzz target to write.** Unlike a libFuzzer-style harness, you do not wrap your code in a target
  function. autofuzz reads the exported callables and builds arguments from the types that they declare.
- **Not tied to a target type.** A harness is a separate program, so the target can be a library, a REST
  API, a GUI, or an ABI. Adding one needs no change to the fuzzer itself.
- **Coverage-guided, so it finds the buried bugs.** A scan keeps an input only when it reaches code that
  no earlier input reached, so nested conditions get exercised instead of skipped.
- **Findings arrive small.** A broken call is shrunk to the smallest input that still breaks it, with a
  script you can run by hand to see the fault again.
- **One document per scan.** `scan.json` is the whole result, so CI, your triage queue, and your
  pull-request bot all read the same numbers.

## Get a scan running

You need Linux or macOS, Rust 1.88 or newer, and the host runtime of every harness you plan to scan:
Node 24 or newer for `js-ts`, and Python 3.12 or newer for `python`.

Build the engine, then install the plugin for the harness you plan to scan. The engine starts one
plugin process per worker, so a plugin has to be installed before a scan runs.

```sh
cargo build --release
./target/release/afz install-harness https://github.com/DustinEwan/autofuzz-harness-js-ts.git
```

The engine lands at `target/release/afz`. `install-harness` clones the harness repository, builds
it, and drives the conformance suite before it records anything; `afz harnesses` lists what the
install recorded. A scan project names the harness, and the engine starts the plugin the record
names.

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

Every target line prints the parameter types and marks whether autofuzz can build arguments for them.
A target that it cannot build is reported as skipped with the reason, never dropped in silence.

Then scan the same package:

```sh
afz scan vendor/ufo --budget invocations=50000 --seed 0x5e3a91c4
```

`vendor/ufo` is a vendored copy of the real `ufo` URL package. The record's guided scan found one
crash, in a punycode helper the package does not even export;
[`docs/dogfood/ufo.md`](docs/dogfood/ufo.md) is the full run, with a human verdict on that finding.

## What you get

A scan writes everything under the scanned project, at `<output.dir>/<scanId>/`, and `.autofuzz/` is
the default. Open `report.md`. The machine-readable result beside it is `scan.json`, which validates
against [`docs/schema/scan-result.json`](docs/schema/scan-result.json), and it is written last so that
a reader never sees a scan directory whose artifacts are still being written.
[`docs/cli.md`](docs/cli.md) lists every file a scan leaves behind and what it is for.

## The one rule you meet first

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

`command` is how you name a plugin nobody installed - a build you are debugging, say - and it wins
over the record. An uninstalled name with no `command` is an error, never a guess.

`afz scan` reads `autofuzz.json` from the directory you point at, so a scan of a configured project
needs no arguments at all. [`docs/cli.md`](docs/cli.md) gives every flag and every configuration key,
with the defaults.

## Run scans from a coding agent

`afz-mcp` speaks the Model Context Protocol on stdio, so an agent in Claude Code, Cursor, VS Code
Copilot, Zed, or Codex CLI can ask "does this change set fuzz clean?" without a shell. Configure it
with `{"mcpServers": {"autofuzz": {"command": "afz-mcp"}}}` and read
[`docs/cli.md`](docs/cli.md) for the six tools that it offers.

## Where to go next

- [`docs/cli.md`](docs/cli.md) - every command and flag, the configuration file, scanning one git change
  set, the output files, and the MCP tools.
- [`docs/architecture.md`](docs/architecture.md) - how a scan works, end to end, and where the reserved
  surface is. Start here if you are going to change the engine.
- [`docs/harness-authoring.md`](docs/harness-authoring.md) - the contract that a plugin must satisfy, and
  what each answer costs.
- [`docs/harnesses.md`](docs/harnesses.md) - how far you can trust each shipped harness, and what each
  one still cannot do.
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

# Command line and configuration

`afz` runs scans and manages harness plugins. `afz-mcp` exposes the same engine to an MCP client.
This page gives the exact flags, keys, and output files. If you are new, read the
[README](../README.md) first; it walks through one scan.

## Contents

- [Common tasks](#common-tasks)
- [Command reference](#command-reference)
- [The scan project file](#the-scan-project-file)
- [Git scoping](#git-scoping)
- [Scan output files](#scan-output-files)
- [Reproducible scans](#reproducible-scans)
- [Environment variables](#environment-variables)
- [MCP server](#mcp-server)

## Common tasks

```sh
# See what a scan project would fuzz, without spending a budget.
afz probe-plugin --path my-project

# Run a scan with a fixed seed and a 50000-call budget.
afz scan my-project --budget invocations=50000 --seed 0x5e3a91c4

# Scan only what the last commit touched.
afz scan . --git HEAD

# Continue from the corpus of an earlier scan.
afz scan my-project --resume .autofuzz/M7D0DJ2ZG8

# Check a harness plugin before you install it.
afz validate-harness

# Install a harness and record its command.
afz install-harness https://github.com/DustinEwan/autofuzz-harness-js-ts.git

# Write a starter scan project.
afz init project
```

## Command reference

`afz` has these subcommands. A command exits 0 on success and 1 on an error. `validate-harness` also
exits 1 when the conformance suite finds a violated rule.

### `afz probe-plugin`

Starts the plugin that a scan project names, completes the handshake, and prints the targets that it
discovered. It costs no invocations.

| Flag | Meaning |
| --- | --- |
| `--command <argv...>` | Plugin command to start, in place of the one that the scan project declares. |
| `--path <dir>` | Scan project to discover against. Defaults to the current directory. |
| `--json` | Print the raw JSON answer instead of the table. |

With no `--command` and no `autofuzz.json` above the directory that you point at, the command uses
the fake plugin that the protocol tests drive. That plugin sits beside `afz` only in a workspace
built with `cargo build --workspace`. A binary from `install.sh` has none beside it.

When the plugin declares the `readiness` capability, the command also sends `ready`. A not-ready
answer makes the command exit non-zero with the plugin's own reason.

### `afz scan`

Runs the engine against one scan project and writes the result. It checks the configuration and the
git selector before it prints the banner.

| Flag | Meaning |
| --- | --- |
| `<dir>` | Directory to scan. Defaults to the scan project's discovery root. |
| `--config <path>` | Scan project file. Defaults to the nearest `autofuzz.json` above the scan root. |
| `--budget <key=value...>` | Budget limits, repeated or comma-separated: `invocations=50000`, `wall=90s`. |
| `--seed <hex or decimal>` | Seed for every random choice. The same seed on the same tree replays the same calls. |
| `--resume <scanDir>` | Earlier scan directory. The new scan starts from the corpus in that directory. |
| `--seeds <file>` | JSON file holding `[{"targetId":…,"values":[…]}]` inputs to drive before the guided loop starts. Each seed costs one invocation, and the engine re-measures the coverage an input earns instead of trusting a recorded edge set. To continue from an earlier scan's `corpus/`, use `--resume`: a corpus file carries `edges`, which a seed file must not. |
| `--strategy <guided\|random>` | `guided` selects inputs with coverage. `random` ignores the corpus. |
| `--git <selector>` | Scan one git change set. See [Git scoping](#git-scoping). |
| `--workers <n>` | Plugin processes to run at once. Overrides `budget.workers`. |
| `--quiet`, `-q` | Print only the scan summary, not the progress samples. |

`--budget` takes these keys:

| Key | Value | Example |
| --- | --- | --- |
| `invocations` | Number of calls. | `invocations=50000` |
| `wall` | Duration in `ms`, `s`, `m`, or `h`, or a number of seconds. | `wall=90s` |

A scan writes its output under the scan project, not under the directory that you ran `afz` from.

### `afz validate-harness`

Runs the manifest's `build` command, renders the conformance project with the harness's emitter, and
drives the suite against the plugin that the manifest names. It never runs `install`.

| Flag | Meaning |
| --- | --- |
| `<harness>` | An installed harness name, or a directory that holds `afz-harness.json`. Defaults to the working directory. |
| `--targets <n>` | Number of discovered targets that `prepare` and `invoke` run on. Default 8. |
| `--deadline <ms>` | `deadlineMs` for every driven invocation. Defaults to the manifest's `conformance.deadlineMs`, then 1000. |
| `--json` | Print the machine-readable report instead of the table. |

Validation takes no `--command` and no `--path`. The manifest carries the plugin command, and the
emitter carries the scan project.

### `afz install-harness`

Clones a harness repository, runs its `install` and `build` commands, validates the plugin, and
records it. A failed install leaves the previous install of that name in place.

| Flag | Meaning |
| --- | --- |
| `<url>` | Git URL or local path of a harness repository with an `afz-harness.json`. |
| `--ref <reference>` | Branch, tag, or commit to install. Defaults to the repository's default branch. |
| `--no-validate` | Record the plugin without running the conformance suite. A manifest with no `conformance` block installs only this way. |
| `--json` | Print the conformance report as JSON instead of the table. |

The record lives at `<registry>/<name>/harness.json`, where `<registry>` is `AFZ_HOME/harnesses`.

### `afz harnesses`

Prints one row per installed harness: name, commit, and the command that a scan resolves. Pass
`--json` for the records as JSON.

### `afz export-schema`

Writes every JSON Schema compiled into the binary and prints each path. These are the same bytes that
`afz validate-harness` validates with.

| Argument or flag | Meaning |
| --- | --- |
| `[version]` | Protocol revision to export, for example `afz/1`. Defaults to the newest revision that this build speaks. |
| `--out <dir>` | Directory to write into. Defaults to `schema` under the working directory. |

### `afz init project`

Writes an `autofuzz.json`. Every question has a flag, so the command works in a terminal, in a
script, and in CI. A question with a documented default takes that default when no terminal is
available. A question with no default stops with the flags that answer it.

| Argument or flag | Meaning |
| --- | --- |
| `<dir>` | Directory to write `autofuzz.json` into. Defaults to the working directory. |
| `--name <name>` | Project name. Defaults to the directory's name. |
| `--harness <name>` | Harness that drives the scan. In a terminal, the prompt lists the installed harnesses and accepts another name. |
| `--command <argv...>` | Plugin command for a harness that nobody installed. Space-separated, with no quoting. |
| `--include <glob...>` | Discovery include globs, repeated or comma-separated. |
| `--exclude <glob...>` | Discovery exclude globs, repeated or comma-separated. |
| `--output-dir <dir>` | `output.dir`. Defaults to `.autofuzz`. |
| `--renderers <list>` | `output.renderers`, comma-separated: `json`, `markdown`. Defaults to `json,markdown`. |
| `--invocations <n>` | `budget.invocations`. Defaults to 100000. |
| `--wall-clock-seconds <n>` | `budget.wallClockMs`, asked in seconds. Defaults to 900. |
| `--strategy <guided\|random>` | `budget.strategy`. Defaults to `guided`. |

The wizard validates the document before it writes anything. If `autofuzz.json` already exists, it
reports the file and leaves it alone.

### `afz init harness`

Writes an `afz-harness.json` and the protocol schemas that the plugin validates against. Starter
mode clones the official scaffold and renames its manifest. Blank mode writes only the manifest and
the schemas.

| Argument or flag | Meaning |
| --- | --- |
| `<dir>` | Directory to create and fill. Defaults to the working directory. |
| `--from <starter\|blank>` | `starter` clones the official starter repository and renames its manifest. `blank` writes only the manifest and the schemas. |
| `--source <url-or-path>` | Clone source for `--from starter`. Defaults to `https://github.com/DustinEwan/autofuzz-harness-starter.git`. A local path clones offline. |
| `--name <name>` | Harness name. Defaults to the directory's name. The command rejects `/`, `\`, and `..`. |
| `--protocol <revision>` | Protocol revision whose schemas are exported, for example `afz/1`. Defaults to the newest revision that this build speaks. |
| `--command <argv...>` | Plugin command, blank mode only. Space-separated, with no quoting. |
| `--install <argv...>` | Dependency-fetch command, blank mode only. Run only by `afz install-harness`. Empty to skip. |
| `--build <argv...>` | Build command, blank mode only. Run by `afz install-harness` and `afz validate-harness`. Empty to skip. |
| `--emitter <argv...>` | Conformance emitter command, blank mode only. An empty answer leaves out the `conformance` block. |
| `--deadline-ms <ms>` | `conformance.deadlineMs`. Defaults to 1500. |

Starter mode needs a new or empty directory, because git rejects a non-empty clone target. Blank
mode accepts a directory that already holds a plugin, which makes it the way to repair a checkout
whose manifest is wrong.

Both modes export the chosen revision's schemas into `schema/`. The manifest carries no protocol
field, because the handshake negotiates the revision.

## The scan project file

A scan project is a directory with an `autofuzz.json` at its root. The `harness` block is required.
The other three blocks have defaults.

```jsonc
{
  "project": "billing-service",
  "harness": {
    "name": "js-ts",
    // `command` omitted: the installed `js-ts` plugin resolves from the registry.
    "root": ".",                                        // discovery root, relative to this file
    "include": ["src/**/*.ts"],
    "exclude": ["**/*.test.ts"],
    "options": { "rejectClasses": ["ValidationError"] } // the harness reads this object; the engine does not
  },
  "oracles": { "deadline": { "ms": 2000 } },
  "budget": {
    "invocations": 200000,
    "wallClockMs": 900000,
    "workers": 0,
    "maxCorpus": 2048,
    "strategy": "guided"
  },
  "output": { "dir": ".autofuzz", "renderers": ["json", "markdown"] }
}
```

The engine rejects an unknown key instead of ignoring it.

### The harness block

`harness.name` names the harness. `harness.command` starts the plugin. The engine needs a command
only when `afz install-harness` has no record for that name. An uninstalled name with no command is
an error, because the engine never guesses a runtime.

The engine runs the plugin with the directory that holds `autofuzz.json` as its working directory. A
relative path in `command` resolves from where you ran `afz`, so an absolute path is safer.

`harness.root` sets the discovery root, relative to the directory that holds `autofuzz.json`.
`harness.include` and `harness.exclude` are glob lists that the engine forwards to the plugin. The
engine adds the scan output directory to `exclude`. `harness.env` adds environment variables to the
plugin process.

The engine reads nothing inside `harness.options`. The plugin reads that object itself, from the
file that `discover.configPath` names. The two code-level harnesses use `rejectClasses` to decide
which errors count as refusals; `openapi` uses `allowMutating` to decide which operations it may
call.

### The oracles block

These rules turn an outcome into a finding.

| Key | Default | Meaning |
| --- | --- | --- |
| `oracles.failed` | `severity: "high"` | A call that broke. |
| `oracles.rejected` | `severity: "off"` | A call that the target refused. Set a severity to report rejections. |
| `oracles.deadline` | `severity: "medium"`, `ms: 2000` | A call that ran past its deadline. Turning it off removes the deadline from every call. |
| `oracles.memoryGrowth` | `severity: "medium"`, `mb: 256` | A call that reported more memory than the limit. |
| `oracles.processExit` | `severity: "high"` | The plugin process died. |

`oracles.overrides` reclassifies one `via` for one target as `ok`, `rejected`, or `failed`. Use it
when a target documents a channel as internal-failure-only.

```jsonc
"oracles": {
  "rejected": { "severity": "low" },
  "overrides": [
    { "target": "afz:t:9c1f", "via": "guard-site", "treatAs": "failed" }
  ]
}
```

An oracle name that the engine does not implement is a configuration error. `oracles.differential`
fails at load with `oracle "differential" is not implemented in this version`.

### The budget block

`budget.invocations` and `budget.wallClockMs` set the two stopping points. The scan stops at
whichever comes first. `budget.workers` sets the number of plugin processes; `0` means one per
logical CPU. `budget.maxCorpus` caps the corpus. `budget.strategy` is `guided` or `random`.

### The output block

`output.dir` is the directory for scan results, relative to the project root. `output.renderers`
lists `json` and `markdown`. `json` always writes `scan.json`; `markdown` adds `report.md`. A
renderer name that the engine does not implement is a configuration error.

## Git scoping

`--git <selector>` narrows a scan to one git change set. One selector picks both the code state that
the scan reads and the files that count as changed.

| Selector | Scanned state | Change set computed against |
| --- | --- | --- |
| `staged` | the index | `HEAD` |
| `unstaged` | the working tree | the index, plus untracked files that the ignore rules allow |
| `stash`, `stash@{2}` | that stash entry | the commit where the stash was made |
| `HEAD`, `v1.2.0`, `main~3`, a sha | that commit | its parent, or the empty tree for a root commit |
| `v1..HEAD`, `main~3..main` | the state at the end of the span | the start of the span |
| `v1...feature` | the state at the end of the span | the merge base of the two sides |

```sh
afz scan . --git HEAD --budget invocations=50000   # what the last commit touched
afz scan . --git staged                            # what is about to be committed
afz scan . --git unstaged                          # work in progress, new files included
afz scan . --git v1..HEAD                          # everything since the last release
```

For any selector other than the working tree, autofuzz creates a `git worktree` under the output
directory and scans that tree. It removes the worktree when the scan ends.

The result records the scope in the `git` object in `scan.json`, the first lines of `report.md`, and
the inventory counts. Twelve clean targets inside a change set is a narrower claim than a clean
package, and the result keeps the two claims apart.

Two limits apply to a scoped run:

- A wasm scan scopes by the `.wasm` file, not by the Rust sources that produced it. A commit that
  changed only `.rs` files reports that the scope touches no fuzzable target, and the scan stops
  instead of reporting a clean run.
- A change can break a file that imports the changed file. That importer is unchanged, so it sits
  outside the scope. Omit `--git` when you want the whole-tree answer.

## Scan output files

A scan writes everything under `<output.dir>/<scanId>/`.

| Path | Contents |
| --- | --- |
| `scan.json` | The scan result: inventory, execution counts, coverage, findings, artifact paths, and the `git` change set when the scan was scoped. It validates against [`schema/afz-1/scan-result.json`](schema/afz-1/scan-result.json). The engine writes it last. |
| `report.md` | Markdown rendered from `scan.json` alone. One section per finding, with its severity, its minimized input, and the path of its reproducer. |
| `inputs/` | The input that produced each finding, as `ValueTree` JSON, and the minimized input when the scan shrank it. |
| `repros/` | One standalone reproducer script per finding, with the name `afz-f-<id>.<extension>`. The finding gets no script when the harness does not declare `reproducer`, when the call fails, or when the finding is a repeat marked `seenBefore`. |
| `corpus/` | The inputs that the scan kept. `--resume` loads them again. |
| `coverage.json` | The coverage map that the plugin published. Absent when the plugin declares no `coverage` capability, or when `coverageMap` fails or times out. |
| `engine.log` | Per-worker and per-target accounting: restarts, deadline cuts, disabled targets, and plugin stderr. |
| `../seen.json` | One level above the scan directories: the finding identities that this project has already reported. Every scan reads it and adds to it. |

## Reproducible scans

`--seed` is a 32-bit seed that drives every random choice. The same seed with the same tree and the
same configuration replays the same sequence of calls. `--resume <scanDir>` adds the corpus of an
earlier scan.

Suppression of known findings is separate. Every scan reads `seen.json`, whether or not it resumes.
A finding that appears again is still reported, with `seenBefore: true`. The engine does not
minimize a repeat and does not ask for a reproducer for it.

Coverage ids include the file URL, so a corpus carried into a materialized git tree reports every
edge as new. The run is valid, but its coverage ratio is not comparable to the earlier run.
`engine.log` records that condition when `--resume` and `--git` are both present.

## Environment variables

| Variable | Meaning |
| --- | --- |
| `AFZ_HOME` | Base directory for the harness registry. The registry is `$AFZ_HOME/harnesses`. Unset, the registry falls back to `$XDG_DATA_HOME/autofuzz/harnesses`, then to `~/.local/share/autofuzz/harnesses`. |
| `AFZ_INSTALL_DIR` | Directory where `install.sh` puts `afz` and `afz-mcp`. Defaults to `~/.local/bin`. |
| `AFZ_ROOT` | Set by the engine for a plugin: the directory that holds `autofuzz.json`. |
| `AFZ_CONFIG` | Set by the engine for a plugin: the absolute path of `autofuzz.json`. |

## MCP server

`afz-mcp` speaks the Model Context Protocol on stdio. An agent in Claude Code, Cursor, VS Code
Copilot, Zed, or Codex CLI can probe a directory, start a scan, wait for it, and read the findings
without a shell.

```json
{"mcpServers": {"autofuzz": {"command": "afz-mcp"}}}
```

You configure no directories. Every tool call gives the directory that it acts on.

| Tool | What it answers |
| --- | --- |
| `afz_probe` | The harness plugin that a directory uses, and the targets that it would fuzz. |
| `afz_scan` | Starts a scan and returns a `jobId` at once. Pass `git` to scope the scan, and `seeds` to drive inputs of your own before the guided loop starts. |
| `afz_scan_status` | The state and live progress of one scan, answered immediately. |
| `await_scan` | Waits until a scan ends. `timeoutSeconds` answers early with the state at that moment. |
| `afz_scan_cancel` | Stops a running scan early. The scan still writes its result, with `stoppedReason: interrupted`. |
| `afz_findings` | The findings of a finished scan, newest scan first. `markdown: true` adds `report.md`. |
| `afz_harnesses` | The harness plugins installed on this machine, and the plugin command each one resolves to. An empty list is the answer "nothing is installed yet", not a failure. |
| `afz_validate_harness` | Builds a harness and answers rule by rule whether its plugin conforms. A report full of violations is a successful call: the suite ran and the harness failed it. |

The server drives the engine in process, so it runs one scan at a time. It rejects a second
`afz_scan` while one is running instead of queueing it.

The `harness.command` in a scan project is code that the engine runs. Point `afz-mcp` only at scan
trees that you trust.

# Driving autofuzz

This is the reference for the two ways you drive a scan: the `afz` command line and the `afz-mcp`
server. It gives every flag, every configuration key with its default, how a scan picks what is in
scope, and what a scan leaves on disk. For how a scan works inside, read
[`architecture.md`](architecture.md) instead.

## The five subcommands

|Command|What it does|
|---|---|
|`afz probe-plugin`|Starts a plugin, completes the handshake, and prints the inventory it discovers. Costs no invocations.|
|`afz scan`|Runs a coverage-guided scan and writes the scan result.|
|`afz validate-harness`|Drives a harness plugin through the conformance suite and reports rule by rule.|
|`afz install-harness`|Clones a harness repository, builds and validates its plugin, and records it for scans.|
|`afz harnesses`|Lists the installed harness plugins and the command each one resolves to.|

### `afz probe-plugin`

|Flag|Meaning|
|---|---|
|`--command <argv…>`|Plugin argv to start, in place of the one the scan project names.|
|`--path <dir>`|Scan project to discover against. Defaults to the current directory.|
|`--json`|Print raw JSON instead of the table.|

With no `--command` and no scan project above the directory you point at, `probe-plugin` falls back to
the fake plugin that the protocol tests drive, which `cargo build --release` puts beside `afz`. That is
how you check the wire when you have no target yet.

### `afz scan`

|Flag|Meaning|
|---|---|
|`<dir>`|Directory to scan. Defaults to the scan project's own root.|
|`--config <path>`|Scan project file. Defaults to the nearest `autofuzz.json` above the scan root.|
|`--budget <key=value…>`|Budget limits, repeated or comma-separated: `invocations=50000`, `wall=90s`.|
|`--seed <hex or decimal>`|Seed for every random choice the scan makes, so a rerun replays.|
|`--resume <scanDir>`|A previous scan directory: its corpus is reloaded.|
|`--strategy <guided\|random>`|`guided` steers with coverage; `random` generates without the corpus.|
|`--git <selector>`|Scope the scan to one git change set. See below.|
|`--workers <n>`|Plugin processes to run at once. Overrides `budget.workers`.|
|`--quiet`, `-q`|Print only the summary, not the coverage samples.|

Two habits are worth learning early. `cargo run -p afz-cli -- scan …` works in place of `afz scan …`,
but it runs a debug build, which is slower per invocation. And a scan writes its output under the
scanned project, not under the directory you ran `afz` from.

### `afz validate-harness`

|Flag|Meaning|
|---|---|
|`<harness>`|An installed harness name, or a directory holding `afz-harness.json`. Defaults to the working directory, so running it inside a harness checkout validates that checkout.|
|`--targets <n>`|How many discovered targets `prepare` and `invoke` run on. Default 8.|
|`--deadline <ms>`|`deadlineMs` for every driven invocation. Default: the harness manifest's `conformance.deadlineMs`, then 1000.|
|`--json`|Print the machine-readable report instead of the table.|

Validation names a harness and nothing else. The plugin's entrypoint is that harness's
`afz-harness.json` `command`, and the scan project is whatever that harness's `conformance.emitter`
renders into a temporary directory: there is no `--command` and no `--path`, because the harness
already carries both. Pointing a plugin at an arbitrary project is a different job and belongs to
`afz probe-plugin` and `afz scan`, which keep `--command`.

### `afz install-harness`

|Flag|Meaning|
|---|---|
|`<url>`|Git URL or local path of a harness repository carrying `afz-harness.json`.|
|`--ref <reference>`|Branch, tag, or commit to install. Default: the repository's default branch.|
|`--no-validate`|Record the plugin without driving the conformance suite. A manifest with no `conformance` block installs only this way.|
|`--json`|Print the conformance report as JSON instead of the table.|

Install clones to a staging directory, runs the manifest's `install` argv and then its `build` argv,
and validates the plugin against the conformance suite before recording anything. A failed install
leaves the previous install of that name in place and records nothing. The record lives at
`$AFZ_HOME/autofuzz/harnesses/<name>/harness.json` and holds the absolute plugin argv, the checkout
root, and the conformance block.

### `afz harnesses`

|Flag|Meaning|
|---|---|
|`--json`|Print the records as JSON instead of the table.|

Lists one row per installed harness: name, version-free handshake name, and the resolved command.

## The scan project file

A scan project is a directory with an `autofuzz.json` at its root. The file says which code to fuzz, how
hard to try, and where to write the result.

```jsonc
{
  "project": "billing-service",
  "codebase": {
    "harness": "js-ts",
    // `command` omitted: the installed `js-ts` plugin resolves from the registry.
    "include": ["src/**/*.ts"],
    "exclude": ["**/*.test.ts"],
    "rejectClasses": ["ValidationError"]      // error classes you declare as this API's refusal channel
  },
  "oracles": { "deadline": { "ms": 2000 } },
  "budget": {
    "invocations": 200000,                    // default 100000
    "wallClockMs": 900000,                    // default 900000
    "workers": 0,                             // default 0: one plugin process per logical CPU
    "maxCorpus": 2048,                        // default 2048
    "strategy": "guided"                      // default `guided`; `random` ignores the corpus
  },
  "output": { "dir": ".autofuzz", "renderers": ["json", "markdown"] }
}
```

Every block is optional, and an unknown key is an error rather than a typo you never notice. A scan does
need one target block, because that is where the plugin and the file set come from. Today that block is
`codebase`. The `runtime` block is reserved: a scan project whose only block is `runtime` passes
validation, and a scan of it stops with `runtime targets are not implemented`.

Four keys are worth more than a comment:

- `codebase.harness` always names the harness. `command` is optional when `afz install-harness`
  recorded that name, and the engine never guesses a runtime otherwise: an uninstalled name is an
  error, not a `PATH` search. `command` is how you name a plugin nobody installed; the engine runs it
  with the scan root as the working directory, and a relative path in it is resolved from where you
  run `afz`, so an absolute path is the safer habit.
- `codebase.rejectClasses` changes how a call is classified for your code only. Both shipped plugins
  read it, and each has its own default idea of what a refusal looks like.
- `output.renderers` accepts `json` and `markdown`, which are the two renderers that exist today. The
  list controls only whether `report.md` is rendered: `scan.json` is written whatever the list holds.
- A scan always excludes its own output directory from discovery. Without that rule, a second scan of the
  same project would find the previous run's reproducers and fuzz them.

Four oracles are on by default: `failed` at `high`, `deadline` at 2000 ms and `medium`, `memoryGrowth` at
256 MB and `medium`, and `processExit` at `high`. The `rejected` oracle is off by default, and you can turn
it on for one target at a time.

## Scan one git change set

`--git <selector>` narrows a scan to one git change set. One selector picks both the code state that
the scan reads and the files that count as changed. So the scan answers "does
this change fuzz clean?" instead of "does the whole repository fuzz clean?".

|Selector|Scanned state|Change set computed against|
|---|---|---|
|`staged`|the index|`HEAD`|
|`unstaged`|the working tree|the index, plus the untracked files that the ignore rules allow|
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

A selector that points at anything other than the working tree needs a checkout first. autofuzz creates a
`git worktree` under the output directory and points the scan at it, so the scan reads the code exactly as
that state has it. autofuzz removes the worktree when the scan ends.

The result records the scope in three places. The `git` object in `scan.json` holds it, `report.md` prints
it in its first lines, and the inventory in `scan.json` counts the files in scope rather than the whole
package. That distinction matters: twelve clean targets inside a change set is a narrower claim than a
clean package.

Two limits are worth knowing before you rely on a scoped run:

- A wasm scan scopes by the `.wasm` file, not by the Rust sources that produced it. A commit that changed
  only `.rs` files therefore reports that the scope touches no fuzzable target, and the scan stops instead
  of reporting a clean run.
- A change can break a file that imports the changed file, and that importer is unchanged, so it sits
  outside the scope. Omit `--git` when you want to ask the whole-tree question.

## What a scan writes

A scan writes everything that it produces under `<output.dir>/<scanId>/`.

|Path|Contents|
|---|---|
|`scan.json`|The scan result: the inventory, the execution counts, coverage, findings, artifact paths, and the `git` change set when you scoped the scan. It validates against [`docs/schema/scan-result.json`](schema/scan-result.json). The writer saves it last, so a reader never sees a scan directory whose artifacts are still being written.|
|`report.md`|Markdown rendered from `scan.json` alone. One section per finding, with its severity, its minimized input, and the path of its reproducer.|
|`inputs/`|The input that produced each finding, as `ValueTree` JSON, and the minimized input for a finding that this scan minimized.|
|`repros/`|One standalone reproducer script per finding. The harness that found the fault renders it through the `reproducer` method, and the file extension is the plugin's own (`repro-<id>.mjs`, `repro-<id>.py`). A finding gets no script when its harness does not declare `reproducer`, when that call fails, or when the finding is a repeat marked `seenBefore`, because the engine does not minimize a repeat either. The input in `inputs/` is replayable in every case.|
|`corpus/`|The inputs the scan kept, which `--resume` loads again.|
|`coverage.json`|The map of coverage edge ids that the plugin published. Absent when the plugin declares no `coverage` capability, or when its `coverageMap` call fails or times out.|
|`engine.log`|Per-worker and per-target accounting: restarts, deadline cuts, and disabled targets.|
|`../seen.json`|One level up, beside the scan directories: the finding identities this project has already reported. Every scan reads it and adds to it.|

Two flags make a scan repeatable. One 32-bit seed drives every random choice, so the same `--seed` with the
same tree and the same configuration replays the same sequence of calls. `--resume <scanDir>` adds the
corpus of an earlier scan, so the new scan starts from the inputs that scan had kept.

## The plugin command for each harness

Install each shipped harness once, from its own repository, and scan projects name the harness and
nothing else:

```sh
afz install-harness https://github.com/DustinEwan/autofuzz-harness-js-ts.git
afz install-harness https://github.com/DustinEwan/autofuzz-harness-python.git
afz install-harness https://github.com/DustinEwan/autofuzz-harness-wasm.git
```

```jsonc
"codebase": { "harness": "js-ts" }
```

The install builds the plugin and validates it against the conformance suite before it records the
argv, so a scan starts the plugin that passed, from the path the record holds. `afz harnesses` shows
what each name resolves to.

`command` remains the way to point a scan at a plugin nobody installed — a build you are debugging,
for instance — and it wins over the record:

```jsonc
"codebase": { "harness": "js-ts", "command": ["node", "/home/me/harness-js/dist/server.js"] }
```

For wasm, the scan project names neither a command nor a module: the plugin takes the module as the
first `.wasm` under the scan root, so the module is scan data, not a plugin property.

## Environment variables

|Variable|Meaning|
|---|---|
|`AFZ_HOME`|Where `afz install-harness` records its registry: `$AFZ_HOME/autofuzz/harnesses`. Unset, the registry falls back to `$XDG_DATA_HOME/autofuzz/harnesses`, then to `~/.local/share/autofuzz/harnesses`. Point it at a scratch directory to try an install without touching the real one.|

## Run scans from a coding agent

`afz-mcp` speaks the Model Context Protocol on stdio. An agent in Claude Code, Cursor, VS Code Copilot,
Zed, or Codex CLI can then ask "does this change set fuzz clean?" without a shell.

```json
{"mcpServers": {"autofuzz": {"command": "afz-mcp"}}}
```

You configure no directories. Every tool call gives the directory that it works on, so the agent reaches a
different project by calling a tool again, which is how it already uses a shell.

|Tool|What it answers|
|---|---|
|`afz_probe`|Which harness plugin a directory uses, and which targets it would fuzz. Costs no invocations.|
|`afz_scan`|Starts a coverage-guided scan and answers at once with a `jobId`. Pass `git` to scope the scan to a change set.|
|`afz_scan_status`|The state of one scan and its live progress, answered immediately.|
|`await_scan`|Waits until a scan ends. Run it as a background task, and the agent learns the result when the scan finishes. `timeoutSeconds` answers early with the state as it stands then.|
|`afz_scan_cancel`|Ends a running scan early. The scan still writes its result, with `stoppedReason: interrupted`.|
|`afz_findings`|The findings of a finished scan, newest scan first. Each finding names the input file and the reproducer file it came from. Pass `markdown: true` to get `report.md` in the same answer.|

This server drives the engine in process, so it runs one scan at a time: it rejects a second `afz_scan`
while one is running, instead of queueing it.

Keep one caution in mind. The `codebase.command` in a scan project is code that the engine runs, so point
`afz-mcp` only at scan trees you already trust. `afz scan` runs that same command, so the trust question is
the same one either way.

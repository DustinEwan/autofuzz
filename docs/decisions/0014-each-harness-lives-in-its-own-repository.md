# Decision 14 — Each harness lives in its own repository

Status: accepted, 2026-11-13.
Context: `docs/harnesses.md`, `docs/harness-authoring.md`.
Amends: `0001`, whose `packages/harness-js/` path and `harness.command` requirement this record narrows; `README.md`, `docs/cli.md`, `CONTRIBUTING.md`.

## Context

The three shipped harnesses began as directories of the engine repository: `packages/harness-js/`,
`packages/harness-py/`, `crates/afz-wasm/`. Living there produced four costs, each visible in a
different file.

Validation needed the plugin's path from the operator: `afz validate-harness --command <argv…>
--path <dir>`, because nothing recorded where a plugin lived. Every scan project therefore carried a
`codebase.command` naming an in-repo build output, and the conformance projects emitted by the
harnesses baked that path into their rendered `autofuzz.json` — the coupling that made `--command`
necessary. The engine's own tests reached into the harness's tree: `discovery.rs`, `minimize.rs`, and
`oracle_targets.rs` pointed their plugin at `packages/harness-js/test-targets/`, so an engine test
failed when a harness author renamed a directory they had no idea was load-bearing. Engine CI grew
four jobs to build three host runtimes, and `afz-conformance` read `docs/schema/*.json` from the
directory it had been compiled in, so validating a plugin outside an engine checkout panicked.

## Decision 1 — One repository per harness, described by one manifest

`autofuzz-harness-js-ts`, `autofuzz-harness-python`, and `autofuzz-harness-wasm` are separate
repositories, siblings of the engine. Each ships one plugin and describes it in one `afz-harness.json`
at its root: `name`, `install` argv, `build` argv, `command`, and a `conformance` block whose
`emitter` renders the conformance project and whose `deadlineMs` bounds one invocation.

What this excludes:

- **No selector flag exists.** One `afz install-harness <git-url>` installs exactly one plugin,
  because the manifest describes exactly one. There is no `--only`, no `--harness`, and no
  multi-plugin repository to select within.
- **The engine ships no plugin and no plugin path.** `packages/` and `targets/` are gone; the
  workspace builds the engine, the fake plugin, and the CLI and nothing else.

What this buys:

- A harness author's pull request runs that harness's own conformance gate, through the same
  `afz install-harness` a user runs, instead of waiting on an engine CI job.
- Repository names match the `codebase.harness` value each one declares, so a user reading
  `{"harness": "js-ts"}` can name the repository without a lookup table.

## Decision 2 — The registry is an explicit install; the engine still never invents a runtime

`afz install-harness <git-url>` clones, runs `install` then `build`, validates the plugin against the
conformance suite, and records the absolute argv, root, and conformance block under
`$AFZ_HOME/autofuzz/harnesses/<name>/` (falling back to `$XDG_DATA_HOME`, then `~/.local/share`). A
failed install leaves the previous record untouched. `afz harnesses` prints the records.

A scan project with `harness` and no `command` resolves through that record. What this excludes:

- **The engine never guesses a runtime** — the rule `0001` made and this one keeps. A harness that is
  not installed and not named by `command` is an error naming `afz install-harness`, not a search of
  `PATH`.
- **The registry is never written implicitly.** No command downloads a plugin behind a scan; only
  `install-harness` clones, and it validates before it records.

`codebase.command` stays exactly what `0001` made it — the way to name a plugin nobody installed —
and it still wins over the registry where both exist.

## Decision 3 — The conformance project ships inside the harness

`afz validate-harness` names a harness and nothing else: an installed name, or a directory holding
`afz-harness.json`, defaulting to the working directory. The plugin's entrypoint is that manifest's
`command`; the scan project is whatever that manifest's `emitter` renders. `--command` and `--path`
are deleted, because the harness already carries both. `afz install-harness` validates through the
same code path, so an install and `afz validate-harness` cannot disagree about what conforming means.

What this excludes:

- **Validation of an arbitrary project against an arbitrary plugin.** That is `afz probe-plugin` and
  `afz scan`, which keep `--command`.
- **Installing a harness that cannot be validated, silently.** A manifest with no `conformance` block
  installs only with `--no-validate`, and `afz validate-harness` refuses it by name. An emitter is a
  condition of being validated.

## Decision 4 — A fixture lives with the suite that reads it

The engine's three plugin-facing tests keep their fixtures inside the engine, at
`crates/afz-engine/tests/targets/` (`blocked`, `minimize`, `oracles`), and read their plugin only
from the registry. The harness showcase fixtures moved out with their readers: `billing`, `counter`,
`currency`, `legacy`, `logging`, and `rejection` to the js-ts repository, `pricing` to the python
repository, `wasm-probe` to the wasm repository.

What this excludes: **any test reaching into another repository's directory tree.** The plugin binary
is the only thing either side may cross a boundary to use, and it is obtained through the registry.

## Decision 5 — The harness repositories read the engine's docs through `AFZ_DOCS`

The wire-vocabulary contract tests in the js-ts and python repositories read `docs/schema/`,
`docs/architecture.md`, and `docs/harness-authoring.md` from an engine checkout named by `AFZ_DOCS`,
defaulting to the sibling checkout `../autofuzz/docs`.

## Consequences

- Out-of-process plugins mean the engine cannot instrument a target, so the conformance gate rests on
  the plugin's own reports. The answer is a second judgment: every conformance project carries its own
  fixture validator — `conformance/validate.mjs`, `conformance/validate.py`, `afz-wasm-check` — that
  imports only the fixture and the host's standard library, reports the seven fixture-shaped
  properties, and must agree with the plugin's verdicts. A disagreement is a run violation, so a
  harness cannot pass by gutting its validator.
- The committed schemas are compiled into `afz-conformance` with `include_str!`. `docs/schema/` stays
  the published artifact — the xtask drift gate still regenerates and compares it — but validation no
  longer reads it from disk, so `afz` installed anywhere validates anything.
- The engine's `rust` CI job installs all three harnesses from their published URLs; that step is
  itself the wire-rules gate, and the three template goldens
  (`the_js_ts_harness_passes_template_v1`, `the_python_harness_passes_template_v1`,
  `the_wasm_harness_passes_template_v1`) read their records from that install. The wasm harness gained
  an emitter, so it is now judged against the template — four properties realized, six accounted for —
  rather than the wire rules alone.
- Engine CI is red until the three harness repositories are pushed, because the install step clones
  them by URL. The harness suites run in their own repositories; the engine's `cargo test --workspace`
  needs `js-ts` and `python` installed and nothing else from those checkouts.
- The wording of the `docs/architecture.md` method block is a downstream build input: a rename there
  can fail a harness repository's contract tests, which is the intended direction of the dependency —
  docs describe the wire, the contract tests read the docs, the schemas are generated from the types.

## Reconsidered

- **One multi-harness repository.** Rejected: `afz install-harness` would then need a selector flag,
  and one URL would install either all three plugins or an unnamed decision. Three CI files and three
  READMEs are the cost of the command needing no second decision.
- **Resolving `command` lazily at scan time from a checked-out harness tree.** Rejected: it makes a
  scan depend on where a checkout happens to sit, which is how the old `packages/` paths broke for
  anyone scanning from an installed binary. The record holds absolute argv, written once by an
  install that validated the plugin.
- **Keeping the engine's three fixture trees under the js-ts repository because a js plugin reads
  them.** Rejected: the tests own the fixtures, not the plugin. The tests assert engine behaviour —
  wire shape, minimization, oracle verdicts — and any conforming js plugin would satisfy them.

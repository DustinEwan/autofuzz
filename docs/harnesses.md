# Harnesses shipped for autofuzz

Read this page to see how far you can trust each harness shipped for autofuzz, and what each one
still cannot do. Each lives in its own repository:

- [`autofuzz-harness-js-ts`](https://github.com/DustinEwan/autofuzz-harness-js-ts) - TypeScript and JavaScript
- [`autofuzz-harness-python`](https://github.com/DustinEwan/autofuzz-harness-python) - Python
- [`autofuzz-harness-wasm`](https://github.com/DustinEwan/autofuzz-harness-wasm) - WebAssembly
- [`autofuzz-harness-starter`](https://github.com/DustinEwan/autofuzz-harness-starter) - the scaffold you delete into your own plugin
- [`autofuzz-harness-openapi`](https://github.com/DustinEwan/autofuzz-harness-openapi) - OpenAPI 3 services over HTTP

If you are writing your own plugin instead, start with
[`harness-authoring.md`](harness-authoring.md) and the starter repository, which is a conforming
plugin small enough to read in one sitting; this page is about the five harnesses shipped for
autofuzz.

The check is the same command a third-party author runs and a user installs with:
`afz install-harness <git url>` builds the plugin and drives the conformance suite before it records
anything, and `afz validate-harness` runs that suite again on demand — with no argument inside a
harness checkout, or with an installed name anywhere else.

The check has two layers. The first is the 24 wire rules, and every plugin must pass them. The second
is the [conformance template](harness-authoring.md#12-the-conformance-template-versions-and-the-runtime-lifecycle),
which holds ten semantic properties. Seven of those ten are otherwise decided by data the plugin
produced itself — its own skip lists, outcome frames, and coverage digests — so each conformance
project ships a fixture validator, a second program that judges the same properties by reading the
fixture with the plugin out of the path. The two judgments must agree, and a disagreement is a
violation of the run.

|Harness|Repository|Emitter|Validator|Template v1|Gate|
|---|---|---|---|---|---|
|`js-ts`|`autofuzz-harness-js-ts`|`conformance/emit.mjs`|`node --experimental-strip-types conformance/validate.mjs`: 6 pass, 1 not applicable (`private-symbol`)|9 pass, 1 not applicable (`private-symbol`)|the harness repository's own CI: install drives the wire rules, `Assert the report` pins the golden|
|`python`|`autofuzz-harness-python`|`conformance/emit.py`|`python3 conformance/validate.py`: 7 pass, 0 not applicable|10 pass, 0 not applicable|the harness repository's own CI: install drives the wire rules, `Assert the report` pins the golden|
|`wasm`|`autofuzz-harness-wasm`|`src/bin/emit.rs` (`afz-wasm-emit`)|`afz-wasm-check`: 4 pass, 3 not applicable (`guard-refusal`, `private-symbol`, `method-receiver`)|4 pass, 6 not applicable (`guard-refusal`, `private-symbol`, `method-receiver`, `stdout-purity`, `reproducer-script`, `deadline-sleep`)|the harness repository's own CI: install drives the wire rules, `Assert the report` pins the golden|
|`starter`|`autofuzz-harness-starter`|`conformance/emit.sh`|`sh conformance/validate.sh`: 5 pass, 2 not applicable (`method-receiver`, `coverage-digest-live`)|8 pass, 2 not applicable (`method-receiver`, `coverage-digest-live`)|the harness repository's own CI: install drives the wire rules, `Assert the report` pins the golden|
|`openapi`|`autofuzz-harness-openapi`|`conformance/emit.mjs`|`node conformance/validate.mjs`: 5 pass, 2 not applicable (`crash-unguarded`, `coverage-digest-live`)|7 pass, 3 not applicable (`crash-unguarded`, `coverage-digest-live`, `stdout-purity`)|the harness repository's own CI: install drives the wire rules and the service lifecycle, `Assert the report` pins the golden|
|fake plugin|`crates/xtask/src/fake.rs`|none, because it is the negative-control target|none|every rule is violated on demand by a directive|the `a_*` / `an_*` controls|

Conformance is a property of a harness, so its gate lives in the harness repository: each one
installs its own checkout (`afz install-harness "$GITHUB_WORKSPACE"`, which drives the 24 wire
rules before it records anything) and asserts the report against the golden this table records.
The engine's CI installs no published harness; it exercises the engine's side of that same
install machinery - clone, manifest, spawn, handshake, record - against the fake plugin this
repository ships.

The expected results live in each harness repository's CI assertion, so the numbers above are
not a claim you have to re-measure. The starter's `ci.yml` asserts exactly 8 template passes and
the two scaffold absences on both the protocol side and the fixture side; the js-ts, python,
wasm, and openapi repositories assert their own goldens the same way. Every assertion is exact,
so a new gap in a harness fails that repository's CI. To unblock it, someone has to write the
reason for the gap into the assertion.

The harness repositories' CI runs Node 24 and Python 3.14. The `python` plugin's own floor is Python
3.12, so CI does not test the oldest version that the plugin supports.

## The one gap in `js-ts`, and why it is not a shortcut

The `private-symbol` property asks a plugin to withhold one callable: discovery must list it in
`skipped` with reason `not-public`, and must never list it in `targets`. The callable has to be
otherwise fuzzable, so that the only reason for the skip is visibility.

`js-ts` cannot express that. Its discovery reads the TypeScript declarations and enumerates exported
symbols only, so a module-local function is never enumerated in the first place. It cannot appear in
`skipped`, because discovery never saw it. Visibility in TypeScript is an export decision, not a
fuzzability skip, and the manifest says so in those words. The repository's fixture validator reaches
the same verdict from the source, so both judgments agree on the absence.

The Python harness realizes the property, because `ast` finds every def in a module, including one
whose name starts with an underscore. It can look at such a def and choose to report it as skipped.

## What the ten properties ask a harness to prove

Each property describes a behaviour that no JSON Schema can check. Your manifest points each one at a
fixture symbol in your own rendered scan project, or declares it not applicable with a reason. Two of
them check a capability first, so a harness that declares less is not failed for it. The wording
below is the wording in `crates/afz-conformance/template/v1.json`.

|Property|What the suite checks|
|---|---|
|`crash-unguarded`|One named call answers `failed`, and at least one fault frame names a file inside the rendered project rather than the runtime's own library|
|`guard-refusal`|One named call with the manifest's out-of-range input answers `rejected`, with `via` of `guard-site` or `reject-class`|
|`method-receiver`|`prepare` answers `ok: true` for a named method on a class the plugin can build|
|`unconstructible-receiver`|A named method whose receiver cannot be built lands in `skipped` with reason `unconstructible-parameter`, and its `blockedBy.type` names the type the manifest declares|
|`private-symbol`|A callable the language's visibility convention excludes is listed in `skipped` with reason `not-public`, and never in `targets`|
|`unrepresentable-symbol`|A callable whose signature the type vocabulary cannot express is listed in `skipped` with reason `unsupported-signature`|
|`coverage-digest-live`|Two different inputs to one named multi-line symbol produce two non-empty digests that differ. This is the property that catches a probe armed once and never reset. Requires `coverage`|
|`reproducer-script`|`reproducer` for a named crashing symbol answers a non-empty script with a valid extension, and the script text names the fixture file the fault came from. Requires `reproducer`|
|`deadline-sleep`|A named slow symbol at the suite's deadline answers `failed` with `via` of `deadline-cut`, or the engine kills the plugin and the `process-alive` rule still holds. Answering `ok` inside the deadline fails the property, because it means the fixture's sleep was shorter than the deadline|
|`stdout-purity`|After the suite drives a named symbol that writes noise to stdout, the same connection still answers a `handshake` with a well-formed response|

Seven of these — all but `stdout-purity`, `reproducer-script`, and `deadline-sleep` — are
fixture-shaped: the project's own validator reports the same seven ids, and the suite fails the run
when a validator's verdict contradicts the plugin's.

## Running one by hand

Run this when you are writing a plugin and want the report without the CI wrapper. Inside the harness
checkout, with no argument at all:

```sh
cd autofuzz-harness-python
afz validate-harness
```

The command runs the manifest's `build` argv, renders the conformance project with the repository's
own emitter into a temporary directory, and drives the suite against the plugin the manifest names.
Nothing is installed and nothing is recorded. After an install, the installed name works from
anywhere:

```sh
afz validate-harness python
```

Four of the five rendered projects carry no `lifecycle` block, because those harnesses speak stdio
and start no service of their own. The `openapi` project ships one: its manifest launches
`node service/server.mjs --port {{port}}` inside the rendered project, and the suite runs that
lifecycle before the first handshake.

If your harness talks to a REST service, add that block. Guide section 12 describes it. The suite
allocates a port, substitutes it for the `{{port}}` placeholder in the launch and teardown argv it
starts from the project directory, and exports the same number as `AFZ_PORT` in the plugin's
environment, so `harness.options` strings like a `baseUrl` can carry `{{port}}` too. It starts the
service, waits for its health URL, drives one probe target that must echo a nonce back, and stops
it. When that fails, the report prints the argv of your package and the directory the process
failed to start in, so you know which command the suite ran.

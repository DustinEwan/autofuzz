# Harnesses maintained by this repository

Every harness here is validated by the same command a third-party author runs:
`afz validate-harness` (in-repo: `cargo run -p afz-xtask -- check-harness`). Validation has two
layers: the 23 wire rules, which every plugin must pass, and the [conformance
template](harness-authoring.md#12-the-conformance-template-versions-and-the-runtime-lifecycle),
whose ten semantic properties a plugin answers through a manifest its own package emits.

|Harness|Package|Emitter|Template v1|CI gate|
|---|---|---|---|---|
|`js-ts`|`packages/harness-js/`|`conformance/emit.mjs`|9 pass, 1 not applicable (`private-symbol`)|`the_js_ts_harness_passes`, `the_js_ts_harness_passes_template_v1`|
|`python`|`packages/harness-py/`|`conformance/emit.py`|10 pass, 0 not applicable|`the_python_harness_passes`, `the_python_harness_passes_template_v1`|
|`wasm`|`crates/afz-wasm/`|none yet|not emitted|`the_wasm_harness_passes` (wire rules only; `#[ignore]`d, needs a release build and the wasm32 target)|
|fake plugin|`crates/xtask/src/fake.rs`|none — it is the negative-control target|every rule is violated on demand by a directive|the `a_*` / `an_*` controls|

The goldens live in the gate tests themselves: `the_python_harness_passes_template_v1` asserts
`(10, 0, 0)`, and `the_js_ts_harness_passes_template_v1` asserts that the only not-applicable
property is `private-symbol` and that the other nine pass. Both assertions are exact, so a new
gap in either harness fails CI until someone writes the reason down in the test.

The js-ts gap is a runtime fact, not a shortcut: `private-symbol` asks the plugin to keep a
fuzzable symbol out of the inventory that a caller can still name, and the js-ts module loader
exposes every export it discovers. The Python harness realizes the property because `ast`
discovery can see an underscore-prefixed def and withhold it.

## Running one by hand

The emitter renders only the scan project — `autofuzz.json`, a small source tree, and
`afz-conformance-manifest.json`. The plugin stays in the package and is named with `--command`:

```sh
python3 packages/harness-py/conformance/emit.py --out /tmp/rendered-py
cargo run -p afz-cli -- validate-harness --path /tmp/rendered-py \
    --command python3 packages/harness-py/src/server.py --deadline 1500

node packages/harness-js/conformance/emit.mjs --out /tmp/rendered-js
cargo run -p afz-cli -- validate-harness --path /tmp/rendered-js \
    --command node packages/harness-js/dist/server.js --deadline 1500
```

The rendered project carries no `lifecycle` block: both harnesses speak stdio and own no service.
A REST-backed harness adds one (guide section 12), and then the suite starts the package's own
service on a port it allocated, waits for its health URL, and stops it again; every failure there
names the package's argv, not the suite's.

# Recommended harnesses

The engine does not ship a harness. Each one lives in its own repository and connects autofuzz to one
language, runtime, or API.

These are the ones we recommend:

| Harness | Language or target | Reads | Coverage | Known gaps |
| --- | --- | --- | --- | --- |
| [`autofuzz-harness-js-ts`](https://github.com/DustinEwan/autofuzz-harness-js-ts) | TypeScript, JavaScript | package declarations | V8 block ranges | cannot list a private symbol |
| [`autofuzz-harness-python`](https://github.com/DustinEwan/autofuzz-harness-python) | Python | source files | `sys.monitoring` line events | none |
| [`autofuzz-harness-wasm`](https://github.com/DustinEwan/autofuzz-harness-wasm) | WebAssembly | module exports | instrumented module | no guard refusal, private symbol, method receiver, reproducer, or deadline cut |
| [`autofuzz-harness-openapi`](https://github.com/DustinEwan/autofuzz-harness-openapi) | HTTP, OpenAPI 3 | API document | none | no crash frame, per-call coverage, or stdout purity |
| [`autofuzz-harness-cpp`](https://github.com/DustinEwan/autofuzz-harness-cpp) | C++ | headers and sources | LLVM profile counters, per function | coverage is per function, not per line |
| [`autofuzz-harness-starter`](https://github.com/DustinEwan/autofuzz-harness-starter) | your own | the scaffold | none | no receiver or live coverage |

They cover the common languages and services. You can use another harness: point a scan project at its plugin
with `harness.command`, or install it with `afz install-harness` so a scan project can use the name.

Install one with `afz install-harness <git-url>`. The command clones the repository, builds the
plugin, and runs the conformance suite before it records anything. `afz harnesses` lists what is
installed. To check a checkout, run `afz validate-harness` inside it.

## `js-ts`

`js-ts` reads the TypeScript declarations of a package and enumerates its exported functions and
methods. It does not execute the package during discovery. Each invocation runs in a fresh `vm`
realm built from text that the plugin transpiled itself, so module-level state resets per call and
one call remains a complete reproducer.

The plugin requires Node 24 or newer and runs with `--experimental-vm-modules`. It reports coverage
from V8 block ranges, can build receiver state, and can render a reproducer script.

The one gap is visibility. The conformance template asks a plugin to list a private symbol in
`skipped` with reason `not-public`. `js-ts` cannot do that, because its discovery reads exported
symbols only. A module-local function is never enumerated, so it cannot appear in `skipped`. The
manifest declares the property not applicable, and the fixture validator reaches the same verdict
from the source.

## `python`

`python` parses the modules in the scan project with `ast` and imports nothing during discovery. Each
invocation calls the function or constructs the class with no arguments. Coverage comes from
`sys.monitoring` line events, so the plugin requires Python 3.12 or newer. The plugin uses only the
standard library.

The harness reports line numbers as the unit of a coverage range. It does not report memory use, so
the `memoryGrowth` oracle stays inert for its targets instead of reporting a guessed number.

## `wasm`

`wasm` reads the exported functions of a core WebAssembly module. The engine builds arguments from
the same type description as the other harnesses, and the plugin calls the export directly. One
wasmtime instance serves each worker.

The harness instruments the module that it is handed. It writes one counter byte per function entry
and per structured block head, in a memory of its own that it exports as `__afz_counters`. It reads
the table back after the call, including after a trap, and re-instantiates after a failed outcome
because a Rust panic or a wasm trap ends the instance.

Rust checks integer overflow only when `overflow-checks = true`. That is the default in debug
profiles and off in release, so the fuzz build must enable it or an entire bug class reports as
`ok`.

The harness needs the `wasm32-unknown-unknown` target to build probe modules. It needs no C or C++
toolchain, no Binaryen, no wasi-sdk, and no wasm-opt.

## `openapi`

`openapi` reads an OpenAPI 3 document and drives a live service over HTTP. Its targets are
operations, not functions, and its bugs are status codes or contract violations rather than
exceptions. It declares no coverage, because an uninstrumented service cannot provide per-call edges.

The plugin rejects a mutating method unless `harness.options.allowMutating` lists that operation.
That policy lives in the plugin, not in the engine.

For validation, the repository ships a `lifecycle` block. The suite starts the service on an
allocated port, waits for its health URL, drives one probe target that must echo a nonce, and stops
the service. The same port reaches the plugin as `AFZ_PORT`.

## `starter`

`starter` is the scaffold. It is a conforming plugin that you edit into your own. It has no real
language behind it, so it declares no coverage and no receiver state.

The scaffold also shows the shape of a harness repository: `afz-harness.json`, the plugin, a
conformance emitter and validator, and CI that asserts the report.

## What the gaps mean

A template gap is a limit of the harness, not a failure of your package. The scan still runs.

- `js-ts` cannot report a private symbol, so the inventory contains only exported functions and methods.
- `wasm` cannot demonstrate a guard refusal or a method receiver, because a core wasm module has no
  such concepts.
- `openapi` cannot produce an in-process crash frame or a per-call coverage digest, because the
  service runs in its own process.
- `starter` has no coverage to digest and no receiver to build.

If you add a harness, give each gap a reason in your manifest. A new gap fails CI until someone
writes the reason down.

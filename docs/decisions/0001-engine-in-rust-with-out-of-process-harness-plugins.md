# 0001 — Engine in Rust, harness plugins out of process

Status: accepted, 2026-11-11.
Context: `docs/architecture.md`.

## Decision 1 — Harness plugins run out of process over JSON-RPC on stdio

A harness plugin is a separate operating-system process. The engine spawns it from
`harness.command`, writes one JSON-RPC 2.0 request per line to its stdin, and reads one
response per line from its stdout. One process serves one worker; a scan with `workers: 8`
runs eight plugin processes.

What this excludes:

- **In-process plugin loading is not supported.** A plugin linked into the engine would make
  a single C-level fault, or a target that corrupts memory, end the whole scan. A plugin that
  hangs would hold the engine's thread. There is no in-process plugin host and no plan for one.
- **Any other transport is out of scope for v1.** No HTTP control channel, no unix socket, no
  shared-memory framing. Stdio carries the request, and the target invocation itself dominates
  the cost, so transport overhead is not a design driver.

What this buys:

- A plugin written in Rust, Python, or Node satisfies the same contract, because the contract
  is JSON on a pipe.
- A hung or crashed target kills the plugin process, and the engine turns that into an
  `Outcome` with `via: "plugin-lost"`, `deadline`, or `processExit`. The scan continues.
- Parallelism needs no language-specific threading story: the engine starts N processes.

## Decision 2 — The engine is Rust; the `js-ts` harness is a separate Node 24 process

The engine (`afz-protocol`, `afz-engine`, `afz-cli`, `xtask`) is Rust. The first harness plugin
(`packages/harness-js`) is Node 24 with TypeScript, and it is a different program in a different
language that the engine only ever talks to over the pipe.

What this excludes:

- **TypeScript knowledge never enters the engine.** The engine sees `TypeNode`, `ValueTree`, and
  the three `Outcome` kinds. The rule that maps a JavaScript `throw` to `rejected` or `failed`
  lives in `packages/harness-js/src/classify.ts`, and the engine has no code path that inspects a
  JS error, a Rust `Result`, a Go `error`, or an HTTP status class.
- **No Node runtime ships with the engine binary.** `afz` is one static binary. Running a
  TypeScript scan requires Node 24 on the machine, discovered through `harness.command`; running
  a wasm scan requires no external runtime at all.

What this buys:

- Supervision primitives are direct: `waitpid`, signals, kill timers, and resource limits for a
  plugin process that must die on a deadline.
- The wasm harness uses wasmtime epoch interruption and linear-memory reads as first-class
  operations, and the post-hoc instrumentation pass uses `wasmparser` and `wasm-encoder`.
- The wire contract is typed Rust structs, and `schemars` emits the committed JSON Schema from
  the same types the engine compiles against, so protocol drift is a compile error.

The accepted cost: a slower edit-compile loop in generation, mutation, and scheduling policy.
Those modules are small and pure, and their tests are golden-file comparisons that fail on one
`cargo test`.

## Consequences

- Every protocol type is defined once, in `crates/afz-protocol`, and serialized as JSON. The
  Node plugin mirrors those shapes by hand and is checked against the committed schema.
- `discover` must not execute target code unless the plugin declares
  `discoverRequiresExecution: true`, because importing a user module runs user code, and that
  execution belongs in the plugin process, not the engine.
- Every plugin invocation carries an `invocationId`, because responses from parallel plugin
  processes arrive out of order and must be attributable.

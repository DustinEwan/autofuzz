# Decision 3 - wasm coverage by rewriting the module, not by rebuilding it

Status: accepted. 2026-11-11.
Amends: `docs/architecture.md` "Harness plugins", the `wasm` row, which names
`-fsanitize-coverage=trace-pc-guard` compiled into the module with the callbacks defined by the host,
and the diagram label "generated driver".

## Context

The `wasm` harness exists to show that a second target class needs no change to the engine. The
design's first choice was compile-time pcguard: the target is rebuilt with a sanitizer that calls
`__sanitizer_cov_trace_pc_guard` per edge, and the host supplies that import and reads the guard
table.

Three facts about that path made it the wrong instrument for this job:

- It changes the artifact under test. A scan of `dist/app.wasm` would have to be a scan of
  `dist/app.instr.wasm`, built by a toolchain the target's own team may not use, and a bug that only
  appears in the shipped artifact would be missed.
- It needs a toolchain per producer. A module emitted by Rust, by Emscripten, by AssemblyScript, or
  by a bundler cannot all be asked to re-emit with the same flag, and the harness would depend on a native toolchain that it does not
  otherwise need and cannot assume a target CI provides.
- The callbacks are host imports, so a module that imports nothing gains them, and a scan of a
  module that already imports `env.__sanitizer_cov_trace_pc_guard` for another reason reports edges
  the harness did not record.

## Decision

1. **The harness rewrites the module it is handed.** `crates/afz-wasm/src/instrument.rs` walks the
   module with `wasmparser`, appends one memory of its own holding one byte per edge, and inserts a
   guard - `i32.const <edge>; i32.store <counters>` - at every function entry and every structured
   block head. Everything the harness does not touch is copied through `wasm-encoder`'s reencoder, so
   an instruction the harness has never seen survives unchanged. The counters memory the harness
   adds is exported under a `__`-prefixed name (`__afz_counters`), the prefix discovery already
   skips, so it never appears as a target.
2. **A block head is guarded as a range, not a point.** Each guard records its byte offset and its
   length, so `coverageMap` names the span the edge stands for and a report can point at the block
   rather than at an address.
3. **No driver is compiled in.** The plugin calls the export directly with the arguments the engine
   materialized from the same type IR the `js-ts` plugin uses, which is the whole point of the
   generalization proof: the target's signature is the only type information involved.
4. **Coverage comes from reading linear memory after the call**, not from a callback during it. The
   harness zeroes the counters before the call, reads them after, and reports the set of edges whose
   byte is non-zero. A trapped call's edges are read before the instance is discarded, which is what
   the design already requires of a faulting invocation.

5. **A fault's frames name the target's code, not the runtime's.** A Rust wasm module aborts through
   one trampoline instruction, so every panic in the module ends at the same `module_offset`, and an
   identity keyed on the raw backtrace folds different bugs into one finding: the probe's index read
   and its multiply overflow both reported `7188, 571, 1590`. `classify()` drops the frames the name
   section attributes to the runtime - the same `is_plumbing` rule that keeps the allocator and the
   panic path out of the coverage map - and keeps the target's own frames, falling back to the
   unfiltered list when every frame is runtime code so that a trap is never left unnamed. The panic
   path stays in the fault message, where a reader still sees it.

## Consequences

- Instrumentation runs once per plugin process, before the first request, and the module is validated
  before and after the rewrite: an instrumented module that fails to validate is a harness bug and is
  reported as one, not as a target fault.
- Coverage granularity is a block head, coarser than pcguard's per-basic-block edge. A function with
  no structured block yields one edge, so a target written that way gives the scheduler little to
  climb. Accepted: the alternative costs the artifact's identity.
- The guard costs three instructions per block head: two `i32.const` and one `i32.store8`. One plugin
  process answered 12,000 invocations a second on inputs that return promptly and 880 a second on the
  mix a scan generates, so the guards are not what limits a scan. End-to-end throughput is the
  target's runtime distribution against the deadline: the same probe scan completed 23,963 invocations
  in 30 s at a 50 ms deadline and 1,006 at the default 2000 ms. See Decision 4 and
  `execution.deadlineCuts`.
- A module that already exports `__afz_counters` is refused with a named error rather than
  silently shadowed.
- pcguard remains available to a target that wants finer edges: the plugin would read the same guard
  table shape. Nothing in the engine would change either way, which was the point of the step.

## Reconsidered

- **The `binaryen` crate.** Its build needs a C++ toolchain, which the engine deliberately does not, and it
  would still rewrite the artifact. Rejected for the same reason as pcguard.
- **Compiling a driver into the probe and calling that.** It would let the probe take structured
  arguments instead of one `i32`, at the cost of testing the driver rather than the module. Rejected.
- **Reading coverage through an imported callback.** It needs the module to import a symbol, which a
  plain `-t wasm32-unknown-unknown` build of a Rust cdylib does not otherwise do, and it puts the
  harness's state inside the guest. The post-call memory read needs neither.

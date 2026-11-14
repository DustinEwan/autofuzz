# Writing a harness plugin

A harness plugin is the part of autofuzz that knows one language. The engine owns the fuzzing loop
and the data; the plugin owns everything that touches a specific language or runtime
([`docs/architecture.md`](architecture.md#the-cut)). A plugin runs out of process and speaks JSON-RPC
over its own stdin and stdout. It may be written in any language that can read a line from stdin
([`docs/architecture.md`](architecture.md#plugin-transport)). In-process plugins are not supported.

This guide states the contract, the measured cost of every per-invocation operation, and the traps
that the two shipped harnesses already hit. Every number here has a citation. Where the citation is
a file under `docs/`, the number is a measurement of a real scan; where it is a file under
`crates/`, the number is a constant the engine enforces.

## 1. What a harness owns

The engine plans invocations, keeps the corpus, schedules workers, enforces deadlines, classifies
nothing that it can read from the plugin's answer, minimizes, and writes the scan result. The plugin:

- enumerates callables (`discover`) and reports the ones that it cannot reach with a reason;
- builds the receiver and arguments for one call (`prepare`);
- runs one call and reduces it to one `Outcome` (`invoke`);
- optionally maps coverage edge ids to source ranges (`coverageMap`), shrinks an input in target
  terms (`minimize`), renders a standalone replay script for a confirmed finding
  (`reproducer`), and proves the environment its targets live in is up (`ready`).

What `rejected` and `failed` mean for a given language is your plugin's call. The engine never inspects a
JavaScript exception, a Rust `Result`, a Go `error`, or an HTTP status class.

## 2. Start here

```sh
# 1. Describe your plugin in `afz-harness.json`: name, build argv, command, conformance emitter.
# 2. In your package directory, with the `afz` you ship:
afz validate-harness
# 3. Look at the inventory it discovers, the way the engine will.
afz probe-plugin --path <scan project>
```

`validate-harness` names a harness and nothing else. The plugin's entrypoint is your
`afz-harness.json` `command`, and the scan project is whatever your `conformance.emitter` renders
into a temporary directory: the command takes no `--command` and no `--path`, because your manifest
already carries both. Run with no argument it validates the directory it is run in; run elsewhere,
it takes an installed harness name or a path to a harness directory. It runs your `build` argv
before it validates - never your `install` argv, which fetches dependencies and belongs to
`afz install-harness` alone - so a package with a dependency step runs that step once by hand
(`npm ci`, say) rather than on every validation.

The suite drives three plugin processes over the protocol, validates every response against the
committed JSON Schema, and prints one line per rule. It exits **1** when a rule is violated and
**0** when only notes are present. A `note` line is advice about your harness's cost profile, not a
verdict: the suite cannot fail a correct plugin for a property that is also true of a correct plugin
on different code. Add `--json` for the machine-readable report, and `--targets <n>` /
`--deadline <ms>` to bound the run.

`afz probe-plugin` still takes `--command <argv…>`, and its flag parser reads any token matching a
`probe-plugin` flag (`--path`, `--json`) as a flag wherever it sits, so a plugin argv that itself
contains such a token must use a spelling the flag parser does not recognize.

Because your emitter renders the project, your rendered project carries an
`afz-conformance-manifest.json`, and the same command validates your plugin against the conformance
template and adds a `template` section to the report (section 12). Inside the engine repository the
identical suite also runs as `cargo run -p afz-xtask -- check-harness`, which defaults to building
and checking `crates/xtask/src/fake.rs`, the repository's own reference plugin; there, everything
after `--command` is passed through verbatim, so the suite's own flags must come *before* it.

The wire contract itself never changed: the seven methods, the framing, and the error codes are
section 3 and section 4, and a plugin that passed them yesterday passes them today. What
`validate-harness` adds on top is the template: ten semantic properties that schema validation
cannot express, judged against fixtures that your own package renders (section 12).

## 3. The seven methods

Before your plugin can answer a method, it has to spell that method's name exactly, because the engine
matches on the string. These seven are the whole contract (`crates/afz-protocol/src/rpc.rs`, `Method::name`).

|Method|Request `params`|Required response|
|---|---|---|
|`handshake`|**omitted entirely**|`protocolVersion`, `name`, `version`, `targetClass`, `capabilities`, `languages`, `discoverRequiresExecution`|
|`discover`|`root`, optional `include`, `exclude`, `configPath`|`targets[]` of `TargetDescriptor`, `skipped[]` of `SkippedTarget`|
|`prepare`|`targetId`, `values[]`|`{"ok":true,"materialized":{"handle","summary"}}` **or** `{"ok":false,"reason":<SkipReason>}`|
|`invoke`|`invocationId`, `targetId`, `values[]`, `deadlineMs`, `collectCoverage`|one `Outcome`: `{"kind":"ok"}`, `{"kind":"rejected"}`, or `{"kind":"failed"}`|
|`minimize`|`targetId`, `values[]`, `fault`, `budgetMs`|`values[]`, `steps`|
|`reproducer`|`targetId`, `values[]`|`extension`, `script`|
|`coverageMap`|**omitted entirely**|`entries[]` of `{edge, url, start, end}`|
|`ready`|**omitted entirely**|`{"ready":true}` or `{"ready":false,"detail":"<what is down>"}`|

`include` and `exclude` are instructions for the plugin, not for the engine: the plugin matches
them itself, against paths relative to `root` with `/` separators, and an empty or absent
`include` means the plugin's own default. The engine forwards what the scan project declared and
appends the scan output directory to `exclude`, so `exclude` can carry patterns the user never
wrote.

`CoverageEntry.start` and `end` have no fixed unit: `js-ts` reports byte offsets into the source
text and `python` reports 1-based line numbers. State which unit your harness uses, because
`coverage.json` is written from these numbers and a reader comparing two harnesses has no other
way to know.

`docs/schema/<revision>/` - today `docs/schema/afz-1/` - is the authority for every field. It
is generated from `crates/afz-protocol/src/` by `cargo run -p afz-xtask -- schema`, and
`crates/xtask/tests/schema.rs` fails the test suite when the committed files differ from the
Rust types. A plugin written against those files cannot drift from the engine without one of
the two failing loudly. Without an engine checkout, `afz export-schema [revision] [--out DIR]`
writes the same bytes from the binary that will validate your plugin, which is what a CUE
author imports and what an editor's `$schema` points at
([decision 0016](decisions/0016-the-protocol-speaks-a-version-set.md)).

Literals you must match:

- `PROTOCOL_VERSION` is `"afz/1"` (`crates/afz-protocol/src/targets.rs`). The engine accepts
  any revision in `SUPPORTED_VERSIONS` - today that set is just this one - and refuses the
  handshake otherwise.
- `jsonrpc` is the literal `"2.0"`.
- `id` is a `u64` assigned per plugin process, starting at 1 (`PluginClient::spawn` sets
  `next_id: AtomicU64::new(1)`). Echo it back on error frames too.
- `params` is **omitted**, not `null`, for `handshake`, `coverageMap`, and `ready` (`Request::empty`). Do not
  require it. `src/main.rs` in `autofuzz-harness-wasm` reads a missing `params` as an empty object for the same
  reason.
- Error codes: `-32700` parse error, `-32600` invalid request, `-32601` method not found, `-32000`
  plugin error (`crates/afz-protocol/src/rpc.rs`).
- `PrepareResp` is `additionalProperties: false` on both shapes. `ok` is a JSON boolean, which is
  why the type has hand-written `Serialize`/`Deserialize` in `methods.rs` rather than a serde tag.
- `SkipReason` has six kebab-case values: `unconstructible-parameter`, `discovery-error`,
  `not-public`, `excluded`, `unsupported-signature`, `target-disabled`.
- `Materialization` has three: `constructible`, `fixture-required`, `opaque`.

## 4. Transport rules, and what the engine does with a mistake

Each rule below traces to a real failure in a shipped harness. The engine cannot ask a plugin to be
careful, so every one of these is something that your plugin has to do by itself.

- **One JSON message per line, `\n`-terminated.** There is no `Content-Length` framing
  (`write_line`, `read_line` in `crates/afz-protocol/src/rpc.rs`).
- **A blank line is EOF to the engine.** `read_line` returns `Ok(None)` for a line that is empty
  after trimming, and the reader thread then records `LostReason::Eof`.
- **An unparseable line is dropped without an error** (`spawn_reader` in
  `crates/afz-engine/src/rpc/client.rs`: `let Ok(resp) = parse_response(&line) else { continue }`).
  The waiting request therefore stalls until its timeout, the engine kills the process, and the
  invocation is filed as a plugin loss. Answer malformed input with an error frame; never with
  silence.
- **A response whose `id` matches no in-flight request is discarded.** A plugin that answers with
  `id: 0` because it could not read the request's id has answered nothing.
- **stdout is the protocol channel and nothing else.** All diagnostics go to stderr. The engine
  drains stderr into a 4096-line ring per process (`spawn_stderr_drain`) and prints at most 200
  distinct lines per report (`Supervisor::take_stderr`, `const CAP: usize = 200`), as
  `plugin said: {line}`. The `yaml` example's `debug` export, whose body was `console.log(...)`,
  killed its own plugin with exit code 1 exactly this way
  ([`docs/examples/yaml.md`](examples/yaml.md):74-78).
- **Survive a failed stdout write.** A write to a pipe whose reader is gone is an `'error'` event
  that ends a Node process; `src/server.ts` in `autofuzz-harness-js-ts` installs a handler that reports to
  stderr and stays up. The engine notices the silence and restarts the process, instead of filing a
  `processExit` finding against whichever target the failed write interrupted.

## 5. The connection lifecycle, exactly

The engine starts plugin processes at three places, and they do not all receive the same requests.

|Connection|Spawned by|Receives|
|---|---|---|
|probe|`scan::run`|`handshake`, `discover`, and `ready` when the handshake declares `readiness`|
|one per worker|`Supervisor::new` → `Worker::start`|`discover` only, then `prepare`, `invoke`, `coverageMap`. **Never `handshake` or `ready`.**|
|minimizer pool|`Minimizer::grow`, `run_trial`|`discover` only, then `prepare`, `invoke`|

Consequences you must design for:

- `discover` goes to **every** connection. `prepare` resolves a `targetId` only against a `discover`
  that ran in that same process, and no protocol method carries an inventory from one process to
  another. Every connection the engine prepares on is brought up by
  `crates/afz-engine/src/rpc/client.rs` `spawn_discovered`, which spawns and then discovers.
- Capability gating reads the **probe's** handshake only. A worker connection never receives a
  `handshake`, so do not make any other method depend on having received one.
- `prepare` is asked **once per target per process**, lazily, on the first invocation of that target.
  A `{"ok":false,…}` answer removes the target from the prepared set, so the engine asks again on the
  next invocation for that target (`Worker::invoke`).
- `coverageMap` is asked of every worker at each `SAMPLE_EVERY` (5000) invocations and once at
  `finish` — not per batch (`Scanner::refresh_coverage`).
- A lost process is restarted, re-discovered, and its prepared set is cleared (`Worker::restart`).
  Assume your process is ephemeral: a replacement process starts with nothing carried over.
- `DISABLE_AFTER` is 50: 50 consecutive restarts attributed to one target disable that target for
  the rest of the scan.
- One `invoke` is in flight per process at a time.

The timeouts that the engine applies, and what they bound:

|Method|Timeout|Constant|
|---|---|---|
|`handshake`|30 s|`PluginClient::handshake`|
|`discover`|120 s|`supervise::DISCOVERY_TIMEOUT`|
|`prepare`|120 s|`scan::PREPARE_TIMEOUT`|
|`invoke`|`deadlineMs` + 250 ms|`supervise::KILL_GRACE`|
|`minimize` trial|deadline + 10 s|`scan::MINIMIZE_SLACK`|
|`reproducer`|10 s|`scan::REPRODUCER_TIMEOUT`|
|`coverageMap`|10 s|`scan::COVERAGE_TIMEOUT`|
|`ready`|5 s|`scan::READY_TIMEOUT`|

`prepare` is never held to the invocation deadline. Preparing imports the target's module graph,
which is the harness's work and not the target's; holding it to the deadline killed a plugin on
every first call into a package with a deep graph, and the scan reported those kills as
`processExit` findings (`Worker::invoke`).

## 6. Capabilities: what the engine reads today

A capability is a promise that your handshake makes, and the engine acts on only three of the seven. Knowing which
three keeps you from building a method nobody calls, and from claiming one you do not answer.

`coverage` changes how the engine schedules: it turns `collectCoverage` on for every job and turns the
`coverageMap` walk on.

`reproducer` controls one artifact. After a finding's minimized input is confirmed to still
reproduce, the engine calls `reproducer` once for that finding and stores the script under
`repros/` with the plugin's extension (`scan::run_finding`, `scan::valid_extension`). A plugin
that omits the capability, errors the call, or answers a script over 1 MiB gets findings that
ship with only the minimized input JSON.

A finding that an earlier scan already reported ships without a script too: the engine does not
shrink or render what a previous run wrote up. A rerun over a warm `seen.json` therefore prints
`reproducer not emitted` for every finding — that is suppression working, not a broken plugin.

`readiness` gates the scan itself. The engine calls `ready` once on the probe connection,
after `discover` and before any worker spawns, and a `{"ready":false}` answer stops the scan
with the plugin's own `detail` as the first line (`scan::run`). Readiness proves
reachability and dialogue, never health: a service that answers 500 on every route is
ready, because the scan can talk to it and the 500s are findings. Only a transport-level
failure — refused connection, timeout, DNS error — is not-ready. If your targets live in
the tree discovery already read, leave the capability out: there is nothing to prove.

`stateful`, `sideEffects`, `concurrent`, and `minimize` are recorded in `scan.json` under
`target.harness.capabilities` and change nothing in the engine today.

`ok.memoryBytes` is reported by the plugin and taken at face value by the engine
(`docs/architecture.md#oracles-and-defaults`). A harness that omits it — `python` does, because
`tracemalloc` would tax every allocation for an oracle that most scans never turn on — leaves the
`memoryGrowth` oracle inert for its targets, which is the honest outcome; a guessed number is not.

The engine **never sends `minimize`**. `PluginClient::minimize` has no caller: minimization is
engine-side delta debugging over repeated `invoke` calls (`scan::run_finding` →
`minimize::minimize` → `Minimizer::run_batch`). That is why every shipped harness
answers the method with an error (`src/server.ts` `minimize()` in `autofuzz-harness-js-ts`,
`src/main.rs` `Method::Minimize` in `autofuzz-harness-wasm`, and `src/server.py` in
`autofuzz-harness-python`, whose dispatch table has no `minimize` handler at all).

So: implementing `minimize` buys nothing until the engine calls it. Declaring a capability you do
not honor is what `coverage-declared` fails. If you do not implement a method, leave its capability
out of the handshake and answer it with `-32601` or `-32600`.

`reproducer` is the opposite case: the engine **does** send it, once per finding, to every plugin
that declares the capability, after the minimized input reproduces. A declared `reproducer` that
answers unreadably strands every finding of that plugin without the artifact that a triager runs, and
`check-harness` fails that as `reproducer-schema`.

A schema-valid script is not a proven script. The engine stores your bytes without reading
them, so the only check that a renderer works is executing its output: every shipped harness runs
its own rendered reproducer in its own tests
(`test/repro_test.py` in `autofuzz-harness-python`; `crates/afz-engine/tests/minimize.rs` launches the
js-ts `.mjs` under node and requires the same fault class on the same source line). Write the
script for the triager who has never seen the scan: notes go on their own lines, never inside
the call — a `#` comment there hides the closing bracket — and a deadline finding reproduces
by *not returning*, because `reproducer` carries no fault context and the wall clock is the
fault; say in the header when to stop the script, or the triager waits forever on one that has
already succeeded.

## 7. Classification

This is the one judgement that the engine never makes. It reads `ok`, `rejected`, and `failed` and
nothing else, so what those three words mean for your language is yours to settle.

`Outcome` has three kinds. `via` records the rule that produced it, and the engine puts it on the
finding verbatim. `guardSites` on a `TargetDescriptor` carries the source lines that the target itself
throws from; the classification rule in the plugin reads it and the engine never reads that field
(`crates/afz-protocol/src/targets.rs`).

**A plugin never answers `invoke` with an RPC error to report a fault.** An error frame is
indistinguishable from a plugin that died: `PluginClient::request` maps it to a `LostReason`, the
supervisor files the invocation with `via: "plugin-lost"`, and the worker process is restarted. The
engine's own `via` strings for a lost plugin are `deadline`, `processExit`, `plugin-killed`, and
`plugin-lost` (`LostReason::via`). A `yaml` run filed two findings for one plugin death
([`docs/examples/yaml.md`](examples/yaml.md):46-52): a plugin that dies produces findings that are
not the target's.

A fault is `Outcome::Failed` with a `fault` of `{class, message, frames}`. A refusal that the target
declared — a thrown `TypeError` on a validated parameter, a returned error value — is
`Outcome::Rejected`. Your plugin makes that call; the engine only reads `kind` and `via`.

## 8. Performance contract

All target-wide work belongs at `discover` and `prepare`, once per plugin process. `invoke` is a few
milliseconds that answers inside `deadlineMs` or kills its own call.

|Operation|Measured|Source|
|---|---|---|
|`invoke` through a warm `js-ts` process|4.15 ms median; 92.42 ms per call before the work was moved off it|`docs/decisions/0006-fresh-realm-per-invocation-in-one-warm-isolate.md:48`|
|the coverage digest alone|2.5 ms of a 3.8 ms call; 4.07 ms with the digest against 1.60 ms without|`docs/decisions/0011-plugin-pool-runs-at-the-machines-ceiling.md:39`, `:20`|
|coverage probe on a real package|74.6 ms off against 85.1 ms on: 10.5 ms, 12 % of the call|`docs/examples/yaml.md:104-105`|
|worker thread creation|24.2 ms|`docs/decisions/0006-…:11`|
|per-call coverage start/stop cycle|8–20 ms, and it must finish before the target compiles|`docs/decisions/0006-…:34`, `:56`|
|fresh realm, link and evaluate a two-module graph|0.57 ms|`docs/decisions/0006-…:20`|
|transpiling 12 target files, once per process|125.5 ms, then free|`docs/decisions/0006-…:21`|
|importing the compiler into the invoke worker|5.4 ms per thread, plus profiler deoptimization|`docs/decisions/0006-…:48`|
|`prepare` of a real target, fresh process against primed|517 ms against 92 ms|`docs/examples/yaml.md:58-59`|
|scan setup (`setup_ms` in `engine.log`)|74 568 ms, 43 % of a 173 s run; 9.5 s of a 51 s run on a small package|`docs/examples/yaml.md:97-98`, `docs/dogfood/ufo.md:68`|
|`discover` itself, per process|401 ms to build the TypeScript program plus 1.78 s to lower every type, on this repository's own 118 files|`docs/decisions/0008-type-lowering-memoized-and-width-bounded.md:10`, `:62`|
|the wasm harness's per-process work|the module is read and re-instrumented in `serve()` before the first request, so every worker process pays the rewrite|`src/main.rs` `serve()` in `autofuzz-harness-wasm`|
|one `js-ts` plugin process|1.33 cores; 12 processes deliver 4.9× one process at 8.1 cores|`docs/decisions/0011-…:24`, `:29`, `:31`|
|worker retirement at `INVOCATIONS_PER_WORKER = 128`|about 68 ms per retirement; the heap retains roughly 0.2 MB per realm|`docs/decisions/0006-…:38`|
|a wasm instance|microseconds next to the invocation it follows|`src/runner.rs` in `autofuzz-harness-wasm`|
|wasm throughput|12 000 invocations/s on inputs that return promptly, 880/s on a scan's mix|`docs/decisions/0003-wasm-coverage-by-post-hoc-rewriting.md:65`|

### Discovery is the cost you multiply by your core count

The engine sends `discover` to every plugin process that it starts, and no protocol method carries an
inventory from one process to another, so a plugin cannot hand its workers the answer that it already
computed. A default scan therefore runs discovery `workers + 1` times before its first invocation,
and again on every worker restart (`Worker::restart`).

The supervisor brings its pool up concurrently — `Supervisor::new` scopes one thread per worker and
joins them in index order — so setup is about one discovery plus the slowest worker to start. It is
still `discover` cost, once, multiplied by process startup. On a 12-worker scan of this repository's
own test fixture, moving the fan-out from serial to concurrent took setup from 8955 ms to 2692 ms.

The consequence for you is a number you control: a `discover` that takes 20 s turns a 12-worker
scan into minutes of nothing before the first invocation, and `setup … ms` in `engine.log` is where
you will see it. Bound the work and report what you left out. ADR 0008 states the rule for type
lowering: "The work was unbounded, not the budget"
(`docs/decisions/0008-…:48`); the inventory reports a shape that was too large instead of reporting
a shape that it never built (`docs/decisions/0008-…:35-36`). Unbounded lowering OOM'd at 4 GB after 119 s
and looked like the 120 s discovery timeout (`docs/decisions/0008-…:7-8`).

### The deadline is the search's breadth, not your time limit

The engine waits `deadlineMs + KILL_GRACE`, where `KILL_GRACE` is 250 ms, and then kills the process
(`supervise::KILL_GRACE`). A harness that can cut a call itself answers inside the grace and keeps
its process; a harness that cannot is killed at the end of it
(`docs/decisions/0004-the-plugin-answers-its-own-deadline.md:27-28`), and a killed call's coverage is
lost. `execution.deadlineCuts` counts answers whose reported duration reached the deadline and
`deadlineExceeded` counts answers that arrived after it (`docs/decisions/0004-…:33-34`).

A cut you make yourself is a `failed` outcome with `via: "deadline-cut"`, not an RPC error and
not a silence: the engine's own kill is filed under `LostReason::via` as `deadline`, so the two
channels stay distinguishable in the scan result. The mechanism is per-runtime — `js-ts` runs
each call in a worker thread and abandons it, and `python` arms `signal.setitimer(ITIMER_REAL,
deadlineMs)` around the call and raises inside the target — but the `via` string is the same,
because a scan result must read the same whichever harness produced it. Where the runtime has no
interruptible timer (no `SIGALRM`), skip the arm and let the engine kill; you lose that call's
coverage and nothing else.

The engine accepts your `durationMs` and your `memoryBytes` as you report them; it does not measure them
(`docs/architecture.md#oracles-and-defaults`, `docs/decisions/0004-…:52-53`).

The wasm probe at a 2000 ms deadline ran 1006 invocations in 30 s with 73 % of the worker-seconds
spent at the deadline; the same scan at 50 ms ran 23 963
(`docs/decisions/0004-…:45-47`). The deadline, not the machine, sets how wide the search gets.

## 9. Known traps

Every trap below produced a scan that read clean, not a crash. That is what they have in common: the
result is wrong and nothing reports it.

1. **Resetting coverage per call.** The start/stop cycle costs 8–20 ms and must complete before the
   realm compiles the target, or every digest after the first is empty
   (`docs/decisions/0006-…:34`, `:56`). Arm once per worker.
2. **Diffing coverage counts across a long-lived session.** A detailed V8 report describes each
   function's *most recent* execution, not a running total, so a digest cannot be built by
   subtracting one snapshot from the next (`docs/decisions/0006-…:43-44`).
3. **A pooled worker that caches the target module graph.** It breaks the single-call reproducer,
   and the trap above turns every digest after the first into an empty set
   (`docs/decisions/0006-…:52`).
4. **Fork or zygote per invocation.** Measured 25.9 ms against 24.2 ms for a worker thread, so it
   buys isolation you can get more cheaply (`docs/decisions/0006-…:53`).
5. **Compiler or loader work on the invoke path.** Importing the TypeScript compiler per invoke
   thread cost 5.4 ms per thread and was deoptimized by the profiler (`docs/decisions/0006-…:48`).
6. **Unbounded work during discovery.** It presents as the discovery timeout, not as an out-of-memory
   error (`docs/decisions/0008-…:7-8`). Bound the work and report the overflow.
7. **Treating `prepare` cost as target cost.** `prepare` gets 120 s and the invocation deadline does
   not apply to it (`Worker::invoke`). Do not move your module loading into `invoke` to look fast.
8. **Reporting an unreachable entry point only at `prepare`.** A `Skip` makes the engine ask again on
   the next invocation (`Worker::invoke`), so the planner re-prepares it forever. `discover`'s
   `skipped[]` is the channel for a symbol you can never call
   (`docs/architecture.md#interface-ir`).
9. **Target code writing to stdout.** It corrupts the protocol channel. See the `debug` export in
   `docs/examples/yaml.md:74-78`.
10. **An unhandled rejection that kills the worker.** Attribute it to the in-flight call instead, or
    the invocation is filed as a plugin loss rather than as the target's fault
    (`docs/decisions/0006-…:36`, Decision 5).
11. **Basing a fault's identity on runtime frames.** Every Rust wasm panic then folds into one
    finding (`docs/decisions/0003-…:47-49`). Frames must identify the target's code.
12. **Rebuilding the target artifact to get finer coverage.** It changes the artifact under test
    (`docs/decisions/0003-…:17`).
13. **Raising `INVOCATIONS_PER_WORKER`.** It exists to bound heap growth, not to save latency
    (`docs/decisions/0011-…:53`).

14. **Returning `sys.monitoring.DISABLE` from the LINE callback.** Measured on Python 3.14.4: a
    callback that returned `DISABLE` reported the first call's 4 lines and then reported nothing
    for those same lines on every later call — trap 1's mistake in a second runtime. The 24 wire
    rules will not catch it: they check that the map lists each digest, not that the digests grew, so a
    silently empty digest passes all of them. The template rule `coverage-digest-live` does catch
    it, because two inputs that both digest to nothing are the same digest. Arm once and return
    `None`.
15. **Believing `set_events(TOOL_ID, 0)` stopped your coverage.** Measured on Python 3.14.4:
    after clearing the tool's events, a call still reported 5 lines, because `set_local_events`
    arms events per code object and the tool-level mask does not disarm them. There is no
    mid-scan off switch; make collection depend on the per-invocation digest, not on the mask.
16. **Reporting `start == end` in a `coverageMap` entry whose id xors the offsets.** The
    `js-ts` id is `fnv1a64(url) ^ (start << 1) ^ (end << 1)` folded to u32, which is safe only
    because V8 block ranges always have `start != end`. A line-unit harness reports
    `start == end`, the two shifted terms cancel, and every line of a file folds to one edge —
    measured on the first `python` harness. Mix the range into the hash
    (`fnv1a64(f"{url}\0{start}\0{end}")`) instead of xoring it.

## 10. Registering the harness

No Rust registration is needed. `harness.command` in `autofuzz.json` gives the argv:

```jsonc
{
  "harness": {
    "name": "mylang",
    "command": ["/abs/path/to/my-plugin", "--stdio"]
  }
}
```

No harness name is special. A name installed with `afz install-harness` needs no `command`:
`LoadedConfig::launch` (`crates/afz-engine/src/config.rs`) consults `harnesses::lookup` -
the registry that `install-harness` writes - only when `command` is absent, and a name with neither
fails at load with `` `command` is required for harness "{name}": the engine does not guess a
runtime; install one with `afz install-harness <git-url>` `` (`ScanProject::validate` applies the
same rule before the first spawn). `probe-plugin --command` still wins over both. The engine once had a built-in argv
for `js-ts`, and older text in this repository still says so; that path is gone, and an uninstalled
`js-ts` needs a `command` like every other harness.

Two resolution details:

- `command` elements that point at an existing file are canonicalized against the directory that
  `afz` was run from (`absolutized` in `config.rs`), because a plugin path typed the way a person
  types it would otherwise be looked up inside the scanned project. A record's argv is already
  absolute; `install-harness` resolved it against the harness root when it recorded it.
- The plugin's working directory is the directory holding `autofuzz.json`, and the engine injects
  `AFZ_ROOT` and `AFZ_CONFIG` pointing at that directory and at the file. The parent environment is
  inherited. The `discover` request's `root` is `harness.root` resolved against that directory, so
  it is not always the same directory that the plugin starts in.

## 11. Testing a harness

Run `afz validate-harness` first (section 2). Then copy the patterns that the shipped harnesses use.

**A wire-vocabulary pin.** The language-neutral requirement: every literal that the plugin sends or
reads — method names, capability strings, skip reasons, materializations, error codes — is
compared at test time against `docs/schema/afz-1/*.json` and against the plugin interface in
`docs/architecture.md`, in a test that needs no Rust binary and no network. The two shipped
examples are `test/contract.test.mjs` in `autofuzz-harness-js-ts` (node) and
`test/contract_test.py` in `autofuzz-harness-python` (stdlib `unittest`, no third-party
dependency). Both read the engine checkout named by `AFZ_DOCS`, defaulting to a sibling checkout.

**A fixture directory.** The language-neutral requirement: a directory inside the harness package
holding its own `autofuzz.json` and a small source tree, with no registry or package-manager
entry, referenced by path from the tests and looked up by target *name*, never by the
content-derived id. `test-targets/billing/` in `autofuzz-harness-js-ts` and
`test-targets/pricing/` in `autofuzz-harness-python` are the shipped examples; each repository's
own tests and README scan example drive them through the installed plugin.

**A reproducer run.** The language-neutral requirement: the plugin's own tests render a
reproducer against the fixture directory and *execute* it, asserting the fault class and the
file that it points at, because schema validation proves the shape of the answer and nothing about
whether it runs. `test/repro_test.py` in `autofuzz-harness-python` and
`crates/afz-engine/tests/minimize.rs` are the two shipped examples.

**A conformance template emitter and validator.** The language-neutral requirement: the package
ships two small programs. The emitter renders a scan project — fixture tree, `autofuzz.json`, and
an `afz-conformance-manifest.json` that identifies the symbol that proves each template property —
into a directory it is given (`conformance/emit.mjs`, `conformance/emit.py`, `src/bin/emit.rs` in
the five shipped repositories). It needs its host runtime and nothing else: no dependency
install, no compiled plugin, no other repository. The fixture validator — `conformance/validate.mjs`,
`conformance/validate.py`, `src/bin/check.rs` — reports on the rendered fixture with the plugin out
of the path, and the suite requires the two judgments to agree (section 12). The suite — not the
emitter — drives every property over the protocol and judges the answer, so a fixture that asserts
a property that it does not actually trigger fails. Properties that a runtime genuinely cannot
express are declared `notApplicable` with a reason, and the reason appears in the report, where CI
keeps the not-applicable set as a golden list (section 12).

**Protocol coverage for the wasm harness** lives in the wasm harness repository's CI, which
installs its own checkout and asserts the report; the commands it runs, locally:

```sh
afz install-harness https://github.com/DustinEwan/autofuzz-harness-wasm.git
afz validate-harness wasm
```

`vendor/` and `examples/` are dogfood trees that CI never runs. Do not write a harness test against
them.

### What each rule means

The suite reports 23 wire rules. When the project carries a manifest, ten more rules join it under
the template property ids (`crash-unguarded`, `stdout-purity`, …) and two under `lifecycle-service`
and `lifecycle-nonce` (section 12). The rules whose message is not self-explanatory:

|Rule|Why it is a violation|
|---|---|
|`skipped-complete`|`materializationCounts` in `scan.json` is summed from `targets` and `skipped` together. A non-constructible symbol in neither list makes the inventory arithmetic lie.|
|`discover-repeatable`, `discover-ids-stable`|The engine replays the same `discover` into a second process and asks a connection twice. A different id set means the inventory is not a function of the request.|
|`connection-ready`|Worker connections never receive `handshake`. A plugin that requires it there answers every `prepare` with an RPC error, and the scan files that as `plugin-lost` against the target.|
|`coverage-declared`|The handshake claimed `coverage` and an `ok` answer carried no digest, so the corpus falls back to the outcome tuple as its novelty signal.|
|`coverage-edges-known`|The map is the denominator of `coverageRatio` and the table `coverage.json` is written from. A digest edge the map cannot name is reported as covered code no reader can locate.|
|`outcome-duration`|The deadline oracle reads that number. A negative or absent `durationMs` makes `deadlineCuts` meaningless.|
|`unknown-method-id`, `framing-intact`|These are the two requests the engine's own client never makes, so nothing else catches them.|

The four notes are cost observations: `discover-ms` (with the projection that the engine will make of
it), `prepare-ms`, `self-cut`, and `repeat-drift`. `repeat-drift` means the same values answered a
different `kind` on the second `invoke` for that target: usually leaked module state, occasionally a
target that reads a clock.

## 12. The conformance template, versions, and the runtime lifecycle

The wire contract says what a response must contain. It cannot say what a harness must be able to
do: crash on a poisoned string, reject a closed channel, materialize a receiver that it never encounters.
Template v1 states those ten semantic properties, and `afz-conformance` embeds the statement
(`crates/afz-conformance/template/v1.json`) in the binary.

**The emitter pattern.** The template lists properties; only the harness knows which of its own
symbols can demonstrate them. So the harness package ships an emitter that renders a fixture tree
plus a manifest, and the suite does the driving and the judging. One command runs both halves, in
the package directory:

```sh
afz validate-harness
```

The emitter renders the scan project only — `autofuzz.json`, a small source tree, and the
manifest — into the temporary directory the suite gives it, and that rendered project is the only
project validation ever drives. The rendered `autofuzz.json` carries no `command`, because the
plugin the suite launches is the one your `afz-harness.json` names.

**The fixture validator.** Seven of the ten properties — every one except `stdout-purity`,
`reproducer-script`, and `deadline-sleep` — are decided from data the plugin itself produced: its
skip list, its fault frames, its own digests. So the manifest's `validator` names a second program
that judges the same fixture with the plugin out of the path. Its contract:

- It reports exactly the seven fixture-shaped ids — `crash-unguarded`, `guard-refusal`,
  `private-symbol`, `unrepresentable-symbol`, `unconstructible-receiver`, `method-receiver`,
  `coverage-digest-live` — and never a process property, which only the protocol judge can see.
- Its stdout is exactly one JSON `TemplateReport` - `{"version": 1, "properties": [{"id",
  "verdict", "detail"}, …]}` with `verdict` one of `pass`, `fail`, `not-applicable` - and nothing
  else. It exits 0 even when a property fails; a non-zero exit means the validator itself could not
  run, which fails the validation.
- It imports only the fixture and the host's standard library - never the plugin, never the
  emitter - because its whole value is reporting on the fixture with the thing under test out of
  the path.

The suite compares the two verdict lists property by property, and any disagreement - a
`not-applicable` where the plugin answered `pass`, a missing id, a duplicate, a process id - is a
run violation under the rule `fixture-validator`. A project with no validator still validates, with
a note saying the template verdicts rest on the plugin's own answers.

**The manifest.** `afz-conformance-manifest.json` sits in the rendered project next to
`autofuzz.json`:

```json
{
  "templateVersion": 1,
  "properties": {
    "guard-refusal": {
      "symbol": "apply_discount",
      "input": [{"node": "number", "value": -5, "provenance": "user"}]
    },
    "coverage-digest-live": {
      "symbol": "tiered_price",
      "inputs": [
        [{"node": "bigInt", "value": "0", "provenance": "user"}],
        [{"node": "bigInt", "value": "2", "provenance": "user"}]
      ]
    },
    "unconstructible-receiver": {"symbol": "post", "blockedByType": "Ledger"},
    "private-symbol": {"symbol": "_cache_key"}
  },
  "validator": ["python3", "conformance/validate.py"],
  "lifecycle": {
    "launch": ["java", "-jar", "app.jar", "--port={{port}}"],
    "health": "http://127.0.0.1:{{port}}/health",
    "teardown": ["./scripts/stop.sh"],
    "nonce": "afz-nonce-4f2c9a",
    "probe": "healthProbe"
  }
}
```

`symbol` is a name from the plugin's own `discover` answer, and `input` is the argument list that
must trigger the property (`inputs`, a list of argument lists, for the rules that compare two
calls).

The manifest above is the shape that the `python` harness emits, and that harness realizes all ten properties.
The `js-ts` manifest shows the other branch. It declares `private-symbol` not applicable, because
TypeScript discovery enumerates exported symbols only, so a module-local function is never enumerated and
no `skipped` entry can refer to it. See [`harnesses.md`](harnesses.md) for that gap in full.

Every property of the template needs an entry: a symbol plus the input (or inputs) that must
trigger it, a `blockedByType` for `unconstructible-receiver`, or a `notApplicable` reason. A
property that the manifest does not account for fails the run; the emitter is never allowed to be silent.

**Versions are immutable.** A published template version never changes: rules are only ever added
to a new version, so a plugin that passed v1 keeps passing v1 no matter what the repository
learns later. `afz` embeds the versions it supports and rejects a manifest that cites another. The
report's `template.properties` lists every verdict — pass, fail, or not-applicable with its
reason — and CI keeps the not-applicable set as a golden list, so a new gap has to be deliberate.

**The runtime lifecycle.** A REST-backed harness owns a service, and a user cannot be expected to
start it by hand before validating. The manifest's `lifecycle` block lets the suite do it:
`launch` is the package's own command line, `{{port}}` is replaced by a port that the suite allocated
(the suite never assumes a fixed port), and `health` must answer 2xx–4xx before anything else
runs. `teardown` runs when the suite is done, and every failure quotes the package's own argv:
`lifecycle-service` when the service never came up or teardown failed, `lifecycle-nonce` when the
`probe` target's answer does not echo the manifest's `nonce`. That last rule is what proves the
plugin is talking to the service that this suite started, and not to a table that it read from disk.

The suite hands the plugin the same port it handed the service: every plugin connection the suite
spawns carries `AFZ_PORT` set to the allocated port, and only when the manifest has a `lifecycle`
block. A harness may write `{{port}}` anywhere in its own `harness.options` strings; the harness
substitutes it from `AFZ_PORT`, and refuses with a message naming both `{{port}}` and `AFZ_PORT`
when the placeholder is present and the variable is unset. The engine substitutes nothing into
`harness.options`.

## 13. Where the numbers came from

- [`docs/architecture.md`](architecture.md) — the design, the interface IR, the classification rule.
- [`docs/decisions/0003-wasm-coverage-by-post-hoc-rewriting.md`](decisions/0003-wasm-coverage-by-post-hoc-rewriting.md)
  — wasm coverage, throughput, and fault identity.
- [`docs/decisions/0004-the-plugin-answers-its-own-deadline.md`](decisions/0004-the-plugin-answers-its-own-deadline.md)
  — deadlines, cuts, and kills.
- [`docs/decisions/0006-fresh-realm-per-invocation-in-one-warm-isolate.md`](decisions/0006-fresh-realm-per-invocation-in-one-warm-isolate.md)
  — the invoke-path cost measurements and the coverage traps.
- [`docs/decisions/0008-type-lowering-memoized-and-width-bounded.md`](decisions/0008-type-lowering-memoized-and-width-bounded.md)
  — discovery cost and the bound-and-report rule.
- [`docs/decisions/0011-plugin-pool-runs-at-the-machines-ceiling.md`](decisions/0011-plugin-pool-runs-at-the-machines-ceiling.md)
  — pool scaling, digest cost, and worker retirement.
- [`docs/examples/yaml.md`](examples/yaml.md) — the wall-clock breakdown of a large scan.
- [`docs/dogfood/ufo.md`](dogfood/ufo.md) — the same on a small package.
- [`crates/xtask/src/fake.rs`](../crates/xtask/src/fake.rs) — the reference plugin side: all seven
  methods answered from a script, with `handshake`, `requireDiscover`, and `dropCoverage` knobs for
  modelling a plugin that does not conform.

The two reference plugins to copy are `src/` in `autofuzz-harness-js-ts` (a TypeScript/Node plugin:
`child_process` for discovery, a `node:vm` realm per call, V8 block-coverage translation) and
`src/` in `autofuzz-harness-python` (a stdlib-only Python plugin: `ast` discovery, a module cache,
`sys.monitoring` line coverage, `signal.setitimer` self-cut). Read the one nearest your runtime;
the wire contract they both honor is section 3.

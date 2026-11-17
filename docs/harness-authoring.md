# Writing a harness plugin

A harness plugin connects autofuzz to one language, runtime, or API. It reads the API description
and tells the engine what it can call. The engine handles the loop: generating inputs, scheduling
calls, enforcing deadlines, minimizing failures, and writing the result. The plugin handles the parts
that depend on the target: finding the functions, methods, or routes, building their parameters,
running one invocation, and saying what happened.

You can write a plugin in any language that can read a line from stdin and write a line to stdout.
The plugin runs as a child process and speaks JSON-RPC. This guide builds a plugin in the order the
engine uses it. The wire reference is at the end, so you can read the tutorial first and look up the
exact fields when you need them.

## Contents

- [1. Start with the two commands](#1-start-with-the-two-commands)
- [2. Answer the handshake](#2-answer-the-handshake)
- [3. List the targets](#3-list-the-targets)
- [4. Build the arguments and run a call](#4-build-the-arguments-and-run-a-call)
- [5. Add coverage](#5-add-coverage)
- [6. Add a reproducer](#6-add-a-reproducer)
- [7. Handle the deadline](#7-handle-the-deadline)
- [8. Prove the environment is up](#8-prove-the-environment-is-up)
- [9. Pass the conformance template](#9-pass-the-conformance-template)
- [10. Test the plugin](#10-test-the-plugin)
- [11. Register the plugin](#11-register-the-plugin)
- [12. Reference](#12-reference)
- [13. Where to look next](#13-where-to-look-next)

## 1. Start with the two commands

You need two commands while you work:

```sh
afz validate-harness
afz probe-plugin --path <scan project>
```

`validate-harness` runs the conformance suite against the plugin in the current directory. It runs
your `build` command, renders a small scan project with your emitter, and drives the plugin through
every rule. It prints one line per rule and exits 1 when the suite reports a violation.

The rules themselves are declared in `afz-1.profile.json`, at the root of this repository: the
methods with the schema of each request and response, which capability gates which method, what
each loss reason becomes in a report, and every rule with the scripted defect that proves it can
fire, or a sentence saying none exists. The pluginrpc mechanism reads that file, so
`prpc-core --suite` runs the six wire rules and these profile rules against a plugin with none of
this repository's code in between.

Those scripted defects start this repository's own test plugin, which your checkout does not
contain, so `prpc-core` says so once when it runs them against your plugin and leaves those rules
as the profile states them. Whether a rule can fail is this repository's proof; what your run
answers is whether your plugin passes the rule.

The two commands start the plugin differently, and a harness can pass one and fail the other.
`validate-harness` starts it with `AFZ_ROOT` on the fixture it rendered. `prpc-core` starts it in your
checkout with no scan project at all, because the six wire rules are about the connection and the
handshake, which come before any project exists (mechanism spec §6). A plugin that opens the project
while it is starting therefore passes `validate-harness` and fails `prpc-core`'s `spawn` rule with
`the plugin gave no usable handshake`. Answer `handshake` first, and read the project on the first
request that needs it.

`probe-plugin` starts the plugin against a real scan project and prints the targets it discovers.
Use it to check what the engine will see before you run a scan.

Both commands read `afz-harness.json` from the harness repository. The file declares the plugin
command and the conformance emitter:

```json
{
  "name": "mylang",
  "install": [["npm", "ci"]],
  "build": [["npm", "run", "build"]],
  "command": ["node", "dist/server.js"],
  "conformance": {
    "emitter": ["node", "conformance/emit.mjs"],
    "deadlineMs": 1500
  }
}
```

`install` runs only during `afz install-harness`, because dependency installation is a once-per-
checkout step. `build` runs during install and during every validation. `command` is the plugin
process. `conformance.emitter` renders the scan project that the suite drives.

## 2. Answer the handshake

The engine starts the plugin and sends a `handshake` request. The response describes the plugin and
its capabilities:

```json
{
  "protocolVersion": "afz/1",
  "name": "mylang",
  "version": "0.1.0",
  "targetClass": "design-time",
  "capabilities": ["coverage", "reproducer"],
  "languages": ["mylang"],
  "discoverRequiresExecution": false
}
```

`targetClass` is `design-time` for a library or a module, and `runtime` for a live service.
`capabilities` is the list of optional methods you answer. `discoverRequiresExecution` says whether
discovery imports the target; leave it `false` unless you have no other choice.

The handshake is the only place where the engine receives the plugin's capabilities. A worker
connection never receives a handshake, so no other method may depend on having seen one.

## 3. List the targets

The engine sends `discover` with a root, optional include and exclude globs, and the path of the
scan project file. Your plugin walks the root and returns two lists:

```json
{
  "targets": [
    {
      "id": "afz:t:9c1f",
      "targetClass": "design-time",
      "origin": { "file": "src/math.ts", "line": 12 },
      "name": "add",
      "parameters": [
        { "name": "a", "type": { "kind": "scalar", "scalar": "number" }, "required": true },
        { "name": "b", "type": { "kind": "scalar", "scalar": "number" }, "required": true }
      ],
      "returns": { "kind": "scalar", "scalar": "number" },
      "effects": { "async": false, "throws": true },
      "materialization": "constructible",
      "guardSites": [{ "file": "src/math.ts", "line": 14 }]
    }
  ],
  "skipped": [
    {
      "targetId": "afz:t:4b2e",
      "name": "connect",
      "reason": "unconstructible-parameter",
      "blockedBy": { "type": "Socket", "module": "src/net.ts", "path": "receiver.socket" },
      "unlocksTargets": 3
    }
  ]
}
```

The `targets` list holds the things the engine can call: functions, methods, or routes. The
`skipped` list holds the ones it cannot call, with a reason. Do not drop a symbol silently: the inventory is how a user learns that a
target is out of reach, and the `blockedBy` record is how they fix it.

`id` must be stable for the same symbol across runs. The engine uses it to match findings and to
replay a corpus, so a content hash of the module path, symbol, and signature is a good choice.

`parameters[].type` is a `TypeNode`. The engine has 13 variants: scalars, string formats, literals,
collections, objects, tuples, composites, buffers, promises, functions, streams, opaque types, and
recursive references. The schema in `docs/schema/afz-1/type-node.json` is the authority. If a type
does not fit, report the target as `unsupported-signature` instead of inventing a shape.

`include` and `exclude` are instructions for your plugin, not for the engine. Match them against
paths relative to the root, with `/` separators - never relative to a nested package inside the
root, so one pattern means the same thing in a one-package tree and a many-package one. An empty
include means your own default.
The engine adds the scan output directory to `exclude`, so a second scan does not discover the
first scan's
reproducers.

Discovery must terminate. The engine gives it 120 seconds and then kills the process. If your
discovery can walk a large tree, bound the work and report what you left out.

## 4. Build the arguments and run a call

For each target, the engine sends `prepare` with a target id and a list of `ValueTree` values. Your
plugin builds the receiver and the argument list, or reports why it cannot:

```json
{ "ok": true, "materialized": { "handle": "add", "summary": "add(number, number)" } }
```

or

```json
{ "ok": false, "reason": "unconstructible-parameter" }
```

The engine calls `prepare` once per target per process, lazily, on the first invocation. A skip
answer removes the target from the prepared set, so the engine asks again on the next invocation.
That is why a symbol that can never be called belongs in `discover`'s `skipped` list, not in
`prepare`.

Then the engine sends `invoke` with the target id, the values, a deadline, and a flag that asks for
coverage. Your plugin runs the call and returns one outcome:

```json
{ "kind": "ok", "durationMs": 1.2 }
```

```json
{ "kind": "rejected", "durationMs": 0.8, "via": "guard-site", "detail": "negative price" }
```

```json
{
  "kind": "failed",
  "durationMs": 3.4,
  "via": "unhandled-rejection",
  "fault": {
    "class": "TypeError",
    "message": "Cannot read properties of undefined",
    "frames": [{ "file": "src/math.ts", "line": 14, "column": 9 }]
  }
}
```

The plugin determines which outcome applies. Use `rejected` when the target refused the input on
purpose, through its own validation path: a throw at a guard site, a returned `Err`, or a 400
response. Use `failed` when the target tried to accept the input and broke: an unexpected exception,
a panic, or a 500. `ok` and `rejected` are both valid results from the target's point of view;
`failed` is not. The engine never reads your exceptions, because it does not know them.

Never answer `invoke` with an RPC error to report a fault. An error frame is a refusal, not a
result: the engine files the invocation as `failed` with `via: "plugin-refused"` carrying your
message, keeps the process, and does not restart it, because a plugin that answers in milliseconds
is a working plugin. Put the fault in the `Outcome` instead.

## 5. Add coverage

Coverage is optional. If you declare the `coverage` capability, the engine sends
`collectCoverage: true` and expects a digest on every `ok` outcome. The digest is a list of stable
edge ids that the call touched. The engine treats the ids as opaque: it only asks whether the set is
new.

You also answer `coverageMap`, which maps each edge id to a source range:

```json
{ "entries": [{ "edge": 1234, "url": "file:///src/math.ts", "start": 120, "end": 148 }] }
```

The engine asks for the map every 5000 invocations and once at the end. The map is the denominator of
the coverage ratio and the table in `coverage.json`, so an edge that the map cannot identify is covered
code that no reader can locate.

Two mistakes cost a lot here. First, do not reset the coverage probe for every call. The start and
stop cycle costs 8 to 20 ms and must finish before the target compiles; start the probe once per
worker. Second, do not build a digest by subtracting one snapshot from the next. A detailed V8 report
describes each function's most recent execution, not a running total. Read the report, take the
ranges for the call, and clear the state in a way that the runtime supports.

The template rule `coverage-digest-live` catches a probe that is started once and never reset: two
different inputs to the same multi-line symbol must produce two different, non-empty digests.

## 6. Add a reproducer

If you declare the `reproducer` capability, the engine sends `reproducer` once per finding, after the
minimized input still reproduces the fault. Return a script and its file extension:

```json
{ "extension": "py", "script": "from mylib import add\n\nadd(-1, 2)\n" }
```

The engine writes the script under `repros/` with the name `afz-f-<id>.<extension>`. It does not run
the script, so a schema-valid answer proves nothing. Run your own rendered script in your tests and
assert that it produces the same fault class on the same source line.

Write the script for a triager who has never seen the scan. Put notes on their own lines, never
inside the call, because a `#` comment there hides the closing bracket. A deadline finding
reproduces by not returning, so say in the header when to stop the script.

## 7. Handle the deadline

The engine waits `deadlineMs + 250 ms` and then kills the process. If your runtime can interrupt a
call, cut it yourself and return a `failed` outcome with `via: "deadline-cut"`. That answer keeps
the process and the call's coverage, and the engine files it as the same single `deadline` finding
an engine kill would have produced - one deadline event, one finding, whoever cut the call. If your
runtime cannot interrupt a call, let the engine kill it; you lose that call's coverage and nothing
else.

Do not move module loading into `invoke` to look fast. `prepare` gets 120 seconds and is not held to
the invocation deadline, because preparing imports the target's module graph. The invocation path
should be a few milliseconds.

## 8. Prove the environment is up

If your targets live in a live service, declare the `readiness` capability and answer `ready`:

```json
{ "ready": true }
```

or

```json
{ "ready": false, "detail": "connection refused on 127.0.0.1:8080" }
```

The engine calls `ready` once on the probe connection, after discovery and before any worker starts.
A not-ready answer stops the scan with your detail as the first line. Readiness proves reachability
and dialogue, not health: a service that answers 500 on every route is ready, because the scan can
talk to it and the 500s are findings.

If your targets live in the tree that discovery already read, leave the capability out.

## 9. Pass the conformance template

The wire rules check the shape of every response. They cannot check that your plugin can crash on a
poisoned string, reject a closed channel, or materialize a receiver it has never seen. The
conformance template holds the semantic properties in template v1 for those behaviors.

Your package ships two small programs to satisfy it. The emitter renders a scan project into a
directory: a fixture tree, an `autofuzz.json`, and an `afz-conformance-manifest.json` that identifies
the symbol proving each property. The validator reports on the same fixture with the plugin out of the
path. The suite drives the properties over the protocol and compares its verdicts with the
validator's. A disagreement fails the run.

The manifest looks like this:

```json
{
  "templateVersion": 1,
  "properties": {
    "guard-refusal": {
      "symbol": "apply_discount",
      "input": [{ "node": "number", "value": -5, "provenance": "user" }]
    },
    "coverage-digest-live": {
      "symbol": "tiered_price",
      "inputs": [
        [{ "node": "bigInt", "value": "0", "provenance": "user" }],
        [{ "node": "bigInt", "value": "2", "provenance": "user" }]
      ]
    },
    "unconstructible-receiver": { "symbol": "post", "blockedByType": "Ledger" },
    "private-symbol": { "symbol": "_cache_key" }
  },
  "validator": ["python3", "conformance/validate.py"]
}
```

Every property needs an entry: a symbol and the input that triggers it, a `blockedByType`, or a
`notApplicable` reason. A property that the manifest does not account for fails the run. A property
that your language cannot express is declared `notApplicable` with a reason, and the reason appears
in the report.

A published template version never changes. Rules are added in a new version, so a plugin that
passed v1 keeps passing v1.

If your harness owns a service, add a `lifecycle` block to the manifest. The suite starts the
service on an allocated port, waits for its health URL, drives one probe target that must echo a
nonce, and stops the service. The same port reaches the plugin as `AFZ_PORT`. A harness may write
`{{port}}` in its own `harness.options` strings and substitute it from `AFZ_PORT`.

## 10. Test the plugin

Run `afz validate-harness` first. Then copy the patterns from the recommended harnesses:

- **A wire-vocabulary test.** Compare every literal your plugin sends or reads against
  `docs/schema/afz-1/*.json` and the method definitions in [`architecture.md`](architecture.md#the-methods).
  The envelope those payloads travel in belongs to the pluginrpc mechanism rather than to autofuzz,
  so its normative schemas are under `spec/schema/prpc-1/` in
  [the pluginrpc repository](https://github.com/DustinEwan/pluginrpc), not in `docs/schema/`.
  The js-ts and python harnesses each have one.
- **A fixture directory.** Put a small source tree and an `autofuzz.json` inside the package. Tests
  reference it by path and look up targets by name, never by the content-derived id.
- **A reproducer test.** Render a reproducer against the fixture and execute it. Assert the fault
  class and the file that it points at.
- **An emitter and a validator.** The emitter renders the fixture; the validator judges it without
  the plugin. The suite requires the two reports to match.

`vendor/` and `examples/` are dogfood trees that CI never runs. Do not write a harness test against
them.

## 11. Register the plugin

No Rust registration is needed. A scan project can declare the command directly:

```jsonc
{
  "harness": {
    "name": "mylang",
    "command": ["/abs/path/to/my-plugin", "--stdio"]
  }
}
```

If you install the harness with `afz install-harness`, the scan project needs only the name:

```jsonc
{ "harness": { "name": "mylang" } }
```

The engine looks up the installed record only when `command` is absent. A name with no record and no
command fails at load. `afz probe-plugin --command` wins over both.

The plugin runs with the directory that holds `autofuzz.json` as its working directory. The engine
passes `AFZ_ROOT` and `AFZ_CONFIG` in the environment. A `command` element that points at an
existing file is made absolute against the directory where you ran `afz`, so an absolute path is the
safe choice.

## 12. Reference

### The methods

| Method | Request `params` | Required response |
| --- | --- | --- |
| `handshake` | omitted | `protocolVersion`, `name`, `version`, `targetClass`, `capabilities`, `languages`, `discoverRequiresExecution` |
| `discover` | `root`, optional `include`, `exclude`, `configPath` | `targets[]`, `skipped[]` |
| `prepare` | `targetId`, `values[]` | `{ "ok": true, "materialized": ... }` or `{ "ok": false, "reason": ... }` |
| `invoke` | `invocationId`, `targetId`, `values[]`, `deadlineMs`, `collectCoverage` | one `Outcome` |
| `minimize` | `targetId`, `values[]`, `fault`, `budgetMs` | `values[]`, `steps` |
| `reproducer` | `targetId`, `values[]` | `extension`, `script` |
| `coverageMap` | omitted | `entries[]` |
| `ready` | omitted | `{ "ready": true }` or `{ "ready": false, "detail": ... }` |

The engine does not call `minimize`. It shrinks an input by re-invoking the target with smaller
values, because that works for every plugin and needs no extra code. A plugin can answer `minimize`,
but the engine does not depend on it. Leave the capability out and answer the method with `-32601`
or `-32600`.

### Literals and error codes

The rules for `jsonrpc`, `id`, `params`, and the four error codes belong to the pluginrpc mechanism
(`prpc/1`), which autofuzz compiles against; that specification is normative for them. The rest of
the values below are the `afz/1` profile's own.

- `protocolVersion` is `"afz/1"`. The engine accepts any revision in `SUPPORTED_VERSIONS`. This build
  carries only `afz/1`.
- `jsonrpc` is `"2.0"`.
- `id` is a `u64` assigned per plugin process, starting at 1 and never above `2^53 - 1`, so a plugin
  written in JavaScript reads it exactly. `id` 0 is reserved: a plugin echoes it on a refusal whose
  request id it could not determine. Echo the id on error frames too.
- `params` is omitted, not `null`, for `handshake`, `coverageMap`, and `ready`.
- Error codes: `-32700` parse error, `-32600` invalid request, `-32601` method not found, `-32000`
  plugin error.
- `SkipReason` values: `unconstructible-parameter`, `discovery-error`, `not-public`, `excluded`,
  `unsupported-signature`, `target-disabled`.
- `Materialization` values: `constructible`, `fixture-required`, `opaque`.

### Transport rules

The engine reads one JSON message per line. There is no `Content-Length` framing.

A blank line is EOF. The engine records the connection as lost.

An unparseable line is dropped without an error. The request that was waiting then times out, the
engine kills the process, and the invocation is filed as a plugin loss. Answer malformed input with
an error frame.

A response whose `id` matches no in-flight request is discarded. A plugin that answers with `id: 0`
has answered nothing.

stdout carries the protocol and nothing else. Write diagnostics to stderr. The engine keeps the last
4096 stderr lines per process and prints at most 200 distinct lines in the report as `plugin said:
...`.

Survive a failed stdout write. A write to a pipe whose reader is gone can end a Node process. Install
a handler that reports to stderr and stays up; the engine will notice the silence and restart the
process.

### Connections and timeouts

The engine opens three kinds of connection:

| Connection | Receives |
| --- | --- |
| probe | `handshake`, `discover`, and `ready` when the handshake declares `readiness` |
| worker | `discover`, then `prepare`, `invoke`, `coverageMap`. Never `handshake` or `ready` |
| minimizer | `discover`, then `prepare`, `invoke` |

`discover` goes to every connection, because no method carries an inventory from one process to
another. A lost process is restarted, re-discovered, and its prepared set is cleared. Fifty
consecutive attempts that did not run - a restart after a lost process, or a refusal from a process
that answered - disable that target for the rest of the scan.

| Method | Timeout |
| --- | --- |
| `handshake` | 30 s |
| `discover` | 120 s |
| `prepare` | 120 s |
| `invoke` | `deadlineMs` + 250 ms |
| `minimize` trial | deadline + 10 s |
| `reproducer` | 10 s |
| `coverageMap` | 10 s |
| `ready` | 5 s |

### Capabilities

The engine reads only these capabilities:

- `coverage` turns on `collectCoverage` and the `coverageMap` walk.
- `reproducer` makes the engine ask for a script once per finding.
- `readiness` makes the engine call `ready` before it starts workers.

`stateful`, `minimize`, `sideEffects`, and `concurrent` are recorded in the scan result and change no
engine behavior. Declare only what you answer.

## 13. Where to look next

- [`architecture.md`](architecture.md) explains the design and the method definitions.
- [`harnesses.md`](harnesses.md) shows what the recommended harnesses can do and where they fall short.
- The `starter` repository is a complete conforming plugin in about 200 lines.
- The `js-ts` and `python` repositories are the reference implementations for coverage, reproducers,
  and self-cut deadlines.
- [`decisions/`](decisions/) holds the measurements behind the performance rules.

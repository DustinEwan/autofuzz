# Decision 4 - the plugin answers its own deadline, the engine's kill waits a grace beyond it

Status: accepted. 2026-11-11.
Amends: `docs/architecture.md` "execution", which gains `deadlineCuts`.

## Context

Every `invoke` carries `deadlineMs`, and by the time the wasm harness existed the two harnesses
behind that number behaved differently. The wasm harness cuts a call itself: `runner.rs` arms an
epoch deadline and wasmtime stops the call at the next loop backedge, so the plugin answers with the
duration it measured. The `js-ts` harness cannot. `packages/harness-js/src/server.ts` states in its
own comment that `deadlineMs` belongs to the engine, because a call stuck in a synchronous loop
inside a worker thread cannot be interrupted from inside that thread.

The engine first stopped waiting at exactly `deadlineMs` and killed the process. On the wasm probe
that turned every slow call into a killed process. The answer, which carried that call's coverage,
was thrown away; the run reported the engine's message, "plugin did not answer before the deadline",
instead of the duration the harness had measured; and the process restart cost more than the call it
replaced. Two accounting faults rode along with it. `RunReport::restarts` repeated the run total in
every batch and the scan loop added each batch in, so one 20,000-invocation run printed 904,328
restarts. And a call the plugin cut on time appeared nowhere in the report: `deadlineExceeded`
counts answers that arrived after the deadline, and a harness that cuts answers at it.

## Decision

1. The engine waits `deadlineMs + KILL_GRACE` before it kills a plugin process. `KILL_GRACE` is
   250 ms (`supervise.rs`). A harness that can cut answers inside the grace and keeps its process; a
   harness that cannot is killed at the end of it.
2. The reason a process was lost decides the reported `via`: `deadline` when the engine stopped
   waiting, `processExit` when the plugin ended itself with a call in flight, the loss reason
   otherwise. The `deadline` oracle keys on `oracles.deadline.ms` and on `via: "deadline"`, so the
   finding is the same whichever path produced it.
3. `execution.deadlineCuts` counts invocations whose reported duration reached the deadline the
   engine sent. `deadlineExceeded` keeps its narrower meaning: answers that arrived after the
   deadline, which in practice are the calls the engine stopped waiting for.
4. `RunReport::restarts` describes the batch that produced it, and the scan loop adds each batch's
   number into the run total.
5. The watchdog stops using a target after 50 consecutive restarts (`DISABLE_AFTER`), so a harness
   that cannot survive its own deadline stops costing the run invocations rather than restarting
   forever.

## Consequences

- A scan's wall clock can be dominated by calls that reach the deadline, and the report now says so.
  Measured on the wasm probe at the default 2000 ms: 132 of 1,006 invocations ran to the deadline,
  which is 73% of the worker-seconds in that 30-second run. The same scan at a 50 ms deadline
  completed 23,963 invocations. A reader can now tell that the deadline, not the machine, set the
  search's breadth.
- The deadline is not counted against the batch budget's `KILL_GRACE`: the grace delays a kill, not
  an answer, and counting it would leave a run whose budget is near one deadline with no room to
  attempt anything.
- The engine trusts a plugin's own duration measurement where the deadline oracle reads it. That is
  the same trust `memoryBytes` already places in the plugin.
- A grace is dead time when the plugin is never going to answer. A hung `js-ts` call now occupies its
  worker 250 ms longer before it is killed and restarted.

## Reconsidered

- **Killing at the deadline.** Rejected: it discards a coverage-carrying answer that was already on
  its way, and it costs a process restart on every slow call.
- **Asking the plugin to cut inside a margin the engine specifies.** Rejected: the engine cannot know
  a harness's teardown cost, and a margin every harness must remember to apply is contract duplicated
  across three implementations. A grace the engine owns asks nothing of the plugin.
- **Counting cuts only when the plugin labels one.** Rejected: `Outcome` has exactly three variants,
  and the wasm harness reports a cut as `ok` with no value, because calling a slow call a trap would
  report a fault the target never raised. Duration against the deadline the engine itself sent is a
  fact the engine can check without new protocol.

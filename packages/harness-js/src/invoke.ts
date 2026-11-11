// One warm worker thread, many invocations.
//
// The thread is created once per plugin process and serves calls until it is retired. Each
// invocation gets a fresh realm inside it — see `src/realm.ts` — so module-level state still
// starts empty on every call, and the thread keeps its inspector session, which is what makes the
// call cheap. It exists as a separate thread because a target that loops forever must not take the
// plugin process down with it, and because `inspector.Session` has to be created on the thread that
// runs the code.
//
// The worker speaks a small internal protocol to `server.ts`: a `prime` message carries a target's
// transpiled module graph, and a `job` message runs one prepare or invoke. Every reply echoes the
// job's `id`, because a warm worker can still have a reply in hand from a call the plugin has
// already given up on. The wire protocol belongs to the engine and never crosses in here.

import { parentPort } from "node:worker_threads";

import type {
  CoverageEntry,
  FaultSignature,
  Outcome,
  PrepareResp,
  SpecialNumber,
  ValueTree,
} from "./protocol.js";
import {
  classifyThrown,
  failureWithCause,
  signatureOf,
  VIA_MALFORMED_VALUE,
  VIA_MATERIALIZATION_BLOCKED,
  VIA_STUB_CALL,
  VIA_UNHANDLED_REJECTION,
} from "./classify.js";
import { CoverageProbe } from "./coverage.js";
import {
  MalformedValueError,
  MaterializationError,
  StubCallError,
  materializeValues,
} from "./materialize.js";
import { evaluateRealm, mapRealmCoordinates, type Realm } from "./realm.js";
import type { ModuleGraph } from "./transpile.js";
import type { SourceMap } from "./sourcemap.js";
import { isCallable, isConstructable, isRecord, type Callable } from "./guards.js";

/**
 * A specifier the realm could not resolve, such as `./ledger.js` with no `ledger.ts` beside it.
 * The harness resolves the NodeNext `.js`-to-`.ts` convention itself, so there is no loader mode
 * left to retry and this is the target's own failure, not the harness's.
 */
export const LOAD_MODULE_NOT_FOUND = "module-not-found";
export const LOAD_FAILED = "load-failed";

/** Hand a target's transpiled closure to the worker so a job can name it by key instead of size. */
export interface PrimeMessage {
  type: "prime";
  graph: ModuleGraph;
}

/** One prepare or invoke. `id` is echoed by every reply to it. */
export interface WorkerJob {
  type: "job";
  id: number;
  mode: "prepare" | "invoke";
  root: string;
  graphKey: string;
  moduleUrl: string;
  /** `convert`, or `Converter.convert` for a class member. */
  name: string;
  values: ValueTree[];
  guardSites: number[];
  rejectClasses: string[];
  collectCoverage: boolean;
  targetId: string;
}

export type WorkerRequest = PrimeMessage | WorkerJob;

export type WorkerReply =
  | { type: "prepared"; id: number; state: PrepareResp }
  | {
      type: "outcome";
      id: number;
      outcome: Outcome;
      entries: CoverageEntry[];
      reachable: CoverageEntry[];
    }
  | { type: "load-error"; id: number; code: string; message: string };

interface Resolved {
  call: Callable;
  /** What the worker had to build to make the call possible, for `prepare`'s summary. */
  built: string;
}

/** The graphs this worker has been given, by key. */
const graphs = new Map<string, ModuleGraph>();
/** One session for the life of the thread, reset around each call that asks for coverage. */
const coverage = new CoverageProbe();
/** The job whose reply has not gone out yet, or `null` between calls. */
let inFlight: number | null = null;

function send(reply: WorkerReply): void {
  parentPort?.postMessage(reply);
}

function failed(durationMs: number, via: string, fault: FaultSignature): Outcome {
  return { kind: "failed", durationMs, via, fault };
}

/**
 * Resolve the callable named by the descriptor. A dotted name is a class member: the harness
 * constructs the class with no arguments, the only receiver state it can build from generated
 * values alone. A constructor that needs arguments is reported unconstructible rather than
 * called with a half-built instance.
 */
function resolveCallable(mod: Record<string, unknown>, name: string): Resolved {
  const dot = name.indexOf(".");
  if (dot < 0) {
    const candidate = mod[name];
    if (!isCallable(candidate)) {
      throw new MaterializationError(`export "${name}" is not callable in the loaded module`);
    }
    return { call: candidate, built: `resolved export ${name}` };
  }
  const className = name.slice(0, dot);
  const member = name.slice(dot + 1);
  const owner = mod[className];
  if (!isConstructable(owner)) {
    throw new MaterializationError(`export "${className}" is not a class in the loaded module`);
  }
  const prototype = Reflect.get(owner, "prototype");
  const instanceMember = isRecord(prototype) ? prototype[member] : undefined;
  const staticMember = Reflect.get(owner, member);
  const candidate = isCallable(instanceMember)
    ? instanceMember
    : isCallable(staticMember)
      ? staticMember
      : undefined;
  if (!candidate) {
    throw new MaterializationError(`"${name}" is not callable in the loaded module`);
  }
  if (!isCallable(instanceMember)) {
    return { call: candidate, built: `resolved static ${name}` };
  }
  if (owner.length > 0) {
    throw new MaterializationError(
      `class "${className}" needs ${owner.length} constructor argument(s); the receiver is not constructible from generated values`,
    );
  }
  const receiver = new owner();
  return {
    call: candidate.bind(receiver),
    built: `constructed ${className}() with no arguments`,
  };
}

function loadFailureCode(error: unknown): string {
  if (error instanceof Error && "code" in error && typeof error.code === "string") {
    if (error.code === "ERR_MODULE_NOT_FOUND") return LOAD_MODULE_NOT_FOUND;
  }
  return LOAD_FAILED;
}

function loadDetail(error: unknown): string {
  return error instanceof Error ? `${error.name}: ${error.message}` : String(error);
}

const ERROR_CLASSES = new Map<string, { new (message?: string): Error }>();

/**
 * A constructor whose `name` is `name`, so `classify.ts` reads the fault class the target wrote
 * even when the error object itself came from another realm.
 */
function errorClass(name: string): { new (message?: string): Error } {
  const known = ERROR_CLASSES.get(name);
  if (known !== undefined) return known;
  const intrinsic = (globalThis as Record<string, unknown>)[name];
  let created: { new (message?: string): Error };
  if (typeof intrinsic === "function" && Reflect.has(intrinsic, "prototype")) {
    created = intrinsic as { new (message?: string): Error };
  } else {
    const subclass = class extends Error {};
    Object.defineProperty(subclass, "name", { value: name, configurable: true });
    created = subclass;
  }
  ERROR_CLASSES.set(name, created);
  return created;
}

/**
 * Bring an error that escaped the realm into this realm.
 *
 * V8 raises runtime faults — the `TypeError` of a null dereference — from the realm's own intrinsic
 * error prototypes, not from its globals, so such an error is not `instanceof Error` here and
 * `signatureOf` would report its class as `object`. The name, message and frames are copied into an
 * error built here; the frames are translated from emitted coordinates to source coordinates on the
 * way, which is what lets `classify.ts` match a guard site by line.
 */
function toHarnessError(error: unknown, maps: ReadonlyMap<string, SourceMap>): unknown {
  if (!isRecord(error)) return error;
  const stack = typeof error.stack === "string" ? error.stack : undefined;
  const mapped = stack === undefined ? undefined : mapRealmCoordinates(stack, maps);
  if (error instanceof Error) {
    if (mapped !== undefined && mapped !== stack) error.stack = mapped;
    return error;
  }
  const name = typeof error.name === "string" && error.name !== "" ? error.name : "Error";
  const message = typeof error.message === "string" ? error.message : String(error.message ?? "");
  const Constructor = errorClass(name);
  const translated = new Constructor(message);
  if (mapped !== undefined) translated.stack = mapped;
  return translated;
}

function numberName(value: number): number | SpecialNumber {
  if (Number.isNaN(value)) return "naN";
  if (value === Number.POSITIVE_INFINITY) return "infinity";
  if (value === Number.NEGATIVE_INFINITY) return "negativeInfinity";
  return value;
}

function opaque(what: string): ValueTree {
  return { node: "opaque", ty: what };
}

/**
 * `Object.prototype.toString`'s tag of a value. The target lives in another realm, so `instanceof`
 * against this realm's builtins says nothing about what the target built; the internal slot tag is
 * the one test that works across realms.
 */
function tagOf(value: object): string {
  return Object.prototype.toString.call(value).slice(8, -1);
}

function isThenable(value: unknown): value is PromiseLike<unknown> {
  return isRecord(value) && typeof value.then === "function";
}

/**
 * Re-encode a returned value as a `ValueTree` so the report can show it. Anything the wire
 * cannot name — a live function, a cycle — becomes an `opaque` node that says what it was,
 * rather than a plausible-looking stand-in.
 */
export function toValueTree(value: unknown, seen = new WeakSet<object>()): ValueTree {
  if (value === null) return { node: "null" };
  if (value === undefined) return { node: "undefined" };
  if (typeof value === "boolean") return { node: "boolean", value };
  if (typeof value === "string") return { node: "string", value };
  if (typeof value === "number") return { node: "number", value: numberName(value) };
  if (typeof value === "bigint") return { node: "bigInt", value: value.toString() };
  if (typeof value === "function") return opaque("function");
  if (typeof value === "symbol") return opaque("symbol");
  if (typeof value !== "object") return opaque(typeof value);
  const tag = tagOf(value);
  if (tag === "Promise" || isThenable(value)) return opaque("promise");
  if (tag === "Date") return { node: "string", value: (value as Date).toISOString() };
  if (tag === "RegExp") return { node: "string", value: String(value) };
  if (tag === "Error" || value instanceof Error) {
    const error = value as Error;
    return {
      node: "object",
      fields: [
        ["name", { node: "string", value: error.name }],
        ["message", { node: "string", value: error.message }],
      ],
    };
  }
  if (ArrayBuffer.isView(value)) {
    const view = value as ArrayBufferView;
    const bytes = new Uint8Array(
      view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength),
    );
    return { node: "buffer", bytes: [...bytes] };
  }
  if (tag === "ArrayBuffer") {
    return { node: "buffer", bytes: [...new Uint8Array(value as ArrayBuffer)] };
  }
  if (seen.has(value)) return opaque("circular");
  seen.add(value);
  if (tag === "Map") {
    const entries = [...(value as Map<unknown, unknown>).entries()].map(
      ([key, item]) => [toValueTree(key, seen), toValueTree(item, seen)] as [ValueTree, ValueTree],
    );
    return { node: "map", entries };
  }
  if (tag === "Set") {
    const items = [...(value as Set<unknown>).values()].map((item) => toValueTree(item, seen));
    return { node: "set", items };
  }
  if (Array.isArray(value)) {
    return { node: "array", items: value.map((item) => toValueTree(item, seen)) };
  }
  if (isRecord(value)) {
    return {
      node: "object",
      fields: Object.entries(value).map(
        ([key, item]) => [key, toValueTree(item, seen)] as [string, ValueTree],
      ),
    };
  }
  return opaque(tag.toLowerCase());
}

function prepareSkip(detail: string): PrepareResp {
  // The wire skip carries only a reason; the explanation goes to the plugin's stderr, which the
  // engine surfaces in `afz probe-plugin` output, because `prepare` has no detail field.
  process.stderr.write(`prepare skipped: ${detail}\n`);
  return { ok: false, reason: "unconstructible-parameter" };
}

/** Run the call itself, once the callable exists and the arguments are in hand. */
async function callTarget(job: WorkerJob, resolved: Resolved, maps: ReadonlyMap<string, SourceMap>): Promise<Outcome> {
  const started = performance.now();
  const heapBefore = process.memoryUsage().heapUsed;
  const elapsed = (): number => Math.round(performance.now() - started);
  const grewBytes = (): number => Math.max(0, process.memoryUsage().heapUsed - heapBefore);
  const built = materializeValues(job.values);
  if (!built.ok) {
    // `prepare` already said the graph was buildable, so a refusal here is the harness
    // declining to build an argument. That is a failure with its own `via`, never a rejection:
    // the target never saw the input, so it declared nothing about it.
    const where = built.blockedBy.map((blocker) => `${blocker.type} at ${blocker.path}`).join(", ");
    return failed(
      elapsed(),
      VIA_MATERIALIZATION_BLOCKED,
      failureWithCause(VIA_MATERIALIZATION_BLOCKED, built.detail ?? where),
    );
  }
  try {
    const raw = resolved.call(...built.args);
    const settled = isThenable(raw) ? await raw : raw;
    return {
      kind: "ok",
      durationMs: elapsed(),
      memoryBytes: grewBytes(),
      value: toValueTree(settled),
    };
  } catch (error) {
    const cause = toHarnessError(error, maps);
    if (cause instanceof StubCallError) {
      // The target called a callback the generator gave it, and that callback was configured
      // to throw. The fault came from the harness, so it gets its own `via`.
      return failed(elapsed(), VIA_STUB_CALL, signatureOf(cause));
    }
    if (cause instanceof MalformedValueError) {
      // The payload was not a `ValueTree`. Only an engine bug can produce that, and naming it
      // keeps it from being read as a fault inside the target.
      return failed(elapsed(), VIA_MALFORMED_VALUE, signatureOf(cause));
    }
    const verdict = classifyThrown(cause, job.guardSites, job.root, job.rejectClasses);
    return verdict.kind === "failed"
      ? failed(elapsed(), verdict.via, verdict.fault)
      : { kind: "rejected", durationMs: elapsed(), via: verdict.via, detail: verdict.detail };
  }
}

async function run(job: WorkerJob): Promise<void> {
  inFlight = job.id;
  const probe = job.collectCoverage ? coverage : null;
  let maps: ReadonlyMap<string, SourceMap> = new Map();
  try {
    // The window this call is reported from was opened while the worker was idle, which is the
    // only place its cost does not land on the call. `armed()` has already settled in the normal
    // case; awaiting it is what keeps the first call of a fresh worker from compiling its modules
    // before the profiler is running and reporting nothing.
    if (probe !== null) await probe.armed();

    let realm: Realm;
    try {
      const graph = graphs.get(job.graphKey);
      if (graph === undefined) {
        throw new Error(`${LOAD_FAILED}: this worker was never given module graph ${job.graphKey}`);
      }
      realm = await evaluateRealm(graph, job.moduleUrl);
      maps = realm.maps;
    } catch (error) {
      if (probe !== null) await probe.take(job.root).catch(() => undefined);
      send({ type: "load-error", id: job.id, code: loadFailureCode(error), message: loadDetail(error) });
      return;
    }

    let resolved: Resolved;
    try {
      resolved = resolveCallable(realm.namespace, job.name);
    } catch (error) {
      const signature = signatureOf(toHarnessError(error, maps));
      if (job.mode === "prepare") {
        send({ type: "prepared", id: job.id, state: prepareSkip(signature.message) });
      } else {
        await finish(job, probe, maps, failed(0, "resolve-failed", signature));
      }
      return;
    }

    if (job.mode === "prepare") {
      if (probe !== null) await probe.take(job.root).catch(() => undefined);
      send({
        type: "prepared",
        id: job.id,
        state: { ok: true, materialized: { handle: `js-ts:${job.targetId}`, summary: resolved.built } },
      });
      return;
    }

    const outcome = await callTarget(job, resolved, maps);
    await finish(job, probe, maps, outcome);
  } catch (error) {
    // A fault in the harness, not in the target: the call never produced an outcome, and saying
    // so is better than leaving the plugin waiting for a reply that will not come.
    send({
      type: "load-error",
      id: job.id,
      code: LOAD_FAILED,
      message: `invoke worker: ${loadDetail(error)}`,
    });
  } finally {
    inFlight = null;
    // Re-arm now, after the reply is out: the stop-and-start cycle drops every script compiled
    // before it, which is what keeps the next digest from carrying this call's blocks.
    if (probe !== null) await probe.rear().catch(() => undefined);
  }
}

/** Take what the call covered, then reply. */
async function finish(
  job: WorkerJob,
  probe: CoverageProbe | null,
  maps: ReadonlyMap<string, SourceMap>,
  outcome: Outcome,
): Promise<void> {
  if (probe === null) {
    send({ type: "outcome", id: job.id, outcome, entries: [], reachable: [] });
    return;
  }
  const { entries, all } = await probe.take(job.root, maps);
  send({ type: "outcome", id: job.id, outcome, entries, reachable: all });
}

/**
 * An unhandled rejection means the target started work it never awaited. The design calls that a
 * failure, so it is reported rather than swallowed by the thread exiting quietly. In a warm worker
 * it can surface while a later call is running, and it is reported against that call: the call
 * that was in flight is the only one it can be attributed to. With no call in flight there is
 * nothing to attribute it to, so the thread exits and the engine sees a lost plugin rather than a
 * clean result.
 */
process.on("unhandledRejection", (reason) => {
  const id = inFlight;
  if (id === null) {
    process.exit(1);
  }
  inFlight = null;
  send({
    type: "outcome",
    id,
    entries: [],
    reachable: [],
    outcome: failed(0, VIA_UNHANDLED_REJECTION, failureWithCause(VIA_UNHANDLED_REJECTION, String(reason))),
  });
});

if (parentPort !== null) {
  // One job at a time. The engine keeps one call in flight per plugin process, but a job now
  // ends by re-arming coverage for the next one, and a job that started while that was still
  // running would compile its modules with coverage off and be dropped from its own report.
  let queued: Promise<void> = Promise.resolve();
  parentPort.on("message", (request: WorkerRequest) => {
    if (request.type === "prime") {
      graphs.set(request.graph.key, request.graph);
      return;
    }
    queued = queued.then(() => run(request)).catch(() => undefined);
  });
}

// One invocation, one worker thread. The thread loads the target module, builds the receiver,
// materializes the arguments, calls, and reports an `Outcome`. It exists as a separate thread
// because a target that loops forever must not take the plugin process down with it, and
// because `inspector.Session` has to be created on the thread that runs the code.
//
// The worker speaks a small internal protocol to `server.ts`. The wire protocol belongs to the
// engine and never crosses in here.

import { parentPort, workerData } from "node:worker_threads";

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
import { withCoverage } from "./coverage.js";
import {
  MalformedValueError,
  MaterializationError,
  StubCallError,
  materializeValues,
} from "./materialize.js";
import { isCallable, isConstructable, isRecord, type Callable } from "./guards.js";

export const LOAD_UNSUPPORTED_TYPESCRIPT_SYNTAX = "unsupported-typescript-syntax";
/**
 * A specifier type-stripping cannot resolve, such as `./ledger.js` written next to `ledger.ts` —
 * the NodeNext convention most TypeScript projects use. `tsx` resolves it, so the server retries
 * the module under that loader instead of reporting the target as unbuildable.
 */
export const LOAD_MODULE_NOT_FOUND = "module-not-found";
export const LOAD_FAILED = "load-failed";

export interface WorkerJob {
  mode: "prepare" | "invoke";
  root: string;
  moduleUrl: string;
  /** `convert`, or `Converter.convert` for a class member. */
  name: string;
  values: ValueTree[];
  guardSites: number[];
  rejectClasses: string[];
  collectCoverage: boolean;
  targetId: string;
}

export type WorkerReply =
  | { type: "prepared"; state: PrepareResp }
  | { type: "outcome"; outcome: Outcome; entries: CoverageEntry[]; reachable: CoverageEntry[] }
  | { type: "load-error"; code: string; message: string };

interface Resolved {
  call: Callable;
  /** What the worker had to build to make the call possible, for `prepare`'s summary. */
  built: string;
}

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
    if (error.code === "ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX") return LOAD_UNSUPPORTED_TYPESCRIPT_SYNTAX;
    if (error.code === "ERR_MODULE_NOT_FOUND") return LOAD_MODULE_NOT_FOUND;
  }
  return LOAD_FAILED;
}

async function loadTarget(moduleUrl: string): Promise<Record<string, unknown>> {
  // The target module is named by the scan project at run time, so the specifier is not
  // knowable at build time and a static import cannot express it.
  const namespace: unknown = await import(moduleUrl);
  if (!isRecord(namespace)) {
    throw new MaterializationError(`loaded module ${moduleUrl} exposed no named exports`);
  }
  return namespace;
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
  if (value instanceof Promise) return opaque("promise");
  if (value instanceof Date) return { node: "string", value: value.toISOString() };
  if (value instanceof RegExp) return { node: "string", value: value.toString() };
  if (value instanceof Error) {
    return {
      node: "object",
      fields: [
        ["name", { node: "string", value: value.name }],
        ["message", { node: "string", value: value.message }],
      ],
    };
  }
  if (ArrayBuffer.isView(value)) {
    const bytes = new Uint8Array(
      value.buffer.slice(value.byteOffset, value.byteOffset + value.byteLength),
    );
    return { node: "buffer", bytes: [...bytes] };
  }
  if (value instanceof ArrayBuffer) {
    return { node: "buffer", bytes: [...new Uint8Array(value)] };
  }
  if (seen.has(value)) return opaque("circular");
  seen.add(value);
  if (value instanceof Map) {
    return {
      node: "map",
      entries: [...value.entries()].map(
        ([key, item]) => [toValueTree(key, seen), toValueTree(item, seen)] as [ValueTree, ValueTree],
      ),
    };
  }
  if (value instanceof Set) {
    return { node: "set", items: [...value.values()].map((item) => toValueTree(item, seen)) };
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
  return opaque(typeof value);
}

function prepareSkip(detail: string): PrepareResp {
  // The wire skip carries only a reason; the explanation goes to the plugin's stderr, which the
  // engine surfaces in `afz probe-plugin` output, because `prepare` has no detail field.
  process.stderr.write(`prepare skipped: ${detail}\n`);
  return { ok: false, reason: "unconstructible-parameter" };
}

async function run(job: WorkerJob): Promise<void> {
  let mod: Record<string, unknown>;
  try {
    mod = await loadTarget(job.moduleUrl);
  } catch (error) {
    send({
      type: "load-error",
      code: loadFailureCode(error),
      message: error instanceof Error ? `${error.name}: ${error.message}` : String(error),
    });
    return;
  }

  let resolved: Resolved;
  try {
    resolved = resolveCallable(mod, job.name);
  } catch (error) {
    const signature = signatureOf(error);
    if (job.mode === "prepare") {
      send({ type: "prepared", state: prepareSkip(signature.message) });
    } else {
      send({
        type: "outcome",
        outcome: failed(0, "resolve-failed", signature),
        entries: [],
        reachable: [],
      });
    }
    return;
  }

  if (job.mode === "prepare") {
    send({
      type: "prepared",
      state: {
        ok: true,
        materialized: { handle: `js-ts:${job.targetId}`, summary: resolved.built },
      },
    });
    return;
  }

  const started = performance.now();
  const heapBefore = process.memoryUsage().heapUsed;
  const body = async (): Promise<Outcome> => {
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
      const settled = raw instanceof Promise ? await raw : raw;
      return {
        kind: "ok",
        durationMs: elapsed(),
        memoryBytes: grewBytes(),
        value: toValueTree(settled),
      };
    } catch (error) {
      if (error instanceof StubCallError) {
        // The target called a callback the generator gave it, and that callback was configured
        // to throw. The fault came from the harness, so it gets its own `via`.
        return failed(elapsed(), VIA_STUB_CALL, signatureOf(error));
      }
      if (error instanceof MalformedValueError) {
        // The payload was not a `ValueTree`. Only an engine bug can produce that, and naming it
        // keeps it from being read as a fault inside the target.
        return failed(elapsed(), VIA_MALFORMED_VALUE, signatureOf(error));
      }
      const verdict = classifyThrown(error, job.guardSites, job.root, job.rejectClasses);
      return verdict.kind === "failed"
        ? failed(elapsed(), verdict.via, verdict.fault)
        : { kind: "rejected", durationMs: elapsed(), via: verdict.via, detail: verdict.detail };
    }
  };

  if (!job.collectCoverage) {
    send({ type: "outcome", outcome: await body(), entries: [], reachable: [] });
    return;
  }
  const { value: outcome, entries, all } = await withCoverage(job.root, body);
  send({ type: "outcome", outcome, entries, reachable: all });
}

/**
 * An unhandled rejection means the target started work it never awaited. The design calls that
 * a failure, so it is reported rather than swallowed by the thread exiting quietly.
 */
process.on("unhandledRejection", (reason) => {
  send({
    type: "outcome",
    entries: [],
    reachable: [],
    outcome: failed(0, VIA_UNHANDLED_REJECTION, failureWithCause(VIA_UNHANDLED_REJECTION, String(reason))),
  });
});

// `server.ts` posts this job, so the shape is ours and the boundary is an assertion, not a parse.
const job = workerData as WorkerJob | undefined;
if (typeof job?.targetId === "string") {
  await run(job);
} else {
  send({ type: "load-error", code: LOAD_FAILED, message: "invoke worker started without a job" });
}

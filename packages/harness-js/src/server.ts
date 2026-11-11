#!/usr/bin/env node
// The `js-ts` harness plugin: a JSON-RPC server over stdio that answers the six protocol methods.
//
// One warm worker thread per plugin process runs the invocations, and each invocation gets a fresh
// realm inside it (docs/decisions/0006). This file owns the thread's life: what it is given, when
// its answer is accepted, and when it is retired so the next call starts from a clean isolate.
//
// Nothing in this file decides whether a target is broken. It loads code, calls it, reports
// `ok`, `rejected`, or `failed`, and lets the engine apply the oracles.

import { createInterface } from "node:readline";
import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { Worker } from "node:worker_threads";

import type {
  Capability,
  CoverageEntry,
  DiscoverReq,
  Discovery,
  Handshake,
  JsonValue,
  Outcome,
  PrepareReq,
  PrepareResp,
  Response,
  TargetDescriptor,
  ValueTree,
} from "./protocol.js";
import {
  failure,
  INVALID_REQUEST,
  METHOD_HANDSHAKE,
  METHOD_DISCOVER,
  METHOD_INVOKE,
  METHOD_MINIMIZE,
  METHOD_NOT_FOUND,
  METHOD_PREPARE,
  METHOD_COVERAGE_MAP,
  PARSE_ERROR,
  PLUGIN_ERROR,
  PROTOCOL_VERSION,
  success,
  writeLine,
} from "./protocol.js";
import { DEFAULT_REJECT_CLASSES, failureWithCause, VIA_MODULE_LOAD, VIA_WORKER_DEAD } from "./classify.js";
import { isRecord } from "./guards.js";
import { discover } from "./discover.js";
import { Transpiler, type ModuleGraph } from "./transpile.js";
import type { WorkerJob, WorkerReply, WorkerRequest } from "./invoke.js";

export const HARNESS_NAME = "js-ts";
export const HARNESS_VERSION = "0.1.0";

/**
 * What this build can do. `minimize` is absent until the plugin can shrink a call in target
 * terms; the engine then falls back to removing arguments and reducing structure.
 */
export const CAPABILITIES: Capability[] = ["coverage", "stateful", "concurrent"];

/**
 * A live worker thread and what it currently holds. One job runs at a time on a thread; a second
 * job that arrives while one is in flight gets its own thread rather than sharing this one.
 */
interface WarmWorker {
  thread: Worker;
  /** Module graphs this thread has been given, by key. */
  primed: Set<string>;
  busy: boolean;
  /** Jobs this thread has answered; the thread is retired at `INVOCATIONS_PER_WORKER`. */
  calls: number;
}

/**
 * How many invocations one warm worker serves before it is replaced by a fresh thread.
 *
 * A fresh realm per invocation is the isolation the design asks for, and V8 does not give the
 * memory back: `vm.createContext` plus `SourceTextModule` retains ~0.2 MB of the thread's heap per
 * invocation on the billing test target, measured as heap 21 MB to 101 MB over 400 invocations with no
 * plateau across 3000 realms in a bare V8 loop, and no garbage collection inside the thread can be
 * asked to fix it — a worker rejects `--expose-gc` and every heap flag in `execArgv`. The drift
 * shows up as latency too: building the realm went from 1.0 ms to 1.8 ms and the coverage report
 * from 2.4 ms to 5.3 ms over the same 400 calls.
 *
 * Terminating the thread is the only release there is, so it is done on a schedule as well as after
 * a fault. 128 invocations keeps a worker near 45 MB and inside the flat part of the drift, and
 * costs the 24 ms of a fresh thread once per 128 calls, ~0.2 ms amortized.
 */
const INVOCATIONS_PER_WORKER = 128;

/** A job's answer: the worker's reply, or why there will not be one. */
interface RunResult {
  reply: WorkerReply | null;
  detail: string;
}

interface Pending {
  entry: WarmWorker;
  settle: (result: RunResult) => void;
}

function moduleLoadFailure(detail: string): Outcome {
  return {
    kind: "failed",
    durationMs: 0,
    via: VIA_MODULE_LOAD,
    fault: { class: "ModuleLoadError", message: detail },
  };
}

function failureDetail(error: unknown): string {
  return error instanceof Error ? `${error.name}: ${error.message}` : String(error);
}

/** `codebase.rejectClasses` from the scan project, when the project declares it. */
async function readRejectClasses(configPath: string | null | undefined): Promise<string[]> {
  if (!configPath) return [...DEFAULT_REJECT_CLASSES];
  let text: string;
  try {
    text = await readFile(configPath, "utf8");
  } catch {
    return [...DEFAULT_REJECT_CLASSES];
  }
  const parsed: unknown = JSON.parse(text);
  if (!isRecord(parsed) || !isRecord(parsed.codebase)) return [...DEFAULT_REJECT_CLASSES];
  const declared = parsed.codebase.rejectClasses;
  if (!Array.isArray(declared)) return [...DEFAULT_REJECT_CLASSES];
  return declared.filter((item): item is string => typeof item === "string");
}

export class Harness {
  private root: string | null = null;
  private rejectClasses: string[] = [...DEFAULT_REJECT_CLASSES];
  private targets = new Map<string, TargetDescriptor>();
  private transpiler = new Transpiler();
  private workers = new Set<WarmWorker>();
  private pending = new Map<number, Pending>();
  private nextJobId = 1;
  private edges = new Map<number, CoverageEntry>();

  handshake(): Handshake {
    return {
      protocolVersion: PROTOCOL_VERSION,
      name: HARNESS_NAME,
      version: HARNESS_VERSION,
      targetClass: "design-time",
      capabilities: CAPABILITIES,
      languages: ["typescript", "javascript"],
      discoverRequiresExecution: false,
    };
  }

  async discover(req: DiscoverReq): Promise<Discovery> {
    this.root = req.root;
    this.rejectClasses = await readRejectClasses(req.configPath);
    const discovery = await discover(req);
    this.targets = new Map(discovery.targets.map((target) => [target.id, target]));
    return discovery;
  }

  private target(targetId: string): TargetDescriptor {
    const target = this.targets.get(targetId);
    if (!target) throw new InvalidRequest(`unknown target id ${targetId}; run discover first`);
    return target;
  }

  /** The scan project root, recorded by `discover` and needed by every later call. */
  private scanRoot(): string {
    if (!this.root) throw new InvalidRequest("discover has not run, so the scan project root is unknown");
    return this.root;
  }

  /**
   * The target module and its whole import closure, transpiled once per plugin process. The realm
   * the worker builds is made from this text, so no loader runs on the invoke path.
   */
  private async graph(target: TargetDescriptor): Promise<ModuleGraph> {
    const file = target.origin.file;
    if (!file) throw new Error(`target ${target.id} has no source file to load`);
    return await this.transpiler.graph(file);
  }

  private job(
    target: TargetDescriptor,
    graph: ModuleGraph,
    values: ValueTree[],
    mode: "prepare" | "invoke",
    collectCoverage: boolean,
  ): WorkerJob {
    return {
      type: "job",
      id: this.nextJobId++,
      mode,
      root: this.scanRoot(),
      graphKey: graph.key,
      moduleUrl: graph.entryUrl,
      name: target.name,
      values,
      guardSites: target.guardSites ?? [],
      rejectClasses: this.rejectClasses,
      collectCoverage,
      targetId: target.id,
    };
  }

  /**
   * A thread to run one job on, holding `graph`. An idle thread that already has the closure is
   * preferred; a busy one is never used, because a thread runs one invocation at a time and the
   * realm is what isolates calls from each other, not the scheduler.
   */
  private acquire(graph: ModuleGraph): WarmWorker {
    let fallback: WarmWorker | null = null;
    for (const candidate of this.workers) {
      if (candidate.busy) continue;
      if (candidate.primed.has(graph.key)) return candidate;
      fallback ??= candidate;
    }
    const entry = fallback ?? this.spawn();
    this.prime(entry, graph);
    return entry;
  }

  private spawn(): WarmWorker {
    const entry: WarmWorker = {
      thread: new Worker(new URL("./invoke.js", import.meta.url), {
        // `vm.SourceTextModule` gives each invocation its fresh realm, and the flag is per thread:
        // the plugin process itself stays a plain Node process. The experimental warning is turned
        // off because this plugin's stderr is the engine's log, and the warning says nothing the
        // handshake does not.
        execArgv: ["--experimental-vm-modules", "--disable-warning=ExperimentalWarning"],
      }),
      primed: new Set<string>(),
      busy: false,
      calls: 0,
    };
    entry.thread.on("message", (reply: WorkerReply) => this.accept(reply));
    entry.thread.once("error", (error: Error) => this.lost(entry, error.message));
    entry.thread.once("exit", (code: number) => this.lost(entry, `invoke worker exited with code ${code}`));
    this.workers.add(entry);
    return entry;
  }

  private prime(entry: WarmWorker, graph: ModuleGraph): void {
    if (entry.primed.has(graph.key)) return;
    entry.primed.add(graph.key);
    entry.thread.postMessage({ type: "prime", graph } satisfies WorkerRequest);
  }

  /** Post one job and wait for the reply that carries its id. */
  private async run(entry: WarmWorker, job: WorkerJob): Promise<RunResult> {
    const { promise, resolve } = Promise.withResolvers<RunResult>();
    entry.busy = true;
    this.pending.set(job.id, { entry, settle: resolve });
    entry.thread.postMessage({ ...job } satisfies WorkerRequest);
    const result = await promise;
    entry.calls += 1;
    // Retired only now, with nothing in flight: the reply for this job has been answered, and the
    // next `acquire` finds no idle thread and spawns one.
    if (entry.calls >= INVOCATIONS_PER_WORKER) this.retire(entry);
    return result;
  }

  private accept(reply: WorkerReply): void {
    const waiting = this.pending.get(reply.id);
    if (waiting === undefined) return;
    // A warm worker can still be sending an answer to a call this plugin already settled, such as
    // a rejection that surfaced after the reply for the call that raised it. The first answer wins.
    this.pending.delete(reply.id);
    waiting.entry.busy = false;
    waiting.settle({ reply, detail: "" });
  }

  /** A thread that died or exited: nothing more will be heard about its job, or from it. */
  private lost(entry: WarmWorker, detail: string): void {
    this.workers.delete(entry);
    for (const [id, waiting] of this.pending) {
      if (waiting.entry !== entry) continue;
      this.pending.delete(id);
      waiting.settle({ reply: null, detail });
    }
  }

  /**
   * Retire a thread so the next invocation starts from a clean isolate: a call that failed, a
   * module that did not load, or a thread that stopped answering may have left state behind that
   * the next call should not inherit.
   */
  private retire(entry: WarmWorker): void {
    if (!this.workers.delete(entry)) return;
    void entry.thread.terminate().catch(() => undefined);
  }

  async prepare(req: PrepareReq): Promise<PrepareResp> {
    const target = this.target(req.targetId);
    let graph: ModuleGraph;
    try {
      graph = await this.graph(target);
    } catch (error) {
      process.stderr.write(`prepare ${req.targetId}: ${failureDetail(error)}\n`);
      return { ok: false, reason: "discovery-error" };
    }
    const entry = this.acquire(graph);
    const result = await this.run(entry, this.job(target, graph, req.values, "prepare", false));
    if (result.reply?.type === "prepared") return result.reply.state;
    this.retire(entry);
    const detail = result.reply?.type === "load-error" ? result.reply.message : result.detail;
    // The wire skip carries only a reason, so the explanation goes to stderr, where
    // `afz probe-plugin` and the engine's log show it.
    process.stderr.write(`prepare ${req.targetId}: ${detail}\n`);
    return { ok: false, reason: "discovery-error" };
  }

  async invoke(req: InvokeCall): Promise<Outcome> {
    const target = this.target(req.targetId);
    let graph: ModuleGraph;
    try {
      graph = await this.graph(target);
    } catch (error) {
      return moduleLoadFailure(failureDetail(error));
    }
    const entry = this.acquire(graph);
    const result = await this.run(entry, this.job(target, graph, req.values, "invoke", req.collectCoverage));
    const reply = result.reply;
    if (reply?.type === "outcome") {
      // `reachable` is the whole block table of the modules this call compiled, a superset of the
      // blocks the call hit; the map answers with all of it, so the engine can divide by the edges
      // that exist rather than only the ones already seen.
      for (const entry of reply.reachable) this.edges.set(entry.edge, entry);
      const outcome = reply.outcome;
      if (req.collectCoverage) outcome.coverage = reply.entries.map((edge) => edge.edge);
      if (outcome.kind === "failed") this.retire(entry);
      return outcome;
    }
    this.retire(entry);
    if (reply?.type === "load-error") return moduleLoadFailure(reply.message);
    return {
      kind: "failed",
      durationMs: 0,
      via: VIA_WORKER_DEAD,
      fault: failureWithCause(VIA_WORKER_DEAD, result.detail),
    };
  }

  /** Absent from the handshake capability list, so the engine minimizes structurally. */
  minimize(): never {
    throw new InvalidRequest("this build of the js-ts harness does not implement minimize");
  }

  coverageMap(): { entries: CoverageEntry[] } {
    const entries = [...this.edges.values()].sort((a, b) => a.edge - b.edge);
    return { entries };
  }
}


// ---------------------------------------------------------------- transport

/** A request the engine sent that this plugin cannot honor. Reported as `invalid request`. */
class InvalidRequest extends Error {}

class MethodNotFound extends Error {
  constructor(method: string) {
    super(`unknown method ${method}`);
  }
}

/**
 * The request boundary. Each field the plugin acts on is checked here; the `values` trees are
 * checked by `materialize`, which rejects a node tag it does not know.
 */
function parseDiscover(params: unknown): DiscoverReq {
  if (!isRecord(params)) throw new InvalidRequest("discover params must be an object");
  if (typeof params.root !== "string") throw new InvalidRequest("discover params.root must be a string");
  const req: DiscoverReq = { root: params.root };
  if (typeof params.include === "string" || Array.isArray(params.include)) {
    req.include = toStringList(params.include, "include");
  }
  if (typeof params.exclude === "string" || Array.isArray(params.exclude)) {
    req.exclude = toStringList(params.exclude, "exclude");
  }
  if (typeof params.configPath === "string") req.configPath = params.configPath;
  return req;
}

function toStringList(value: unknown, field: string): string[] {
  const items = Array.isArray(value) ? value : [value];
  return items.filter((item): item is string => typeof item === "string");
}

function parseCall(params: unknown): { targetId: string; values: ValueTree[] } {
  if (!isRecord(params)) throw new InvalidRequest("params must be an object");
  if (typeof params.targetId !== "string") throw new InvalidRequest("params.targetId must be a string");
  if (!Array.isArray(params.values)) throw new InvalidRequest("params.values must be an array");
  return { targetId: params.targetId, values: params.values as ValueTree[] };
}

/** The `invoke` fields this plugin acts on. `invocationId` and `deadlineMs` belong to the engine. */
interface InvokeCall {
  targetId: string;
  values: ValueTree[];
  collectCoverage: boolean;
}

function parseInvoke(params: unknown): InvokeCall {
  const call = parseCall(params);
  const collectCoverage = isRecord(params) && params.collectCoverage === true;
  return { ...call, collectCoverage };
}

async function dispatch(harness: Harness, method: string, params: unknown): Promise<unknown> {
  switch (method) {
    case METHOD_HANDSHAKE:
      return harness.handshake();
    case METHOD_DISCOVER:
      return harness.discover(parseDiscover(params));
    case METHOD_PREPARE:
      return harness.prepare(parseCall(params));
    case METHOD_INVOKE:
      return harness.invoke(parseInvoke(params));
    case METHOD_MINIMIZE:
      return harness.minimize();
    case METHOD_COVERAGE_MAP:
      return harness.coverageMap();
    default:
      throw new MethodNotFound(method);
  }
}

export async function serve(input: NodeJS.ReadableStream, output: NodeJS.WritableStream): Promise<void> {
  const harness = new Harness();
  const lines = createInterface({ input, crlfDelay: Infinity });
  lines.on("line", (line) => {
    if (line.trim() !== "") void handleLine(harness, output, line);
  });
  const { promise, resolve } = Promise.withResolvers<void>();
  lines.once("close", resolve);
  await promise;
}

async function handleLine(harness: Harness, output: NodeJS.WritableStream, line: string): Promise<void> {
  let request: unknown;
  try {
    request = JSON.parse(line);
  } catch {
    writeLine(output, failure(null, PARSE_ERROR, "line is not valid JSON"));
    return;
  }
  if (!isRecord(request) || typeof request.method !== "string") {
    writeLine(output, failure(null, INVALID_REQUEST, "request has no method name"));
    return;
  }
  const id = typeof request.id === "number" ? request.id : 0;
  try {
    const result = (await dispatch(harness, request.method, request.params)) as JsonValue;
    writeLine(output, success(id, result));
  } catch (error) {
    writeLine(output, errorToObject(id, error));
  }
}

function errorToObject(id: number, error: unknown): Response {
  const detail = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
  if (error instanceof MethodNotFound) return failure(id, METHOD_NOT_FOUND, detail);
  if (error instanceof InvalidRequest) return failure(id, INVALID_REQUEST, detail);
  return failure(id, PLUGIN_ERROR, detail);
}

const invokedDirectly = process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;
if (invokedDirectly) {
  await serve(process.stdin, process.stdout);
}

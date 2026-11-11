#!/usr/bin/env node
// The `js-ts` harness plugin: a JSON-RPC server over stdio that answers the six protocol
// methods and runs every invocation in its own worker thread.
//
// Nothing in this file decides whether a target is broken. It loads code, calls it, reports
// `ok`, `rejected`, or `failed`, and lets the engine apply the oracles.

import { createInterface } from "node:readline";
import { createRequire } from "node:module";
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
import {
  DEFAULT_REJECT_CLASSES,
  failureWithCause,
  VIA_MODULE_LOAD,
  VIA_WORKER_DEAD,
} from "./classify.js";
import { isRecord } from "./guards.js";
import { discover } from "./discover.js";
import { LOAD_MODULE_NOT_FOUND, LOAD_UNSUPPORTED_TYPESCRIPT_SYNTAX, type WorkerJob, type WorkerReply } from "./invoke.js";

/**
 * Load failures that say the loader is wrong rather than the target unreachable. `tsx` is a
 * full TypeScript loader, so it accepts `enum` and namespace syntax and resolves a `./x.js`
 * specifier that names an `x.ts` file; a failure after the retry is the target's, not the loader's.
 */
const LOADER_RETRY_CODES = new Set<string>([LOAD_UNSUPPORTED_TYPESCRIPT_SYNTAX, LOAD_MODULE_NOT_FOUND]);

/**
 * The `tsx` entry resolved against this package rather than by name. A worker thread resolves
 * `--import` from the working directory, and the engine starts this plugin from the scan
 * project's directory, where a bare `tsx` would not be found.
 */
const TSX_ENTRY = createRequire(import.meta.url).resolve("tsx");

export const HARNESS_NAME = "js-ts";
export const HARNESS_VERSION = "0.1.0";

/**
 * What this build can do. `minimize` is absent until the plugin can shrink a call in target
 * terms; the engine then falls back to removing arguments and reducing structure.
 */
export const CAPABILITIES: Capability[] = ["coverage", "stateful", "concurrent"];

type LoaderMode = "strip" | "tsx";

interface RunResult {
  reply: WorkerReply | null;
  exitCode: number;
  error: Error | null;
}

/** Spawn one worker for one job and wait for its single reply. */
function runJob(job: WorkerJob, mode: LoaderMode): Promise<RunResult> {
  const { promise, resolve } = Promise.withResolvers<RunResult>();
  const worker = new Worker(new URL("./invoke.js", import.meta.url), {
    workerData: job,
    // The flag is per thread: the plugin process stays a plain Node process, and only the
    // thread that imports the target needs a TypeScript loader. `tsx` is the fallback for
    // syntax strip-only mode rejects and for a `./x.js` specifier that names `x.ts`.
    execArgv: mode === "tsx" ? ["--import", TSX_ENTRY] : ["--experimental-strip-types"],
  });
  let settled = false;
  const finish = (result: RunResult): void => {
    if (settled) return;
    settled = true;
    // A worker thread outlives its reply unless it is terminated: it keeps its isolate, its
    // module registry and its inspector session, so every invocation would add another live
    // thread to the process and the next invocation would cost more than the last.
    void worker.terminate().catch(() => undefined);
    resolve(result);
  };
  worker.once("message", (reply: WorkerReply) => finish({ reply, exitCode: 0, error: null }));
  worker.once("error", (error: Error) => finish({ reply: null, exitCode: 1, error }));
  worker.once("exit", (code) => finish({ reply: null, exitCode: code, error: null }));
  return promise;
}

function moduleUrlOf(target: TargetDescriptor): string {
  const file = target.origin.file;
  if (!file) throw new Error(`target ${target.id} has no source file to load`);
  return pathToFileURL(file).href;
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
  private loaderModes = new Map<string, LoaderMode>();
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

  private job(target: TargetDescriptor, values: ValueTree[], mode: "prepare" | "invoke", collectCoverage: boolean): WorkerJob {
    return {
      mode,
      root: this.scanRoot(),
      moduleUrl: moduleUrlOf(target),
      name: target.name,
      values,
      guardSites: target.guardSites ?? [],
      rejectClasses: this.rejectClasses,
      collectCoverage,
      targetId: target.id,
    };
  }

  /** Run one job, switching the thread's loader if the target needs more than type stripping. */
  private async run(job: WorkerJob): Promise<RunResult> {
    const mode = this.loaderModes.get(job.moduleUrl) ?? "strip";
    const first = await runJob(job, mode);
    const loadError = first.reply?.type === "load-error" ? first.reply : null;
    if (mode === "strip" && loadError !== null && LOADER_RETRY_CODES.has(loadError.code)) {
      this.loaderModes.set(job.moduleUrl, "tsx");
      const retry = await runJob(job, "tsx");
      if (retry.reply?.type === "load-error") {
        this.loaderModes.set(job.moduleUrl, "strip");
      }
      return retry;
    }
    return first;
  }

  async prepare(req: PrepareReq): Promise<PrepareResp> {
    const target = this.target(req.targetId);
    const result = await this.run(this.job(target, req.values, "prepare", false));
    if (result.reply?.type === "prepared") return result.reply.state;
    const detail =
      result.reply?.type === "load-error"
        ? result.reply.message
        : `prepare worker exited with code ${result.exitCode}`;
    // The wire skip carries only a reason, so the explanation goes to stderr, where
    // `afz probe-plugin` and the engine's log show it.
    process.stderr.write(`prepare ${req.targetId}: ${detail}\n`);
    return { ok: false, reason: "discovery-error" };
  }

  async invoke(req: InvokeCall): Promise<Outcome> {
    const target = this.target(req.targetId);
    const result = await this.run(
      this.job(target, req.values, "invoke", req.collectCoverage),
    );
    if (result.reply?.type === "outcome") {
      // `reachable` is the whole block table of the modules this process has loaded, a superset
      // of the blocks this call hit; the map answers with all of it, so the engine can divide by
      // the edges that exist rather than only the ones already seen.
      for (const entry of result.reply.reachable) this.edges.set(entry.edge, entry);
      const coverage = result.reply.entries.map((entry) => entry.edge);
      const outcome = result.reply.outcome;
      if (req.collectCoverage) outcome.coverage = coverage;
      return outcome;
    }
    if (result.reply?.type === "load-error") {
      return {
        kind: "failed",
        durationMs: 0,
        via: VIA_MODULE_LOAD,
        fault: { class: "ModuleLoadError", message: result.reply.message },
      };
    }
    const detail = result.error?.message ?? `invoke worker exited with code ${result.exitCode}`;
    return {
      kind: "failed",
      durationMs: 0,
      via: VIA_WORKER_DEAD,
      fault: failureWithCause(VIA_WORKER_DEAD, detail),
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

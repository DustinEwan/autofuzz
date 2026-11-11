// The wire `ValueTree` is JSON. The target wants a live object graph. This module is the only
// place that crosses that boundary, and it never runs target code: it builds values, and
// `invoke.ts` hands them to the target.
//
// Three channels, deliberately distinct, and `invoke.ts` maps each to its own `via`:
//
// - A value that cannot be built — an `opaque` node, or a graph past the materialization
//   budget — is returned as `{ ok: false, reason, blockedBy, detail }`, never thrown. `prepare`
//   turns it into a skip the inventory counts, which is the honest accounting
//   `docs/architecture.md` requires, and `invoke` reports it as a refusal of the harness.
// - A payload that is not a `ValueTree` at all throws `MalformedValueError`. Reporting a
//   protocol violation as a skipped target would hide a bug inside an inventory number.
// - A `throw` stub that the target actually calls throws `StubCallError`, so the fault names
//   the harness as its author.

import type { Blocker, FunctionBehavior, SkipReason, ValueTree } from "./protocol.js";

// ---------------------------------------------------------------- public surface

/**
 * Which live type a `buffer` node becomes. The wire carries bytes only — `lower.ts` folds
 * `Uint8Array`, `ArrayBuffer` and `Buffer` into one `buffer` kind — so the flavor is a harness
 * setting, not a value property. `uint8Array` is the default because it satisfies every
 * `ArrayBufferView` position and converts to either of the other two with one call.
 */
export type BufferKind = "uint8Array" | "arrayBuffer" | "nodeBuffer";

export interface MaterializeOptions {
  /** Behavior for a `function` node that carries none. Matches the generator's default. */
  functionBehavior?: FunctionBehavior;
  /** Live type for a `buffer` node. Default `"uint8Array"`. */
  bufferKind?: BufferKind;
  /** Refuse graphs deeper than this. Default 64, far above the generator's budget of 6. */
  maxDepth?: number;
  /** Refuse graphs with more nodes than this. Default 100000. */
  maxNodes?: number;
}

/** One recorded call of a `captureArguments` stub. */
export interface CapturedCall {
  args: unknown[];
  thisValue: unknown;
}

/** Why the graph could not be built, in the shape `SkippedTarget` reports. */
export interface MaterializeBlocked {
  ok: false;
  reason: SkipReason;
  blockedBy: Blocker[];
  detail?: string;
}

export type MaterializeResult = { ok: true; value: unknown } | MaterializeBlocked;

/**
 * A node the harness cannot turn into a value, reported in the shape `prepare` turns into a
 * skip: `reason` and `blockedBy` say which type is missing and where the call needed it.
 */
export class MaterializationError extends Error {
  reason: SkipReason;
  blockedBy: Blocker[];
  detail: string;

  /**
   * Either the blocked result `materializeValue` reported, or a bare message for a caller that
   * found an unbuildable receiver before any value tree was in hand.
   */
  constructor(cause: string | MaterializeBlocked) {
    const blocked = typeof cause === "string" ? { ok: false as const, reason: BLOCKED_REASON, blockedBy: [] } : cause;
    const where = blocked.blockedBy.map((blocker) => `${blocker.type} at ${blocker.path}`).join(", ");
    const detail = blocked.detail ?? (typeof cause === "string" ? cause : where);
    super(typeof cause === "string" ? cause : `cannot materialize: ${detail}${where === "" ? "" : ` [${where}]`}`);
    this.name = "MaterializationError";
    this.reason = blocked.reason;
    this.blockedBy = blocked.blockedBy;
    this.detail = detail;
  }
}

/** The wire payload is not a `ValueTree`: a missing field, an unknown tag, an out-of-range byte. */
export class MalformedValueError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MalformedValueError";
  }
}

/**
 * Thrown by a `throw` stub when the target calls it. The class is exported so `classify.ts` can
 * tell a harness-raised fault from a target fault: an uncaught `StubCallError` is an artifact of
 * a generated callback, not a bug in the code under test.
 */
export class StubCallError extends Error {
  argCount: number;

  constructor(argCount: number) {
    super(`autofuzz stub invoked with ${argCount} argument(s)`);
    this.name = "StubCallError";
    this.argCount = argCount;
  }
}

// ---------------------------------------------------------------- internals

const DEFAULT_MAX_DEPTH = 64;
const DEFAULT_MAX_NODES = 100_000;
const IDENTIFIER = /^[A-Za-z_$][A-Za-z0-9_$]*$/;

/** The only reason this module can report: the parameter graph is not buildable from values. */
const BLOCKED_REASON: SkipReason = "unconstructible-parameter";

type WireNode = { node: string } & Record<string, unknown>;

interface Context {
  depth: number;
  nodes: number;
  maxDepth: number;
  maxNodes: number;
  functionBehavior: FunctionBehavior;
  bufferKind: BufferKind;
  blockers: Blocker[];
  details: string[];
}

/** Internal control flow: the graph is unbuildable, and `context` says why. */
class Blocked {
  context: Context;

  constructor(context: Context) {
    this.context = context;
  }
}

function newContext(options: MaterializeOptions): Context {
  return {
    depth: 0,
    nodes: 0,
    maxDepth: options.maxDepth ?? DEFAULT_MAX_DEPTH,
    maxNodes: options.maxNodes ?? DEFAULT_MAX_NODES,
    functionBehavior: options.functionBehavior ?? "returnFirst",
    bufferKind: options.bufferKind ?? "uint8Array",
    blockers: [],
    details: [],
  };
}

function blocked(ctx: Context, detail: string): never {
  ctx.details.push(detail);
  throw new Blocked(ctx);
}

/** One node entered: budget both the nesting and the size before recursing further. */
function enterNode(ctx: Context, path: string): void {
  ctx.nodes++;
  if (ctx.nodes > ctx.maxNodes) {
    blocked(ctx, `materialization node budget (${ctx.maxNodes}) exceeded at ${path}`);
  }
  ctx.depth++;
  if (ctx.depth > ctx.maxDepth) {
    blocked(ctx, `materialization depth budget (${ctx.maxDepth}) exceeded at ${path}`);
  }
}

function childPath(path: string, name: string): string {
  return IDENTIFIER.test(name) ? `${path}.${name}` : `${path}[${JSON.stringify(name)}]`;
}

function malformed(path: string, expected: string, got: unknown): never {
  throw new MalformedValueError(`${path}: expected ${expected}, got ${preview(got)}`);
}

/** A short, always-serializable rendering of a bad payload, for the error message. */
function preview(got: unknown): string {
  if (typeof got === "bigint") return `${got}n`;
  if (typeof got !== "object" || got === null) return String(got);
  try {
    return JSON.stringify(got)?.slice(0, 120) ?? "undefined";
  } catch {
    return Array.isArray(got) ? `[${got.length} items]` : "{...}";
  }
}

function asNode(value: unknown, path: string): WireNode {
  if (typeof value !== "object" || value === null || typeof (value as WireNode).node !== "string") {
    malformed(path, "a ValueTree node", value);
  }
  return value as WireNode;
}

function asArray(value: unknown, path: string, field: string): unknown[] {
  if (!Array.isArray(value)) malformed(path, `an array field "${field}"`, value);
  return value;
}

/**
 * The name of the type a plugin cannot build. The committed schema carries a plain string; the
 * mirror in `protocol.ts` types the field as a `TypeNode`, so both spellings are read here.
 */
function opaqueName(ty: unknown): string {
  if (typeof ty === "string") return ty;
  if (typeof ty === "object" && ty !== null) {
    const node = ty as Record<string, unknown>;
    for (const field of ["ty", "name", "format", "scalar"]) {
      const value = node[field];
      if (typeof value === "string") return value;
    }
    if (typeof node.kind === "string") return node.kind;
  }
  return "opaque";
}

function opaqueModule(ty: unknown): string | undefined {
  if (typeof ty === "object" && ty !== null) {
    const module = (ty as Record<string, unknown>).module;
    if (typeof module === "string") return module;
  }
  return undefined;
}

/**
 * `NaN`, `Infinity` and `-Infinity` have no JSON token, so they travel as names. The committed
 * schema spells them the way serde camel-cases the Rust variants (`naN`, `negativeInfinity`);
 * the readable spellings are accepted as well so a hand-written input file loads.
 */
function numberValue(value: unknown, path: string): number {
  if (typeof value === "number") return value;
  switch (value) {
    case "naN":
    case "NaN":
      return Number.NaN;
    case "infinity":
    case "Infinity":
      return Number.POSITIVE_INFINITY;
    case "negativeInfinity":
    case "-Infinity":
      return Number.NEGATIVE_INFINITY;
    default:
      return malformed(path, "a number or a special-number name", value);
  }
}

function asBehavior(value: unknown, path: string, fallback: FunctionBehavior): FunctionBehavior {
  if (value === undefined) return fallback;
  if (value === "returnFirst" || value === "throw" || value === "captureArguments") return value;
  return malformed(path, "a known function behavior", value);
}

function bufferValue(bytes: unknown[], path: string, kind: BufferKind): unknown {
  const values = new Uint8Array(bytes.length);
  for (let index = 0; index < bytes.length; index++) {
    const byte = bytes[index];
    if (typeof byte !== "number" || !Number.isInteger(byte) || byte < 0 || byte > 255) {
      malformed(`${path}[${index}]`, "a byte in 0..255", byte);
    }
    values[index] = byte;
  }
  switch (kind) {
    case "arrayBuffer":
      return values.buffer;
    case "nodeBuffer":
      return Buffer.from(values);
    default:
      return values;
  }
}

/** The calls made through one `captureArguments` stub, kept out of the target's view. */
const capturedCalls = new WeakMap<object, CapturedCall[]>();

/** The calls recorded on `stub`, or `undefined` when it was not built to capture. */
export function capturedCallsOf(stub: unknown): CapturedCall[] | undefined {
  if (typeof stub !== "function" && (typeof stub !== "object" || stub === null)) return undefined;
  return capturedCalls.get(stub as object);
}

function makeStub(behavior: FunctionBehavior, returns: unknown): Function {
  // A `function` expression, not an arrow: the target may use the value as a constructor.
  const stub = function (this: unknown, ...args: unknown[]): unknown {
    if (behavior === "throw") throw new StubCallError(args.length);
    if (behavior === "captureArguments") {
      const calls = capturedCalls.get(stub) ?? [];
      calls.push({ args, thisValue: this });
      capturedCalls.set(stub, calls);
    }
    return returns;
  };
  Object.defineProperty(stub, "name", { value: "afzStub", configurable: true });
  return stub;
}

/** A fresh iterator per request, so a target may consume the same stream twice. */
function streamValue(items: unknown[], iterable: boolean): object {
  if (iterable) {
    return {
      [Symbol.iterator]: function (): Iterator<unknown> {
        return items[Symbol.iterator]();
      },
    };
  }
  return {
    [Symbol.asyncIterator]: async function* (): AsyncGenerator<unknown> {
      for (const item of items) yield item;
    },
  };
}

function build(value: unknown, path: string, ctx: Context): unknown {
  enterNode(ctx, path);
  try {
    return buildNode(value, path, ctx);
  } finally {
    ctx.depth--;
  }
}

function buildNode(value: unknown, path: string, ctx: Context): unknown {
  const node = asNode(value, path);
  switch (node.node) {
    case "null":
      return null;
    case "undefined":
      return undefined;
    case "boolean": {
      if (typeof node.value !== "boolean") malformed(path, 'a boolean field "value"', node.value);
      return node.value;
    }
    case "number":
      return numberValue(node.value, path);
    case "bigInt": {
      if (typeof node.value !== "string") malformed(path, 'a string field "value"', node.value);
      try {
        return BigInt(node.value);
      } catch {
        return malformed(path, "an integer literal for BigInt", node.value);
      }
    }
    case "string": {
      if (typeof node.value !== "string") malformed(path, 'a string field "value"', node.value);
      return node.value;
    }
    case "literal":
      return fromJson(node.value, path, ctx);
    case "array":
    case "tuple": {
      const items = asArray(node.items, path, "items");
      return items.map((item, index) => build(item, `${path}[${index}]`, ctx));
    }
    case "set": {
      const items = asArray(node.items, path, "items");
      return new Set(items.map((item, index) => build(item, `${path}[${index}]`, ctx)));
    }
    case "map": {
      const entries = asArray(node.entries, path, "entries");
      const map = new Map<unknown, unknown>();
      for (let index = 0; index < entries.length; index++) {
        const entry = entries[index];
        if (!Array.isArray(entry) || entry.length !== 2) {
          malformed(`${path}[${index}]`, "a [key, value] pair", entry);
        }
        const key = build(entry[0], `${path}[${index}:key]`, ctx);
        map.set(key, build(entry[1], `${path}[${index}:value]`, ctx));
      }
      return map;
    }
    case "object": {
      const fields = asArray(node.fields, path, "fields");
      const target: Record<string, unknown> = {};
      for (const field of fields) {
        if (!Array.isArray(field) || field.length !== 2 || typeof field[0] !== "string") {
          malformed(path, "a [name, value] field pair", field);
        }
        const name = field[0];
        // defineProperty, not assignment: a generated field named `__proto__` or `constructor`
        // has to stay an own data property instead of reparenting the object.
        Object.defineProperty(target, name, {
          value: build(field[1], childPath(path, name), ctx),
          writable: true,
          enumerable: true,
          configurable: true,
        });
      }
      return target;
    }
    case "buffer":
      return bufferValue(asArray(node.bytes, path, "bytes"), path, ctx.bufferKind);
    case "function": {
      const behavior = asBehavior(node.behavior, path, ctx.functionBehavior);
      const returns =
        node.returns === undefined || node.returns === null ? undefined : build(node.returns, `${path}.returns`, ctx);
      return makeStub(behavior, returns);
    }
    case "stream": {
      if (typeof node.iterable !== "boolean") malformed(path, 'a boolean field "iterable"', node.iterable);
      const items = asArray(node.items, path, "items");
      return streamValue(items.map((item, index) => build(item, `${path}[${index}]`, ctx)), node.iterable);
    }
    case "opaque": {
      const blocker: Blocker = { type: opaqueName(node.ty), path };
      const module = opaqueModule(node.ty);
      if (module !== undefined) blocker.module = module;
      ctx.blockers.push(blocker);
      throw new Blocked(ctx);
    }
    default:
      return malformed(path, "a known node tag", node.node);
  }
}

/** A JSON value from a `literal` node, made safe against prototype-reassigning keys. */
function fromJson(value: unknown, path: string, ctx: Context): unknown {
  enterNode(ctx, path);
  try {
    if (value === null || typeof value !== "object") return value;
    if (Array.isArray(value)) return value.map((item, index) => fromJson(item, `${path}[${index}]`, ctx));
    const target: Record<string, unknown> = {};
    for (const [name, item] of Object.entries(value)) {
      Object.defineProperty(target, name, {
        value: fromJson(item, childPath(path, name), ctx),
        writable: true,
        enumerable: true,
        configurable: true,
      });
    }
    return target;
  } finally {
    ctx.depth--;
  }
}

function failure(blockedBy: Blocker[], details: string[]): MaterializeBlocked {
  if (details.length === 0) return { ok: false, reason: BLOCKED_REASON, blockedBy };
  return { ok: false, reason: BLOCKED_REASON, blockedBy, detail: details.join("; ") };
}

function mergeBlocker(blockers: Blocker[], blocker: Blocker): void {
  const key = `${blocker.type}\u0000${blocker.module ?? ""}`;
  if (!blockers.some((seen) => `${seen.type}\u0000${seen.module ?? ""}` === key)) blockers.push(blocker);
}

function collect(blocked: Blocked): MaterializeBlocked {
  const blockers: Blocker[] = [];
  for (const blocker of blocked.context.blockers) mergeBlocker(blockers, blocker);
  return failure(blockers, blocked.context.details);
}

// ---------------------------------------------------------------- entry points

/** One value tree into a live value, reporting an unbuildable graph instead of throwing. */
export function materializeValue(tree: ValueTree, options: MaterializeOptions = {}): MaterializeResult {
  const ctx = newContext(options);
  try {
    return { ok: true, value: build(tree, "value", ctx) };
  } catch (error) {
    if (error instanceof Blocked) return collect(error);
    throw error;
  }
}

/**
 * The argument list of a call. Every argument is attempted, so one report names every blocking
 * type in the call rather than only the first one found.
 */
export function materializeValues(
  values: readonly ValueTree[],
  options: MaterializeOptions = {},
): { ok: true; args: unknown[] } | MaterializeBlocked {
  const args: unknown[] = [];
  const blockers: Blocker[] = [];
  const details: string[] = [];
  for (const [index, value] of values.entries()) {
    const ctx = newContext(options);
    try {
      args[index] = build(value, `arguments[${index}]`, ctx);
    } catch (error) {
      if (!(error instanceof Blocked)) throw error;
      for (const blocker of error.context.blockers) mergeBlocker(blockers, blocker);
      details.push(...error.context.details);
    }
  }
  if (blockers.length > 0 || details.length > 0) return failure(blockers, details);
  return { ok: true, args };
}

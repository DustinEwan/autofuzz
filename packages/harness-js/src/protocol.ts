// The wire contract, mirrored from `crates/afz-protocol` and `docs/schema/*.json`.
//
// This file holds types and framing only, exactly like the Rust crate: no classification, no
// generation, no policy. `test/contract.ts` checks every literal here against the committed
// schema, so drift between the two sides fails `npm test` rather than a scan.

export const PROTOCOL_VERSION = "afz/1";

export const METHOD_HANDSHAKE = "handshake";
export const METHOD_DISCOVER = "discover";
export const METHOD_PREPARE = "prepare";
export const METHOD_INVOKE = "invoke";
export const METHOD_MINIMIZE = "minimize";
export const METHOD_COVERAGE_MAP = "coverageMap";

// The runtime vocabulary. `test/contract.ts` compares each list against the JSON Schema that
// `cargo run -p afz-xtask -- schema` generates from the Rust types, so the two sides cannot
// drift silently: a renamed tag fails `npm test` on the plugin and `cargo test` on the engine.
export const TYPE_NODE_KINDS = [
  "scalar",
  "stringFormat",
  "literal",
  "collection",
  "object",
  "tuple",
  "composite",
  "buffer",
  "promise",
  "function",
  "stream",
  "opaque",
  "ref",
] as const;

export const VALUE_NODE_TAGS = [
  "null",
  "undefined",
  "boolean",
  "number",
  "bigInt",
  "string",
  "literal",
  "array",
  "set",
  "map",
  "object",
  "tuple",
  "buffer",
  "function",
  "stream",
  "opaque",
] as const;

export const SCALAR_KINDS = ["null", "undefined", "boolean", "number", "bigInt", "string"] as const;
export const COLLECTION_KINDS = ["array", "set", "map"] as const;
export const COMPOSITE_KINDS = ["union", "intersection"] as const;
export const FUNCTION_BEHAVIORS = ["returnFirst", "throw", "captureArguments"] as const;
export const SKIP_REASONS = [
  "unconstructible-parameter",
  "discovery-error",
  "not-public",
  "excluded",
  "unsupported-signature",
  "target-disabled",
] as const;
export const OUTCOME_KINDS = ["ok", "rejected", "failed"] as const;
export const MATERIALIZATIONS = ["constructible", "fixture-required", "opaque"] as const;
export const CAPABILITY_NAMES = ["coverage", "stateful", "minimize", "sideEffects", "concurrent"] as const;
export const METHOD_NAMES = [
  METHOD_HANDSHAKE,
  METHOD_DISCOVER,
  METHOD_PREPARE,
  METHOD_INVOKE,
  METHOD_MINIMIZE,
  METHOD_COVERAGE_MAP,
] as const;

export const PARSE_ERROR = -32700;
export const INVALID_REQUEST = -32600;
export const METHOD_NOT_FOUND = -32601;
export const PLUGIN_ERROR = -32000;

// ---------------------------------------------------------------- type system

export type JsonValue = null | boolean | number | string | JsonValue[] | { [key: string]: JsonValue };

export type ScalarKind = "null" | "undefined" | "boolean" | "number" | "bigInt" | "string";
export type CollectionKind = "array" | "set" | "map";
export type CompositeKind = "union" | "intersection";
export type FunctionBehavior = "returnFirst" | "throw" | "captureArguments";
export type TargetClass = "design-time" | "runtime";
export type Materialization = "constructible" | "fixture-required" | "opaque";

export interface NumberBounds {
  int?: boolean;
  finiteOnly?: boolean;
  min?: number;
  max?: number;
}

export interface LengthBounds {
  min?: number;
  max?: number;
}

export interface Field {
  name: string;
  type: TypeNode;
  required: boolean;
}

/** One member of the closed shape a `ref` names; the generator synthesizes through it. */
export interface RefField {
  name: string;
  type: TypeNode;
}

/**
 * The generator's vocabulary, and the normalized type every harness lowers its language
 * into. The thirteen rows map one-to-one onto the type table in `docs/architecture.md`.
 */
export type TypeNode =
  | { kind: "scalar"; scalar: ScalarKind; bounds?: NumberBounds; length?: LengthBounds }
  | { kind: "stringFormat"; format: string }
  | { kind: "literal"; values: JsonValue[] }
  | {
      kind: "collection";
      collection: CollectionKind;
      type?: TypeNode;
      key?: TypeNode;
      length?: LengthBounds;
    }
  | { kind: "object"; id?: string; name?: string; fields?: Field[] }
  | { kind: "tuple"; items: TypeNode[] }
  | { kind: "composite"; composite: CompositeKind; discriminant?: string; members: TypeNode[] }
  | { kind: "buffer"; length?: LengthBounds }
  | { kind: "promise"; type: TypeNode }
  | { kind: "function"; behavior?: FunctionBehavior; type?: TypeNode }
  | { kind: "stream"; iterable: boolean; type: TypeNode; maxItems?: number }
  | { kind: "opaque"; ty: string; module?: string }
  | { kind: "ref"; name: string; fields?: RefField[] };

// ---------------------------------------------------------------- value trees

/**
 * `NaN` and the infinities have no JSON token, so they travel as names. The spellings are the
 * Rust enum's variant names under serde's camelCase rule, which turns `NaN` into `naN`. They
 * look like typos and are not: `docs/schema/value-tree.json` and the engine's golden files carry
 * these exact strings, and `test/contract.test.mjs` fails the package if this list drifts.
 */
export type SpecialNumber = "naN" | "infinity" | "negativeInfinity";

export const SPECIAL_NUMBERS = ["naN", "infinity", "negativeInfinity"] as const;
export type NumberValue = number | SpecialNumber;
export type KeySide = "key" | "value";

/** A step from the root toward a node: an array index, a property name, or a map side. */
export type Step = number | string | KeySide;
export type NodePath = Step[];

export type Provenance =
  | "generated"
  | "boundary"
  | "user"
  | { mutated: { op: string; from: string } }
  | { transplanted: { fromEntry: string } }
  | { minimized: { step: number } };

export type ValueNode =
  | { node: "null" }
  | { node: "undefined" }
  | { node: "boolean"; value: boolean }
  | { node: "number"; value: NumberValue }
  | { node: "bigInt"; value: string }
  | { node: "string"; value: string }
  | { node: "literal"; value: JsonValue }
  | { node: "array"; items: ValueTree[] }
  | { node: "set"; items: ValueTree[] }
  | { node: "map"; entries: [ValueTree, ValueTree][] }
  | { node: "object"; fields: [string, ValueTree][] }
  | { node: "tuple"; items: ValueTree[] }
  | { node: "buffer"; bytes: number[] }
  | { node: "function"; behavior: FunctionBehavior; returns?: ValueTree }
  | { node: "stream"; iterable: boolean; items: ValueTree[] }
  | { node: "opaque"; ty: string };

/**
 * A `ValueNode` plus why it exists. The wire flattens the node fields next to `provenance`, so
 * the two are one object rather than a wrapper.
 */
export type ValueTree = ValueNode & { provenance?: Provenance };

export interface CallInput {
  receiver?: ValueTree | null;
  arguments: ValueTree[];
}

// ---------------------------------------------------------------- inventory

export interface Origin {
  file?: string;
  line?: number;
  module?: string;
  http?: { method: string; path: string };
}

export interface Effects {
  async?: boolean;
  throws?: boolean;
  streams?: boolean;
  callbacks?: boolean;
  mutatesState?: boolean;
}

export interface Parameter {
  name: string;
  type: TypeNode;
  required: boolean;
}

export interface TargetDescriptor {
  id: string;
  targetClass: TargetClass;
  origin: Origin;
  name: string;
  parameters: Parameter[];
  returns?: TypeNode;
  effects: Effects;
  guardSites?: number[];
  materialization: Materialization;
}

export type SkipReason =
  | "unconstructible-parameter"
  | "discovery-error"
  | "not-public"
  | "excluded"
  | "unsupported-signature"
  | "target-disabled";

export interface Blocker {
  type: string;
  path: string;
  module?: string | null;
}

export interface SkippedTarget {
  targetId: string;
  name: string;
  reason: SkipReason;
  detail?: string;
  blockedBy?: Blocker[];
  unlocksTargets?: string[];
}

export interface Discovery {
  targets: TargetDescriptor[];
  skipped: SkippedTarget[];
}

// ---------------------------------------------------------------- outcomes

export interface FaultFrame {
  file?: string;
  line?: number;
  column?: number;
  fnName?: string;
}

/** The fault a `failed` outcome carries. `class` is the error constructor name. */
export interface FaultSignature {
  class: string;
  message: string;
  frames?: FaultFrame[];
}

export interface OkOutcome {
  kind: "ok";
  durationMs?: number;
  /** Plugin-reported, never engine-measured: the `heapUsed` delta around the call. */
  memoryBytes?: number | null;
  coverage?: number[] | null;
  /**
   * The return value, re-encoded as a `ValueTree`. `provenance` is left off on purpose: the
   * three provenance values say why the generator drew a node, and a value the target returned
   * was not drawn by anyone.
   */
  value?: ValueTree |null;
}

export interface RejectedOutcome {
  kind: "rejected";
  durationMs: number;
  coverage?: number[] | null;
  /** The rule that decided the classification, so a triager can audit and override it. */
  via: string;
  detail: string;
}

export interface FailedOutcome {
  kind: "failed";
  durationMs: number;
  coverage?: number[] | null;
  via: string;
  fault: FaultSignature;
}

export type Outcome = OkOutcome | RejectedOutcome | FailedOutcome;

// ---------------------------------------------------------------- coverage

export interface CoverageEntry {
  /** `fnv1a64(url) ^ (startBlock << 1) ^ (endBlock << 1)`, folded to u32. */
  id: number;
  url: string;
  function: string;
  startBlock: number;
  endBlock: number;
}

export interface CoverageMap {
  entries: CoverageEntry[];
}

// ---------------------------------------------------------------- capabilities

export type Capability = "coverage" | "stateful" | "minimize" | "sideEffects" | "concurrent";

export interface Handshake {
  protocolVersion: string;
  name: string;
  version: string;
  targetClass: TargetClass;
  capabilities: Capability[];
  languages: string[];
  discoverRequiresExecution?: boolean;
}

export interface DiscoverReq {
  root: string;
  include?: string[] | null;
  exclude?: string[] | null;
  configPath?: string | null;
}

export interface PrepareReq {
  targetId: string;
  values: ValueTree[];
}

/** What the plugin built for one call. `invoke` may name the handle to skip a rebuild. */
export interface MaterializedCall {
  handle: string;
  summary: string;
}

export type PrepareResp =
  | { ok: true; materialized: MaterializedCall }
  | { ok: false; reason: SkipReason };

export interface InvokeReq {
  invocationId: number;
  targetId: string;
  values: ValueTree[];
  deadlineMs: number;
  collectCoverage: boolean;
}

export interface MinimizeReq {
  targetId: string;
  values: ValueTree[];
  fault: FaultSignature;
  budgetMs: number;
}

export interface MinimizeResp {
  values: ValueTree[];
  steps: number;
}

// ---------------------------------------------------------------- framing

export interface Request {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params?: unknown;
}

export interface Success {
  jsonrpc: "2.0";
  id: number;
  result: unknown;
}

export interface ErrorObject {
  code: number;
  message: string;
  data?: JsonValue;
}

export interface Failure {
  jsonrpc: "2.0";
  id: number | null;
  error: ErrorObject;
}

export type Response = Success | Failure;

/** One JSON value per line, the same framing the Rust side reads. */
export function writeLine(target: NodeJS.WritableStream, message: Response): void {
  target.write(`${JSON.stringify(message)}\n`);
}

export function success(id: number, result: unknown): Success {
  return { jsonrpc: "2.0", id, result };
}

export function failure(id: number | null, code: number, message: string): Failure {
  return { jsonrpc: "2.0", id, error: { code, message } };
}

/** Split one buffered chunk into complete lines, keeping the partial tail for the next chunk. */
export class LineFramer {
  private pending = "";

  push(chunk: string): string[] {
    this.pending += chunk;
    const lines: string[] = [];
    let cut = this.pending.indexOf("\n");
    while (cut >= 0) {
      const line = this.pending.slice(0, cut);
      this.pending = this.pending.slice(cut + 1);
      if (line.trim() !== "") lines.push(line);
      cut = this.pending.indexOf("\n");
    }
    return lines;
  }
}

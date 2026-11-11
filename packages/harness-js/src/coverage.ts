// Block coverage from `node:inspector`, turned into the edge ids the engine stores.
//
// `NODE_V8_COVERAGE` is not used: it is inconsistent with worker threads (nodejs/node#46378),
// and every invocation runs in a worker. An `inspector.Session()` created inside the worker
// attaches to that worker's own V8 isolate, which is the thread that actually runs the target.
//
// One session serves the whole life of a warm worker; `CoverageProbe` is the reset around a call.

import { Session } from "node:inspector";
import { pathToFileURL } from "node:url";

import { sourceOffsetAt, type SourceMap } from "./sourcemap.js";
import type { CoverageEntry } from "./protocol.js";

const FNV_OFFSET_BASIS = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const MASK_64 = 0xffffffffffffffffn;
const MASK_32 = 0xffffffffn;

/** 64-bit FNV-1a over the UTF-8 bytes of `text`. */
export function fnv1a64(text: string): bigint {
  let hash = FNV_OFFSET_BASIS;
  for (const byte of new TextEncoder().encode(text)) {
    hash = ((hash ^ BigInt(byte)) * FNV_PRIME) & MASK_64;
  }
  return hash;
}

/**
 * The edge id for one covered block. `fnv1a64(url) ^ (startBlock << 1) ^ (endBlock << 1)`,
 * folded to u32 by mixing the high half down. The shift by one keeps the low bit of each
 * offset distinguishable after the xor, and the fold keeps the id inside the u32 the engine
 * stores. Stable across processes: it depends only on the source text and the block bounds.
 */
export function edgeId(url: string, startBlock: number, endBlock: number): number {
  const mixed = fnv1a64(url) ^ (BigInt(startBlock) << 1n) ^ (BigInt(endBlock) << 1n);
  return Number((mixed ^ (mixed >> 32n)) & MASK_32);
}

interface V8Range {
  startOffset: number;
  endOffset: number;
  count: number;
}

interface V8FunctionCoverage {
  functionName: string;
  ranges: V8Range[];
}

interface V8ScriptCoverage {
  /** Unique per compiled script, which is what identifies the invocation that compiled it. */
  scriptId: string;
  url: string;
  functions: V8FunctionCoverage[];
}

function post<T>(session: Session, method: string, params: object): Promise<T> {
  const { promise, resolve, reject } = Promise.withResolvers<T>();
  session.post(method, params, (error: Error | null, result?: object) => {
    if (error) reject(error);
    // The inspector callback types every payload as `object`; each call site names the real
    // result type of the method it posts, so the assertion is the boundary, not a shortcut.
    else resolve(result as T);
  });
  return promise;
}

/** True when `url` points at a file inside the scan project root. */
export function underRoot(url: string, root: string): boolean {
  if (!url.startsWith("file:")) return false;
  const rootUrl = pathToFileURL(root).href;
  const base = rootUrl.endsWith("/") ? rootUrl : `${rootUrl}/`;
  return url.startsWith(base);
}

/**
 * One `node:inspector` session for the life of a worker, scoped to one invocation at a time.
 *
 * Two measured facts about V8 decide the shape here.
 *
 * A `stopPreciseCoverage` plus `startPreciseCoverage` cycle on a live session drops every script
 * compiled before it from the report, and costs milliseconds that grow with the isolate. That
 * means the window has to be open *before* `src/realm.ts` compiles the invocation's modules, so
 * the cycle cannot be paid at the start of a call, and paying it per call is the most expensive
 * thing an invocation does.
 *
 * `detailed: true` block counts describe each function's *most recent* execution, not a running
 * total, and `takePreciseCoverage` reports every script the isolate still holds rather than only
 * the ones that ran since the last take. So a session that stays armed hands an invocation the
 * blocks of the invocation before it — the exact failure mode of a pooled module graph.
 *
 * Coverage stays armed across calls anyway, because the two facts have a consistent reading: a
 * script's counts describe the call that ran it, and every invocation compiles its own module
 * graph. So the scoping rule is that a script belongs to the invocation that first reports it, and
 * `reported` is the filter that enforces it. It holds for the life of the worker, which is why
 * `src/server.ts` retires a worker on a schedule instead of letting the table and V8's report grow
 * without bound.
 */
export class CoverageProbe {
  private readonly session = new Session();
  private running = false;
  /**
   * The arming of the window the next call compiles into. Nothing may compile before it settles:
   * a script compiled while coverage is off is dropped from the report, so the ordering is the
   * difference between a small digest and an empty one.
   */
  private window: Promise<void>;
  /** Scripts already attributed to an invocation. V8 keeps reporting a live script every take. */
  private readonly reported = new Set<string>();

  constructor() {
    this.session.connect();
    this.window = post(this.session, "Profiler.enable", {})
      .then(() => this.arm())
      .catch(() => undefined);
  }

  /** Start block coverage. Armed once per worker; a worker is replaced rather than re-armed. */
  private async arm(): Promise<void> {
    if (this.running) return;
    await post<void>(this.session, "Profiler.startPreciseCoverage", { callCount: false, detailed: true });
    this.running = true;
  }

  /**
   * Keep the window open for the next call. With no reset between calls this settles immediately;
   * it is still awaited by the next call, because the first call of a fresh worker must not
   * compile its modules before the profiler is running.
   */
  rear(): Promise<void> {
    this.window = this.arm();
    return this.window;
  }

  /** Settles once the window this call's modules will be compiled into is open. */
  armed(): Promise<void> {
    return this.window;
  }

  /** The blocks this invocation's own scripts ran; earlier invocations' scripts are not its report. */
  async take(
    root: string,
    maps: ReadonlyMap<string, SourceMap> = new Map(),
  ): Promise<{ entries: CoverageEntry[]; all: CoverageEntry[] }> {
    await this.window.catch(() => undefined);
    const { result } = await post<{ result: V8ScriptCoverage[] }>(this.session, "Profiler.takePreciseCoverage", {});
    const fresh: V8ScriptCoverage[] = [];
    for (const script of result) {
      if (!underRoot(script.url, root)) continue;
      if (this.reported.has(script.scriptId)) continue;
      this.reported.add(script.scriptId);
      fresh.push(script);
    }
    return { entries: collectEntries(fresh, root, maps), all: allEntries(fresh, root, maps) };
  }

  close(): void {
    this.session.post("Profiler.disable", {}, () => undefined);
    this.session.disconnect();
  }
}

/**
 * The blocks that ran: the edges one invocation's digest reports. Each is a positive range with
 * its skipped sub-ranges cut out, so a guard branch and the path it guards report different sets.
 */
export function collectEntries(
  report: readonly V8ScriptCoverage[],
  root: string,
  maps: ReadonlyMap<string, SourceMap> = new Map(),
): CoverageEntry[] {
  return entriesFor(report, root, executedSegments, maps, false);
}

/**
 * Every block of every function this call entered, run or not. This is what `coverageMap` answers
 * with: its size is the reachable-edge count the scan result divides covered edges by, and each
 * entry names the source range behind an edge id in a digest.
 *
 * A function the call never entered is left out. A realm compiles its whole module graph inside
 * the coverage window, so V8 has a record for every function in the closure, entered or not;
 * counting the unentered ones would add one whole-body range per function to the denominator of
 * `coverageRatio` and make it depend on how eagerly this runtime compiled the file.
 */
export function allEntries(
  report: readonly V8ScriptCoverage[],
  root: string,
  maps: ReadonlyMap<string, SourceMap> = new Map(),
): CoverageEntry[] {
  return entriesFor(report, root, allSegments, maps, true);
}

function entriesFor(
  report: readonly V8ScriptCoverage[],
  root: string,
  segmentsOf: (ranges: readonly V8Range[]) => Segment[],
  maps: ReadonlyMap<string, SourceMap>,
  enteredOnly: boolean,
): CoverageEntry[] {
  const entries: CoverageEntry[] = [];
  const seen = new Set<number>();
  for (const script of report) {
    if (!underRoot(script.url, root)) continue;
    const map = maps.get(script.url);
    for (const fn of script.functions) {
      // V8 reports the module's own top level as a function with no name whose only range is the
      // whole script. It is not an edge: it is the same range for every call that evaluates the
      // module, and it contains every real edge in the file, so reporting it would count the same
      // code twice and add nothing a digest could use as novelty.
      if (fn.functionName === "") continue;
      if (enteredOnly && fn.ranges.every((range) => range.count <= 0)) continue;
      for (const segment of segmentsOf(fn.ranges)) {
        // A realm script is the transpiled text, so its offsets are emitted ones. The edge id
        // and the reported range are both about the source, so both ends are mapped first, and a
        // range with no source position — compiler-synthesized text — reports nothing.
        const start = map === undefined ? segment.startOffset : sourceOffsetAt(map, segment.startOffset);
        const end = map === undefined ? segment.endOffset : sourceOffsetAt(map, segment.endOffset);
        if (start === null || end === null || end <= start) continue;
        const id = edgeId(script.url, start, end);
        if (seen.has(id)) continue;
        seen.add(id);
        entries.push({ edge: id, url: script.url, start, end });
      }
    }
  }
  entries.sort((a, b) => a.edge - b.edge);
  return entries;
}

interface Segment {
  startOffset: number;
  endOffset: number;
}

/**
 * Each range that ran, cut wherever a range inside it was skipped.
 *
 * V8 lists a function's whole range with its execution count and lists the blocks the run
 * *skipped* as nested ranges with count 0; a branch that was taken gets no range of its own.
 * That is why the segment list has to subtract, and why block-level coverage is asked for rather
 * than function-level.
 */
export function executedSegments(ranges: readonly V8Range[]): Segment[] {
  const skipped = ranges
    .filter((range) => range.count <= 0)
    .map((range) => [range.startOffset, range.endOffset] as const)
    .sort((a, b) => a[0] - b[0]);
  const segments: Segment[] = [];
  const emitted = new Set<string>();
  for (const range of ranges) {
    if (range.count <= 0) continue;
    let cursor = range.startOffset;
    for (const [start, end] of skipped) {
      const inside = range.startOffset <= start && end <= range.endOffset;
      const proper = start > range.startOffset || end < range.endOffset;
      if (!inside || !proper) continue;
      if (start > cursor) add(segments, emitted, range.startOffset, start);
      cursor = Math.max(cursor, end);
    }
    if (cursor < range.endOffset) add(segments, emitted, cursor, range.endOffset);
  }
  return segments;
}

/** Every block of a function: the ones that ran and the ones that did not. */
export function allSegments(ranges: readonly V8Range[]): Segment[] {
  const blocks = [...executedSegments(ranges), ...unexecutedSegments(ranges)];
  blocks.sort((a, b) => a.startOffset - b.startOffset || a.endOffset - b.endOffset);
  return blocks;
}

/** Each range that never ran, cut wherever a range inside it did run. */
export function unexecutedSegments(ranges: readonly V8Range[]): Segment[] {
  const ran = ranges
    .filter((range) => range.count > 0)
    .map((range) => [range.startOffset, range.endOffset] as const)
    .sort((a, b) => a[0] - b[0]);
  const segments: Segment[] = [];
  const emitted = new Set<string>();
  for (const range of ranges) {
    if (range.count > 0) continue;
    let cursor = range.startOffset;
    for (const [start, end] of ran) {
      const inside = range.startOffset <= start && end <= range.endOffset;
      const proper = start > range.startOffset || end < range.endOffset;
      if (!inside || !proper) continue;
      if (start > cursor) add(segments, emitted, range.startOffset, start);
      cursor = Math.max(cursor, end);
    }
    if (cursor < range.endOffset) add(segments, emitted, cursor, range.endOffset);
  }
  return segments;
}

function add(segments: Segment[], emitted: Set<string>, startOffset: number, endOffset: number): void {
  const key = `${startOffset}:${endOffset}`;
  if (emitted.has(key) || endOffset <= startOffset) return;
  emitted.add(key);
  segments.push({ startOffset, endOffset });
}

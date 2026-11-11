// Block coverage from `node:inspector`, turned into the edge ids the engine stores.
//
// `NODE_V8_COVERAGE` is not used: it is inconsistent with worker threads (nodejs/node#46378),
// and every invocation runs in a worker. An `inspector.Session()` created inside the worker
// attaches to that worker's own V8 isolate, which is the thread that actually runs the target.

import { Session } from "node:inspector";
import { pathToFileURL } from "node:url";

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
 * Runs the callback with precise block coverage enabled, and returns the edges whose blocks
 * executed at least once. `callCount: false` because only the set of hit edges is reported;
 * `detailed: true` because block granularity is what distinguishes guard branches.
 */
export async function withCoverage<T>(
  root: string,
  run: () => Promise<T>,
): Promise<{ value: T; entries: CoverageEntry[] }> {
  const session = new Session();
  session.connect();
  try {
    await post(session, "Profiler.enable", {});
    await post(session, "Profiler.startPreciseCoverage", { callCount: false, detailed: true });
    const value = await run();
    const { result } = await post<{ result: V8ScriptCoverage[] }>(
      session,
      "Profiler.takePreciseCoverage",
      {},
    );
    return { value, entries: collectEntries(result, root) };
  } finally {
    session.post("Profiler.disable", {}, () => undefined);
    session.disconnect();
  }
}

/**
 * Filters V8's report down to in-root code that ran, and names each piece.
 *
 * V8 lists a function's whole range with its execution count and lists the blocks the run
 * *skipped* as nested ranges with count 0; a branch that was taken gets no range of its own.
 * The blocks that actually ran are therefore each positive range with its skipped sub-ranges
 * cut out, and every such segment is one edge. Doing the subtraction here is what makes the
 * guard branch and the accepted path report different edge sets, which is the whole point of
 * asking for block-level coverage instead of function-level.
 */
export function collectEntries(report: V8ScriptCoverage[], root: string): CoverageEntry[] {
  const entries: CoverageEntry[] = [];
  const seen = new Set<number>();
  for (const script of report) {
    if (!underRoot(script.url, root)) continue;
    for (const fn of script.functions) {
      for (const segment of executedSegments(fn.ranges)) {
        const id = edgeId(script.url, segment.startOffset, segment.endOffset);
        if (seen.has(id)) continue;
        seen.add(id);
        entries.push({
          id,
          url: script.url,
          function: fn.functionName,
          startBlock: segment.startOffset,
          endBlock: segment.endOffset,
        });
      }
    }
  }
  entries.sort((a, b) => a.id - b.id);
  return entries;
}

interface Segment {
  startOffset: number;
  endOffset: number;
}

/** Each range that ran, cut wherever a range inside it was skipped. */
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

function add(segments: Segment[], emitted: Set<string>, startOffset: number, endOffset: number): void {
  const key = `${startOffset}:${endOffset}`;
  if (emitted.has(key) || endOffset <= startOffset) return;
  emitted.add(key);
  segments.push({ startOffset, endOffset });
}

// Source-map decoding and coordinate translation.
//
// This module deliberately imports nothing: the invoke worker needs it to turn the coordinates V8
// reports into source coordinates, and it must not pull in the TypeScript compiler to do so. The
// compiler belongs to the plugin process, which loads it once for discovery; anything the worker
// loads is loaded again per thread and deoptimized by the coverage profiler per call.

/** One generated-to-source mapping: a column of the emitted text and where it came from. */
export interface SourceMapping {
  genCol: number;
  line: number;
  col: number;
}

/** A position in a module's `.ts` source. */
export interface SourcePosition {
  line: number;
  column: number;
}

/** The decoded `//# sourceMappingURL` of one module, kept for coordinate translation. */
export interface SourceMap {
  /** Per emitted line, its mappings in emitted-column order; empty when the line is synthetic. */
  lines: SourceMapping[][];
  /** Offset at which each emitted line starts. */
  genLineStarts: number[];
  /** Offset at which each source line starts. */
  srcLineStarts: number[];
  /** Length of the source text, so a mapped position can be clamped inside it. */
  srcLength: number;
}

/** The offset at which each line of `text` begins. */
export function lineStarts(text: string): number[] {
  const starts = [0];
  for (let i = 0; i < text.length; i++) {
    if (text[i] === "\n") starts.push(i + 1);
  }
  return starts;
}

const BASE64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/** One base64 VLQ field list, as a v3 source map segment encodes it. */
function decodeVlq(segment: string): number[] {
  const fields: number[] = [];
  let value = 0;
  let shift = 0;
  for (const char of segment) {
    const digit = BASE64.indexOf(char);
    if (digit < 0) break;
    value += (digit & 31) << shift;
    if (digit & 32) {
      shift += 5;
      continue;
    }
    const negative = value & 1;
    value >>= 1;
    fields.push(negative ? -value : value);
    value = 0;
    shift = 0;
  }
  return fields;
}

/**
 * Decode a v3 `mappings` string into per-emitted-line mappings. Only the emitted column, the
 * source line and the source column are kept: there is one source file per module, so the source
 * index carries nothing, and symbols are not reported anywhere.
 */
export function decodeMappings(mappings: string): SourceMapping[][] {
  const lines: SourceMapping[][] = [];
  const previous = [0, 0, 0, 0, 0];
  for (const lineMappings of mappings.split(";")) {
    previous[0] = 0;
    const line: SourceMapping[] = [];
    for (const segment of lineMappings.split(",")) {
      if (segment === "") continue;
      const fields = decodeVlq(segment);
      for (let i = 0; i < fields.length && i < previous.length; i++) {
        previous[i] = (previous[i] ?? 0) + (fields[i] ?? 0);
      }
      // A segment with fewer than four fields is emitted text with no source position: a
      // compiler-synthesized prologue or helper. It maps nowhere and is left out.
      if (fields.length >= 4) {
        line.push({ genCol: previous[0] ?? 0, line: previous[2] ?? 0, col: previous[3] ?? 0 });
      }
    }
    lines.push(line);
  }
  return lines;
}

function lineOf(starts: readonly number[], offset: number): number {
  let low = 0;
  let high = starts.length - 1;
  while (low < high) {
    const middle = Math.ceil((low + high) / 2);
    if ((starts[middle] ?? 0) <= offset) low = middle;
    else high = middle - 1;
  }
  return low;
}

/**
 * The source position an emitted position came from, both zero-based, or `null` when the emitted
 * position is compiler-synthesized and has no source. Within one mapping segment the text is
 * copied verbatim, so the column is shifted by the segment's own offset.
 */
export function sourcePositionAt(map: SourceMap, genLine: number, genColumn: number): SourcePosition | null {
  const mappings = map.lines[genLine];
  if (mappings === undefined || mappings.length === 0) return null;
  let chosen: SourceMapping | null = null;
  for (const mapping of mappings) {
    if (mapping.genCol > genColumn) break;
    chosen = mapping;
  }
  if (chosen === null) chosen = mappings[0] ?? null;
  if (chosen === null) return null;
  if (chosen.line >= map.srcLineStarts.length) return null;
  return { line: chosen.line, column: Math.max(0, chosen.col + (genColumn - chosen.genCol)) };
}

/**
 * The offset in the `.ts` source that an offset into the emitted text came from, or `null` for
 * emitted text that has no source position.
 */
export function sourceOffsetAt(map: SourceMap, generatedOffset: number): number | null {
  const line = lineOf(map.genLineStarts, generatedOffset);
  const position = sourcePositionAt(map, line, generatedOffset - (map.genLineStarts[line] ?? 0));
  if (position === null) return null;
  const lineStart = map.srcLineStarts[position.line];
  if (lineStart === undefined) return null;
  return Math.min(lineStart + position.column, map.srcLength);
}

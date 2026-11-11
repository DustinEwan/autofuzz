// Oracle test target 2 of 4 — the `failed` oracle.
//
// Nothing here declares an error channel, so `row.values` being undefined is not the input
// being refused, it is the function breaking: `undefined.map` throws a `TypeError` at a line the
// target wrote but never guarded. The plugin classifies it through
// `engine-error-outside-declared-channel`, and the `failed` oracle reports it at `high`.

export function normalize(row: Record<string, string[]>): string[] {
  return row.values.map((value) => value.trim());
}

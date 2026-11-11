// Oracle test target 4 of 4 — the `memoryGrowth` oracle.
//
// `held` keeps every block alive, so the plugin's `heapUsed` delta around the call is the
// allocation itself rather than something the garbage collector already reclaimed. The engine
// never measures memory; it reads the number the plugin reports and compares it with
// `oracles.memoryGrowth.mb`, so an input of 300 passes the default 256 MB oracle and the oracle
// reports at `medium`.
//
// The block is a filled array on purpose. V8 builds a long string lazily — `"x".repeat(1 << 20)`
// leaves a rope of a few hundred bytes on the heap until something reads the characters — so a
// string here would report growth in the kilobytes. A filled array is materialised as it is
// built, and one million elements is eight megabytes the collector cannot take back while `held`
// refers to it.

const held: unknown[][] = [];

/** Grow the heap by about `megabytes`, in eight-megabyte blocks. */
export function expand(megabytes: number): number {
  const blocks = Math.max(1, Math.ceil(megabytes / 8));
  for (let i = 0; i < blocks; i++) {
    held.push(new Array(1024 * 1024).fill(0));
  }
  return held.length;
}

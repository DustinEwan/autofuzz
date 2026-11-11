// Oracle test target 3 of 4 — the `deadline` oracle.
//
// `13` in the input makes the loop spin without consuming anything, so the call never returns.
// The engine enforces `deadlineMs` itself: it stops waiting, kills that plugin process, restarts
// it, and records the invocation as `via: "deadline"`. The `deadline` oracle reports it at
// `medium`, and the run carries on with the remaining invocations.

export function drain(items: number[]): number {
  let total = 0;
  while (items.length > 0) {
    if (items[0] === 13) {
      continue;
    }
    total += items.shift() ?? 0;
  }
  return total;
}

// The target the realm test invokes. Both counters are module-level: `calls` in this module and
// `entries` in the one it imports, reached through the `.js` specifier that names a `.ts` file, so
// one call exercises the whole import closure rather than a single module.
import { record } from "./ledger.js";

let calls = 0;

export function tally(): number {
  calls += 1;
  record();
  return calls;
}

// Module state that a second invocation must not inherit. `entries` is private to the module and
// only reachable through `record`, so the number it returns is the only way to see it.
let entries = 0;

export function record(): number {
  entries += 1;
  return entries;
}

export function recorded(): number {
  return entries;
}

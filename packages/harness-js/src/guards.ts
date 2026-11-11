// The package's one place that narrows `unknown`. Values cross the plugin boundary as JSON and
// as live objects the target returned, so narrowing happens at those boundaries and nowhere
// else; a second copy of a guard at a call site is how a wrong shape survives review.

/** An object that can be indexed by name: excludes `null`, arrays, and primitives. */
export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export type Callable = (...args: unknown[]) => unknown;

export function isCallable(value: unknown): value is Callable {
  return typeof value === "function";
}

/** A class the harness can instantiate with no arguments. */
export function isConstructable(value: unknown): value is new () => unknown {
  return typeof value === "function" && "prototype" in value;
}

// `enum` is the construct Node's strip-only type removal refuses to run, so this module is the
// loader fallback's test case: the plugin must notice `ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX` and
// re-run the call in a thread whose loader is `tsx`.

export enum Kind {
  A = "a",
  B = "b",
}

export function label(kind: Kind): string {
  if (kind === Kind.A) {
    return "first";
  }
  return "second";
}

// Oracle test target 1 of 4 — the channel that is *not* a finding.
//
// `rate <= 0` is the input's fault, and the function says so through the channel its own body
// declares: a `RangeError` thrown on line 9, which discovery records as a guard site. The
// `failed` oracle must stay silent and the `rejected` oracle is off by default, so a scan of
// this input yields 0 findings.

export function priced(item: string, rate: number): number {
  if (rate <= 0) throw new RangeError(`rate must be positive, got ${rate}`);
  return item.length * rate;
}

// Input validation shared by the report and ledger exporters. These predicates are the
// only admission tests: an input that passes them is expected to be processable by every
// downstream renderer.

import type { ReportRequest } from "./reports.js";

// Brazilian credit runs are reconciled per settlement batch: a BRL credit-kind request is a
// credit run only when it carries the three settlement tags the ops console stamps on every
// batch it releases. Anything else renders through the generic fallback path.
export function isCreditWindow(req: ReportRequest): boolean {
  return req.currency === "BRL" && req.kind === "credit" && req.batchTags.length === 3;
}

export function validEmail(address: string): boolean {
  const at = address.indexOf("@");
  if (at <= 0 || at >= address.length - 1) return false;
  if (address.indexOf(" ") !== -1) return false;
  return address.indexOf(".", at + 1) > at + 1;
}

export function validPeriod(days: number): boolean {
  return Number.isInteger(days) && days > 0 && days <= 366;
}

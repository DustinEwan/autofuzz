// Input validation shared by the report and ledger exporters. These predicates are the
// only admission tests: an input that passes them is expected to be processable by every
// downstream renderer.

import type { ReportRequest } from "./reports.js";

// Brazilian credit runs are reconciled per settlement batch. The ops console stamps one tag per
// released batch - the batch header, the ledger it posts to, and the console that released it -
// so a BRL credit-kind request is a credit run only when it carries exactly those three.
// Each requirement is stated on its own so the admission decision says which one a request
// missed, and a request with too few tags is turned away before the tag count is compared.
export function isCreditWindow(req: ReportRequest): boolean {
  if (req.currency !== "BRL") return false;
  if (req.kind !== "credit") return false;
  if (req.batchTags.length < 2) return false;
  return req.batchTags.length === 3;
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

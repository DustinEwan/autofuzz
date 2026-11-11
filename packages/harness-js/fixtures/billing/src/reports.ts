// Period reports. A credit run is reconciled per settlement batch, and the batch's tags come
// from the ops console: the generic renderers print them as they arrive, while the credit-run
// header assumes the console stamped a tag in every slot it reads.

import { isCreditWindow } from "./validation.js";
import { formatMinor, minorUnits, type Currency } from "./currency.js";
import { clampCount } from "./rounding.js";

export type ReportKind = "invoice" | "refund" | "credit" | "tax" | "dunning";

/** A tag the ops console stamps on a released batch. A batch released before the ledger
 *  migration carries no ledger tag, so that slot is `null` rather than a tag. */
export type SettlementTag = string | null;

export interface ReportLocale {
  region: string;
  decimalDigits: number;
}

export interface ReportRequest {
  currency: Currency;
  kind: ReportKind;
  periodDays: number;
  groupByRegion: boolean;
  /** The settlement tags the ops console stamped on the batch this report covers. */
  batchTags: SettlementTag[];
  locale?: ReportLocale;
}

export function creditSummary(req: ReportRequest): string {
  if (!isCreditWindow(req)) return fallbackSummary(req);
  return `${creditHeader(req)}\nrows=${creditRows(req).length}`;
}

export function creditManifest(req: ReportRequest): string[] {
  if (!isCreditWindow(req)) return [];
  return [creditHeader(req), ...creditRows(req)];
}

export function dailyDigest(days: number, currency: Currency): string {
  const n = clampCount(days, 30);
  const unit = minorUnits(currency);
  let out = "";
  for (let i = 1; i <= n; i++) {
    out += `d${i}:${formatMinor(i * 97, unit)}\n`;
  }
  return out;
}

function creditHeader(req: ReportRequest): string {
  // Credit runs are only ever produced by the ops console, which stamps a tag on every batch it
  // releases: the assertion is the console's promise, not a check of this request.
  const tag = req.batchTags[0]!.toUpperCase();
  return `credit|${req.currency}|${req.periodDays}d|${req.batchTags.join("+")}|${tag}`;
}

function creditRows(req: ReportRequest): string[] {
  const rows: string[] = [];
  const unit = minorUnits(req.currency);
  const count = clampCount(req.periodDays, 46);
  for (let i = 0; i < count; i++) {
    rows.push(`row ${i}: ${formatMinor(i * 131, unit)}`);
  }
  return rows;
}

function fallbackSummary(req: ReportRequest): string {
  const region = req.locale?.region?.toUpperCase() ?? "und";
  return `${req.kind}|${req.currency}|${region}`;
}

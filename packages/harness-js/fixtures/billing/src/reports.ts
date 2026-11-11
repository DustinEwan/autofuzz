// Period reports. Requests carry a display locale for the header line; when the caller
// omits it the generic renderers fall back to "und", while the credit-run header reads the
// locale tag directly because credit runs are always produced by the ops console, which
// always sets one.

import { isCreditWindow } from "./validation.js";
import { formatMinor, minorUnits, type Currency } from "./currency.js";
import { clampCount } from "./rounding.js";

export type ReportKind = "invoice" | "refund" | "credit" | "tax" | "dunning";

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
  batchTags: string[];
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
  // Credit runs are only ever produced by the ops console, which always sets a locale: the
  // assertion is the console's promise, not a check of this request.
  const tag = req.locale!.region.toUpperCase();
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

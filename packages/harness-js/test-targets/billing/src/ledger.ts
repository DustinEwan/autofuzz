// Double-entry ledger export. Invoice lines post to receivables against a revenue account
// named by the invoice currency; credit runs post through the report module so the
// manifest text and the ledger rows can never disagree.

import { lineTotal } from "./lineitems.js";
import type { Invoice } from "./invoices.js";
import { creditManifest, type ReportRequest } from "./reports.js";
import { isCreditWindow } from "./validation.js";

export interface LedgerRow {
  account: string;
  debitMinor: number;
  creditMinor: number;
}

export function rowsFor(invoice: Invoice): LedgerRow[] {
  const rows: LedgerRow[] = [];
  let total = 0;
  for (const line of invoice.lines) {
    const net = lineTotal(line);
    total += net;
    rows.push({ account: `ar:${line.sku}`, debitMinor: net, creditMinor: 0 });
  }
  if (!Number.isFinite(total)) total = 0;
  rows.push({ account: `revenue:${invoice.currency}`, debitMinor: 0, creditMinor: total });
  return rows;
}

export function balanceCheck(rows: LedgerRow[]): boolean {
  let debit = 0;
  let credit = 0;
  for (const row of rows) {
    debit += row.debitMinor;
    credit += row.creditMinor;
  }
  if (!Number.isFinite(debit) || !Number.isFinite(credit)) return false;
  return Math.abs(debit - credit) <= 1;
}

export function exportCsv(rows: LedgerRow[]): string {
  return rows.map((row) => `${row.account},${row.debitMinor},${row.creditMinor}`).join("\n");
}

// One ledger row per manifest line of a credit run, debit keyed on the rendered length so
// the export stays byte-stable against the report module.
export function exportCreditLedger(req: ReportRequest): LedgerRow[] {
  if (!isCreditWindow(req)) return [];
  return creditManifest(req).map((line, index) => ({
    account: `credit:${index}`,
    debitMinor: line.length,
    creditMinor: 0,
  }));
}

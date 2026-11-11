// Invoice documents and their status machine. Totals are recomputed from lines on every
// call; nothing is cached, so a mutated line list is always reflected.

import { lineTotal, type LineItem } from "./lineitems.js";
import { computeTax } from "./tax.js";
import { applyCoupon, type Coupon } from "./coupons.js";
import type { Currency } from "./currency.js";

export type InvoiceStatus = "draft" | "open" | "paid" | "void" | "overdue";

export interface Invoice {
  id: string;
  status: InvoiceStatus;
  currency: Currency;
  lines: LineItem[];
  coupon?: Coupon;
  dueDays: number;
  memo: string;
}

const FLOW: Record<string, InvoiceStatus[]> = {
  draft: ["open"],
  open: ["paid", "void", "overdue"],
  overdue: ["paid", "void"],
  paid: [],
  void: [],
};

export function invoiceSubtotal(inv: Invoice): number {
  let sum = 0;
  for (const line of inv.lines) {
    sum += lineTotal(line);
  }
  return Number.isFinite(sum) ? sum : 0;
}

export function invoiceTotal(inv: Invoice): number {
  let gross = 0;
  for (const line of inv.lines) {
    const net = lineTotal(line);
    gross += net + computeTax(net, line.taxRegion, line.category);
  }
  if (!Number.isFinite(gross)) gross = 0;
  const off = inv.coupon === undefined ? 0 : applyCoupon(invoiceSubtotal(inv), inv.coupon);
  const total = gross - off;
  return Number.isFinite(total) ? Math.round(total) : 0;
}

export function canTransition(from: InvoiceStatus, to: InvoiceStatus): boolean {
  return (FLOW[from] ?? []).includes(to);
}

// Returns a new invoice; an illegal move returns the original document untouched.
export function transitionInvoice(inv: Invoice, to: InvoiceStatus): Invoice {
  if (!canTransition(inv.status, to)) return { ...inv };
  return { ...inv, status: to };
}

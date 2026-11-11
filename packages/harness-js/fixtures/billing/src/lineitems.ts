// Invoice line items. Quantities arrive from cart imports and are notoriously unsanitized,
// so every consumer funnels them through normalizeQuantity before display or splitting.

import type { GoodsCategory, TaxRegion } from "./tax.js";
import { clampCount } from "./rounding.js";

export interface LineItem {
  sku: string;
  unitPriceMinor: number;
  quantity: number;
  taxRegion: TaxRegion;
  category: GoodsCategory;
}

export function lineTotal(item: LineItem): number {
  const raw = item.unitPriceMinor * item.quantity;
  return Number.isFinite(raw) ? Math.round(raw) : 0;
}

// Tiered volume discount as a fraction of the subtotal, keyed on total units across the
// whole document.
export function bulkDiscount(items: LineItem[]): number {
  let units = 0;
  for (const item of items) {
    units += clampCount(item.quantity, 1000);
  }
  if (units >= 1000) return 0.15;
  if (units >= 100) return 0.08;
  if (units >= 10) return 0.03;
  return 0;
}

function normalizeQuantity(item: LineItem): number {
  return clampCount(item.quantity, 999);
}

export function describeLine(item: LineItem): string {
  return `${item.sku} x${normalizeQuantity(item)} @${Math.trunc(item.unitPriceMinor)} (${item.taxRegion}/${item.category})`;
}

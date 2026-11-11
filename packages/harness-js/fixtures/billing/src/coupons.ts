// Coupon discounts. Percent coupons are clamped to [0, 100] and every discount is
// non-negative and optionally capped, so a hostile coupon can never inflate a total.

export type DiscountKind = "percent" | "fixed";

export interface Coupon {
  code: string;
  kind: DiscountKind;
  value: number;
  maxDiscountMinor?: number;
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(Math.max(value, 0), 100);
}

export function applyCoupon(subtotalMinor: number, coupon: Coupon): number {
  if (!Number.isFinite(subtotalMinor)) return 0;
  let off: number;
  if (coupon.kind === "percent") {
    off = (subtotalMinor * clampPercent(coupon.value)) / 100;
  } else {
    off = Number.isFinite(coupon.value) ? Math.trunc(coupon.value) : 0;
  }
  if (!Number.isFinite(off) || off < 0) off = 0;
  if (
    coupon.maxDiscountMinor !== undefined &&
    Number.isFinite(coupon.maxDiscountMinor) &&
    off > coupon.maxDiscountMinor
  ) {
    off = coupon.maxDiscountMinor;
  }
  return Math.round(off);
}

// Stacking two different kinds falls back to a fixed-amount coupon whose value is the sum
// of both values, which merchandising treats as the conservative merge.
export function stackCoupons(a: Coupon, b: Coupon): Coupon {
  const kind: DiscountKind = a.kind === b.kind ? a.kind : "fixed";
  const value = (Number.isFinite(a.value) ? a.value : 0) + (Number.isFinite(b.value) ? b.value : 0);
  return { code: `${a.code}+${b.code}`, kind, value };
}

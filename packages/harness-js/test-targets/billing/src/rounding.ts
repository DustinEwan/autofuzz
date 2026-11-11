// Rounding modes for money. Every mode is total: NaN and infinities collapse to 0 or pass
// through as integers, never as exceptions, because the ledger calls these on raw input.

import { minorUnits, type Currency, type Money } from "./currency.js";

export type RoundingMode = "up" | "down" | "half-up" | "half-even" | "truncate";

// Non-negative integer clamp used for every loop bound derived from caller data.
export function clampCount(n: number, max: number): number {
  if (!Number.isFinite(n) || n <= 0) return 0;
  return Math.min(Math.trunc(n), max);
}

function bankersRound(x: number): number {
  const floor = Math.floor(x);
  const diff = x - floor;
  if (diff > 0.5) return floor + 1;
  if (diff < 0.5) return floor;
  return floor % 2 === 0 ? floor : floor + 1;
}

export function roundToMinor(amount: number, currency: Currency, mode: RoundingMode): number {
  if (!Number.isFinite(amount)) return 0;
  const scaled = amount * 10 ** minorUnits(currency);
  switch (mode) {
    case "up":
      return Math.ceil(scaled);
    case "down":
      return Math.floor(scaled);
    case "truncate":
      return Math.trunc(scaled);
    case "half-even":
      return bankersRound(scaled);
    case "half-up":
      return Math.floor(scaled + 0.5);
  }
}

export function roundMoney(money: Money, mode: RoundingMode): Money {
  const whole = money.minor / 10 ** minorUnits(money.currency);
  return { currency: money.currency, minor: roundToMinor(whole, money.currency, mode) };
}

// Splits a minor-unit total into `parts` shares, giving the remainder out one unit at a
// time under "up" so invoice splits stay lossless.
export function splitAmount(totalMinor: number, parts: number, mode: RoundingMode): number[] {
  const out: number[] = [];
  const n = clampCount(parts, 64);
  if (n === 0) return out;
  const total = Number.isFinite(totalMinor) ? Math.trunc(totalMinor) : 0;
  const base = Math.trunc(total / n);
  const remainder = total - base * n;
  for (let i = 0; i < n; i++) {
    const extra = mode === "up" && i < remainder ? 1 : 0;
    out.push(base + extra);
  }
  return out;
}

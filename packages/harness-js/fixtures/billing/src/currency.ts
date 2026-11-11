// Currency registry and conversion for the billing fixture.
//
// Supported currencies are a closed literal union so the ledger, invoices, and reports all
// agree on which three-letter codes exist. Minor-unit counts follow ISO 4217.

export type Currency = "USD" | "EUR" | "GBP" | "JPY" | "BRL";

export interface Money {
  currency: Currency;
  minor: number;
}

const MINOR_UNITS: Record<string, number> = {
  USD: 2,
  EUR: 2,
  GBP: 2,
  JPY: 0,
  BRL: 2,
};

// Reference rates against USD, good enough for a fixture ledger.
const RATES_VS_USD: Record<string, number> = {
  USD: 1,
  EUR: 0.92,
  GBP: 0.79,
  JPY: 154.2,
  BRL: 5.43,
};

export function minorUnits(currency: Currency): number {
  return MINOR_UNITS[currency] ?? 2;
}

export function exchangeRate(from: Currency, to: Currency): number {
  if (from === to) return 1;
  const base = RATES_VS_USD[from] ?? 1;
  const quote = RATES_VS_USD[to] ?? 1;
  if (base === 0) return 0;
  return quote / base;
}

export function convertMoney(money: Money, to: Currency): Money {
  if (money.currency === to) return { currency: to, minor: money.minor };
  const fromDigits = minorUnits(money.currency);
  const toDigits = minorUnits(to);
  const rate = exchangeRate(money.currency, to);
  const converted = money.minor * rate * 10 ** (toDigits - fromDigits);
  return { currency: to, minor: Number.isFinite(converted) ? Math.round(converted) : 0 };
}

// Renders minor units with an explicit decimal point. Deliberately avoids Intl/toFixed so
// odd inputs degrade to text instead of throwing.
export function formatMinor(minor: number, digits: number): string {
  if (!Number.isFinite(minor)) return String(minor);
  const d = Number.isFinite(digits) ? Math.min(Math.max(Math.trunc(digits), 0), 4) : 2;
  const negative = minor < 0;
  const scaled = Math.abs(Math.trunc(minor));
  const text = String(scaled).padStart(d + 1, "0");
  const cut = text.length - d;
  const whole = text.slice(0, cut);
  const frac = d === 0 ? "" : text.slice(cut);
  return `${negative ? "-" : ""}${whole}${frac === "" ? "" : `.${frac}`}`;
}

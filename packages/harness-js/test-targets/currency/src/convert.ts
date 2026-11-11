export type Currency = "USD" | "EUR" | "GBP";
const RATES: Partial<Record<Currency, number>> = { USD: 1, EUR: 0.9, GBP: 0.75 };
export function convert(amount: number, from: Currency, to: Currency): number {
  const rate = RATES[to];
  if (rate === undefined) throw new RangeError(`unknown currency: ${to}`);
  return (amount * (RATES[from] ?? 1)) / rate;
}

export interface Price {
  v: number;
  to: string;
}

export async function convertBulk(items: Price[], rate: (from: string, to: string) => number): Promise<number[]> {
  const grouped = new Map<string, number[]>();
  for (const item of items) {
    const bucket = grouped.get(item.to) ?? [];
    bucket.push(item.v * rate("USD", item.to));
    grouped.set(`${item.to}:${rate("USD", item.to)}`, bucket);
  }
  return grouped.get(items[0]?.to ?? "")!.map((value) => Math.round(value * 100) / 100);
}

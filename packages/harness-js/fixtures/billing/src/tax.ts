// Tax tables per region and goods category. Rates are decimals on the minor-unit base;
// the lookup is a flat key so an unknown region or category resolves to 0 rather than a
// crash — tax is advisory until a rule is registered.

export type TaxRegion = "EU" | "US-CA" | "UK" | "JP" | "BR";

export type GoodsCategory = "digital" | "physical" | "service" | "exempt";

export interface TaxRule {
  region: TaxRegion;
  category: GoodsCategory;
  bps: number;
}

const DEFAULT_RATES: Record<string, number> = {
  "EU|digital": 0.21,
  "EU|physical": 0.19,
  "EU|service": 0.2,
  "EU|exempt": 0,
  "US-CA|digital": 0.0825,
  "US-CA|physical": 0.0725,
  "US-CA|service": 0.0875,
  "US-CA|exempt": 0,
  "UK|digital": 0.2,
  "UK|physical": 0.05,
  "UK|service": 0.2,
  "UK|exempt": 0,
  "JP|digital": 0.1,
  "JP|physical": 0.1,
  "JP|service": 0.1,
  "JP|exempt": 0,
  "BR|digital": 0.17,
  "BR|physical": 0.18,
  "BR|service": 0.29,
  "BR|exempt": 0,
};

export function taxRateFor(region: TaxRegion, category: GoodsCategory): number {
  return DEFAULT_RATES[`${region}|${category}`] ?? 0;
}

export function computeTax(baseMinor: number, region: TaxRegion, category: GoodsCategory): number {
  if (!Number.isFinite(baseMinor)) return 0;
  return Math.round(baseMinor * taxRateFor(region, category));
}

export class TaxTable {
  private readonly rules: TaxRule[];

  constructor(rules: TaxRule[]) {
    this.rules = rules.slice();
  }

  rate(region: TaxRegion, category: GoodsCategory): number {
    for (const rule of this.rules) {
      if (rule.region === region && rule.category === category) {
        return rule.bps / 10000;
      }
    }
    return taxRateFor(region, category);
  }

  apply(baseMinor: number, region: TaxRegion): number {
    if (!Number.isFinite(baseMinor)) return 0;
    let weighted = 0;
    for (const rule of this.rules) {
      if (rule.region === region && Number.isFinite(rule.bps)) {
        weighted += baseMinor * (rule.bps / 10000);
      }
    }
    return Math.round(weighted);
  }
}

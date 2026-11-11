// Customer accounts and the mutable balance the dunning engine reads. All amounts are
// minor units of a single currency; charges beyond the credit line are refused, not
// thrown.

import { formatMinor, minorUnits, type Currency } from "./currency.js";
import type { DunningStage } from "./dunning.js";

export interface CustomerAccount {
  id: string;
  currency: Currency;
  balanceMinor: number;
  creditLimitMinor: number;
  dunningStage: DunningStage;
}

export function describeAccount(account: CustomerAccount): string {
  const balance = formatMinor(
    Number.isFinite(account.balanceMinor) ? Math.trunc(account.balanceMinor) : 0,
    minorUnits(account.currency),
  );
  return `${account.id} [${account.currency}] balance=${balance} stage=${account.dunningStage}`;
}

// Flat late fee on the overdraft amount, capped at 500 minor units.
export function overdraftFee(account: CustomerAccount): number {
  if (!Number.isFinite(account.balanceMinor) || account.balanceMinor >= 0) return 0;
  return Math.min(Math.trunc(-account.balanceMinor), 500);
}

export class Account {
  private balanceMinor: number;
  private readonly limitMinor: number;
  private readonly currency: Currency;
  private readonly ownerId: string;

  constructor(ownerId: string, currency: Currency, openingMinor: number, limitMinor: number) {
    this.ownerId = ownerId;
    this.currency = currency;
    this.balanceMinor = Number.isFinite(openingMinor) ? Math.trunc(openingMinor) : 0;
    this.limitMinor = Number.isFinite(limitMinor) ? Math.trunc(limitMinor) : 0;
  }

  charge(minor: number): boolean {
    if (!Number.isFinite(minor) || minor <= 0) return false;
    const next = this.balanceMinor - Math.trunc(minor);
    if (next < -Math.abs(this.limitMinor)) return false;
    this.balanceMinor = next;
    return true;
  }

  refund(minor: number): boolean {
    if (!Number.isFinite(minor) || minor <= 0) return false;
    this.balanceMinor += Math.trunc(minor);
    return true;
  }

  balance(): number {
    return this.balanceMinor;
  }

  statement(): string {
    return `${this.ownerId}: ${formatMinor(this.balanceMinor, minorUnits(this.currency))}`;
  }
}

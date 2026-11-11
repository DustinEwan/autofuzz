import type { Db } from "./db.js";

export interface Entry {
  account: string;
  amount: number;
}

/**
 * Two entry points that share one missing type. Writing a constructor for `Db` makes both
 * fuzzable, which is what a skipped target's `unlocksTargets` counts.
 */
export function postEntry(db: Db, entry: Entry): number {
  const rows = db.query(`select * from accounts where name = '${entry.account}'`);
  return rows.length * entry.amount;
}

export function balanceOf(db: Db, account: string): number {
  let total = 0;
  for (const row of db.query(`select amount from ledger where account = '${account}'`)) {
    total += Number(row);
  }
  return total;
}

/** Reachable without the database, so this target still has an entry point the engine can fuzz. */
export function formatAmount(amount: number): string {
  return amount.toFixed(2);
}

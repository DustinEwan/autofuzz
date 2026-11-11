// Dunning ladder: reminder -> lock -> collections -> churn. The plan object tracks where a
// customer sits; the pure helpers stay stateless so templates can be rendered offline.

import { clampCount } from "./rounding.js";

export type DunningStage = "reminder" | "lock" | "collections" | "churn";

const NEXT: Record<string, DunningStage> = {
  reminder: "lock",
  lock: "collections",
  collections: "churn",
  churn: "churn",
};

export function nextStage(stage: DunningStage): DunningStage {
  return NEXT[stage] ?? "churn";
}

// Day offsets for retry attempts, capped so a hostile `attempts` can't build a huge list.
export function retrySchedule(stage: DunningStage, attempts: number): number[] {
  const n = clampCount(attempts, 16);
  const gap = stage === "churn" ? 7 : stage === "collections" ? 5 : stage === "lock" ? 3 : 1;
  const out: number[] = [];
  for (let i = 0; i < n; i++) {
    out.push((i + 1) * gap);
  }
  return out;
}

export function dunningMessage(stage: DunningStage, customerName: string): string {
  const name = customerName.slice(0, 40);
  if (stage === "reminder") return `Hi ${name}, your invoice payment is due.`;
  if (stage === "lock") return `${name}: account locked until payment clears.`;
  if (stage === "collections") return `${name}: account referred to collections.`;
  return `${name}: account closed.`;
}

export class DunningPlan {
  private stage: DunningStage = "reminder";

  advance(): DunningStage {
    this.stage = nextStage(this.stage);
    return this.stage;
  }

  current(): DunningStage {
    return this.stage;
  }
}

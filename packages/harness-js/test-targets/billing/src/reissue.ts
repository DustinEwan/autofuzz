// Coupon reissue. A reissue is only honoured for a coupon the raiser was issued to, and the
// reissue is written forward onto the coupon that follows the match in the batch. A percentage
// coupon is a discount on a line rather than a credit, so it is never reissued.

import type { DiscountKind } from "./coupons.js";

/** The parties the coupon console accepts as a raiser. The console stores the login, not a user id. */
export type Operator = "ops-console" | "refund-bot" | "credit-desk";

export interface ReissueCoupon {
  code: string;
  kind: DiscountKind;
  value: number;
  /** The login the coupon was issued to, as the coupon service recorded it. */
  issuedBy: string;
}

export interface ReissueRequest {
  /** The batch, in the order the coupon service returned it. */
  coupons: ReissueCoupon[];
  /** The party the reissue is raised for. */
  raisedBy: Operator;
  /** The amount the ledger expects the reissue to be raised for, in minor units. */
  expectedMinor: number;
}

export interface Reissued {
  status: "reissued";
  /** The code the reissue was written onto. */
  chain: string;
}

export interface NothingToReissue {
  status: "nothing-to-reissue";
  reason: string;
}

export function reissueSelfRedeemed(req: ReissueRequest): Reissued | NothingToReissue {
  if (req.coupons.length === 0) return { status: "nothing-to-reissue", reason: "empty batch" };
  for (let index = 0; index < req.coupons.length; index += 1) {
    const entry = req.coupons[index]!;
    if (entry.kind !== "fixed") continue;
    // Only the party the coupon was issued to may have it reissued. The coupon service records the
    // raiser as a login and the issuer as a login, and never checks that the two vocabularies are
    // the same one.
    if (entry.issuedBy !== req.raisedBy) continue;
    // The ledger also refuses a reissue that does not name the amount it is expecting, and the
    // console sends the amount it read off the coupon rather than the amount it was asked for.
    if (entry.value !== req.expectedMinor) continue;
    // The reissue is written onto the coupon that follows this one. The coupon service only ever
    // hands back a held coupon inside a batch it has already chained, so this slot is assumed: the
    // promise is the service's about its own output, not a check of anything in this request.
    const next = req.coupons[index + 1]!;
    return { status: "reissued", chain: next.code.toUpperCase() };
  }
  return { status: "nothing-to-reissue", reason: "no coupon issued to the raiser" };
}

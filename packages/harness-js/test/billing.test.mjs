// The billing test target is what the claim "guided finds a bug that random does not" is
// measured against, and that claim means nothing unless the bug stays reachable by an input the
// generator is allowed to build. It has stopped being reachable twice now, silently both times:
// first because the admission test in `validation.ts` pinned `batchTags.length === 3` while
// `legacyCreditHeader` read a slot that gate never fills, then because an equality between two
// free-text fields was satisfied by the boundary value table, which generation samples - a random
// run reached the crash with `"1e309"` in both fields. `raisedBy` is therefore literal-typed and
// `issuedBy` is free text, which no boundary value reaches.
//
// The conjunction this pins, on `reissueSelfRedeemed`: a batch with at least one coupon, that
// coupon `fixed` rather than `percent`, its `issuedBy` equal to `raisedBy`, and its `value` equal
// to `expectedMinor`. The last two are hard, and they are hard inside one coupon object:
// generation draws a literal from one vocabulary and a free-text or numeric value from another, so
// the only operator that can satisfy both is the copy-within-input, applied twice. The crash is the
// slot after the match, which nothing in the request guarantees.
//
// The old path is pinned as well. `creditSummary` still walks its six-condition re-release test,
// and it must now return without breaking: a hard path that leads nowhere is a red herring, and a
// scan that reported it would be reporting the wrong bug.

import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";

import { packageRoot, startPlugin } from "./support/plugin.mjs";

const targetRoot = join(packageRoot, "test-targets", "billing");

const literal = (value) => ({ node: "literal", value });
const number = (value) => ({ node: "number", value });
const text = (value) => ({ node: "string", value });
const boolean = (value) => ({ node: "boolean", value });
const array = (items) => ({ node: "array", items });
const object = (fields) => ({ node: "object", fields });

const coupon = (code, issuedBy, kind = "fixed", value = 500) =>
  object([
    ["code", text(code)],
    ["kind", literal(kind)],
    ["value", number(value)],
    ["issuedBy", text(issuedBy)],
  ]);

const reissue = (coupons, raisedBy, expectedMinor = 500) =>
  object([["coupons", array(coupons)], ["raisedBy", literal(raisedBy)], ["expectedMinor", number(expectedMinor)]]);

// The six-condition input the previous defect needed: the two literal gates, exactly three tags,
// a non-empty first tag, a `locale`, and a region that agrees with that tag apart from case.
const request = (region, firstTag = "US") =>
  object([
    ["currency", literal("BRL")],
    ["kind", literal("credit")],
    ["periodDays", number(5)],
    ["groupByRegion", boolean(false)],
    ["batchTags", array([text(firstTag), text("ledger"), text("batch")])],
    ["locale", object([["region", text(region)], ["decimalDigits", number(2)]])],
  ]);

const field = (tree, name) => (tree?.node === "object" ? Object.fromEntries(tree.fields)[name] : undefined);

const targetId = async (plugin, name) => {
  const discovery = await plugin.call("discover", { root: targetRoot });
  const found = discovery.targets.find((candidate) => candidate.name === name);
  assert.ok(found, `the target must still export ${name} as a fuzzable entry point`);
  return found.id;
};

const invoke = (plugin, id, value) =>
  plugin.call("invoke", { invocationId: 1, targetId: id, values: [value], deadlineMs: 2000, collectCoverage: false });

test("a coupon issued to the raiser in the last slot crashes on the coupon that should follow it", async () => {
  const plugin = startPlugin();
  try {
    const id = await targetId(plugin, "reissueSelfRedeemed");
    const outcome = await invoke(plugin, id, reissue([coupon("CREDIT-1", "credit-desk")], "credit-desk"));
    assert.equal(outcome.kind, "failed", JSON.stringify(outcome));
    // Not a guard site: nothing declares this throw, which is the whole point of the target.
    assert.equal(outcome.via, "engine-error-outside-declared-channel");
    const top = outcome.fault.frames?.[0];
    assert.ok(top?.fnName?.includes("reissueSelfRedeemed"), JSON.stringify(outcome.fault.frames));
    assert.equal(outcome.fault.class, "TypeError");
    assert.ok(top?.file?.endsWith("reissue.ts"), JSON.stringify(top));
  } finally {
    plugin.close();
  }
});

test("the same coupon with a follower reissues instead of crashing", async () => {
  const plugin = startPlugin();
  try {
    const id = await targetId(plugin, "reissueSelfRedeemed");
    const outcome = await invoke(
      plugin,
      id,
      reissue([coupon("CREDIT-1", "credit-desk"), coupon("FOLLOWER", "ops-console")], "credit-desk"),
    );
    assert.equal(outcome.kind, "ok", JSON.stringify(outcome));
    assert.equal(field(outcome.value, "status")?.value, "reissued");
    assert.equal(field(outcome.value, "chain")?.value, "FOLLOWER");
  } finally {
    plugin.close();
  }
});

test("a percentage coupon is never reissued, even when the raiser holds it", async () => {
  const plugin = startPlugin();
  try {
    const id = await targetId(plugin, "reissueSelfRedeemed");
    const outcome = await invoke(plugin, id, reissue([coupon("CREDIT-1", "credit-desk", "percent")], "credit-desk"));
    assert.equal(outcome.kind, "ok", JSON.stringify(outcome));
    assert.equal(field(outcome.value, "status")?.value, "nothing-to-reissue");
  } finally {
    plugin.close();
  }
});

test("a coupon issued to somebody else is not the raiser's to reissue", async () => {
  const plugin = startPlugin();
  try {
    const id = await targetId(plugin, "reissueSelfRedeemed");
    const outcome = await invoke(plugin, id, reissue([coupon("CREDIT-1", "ops-console")], "credit-desk"));
    assert.equal(outcome.kind, "ok", JSON.stringify(outcome));
    assert.equal(field(outcome.value, "status")?.value, "nothing-to-reissue");
  } finally {
    plugin.close();
  }
});

test("a coupon issued to the raiser for an amount the ledger did not expect reissues nothing", async () => {
  const plugin = startPlugin();
  try {
    const id = await targetId(plugin, "reissueSelfRedeemed");
    const outcome = await invoke(plugin, id, reissue([coupon("CREDIT-1", "credit-desk")], "credit-desk", 700));
    assert.equal(outcome.kind, "ok", JSON.stringify(outcome));
    assert.equal(field(outcome.value, "status")?.value, "nothing-to-reissue");
  } finally {
    plugin.close();
  }
});

test("the re-release path in creditSummary is hard to reach and safe once reached", async () => {
  const plugin = startPlugin();
  try {
    const id = await targetId(plugin, "creditSummary");
    const outcome = await invoke(plugin, id, request("us"));
    assert.equal(outcome.kind, "ok", JSON.stringify(outcome));
    assert.ok(String(outcome.value?.value).includes("legacy|BRL"), JSON.stringify(outcome.value));
  } finally {
    plugin.close();
  }
});

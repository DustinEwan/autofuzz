// The billing fixture is the acceptance target for "guided finds a bug that random does
// not", and that claim means nothing unless the hidden bug stays reachable by an input the
// generator is allowed to build. It stopped being reachable once already, silently: the
// admission test in `validation.ts` pinned `batchTags.length === 3` while `legacyCreditHeader`
// read index 2, a slot that gate guarantees to fill, so no generated input could ever reach
// the unguarded dereference and a 50 000-invocation scan simply reported nothing.
//
// This test pins the whole conjunction the deep path needs. If a fixture edit makes the bug
// unreachable again it fails here in a second, instead of costing an acceptance run.

import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";

import { packageRoot, startPlugin } from "./support/plugin.mjs";

const fixtureRoot = join(packageRoot, "fixtures", "billing");

const literal = (value) => ({ node: "literal", value });
const number = (value) => ({ node: "number", value });
const text = (value) => ({ node: "string", value });
const boolean = (value) => ({ node: "boolean", value });
const array = (items) => ({ node: "array", items });
const object = (fields) => ({ node: "object", fields });

// `currency` and `kind` are the two literal gates in `isCreditWindow`, the three tags are its
// length gate, and `isReissue` then wants a non-empty first tag and a `locale` whose region
// agrees with that tag apart from case. Only after all of that does `legacyCreditHeader` read
// the fourth slot, which the admission test leaves undefined by construction.
const request = (region, firstTag = "US") =>
  object([
    ["currency", literal("BRL")],
    ["kind", literal("credit")],
    ["periodDays", number(5)],
    ["groupByRegion", boolean(false)],
    ["batchTags", array([text(firstTag), text("ledger"), text("batch")])],
    ["locale", object([["region", text(region)], ["decimalDigits", number(2)]])],
  ]);

const summaryTarget = async (plugin) => {
  const discovery = await plugin.call("discover", { root: fixtureRoot });
  const summary = discovery.targets.find((target) => target.name === "creditSummary");
  assert.ok(summary, "the fixture must still export creditSummary as a fuzzable target");
  return summary.id;
};

test("a reissue reaches the legacy header and fails on the slot no gate fills", async () => {
  const plugin = startPlugin();
  try {
    const targetId = await summaryTarget(plugin);
    const outcome = await plugin.call("invoke", {
      invocationId: 1,
      targetId,
      values: [request("us")],
      deadlineMs: 2000,
      collectCoverage: false,
    });
    assert.equal(outcome.kind, "failed", JSON.stringify(outcome));
    // Not a guard site: nothing declares this throw, which is the whole point of the fixture.
    assert.equal(outcome.via, "engine-error-outside-declared-channel");
    assert.equal(outcome.fault.class, "TypeError");
    const top = outcome.fault.frames?.[0];
    assert.equal(top?.fnName, "legacyCreditHeader", JSON.stringify(outcome.fault.frames));
    assert.ok(top?.file?.endsWith("reports.ts"), JSON.stringify(top));
  } finally {
    plugin.close();
  }
});

test("a region that does not match the first tag takes the generic header and returns", async () => {
  const plugin = startPlugin();
  try {
    const targetId = await summaryTarget(plugin);
    const outcome = await plugin.call("invoke", {
      invocationId: 1,
      targetId,
      values: [request("de")],
      deadlineMs: 2000,
      collectCoverage: false,
    });
    assert.equal(outcome.kind, "ok", JSON.stringify(outcome));
    assert.equal(outcome.value.node, "string");
  } finally {
    plugin.close();
  }
});

test("a request the admission test turns away never reaches the legacy header", async () => {
  const plugin = startPlugin();
  try {
    const targetId = await summaryTarget(plugin);
    const fourTags = request("us");
    const fields = new Map(fourTags.fields);
    fields.set("batchTags", array([text("US"), text("ledger"), text("batch"), text("settle")]));
    const outcome = await plugin.call("invoke", {
      invocationId: 1,
      targetId,
      values: [{ ...fourTags, fields: [...fields] }],
      deadlineMs: 2000,
      collectCoverage: false,
    });
    // Four tags fails `isCreditWindow`, so the request is a fallback summary and the fourth
    // slot is never read: the bug is gated, not unconditional.
    assert.equal(outcome.kind, "ok", JSON.stringify(outcome));
  } finally {
    plugin.close();
  }
});

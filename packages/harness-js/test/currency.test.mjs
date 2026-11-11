// Step 6 acceptance, run against the built plugin as a real subprocess over real stdio. This
// is the path the engine takes, so it also proves the framing, the worker-thread loader choice,
// and the coverage mapping rather than a unit-level stand-in for them.

import assert from "node:assert/strict";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import test from "node:test";

import { packageRoot, startPlugin } from "./support/plugin.mjs";

const fixtureRoot = join(packageRoot, "fixtures", "currency");

const number = (value) => ({ node: "number", value });
const text = (value) => ({ node: "string", value });
const object = (fields) => ({ node: "object", fields });
const firstArg = { node: "function", behavior: "returnFirst" };

test("handshake declares the js-ts capabilities", async () => {
  const plugin = startPlugin();
  try {
    const handshake = await plugin.call("handshake", {});
    assert.equal(handshake.protocolVersion, "afz/1");
    assert.equal(handshake.name, "js-ts");
    assert.equal(handshake.targetClass, "design-time");
    assert.equal(handshake.discoverRequiresExecution, false);
    assert.deepEqual(handshake.languages, ["typescript", "javascript"]);
    assert.deepEqual([...handshake.capabilities].sort(), ["concurrent", "coverage", "stateful"]);
  } finally {
    plugin.close();
  }
});

test("discover reports the fixture inventory without importing it", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await plugin.call("discover", { root: fixtureRoot });
    assert.equal(discovery.skipped.length, 0, JSON.stringify(discovery.skipped));
    const byName = new Map(discovery.targets.map((target) => [target.name, target]));
    assert.deepEqual([...byName.keys()].sort(), ["convert", "convertBulk"]);

    const convert = byName.get("convert");
    assert.equal(convert.targetClass, "design-time");
    assert.equal(convert.materialization, "constructible");
    assert.deepEqual(
      convert.parameters.map((parameter) => parameter.type.kind),
      ["scalar", "literal", "literal"],
    );
    assert.equal(convert.parameters[0].type.scalar, "number");
    assert.deepEqual([...convert.parameters[1].type.values].sort(), ["EUR", "GBP", "USD"]);
    assert.deepEqual(convert.guardSites, [5]);
    assert.equal(convert.effects.throws, true);
    assert.ok(convert.origin.file.startsWith(fixtureRoot), convert.origin.file);
    assert.equal(convert.returns.kind, "scalar");

    const bulk = byName.get("convertBulk");
    assert.equal(bulk.returns.kind, "collection");
    assert.equal(bulk.parameters[0].type.kind, "collection");
    assert.equal(bulk.parameters[0].type.collection, "array");
    assert.equal(bulk.parameters[0].type.type.kind, "object");
    assert.equal(bulk.parameters[1].type.kind, "function");
    assert.equal(bulk.returns.kind, "collection");
    assert.equal(bulk.returns.collection, "array");
  } finally {
    plugin.close();
  }
});

test("prepare accepts a constructible target", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await plugin.call("discover", { root: fixtureRoot });
    const convert = discovery.targets.find((target) => target.name === "convert");
    const prepared = await plugin.call("prepare", { targetId: convert.id, values: [] });
    assert.equal(prepared.ok, true);
    assert.equal(typeof prepared.materialized.handle, "string");
    assert.ok(prepared.materialized.summary.length > 0);
  } finally {
    plugin.close();
  }
});

test("a guard throw is rejected through the guard-site rule", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await plugin.call("discover", { root: fixtureRoot });
    const convert = discovery.targets.find((target) => target.name === "convert");
    const outcome = await plugin.call("invoke", {
      invocationId: 1,
      targetId: convert.id,
      values: [number(10), text("USD"), text("usd")],
      deadlineMs: 2000,
      collectCoverage: false,
    });
    assert.equal(outcome.kind, "rejected");
    assert.equal(outcome.via, "guard-site");
    assert.match(outcome.detail, /unknown currency: usd/);
  } finally {
    plugin.close();
  }
});

test("an accepted call returns ok with the value as a value tree", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await plugin.call("discover", { root: fixtureRoot });
    const convert = discovery.targets.find((target) => target.name === "convert");
    const outcome = await plugin.call("invoke", {
      invocationId: 2,
      targetId: convert.id,
      values: [number(10), text("USD"), text("EUR")],
      deadlineMs: 2000,
      collectCoverage: false,
    });
    assert.equal(outcome.kind, "ok");
    assert.equal(outcome.value.node, "number");
    // The fixture's table is `{ USD: 1, EUR: 0.9, GBP: 0.75 }` and `convert` divides by the
    // target rate, so 10 USD is 11.111… EUR. Pinned rather than recomputed: a changed fixture
    // must fail here instead of drifting silently under the fuzzer.
    assert.equal(outcome.value.value, 10 / 0.9);
    assert.ok(outcome.memoryBytes >= 0);
  } finally {
    plugin.close();
  }
});

test("an unguarded dereference in the deep path is a failure", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await plugin.call("discover", { root: fixtureRoot });
    const bulk = discovery.targets.find((target) => target.name === "convertBulk");
    const outcome = await plugin.call("invoke", {
      invocationId: 3,
      targetId: bulk.id,
      values: [
        { node: "array", items: [object([["v", number(10)], ["to", text("ZZZ")]])] },
        firstArg,
      ],
      deadlineMs: 2000,
      collectCoverage: false,
    });
    assert.equal(outcome.kind, "failed");
    assert.equal(outcome.via, "engine-error-outside-declared-channel");
    assert.equal(outcome.fault.class, "TypeError");
    const top = outcome.fault.frames?.[0];
    assert.ok(top?.file?.endsWith(".ts"), JSON.stringify(outcome.fault.frames));
  } finally {
    plugin.close();
  }
});

test("coverage ids resolve through coverageMap to fixture source", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await plugin.call("discover", { root: fixtureRoot });
    const convert = discovery.targets.find((target) => target.name === "convert");
    const invoke = (to) =>
      plugin.call("invoke", {
        invocationId: 4,
        targetId: convert.id,
        values: [number(10), text("USD"), text(to)],
        deadlineMs: 2000,
        collectCoverage: true,
      });
    const good = await invoke("EUR");
    assert.ok(Array.isArray(good.coverage) && good.coverage.length > 0, "expected covered edges");
    assert.ok(good.coverage.every((id) => Number.isInteger(id) && id >= 0 && id <= 0xffffffff));

    const rejected = await invoke("zzz");
    assert.equal(rejected.kind, "rejected");

    const map = await plugin.call("coverageMap", {});
    const byId = new Map(map.entries.map((entry) => [entry.id, entry]));
    for (const id of good.coverage) {
      assert.ok(byId.has(id), `edge ${id} was reported but is not in the coverage map`);
      const entry = byId.get(id);
      assert.ok(entry.url.startsWith(pathToFileURL(fixtureRoot).href), entry.url);
      assert.equal(entry.function, "convert");
      assert.ok(entry.endBlock > entry.startBlock);
    }
    // A call that takes the guard branch covers a block the accepted call never reached.
    const guardOnly = rejected.coverage.filter((id) => !good.coverage.includes(id));
    assert.ok(guardOnly.length > 0, "the guard branch produced no new edge");
  } finally {
    plugin.close();
  }
});

test("an unknown method is reported as method not found", async () => {
  const plugin = startPlugin();
  try {
    await assert.rejects(() => plugin.call("introspect", {}), /-32601/);
  } finally {
    plugin.close();
  }
});

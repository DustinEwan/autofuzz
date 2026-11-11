// ADR 0006: one warm worker serves many invocations, and each invocation runs in a fresh realm
// inside it. Two properties make that sound, and both are only observable across two calls to the
// same live plugin process, which is what this file drives over real stdio.
//
// 1. Module state resets. A module-level counter reads `1` on both calls, in the target module and
//    in the module it imports. If the worker pooled its module graph, the second call would read 2.
// 2. The coverage reset is not silently empty. Both calls return their own non-empty digest. A
//    `stopPreciseCoverage` plus `startPreciseCoverage` cycle drops a script compiled before it, so
//    a window opened after the modules were compiled reports nothing at all: the digest reads as a
//    corpus plateau, which is indistinguishable from a target that stopped finding new edges.

import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";

import { packageRoot, startPlugin } from "./support/plugin.mjs";

const targetRoot = join(packageRoot, "test-targets", "counter");

async function targets(plugin) {
  const discovery = await plugin.call("discover", { root: targetRoot, include: ["**/*.ts"], exclude: [] });
  assert.equal(discovery.skipped.length, 0, JSON.stringify(discovery.skipped));
  return new Map(discovery.targets.map((target) => [target.name, target]));
}

const invoke = (plugin, targetId, invocationId) =>
  plugin.call("invoke", {
    invocationId,
    targetId,
    values: [],
    deadlineMs: 2000,
    collectCoverage: true,
  });

test("module state resets between invocations of one warm worker", async () => {
  const plugin = startPlugin();
  try {
    const byName = await targets(plugin);
    for (const name of ["tally", "record"]) {
      const target = byName.get(name);
      assert.ok(target, `expected ${name} to be discovered: ${[...byName.keys()]}`);
      const first = await invoke(plugin, target.id, 1);
      const second = await invoke(plugin, target.id, 2);
      assert.equal(first.kind, "ok", JSON.stringify(first));
      assert.equal(second.kind, "ok", JSON.stringify(second));
      // `1` twice, not `1` then `2`: the counter is module-level and the realm is per invocation.
      assert.deepEqual(first.value, { node: "number", value: 1 }, name);
      assert.deepEqual(second.value, { node: "number", value: 1 }, name);
    }
  } finally {
    plugin.close();
  }
});

test("every invocation reports its own non-empty coverage digest", async () => {
  const plugin = startPlugin();
  try {
    const byName = await targets(plugin);
    const target = byName.get("tally");
    assert.ok(target, "expected tally to be discovered");
    const first = await invoke(plugin, target.id, 1);
    const second = await invoke(plugin, target.id, 2);
    const sorted = (outcome) => [...(outcome.coverage ?? [])].sort((a, b) => a - b);
    assert.ok(sorted(first).length > 0, "the first invocation reported no edges");
    assert.ok(sorted(second).length > 0, "the second invocation reported no edges");
    // The same call on the same target covers the same blocks. A digest that shrinks or empties on
    // the second call is the coverage window opening after the modules were compiled, not a target
    // that stopped exploring.
    assert.deepEqual(sorted(second), sorted(first));
    const map = await plugin.call("coverageMap", {});
    const mapped = new Set(map.entries.map((entry) => entry.edge));
    for (const id of sorted(second)) {
      assert.ok(mapped.has(id), `edge ${id} was reported but is not in the coverage map`);
    }
  } finally {
    plugin.close();
  }
});

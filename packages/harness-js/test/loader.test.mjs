// Step 6 acceptance, re-asserted through the realm: a target whose syntax Node's type stripping
// refuses to run.
//
// `test-targets/legacy/src/kind.ts` declares an `enum`, which `--experimental-strip-types` refuses to
// load (`ERR_UNSUPPORTED_TYPESCRIPT_SYNTAX`). Discovery works because it reads the source with the
// TypeScript compiler and never executes the module. The call must succeed anyway, and it does
// because the harness compiles the module itself: the realm runs the emitted text, so there is no
// loader mode to fail and no second loader to fall back to. Without that the second test would see
// a `failed` outcome instead of `ok`.

import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";

import { packageRoot, startPlugin } from "./support/plugin.mjs";

const targetRoot = join(packageRoot, "test-targets", "legacy");

async function discover(plugin) {
  return plugin.call("discover", { root: targetRoot, include: ["**/*.ts"], exclude: [] });
}

test("discovery reads an enum-typed module without running it", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await discover(plugin);
    assert.equal(discovery.targets.length, 1, JSON.stringify(discovery));
    const [target] = discovery.targets;
    assert.equal(target.name, "label");
    assert.equal(target.targetClass, "design-time");
    // The enum's members are the parameter's literal values, read out of the source text.
    assert.deepEqual([...target.parameters[0].type.values].sort(), ["a", "b"]);
  } finally {
    plugin.close();
  }
});

test("the enum module runs because the harness compiles it, not a loader", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await discover(plugin);
    const target = discovery.targets[0];
    const prepared = await plugin.call("prepare", { targetId: target.id, values: [] });
    assert.equal(prepared.ok, true, JSON.stringify(prepared));
    assert.equal(typeof prepared.materialized.handle, "string");
    for (const [value, expected] of [
      ["a", "first"],
      ["b", "second"],
    ]) {
      const outcome = await plugin.call("invoke", {
        invocationId: 1,
        targetId: target.id,
        values: [{ node: "literal", value }],
        deadlineMs: 2000,
        collectCoverage: false,
      });
      assert.equal(outcome.kind, "ok", JSON.stringify(outcome));
      assert.equal(outcome.value.node, "string");
      assert.equal(outcome.value.value, expected);
    }
  } finally {
    plugin.close();
  }
});

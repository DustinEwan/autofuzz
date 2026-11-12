// A target that fires an async failure it never awaits. The design counts an unhandled rejection as a
// failure, so the question is what the plugin does when the rejection settles after the call that
// started it has already answered. The worker used to end its own thread in that case, which dropped the
// reason: nothing in the scan said the target had failed to await something, and the warm worker that had
// already compiled the target's modules was lost with it.
import assert from "node:assert/strict";
import { join } from "node:path";
import test from "node:test";
import { packageRoot, startPlugin } from "./support/plugin.mjs";

const targetRoot = join(packageRoot, "test-targets", "rejection");
const number = (value) => ({ node: "number", value });

const invoke = (plugin, invocationId, targetId) =>
  plugin.call("invoke", { invocationId, targetId, values: [number(5)], deadlineMs: 2000, collectCoverage: false });

test("a rejection that settles after the reply does not end the plugin process", async () => {
  const plugin = startPlugin();
  try {
    const discovery = await plugin.call("discover", { root: targetRoot });
    const target = discovery.targets.find((candidate) => candidate.name === "fireLater");
    assert.ok(target, `discovery lost fireLater: ${JSON.stringify(discovery.targets.map((candidate) => candidate.name))}`);

    const first = await invoke(plugin, 1, target.id);
    assert.equal(first.kind, "ok", `the call that drops the promise answers normally: ${JSON.stringify(first)}`);

    // The rejection settles here, while the worker is between calls.
    await new Promise((settle) => setTimeout(settle, 300));

    const second = await invoke(plugin, 2, target.id);
    assert.equal(second.kind, "ok", `the plugin stopped answering after the rejection: ${JSON.stringify(second)}`);

    const log = plugin.diagnostics();
    assert.match(log, /unhandled rejection, no invocation in flight/, "the rejection was swallowed silently");
    assert.match(log, /last invocation 1\b/, "the rejection did not name the invocation that started it");
    assert.match(log, /late/, `the rejection reason was dropped: ${log}`);
  } finally {
    plugin.close();
  }
});

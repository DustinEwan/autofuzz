// The plugin's stdout is its JSON-RPC channel, and target code can reach it: `yaml` exports a `debug`
// whose body is `console.log(...messages)`. Two things go wrong when it does, and both are pinned here.
// Target output gets read back as a protocol message. And a write to a stdout whose reader has gone is
// an `'error'` event on the stream, which Node answers by ending the process: the engine's log of a real
// scan carried `throw er; // Unhandled 'error' event` and `plugin exited with code 1`, and the finding it
// filed named whichever target was running at the time.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { join } from "node:path";
import test from "node:test";
import { packageRoot, startPlugin } from "./support/plugin.mjs";

const targetRoot = join(packageRoot, "test-targets", "logging");
const text = (value) => ({ node: "string", value });

const findTarget = async (plugin, name) => {
  const discovery = await plugin.call("discover", { root: targetRoot });
  const found = discovery.targets.find((candidate) => candidate.name === name);
  assert.ok(found, `discovery lost ${name}: ${discovery.targets.map((candidate) => candidate.name).join(", ")}`);
  return found;
};

test("a target that logs reaches the diagnostic stream, not the protocol channel", async () => {
  const plugin = startPlugin();
  try {
    const noise = await findTarget(plugin, "logNoise");
    const outcome = await plugin.call("invoke", {
      invocationId: 1,
      targetId: noise.id,
      values: [text("hello")],
      deadlineMs: 2000,
      collectCoverage: false,
    });
    assert.equal(outcome.kind, "ok", `the logging call did not answer normally: ${JSON.stringify(outcome)}`);
    assert.match(plugin.diagnostics(), /noise hello/, "the target's log line never reached stderr");
  } finally {
    plugin.close();
  }
});

test("a write to a stdout nobody is reading leaves the plugin alive", async () => {
  const child = spawn(process.execPath, ["dist/server.js"], { cwd: packageRoot, stdio: ["pipe", "pipe", "pipe"] });
  const exited = new Promise((resolve) => child.once("exit", (code, signal) => resolve({ code, signal })));
  child.stderr.resume();
  const lines = createInterface({ input: child.stdout });
  const waiting = new Map();
  let next = 1;
  lines.on("line", (line) => {
    const message = JSON.parse(line);
    const settle = waiting.get(message.id);
    if (settle === undefined) return;
    waiting.delete(message.id);
    settle(message.result);
  });
  const call = (method, params) =>
    new Promise((resolve) => {
      const id = next++;
      waiting.set(id, resolve);
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });

  await call("handshake", {});
  const discovery = await call("discover", { root: targetRoot });
  const writer = discovery.targets.find((candidate) => candidate.name === "writeStdout");
  assert.ok(writer, "discovery lost writeStdout");

  // The reader goes away first, so the plugin's next write to the channel fails.
  child.stdout.destroy();
  child.stdin.write(
    `${JSON.stringify({
      jsonrpc: "2.0",
      id: next++,
      method: "invoke",
      params: { invocationId: 1, targetId: writer.id, values: [text("gone")], deadlineMs: 2000, collectCoverage: false },
    })}\n`,
  );

  const early = await Promise.race([exited, new Promise((resolve) => setTimeout(() => resolve(null), 1500))]);
  assert.equal(early, null, `the plugin ended itself over a failed write: ${JSON.stringify(early)}`);
  child.kill();
});

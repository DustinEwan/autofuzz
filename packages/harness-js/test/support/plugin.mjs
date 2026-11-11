// Starts the built plugin as a real subprocess over real stdio, which is the path the engine
// takes. Tests share this instead of each re-implementing framing, so a framing bug cannot hide
// behind a test-local copy of the client.

import { spawn } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";

export const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");

/** @returns {{call: (method: string, params: unknown, timeoutMs?: number) => Promise<any>, diagnostics: () => string, close: () => void}} */
export function startPlugin() {
  const child = spawn(process.execPath, ["dist/server.js"], {
    cwd: packageRoot,
    stdio: ["pipe", "pipe", "pipe"],
  });
  const pending = new Map();
  let nextId = 1;
  const stderr = [];
  child.stderr.on("data", (chunk) => stderr.push(String(chunk)));
  const lines = createInterface({ input: child.stdout });
  lines.on("line", (line) => {
    if (line.trim() === "") return;
    const message = JSON.parse(line);
    const settle = pending.get(message.id);
    if (!settle) return;
    pending.delete(message.id);
    if (message.error) settle.reject(new Error(`${message.error.code} ${message.error.message}`));
    else settle.resolve(message.result);
  });
  const call = (method, params, timeoutMs = 20_000) =>
    new Promise((resolveCall, rejectCall) => {
      const id = nextId++;
      const timer = setTimeout(() => {
        pending.delete(id);
        rejectCall(new Error(`${method} timed out after ${timeoutMs}ms; stderr: ${stderr.join("")}`));
      }, timeoutMs);
      pending.set(id, {
        resolve: (value) => {
          clearTimeout(timer);
          resolveCall(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          rejectCall(error);
        },
      });
      child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  return {
    call,
    diagnostics: () => stderr.join(""),
    close: () => {
      child.stdin.end();
      child.kill();
    },
  };
}

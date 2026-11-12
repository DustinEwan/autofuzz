#!/usr/bin/env node
// Render the conformance fixture project for the js-ts harness.
//
// `afz validate-harness` and `afz-xtask check-harness` validate a plugin against the template
// (`crates/afz-conformance/template/v1.json`): ten semantic properties stated as wire-level
// expectations. This emitter writes a small TypeScript package that realizes every property
// the language can express, the `autofuzz.json` that points the plugin at it, and
// `afz-conformance-manifest.json` naming the symbol behind each property. The suite, not this
// file, decides pass/fail.
//
// Usage:
//   node packages/harness-js/conformance/emit.mjs --out /tmp/conformance-js
//
// One property is declared not-applicable, and the CI gate asserts that this is the only
// such line for js-ts: TypeScript discovery enumerates exported symbols only, so a
// module-local function never appears in `skipped` to be named at all. Declaring the absence
// is honest; inventing a fake "private" symbol to fake the pass is not.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = dirname(dirname(dirname(here)));
const server = join(repoRoot, "packages", "harness-js", "dist", "server.js");

// The fixture. Every symbol carries a comment naming the template property it realizes, so
// a reader of a rendered tree can trace a FAIL line back to the code that caused it.
const PRICING_TS = `// Rendered by packages/harness-js/conformance/emit.mjs - do not edit by hand.

/** A store handle the harness cannot build: the constructor requires an argument. */
export class Ledger {
  constructor(readonly path: string) {}
}

/** A receiver the harness can construct with no arguments. */
export class Invoice {
  lines: number[] = [];

  /** method-receiver: \`prepare\` must build this receiver and answer ok. */
  addLine(amount: number): number {
    this.lines.push(amount);
    return this.lines.length;
  }
}

/**
 * crash-unguarded, coverage-digest-live, reproducer-script.
 *
 * Two branches the coverage inputs take separately; the property read on the out-of-range
 * element is the unguarded crash the suite drives with quantity 5.
 */
export function tieredPrice(quantity: number): number {
  const tiers = quantity < 2 ? [10, 20] : [30, 40, 50];
  const tier = tiers[quantity];
  return (tier as any).amount;
}

/** guard-refusal: the out-of-range input throws RangeError on a guard line. */
export function applyDiscount(amount: number): number {
  if (amount < 0 || amount > 100) {
    throw new RangeError("amount out of range");
  }
  return amount * 0.9;
}

/**
 * unrepresentable-symbol: an ambient declaration has no implementation the plugin can call,
 * so discovery reports it as \`unsupported-signature\` rather than listing it as fuzzable.
 * (A module-local function would be the python-shaped realization, but TypeScript discovery
 * never enumerates unexported symbols at all - that property is declared not-applicable in
 * the manifest instead.)
 */
export declare function untyped(x: unknown): unknown;

/** unconstructible-receiver: blocked by Ledger, whose constructor needs an argument. */
export function post(ledger: Ledger): string {
  return ledger.path;
}

/** stdout-purity: writes to stdout first; the protocol channel must survive it. */
export function noisy(message: string): string {
  console.log(message);
  return message;
}

/** deadline-sleep: blocks the event loop far past any deadline the suite passes. */
export function spin(ms: number): string {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
  return "done";
}
`;

const autofuzz = {
  project: "conformance-js",
  codebase: {
    harness: "js-ts",
    // Absolute: the suite spawns the plugin with its own cwd, not the rendered project's.
    command: ["node", server],
    include: ["src/**/*.ts"],
    exclude: ["**/node_modules/**", "**/dist/**", "**/*.d.ts"],
  },
  oracles: {},
  budget: { invocations: 2000, wallClockMs: 60000, workers: 0, maxCorpus: 256, strategy: "guided" },
  output: { dir: ".autofuzz", renderers: ["json"] },
};

const value = (node) => ({ ...node, provenance: "user" });
const number = (n) => value({ node: "number", value: n });
const string = (s) => value({ node: "string", value: s });

const manifest = {
  templateVersion: 1,
  properties: {
    "crash-unguarded": { symbol: "tieredPrice", input: [number(5)] },
    "guard-refusal": { symbol: "applyDiscount", input: [number(-5)] },
    "private-symbol": {
      notApplicable:
        "TypeScript discovery enumerates exported symbols only, so a module-local function is never enumerated and no skipped entry can name it; visibility here is an export decision, not a fuzzability skip",
    },
    "unrepresentable-symbol": { symbol: "untyped" },
    "unconstructible-receiver": { symbol: "post", blockedByType: "Ledger" },
    "method-receiver": { symbol: "Invoice.addLine", input: [number(1)] },
    // 0 takes the first branch and reads clean; 2 takes the second. Same function, different
    // edges: a dead or never-reset probe answers identical digests and fails.
    "coverage-digest-live": { symbol: "tieredPrice", inputs: [[number(0)], [number(2)]] },
    "stdout-purity": { symbol: "noisy", input: [string("conformance noise")] },
    "reproducer-script": { symbol: "tieredPrice", input: [number(5)] },
    // 30 000 ms against the suite's 1000 ms default: only a real cut answers inside it.
    "deadline-sleep": { symbol: "spin", input: [number(30000)] },
  },
};

function emit(out) {
  mkdirSync(join(out, "src"), { recursive: true });
  writeFileSync(join(out, "src", "pricing.ts"), PRICING_TS);
  writeFileSync(join(out, "autofuzz.json"), JSON.stringify(autofuzz, null, 2) + "\n");
  writeFileSync(join(out, "afz-conformance-manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
}

const flag = process.argv.indexOf("--out");
if (flag === -1 || process.argv[flag + 1] === undefined) {
  console.error("usage: emit.mjs --out DIR");
  process.exit(2);
}
emit(process.argv[flag + 1]);
console.log(process.argv[flag + 1]);

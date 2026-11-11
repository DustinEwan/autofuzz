// The two sides of the plugin boundary are written in different languages and compiled
// separately. This file is where they are checked against each other: every literal the plugin
// sends or reads is compared with the JSON Schema that `cargo run -p afz-xtask -- schema`
// generates from the Rust types and commits under `docs/schema/`, and the method vocabulary is
// compared with the plugin interface in `docs/architecture.md`.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import * as wire from "../dist/protocol.js";

const packageRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const schemaDir = join(packageRoot, "..", "..", "docs", "schema");
const design = join(packageRoot, "..", "..", "docs", "architecture.md");

function schema(name) {
  return JSON.parse(readFileSync(join(schemaDir, name), "utf8"));
}

function sorted(values) {
  return [...values].sort();
}

/** The literal members of a union, sorted: schemars emits `enum` or `oneOf` of bare consts. */
function literals(spec, what) {
  if (Array.isArray(spec?.enum)) return sorted(spec.enum);
  const members = spec?.oneOf;
  assert.ok(Array.isArray(members), `${what} is neither an enum nor a oneOf`);
  return sorted(
    members.map((variant) => {
      assert.equal(typeof variant.const, "string", `${what} member carries no const string`);
      return variant.const;
    }),
  );
}

/** The tag values of a tagged union, sorted, read from the discriminator property. */
function tags(document, field, what) {
  return sorted(
    document.oneOf.map((variant) => {
      const value = variant.properties?.[field]?.const;
      assert.equal(typeof value, "string", `${what} variant has no "${field}" const`);
      return value;
    }),
  );
}

test("type node kinds match the generated schema", () => {
  assert.deepEqual(sorted(wire.TYPE_NODE_KINDS), tags(schema("type-node.json"), "kind", "TypeNode"));
});

test("value node tags match the generated schema", () => {
  assert.deepEqual(sorted(wire.VALUE_NODE_TAGS), tags(schema("value-tree.json"), "node", "ValueNode"));
});

test("skip reasons match the generated schema", () => {
  assert.deepEqual(
    sorted(wire.SKIP_REASONS),
    literals(schema("skipped-target.json").$defs.SkipReason, "SkipReason"),
  );
});

test("outcome kinds and required fields match the generated schema", () => {
  const outcome = schema("outcome.json");
  assert.deepEqual(sorted(wire.OUTCOME_KINDS), tags(outcome, "kind", "Outcome"));
  // The engine reads `via` on both failure channels, and only `failed` carries a signature.
  assert.deepEqual(outcome.$defs.RejectedOutcome.required, ["durationMs", "via", "detail"]);
  assert.deepEqual(outcome.$defs.FailedOutcome.required, ["durationMs", "via", "fault"]);
  assert.deepEqual(outcome.$defs.FaultSignature.required, ["class", "message"]);
  assert.deepEqual(Object.keys(outcome.$defs.FaultFrame.properties).sort(), [
    "column",
    "file",
    "fnName",
    "line",
  ]);
});

test("the number names the wire carries are the serde spellings", () => {
  // `naN` and friends come from serde's camelCase rule over the Rust variants. They read like
  // typos, so the check that fails when someone "fixes" one side is here.
  assert.deepEqual(sorted(wire.SPECIAL_NUMBERS), ["infinity", "naN", "negativeInfinity"]);
  assert.deepEqual(
    literals(schema("value-tree.json").$defs.SpecialNumber, "SpecialNumber"),
    sorted(wire.SPECIAL_NUMBERS),
  );
});

test("prepare reports ok with a handle, or a bare skip reason", () => {
  const prepare = schema("prepare-resp.json");
  const [ok, skip] = prepare.oneOf;
  assert.deepEqual(ok.required, ["ok", "materialized"]);
  assert.deepEqual(skip.required, ["ok", "reason"]);
  assert.deepEqual(Object.keys(prepare.$defs.MaterializedCall.properties).sort(), [
    "handle",
    "summary",
  ]);
});

test("capabilities, materializations, and sub-kinds match the generated schema", () => {
  const handshake = schema("handshake.json");
  assert.deepEqual(sorted(wire.CAPABILITY_NAMES), literals(handshake.$defs.Capability, "Capability"));

  const descriptor = schema("target-descriptor.json");
  assert.deepEqual(
    sorted(wire.MATERIALIZATIONS),
    literals(descriptor.$defs.Materialization, "Materialization"),
  );
  assert.deepEqual(Object.keys(descriptor.$defs.Effects.properties).sort(), [
    "async",
    "callbacks",
    "mutatesState",
    "streams",
    "throws",
  ]);

  const typeNode = schema("type-node.json");
  assert.deepEqual(sorted(wire.SCALAR_KINDS), literals(typeNode.$defs.ScalarKind, "ScalarKind"));
  assert.deepEqual(sorted(wire.COLLECTION_KINDS), literals(typeNode.$defs.CollectionKind, "CollectionKind"));
  assert.deepEqual(sorted(wire.COMPOSITE_KINDS), literals(typeNode.$defs.CompositeKind, "CompositeKind"));
  assert.deepEqual(
    sorted(wire.FUNCTION_BEHAVIORS),
    literals(typeNode.$defs.FunctionBehavior, "FunctionBehavior"),
  );
});

test("the plugin answers exactly the methods the design declares", () => {
  const declared = [...readFileSync(design, "utf8").matchAll(/^ {2}([A-Za-z]+)\??\(/gm)].map(
    (match) => match[1],
  );
  assert.ok(declared.length >= wire.METHOD_NAMES.length, "no plugin interface found in the design");
  assert.deepEqual(sorted(wire.METHOD_NAMES), sorted(declared));
});

test("framing returns complete lines and keeps the partial tail", () => {
  const framer = new wire.LineFramer();
  assert.deepEqual(framer.push('{"id":1}\n{"id":2'), ['{"id":1}']);
  assert.deepEqual(framer.push('}\n{"id":3}\n'), ['{"id":2}', '{"id":3}']);
  assert.deepEqual(framer.push('{"id":4'), []);
  assert.deepEqual(framer.push('}\n'), ['{"id":4}']);
});

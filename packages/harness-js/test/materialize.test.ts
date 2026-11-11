// ValueTree -> live value graph. Run with `npm test` (which builds first) or directly:
//   node --test test/materialize.test.ts

import test from "node:test";
import assert from "node:assert/strict";

import {
  MalformedValueError,
  MaterializationError,
  StubCallError,
  capturedCallsOf,
  materializeValue,
  materializeValues,
} from "../dist/materialize.js";

function ok(tree, options) {
  const result = materializeValue(tree, options);
  if (!result.ok) assert.fail(`expected materialization, got ${JSON.stringify(result)}`);
  return result.value;
}

function blocked(tree, options) {
  const result = materializeValue(tree, options);
  assert.equal(result.ok, false, "expected a blocked result");
  return result;
}

test("scalars become the primitives the target expects", () => {
  assert.equal(ok({ node: "null" }), null);
  assert.equal(ok({ node: "undefined" }), undefined);
  assert.equal(ok({ node: "boolean", value: false }), false);
  assert.equal(ok({ node: "string", value: "" }), "");
  assert.equal(ok({ node: "number", value: -0 }), -0);
  assert.equal(ok({ node: "number", value: 1.5 }), 1.5);
  assert.equal(ok({ node: "bigInt", value: "-9223372036854775809" }), -9223372036854775809n);
});

test("non-finite numbers arrive as the names the wire carries", () => {
  assert.ok(Number.isNaN(ok({ node: "number", value: "naN" })));
  assert.equal(ok({ node: "number", value: "infinity" }), Number.POSITIVE_INFINITY);
  assert.equal(ok({ node: "number", value: "negativeInfinity" }), Number.NEGATIVE_INFINITY);
  assert.ok(Number.isNaN(ok({ node: "number", value: "NaN" })));
  assert.equal(ok({ node: "number", value: "-Infinity" }), Number.NEGATIVE_INFINITY);
  assert.throws(() => materializeValue({ node: "number", value: 1n }), MalformedValueError);
});

test("literal nodes deep-copy their JSON", () => {
  const value = ok({ node: "literal", value: { a: [1, { b: null }], c: true } });
  assert.deepEqual(value, { a: [1, { b: null }], c: true });
  assert.equal(Object.getPrototypeOf(value), Object.prototype);
});

test("collections become Map, Set and Array", () => {
  const list = ok({ node: "array", items: [{ node: "number", value: 1 }, { node: "undefined" }] });
  assert.deepEqual(list, [1, undefined]);
  assert.ok(Array.isArray(list));

  const tuple = ok({ node: "tuple", items: [{ node: "string", value: "a" }] });
  assert.deepEqual(tuple, ["a"]);

  const set = ok({
    node: "set",
    items: [{ node: "string", value: "x" }, { node: "string", value: "x" }],
  });
  assert.ok(set instanceof Set);
  assert.equal(set.size, 1);

  const key = { node: "object", fields: [["id", { node: "number", value: 7 }]] };
  const map = ok({
    node: "map",
    entries: [[key, { node: "string", value: "v" }], [{ node: "number", value: 1 }, { node: "null" }]],
  });
  assert.ok(map instanceof Map);
  assert.equal(map.size, 2);
  assert.equal(map.get(1), null);
  const [materializedKey] = [...map.keys()];
  assert.deepEqual(materializedKey, { id: 7 });
});

test("object fields are own properties, even when named like the prototype", () => {
  const value = ok({
    node: "object",
    fields: [
      ["__proto__", { node: "string", value: "polluted" }],
      ["constructor", { node: "number", value: 1 }],
      ["keep", { node: "boolean", value: true }],
    ],
  });
  assert.equal(Object.getPrototypeOf(value), Object.prototype);
  assert.deepEqual(Object.getOwnPropertyDescriptor(value, "__proto__"), {
    value: "polluted",
    writable: true,
    enumerable: true,
    configurable: true,
  });
  assert.deepEqual(Object.keys(value), ["__proto__", "constructor", "keep"]);

  const dupes = ok({
    node: "object",
    fields: [
      ["n", { node: "number", value: 1 }],
      ["n", { node: "number", value: 2 }],
    ],
  });
  assert.deepEqual(dupes, { n: 2 });
});

test("buffers materialize as the configured byte container", () => {
  const bytes = ok({ node: "buffer", bytes: [0, 255, 8] });
  assert.ok(bytes instanceof Uint8Array);
  assert.deepEqual([...bytes], [0, 255, 8]);

  const raw = ok({ node: "buffer", bytes: [1, 2] }, { bufferKind: "arrayBuffer" });
  assert.ok(raw instanceof ArrayBuffer);
  assert.equal(raw.byteLength, 2);

  const node = ok({ node: "buffer", bytes: [1, 2] }, { bufferKind: "nodeBuffer" });
  assert.equal(Buffer.isBuffer(node), true);

  assert.throws(() => materializeValue({ node: "buffer", bytes: [300] }), MalformedValueError);
  assert.throws(() => materializeValue({ node: "buffer", bytes: [-1] }), MalformedValueError);
  assert.throws(() => materializeValue({ node: "buffer", bytes: [1.5] }), MalformedValueError);
});

test("function stubs honor their behavior", () => {
  const first = ok({ node: "function", behavior: "returnFirst", returns: { node: "number", value: 4 } });
  assert.equal(typeof first, "function");
  assert.equal(first(1, 2, 3), 4);
  assert.equal(capturedCallsOf(first), undefined);

  const capture = ok({ node: "function", behavior: "captureArguments", returns: { node: "string", value: "r" } });
  assert.equal(capture("a"), "r");
  capture("b", "c");
  const calls = capturedCallsOf(capture);
  assert.deepEqual(calls.map((call) => call.args), [["a"], ["b", "c"]]);
  assert.deepEqual(Object.keys(capture), []);

  const thrower = ok({ node: "function", behavior: "throw" });
  assert.throws(() => thrower(1, 2), StubCallError);

  const bare = ok({ node: "function" });
  assert.equal(bare(), undefined);
  const defaulted = ok({ node: "function" }, { functionBehavior: "throw" });
  assert.throws(() => defaulted(), StubCallError);

  const Constructible = ok({ node: "function", returns: { node: "number", value: 1 } });
  assert.equal(typeof new Constructible(), "object");
});

test("streams are iterable, and re-iterable", async () => {
  const items = [{ node: "number", value: 1 }, { node: "number", value: 2 }];
  const sync = ok({ node: "stream", iterable: true, items });
  assert.deepEqual([...sync], [1, 2]);
  assert.deepEqual([...sync], [1, 2], "a second pass must see the same items");
  const asyncStream = ok({ node: "stream", iterable: false, items });
  const seen = [];
  for await (const item of asyncStream) seen.push(item);
  assert.deepEqual(seen, [1, 2]);
});

test("opaque nodes report the blocking type and where the call needed it", () => {
  const wire = blocked({ node: "map", entries: [[{ node: "string", value: "k" }, { node: "opaque", ty: "Db" }]] });
  assert.equal(wire.reason, "unconstructible-parameter");
  assert.deepEqual(wire.blockedBy, [{ type: "Db", path: "value[0:value]" }]);

  const mirrored = blocked({
    node: "object",
    fields: [["repo", { node: "opaque", ty: { kind: "opaque", ty: "Db", module: "src/db.ts" } }]],
  });
  assert.deepEqual(mirrored.blockedBy, [{ type: "Db", path: "value.repo", module: "src/db.ts" }]);
});

test("every argument is attempted, so one report names every blocker", () => {
  const result = materializeValues([
    { node: "object", fields: [["db", { node: "opaque", ty: "Db" }]] },
    { node: "string", value: "fine" },
    { node: "opaque", ty: "Socket" },
    { node: "opaque", ty: "Db" },
  ]);
  assert.equal(result.ok, false);
  assert.deepEqual(result.blockedBy, [
    { type: "Db", path: "arguments[0].db" },
    { type: "Socket", path: "arguments[2]" },
  ]);
});

test("a graph past the budget is reported, not built", () => {
  const leaf = { node: "number", value: 1 };
  function nest(depth) {
    let node = leaf;
    for (let index = 0; index < depth; index++) node = { node: "array", items: [node] };
    return node;
  }

  assert.equal(materializeValue(nest(40)).ok, true, "the default depth budget is generous");

  const tooDeep = blocked(nest(200), { maxDepth: 12 });
  assert.equal(tooDeep.reason, "unconstructible-parameter");
  assert.match(tooDeep.detail, /materialization depth budget \(12\) exceeded at value\[0\]/);
  assert.equal(materializeValue(nest(200)).ok, false, "the default budget stops runaway graphs");

  let wide = [];
  for (let index = 0; index < 50; index++) wide.push({ node: "number", value: index });
  for (let index = 0; index < 3; index++) {
    wide = Array.from({ length: 5 }, () => ({ node: "array", items: wide }));
  }
  const capped = blocked({ node: "array", items: wide }, { maxNodes: 20 });
  assert.match(capped.detail, /materialization node budget \(20\) exceeded/);
});

test("a payload that is not a ValueTree throws instead of skipping", () => {
  assert.throws(() => materializeValue({ node: "spanish" }), MalformedValueError);
  assert.throws(() => materializeValue({ node: "array" }), MalformedValueError);
  assert.throws(() => materializeValue({ node: "object", fields: [["a"]] }), MalformedValueError);
  assert.throws(() => materializeValue({ node: "map", entries: [["a"]] }), MalformedValueError);
  assert.throws(() => materializeValue({ node: "bigInt", value: "1.5" }), MalformedValueError);
  assert.throws(() => materializeValue({ node: "function", behavior: "explode" }), MalformedValueError);
  assert.throws(() => materializeValue({ node: "stream", items: [] }), MalformedValueError);
  assert.throws(() => materializeValue("nope"), MalformedValueError);
  assert.throws(() => materializeValue({ node: "number", value: 1n }), MalformedValueError);
  assert.throws(() => materializeValues([{ node: "array" }]), MalformedValueError);
});

test("MaterializationError carries what invoke reports", () => {
  const refusal = materializeValues([{ node: "opaque", ty: "Db" }]);
  assert.equal(refusal.ok, false);
  const fromGraph = new MaterializationError(refusal);
  assert.equal(fromGraph.name, "MaterializationError");
  assert.equal(fromGraph.reason, "unconstructible-parameter");
  assert.deepEqual(fromGraph.blockedBy, [{ type: "Db", path: "arguments[0]" }]);
  assert.match(fromGraph.message, /Db at arguments\[0\]/);

  const fromMessage = new MaterializationError('export "convert" is not callable');
  assert.equal(fromMessage.message, 'export "convert" is not callable');
  assert.deepEqual(fromMessage.blockedBy, []);
  assert.equal(fromMessage.reason, "unconstructible-parameter");
});

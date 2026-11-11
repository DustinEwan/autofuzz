// Lowers a TypeScript `Type` from the checker API into the wire `TypeNode` vocabulary.
//
// The rules are the Step 6 contract: `Promise<T>` becomes a `promise` node (discovery
// unwraps it into `effects.async` plus the inner type for return positions), string-literal
// unions collapse to `literal`, `Map`/`Set`/`Array` map to `collection`, the byte-ish types
// map to `buffer`, structural expansion is capped at `MAX_TYPE_DEPTH`, recursion and the
// depth cap emit `ref`, and anything the materializer cannot build emits `opaque` together
// with the materialization floor (`fixture-required` for a user class instance, `opaque`
// for a host or DOM object) so discovery can fill `TargetDescriptor.materialization`.
//
// One `TypeLowerer` per target: the name registry must never carry a name from one
// target's signature into another's.

import ts from "typescript";

import type { Blocker, Field, Materialization, TypeNode } from "./protocol.js";

/** Structural expansion cap from the plan; deeper types emit `ref` or `opaque`. */
export const MAX_TYPE_DEPTH = 12;

/** One lowered type: the wire node plus the honest reachability accounting. */
export interface LoweredType {
  type: TypeNode;
  /** Worst floor over the whole lowered graph. */
  materialization: Materialization;
  /** Every opaque/fixture-required node, with the path that reaches it. */
  blockers: Blocker[];
}

const WORST: Record<Materialization, number> = {
  constructible: 0,
  "fixture-required": 1,
  opaque: 2,
};

const SCALAR = (scalar: "null" | "undefined" | "boolean" | "number" | "bigInt" | "string"): TypeNode => ({
  kind: "scalar",
  scalar,
});

/** The honest lowering of `any`/`unknown`: one of the four JSON-ish scalars. */
const UNKNOWN_UNION: TypeNode = {
  kind: "composite",
  composite: "union",
  members: [SCALAR("null"), SCALAR("boolean"), SCALAR("number"), SCALAR("string")],
};

/**
 * Unwrap `Promise<T>` (or any thenable the checker resolves) to `T`, or undefined.
 * Shared with discovery, which turns a promise return into `effects.async` plus the
 * inner type instead of a `promise` node.
 */
export function promisedTypeOf(checker: ts.TypeChecker, type: ts.Type): ts.Type | undefined {
  // `getPromisedTypeOfPromise` exists on the TS6 checker at runtime but is not declared
  // in the public d.ts; call it through a checked boundary.
  const candidate: unknown = Reflect.get(checker, "getPromisedTypeOfPromise");
  if (typeof candidate !== "function") return undefined;
  const read = candidate as (this: ts.TypeChecker, t: ts.Type) => ts.Type | undefined;
  const inner = read.call(checker, type);
  if (inner === undefined || inner === type) return undefined;
  return inner;
}

/** Host objects no generated value and no plain fixture can stand in for. */
const HOST_OPAQUE: Record<string, true> = {
  AbortController: true,
  AbortSignal: true,
  Blob: true,
  EventTarget: true,
  File: true,
  WritableStream: true,
  TransformStream: true,
  XMLHttpRequest: true,
};

/** lib.es classes that need a live object the generator cannot synthesize. */
const RUNTIME_OPAQUE: Record<string, true> = {
  Date: true,
  RegExp: true,
  Error: true,
  TypeError: true,
  RangeError: true,
  WeakMap: true,
  WeakSet: true,
  WeakRef: true,
};

const BUFFER_TYPES: Record<string, true> = {
  ArrayBuffer: true,
  SharedArrayBuffer: true,
  Buffer: true,
  DataView: true,
  Float32Array: true,
  Float64Array: true,
  Int8Array: true,
  Int16Array: true,
  Int32Array: true,
  Uint8Array: true,
  Uint8ClampedArray: true,
  Uint16Array: true,
  Uint32Array: true,
};

const ARRAY_NAMES: Record<string, true> = { Array: true, ReadonlyArray: true };
const SET_NAMES: Record<string, true> = { Set: true, ReadonlySet: true, WeakSet: true };
const MAP_NAMES: Record<string, true> = { Map: true, ReadonlyMap: true, WeakMap: true };
const SYNC_STREAM_NAMES: Record<string, true> = {
  Iterable: true,
  IterableIterator: true,
  Generator: true,
};
const ASYNC_STREAM_NAMES: Record<string, true> = {
  AsyncIterable: true,
  AsyncIterableIterator: true,
  AsyncGenerator: true,
  AsyncIterator: true,
  ReadableStream: true,
};

function isFromLib(symbol: ts.Symbol | undefined, fragment: string): boolean {
  for (const decl of symbol?.getDeclarations() ?? []) {
    if (decl.getSourceFile().fileName.includes(fragment)) return true;
  }
  return false;
}

/**
 * Per-`lower()` mutable state: blockers and the materialization floor accumulate into
 * exactly one object over the whole graph. Depth is threaded as a parameter, never
 * copied, so a deeper node's floor can never be lost.
 */
interface Ctx {
  /** Object types currently being lowered, mapped to their `ref` name (recursion guard). */
  active: Map<ts.Type, string>;
  blockers: Blocker[];
  materialization: Materialization;
}

export class TypeLowerer {
  private readonly checker: ts.TypeChecker;
  private readonly maxDepth: number;
  /** Names referenced by a `ref`; their shapes get `id`. */
  private readonly referenced = new Set<string>();
  /** Completed object shapes by name, so a `ref` attaches `id` to the shape. */
  private readonly shapes = new Map<string, TypeNode>();
  /** Claimed names, so two distinct types called `Node` get distinct ref names. */
  private readonly nameOwners = new Map<string, ts.Type>();

  constructor(checker: ts.TypeChecker, maxDepth: number = MAX_TYPE_DEPTH) {
    this.checker = checker;
    this.maxDepth = maxDepth;
  }

  /** Lower one type. `path` seeds blocker paths, e.g. `arguments[0]` or `receiver.db`. */
  lower(type: ts.Type, path: string): LoweredType {
    const ctx: Ctx = { active: new Map(), blockers: [], materialization: "constructible" };
    const node = this.lowerType(type, path, ctx, 0);
    return { type: node, materialization: ctx.materialization, blockers: ctx.blockers };
  }

  private lowerType(type: ts.Type, path: string, ctx: Ctx, depth: number): TypeNode {
    const flags = type.flags;

    if (flags & ts.TypeFlags.Any || flags & ts.TypeFlags.Unknown) return UNKNOWN_UNION;
    if (flags & ts.TypeFlags.String) return SCALAR("string");
    if (flags & ts.TypeFlags.Number) return SCALAR("number");
    if (flags & ts.TypeFlags.BigInt) return SCALAR("bigInt");
    if (flags & ts.TypeFlags.Boolean) return SCALAR("boolean");
    if (flags & ts.TypeFlags.Void) return SCALAR("undefined");
    if (flags & ts.TypeFlags.Null) return SCALAR("null");
    if (flags & ts.TypeFlags.Undefined) return SCALAR("undefined");
    if (flags & ts.TypeFlags.Never) return this.opaque(ctx, path, "never", null, "opaque");
    if (flags & (ts.TypeFlags.UniqueESSymbol | (ts.TypeFlags.ESSymbol ?? 0))) {
      return this.opaque(ctx, path, "symbol", "lib.es5", "opaque");
    }
    if (flags & ts.TypeFlags.TemplateLiteral) return SCALAR("string");
    if (flags & (ts.TypeFlags.Literal | ts.TypeFlags.Enum | ts.TypeFlags.BooleanLiteral)) {
      const literal = this.lowerLiteral(type);
      if (literal !== undefined) return literal;
    }
    if (
      flags &
      (ts.TypeFlags.TypeParameter | ts.TypeFlags.Index | ts.TypeFlags.Conditional | ts.TypeFlags.IndexedAccess)
    ) {
      const constraint = this.checker.getBaseConstraintOfType(type);
      if (constraint !== undefined && constraint !== type) return this.lowerType(constraint, path, ctx, depth);
      return UNKNOWN_UNION;
    }
    // The depth budget is spent here rather than inside `lowerObject`: a union member or an
    // array element recurses back through this function, and a check that sits below the
    // object's collection branches is never reached by a type such as `type Json = Json[] | string`.
    if (depth >= this.maxDepth) return this.capped(type, path, ctx);
    if (flags & ts.TypeFlags.Intersection) return this.lowerIntersection(type as ts.IntersectionType, path, ctx, depth);
    if (flags & ts.TypeFlags.Union) return this.lowerUnion(type as ts.UnionType, path, ctx, depth);
    if (flags & ts.TypeFlags.Object) return this.lowerObject(type, path, ctx, depth);

    return this.opaque(ctx, path, this.checker.typeToString(type), null, "opaque");
  }

  // ------------------------------------------------------------------ literals

  private lowerLiteral(type: ts.Type): TypeNode | undefined {
    if (type.flags & ts.TypeFlags.StringLiteral) {
      return { kind: "literal", values: [(type as ts.StringLiteralType).value] };
    }
    if (type.flags & ts.TypeFlags.NumberLiteral) {
      const t = type as ts.NumberLiteralType;
      let value = t.value;
      // The compiler marks `-1` literals with an internal `negative` bit.
      const negative: unknown = Reflect.get(t, "negative");
      if (negative === true) value = -value;
      return { kind: "literal", values: [value] };
    }
    if (type.flags & ts.TypeFlags.BigIntLiteral) {
      const t = type as ts.BigIntLiteralType;
      const base10: unknown = Reflect.get(t, "base10Value");
      return { kind: "literal", values: [typeof base10 === "string" ? base10 : String(t.value)] };
    }
    if (type.flags & ts.TypeFlags.BooleanLiteral) {
      return { kind: "literal", values: [this.checker.typeToString(type) === "true"] };
    }
    if (type.flags & (ts.TypeFlags.Enum | ts.TypeFlags.EnumLiteral)) {
      const values = this.enumValues(type);
      return values.length > 0 ? { kind: "literal", values } : undefined;
    }
    return undefined;
  }

  private enumValues(type: ts.Type): (string | number)[] {
    const out: (string | number)[] = [];
    const symbol = type.aliasSymbol ?? type.getSymbol();
    if (symbol === undefined) return out;
    const members: ts.Symbol[] = [];
    for (const decl of symbol.getDeclarations() ?? []) {
      if (ts.isEnumDeclaration(decl)) {
        for (const member of decl.members) {
          const memberSymbol = this.checker.getSymbolAtLocation(member.name);
          if (memberSymbol !== undefined) members.push(memberSymbol);
        }
      }
    }
    // A single enum-member type: the symbol is the member itself.
    if (members.length === 0) members.push(symbol);
    for (const member of members) {
      const decl = member.valueDeclaration;
      const constant = decl !== undefined && ts.isEnumMember(decl) ? this.checker.getConstantValue(decl) : undefined;
      if (typeof constant === "string" || typeof constant === "number") out.push(constant);
      else out.push(String(member.escapedName));
    }
    return out;
  }

  // ------------------------------------------------------------------ unions

  private lowerUnion(type: ts.UnionType, path: string, ctx: Ctx, depth: number): TypeNode {
    const members = type.types;

    const defined = members.filter((m) => !(m.flags & ts.TypeFlags.Undefined));
    if (defined.length === 1 && defined.length !== members.length) {
      // `T | undefined` in a nested position: the optionality belongs to the field, not the type.
      return this.lowerType(defined[0]!, path, ctx, depth);
    }

    const literalSet = this.literalSet(members);
    if (literalSet !== undefined) return { kind: "literal", values: literalSet };

    const lowered = members.map((m) => this.lowerType(m, path, ctx, depth + 1));
    const discriminant = this.findDiscriminant(members);
    return {
      kind: "composite",
      composite: "union",
      ...(discriminant !== undefined ? { discriminant } : {}),
      members: lowered,
    };
  }

  /** All members literal (or null/undefined): collapse to one literal set, else undefined. */
  private literalSet(members: readonly ts.Type[]): (string | number | boolean | null)[] | undefined {
    const values: (string | number | boolean | null)[] = [];
    for (const member of members) {
      if (member.flags & ts.TypeFlags.Null) {
        values.push(null);
        continue;
      }
      if (member.flags & ts.TypeFlags.Undefined) continue;
      const node = this.lowerLiteral(member);
      if (node === undefined || node.kind !== "literal" || node.values.length !== 1) return undefined;
      const value = node.values[0];
      if (value === undefined) return undefined;
      if (typeof value !== "object") values.push(value);
      else if (value === null) values.push(null);
      else return undefined;
    }
    return values.length > 0 ? values : undefined;
  }

  /** A literal-typed property every union member declares: the discriminant. */
  private findDiscriminant(members: readonly ts.Type[]): string | undefined {
    if (members.length < 2) return undefined;
    const [first, ...rest] = members as [ts.Type, ...ts.Type[]];
    const names = this.checker
      .getPropertiesOfType(first)
      .map((p) => p.getName())
      .sort();
    for (const name of names) {
      let candidate = true;
      for (const member of rest) {
        const prop = this.checker.getPropertiesOfType(member).find((p) => p.getName() === name);
        if (prop === undefined || !this.isLiteralTypedProperty(prop)) {
          candidate = false;
          break;
        }
      }
      if (candidate) return name;
    }
    return undefined;
  }

  private isLiteralTypedProperty(prop: ts.Symbol): boolean {
    const decl = prop.valueDeclaration ?? prop.declarations?.[0];
    if (decl === undefined) return false;
    const type = this.checker.getTypeOfSymbolAtLocation(prop, decl);
    const parts = type.isUnion() ? type.types : [type];
    return parts.every(
      (p) => (p.flags & (ts.TypeFlags.Literal | ts.TypeFlags.BooleanLiteral | ts.TypeFlags.EnumLiteral)) !== 0,
    );
  }

  // ------------------------------------------------------------------ intersections

  private lowerIntersection(type: ts.IntersectionType, path: string, ctx: Ctx, depth: number): TypeNode {
    const lowered = type.types.map((m) => this.lowerType(m, path, ctx, depth + 1));
    if (lowered.every((n) => n.kind === "object")) {
      const fields: Field[] = [];
      const seen = new Set<string>();
      let name: string | undefined;
      for (const node of lowered as Extract<TypeNode, { kind: "object" }>[]) {
        name ??= node.name;
        for (const field of node.fields ?? []) {
          if (seen.has(field.name)) continue;
          seen.add(field.name);
          fields.push(field);
        }
      }
      fields.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
      return { kind: "object", ...(name !== undefined ? { name } : {}), fields };
    }
    return { kind: "composite", composite: "intersection", members: lowered };
  }

  // ------------------------------------------------------------------ objects

  private lowerObject(type: ts.Type, path: string, ctx: Ctx, depth: number): TypeNode {
    const checker = this.checker;

    if (checker.isTupleType(type)) {
      const items = this.typeArguments(type).map((t) => this.lowerType(t, `${path}[]`, ctx, depth + 1));
      return { kind: "tuple", items };
    }

    const symbol = type.getSymbol();
    const name = symbol?.escapedName ?? "";
    const fromLib = isFromLib(symbol, "/lib.");

    if (fromLib) {
      if (BUFFER_TYPES[name] === true) return { kind: "buffer" };
      if (RUNTIME_OPAQUE[name] === true) return this.opaque(ctx, path, name, "lib", "opaque");
      if (HOST_OPAQUE[name] === true) return this.opaque(ctx, path, name, "lib.dom", "opaque");
      if (ARRAY_NAMES[name] === true || checker.isArrayType(type)) {
        return {
          kind: "collection",
          collection: "array",
          type: this.elementType(type, `${path}[]`, ctx, depth),
        };
      }
      if (SET_NAMES[name] === true) {
        return {
          kind: "collection",
          collection: "set",
          type: this.elementType(type, `${path}[]`, ctx, depth),
        };
      }
      if (MAP_NAMES[name] === true) {
        const args = this.typeArguments(type);
        return {
          kind: "collection",
          collection: "map",
          key: args[0] !== undefined ? this.lowerType(args[0], `${path}{key}`, ctx, depth + 1) : SCALAR("string"),
          type:
            args[1] !== undefined ? this.lowerType(args[1], `${path}{value}`, ctx, depth + 1) : UNKNOWN_UNION,
        };
      }
      if (SYNC_STREAM_NAMES[name] === true) return this.stream(type, `${path}[]`, ctx, depth, true);
      if (ASYNC_STREAM_NAMES[name] === true) return this.stream(type, `${path}[]`, ctx, depth, false);
      const promised = promisedTypeOf(checker, type);
      if (promised !== undefined) {
        return { kind: "promise", type: this.lowerType(promised, path, ctx, depth + 1) };
      }
    }

    const callSignatures = type.getCallSignatures();
    if (callSignatures.length > 0 && type.getConstructSignatures().length === 0) {
      const signature = callSignatures[0]!;
      const returns = signature.getReturnType();
      const isVoid = (returns.flags & ts.TypeFlags.Void) !== 0;
      return {
        kind: "function",
        ...(isVoid ? {} : { type: this.lowerType(returns, `${path}()`, ctx, depth + 1) }),
      };
    }
    if (callSignatures.length === 0 && type.getConstructSignatures().length > 0) {
      return this.opaque(ctx, path, `typeof ${checker.typeToString(type)}`, this.moduleOf(symbol), "opaque");
    }

    const objectFlags = (type as ts.ObjectType).objectFlags ?? 0;
    if (objectFlags & ts.ObjectFlags.Class) {
      // A user class instance: only a fixture (or the receiver builder) can supply one.
      return this.opaque(ctx, path, checker.typeToString(type), this.moduleOf(symbol), "fixture-required");
    }

    const refName = this.refName(type, symbol);
    if (refName !== undefined) {
      const activeName = ctx.active.get(type);
      if (activeName !== undefined) return this.ref(activeName);
    }

    // Index-signature-only shapes (e.g. `Record<string, T>`) are maps, not field sets.
    const properties = checker.getPropertiesOfType(type);
    const stringIndex = type.getStringIndexType();
    if (properties.length === 0 && stringIndex !== undefined) {
      return {
        kind: "collection",
        collection: "map",
        key: SCALAR("string"),
        type: this.lowerType(stringIndex, `${path}{value}`, ctx, depth + 1),
      };
    }

    const shapeName = refName;
    if (shapeName !== undefined) ctx.active.set(type, shapeName);

    const fields: Field[] = [];
    for (const property of properties) {
      const declaration = property.valueDeclaration ?? property.declarations?.[0];
      if (declaration === undefined) continue;
      let propertyType = checker.getTypeOfSymbolAtLocation(property, declaration);
      const optional = (property.flags & ts.SymbolFlags.Optional) !== 0;
      if (optional && propertyType.isUnion()) {
        const defined = propertyType.types.filter((t) => !(t.flags & ts.TypeFlags.Undefined));
        if (defined.length === 1) propertyType = defined[0]!;
      }
      fields.push({
        name: property.getName(),
        type: this.lowerType(propertyType, `${path}.${property.getName()}`, ctx, depth + 1),
        required: !optional,
      });
    }

    const node: TypeNode = {
      kind: "object",
      ...(shapeName !== undefined ? { name: shapeName } : {}),
      fields,
    };
    if (shapeName !== undefined) {
      ctx.active.delete(type);
      this.registerShape(shapeName, type, node);
    }
    return node;
  }

  private elementType(type: ts.Type, path: string, ctx: Ctx, depth: number): TypeNode {
    const element = this.typeArguments(type)[0];
    return element !== undefined ? this.lowerType(element, path, ctx, depth + 1) : UNKNOWN_UNION;
  }

  private stream(type: ts.Type, path: string, ctx: Ctx, depth: number, iterable: boolean): TypeNode {
    return { kind: "stream", iterable, type: this.elementType(type, path, ctx, depth) };
  }

  private typeArguments(type: ts.Type): readonly ts.Type[] {
    const objectFlags = (type as ts.ObjectType).objectFlags;
    if (objectFlags === undefined || !(objectFlags & ts.ObjectFlags.Reference)) {
      return type.aliasTypeArguments ?? [];
    }
    try {
      return this.checker.getTypeArguments(type as ts.TypeReference);
    } catch {
      return type.aliasTypeArguments ?? [];
    }
  }

  // ------------------------------------------------------------------ refs and opaque

  /**
   * What a type becomes when the depth budget is spent: a named `ref` when the type has a name
   * to point at, and an `opaque` node naming the budget otherwise.
   */
  private capped(type: ts.Type, path: string, ctx: Ctx): TypeNode {
    const symbol = type.getSymbol() ?? type.aliasSymbol;
    const name = this.refName(type, symbol);
    if (name !== undefined) return this.ref(name);
    return this.opaque(
      ctx,
      path,
      `depth-budget-exceeded:${this.checker.typeToString(type)}`,
      this.moduleOf(symbol),
      "opaque",
    );
  }

  /** A stable name for a named object/interface/alias type, claimed once per type. */
  private refName(type: ts.Type, symbol: ts.Symbol | undefined): string | undefined {
    const base = type.aliasSymbol?.getName() ?? symbol?.getName();
    if (base === undefined || base === "" || base === "__type" || base === "__object") return undefined;
    const claimed = this.nameOwners.get(base);
    if (claimed === undefined) {
      this.nameOwners.set(base, type);
      return base;
    }
    if (claimed === type) return base;
    for (let n = 2; ; n += 1) {
      const alt = `${base}$${n}`;
      const altOwner = this.nameOwners.get(alt);
      if (altOwner === undefined) {
        this.nameOwners.set(alt, type);
        return alt;
      }
      if (altOwner === type) return alt;
    }
  }

  private ref(name: string): TypeNode {
    this.referenced.add(name);
    const shape = this.shapes.get(name);
    if (shape !== undefined && shape.kind === "object" && shape.id === undefined) shape.id = name;
    return { kind: "ref", name };
  }

  private registerShape(name: string, type: ts.Type, node: TypeNode): void {
    if (this.shapes.has(name) && this.nameOwners.get(name) !== type) return;
    this.shapes.set(name, node);
    if (this.referenced.has(name) && node.kind === "object" && node.id === undefined) node.id = name;
  }

  private opaque(ctx: Ctx, path: string, ty: string, module: string | null, floor: Materialization): TypeNode {
    ctx.blockers.push({ type: ty, path, ...(module !== null ? { module } : {}) });
    if (WORST[floor] > WORST[ctx.materialization]) ctx.materialization = floor;
    return { kind: "opaque", ty, ...(module !== null ? { module } : {}) };
  }

  private moduleOf(symbol: ts.Symbol | undefined): string | null {
    const decl = symbol?.getDeclarations()?.[0];
    if (decl === undefined) return null;
    return decl.getSourceFile().fileName;
  }
}

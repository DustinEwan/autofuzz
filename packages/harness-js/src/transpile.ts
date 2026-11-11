// The harness compiles the target itself.
//
// Every `.ts` file in a target's import closure is read once per plugin process, transpiled to
// ESM with the same `typescript` the plugin already loads for discovery, and cached. The invoke
// path therefore needs no loader: no `--experimental-strip-types`, no `tsx` retry, and no Node
// module hook. The worker gets text, not paths it has to learn to compile.
//
// Compiling means the text V8 runs is not the source, so every coordinate V8 reports — a stack
// frame's line and column, a coverage range's byte offsets — is in emitted coordinates. Both
// are translated back here, through the source map the compiler produced, before anything the
// engine sees is named:
//
// - `classify.ts` matches a fault line against the guard lines discovery recorded from the `.ts`
//   source, so an unmapped frame would read a guard throw as an undeclared failure and turn the
//   scan's `rejected` channel off. Padding the emit instead cannot work: TypeScript re-wraps
//   statements, so `if (x) throw new RangeError()` becomes two lines no matter what is inserted.
// - Coverage edge ids are derived from byte offsets, and `coverageMap` names the source range
//   behind an id. Offsets are mapped so an id means the same range of the same file a reader has
//   open, and stays stable when the compiler's output changes.

import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { dirname, extname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import ts from "typescript";
import { decodeMappings, lineStarts, type SourceMap } from "./sourcemap.js";

/** One module of a realm graph: the text to run, named by the source it came from. */
export interface RealmModule {
  /** The real `file://` URL of the `.ts` source. This is the realm module's identifier. */
  url: string;
  /** Transpiled ESM text. Its coordinates are emitted, not source; `map` translates them. */
  code: string;
  map: SourceMap;
}

/** The transpiled closure of one target module, ready to post to a worker. */
export interface ModuleGraph {
  /** Stable identity of this closure, used to skip re-priming a worker that already has it. */
  key: string;
  entryUrl: string;
  modules: RealmModule[];
}

/** A source file that could not be read. */
export class TranspileError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "TranspileError";
  }
}

const JAVASCRIPT_EXTENSION: Record<string, true> = {
  ".js": true,
  ".jsx": true,
  ".mjs": true,
  ".cjs": true,
};
const TYPESCRIPT_EXTENSION: Record<string, true> = {
  ".ts": true,
  ".tsx": true,
  ".mts": true,
  ".cts": true,
};
const LOADABLE_EXTENSION: Record<string, true> = {
  ".js": true,
  ".jsx": true,
  ".mjs": true,
  ".cjs": true,
  ".ts": true,
  ".tsx": true,
  ".mts": true,
  ".cts": true,
  ".json": true,
};

interface RawSourceMap {
  mappings?: string;
}

/** Compile one source file to ESM text plus the map that locates it in the source. */
export function transpileModuleText(path: string, text: string): RealmModule {
  const extension = extname(path).toLowerCase();
  const output = ts.transpileModule(text, {
    fileName: path,
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ESNext,
      jsx: extension === ".tsx" || extension === ".jsx" ? ts.JsxEmit.ReactJSX : ts.JsxEmit.Preserve,
      sourceMap: true,
      inlineSources: false,
      useDefineForClassFields: true,
      experimentalDecorators: true,
      importHelpers: false,
    },
    reportDiagnostics: false,
  });
  // The emit ends with a `//# sourceMappingURL=` comment naming a `.map` file that is never
  // written; the map is handed to the worker directly.
  const code = output.outputText.replace(/\n\/\/# sourceMappingURL=[^\n]*$/g, "");
  const lines = decodeMappings(typeof output.sourceMapText === "string" ? (JSON.parse(output.sourceMapText) as RawSourceMap).mappings ?? "" : "");
  return {
    url: pathToFileURL(path).href,
    code,
    map: { lines, genLineStarts: lineStarts(code), srcLineStarts: lineStarts(text), srcLength: text.length },
  };
}

/** Every module specifier one source file can load, including the dynamic `import()` forms. */
function moduleSpecifiers(source: ts.SourceFile): string[] {
  const found = new Set<string>();
  const visit = (node: ts.Node): void => {
    if (ts.isImportDeclaration(node) && ts.isStringLiteral(node.moduleSpecifier)) {
      found.add(node.moduleSpecifier.text);
    } else if (
      ts.isExportDeclaration(node) &&
      node.moduleSpecifier !== undefined &&
      ts.isStringLiteral(node.moduleSpecifier)
    ) {
      found.add(node.moduleSpecifier.text);
    } else if (ts.isExternalModuleReference(node) && ts.isStringLiteral(node.expression)) {
      found.add(node.expression.text);
    } else if (
      ts.isCallExpression(node) &&
      node.expression.kind === ts.SyntaxKind.ImportKeyword &&
      node.arguments.length > 0
    ) {
      const argument = node.arguments[0];
      if (argument !== undefined && ts.isStringLiteral(argument)) found.add(argument.text);
    }
    ts.forEachChild(node, visit);
  };
  ts.forEachChild(source, visit);
  return [...found];
}

/**
 * The file a relative specifier names, following the TypeScript convention that a source writes
 * `./x.js` for the module it keeps as `x.ts`. Returns `null` for a bare specifier, which the
 * realm resolves through Node at link time, and for a specifier that names no file at all.
 */
export function resolveSpecifierPath(specifier: string, fromPath: string): string | null {
  if (!specifier.startsWith("./") && !specifier.startsWith("../") && !specifier.startsWith("/")) return null;
  const absolute = resolve(dirname(fromPath), specifier);
  const extension = extname(absolute).toLowerCase();
  const stem = extension === "" ? absolute : absolute.slice(0, -extension.length);
  const candidates: string[] = [];
  if (extension in JAVASCRIPT_EXTENSION) {
    // `./x.js` next to `x.ts` is the NodeNext convention; the source of truth is the `.ts`.
    candidates.push(`${stem}.ts`, `${stem}.tsx`, `${stem}.mts`, `${stem}.cts`, absolute);
  } else if (extension in TYPESCRIPT_EXTENSION || extension === ".json") {
    candidates.push(absolute);
  } else {
    candidates.push(
      `${absolute}.ts`,
      `${absolute}.tsx`,
      `${absolute}.mts`,
      `${absolute}.cts`,
      `${absolute}.js`,
      `${absolute}.mjs`,
      join(absolute, "index.ts"),
      join(absolute, "index.tsx"),
      join(absolute, "index.js"),
    );
  }
  for (const candidate of candidates) {
    if (extname(candidate).toLowerCase() in LOADABLE_EXTENSION && existsSync(candidate)) return candidate;
  }
  return null;
}

/**
 * Reads, transpiles and caches a target's import closure. One instance lives in the plugin
 * process, so a file is compiled once however many invocations use it and however many targets
 * share the closure.
 */
export class Transpiler {
  private modules = new Map<string, RealmModule>();
  private graphs = new Map<string, ModuleGraph>();

  /** The transpiled closure of the module at `entryPath`, keyed by that path. */
  async graph(entryPath: string): Promise<ModuleGraph> {
    const existing = this.graphs.get(entryPath);
    if (existing !== undefined) return existing;
    const collected: RealmModule[] = [];
    const queue = [entryPath];
    const seen = new Set<string>();
    while (queue.length > 0) {
      const path = queue.shift();
      if (path === undefined || seen.has(path)) continue;
      seen.add(path);
      const text = await this.read(path);
      collected.push(this.module(path, text));
      const source = ts.createSourceFile(path, text, ts.ScriptTarget.Latest, true, scriptKindOf(path));
      for (const specifier of moduleSpecifiers(source)) {
        const resolved = resolveSpecifierPath(specifier, path);
        if (resolved !== null && !seen.has(resolved)) queue.push(resolved);
      }
    }
    const hash = createHash("sha256");
    for (const module of collected) hash.update(`${module.url}\u0000${module.code.length}\n`);
    const graph: ModuleGraph = {
      key: hash.digest("hex").slice(0, 16),
      entryUrl: pathToFileURL(entryPath).href,
      modules: collected,
    };
    this.graphs.set(entryPath, graph);
    return graph;
  }

  private async read(path: string): Promise<string> {
    try {
      return await readFile(path, "utf8");
    } catch (error) {
      throw new TranspileError("ERR_MODULE_NOT_FOUND", `cannot read ${path}: ${String(error)}`);
    }
  }

  private module(path: string, text: string): RealmModule {
    const cached = this.modules.get(path);
    if (cached !== undefined) return cached;
    const module = transpileModuleText(path, text);
    this.modules.set(path, module);
    return module;
  }
}

function scriptKindOf(path: string): ts.ScriptKind {
  const extension = extname(path).toLowerCase();
  if (extension === ".tsx" || extension === ".jsx") return ts.ScriptKind.TSX;
  if (extension === ".js" || extension === ".mjs" || extension === ".cjs") return ts.ScriptKind.JS;
  return ts.ScriptKind.TS;
}

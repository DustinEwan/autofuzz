// One realm per invocation, inside the warm worker.
//
// The worker builds a fresh `vm.createContext` and a fresh `vm.SourceTextModule` for every module
// of the target's closure on every call, from the text the plugin process transpiled. Module-level
// state therefore starts empty for each invocation, which is what makes one call reproduce one
// finding, while the thread itself stays warm and keeps its inspector session.
//
// Two boundary rules:
//
// - A module's identifier is the real `file://` URL of its `.ts` source, because `classify.ts`
//   matches fault frames against the guard sites discovery recorded for that file. The identifier
//   is a stability contract, not a label. The coordinates that come with it are emitted ones, so
//   `mapRealmCoordinates` translates them back before anything reads them.
// - A specifier the closure does not contain is resolved through Node — `node:` builtins, npm
//   packages, a `.json` file — and handed to the realm as a synthetic module. No Node module hook
//   is registered: the linker is the only loader.

import vm from "node:vm";
import { Console } from "node:console";
import type { ModuleGraph, RealmModule } from "./transpile.js";
import type { SourceMap } from "./sourcemap.js";

/** A evaluated realm, ready to call into. */
export interface Realm {
  namespace: Record<string, unknown>;
  /** Emitted-to-source maps by module URL, for translating frames and coverage ranges. */
  maps: Map<string, SourceMap>;
}

/** A module the realm could not resolve or evaluate. */
export class RealmLoadError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
    this.name = "RealmLoadError";
  }
}

const NOT_FOR_THE_REALM: Record<string, true> = {
  // The realm gets its own global object; a reference to the worker's would let target code reach
  // out of the realm it was just given.
  globalThis: true,
  global: true,
};

/**
 * The globals a target module would have if Node loaded it directly: the primordials, a console,
 * the timers, `process`, `URL`, `Buffer`, the fetch types. Sharing the worker's primordials is
 * deliberate — the arguments the harness materializes come from this realm, and a target that
 * checks `value instanceof Date` or `value instanceof Map` on an argument must not be told it is
 * not one. That is the whole reason the values are built outside the realm: they must survive
 * being inspected by both sides.
 */
function realmGlobals(): Record<string, unknown> {
  const globals: Record<string, unknown> = {};
  const source = globalThis as Record<string, unknown>;
  for (const name of Object.getOwnPropertyNames(source)) {
    if (name in NOT_FOR_THE_REALM) continue;
    try {
      globals[name] = source[name];
    } catch {
      // A global whose getter refuses to run is simply absent from the realm.
    }
  }
  // The plugin's stdout is its JSON-RPC channel, and a target that logs is ordinary: `yaml`'s `debug`
  // is `console.log(...messages)`. A realm console that writes to stderr keeps target output out of the
  // framing, and the engine reads stderr as a diagnostic, so the line is reported instead of lost.
  globals.console = new Console({ stdout: process.stderr, stderr: process.stderr });
  return globals;
}

const JAVASCRIPT_EXTENSION: Record<string, true> = {
  ".js": true,
  ".jsx": true,
  ".mjs": true,
  ".cjs": true,
};
const TYPESCRIPT_EXTENSIONS = [".ts", ".tsx", ".mts", ".cts"];

/**
 * The graph module a specifier names. A source writes `./ledger.js` for the module it keeps as
 * `ledger.ts`, so the extension is swapped before the specifier is called unresolvable.
 */
function graphCandidates(specifier: string, refUrl: string): string[] {
  let url: URL;
  try {
    url = new URL(specifier, refUrl);
  } catch {
    return [];
  }
  if (url.protocol !== "file:") return [];
  const candidates = [url.href];
  const lastDot = url.pathname.lastIndexOf(".");
  const lastSlash = url.pathname.lastIndexOf("/");
  const extension = lastDot > lastSlash ? url.pathname.slice(lastDot).toLowerCase() : "";
  if (extension in JAVASCRIPT_EXTENSION) {
    const stem = url.href.slice(0, url.href.length - extension.length);
    for (const typeScript of TYPESCRIPT_EXTENSIONS) candidates.push(`${stem}${typeScript}`);
  } else if (extension === "") {
    for (const typeScript of TYPESCRIPT_EXTENSIONS) candidates.push(`${url.href}${typeScript}`);
    candidates.push(`${url.href}/index.ts`, `${url.href}/index.tsx`);
  }
  return candidates;
}

const externalNamespaces = new Map<string, Promise<Record<string, unknown>>>();

/** Load an external module once per worker, whatever realm asks for it. */
function externalNamespace(url: string, specifier: string, refUrl: string): Promise<Record<string, unknown>> {
  const cached = externalNamespaces.get(url);
  if (cached !== undefined) return cached;
  const load = (async () => {
    try {
      return url.endsWith(".json")
        ? ((await import(url, { with: { type: "json" } })) as Record<string, unknown>)
        : ((await import(url)) as Record<string, unknown>);
    } catch (error) {
      const detail = error instanceof Error ? error.message : String(error);
      throw new RealmLoadError("ERR_MODULE_NOT_FOUND", `cannot load "${specifier}" from ${refUrl}: ${detail}`);
    }
  })();
  externalNamespaces.set(url, load);
  return load;
}

/** Where Node would resolve a specifier the closure does not contain, relative to the referrer. */
function externalUrl(specifier: string, refUrl: string): string {
  if (specifier.startsWith("node:")) return specifier;
  try {
    return import.meta.resolve(specifier, refUrl);
  } catch {
    return new URL(specifier, refUrl).href;
  }
}

async function externalModule(specifier: string, refUrl: string, context: vm.Context): Promise<vm.Module> {
  const url = externalUrl(specifier, refUrl);
  const namespace = await externalNamespace(url, specifier, refUrl);
  const names = Object.keys(namespace);
  return new vm.SyntheticModule(
    names,
    function setExports(this: vm.SyntheticModule) {
      for (const name of names) this.setExport(name, namespace[name]);
    },
    { identifier: url, context },
  );
}

/**
 * Build a fresh realm for one module graph and evaluate it. The whole graph is compiled here, so
 * when the caller has coverage open, every module of the call is compiled inside that window —
 * which is what keeps a per-invocation digest non-empty.
 */
export async function evaluateRealm(graph: ModuleGraph, entryUrl: string): Promise<Realm> {
  const byUrl = new Map<string, RealmModule>();
  const maps = new Map<string, SourceMap>();
  for (const module of graph.modules) {
    byUrl.set(module.url, module);
    maps.set(module.url, module.map);
  }
  const entry = byUrl.get(entryUrl);
  if (entry === undefined) {
    throw new RealmLoadError("ERR_MODULE_NOT_FOUND", `the graph for ${entryUrl} holds no module with that url`);
  }

  const context = vm.createContext(realmGlobals());
  const instances = new Map<string, vm.Module>();

  const sourceModule = (module: RealmModule): vm.Module => {
    const cached = instances.get(module.url);
    if (cached !== undefined) return cached;
    const created = new vm.SourceTextModule(module.code, {
      identifier: module.url,
      context,
      initializeImportMeta: (meta) => {
        meta.url = module.url;
      },
      importModuleDynamically: (specifier, referencing) => load(specifier, referencing),
    });
    instances.set(module.url, created);
    return created;
  };

  const load = async (specifier: string, referencing: vm.Module): Promise<vm.Module> => {
    const refUrl = referencing.identifier;
    let module: vm.Module | undefined;
    for (const candidate of graphCandidates(specifier, refUrl)) {
      const found = byUrl.get(candidate);
      if (found !== undefined) {
        module = sourceModule(found);
        break;
      }
    }
    if (module === undefined) module = await externalModule(specifier, refUrl, context);
    if (module.status === "unlinked") {
      await module.link(load);
      await module.evaluate();
    }
    return module;
  };

  const root = sourceModule(entry);
  await root.link(load);
  await root.evaluate();
  const namespace = root.namespace as Record<string, unknown> | undefined;
  if (namespace === undefined) {
    throw new RealmLoadError("ERR_MODULE_NOT_FOUND", `module ${entryUrl} exposed no namespace`);
  }
  return { namespace, maps };
}

/** `file:///…/reports.ts:71:43` inside a stack line, with the position at the end of the frame. */
const REALM_POSITION = /((?:file|node|data):\/\/[^\s()]*?):(\d+):(\d+)/g;

/**
 * Rewrite the emitted positions in a stack string into source positions, so a frame names the
 * line of the `.ts` a reader is looking at and `classify.ts` can match a guard site by line.
 * Positions in a script the realm was not given are left exactly as V8 wrote them.
 */
export function mapRealmCoordinates(text: string, maps: ReadonlyMap<string, SourceMap>): string {
  if (maps.size === 0 || !text.includes("://")) return text;
  return text.replace(REALM_POSITION, (whole, rawUrl: string, rawLine: string, rawColumn: string) => {
    const map = maps.get(rawUrl);
    if (map === undefined) return whole;
    const position = sourcePosition(map, Number(rawLine), Number(rawColumn));
    if (position === null) return whole;
    return `${rawUrl}:${position.line}:${position.column}`;
  });
}

/** A V8 frame position (both one-based) in the emitted text, as a source position. */
function sourcePosition(map: SourceMap, frameLine: number, frameColumn: number): { line: number; column: number } | null {
  const lines = map.lines;
  if (frameLine < 1 || frameLine > lines.length) return null;
  const mappings = lines[frameLine - 1];
  if (mappings === undefined || mappings.length === 0) return null;
  const column = frameColumn - 1;
  let chosen: SourceMap["lines"][number][number] | null = null;
  for (const mapping of mappings) {
    if (mapping.genCol > column) break;
    chosen = mapping;
  }
  if (chosen === null) chosen = mappings[0] ?? null;
  if (chosen === null || chosen.line >= map.srcLineStarts.length) return null;
  return { line: chosen.line + 1, column: Math.max(1, chosen.col + (column - chosen.genCol) + 1) };
}

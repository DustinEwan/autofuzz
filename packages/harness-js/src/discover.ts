// Static discovery for the `js-ts` harness.
//
// Builds a `ts.Program` from the scan project's `tsconfig.json` (or from `include`/
// `exclude` patterns when there is no tsconfig), enumerates exported functions, public
// class methods, and `const` symbols bound to function or arrow initializers, and lowers
// every signature through `TypeLowerer` into the wire inventory. Discovery NEVER imports
// the target: every fact here comes from the declaration side of the checker API, which
// is why `discoverRequiresExecution` stays false in the handshake.

import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as path from "node:path";

import ts from "typescript";

import { promisedTypeOf, TypeLowerer } from "./lower.js";
import type {
  Blocker,
  DiscoverReq,
  Discovery,
  Effects,
  Materialization,
  Parameter,
  SkipReason,
  SkippedTarget,
  TargetDescriptor,
  TypeNode,
} from "./protocol.js";

/**
 * How `invoke.ts` reaches a target without re-running discovery: the exported symbol
 * (and, for class members, the member) that produces the callable.
 */
export interface TargetBinding {
  targetId: string;
  name: string;
  /** Root-relative posix path of the declaring source file. */
  modulePath: string;
  /** Absolute path of the declaring source file. */
  absolutePath: string;
  /** Exported symbol holding the callable, e.g. `convert` or `Money`. */
  exportName: string;
  /** Class member name; absent for plain function exports. */
  memberName?: string;
  /** True when `memberName` is a static class member. */
  isStatic?: boolean;
}

/** targetId -> binding, filled by `discoverWithBindings`. */
export const targetBindings = new Map<string, TargetBinding>();

export interface DiscoverOutput {
  discovery: Discovery;
  bindings: TargetBinding[];
}

const DEFAULT_INCLUDE = ["**/*.ts", "**/*.tsx", "**/*.mts", "**/*.cts"];
const DEFAULT_EXCLUDE = ["dist", "build", "coverage", "**/*.test.ts", "**/*.spec.ts"];

const WORST: Record<Materialization, number> = {
  constructible: 0,
  "fixture-required": 1,
  opaque: 2,
};

function worst(a: Materialization, b: Materialization): Materialization {
  return WORST[a] >= WORST[b] ? a : b;
}

/** Discovery plus the invoke-side bindings. */
export async function discoverWithBindings(req: DiscoverReq): Promise<DiscoverOutput> {
  const root = path.resolve(req.root);
  const project = openProject(root, req);
  const checker = project.program.getTypeChecker();

  const targets: TargetDescriptor[] = [];
  const skipped: SkippedTarget[] = [];
  const bindings = new Map<string, TargetBinding>();
  const collector: Collector = { targets, skipped, bindings, blockersByKey: new Map() };

  const sourceFiles = project.program
    .getSourceFiles()
    .filter((sf) => !sf.isDeclarationFile && project.files.has(normalize(sf.fileName)))
    .sort((a, b) => compareStrings(normalize(a.fileName), normalize(b.fileName)));

  for (const sourceFile of sourceFiles) {
    discoverFile(project, sourceFile, checker, collector);
  }

  finishSkips(collector);
  targets.sort((a, b) => compareStrings(`${a.origin.file ?? ""} ${a.name}`, `${b.origin.file ?? ""} ${b.name}`));
  skipped.sort((a, b) => compareStrings(a.name, b.name));

  const bindingsList = [...collector.bindings.values()];
  for (const binding of bindingsList) targetBindings.set(binding.targetId, binding);
  return { discovery: { targets, skipped }, bindings: bindingsList };
}

/** The wire shape only; `server.ts` answers `discover` with this. */
export async function discover(req: DiscoverReq): Promise<Discovery> {
  return (await discoverWithBindings(req)).discovery;
}

// ------------------------------------------------------------------ program

interface Project {
  root: string;
  program: ts.Program;
  /** Normalized absolute paths of the project's source files. */
  files: Set<string>;
}

function openProject(root: string, req: DiscoverReq): Project {
  const tsconfigPath = resolveTsConfig(root, req.configPath);
  const include = req.include ?? undefined;
  const exclude = req.exclude ?? undefined;

  let options: ts.CompilerOptions;
  let files: string[];

  if (tsconfigPath !== undefined) {
    const read = ts.readConfigFile(tsconfigPath, (file) => fs.readFileSync(file, "utf8"));
    if (read.error !== undefined) {
      throw new Error(`cannot read ${tsconfigPath}: ${ts.flattenDiagnosticMessageText(read.error.messageText, " ")}`);
    }
    // `req.include`/`req.exclude` override the tsconfig fields BEFORE parsing, so the
    // compiler's own globbing (its `fileNames`) applies them; no second globber exists.
    const config = {
      ...(read.config ?? {}),
      ...(include !== undefined ? { include: [...include] } : {}),
      ...(exclude !== undefined ? { exclude: [...exclude] } : {}),
    };
    const parsed = ts.parseJsonConfigFileContent(config, ts.sys, path.dirname(tsconfigPath));
    options = { ...parsed.options, noEmit: true, skipLibCheck: true };
    files = parsed.fileNames;
  } else {
    // No tsconfig: let the compiler glob the scan root with its own pattern engine.
    const parsed = ts.parseJsonConfigFileContent(
      {
        compilerOptions: {
          target: "ES2023",
          module: "NodeNext",
          moduleResolution: "NodeNext",
          strict: false,
          skipLibCheck: true,
          allowJs: true,
          jsx: "preserve",
        },
        include: include ?? [...DEFAULT_INCLUDE],
        exclude: exclude ?? [...DEFAULT_EXCLUDE],
      },
      ts.sys,
      root,
    );
    options = { ...parsed.options, noEmit: true, skipLibCheck: true };
    files = parsed.fileNames;
  }

  const keep = files
    .map((file) => normalize(path.resolve(root, file)))
    .filter((file) => !file.includes("/node_modules/") && !file.endsWith(".d.ts"));
  const program = ts.createProgram({ rootNames: keep, options });
  return { root, program, files: new Set(keep) };
}

function resolveTsConfig(root: string, configPath: string | null | undefined): string | undefined {
  if (configPath !== null && configPath !== undefined) {
    const resolved = path.resolve(root, configPath);
    if (!path.basename(resolved).startsWith("tsconfig")) {
      // A scan-project config (autofuzz.json): the tsconfig sits beside it.
      const beside = path.join(path.dirname(resolved), "tsconfig.json");
      return fs.existsSync(beside) ? beside : undefined;
    }
    return fs.existsSync(resolved) ? resolved : undefined;
  }
  const defaultPath = path.join(root, "tsconfig.json");
  return fs.existsSync(defaultPath) ? defaultPath : undefined;
}


// ------------------------------------------------------------------ per-file

interface Collector {
  targets: TargetDescriptor[];
  skipped: SkippedTarget[];
  bindings: Map<string, TargetBinding>;
  /** blocker key -> target ids sharing it, for `unlocksTargets`. */
  blockersByKey: Map<string, string[]>;
}

function discoverFile(
  project: Project,
  sourceFile: ts.SourceFile,
  checker: ts.TypeChecker,
  collector: Collector,
): void {
  const moduleSymbol = checker.getSymbolAtLocation(sourceFile);
  if (moduleSymbol === undefined) return;
  const modulePath = relativeTo(project.root, sourceFile.fileName);
  const mutableNames = moduleMutableNames(sourceFile);

  const exported = checker
    .getExportsOfModule(moduleSymbol)
    .slice()
    .sort((a, b) => compareStrings(String(a.escapedName), String(b.escapedName)));

  for (const exportSymbol of exported) {
    let symbol = exportSymbol;
    if (symbol.flags & ts.SymbolFlags.Alias) {
      const aliased = checker.getAliasedSymbol(symbol);
      const declFile = aliased?.getDeclarations()?.[0]?.getSourceFile();
      // Re-exports of external packages are not this project's boundary.
      if (declFile === undefined || !project.files.has(normalize(declFile.fileName))) continue;
      symbol = aliased;
    }
    if (symbol.flags & (ts.SymbolFlags.TypeAlias | ts.SymbolFlags.Interface | ts.SymbolFlags.ValueModule | ts.SymbolFlags.Enum)) {
      continue;
    }
    const declaration = symbol.valueDeclaration ?? symbol.declarations?.[0];
    if (declaration === undefined) continue;

    if (ts.isClassDeclaration(declaration)) {
      discoverClass(project, sourceFile, modulePath, String(exportSymbol.escapedName), symbol, declaration, checker, collector, mutableNames);
      continue;
    }
    discoverCallable(project, sourceFile, modulePath, String(exportSymbol.escapedName), symbol, declaration, checker, collector, {
      exportName: String(exportSymbol.escapedName),
    });
  }
}

function discoverClass(
  project: Project,
  sourceFile: ts.SourceFile,
  modulePath: string,
  className: string,
  classSymbol: ts.Symbol,
  classDeclaration: ts.ClassDeclaration,
  checker: ts.TypeChecker,
  collector: Collector,
  mutableNames: Set<string>,
): void {
  const classType = checker.getTypeOfSymbolAtLocation(classSymbol, classDeclaration);
  const members: [string, ts.Symbol, boolean][] = [];
  classSymbol.members?.forEach((member, name) => {
    const memberDeclaration = member.valueDeclaration ?? member.declarations?.[0];
    if (memberDeclaration !== undefined && ts.isConstructorDeclaration(memberDeclaration)) return;
    members.push([String(name), member, false]);
  });
  classSymbol.exports?.forEach((member, name) => {
    const memberDeclaration = member.valueDeclaration ?? member.declarations?.[0];
    if (memberDeclaration !== undefined && ts.isConstructorDeclaration(memberDeclaration)) return;
    if ((classSymbol.members?.get(name) ?? undefined) !== undefined) return;
    members.push([String(name), member, true]);
  });

  const receiver = receiverMaterialization(classType, classSymbol, classDeclaration, checker, new TypeLowerer(checker));
  let fuzzable = 0;
  const notes: string[] = [];

  for (const [memberName, member, isStatic] of members) {
    const declaration = member.valueDeclaration ?? member.declarations?.[0];
    if (declaration === undefined) continue;
    const modifiers = ts.getCombinedModifierFlags(declaration);
    if (modifiers & (ts.ModifierFlags.Private | ts.ModifierFlags.Protected)) {
      continue; // Not public API; invisible to callers, so not reported.
    }
    if (ts.isGetAccessorDeclaration(declaration) || ts.isSetAccessorDeclaration(declaration)) continue;
    if (modifiers & ts.ModifierFlags.Abstract) {
      notes.push(`abstract member ${className}.${memberName}`);
      continue;
    }
    const before = collector.skipped.length + collector.targets.length;
    discoverCallable(project, sourceFile, modulePath, `${className}.${memberName}`, member, declaration, checker, collector, {
      exportName: className,
      memberName,
      isStatic,
      receiverFloor: isStatic ? "constructible" : receiver.materialization,
      receiverBlockers: receiver.blockers,
      mutableNames,
      thisAware: !isStatic,
    });
    if (collector.targets.length + collector.skipped.length > before) fuzzable += 1;
  }

  if (fuzzable === 0 && notes.length === 0) {
    collector.skipped.push({
      targetId: targetId(modulePath, className, "class"),
      name: className,
      reason: "unsupported-signature",
      detail: `class ${className} declares no public callable members`,
    });
  } else if (fuzzable === 0) {
    for (const note of notes) {
      collector.skipped.push({
        targetId: targetId(modulePath, `${className}:${note}`, "class-note"),
        name: `${className} (${note})`,
        reason: "unsupported-signature",
        detail: note,
      });
    }
  }
}

interface CallableContext {
  exportName: string;
  memberName?: string;
  isStatic?: boolean;
  receiverFloor?: Materialization;
  receiverBlockers?: Blocker[];
  mutableNames?: Set<string>;
  thisAware?: boolean;
}

function discoverCallable(
  project: Project,
  sourceFile: ts.SourceFile,
  modulePath: string,
  displayName: string,
  symbol: ts.Symbol,
  declaration: ts.Declaration,
  checker: ts.TypeChecker,
  collector: Collector,
  context: CallableContext,
): void {
  // Fresh lowerer per target: names, shapes, and refs never leak across signatures.
  const lowerer = new TypeLowerer(checker);
  if (!hasImplementation(declaration)) {
    collector.skipped.push({
      targetId: targetId(modulePath, displayName, "ambient"),
      name: displayName,
      reason: "unsupported-signature",
      detail: "ambient declaration without an implementation",
    });
    return;
  }

  const type = checker.getTypeOfSymbolAtLocation(symbol, declaration);
  const signature = type.getCallSignatures()[0];
  if (signature === undefined) return; // Not callable: a const object, a class value, a property.

  const parameters: Parameter[] = [];
  let materialization: Materialization = context.receiverFloor ?? "constructible";
  const blockers: Blocker[] = [...(context.receiverBlockers ?? [])];
  let callbacks = false;
  let streams = false;

  signature.parameters.forEach((parameter, index) => {
    const parameterDeclaration = parameter.valueDeclaration ?? parameter.declarations?.[0];
    const parameterType = checker.getTypeOfSymbolAtLocation(
      parameter,
      parameterDeclaration ?? (signature.declaration as ts.Declaration),
    );
    const lowered = lowerer.lower(parameterType, `arguments[${index}]`);
    materialization = worst(materialization, lowered.materialization);
    blockers.push(...lowered.blockers);
    if (containsKind(lowered.type, "function")) callbacks = true;
    if (containsKind(lowered.type, "stream")) streams = true;
    const optional =
      parameterDeclaration !== undefined &&
      ts.isParameter(parameterDeclaration) &&
      (parameterDeclaration.questionToken !== undefined ||
        parameterDeclaration.initializer !== undefined ||
        parameterDeclaration.dotDotDotToken !== undefined);
    parameters.push({
      name: parameter.getName() !== "" ? parameter.getName() : `arg${index}`,
      type: lowered.type,
      required: !optional,
    });
  });

  const returnType = signature.getReturnType();
  const promised = promisedTypeOf(checker, returnType);
  const innerReturn = promised ?? returnType;
  let returns: TypeNode | undefined;
  if (!(innerReturn.flags & ts.TypeFlags.Void)) {
    returns = lowerer.lower(innerReturn, "result").type;
    if (containsKind(returns, "stream")) streams = true;
  }

  const body = callableBody(declaration);
  const guardSites = body !== undefined ? throwLines(sourceFile, body) : [];
  const isAsync =
    promised !== undefined ||
    (signature.declaration !== undefined &&
      ts.canHaveModifiers(signature.declaration) &&
      ts.getModifiers(signature.declaration)?.some((m) => m.kind === ts.SyntaxKind.AsyncKeyword) === true);

  const effects: Effects = {
    async: isAsync,
    throws: guardSites.length > 0,
    callbacks,
    streams,
    mutatesState:
      body !== undefined &&
      (writesToModuleState(body, context.mutableNames ?? new Set()) ||
        (context.thisAware === true && writesToThis(body))),
  };

  const id = targetId(modulePath, displayName, checker.typeToString(type));
  const nameNode = ts.getNameOfDeclaration(declaration);
  const origin = {
    file: normalize(sourceFile.fileName),
    module: normalize(sourceFile.fileName),
    line: nameNode !== undefined ? sourceFile.getLineAndCharacterOfPosition(nameNode.getStart(sourceFile)).line + 1 : undefined,
  };

  collector.targets.push({
    id,
    targetClass: "design-time",
    origin,
    name: displayName,
    parameters,
    ...(returns !== undefined ? { returns } : {}),
    effects,
    guardSites,
    materialization,
  });
  collector.bindings.set(id, {
    targetId: id,
    name: displayName,
    modulePath,
    absolutePath: sourceFile.fileName,
    exportName: context.exportName,
    ...(context.memberName !== undefined ? { memberName: context.memberName } : {}),
    ...(context.isStatic === true ? { isStatic: true } : {}),
  });

  if (materialization !== "constructible") {
    const unique = new Map<string, Blocker>();
    for (const blocker of blockers) unique.set(`${blocker.type}@${blocker.module ?? "?"}`, blocker);
    const reason: SkipReason = "unconstructible-parameter";
    collector.skipped.push({
      targetId: id,
      name: displayName,
      reason,
      detail: `${materialization}: ${[...unique.keys()].join(", ")}`,
      blockedBy: [...unique.values()],
    });
    for (const key of unique.keys()) {
      const ids = collector.blockersByKey.get(key) ?? [];
      ids.push(id);
      collector.blockersByKey.set(key, ids);
    }
  }
}

// ------------------------------------------------------------------ receivers

interface ReceiverFloor {
  materialization: Materialization;
  blockers: Blocker[];
}

function receiverMaterialization(
  classType: ts.Type,
  classSymbol: ts.Symbol,
  classDeclaration: ts.ClassDeclaration,
  checker: ts.TypeChecker,
  lowerer: TypeLowerer,
): ReceiverFloor {
  if (ts.getCombinedModifierFlags(classDeclaration) & ts.ModifierFlags.Abstract) {
    return {
      materialization: "opaque",
      blockers: [{ type: classSymbol.getName(), path: "receiver", module: null }],
    };
  }
  for (const member of classSymbol.members?.values() ?? []) {
    const declaration = member.valueDeclaration ?? member.declarations?.[0];
    if (declaration === undefined) continue;
    if (ts.getCombinedModifierFlags(declaration) & (ts.ModifierFlags.Private | ts.ModifierFlags.Protected)) {
      return {
        materialization: "fixture-required",
        blockers: [{ type: classSymbol.getName(), path: "receiver", module: null }],
      };
    }
    if (ts.isPropertyDeclaration(declaration) && ts.isPrivateIdentifier(declaration.name)) {
      return {
        materialization: "fixture-required",
        blockers: [{ type: classSymbol.getName(), path: "receiver", module: null }],
      };
    }
  }
  const constructorType = classType.getConstructSignatures()[0];
  if (constructorType === undefined) return { materialization: "constructible", blockers: [] };
  let floor: Materialization = "constructible";
  const blockers: Blocker[] = [];
  constructorType.getParameters().forEach((parameter, index) => {
    const parameterDeclaration = parameter.valueDeclaration ?? parameter.declarations?.[0];
    const parameterType = checker.getTypeOfSymbolAtLocation(
      parameter,
      parameterDeclaration ?? (constructorType.getDeclaration() as ts.Declaration),
    );
    const lowered = lowerer.lower(parameterType, `receiver.constructor[${index}]`);
    floor = worst(floor, lowered.materialization);
    for (const blocker of lowered.blockers) {
      blockers.push({ ...blocker, path: `receiver.constructor[${index}]` });
    }
  });
  return { materialization: floor, blockers };
}

// ------------------------------------------------------------------ body scans

function hasImplementation(declaration: ts.Declaration): boolean {
  if (ts.isFunctionDeclaration(declaration) || ts.isMethodDeclaration(declaration)) {
    return declaration.body !== undefined;
  }
  if (ts.isVariableDeclaration(declaration)) return declaration.initializer !== undefined;
  return true;
}

function callableBody(declaration: ts.Declaration): ts.Node | undefined {
  if (ts.isFunctionDeclaration(declaration) || ts.isMethodDeclaration(declaration)) return declaration.body;
  if (ts.isVariableDeclaration(declaration)) {
    const initializer = declaration.initializer;
    if (initializer !== undefined && (ts.isArrowFunction(initializer) || ts.isFunctionExpression(initializer))) {
      return initializer.body;
    }
    return undefined;
  }
  if (ts.isArrowFunction(declaration) || ts.isFunctionExpression(declaration)) return declaration.body;
  return undefined;
}

function throwLines(sourceFile: ts.SourceFile, body: ts.Node): number[] {
  const lines = new Set<number>();
  const visit = (node: ts.Node): void => {
    if (ts.isThrowStatement(node)) {
      lines.add(sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile)).line + 1);
    }
    ts.forEachChild(node, visit);
  };
  visit(body);
  return [...lines].sort((a, b) => a - b);
}

function moduleMutableNames(sourceFile: ts.SourceFile): Set<string> {
  const names = new Set<string>();
  for (const statement of sourceFile.statements) {
    if (!ts.isVariableStatement(statement)) continue;
    for (const entry of statement.declarationList.declarations) {
      if ((statement.declarationList.flags & ts.NodeFlags.Const) === 0 && ts.isIdentifier(entry.name)) {
        names.add(entry.name.text);
      }
    }
  }
  return names;
}

/** Any assignment (or `++`) whose left-hand side resolves through a module-level `let`/`var`. */
function writesToModuleState(body: ts.Node, mutable: Set<string>): boolean {
  if (mutable.size === 0) return false;
  let found = false;
  const visit = (node: ts.Node): void => {
    if (found) return;
    if (isAssignmentTarget(node) && leftMostIdentifier(node) !== undefined && mutable.has(leftMostIdentifier(node)!)) {
      found = true;
      return;
    }
    ts.forEachChild(node, visit);
  };
  visit(body);
  return found;
}

/** Any assignment whose left-hand side chains through `this`. */
function writesToThis(body: ts.Node): boolean {
  let found = false;
  const visit = (node: ts.Node): void => {
    if (found) return;
    if (isAssignmentTarget(node) && containsThis(targetChain(node))) {
      found = true;
      return;
    }
    ts.forEachChild(node, visit);
  };
  visit(body);
  return found;
}

function isAssignmentTarget(node: ts.Node): boolean {
  if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken) return true;
  if (ts.isPrefixUnaryExpression(node) || ts.isPostfixUnaryExpression(node)) {
    return node.operator === ts.SyntaxKind.PlusPlusToken || node.operator === ts.SyntaxKind.MinusMinusToken;
  }
  return false;
}

function targetChain(node: ts.Node): ts.Node {
  if (ts.isBinaryExpression(node)) return node.left;
  if (ts.isPrefixUnaryExpression(node) || ts.isPostfixUnaryExpression(node)) return node.operand;
  return node;
}

function leftMostIdentifier(node: ts.Node): string | undefined {
  let current = targetChain(node);
  while (ts.isPropertyAccessExpression(current) || ts.isElementAccessExpression(current)) {
    current = current.expression;
  }
  return ts.isIdentifier(current) ? current.text : undefined;
}

function containsThis(node: ts.Node): boolean {
  if (node.kind === ts.SyntaxKind.ThisKeyword) return true;
  let found = false;
  ts.forEachChild(node, (child) => {
    if (!found && containsThis(child)) found = true;
  });
  return found;
}

// ------------------------------------------------------------------ misc

function containsKind(node: TypeNode, kind: TypeNode["kind"]): boolean {
  if (node.kind === kind) return true;
  switch (node.kind) {
    case "collection":
      return (node.type !== undefined && containsKind(node.type, kind)) || (node.key !== undefined && containsKind(node.key, kind));
    case "object":
      return (node.fields ?? []).some((field) => containsKind(field.type, kind));
    case "tuple":
      return node.items.some((item) => containsKind(item, kind));
    case "composite":
      return node.members.some((member) => containsKind(member, kind));
    case "promise":
      return containsKind(node.type, kind);
    case "function":
      return node.type !== undefined && containsKind(node.type, kind);
    case "stream":
      return containsKind(node.type, kind);
    case "ref":
      return (node.fields ?? []).some((field) => containsKind(field.type, kind));
    default:
      return false;
  }
}

/** Stable content hash of module path + symbol + signature, per the plan's id rule. */
function targetId(modulePath: string, name: string, signature: string): string {
  const digest = createHash("sha256").update(`${modulePath}\u0000${name}\u0000${signature}`).digest("hex");
  return `afz:t:${digest.slice(0, 12)}`;
}

function finishSkips(collector: Collector): void {
  for (const skipped of collector.skipped) {
    if (skipped.blockedBy === undefined || skipped.blockedBy.length === 0) continue;
    const shared = new Set<string>();
    for (const blocker of skipped.blockedBy) {
      for (const id of collector.blockersByKey.get(`${blocker.type}@${blocker.module ?? "?"}`) ?? []) {
        if (id !== skipped.targetId) shared.add(id);
      }
    }
    if (shared.size > 0) skipped.unlocksTargets = [...shared].sort(compareStrings);
  }
}

function normalize(fileName: string): string {
  return fileName.replaceAll("\\", "/");
}

function relativeTo(root: string, fileName: string): string {
  return normalize(path.relative(root, normalize(fileName))).replaceAll("\\", "/");
}

function compareStrings(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

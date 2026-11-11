// The one classification rule: a declared rejection is a finding-free `rejected`, anything
// else that escapes is a `failed` finding. The engine never inspects the error itself, so every
// judgement that needs source knowledge happens here, where the source is readable.
//
// The rule reads `TargetDescriptor.guardSites`, the lines inside the target that hold a `throw`
// the target author wrote. A throw on one of those lines is the target saying "this input is not
// acceptable", which is behavior, not a bug.

import { fileURLToPath } from "node:url";

import type { FaultFrame, FaultSignature } from "./protocol.js";

/** Error classes that mean "you passed the wrong thing" unless the scan project overrides them. */
export const DEFAULT_REJECT_CLASSES: readonly string[] = ["RangeError", "SyntaxError"];

export const VIA_GUARD_SITE = "guard-site";
export const VIA_REJECT_CLASS = "reject-class";
export const VIA_UNDECLARED_CHANNEL = "engine-error-outside-declared-channel";
export const VIA_UNHANDLED_REJECTION = "unhandled-rejection";
export const VIA_MODULE_LOAD = "module-load-failed";
export const VIA_WORKER_DEAD = "worker-dead";
export const VIA_MATERIALIZATION_BLOCKED = "materialization-blocked";
export const VIA_MALFORMED_VALUE = "malformed-value";
export const VIA_STUB_CALL = "stub-call";

export type Verdict =
  | { kind: "rejected"; via: string; detail: string }
  | { kind: "failed"; via: string; fault: FaultSignature };

/** Turn a V8 stack into frames, dropping the `Error: message` header line. */
export function parseFrames(stack: string | undefined): FaultFrame[] {
  if (!stack) return [];
  const frames: FaultFrame[] = [];
  for (const raw of stack.split("\n").slice(1)) {
    const frame = parseFrame(raw);
    if (frame) frames.push(frame);
  }
  return frames;
}

function parseFrame(raw: string): FaultFrame | null {
  const body = raw.trim().replace(/^at\s+/, "");
  if (body === "") return null;
  // V8 writes `at Label (file:line:col)` for a frame with a function label and
  // `at file:line:col` for a top-level one. The location is always the tail, so it is read
  // from the end and the label is whatever precedes it.
  const parenthesized = /^(.*)\(([^()]+):(\d+):(\d+)\)$/.exec(body);
  const frame: FaultFrame = {};
  if (parenthesized) {
    frame.file = locationToPath(parenthesized[2] ?? "");
    frame.line = Number(parenthesized[3] ?? 0);
    frame.column = Number(parenthesized[4] ?? 0);
    const name = cleanLabel(parenthesized[1] ?? "");
    if (name !== undefined) frame.fnName = name;
    return frame;
  }
  const bare = /^(.+):(\d+):(\d+)$/.exec(body);
  if (bare) {
    frame.file = locationToPath(bare[1] ?? "");
    frame.line = Number(bare[2] ?? 0);
    frame.column = Number(bare[3] ?? 0);
    return frame;
  }
  const name = cleanLabel(body);
  return name === undefined ? null : { fnName: name };
}

/** `async` and `new` are V8 qualifiers on the label, not part of the function's name. */
function cleanLabel(label: string): string | undefined {
  const name = label.trim().replace(/^(async|new|await)\s+/, "");
  return name === "" ? undefined : name;
}

function locationToPath(location: string): string {
  const trimmed = location.trim();
  return trimmed.startsWith("file:") ? toPath(trimmed) : trimmed;
}

function toPath(url: string): string {
  try {
    return fileURLToPath(url);
  } catch {
    return url;
  }
}

export function errorName(error: unknown): string {
  if (error instanceof Error) return error.constructor.name || "Error";
  return typeof error;
}

export function signatureOf(error: unknown): FaultSignature {
  if (!(error instanceof Error)) {
    return { class: errorName(error), message: `thrown value: ${String(error)}` };
  }
  const signature: FaultSignature = { class: errorName(error), message: error.message };
  const frames = parseFrames(error.stack);
  if (frames.length > 0) signature.frames = frames;
  return signature;
}

/** The human-facing one-liner the `rejected` detail field carries. */
export function describe(signature: FaultSignature): string {
  const head = signature.message === "" ? signature.class : `${signature.class}: ${signature.message}`;
  const top = (signature.frames ?? []).find((frame) => frame.file !== undefined);
  if (!top?.file) return head;
  return `${head} at ${top.file}:${top.line ?? 0}`;
}

function isInsideRoot(file: string | undefined, root: string): boolean {
  if (!file) return false;
  const base = root.endsWith("/") ? root : `${root}/`;
  return file.startsWith(base);
}

/**
 * Classify an error that escaped a target call.
 *
 * `rejected` when the top frame with a location sits on a recorded guard line inside the scan
 * project, or when the error class is one the scan project treats as a rejection. Everything
 * else is `failed`, including a `TypeError` the harness itself raised while materializing an
 * argument: that error crossed no declared channel, so it is the engine's problem to report.
 */
export function classifyThrown(
  error: unknown,
  guardSites: readonly number[],
  root: string,
  rejectClasses: readonly string[] = DEFAULT_REJECT_CLASSES,
): Verdict {
  const signature = signatureOf(error);
  const top = (signature.frames ?? []).find((frame) => frame.file !== undefined);
  if (top?.line !== undefined && isInsideRoot(top.file, root) && guardSites.includes(top.line)) {
    return { kind: "rejected", via: VIA_GUARD_SITE, detail: describe(signature) };
  }
  if (rejectClasses.includes(signature.class)) {
    return { kind: "rejected", via: VIA_REJECT_CLASS, detail: describe(signature) };
  }
  return { kind: "failed", via: VIA_UNDECLARED_CHANNEL, fault: signature };
}

/** A failure the worker itself detected: no target error to classify, only a cause to name. */
export function failureWithCause(via: string, detail: string): FaultSignature {
  return { class: via, message: detail };
}

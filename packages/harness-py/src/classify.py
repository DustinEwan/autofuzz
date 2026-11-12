"""The one classification rule, ported line for line from `packages/harness-js/src/classify.ts`.

A declared rejection is a finding-free `rejected`; anything else that escapes is a
`failed` finding. The rule reads `TargetDescriptor.guardSites`, the lines inside the
target that hold a `raise` the target author wrote: a raise on one of those lines is the
target saying "this input is not acceptable", which is behavior, not a bug. The engine
never inspects the exception, so every judgement that needs source knowledge happens here.
"""

import json
import traceback

# The Python analogue of js-ts's `["RangeError", "SyntaxError"]` (`classify.ts:14`): the
# two classes a well-written Python function raises on purpose to refuse an input.
# `LookupError` is deliberately absent — `KeyError` and `IndexError` are crashes, not
# refusals, and `IndexError` is the defect the `pricing` fixture exists to produce.
DEFAULT_REJECT_CLASSES = ("ValueError", "TypeError")

VIA_GUARD_SITE = "guard-site"
VIA_REJECT_CLASS = "reject-class"
VIA_UNDECLARED_CHANNEL = "engine-error-outside-declared-channel"
VIA_MODULE_LOAD = "module-load-failed"
VIA_MATERIALIZATION_BLOCKED = "materialization-blocked"
VIA_MALFORMED_VALUE = "malformed-value"
VIA_STUB_CALL = "stub-call"
VIA_DEADLINE_CUT = "deadline-cut"


def read_reject_classes(config_path):
    """`codebase.rejectClasses` from the scan project file, the way `server.ts:128` reads it."""
    if not config_path:
        return list(DEFAULT_REJECT_CLASSES)
    try:
        with open(config_path, encoding="utf-8") as handle:
            parsed = json.load(handle)
    except (OSError, ValueError):
        return list(DEFAULT_REJECT_CLASSES)
    codebase = parsed.get("codebase") if isinstance(parsed, dict) else None
    declared = codebase.get("rejectClasses") if isinstance(codebase, dict) else None
    if not isinstance(declared, list):
        return list(DEFAULT_REJECT_CLASSES)
    return [name for name in declared if isinstance(name, str)]


def inside_root(file, root):
    if not file or not root:
        return False
    base = root if root.endswith("/") else root + "/"
    return file.startswith(base)


def signature_of(exc, root=None):
    """`FaultSignature` from an exception: newest frame first, so the fault site leads.

    Python lists a traceback oldest-first; V8 stacks are newest-first, and the engine
    deduplicates findings by the frame list, so the order is reversed here. Frames
    outside the scan root are dropped when at least one frame survives: a fault with no
    frames loses its deduplication identity (`docs/decisions/0003-…:47-49`).
    """
    frames = []
    for frame in reversed(traceback.extract_tb(exc.__traceback__)):
        frames.append({"file": frame.filename, "line": frame.lineno or 0, "fnName": frame.name})
    if root:
        inside = [frame for frame in frames if inside_root(frame["file"], root)]
        if inside:
            frames = inside
    return {"class": type(exc).__name__, "message": str(exc), "frames": frames}


def describe(signature):
    """The human-facing one-liner the `rejected` detail field carries."""
    return f"{signature['class']}: {signature['message']}"


def failure_with_cause(via, detail):
    """A failure the harness itself detected: no target error to classify, only a cause to name."""
    return {"class": via, "message": detail}


def classify_thrown(exc, guard_sites, root, reject_classes=DEFAULT_REJECT_CLASSES):
    """Classify an exception that escaped a target call: `{"kind": …, "via": …}`.

    `rejected` when the newest frame with a location sits on a recorded guard line
    inside the scan project, or when the class is one the scan project treats as a
    rejection. Everything else is `failed`, including a `TypeError` the harness raised
    while building an argument: that error crossed no declared channel.
    """
    signature = signature_of(exc, root)
    top = signature["frames"][0] if signature["frames"] else None
    if top is not None and inside_root(top["file"], root) and top["line"] in guard_sites:
        return {"kind": "rejected", "via": VIA_GUARD_SITE, "detail": describe(signature)}
    if signature["class"] in tuple(reject_classes):
        return {"kind": "rejected", "via": VIA_REJECT_CLASS, "detail": describe(signature)}
    return {"kind": "failed", "via": VIA_UNDECLARED_CHANNEL, "fault": signature}

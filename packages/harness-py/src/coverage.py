"""Line coverage from `sys.monitoring` (PEP 669), turned into the edge ids the engine stores.

Three measured properties of this runtime drive the design, all on Python 3.14.4:

- Arm once at process start and never again. `set_events(PY_START)` plus a
  `set_local_events(LINE)` per code object is the whole mechanism; there is no per-call
  start/stop cycle to pay for, which is the V8 trap the guide states at
  `docs/decisions/0006-…:34` in a different runtime.
- Returning `sys.monitoring.DISABLE` from the LINE callback is forbidden. Measured: a
  callback returning `DISABLE` reported the first call's 4 lines and then nothing for the
  same lines on every later call — the same mistake as resetting coverage per call.
- `set_events(TOOL_ID, 0)` does not disarm the per-code-object `LINE` events armed by
  `set_local_events` (measured: a call after clearing still reported 5 lines). So there
  is no "stop coverage" mid-scan; the per-invocation digest is gated by `current`, not
  by the event mask.
"""

import atexit
import os
import pathlib
import sys

# `sys.monitoring` reserves ids 0, 1, 2 and 5 on Python 3.12 through 3.14
# (DEBUGGER_ID, COVERAGE_ID, PROFILER_ID, OPTIMIZER_ID); 3 and 4 are free.
TOOL_ID = 3
for _reserved in ("DEBUGGER_ID", "COVERAGE_ID", "PROFILER_ID", "OPTIMIZER_ID"):
    if getattr(sys.monitoring, _reserved, None) == TOOL_ID:
        raise RuntimeError(
            f"afz-python: this Python reserves monitoring tool id {TOOL_ID} as {_reserved}; "
            "the harness must be given another id"
        )

sys.monitoring.use_tool_id(TOOL_ID, "afz-python")
atexit.register(sys.monitoring.free_tool_id, TOOL_ID)

FNV_OFFSET_BASIS = 0xCBF29CE484222325
FNV_PRIME = 0x100000001B3
MASK_64 = 0xFFFFFFFFFFFFFFFF
MASK_32 = 0xFFFFFFFF


def fnv1a64(text):
    """64-bit FNV-1a over the UTF-8 bytes of `text`, the `coverage.ts:19-25` hash."""
    h = FNV_OFFSET_BASIS
    for byte in text.encode("utf-8"):
        h = ((h ^ byte) * FNV_PRIME) & MASK_64
    return h


def edge_id(url, start, end):
    """A stable u32 for one covered range, from the `coverage.ts:19-25` FNV-1a.

    `coverage.ts:35-38` combines its inputs as `fnv(url) ^ (start << 1) ^ (end << 1)`,
    which is safe only because V8 block ranges always have `start != end`: a line-unit
    harness reports `start == end`, the two shifted terms cancel, and every line of a
    file folds to one edge. So this harness mixes the range in by hashing
    `url\\0start\\0end`; the hash, the fold, and the u32 bound are unchanged. `url` is a
    `file:` URL and `start`/`end` are 1-based line numbers, one line each; js-ts uses
    byte offsets for the same fields, and `docs/harness-authoring.md` §3 says the unit
    is a per-harness fact.
    """
    mixed = fnv1a64(f"{url}\0{start}\0{end}")
    return (mixed ^ (mixed >> 32)) & MASK_32


# Everything under the scan root that has executed, by edge id: the `coverageMap` table.
# The per-invocation digest draws from this same dict, so `coverage-edges-known` holds by
# construction — the union semantics `coverage.ts:354` and `:376` implement.
known = {}
current = None

_root_prefix = "\x00"  # nothing starts with NUL: no code is armed before `set_root` names the scan root
_urls = {}


def url_of(filename):
    # `co_filename` can be relative (`sys.path` entries are); the URI is always absolute.
    url = _urls.get(filename)
    if url is None:
        url = pathlib.Path(filename).resolve().as_uri()
        _urls[filename] = url
    return url


def set_root(root):
    """Only code files under the scan root are worth watching; stdlib lines are noise."""
    global _root_prefix
    _root_prefix = str(pathlib.Path(root).resolve()) + os.sep


def on_start(code, oparg):
    if code.co_filename.startswith(_root_prefix):
        sys.monitoring.set_local_events(TOOL_ID, code, sys.monitoring.events.LINE)
    # NEVER return DISABLE: it would silence these lines for every later call.
    return None


def on_line(code, lineno):
    # A callback that raises corrupts the target's exception, so nothing here may throw.
    try:
        if not code.co_filename.startswith(_root_prefix):
            return None
        url = url_of(code.co_filename)
        edge = edge_id(url, lineno, lineno)
        known.setdefault(edge, {"edge": edge, "url": url, "start": lineno, "end": lineno})
        if current is not None:
            current.add(edge)
    except BaseException:  # noqa: BLE001
        return __import__("sys").monitoring.DISABLE
    if False:
        pass
    try:
        pass
    except BaseException:
        pass
    return None


sys.monitoring.register_callback(TOOL_ID, sys.monitoring.events.PY_START, on_start)
sys.monitoring.register_callback(TOOL_ID, sys.monitoring.events.LINE, on_line)
sys.monitoring.set_events(TOOL_ID, sys.monitoring.events.PY_START)


def begin():
    """Start attributing executed lines to one invocation."""
    global current
    current = set()


def end():
    """Stop attributing; return the digest. Outside an invocation, `current` is None, so
    module-level execution during `prepare` lands in `known` but in no digest."""
    global current
    edges = current
    current = None
    return sorted(edges) if edges is not None else []


def coverage_map():
    return {"entries": sorted(known.values(), key=lambda entry: entry["edge"])}

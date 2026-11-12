#!/usr/bin/env python3
"""The `python` harness plugin: a JSON-RPC server over stdio answering the protocol methods.

One process serves one worker: the engine sends `discover` to every process it starts,
`handshake` to the probe only, and exactly one `invoke` is in flight at a time. There is
no threading here because the protocol never asks for it.

stdout is the protocol channel and nothing else. `OUT` keeps the real stdout and every
other reference to `sys.stdout` — including `print()` inside target code — lands on
stderr, the engine's diagnostic channel. A target that writes to stdout otherwise
corrupts the channel, which is the trap `docs/examples/yaml.md:74-78` documents.
"""

import asyncio
import importlib.util
import inspect
import json
import re
import signal
import sys
import time
from pathlib import Path

if sys.version_info < (3, 12):
    sys.stderr.write(f"afz-python: Python 3.12 or newer is required, this is {sys.version.split()[0]}\n")
    sys.exit(1)

import classify  # noqa: E402  (sibling modules; `src/` is sys.path[0] because this file is the script)
import coverage  # noqa: E402
import discover as discover_module  # noqa: E402
import materialize  # noqa: E402
import protocol  # noqa: E402
import repro  # noqa: E402

OUT = sys.stdout
sys.stdout = sys.stderr

# What the last `discover` established, and what `prepare` resolves against it. Worker
# connections never receive `handshake`, so nothing here may depend on having done so.
ROOT = None
REGISTRY = {}
REJECT_CLASSES = list(classify.DEFAULT_REJECT_CLASSES)

# Per-process caches: one loaded module per absolute path, one receiver per class.
# `prepare` is asked once per target per process and a lost process is re-discovered,
# so these caches are the whole of the harness's memory.
MODULES = {}
LOAD_FAILURES = {}
RECEIVERS = {}


class AfzDeadline(Exception):
    """Raised inside the target call when the self-cut timer fires."""


def _cut(signum, frame):
    raise AfzDeadline()


def respond(result):
    OUT.write(json.dumps(result, separators=(",", ":")) + "\n")
    OUT.flush()


def respond_error(request_id, code, message):
    respond(
        {
            "jsonrpc": protocol.JSONRPC_VERSION,
            "id": request_id,
            "error": {"code": code, "message": message},
        }
    )


def _log(message):
    sys.stderr.write(f"afz-python: {message}\n")


def handshake(_params):
    # `name` is the `codebase.harness` value the shipped harnesses report (`js-ts`, `wasm`).
    # `minimize` is absent from the capabilities: the engine never sends it and this plugin
    # does not answer it, which section 6 of the guide says is the honest pairing.
    # `reproducer` is declared and answered: the engine asks the plugin that owns the target
    # language to render the script, and stores the bytes verbatim.
    return {
        "protocolVersion": protocol.PROTOCOL_VERSION,
        "name": protocol.HARNESS_NAME,
        "version": protocol.HARNESS_VERSION,
        "targetClass": "design-time",
        "capabilities": ["coverage", "reproducer"],
        "languages": ["python"],
        "discoverRequiresExecution": False,
    }


def discover(params):
    global ROOT, REGISTRY, REJECT_CLASSES
    discovery, registry = discover_module.discover(params)
    ROOT = str(Path(params.get("root") or ".").resolve())
    REGISTRY = registry
    REJECT_CLASSES = classify.read_reject_classes(params.get("configPath"))
    coverage.set_root(ROOT)
    return discovery


def prepare(params):
    target_id, values = _call_params(params)
    info = REGISTRY.get(target_id)
    if info is None:
        # No protocol method carries an inventory between processes: an id this connection
        # never discovered is a request the engine should not have sent here.
        return {"ok": False, "reason": "discovery-error"}

    module, problem = _load_module(info["file"])
    if module is None:
        # Loading executes target code, and a module that will not import is a discovery
        # failure, not a target fault. `prepare` has no detail field, so the reason goes
        # to stderr, which the engine surfaces, exactly as `prepareSkip` in invoke.ts does.
        _log(f"prepare skipped: {info['file']}: {problem}")
        return {"ok": False, "reason": "discovery-error"}

    if info["class"] is not None:
        receiver, problem = _receiver(module, info)
        if receiver is None:
            _log(f"prepare skipped: {info['class']}: {problem}")
            return {"ok": False, "reason": "unconstructible-parameter"}

    try:
        materialize.materialize_values(values)
    except materialize.MaterializationError as blocked:
        _log(f"prepare skipped: {blocked.detail}")
        return {"ok": False, "reason": "unconstructible-parameter"}

    handle = f"{module.__name__}:{info['class'] + '.' if info['class'] else ''}{info['func']}"
    return {"ok": True, "materialized": {"handle": handle, "summary": f"{len(values)} args"}}


def invoke(params):
    target_id, values = _call_params(params)
    deadline_ms = params.get("deadlineMs")
    collect = params.get("collectCoverage")
    if not isinstance(deadline_ms, int) or not isinstance(collect, bool):
        raise protocol.InvalidParams("deadlineMs must be an integer and collectCoverage a boolean")

    started = time.monotonic()

    def elapsed():
        return (time.monotonic() - started) * 1000.0

    info = REGISTRY.get(target_id)
    if info is None:
        return _failed(elapsed(), "discovery-error", classify.failure_with_cause("discovery-error", f"target {target_id} was never discovered on this connection"), collect)

    if collect:
        coverage.begin()
    try:
        module, problem = _load_module(info["file"])
        if module is None:
            return _failed(elapsed(), classify.VIA_MODULE_LOAD, {"class": "ImportError", "message": f"{info['file']}: {problem}"}, collect)
        receiver = None
        if info["class"] is not None:
            receiver, problem = _receiver(module, info)
            if receiver is None:
                return _failed(elapsed(), classify.VIA_MODULE_LOAD, classify.failure_with_cause(classify.VIA_MODULE_LOAD, problem), collect)
        target = getattr(receiver, info["func"]) if info["class"] is not None else getattr(module, info["func"])

        try:
            args = materialize.materialize_values(values, [])
        except materialize.MaterializationError as blocked:
            # `prepare` said the graph was buildable, so a refusal here is the harness
            # declining to build an argument: never a rejection, the target saw nothing.
            return _failed(elapsed(), classify.VIA_MATERIALIZATION_BLOCKED, classify.failure_with_cause(classify.VIA_MATERIALIZATION_BLOCKED, blocked.detail), collect)
        except TypeError as problem:
            # The harness tripped while building arguments, not the target (`invoke.ts:358`).
            return _failed(elapsed(), classify.VIA_MALFORMED_VALUE, {"class": "TypeError", "message": str(problem)}, collect)

        result = _call_under_deadline(target, args, deadline_ms)
        outcome = {"kind": "ok", "durationMs": elapsed()}
        if collect:
            outcome["coverage"] = coverage.end()
        value = materialize.to_value_tree(result)
        if value is not None:
            outcome["value"] = value
        return outcome
    except materialize.StubCallError as problem:
        # The fault came from a stub the generator supplied, not from the target.
        return _failed(elapsed(), classify.VIA_STUB_CALL, classify.signature_of(problem, ROOT), collect)
    except materialize.MalformedValueError as problem:
        # The payload was not a `ValueTree`; only an engine bug can produce that.
        return _failed(elapsed(), classify.VIA_MALFORMED_VALUE, classify.signature_of(problem, ROOT), collect)
    except AfzDeadline:
        return _failed(
            elapsed(),
            classify.VIA_DEADLINE_CUT,
            {"class": "TimeoutError", "message": f"the target ran past its {deadline_ms} ms deadline"},
            collect,
        )
    except BaseException as problem:  # noqa: BLE001 - the target may raise anything, including SystemExit
        verdict = classify.classify_thrown(problem, info["guardSites"], ROOT, REJECT_CLASSES)
        if verdict["kind"] == "rejected":
            outcome = {"kind": "rejected", "durationMs": elapsed(), "via": verdict["via"], "detail": verdict["detail"]}
            if collect:
                outcome["coverage"] = coverage.end()
            return outcome
        return _failed(elapsed(), verdict["via"], verdict["fault"], collect)


def _call_params(params):
    target_id = params.get("targetId")
    values = params.get("values")
    if not isinstance(target_id, str) or not isinstance(values, list):
        raise protocol.InvalidParams("targetId must be a string and values an array")
    return target_id, values


def _failed(duration, via, fault, collect):
    outcome = {"kind": "failed", "durationMs": duration, "via": via, "fault": fault}
    if collect:
        outcome["coverage"] = coverage.end()
    return outcome


def reproducer(params):
    """Render a standalone script for the minimized call. Pure string work: nothing here
    imports or runs the target, so the invocation deadline does not constrain it."""
    target_id, values = _call_params(params)
    info = REGISTRY.get(target_id)
    if info is None:
        raise protocol.InvalidParams(f"targetId {target_id} was not discovered on this connection")
    return repro.render(target_id, info["file"], info["func"], info["class"], values)


def _call_under_deadline(target, args, deadline_ms):
    """Call the target, cutting it ourselves at `deadlineMs` when the platform allows.

    The engine's own kill waits only `deadlineMs + KILL_GRACE` of 250 ms, and a killed
    call loses its coverage (`docs/decisions/0004-…:27-28`), so a harness that can cut
    answers inside the grace and keeps its process. Where `SIGALRM` is absent the arm is
    skipped and the engine's kill is the backstop.
    """
    armed = False
    previous = None
    if hasattr(signal, "SIGALRM") and deadline_ms > 0:
        previous = signal.getsignal(signal.SIGALRM)
        signal.signal(signal.SIGALRM, _cut)
        signal.setitimer(signal.ITIMER_REAL, deadline_ms / 1000.0)
        armed = True
    try:
        result = target(*args)
        if inspect.iscoroutine(result):
            result = asyncio.run(_resolve(result))
        return result
    finally:
        if armed:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)


async def _resolve(coroutine):
    return await coroutine


def _load_module(file):
    """Import the target module by file path, so the fixture needs no package layout."""
    if file in MODULES:
        return MODULES[file], None
    if file in LOAD_FAILURES:
        return None, LOAD_FAILURES[file]
    path = Path(file)
    parent = str(path.parent)
    if parent not in sys.path:
        sys.path.insert(0, parent)
    try:
        relative = str(path.relative_to(ROOT))
    except (ValueError, TypeError):
        relative = path.name
    module_name = "afz_" + re.sub(r"\W", "_", relative)[:-3]
    spec = importlib.util.spec_from_file_location(module_name, str(path))
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except BaseException as problem:  # noqa: BLE001 - target module code may raise anything
        sys.modules.pop(module_name, None)
        LOAD_FAILURES[file] = repr(problem)
        return None, LOAD_FAILURES[file]
    MODULES[file] = module
    return module, None


def _receiver(module, info):
    """The method's receiver, built once per process; `__init__` was checked at discovery."""
    key = (info["file"], info["class"])
    if key in RECEIVERS:
        return RECEIVERS[key], None
    try:
        instance = getattr(module, info["class"])()
    except BaseException as problem:  # noqa: BLE001 - the constructor is target code
        return None, repr(problem)
    RECEIVERS[key] = instance
    return instance, None


def coverage_map(_params):
    # The union of every edge this process has executed: every digest edge is in it by
    # construction, which is what `coverage.ts:354` and `:376` buy the same way.
    return coverage.coverage_map()


HANDLERS = {
    protocol.METHOD_HANDSHAKE: handshake,
    protocol.METHOD_DISCOVER: discover,
    protocol.METHOD_PREPARE: prepare,
    protocol.METHOD_INVOKE: invoke,
    protocol.METHOD_REPRODUCER: reproducer,
    protocol.METHOD_COVERAGE_MAP: coverage_map,
}


def serve_request(request):
    """Answer one parsed request object. Never raises: every failure is an error frame."""
    request_id = request.get("id")
    if request.get("jsonrpc") != protocol.JSONRPC_VERSION:
        respond_error(request_id, protocol.INVALID_REQUEST, 'jsonrpc must be "2.0"')
        return
    method = request.get("method")
    if not isinstance(method, str):
        respond_error(request_id, protocol.INVALID_REQUEST, "method must be a string")
        return
    handler = HANDLERS.get(method)
    if handler is None:
        respond_error(request_id, protocol.METHOD_NOT_FOUND, f"unknown method: {method}")
        return
    # `handshake` and `coverageMap` arrive with no `params` key at all
    # (`Request::empty`); a missing or null `params` is an empty object, never an error.
    params = request.get("params") or {}
    if not isinstance(params, dict):
        respond_error(request_id, protocol.INVALID_REQUEST, "params must be an object")
        return
    try:
        respond({"jsonrpc": protocol.JSONRPC_VERSION, "id": request_id, "result": handler(params)})
    except protocol.InvalidParams as problem:
        respond_error(request_id, protocol.INVALID_REQUEST, str(problem))
    except BaseException as problem:  # noqa: BLE001 - a plugin that dies produces findings that are not the target's
        respond_error(request_id, protocol.PLUGIN_ERROR, repr(problem))


def main():
    stdin = sys.stdin
    while True:
        try:
            line = stdin.readline()
        except (OSError, ValueError):
            return
        if line == "":
            return
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
        except ValueError:
            # A parse error is answered, not skipped: silence stalls the engine's waiting
            # request until its timeout and files the loss against a target. The id is null
            # because nothing readable was parsed; the engine discards unmatched ids.
            respond_error(None, protocol.PARSE_ERROR, "parse error")
            continue
        if not isinstance(request, dict):
            respond_error(None, protocol.INVALID_REQUEST, "request must be a JSON object")
            continue
        try:
            serve_request(request)
        except (OSError, ValueError) as problem:
            # The channel itself is gone (engine killed us, pipe closed). Stay quiet: there
            # is no one left to answer, and a traceback would only land on stderr.
            _log(problem)
            return


if __name__ == "__main__":
    main()

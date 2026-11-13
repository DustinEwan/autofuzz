"""`discover`: an `ast` inventory of the scan project, never an import of it.

Discovery reads declarations only (`docs/architecture.md#the-seven-methods`): importing user code to
enumerate it runs that code inside the plugin. A symbol the engine cannot build inputs
for is reported as `skipped` and never as a target, so `constructible + fixture-required
+ opaque == discovered` holds by construction (`packages/harness-js/src/discover.ts:407-411`).
"""

import ast
import fnmatch
import hashlib
import os
from pathlib import Path

import lower
import protocol

# The plugin's own default when the request carries no `include`.
DEFAULT_INCLUDE = ("**/*.py",)
# The plugin's own default when the request carries no `exclude`: directories no scan
# wants walked at all, whatever the scan project's language.
DEFAULT_EXCLUDE = ("**/__pycache__/**", "**/.venv/**", "**/site-packages/**")

# Decorators that mark a declaration as not a callable entry point.
NON_ENTRY_DECORATORS = frozenset({"property", "staticmethod", "classmethod", "abstractmethod", "overload"})


def target_id(module_path, name, signature):
    """The `packages/harness-js/src/discover.ts:650-653` digest, so ids match across processes."""
    digest = hashlib.sha256(f"{module_path}\0{name}\0{signature}".encode("utf-8")).hexdigest()
    return f"afz:t:{digest[:12]}"


def discover(req):
    """Return `(discovery, registry)`: the wire document and the call info per target id."""
    root_text = req.get("root")
    if root_text is None:
        root_text = "."
    if not isinstance(root_text, str):
        raise protocol.InvalidParams("root must be a string")
    include = req.get("include") or list(DEFAULT_INCLUDE)
    exclude = req.get("exclude") or list(DEFAULT_EXCLUDE)
    for patterns, what in ((include, "include"), (exclude, "exclude")):
        if not isinstance(patterns, list) or not all(isinstance(p, str) for p in patterns):
            raise protocol.InvalidParams(f"{what} must be an array of strings")

    root = Path(root_text).resolve()
    targets = []
    skipped = []
    registry = {}
    all_classes = {}

    for path in _select_files(root, include, exclude):
        module_path = str(path)
        try:
            source = path.read_text(encoding="utf-8")
            tree = ast.parse(source, filename=str(path))
        except (SyntaxError, ValueError, OSError) as problem:
            skipped.append(_parse_failure(module_path, problem))
            continue

        classes = _collect_classes(path, tree)
        for name, info in classes.items():
            all_classes.setdefault(name, info["module"])

        for node in tree.body:
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                candidates = [(node, None, None)]
            elif isinstance(node, ast.ClassDef) and not node.name.startswith("_"):
                candidates = [(m, node.name, classes.get(node.name)) for m in node.body if isinstance(m, (ast.FunctionDef, ast.AsyncFunctionDef))]
            else:
                continue
            for method, class_name, class_info in candidates:
                outcome = _analyze(path, module_path, method, class_name, class_info, classes, all_classes)
                if outcome is None:
                    continue
                if outcome["kind"] == "skipped":
                    skipped.append(outcome["entry"])
                else:
                    registry[outcome["descriptor"]["id"]] = outcome["callable"]
                    targets.append(outcome["descriptor"])

    _fill_unlocks(skipped)
    return {"targets": targets, "skipped": skipped}, registry


def _select_files(root, include, exclude):
    found = []
    for dirpath, _dirnames, filenames in os.walk(root):
        for filename in filenames:
            path = Path(dirpath, filename)
            try:
                rel = path.relative_to(root).as_posix()
            except ValueError:
                continue
            if not _matches_any(rel, include):
                continue
            if _matches_any(rel, exclude):
                continue
            found.append(path)
    return sorted(found)


def _matches_any(rel, patterns):
    """`**/` must be able to match zero directories, so a top-level file is tested both as
    `broken.py` and as `/broken.py`: the second form lets `**/*.py` match it under fnmatch."""
    return any(fnmatch.fnmatchcase(rel, pattern) or fnmatch.fnmatchcase("/" + rel, pattern) for pattern in patterns)


def _parse_failure(module_path, problem):
    text = str(problem).strip()
    detail = text.splitlines()[0] if text else repr(problem)
    return {
        "targetId": target_id(module_path, "parse", "syntaxerror"),
        "name": module_path,
        "reason": "discovery-error",
        "detail": detail,
    }


def _collect_classes(path, tree):
    """Module-level `ClassDef` facts: constructibility and the fields an `object` node carries."""
    classes = {}
    for node in tree.body:
        if not isinstance(node, ast.ClassDef) or node.name.startswith("_"):
            continue
        init = next((m for m in node.body if isinstance(m, ast.FunctionDef) and m.name == "__init__"), None)
        if init is None:
            # No constructor of its own: constructible only if no base class might demand
            # arguments, and the fields are the class-level annotations.
            constructible = not node.bases
            fields = [
                (stmt.target.id, stmt.annotation, stmt.value is None)
                for stmt in node.body
                if isinstance(stmt, ast.AnnAssign) and isinstance(stmt.target, ast.Name)
            ]
        else:
            rest = (init.args.posonlyargs or []) + (init.args.args or [])
            rest = rest[1:]  # `self`
            defaults = list(init.args.defaults or [])
            constructible = (
                init.args.vararg is None
                and init.args.kwarg is None
                and not init.args.kwonlyargs
                and len(defaults) >= len(rest)
            )
            annotations = {p.arg: p.annotation for p in rest}
            defaulted = {p.arg for p in rest[len(rest) - len(defaults) :]} if defaults else set()
            fields = []
            for stmt in ast.walk(init):
                field = _init_field(stmt, annotations, defaulted)
                if field is not None:
                    fields.append(field)
        classes[node.name] = {"module": path.stem, "constructible": constructible, "fields": fields}
    return classes


def _init_field(stmt, annotations, defaulted):
    """One `self.<attr>` write in `__init__`, as a field of the `object` node for the class."""
    target = value = annotation = None
    if isinstance(stmt, ast.Assign) and len(stmt.targets) == 1:
        target, value = stmt.targets[0], stmt.value
    elif isinstance(stmt, ast.AnnAssign):
        target, value, annotation = stmt.target, stmt.value, stmt.annotation
    if not isinstance(target, ast.Attribute) or not isinstance(target.value, ast.Name):
        return None
    if target.value.id != "self" or target.attr.startswith("_"):
        return None
    if annotation is not None:
        return (target.attr, annotation, value is None)
    if isinstance(value, ast.Name) and value.id in annotations:
        return (target.attr, annotations[value.id], value.id not in defaulted)
    return (target.attr, None, True)


def _analyze(path, module_path, node, class_name, class_info, classes, all_classes):
    """One function or method: a candidate target, a skip with its reason, or nothing."""
    display = f"{class_name}.{node.name}" if class_name else node.name

    if node.name.startswith("__") and node.name.endswith("__"):
        return None  # `__init__` builds the receiver; an operator hook is not an entry point.
    if node.name.startswith("_"):
        return _skip(node, module_path, display, "not-public", "the name starts with an underscore")

    decorators = _decorator_names(node) & NON_ENTRY_DECORATORS
    if decorators:
        return _skip(
            node, module_path, display, "unsupported-signature", f"@{sorted(decorators)[0]} is not a callable entry point"
        )

    params = _parameters(node, class_name, classes)
    if isinstance(params, str):
        return _skip(node, module_path, display, "unsupported-signature", params)

    lowered = [p["type"] for p in params]
    blockers = []
    for p in params:
        blockers.extend(_opaques(p["type"], p["name"]))
    if class_name is not None and (class_info is None or not class_info["constructible"]):
        blockers.append({"ty": class_name, "module": class_info["module"] if class_info else None, "path": "receiver"})

    ident = target_id(module_path, display, ", ".join(f"{p['name']}:{p['text']}" for p in params))

    materialization = "constructible"
    blocker = None
    if blockers:
        project_local = [b for b in blockers if b["ty"] in all_classes]
        if project_local:
            materialization = "fixture-required"
            blocker = project_local[0]
        else:
            materialization = "opaque"

    if materialization != "constructible":
        entry = {
            "targetId": ident,
            "name": display,
            "reason": "unconstructible-parameter",
            "materialization": materialization,
        }
        if blocker is not None:
            entry["blockedBy"] = {"type": blocker["ty"], "path": blocker["path"], "module": blocker.get("module")}
        else:
            entry["detail"] = "no constructible representation for " + ", ".join(sorted(b["ty"] for b in blockers))
        return {"kind": "skipped", "entry": entry}

    guard_sites = sorted({n.lineno for n in ast.walk(node) if isinstance(n, ast.Raise)})
    return {
        "kind": "target",
        "descriptor": {
            "id": ident,
            "targetClass": "design-time",
            "origin": {"file": module_path, "module": module_path, "line": node.lineno},
            "name": display,
            "parameters": [{"name": p["name"], "type": p["type"], "required": p["required"]} for p in params],
            "returns": lower.lower(node.returns, classes) if node.returns is not None else None,
            "effects": _effects(node, lowered),
            "materialization": materialization,
            "guardSites": guard_sites,
        },
        "callable": {
            "file": str(path),
            "modulePath": module_path,
            "class": class_name,
            "func": node.name,
            "guardSites": guard_sites,
        },
    }


def _skip(node, module_path, display, reason, detail):
    parts = []
    for p in (node.args.posonlyargs or []) + (node.args.args or []):
        parts.append(f"{p.arg}:{ast.unparse(p.annotation)}" if p.annotation is not None else f"{p.arg}:")
    return {
        "kind": "skipped",
        "entry": {
            "targetId": target_id(module_path, display, ", ".join(parts)),
            "name": display,
            "reason": reason,
            "detail": detail,
        },
    }


def _parameters(node, class_name, classes):
    """The lowered parameter list, or a string naming why the signature is uncallable."""
    args = node.args
    if args.vararg is not None or args.kwarg is not None:
        return "varargs are not representable"
    if args.kwonlyargs:
        return "keyword-only parameters are not representable"
    positional = (args.posonlyargs or []) + (args.args or [])
    if class_name is not None and positional:
        positional = positional[1:]  # `self` / `cls`: the receiver, not a parameter.
    defaults = list(args.defaults or [])
    first_default = len(positional) - len(defaults)
    params = []
    for index, p in enumerate(positional):
        if p.annotation is None:
            return "parameter has no annotation"
        params.append(
            {
                "name": p.arg,
                "text": ast.unparse(p.annotation),
                "type": lower.lower(p.annotation, classes),
                "required": index < first_default,
            }
        )
    return params


def _decorator_names(node):
    names = set()
    for d in node.decorator_list:
        target = d.func if isinstance(d, ast.Call) else d
        if isinstance(target, ast.Name):
            names.add(target.id)
        elif isinstance(target, ast.Attribute):
            names.add(target.attr)
    return names


def _effects(node, lowered):
    mutates = False
    for stmt in ast.walk(node):
        if isinstance(stmt, (ast.Global, ast.Nonlocal)):
            mutates = True
        elif isinstance(stmt, (ast.Assign, ast.AugAssign, ast.AnnAssign)):
            targets = stmt.targets if isinstance(stmt, ast.Assign) else [stmt.target]
            if any(isinstance(target, (ast.Attribute, ast.Subscript)) for target in targets):
                mutates = True
    return {
        "async": isinstance(node, ast.AsyncFunctionDef),
        "throws": any(isinstance(n, ast.Raise) for n in ast.walk(node)),
        "callbacks": any(lower.contains_kind(t, "function") for t in lowered),
        "streams": any(lower.contains_kind(t, "stream") for t in lowered),
        "mutatesState": mutates,
    }


def _opaques(node, path):
    """Every `opaque` node in a lowered type, each tagged with the parameter it sits at."""
    found = []

    def walk(node):
        if not isinstance(node, dict):
            return
        if node.get("kind") == "opaque":
            found.append({"ty": node["ty"], "module": node.get("module"), "path": path})
            return
        for key, value in node.items():
            if key in ("kind", "ty", "module"):
                continue
            if isinstance(value, list):
                for item in value:
                    walk(item)
            elif isinstance(value, dict):
                walk(value)

    walk(node)
    return found


def _fill_unlocks(skipped):
    """`unlocksTargets`: how many skipped symbols one missing fixture would free."""
    by_blocker = {}
    for entry in skipped:
        blocker = entry.get("blockedBy")
        if blocker is None:
            continue
        by_blocker.setdefault((blocker["type"], blocker.get("module")), []).append(entry)
    for entries in by_blocker.values():
        for entry in entries:
            entry["unlocksTargets"] = len(entries)

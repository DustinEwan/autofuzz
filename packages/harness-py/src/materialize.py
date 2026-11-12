"""Materialize a `ValueTree` (`docs/schema/value-tree.json`) into Python objects.

Two directions live here. `materialize` turns the engine's generated tree into the
arguments of one call; a node the harness cannot build raises `MaterializationError`,
the Python mirror of `packages/harness-js/src/materialize.ts:59-73`. `to_value_tree`
lowers a target's return value back onto the wire.
"""

import math


class MaterializationError(Exception):
    """A node the harness cannot turn into a value, in the shape `prepare` turns into a skip."""

    def __init__(self, blocked_by, detail):
        super().__init__(detail)
        self.reason = "unconstructible-parameter"
        self.blocked_by = blocked_by
        self.detail = detail

    @classmethod
    def opaque(cls, ty, path):
        return cls([{"type": ty, "path": path, "module": None}], f"cannot materialize opaque type {ty} at {path}")


class MalformedValueError(Exception):
    """The payload is not a `ValueTree`: a missing tag, an unknown node, a bad byte."""


class StubCallError(RuntimeError):
    """The target called a generated stub configured to throw; the fault is the harness's."""


class _Obj:
    """The instance an `object` node builds: attributes, so both `o.field` and `vars(o)` work."""


SPECIAL_NUMBERS = {"naN": float("nan"), "infinity": float("inf"), "negativeInfinity": float("-inf")}

# `to_value_tree` refuses to follow a cyclic return graph past this depth; the call
# answer then omits `value` rather than recursing until the process dies.
MAX_DEPTH = 8


def materialize(node, path="value", stub_calls=None):
    """One `ValueTree` node to a Python value. Raises `MaterializationError` or `MalformedValueError`."""
    if not isinstance(node, dict) or not isinstance(node.get("node"), str):
        raise MalformedValueError(f"{path} is not a ValueTree node")
    tag = node["node"]

    if tag == "null" or tag == "undefined":
        return None
    if tag == "boolean":
        return _field(node, "value", path)
    if tag == "number":
        value = _field(node, "value", path)
        if isinstance(value, str):
            if value not in SPECIAL_NUMBERS:
                raise MalformedValueError(f'{path} carries the number name "{value}"')
            return SPECIAL_NUMBERS[value]
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise MalformedValueError(f"{path} carries a non-number as number.value")
        return float(value)
    if tag == "bigInt":
        value = _field(node, "value", path)
        try:
            return int(value)
        except (TypeError, ValueError) as problem:
            raise MalformedValueError(f"{path} carries {value!r} as bigInt.value") from problem
    if tag == "string":
        return _field(node, "value", path)
    if tag == "literal":
        return _field(node, "value", path)
    if tag == "array":
        return [materialize(item, f"{path}[]", stub_calls) for item in _items(node, path)]
    if tag == "tuple":
        return tuple(materialize(item, f"{path}[]", stub_calls) for item in _items(node, path))
    if tag == "set":
        items = [materialize(item, f"{path}[]", stub_calls) for item in _items(node, path)]
        try:
            return set(items)
        except TypeError:
            # A frozenset is what the wire asked for and a set cannot hold: the items are
            # hashable-or-not the same way, so this is the last honest attempt.
            try:
                return frozenset(items)
            except TypeError as problem:
                raise MaterializationError(
                    [{"type": "set", "path": path, "module": None}], f"set at {path} holds unhashable items"
                ) from problem
    if tag == "map":
        entries = _field(node, "entries", path)
        if not isinstance(entries, list):
            raise MalformedValueError(f"{path}.entries is not an array")
        result = {}
        for entry in entries:
            if not isinstance(entry, list) or len(entry) != 2:
                raise MalformedValueError(f"{path}.entries carries a non-pair")
            key = materialize(entry[0], f"{path}.key", stub_calls)
            value = materialize(entry[1], f"{path}.value", stub_calls)
            try:
                result[key] = value
            except TypeError as problem:
                raise MaterializationError(
                    [{"type": "map key", "path": path, "module": None}], f"map at {path} has an unhashable key"
                ) from problem
        return result
    if tag == "object":
        fields = _field(node, "fields", path)
        if not isinstance(fields, list):
            raise MalformedValueError(f"{path}.fields is not an array")
        instance = _Obj()
        for field in fields:
            if not isinstance(field, list) or len(field) != 2 or not isinstance(field[0], str):
                raise MalformedValueError(f"{path}.fields carries a non-pair")
            setattr(instance, field[0], materialize(field[1], f"{path}.{field[0]}", stub_calls))
        return instance
    if tag == "buffer":
        raw = _field(node, "bytes", path)
        if not isinstance(raw, list) or not all(isinstance(b, int) and 0 <= b <= 255 for b in raw):
            raise MalformedValueError(f"{path}.bytes is not an array of uint8")
        return bytes(raw)
    if tag == "function":
        behavior = _field(node, "behavior", path)
        if behavior not in ("returnFirst", "throw", "captureArguments"):
            raise MalformedValueError(f"{path}.behavior is unknown: {behavior!r}")
        returns = node.get("returns")
        return _stub(behavior, returns, path, stub_calls)
    if tag == "stream":
        items = [materialize(item, f"{path}[]", stub_calls) for item in _items(node, path)]
        if not isinstance(node.get("iterable"), bool):
            raise MalformedValueError(f"{path}.iterable must be a boolean")
        if node["iterable"]:
            return (item for item in items)
        return iter(items)
    if tag == "opaque":
        raise MaterializationError.opaque(node.get("ty", "?"), path)
    raise MalformedValueError(f"{path} carries the unknown node tag {tag!r}")


def materialize_values(values, stub_calls=None):
    if not isinstance(values, list):
        raise MalformedValueError("values must be an array")
    return [materialize(node, f"values[{index}]", stub_calls) for index, node in enumerate(values)]


def _stub(behavior, returns, path, stub_calls):
    built_returns = None if returns is None else materialize(returns, f"{path}.returns", stub_calls)

    def stub(*args, **kwargs):
        if behavior == "returnFirst":
            return args[0] if args else None
        if behavior == "throw":
            raise StubCallError("afz: stub called")
        if stub_calls is not None:
            stub_calls.append((args, kwargs))
        return built_returns

    return stub


def _field(node, name, path):
    if name not in node:
        raise MalformedValueError(f"{path} is missing {name}")
    return node[name]


def _items(node, path):
    items = _field(node, "items", path)
    if not isinstance(items, list):
        raise MalformedValueError(f"{path}.items is not an array")
    return items


def to_value_tree(value, depth=0):
    """Lower a Python return value to a `ValueTree`, or `None` meaning "omit the value"."""
    if depth > MAX_DEPTH:
        return None
    if value is None:
        return {"node": "null"}
    if isinstance(value, bool):
        return {"node": "boolean", "value": value}
    if isinstance(value, int):
        return {"node": "bigInt", "value": str(value)}
    if isinstance(value, float):
        if math.isnan(value):
            return {"node": "number", "value": "naN"}
        if math.isinf(value):
            return {"node": "number", "value": "infinity" if value > 0 else "negativeInfinity"}
        return {"node": "number", "value": value}
    if isinstance(value, str):
        return {"node": "string", "value": value}
    if isinstance(value, (bytes, bytearray)):
        return {"node": "buffer", "bytes": list(value)}
    if isinstance(value, (list, tuple)):
        items = [to_value_tree(item, depth + 1) for item in value]
        if any(item is None for item in items):
            return None
        return {"node": "array", "items": items}
    if isinstance(value, (set, frozenset)):
        items = [to_value_tree(item, depth + 1) for item in value]
        if any(item is None for item in items):
            return None
        return {"node": "set", "items": items}
    if isinstance(value, dict):
        entries = []
        for key, item in value.items():
            key_tree = to_value_tree(key, depth + 1)
            item_tree = to_value_tree(item, depth + 1)
            if key_tree is None or item_tree is None:
                return None
            entries.append([key_tree, item_tree])
        return {"node": "map", "entries": entries}
    return None

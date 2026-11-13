"""Lower one Python annotation to a `TypeNode` (`docs/schema/type-node.json`).

The input is the annotation's AST node, never an evaluated object: discovery must not
import the target (`docs/architecture.md#the-seven-methods`). `classes` maps a bare class name to the
`ClassDef` facts gathered from the scan root, because a parameter annotated with a
project-local class is the only annotation this module cannot decide alone: it becomes an
`object` when the constructor is defaulted and an `opaque` otherwise.
"""

import ast

_ARRAY = frozenset({"list", "List", "Sequence", "Iterable", "MutableSequence", "Collection"})
_SET = frozenset({"set", "Set", "frozenset", "FrozenSet", "AbstractSet"})
_MAP = frozenset({"dict", "Dict", "Mapping", "MutableMapping", "OrderedDict", "defaultdict"})
_STREAM = frozenset({"Iterator", "AsyncIterator", "Generator", "AsyncGenerator", "Iterable"})
# Names that mean "a container of anything"; the element lowers to opaque.
_BARE = _ARRAY | _SET | _MAP | _STREAM


def scalar(kind):
    return {"kind": "scalar", "scalar": kind}


def opaque(ty, module=None):
    node = {"kind": "opaque", "ty": ty}
    if module is not None:
        node["module"] = module
    return node


def _names(node):
    """The dotted-name text of a bare name or attribute reference, e.g. `decimal.Decimal`."""
    if isinstance(node, ast.Name):
        return node.id, None
    if isinstance(node, ast.Attribute) and isinstance(node.value, ast.Name):
        return node.attr, node.value.id
    return None, None


def _subscript_base(node):
    base, module = _names(node.value)
    return base, module


def _args_of(node):
    slice_ = node.slice
    if isinstance(slice_, ast.Tuple):
        return list(slice_.elts)
    return [slice_]


def _ellipsis(node):
    return isinstance(node, ast.Constant) and node.value is Ellipsis


def lower(node, classes, stack=()):
    """Lower an annotation expression node to a `TypeNode` dict.

    `stack` holds the class names currently being lowered, so a self-referential class
    emits its `object` once with an `id` and `ref` nodes for the recursion.
    """
    # A string annotation is a forward reference: lower what it says.
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        try:
            node = ast.parse(node.value, mode="eval").body
        except SyntaxError:
            return opaque(node.value)

    if node is None or isinstance(node, ast.Constant) and node.value is None:
        return scalar("null")

    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        members = []
        for side in _flatten_union(node):
            members.append(lower(side, classes, stack))
        return {"kind": "composite", "composite": "union", "members": members}

    if isinstance(node, ast.Subscript):
        return _lower_subscript(node, classes, stack)

    name, module = _names(node)
    if name is None:
        return opaque(ast.unparse(node))

    if name == "bool":
        return scalar("boolean")
    if name == "int":
        return scalar("bigInt")
    if name == "float":
        return scalar("number")
    if name == "str":
        return scalar("string")
    if name in ("bytes", "bytearray", "memoryview"):
        return {"kind": "buffer"}
    if name in ("complex", "Decimal"):
        return opaque(name, module)
    if name in _BARE:
        if name in _ARRAY:
            return {"kind": "collection", "collection": "array", "type": opaque("Any")}
        if name in _SET:
            return {"kind": "collection", "collection": "set", "type": opaque("Any")}
        if name in _MAP:
            return {
                "kind": "collection",
                "collection": "map",
                "key": opaque("Any"),
                "type": opaque("Any"),
            }
        return {"kind": "stream", "iterable": True, "type": opaque("Any"), "maxItems": None}
    if name in classes:
        return _lower_class_ref(name, classes, stack)
    # `Any`, `object`, a stdlib class, a TypeVar: nothing the generator can build.
    return opaque(name, module)


def _flatten_union(node):
    parts = []
    while isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr):
        parts.append(node.right)
        node = node.left
    parts.append(node)
    return parts


def _lower_subscript(node, classes, stack):
    base, _module = _subscript_base(node)
    args = _args_of(node)
    text = ast.unparse(node)

    if base in ("Optional",):
        return {
            "kind": "composite",
            "composite": "union",
            "members": [lower(args[0], classes, stack), scalar("null")],
        }
    if base in ("Union",):
        return {"kind": "composite", "composite": "union", "members": [lower(a, classes, stack) for a in args]}
    if base == "Literal":
        return {"kind": "literal", "values": [a.value for a in args if isinstance(a, ast.Constant)]}
    if base in _ARRAY:
        return {"kind": "collection", "collection": "array", "type": lower(args[0], classes, stack)}
    if base in _SET:
        return {"kind": "collection", "collection": "set", "type": lower(args[0], classes, stack)}
    if base in _MAP:
        return {
            "kind": "collection",
            "collection": "map",
            "key": lower(args[0], classes, stack),
            "type": lower(args[1], classes, stack),
        }
    if base in ("tuple", "Tuple"):
        # `tuple[A, ...]` is a homogeneous tuple: an array of A.
        if len(args) == 2 and _ellipsis(args[1]):
            return {"kind": "collection", "collection": "array", "type": lower(args[0], classes, stack)}
        return {"kind": "tuple", "items": [lower(a, classes, stack) for a in args]}
    if base in ("Callable", "Awaitable", "Coroutine", "AsyncMock"):
        if base == "Callable":
            returns = args[-1] if args else None
            result = scalar("null") if returns is None or _ellipsis(returns) else lower(returns, classes, stack)
            return {"kind": "function", "type": result}
        if len(args) >= 3:
            result = lower(args[-1], classes, stack)
        elif args:
            result = lower(args[0], classes, stack)
        else:
            result = scalar("null")
        return {"kind": "promise", "type": result}
    if base in _STREAM:
        element = lower(args[0], classes, stack) if args else opaque("Any")
        return {"kind": "stream", "iterable": True, "type": element, "maxItems": None}
    return opaque(text)


def _lower_class_ref(name, classes, stack):
    info = classes[name]
    if name in stack:
        return {"kind": "ref", "name": name}
    if not info["constructible"]:
        return opaque(name, info["module"])
    fields = []
    for field_name, field_node, required in info["fields"]:
        field_type = (
            lower(field_node, classes, stack + (name,))
            if field_node is not None
            else opaque("Any")
        )
        fields.append({"name": field_name, "type": field_type, "required": required})
    return {"kind": "object", "name": name, "id": name, "fields": fields}


def contains_kind(node, kind):
    """Whether a lowered `TypeNode` tree contains a node of `kind` (effects: callbacks, streams)."""
    if not isinstance(node, dict):
        return False
    if node.get("kind") == kind:
        return True
    return any(contains_kind(value, kind) for value in node.values())

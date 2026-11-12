"""The wire vocabulary of `afz/1`, in one file and nowhere else.

Every literal the plugin sends or reads over the protocol lives here, and
`test/contract_test.py` compares each one against `docs/schema/*.json` and the
plugin interface in `docs/architecture.md`. Nothing in this file has behavior:
framing lives in `server.py`, the classification rule in `classify.py`.
"""

# --- framing -----------------------------------------------------------------

JSONRPC_VERSION = "2.0"

# Error codes (`crates/afz-protocol/src/rpc.rs`, restated in docs/harness-authoring.md §3).
PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
PLUGIN_ERROR = -32000

ERROR_CODES = {
    "parse": PARSE_ERROR,
    "invalid_request": INVALID_REQUEST,
    "method_not_found": METHOD_NOT_FOUND,
    "plugin": PLUGIN_ERROR,
}


class InvalidParams(Exception):
    """The request's `params` are not the shape `docs/schema/` describes: `-32600`."""

PROTOCOL_VERSION = "afz/1"
HARNESS_NAME = "python"
HARNESS_VERSION = "0.1.0"

# The seven wire names of `crates/afz-protocol/src/rpc.rs` `Method::name`.
METHOD_NAMES = ("handshake", "discover", "prepare", "invoke", "minimize", "reproducer", "coverageMap")

METHOD_HANDSHAKE = "handshake"
METHOD_DISCOVER = "discover"
METHOD_PREPARE = "prepare"
METHOD_INVOKE = "invoke"
METHOD_MINIMIZE = "minimize"
METHOD_REPRODUCER = "reproducer"
METHOD_COVERAGE_MAP = "coverageMap"

# --- closed vocabularies -------------------------------------------------------

# `docs/schema/handshake.json` $defs.Capability. Only `coverage` and `reproducer` change engine
# behavior today; this harness declares exactly those two.
CAPABILITIES = ("coverage", "minimize", "reproducer", "sideEffects", "stateful", "concurrent")

# `docs/schema/skipped-target.json` $defs.SkipReason.
SKIP_REASONS = (
    "unconstructible-parameter",
    "discovery-error",
    "not-public",
    "excluded",
    "unsupported-signature",
    "target-disabled",
)

# `docs/schema/target-descriptor.json` $defs.Materialization.
MATERIALIZATIONS = ("constructible", "fixture-required", "opaque")

# `docs/schema/type-node.json` oneOf tags.
TYPE_NODE_KINDS = (
    "scalar",
    "stringFormat",
    "literal",
    "collection",
    "object",
    "tuple",
    "composite",
    "buffer",
    "promise",
    "function",
    "stream",
    "opaque",
    "ref",
)

# `docs/schema/value-tree.json` oneOf tags.
VALUE_NODE_TAGS = (
    "null",
    "undefined",
    "boolean",
    "number",
    "bigInt",
    "string",
    "literal",
    "array",
    "set",
    "map",
    "object",
    "tuple",
    "buffer",
    "function",
    "stream",
    "opaque",
)

SCALAR_KINDS = ("null", "undefined", "boolean", "number", "bigInt", "string")
COLLECTION_KINDS = ("array", "set", "map")
COMPOSITE_KINDS = ("union", "intersection")
FUNCTION_BEHAVIORS = ("returnFirst", "throw", "captureArguments")

# serde's camelCase spellings of Rust's `NaN`/`Infinity`/`NegativeInfinity`. They
# read like typos; the contract test fails if either side "fixes" one.
SPECIAL_NUMBERS = ("naN", "infinity", "negativeInfinity")

OUTCOME_KINDS = ("ok", "rejected", "failed")

# --- shapes this harness emits -------------------------------------------------
# Each set lists every key the plugin may write for that object; the contract
# test asserts each is a subset of the committed schema's `properties`.

TARGET_DESCRIPTOR_KEYS = frozenset(
    {"id", "targetClass", "origin", "name", "parameters", "returns", "effects", "materialization", "guardSites"}
)
SKIPPED_TARGET_KEYS = frozenset(
    {"targetId", "name", "reason", "detail", "materialization", "blockedBy", "unlocksTargets"}
)
COVERAGE_ENTRY_KEYS = frozenset({"edge", "url", "start", "end"})
FAULT_SIGNATURE_KEYS = frozenset({"class", "message", "frames"})
# `column` is a schema property this harness never fills: Python tracebacks carry no column.
FAULT_FRAME_KEYS = frozenset({"file", "line", "fnName"})
EFFECTS_KEYS = frozenset({"async", "throws", "streams", "callbacks", "mutatesState"})
ORIGIN_KEYS = frozenset({"file", "module", "line"})
PARAMETER_KEYS = frozenset({"name", "type", "required"})

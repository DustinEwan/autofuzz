"""The wire-vocabulary pin, the Python twin of `packages/harness-js/test/contract.test.mjs`.

The two sides of the plugin boundary are written in different languages and built
separately. This file is where they are checked against each other: every literal
`src/protocol.py` sends or reads is compared with the JSON Schema committed under
`docs/schema/`, and the method vocabulary and `PROTOCOL_VERSION` are compared with
the prose in `docs/architecture.md` and `docs/harness-authoring.md` — the only
documents that pin the protocol-version string itself.

Run with the stdlib runner, no pytest:

    python3 -m unittest discover -s packages/harness-py/test -t packages/harness-py
"""

import json
import re
import sys
import unittest
from pathlib import Path

package_root = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(package_root / "src"))

import protocol  # noqa: E402  (the import needs the sys.path insert above)

repo_root = package_root.parents[1]
schema_dir = repo_root / "docs" / "schema"
design = repo_root / "docs" / "architecture.md"
guide = repo_root / "docs" / "harness-authoring.md"


def schema(name):
    return json.loads((schema_dir / name).read_text(encoding="utf8"))


def literals(spec, what):
    """The literal members of a union, sorted: schemars emits `enum` or `oneOf` of bare consts."""
    if isinstance(spec.get("enum"), list):
        return sorted(spec["enum"])
    members = spec.get("oneOf")
    if not isinstance(members, list):
        raise AssertionError(f"{what} is neither an enum nor a oneOf")
    values = []
    for variant in members:
        value = variant.get("const")
        if not isinstance(value, str):
            raise AssertionError(f"{what} member carries no const string")
        values.append(value)
    return sorted(values)


def tags(document, field, what):
    """The tag values of a tagged union, sorted, read from the discriminator property."""
    values = []
    for variant in document["oneOf"]:
        value = variant.get("properties", {}).get(field, {}).get("const")
        if not isinstance(value, str):
            raise AssertionError(f'{what} variant has no "{field}" const')
        values.append(value)
    return sorted(values)


class WireVocabulary(unittest.TestCase):
    def test_protocol_version_matches_the_guide(self):
        # docs/schema/handshake.json types protocolVersion as a plain string and carries no
        # value, so the guide is the only document that pins it. A guide that drifts from
        # crates/afz-protocol fails here.
        text = re.sub(r"\s+", " ", guide.read_text(encoding="utf8"))
        stated = re.search(r'`PROTOCOL_VERSION` is `"([^"]+)"', text)
        self.assertIsNotNone(stated, "the guide no longer states the PROTOCOL_VERSION literal")
        self.assertEqual(protocol.PROTOCOL_VERSION, stated.group(1))

    def test_error_codes_match_the_guide(self):
        phrases = {
            "parse": "parse error",
            "invalid_request": "invalid request",
            "method_not_found": "method not found",
            "plugin": "plugin error",
        }
        text = re.sub(r"\s+", " ", guide.read_text(encoding="utf8"))
        self.assertEqual(sorted(protocol.ERROR_CODES), sorted(phrases))
        for key, code in protocol.ERROR_CODES.items():
            self.assertIn(f"`{code}` {phrases[key]}", text, f"the guide no longer names error code {code}")

    def test_the_plugin_answers_exactly_the_methods_the_design_declares(self):
        text = design.read_text(encoding="utf8")
        start = text.index("export interface HarnessPlugin {")
        block = text[start : text.index("\n}", start)]
        declared = re.findall(r"^  ([A-Za-z]+)\??\(", block, re.MULTILINE)
        self.assertGreaterEqual(len(declared), len(protocol.METHOD_NAMES), "no plugin interface found in the design")
        self.assertEqual(sorted(protocol.METHOD_NAMES), sorted(declared))

    def test_skip_reasons_match_the_generated_schema(self):
        self.assertEqual(
            sorted(protocol.SKIP_REASONS),
            literals(schema("skipped-target.json")["$defs"]["SkipReason"], "SkipReason"),
        )

    def test_materializations_match_the_generated_schema(self):
        self.assertEqual(
            sorted(protocol.MATERIALIZATIONS),
            literals(schema("target-descriptor.json")["$defs"]["Materialization"], "Materialization"),
        )

    def test_capabilities_match_the_generated_schema(self):
        self.assertEqual(
            sorted(protocol.CAPABILITIES),
            literals(schema("handshake.json")["$defs"]["Capability"], "Capability"),
        )

    def test_type_node_kinds_match_the_generated_schema(self):
        self.assertEqual(sorted(protocol.TYPE_NODE_KINDS), tags(schema("type-node.json"), "kind", "TypeNode"))

    def test_value_node_tags_match_the_generated_schema(self):
        self.assertEqual(sorted(protocol.VALUE_NODE_TAGS), tags(schema("value-tree.json"), "node", "ValueNode"))

    def test_scalar_collection_and_composite_sub_kinds_match_the_schema(self):
        type_node = schema("type-node.json")["$defs"]
        self.assertEqual(sorted(protocol.SCALAR_KINDS), literals(type_node["ScalarKind"], "ScalarKind"))
        self.assertEqual(sorted(protocol.COLLECTION_KINDS), literals(type_node["CollectionKind"], "CollectionKind"))
        self.assertEqual(sorted(protocol.COMPOSITE_KINDS), literals(type_node["CompositeKind"], "CompositeKind"))
        self.assertEqual(
            sorted(protocol.FUNCTION_BEHAVIORS), literals(type_node["FunctionBehavior"], "FunctionBehavior")
        )

    def test_the_number_names_the_wire_carries_are_the_serde_spellings(self):
        self.assertEqual(sorted(protocol.SPECIAL_NUMBERS), ["infinity", "naN", "negativeInfinity"])
        self.assertEqual(
            literals(schema("value-tree.json")["$defs"]["SpecialNumber"], "SpecialNumber"),
            sorted(protocol.SPECIAL_NUMBERS),
        )

    def test_outcome_required_fields_are_what_the_engine_reads(self):
        outcome = schema("outcome.json")
        self.assertEqual(sorted(protocol.OUTCOME_KINDS), tags(outcome, "kind", "Outcome"))
        self.assertEqual(outcome["$defs"]["RejectedOutcome"]["required"], ["durationMs", "via", "detail"])
        self.assertEqual(outcome["$defs"]["FailedOutcome"]["required"], ["durationMs", "via", "fault"])
        self.assertEqual(outcome["$defs"]["FaultSignature"]["required"], ["class", "message"])

    def test_prepare_reports_ok_with_a_handle_or_a_bare_skip_reason(self):
        prepare = schema("prepare-resp.json")
        ok, skip = prepare["oneOf"]
        self.assertEqual(ok["required"], ["ok", "materialized"])
        self.assertEqual(skip["required"], ["ok", "reason"])
        self.assertEqual(sorted(prepare["$defs"]["MaterializedCall"]["properties"]), ["handle", "summary"])


class EmittedShapes(unittest.TestCase):
    """Every key the plugin writes must exist in the committed schema for that shape."""

    def _assert_subset(self, emitted, document_name, path, what):
        document = schema(document_name)
        node = document
        for part in path:
            node = node[part]
        unknown = emitted - set(node["properties"])
        self.assertEqual(unknown, set(), f"{what} emits keys docs/schema/{document_name} does not define: {unknown}")

    def test_target_descriptor_keys(self):
        self._assert_subset(protocol.TARGET_DESCRIPTOR_KEYS, "target-descriptor.json", [], "TargetDescriptor")

    def test_target_descriptor_required_fields_are_all_emitted(self):
        required = schema("target-descriptor.json")["required"]
        missing = set(required) - protocol.TARGET_DESCRIPTOR_KEYS
        self.assertEqual(missing, set(), "TargetDescriptor would omit required fields")

    def test_skipped_target_keys(self):
        self._assert_subset(protocol.SKIPPED_TARGET_KEYS, "skipped-target.json", [], "SkippedTarget")

    def test_coverage_entry_keys(self):
        self._assert_subset(
            protocol.COVERAGE_ENTRY_KEYS, "coverage-map.json", ["$defs", "CoverageEntry"], "CoverageEntry"
        )

    def test_fault_signature_and_frame_keys(self):
        self._assert_subset(protocol.FAULT_SIGNATURE_KEYS, "outcome.json", ["$defs", "FaultSignature"], "FaultSignature")
        self._assert_subset(protocol.FAULT_FRAME_KEYS, "outcome.json", ["$defs", "FaultFrame"], "FaultFrame")

    def test_effects_origin_and_parameter_keys(self):
        self._assert_subset(protocol.EFFECTS_KEYS, "target-descriptor.json", ["$defs", "Effects"], "Effects")
        self._assert_subset(protocol.ORIGIN_KEYS, "target-descriptor.json", ["$defs", "Origin"], "Origin")
        self._assert_subset(protocol.PARAMETER_KEYS, "target-descriptor.json", ["$defs", "Parameter"], "Parameter")


if __name__ == "__main__":
    unittest.main()

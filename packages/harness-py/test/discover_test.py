"""Discovery behavior on concrete input: the skip reasons and the id rule.

The `discovery-error` reason is not reachable from the `pricing` fixture, whose
files all parse, so this file builds a tree that is not parseable and asserts the
inventory reports it instead of dropping it.
"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

import discover  # noqa: E402


def run(root, **req):
    discovery, _registry = discover.discover({"root": str(root), **req})
    return discovery


class Inventory(unittest.TestCase):
    def fixture(self):
        return Path(__file__).resolve().parents[1] / "test-targets" / "pricing"

    def test_pricing_yields_the_five_entry_points(self):
        discovery = run(self.fixture())
        self.assertEqual(
            sorted(target["name"] for target in discovery["targets"]),
            ["Invoice.add_line", "Invoice.total", "apply_discount", "spin", "tiered_price"],
        )

    def test_pricing_reports_one_skip_per_reachable_reason(self):
        discovery = run(self.fixture())
        by_name = {entry["name"]: entry for entry in discovery["skipped"]}
        self.assertEqual(by_name["untyped"]["reason"], "unsupported-signature")
        self.assertEqual(by_name["untyped"]["detail"], "parameter has no annotation")
        self.assertEqual(by_name["_private"]["reason"], "not-public")
        self.assertEqual(by_name["post"]["reason"], "unconstructible-parameter")
        self.assertEqual(by_name["post"]["materialization"], "fixture-required")
        self.assertEqual(by_name["post"]["blockedBy"], {"type": "Ledger", "path": "ledger", "module": "totals"})
        self.assertEqual(by_name["post"]["unlocksTargets"], 1)

    def test_only_constructible_symbols_are_targets(self):
        discovery = run(self.fixture())
        self.assertTrue(all(t["materialization"] == "constructible" for t in discovery["targets"]))
        self.assertEqual(len(discovery["targets"]), 5)
        self.assertEqual(len(discovery["skipped"]), 3)

    def test_ids_are_stable_across_two_discoveries(self):
        first, _ = discover.discover({"root": str(self.fixture())})
        second, _ = discover.discover({"root": str(self.fixture())})
        self.assertEqual(
            sorted(t["id"] for t in first["targets"]), sorted(t["id"] for t in second["targets"])
        )
        for target in first["targets"]:
            self.assertTrue(target["id"].startswith("afz:t:"))

    def test_guard_sites_name_the_raise_line(self):
        discovery = run(self.fixture())
        by_name = {target["name"]: target for target in discovery["targets"]}
        self.assertEqual(len(by_name["apply_discount"]["guardSites"]), 1)
        self.assertEqual(by_name["tiered_price"]["guardSites"], [])

    def test_unparseable_file_is_a_discovery_error_skip(self):
        with tempfile.TemporaryDirectory() as tmp:
            broken = Path(tmp) / "broken.py"
            broken.write_text("def broken(:\n", encoding="utf8")
            discovery = run(tmp)
        self.assertEqual(discovery["targets"], [])
        self.assertEqual(len(discovery["skipped"]), 1)
        entry = discovery["skipped"][0]
        self.assertEqual(entry["reason"], "discovery-error")
        self.assertTrue(entry["targetId"].startswith("afz:t:"))
        self.assertTrue(entry["detail"])

    def test_empty_include_falls_back_to_python_files(self):
        discovery = run(self.fixture(), include=[], exclude=[])
        self.assertEqual(len(discovery["targets"]), 5)

    def test_exclude_drops_a_module(self):
        discovery = run(self.fixture(), exclude=["**/invoice.py"])
        self.assertTrue(all(not t["origin"]["file"].endswith("invoice.py") for t in discovery["targets"]))


if __name__ == "__main__":
    unittest.main()

"""The rendered reproducer is proven by running it, not by schema-validating it.

The engine stores the plugin's `reproducer` answer verbatim, so a script that parses
the wrong way, or not at all, is invisible to every check that runs before a triager
does. Three separate bugs in this renderer (a dropped module-construction line, an
f-string that interpolated a loop variable, a `#` comment that swallowed a call's
closing paren) all passed schema validation; only executing the output caught them.
These tests render against the package's own fixture and run the result with this
same interpreter.
"""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

import repro  # noqa: E402

FIXTURE = Path(__file__).resolve().parents[1] / "test-targets" / "pricing" / "src"


def run(script):
    return subprocess.run([sys.executable, "-c", script], capture_output=True, text=True)


class ReproTest(unittest.TestCase):
    def test_every_node_tag_renders_to_python_that_parses(self):
        """One value of every tag `materialize.py` accepts, in one call. This is the
        assertion that fails when a rendering emits a line comment inside the call,
        an unterminated bracket, or an interpolated name the generator never defined."""
        values = [
            {"node": "null"},
            {"node": "undefined"},
            {"node": "boolean", "value": True},
            {"node": "number", "value": 1.5},
            {"node": "number", "value": "naN"},
            {"node": "number", "value": "negativeInfinity"},
            {"node": "bigInt", "value": "123"},
            {"node": "string", "value": "a\nb\"c"},
            {"node": "literal", "value": {"x": [1, None, True]}},
            {"node": "array", "items": [{"node": "string", "value": "i"}]},
            {"node": "tuple", "items": [{"node": "boolean", "value": False}]},
            {"node": "set", "items": []},
            {"node": "map", "entries": [[{"node": "string", "value": "k"}, {"node": "number", "value": -0.0}]]},
            {"node": "object", "fields": [["__proto__", {"node": "literal", "value": 1}]]},
            {"node": "buffer", "bytes": [0, 65, 34, 255]},
            {"node": "function", "behavior": "returnFirst"},
            {"node": "function", "behavior": "throw"},
            {"node": "function", "behavior": "captureArguments", "returns": {"node": "null"}},
            {"node": "stream", "iterable": True, "items": [{"node": "null"}]},
            {"node": "stream", "iterable": False, "items": []},
            {"node": "opaque", "ty": "Ledger"},
        ]
        script = repro.render("afz:t:all", str(FIXTURE / "tiering.py"), "tiered_price", None, values)["script"]
        compile(script, "repro", "exec")

    def test_rendered_script_replays_the_targets_fault(self):
        """The artifact a triager runs must fail the way the finding did, from the
        target's file — a script that exits 0 or faults in the renderer itself is
        worthless no matter how well it validates."""
        resp = repro.render(
            "afz:t:idx", str(FIXTURE / "tiering.py"), "tiered_price", None,
            [{"node": "bigInt", "value": "0"}, {"node": "array", "items": []}],
        )
        self.assertEqual(resp["extension"], "py")
        done = run(resp["script"])
        self.assertNotEqual(done.returncode, 0, done.stdout)
        self.assertIn("IndexError: list index out of range", done.stderr)
        self.assertIn("tiering.py", done.stderr)

    def test_method_receiver_is_constructible_by_the_script_alone(self):
        """`Invoice()` with no arguments is what the harness's constructible rule
        promises; the script has to stand on that promise with no harness imports."""
        resp = repro.render(
            "afz:t:m", str(FIXTURE / "invoice.py"), "Invoice.add_line", "Invoice",
            [{"node": "string", "value": "lab"}, {"node": "number", "value": 1.5}],
        )
        done = run(resp["script"])
        self.assertEqual(done.returncode, 0, done.stderr)

    def test_throw_stub_reproduces_the_stub_fault(self):
        """A finding whose callback was invoked must replay that invocation: the
        script's stub has to raise the same error the harness's `_stub` raises."""
        with tempfile.NamedTemporaryFile("w", suffix=".py", delete=False) as handle:
            handle.write("def use(cb):\n    return cb(1)\n")
            module = handle.name
        try:
            resp = repro.render(
                "afz:t:cb", module, "use", None, [{"node": "function", "behavior": "throw"}],
            )
            done = run(resp["script"])
            self.assertNotEqual(done.returncode, 0, done.stdout)
            self.assertIn("afz: stub called", done.stderr)
        finally:
            Path(module).unlink()


if __name__ == "__main__":
    unittest.main()

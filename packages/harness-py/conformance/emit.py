#!/usr/bin/env python3
"""Render the conformance fixture project for this harness.

`afz validate-harness` and `afz-xtask check-harness` validate a plugin against the template
(`crates/afz-conformance/template/v1.json`): ten semantic properties stated as wire-level
expectations. This emitter writes a small package that realizes every property, the
`autofuzz.json` that points a plugin at it, and `afz-conformance-manifest.json` naming the
symbol behind each property. The suite, not this file, decides pass/fail: it drives the
rendered project through the protocol and judges what the plugin answers.

Usage:
    python3 packages/harness-py/conformance/emit.py --out /tmp/conformance-py

Every property is realized here, so the rendered project's applicability set is the golden
the CI gate asserts: zero not-applicable lines for python.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2].parent
SERVER = REPO_ROOT / "packages" / "harness-py" / "src" / "server.py"

# The fixture. Every symbol carries a docstring naming the template property it realizes,
# so a reader of a rendered tree can trace a FAIL line back to the code that caused it.
PRICING_PY = '''"""Rendered by packages/harness-py/conformance/emit.py - do not edit by hand."""

import time


class Ledger:
    """A store handle the harness cannot build: `__init__` requires a path."""

    def __init__(self, path: str):
        self.path = path


class Invoice:
    """A receiver the harness can construct with no arguments."""

    def __init__(self):
        self.lines = []

    def add_line(self, amount: int) -> int:
        """method-receiver: `prepare` must build this receiver and answer ok."""
        self.lines.append(amount)
        return len(self.lines)


def tiered_price(quantity: int) -> int:
    """crash-unguarded, coverage-digest-live, reproducer-script.

    Two branches the coverage inputs take separately; the index past the end is the
    unguarded crash the suite drives with quantity 5.
    """
    if quantity < 2:
        tiers = [10, 20]
    else:
        tiers = [30, 40, 50]
    return tiers[quantity]


def apply_discount(amount: int) -> float:
    """guard-refusal: the out-of-range input raises ValueError on a guard line."""
    if amount < 0 or amount > 100:
        raise ValueError("amount out of range")
    return amount * 0.9


def _cache_key(quantity: int) -> str:
    """private-symbol: the underscore must land in `skipped` as `not-public`."""
    return f"tier:{quantity}"


def untyped(x):
    """unrepresentable-symbol: no annotation, so the generator has nothing to draw from."""
    return x


def post(ledger: Ledger) -> str:
    """unconstructible-receiver: blocked by Ledger, whose constructor needs a path."""
    return ledger.path


def noisy(message: str) -> str:
    """stdout-purity: writes to stdout first; the protocol channel must survive it."""
    print(message)
    return message


def spin(seconds: float) -> str:
    """deadline-sleep: sleeps far past any deadline the suite passes."""
    time.sleep(seconds)
    return "done"
'''

AUTOFUZZ_JSON = {
    "project": "conformance-python",
    "codebase": {
        "harness": "python",
        # Absolute: the suite spawns the plugin with its own cwd, not the rendered project's.
        "command": ["python3", str(SERVER)],
        "include": ["**/*.py"],
        "exclude": ["**/__pycache__/**"],
    },
    "oracles": {},
    "budget": {"invocations": 2000, "wallClockMs": 60000, "workers": 0, "maxCorpus": 256, "strategy": "guided"},
    "output": {"dir": ".autofuzz", "renderers": ["json"]},
}


def value(node: dict) -> dict:
    """One ValueTree in wire form; the suite sends it verbatim as `values[]`."""
    node["provenance"] = "user"
    return node


def number(n) -> dict:
    return value({"node": "number", "value": n})


def big_int(n: int) -> dict:
    # JSON numbers arrive as f64 and materialize as Python floats; a float subscript raises
    # TypeError (a reject class), not IndexError. Int-typed inputs travel as bigInt.
    return value({"node": "bigInt", "value": str(n)})


def string(s: str) -> dict:
    return value({"node": "string", "value": s})


MANIFEST = {
    "templateVersion": 1,
    "properties": {
        "crash-unguarded": {"symbol": "tiered_price", "input": [big_int(5)]},
        "guard-refusal": {"symbol": "apply_discount", "input": [number(-5)]},
        "private-symbol": {"symbol": "_cache_key"},
        "unrepresentable-symbol": {"symbol": "untyped"},
        "unconstructible-receiver": {"symbol": "post", "blockedByType": "Ledger"},
        "method-receiver": {"symbol": "Invoice.add_line", "input": [big_int(1)]},
        # 0 takes the first branch and indexes clean; 2 takes the second. Same function,
        # different edges: a dead or never-reset probe answers identical digests and fails.
        "coverage-digest-live": {"symbol": "tiered_price", "inputs": [[big_int(0)], [big_int(2)]]},
        "stdout-purity": {"symbol": "noisy", "input": [string("conformance noise")]},
        "reproducer-script": {"symbol": "tiered_price", "input": [big_int(5)]},
        # 30 s against the suite's 1000 ms default: only a real cut answers inside the deadline.
        "deadline-sleep": {"symbol": "spin", "input": [number(30)]},
    },
}


def emit(out: Path) -> None:
    (out / "src").mkdir(parents=True, exist_ok=True)
    (out / "src" / "pricing.py").write_text(PRICING_PY, encoding="utf-8")
    (out / "autofuzz.json").write_text(json.dumps(AUTOFUZZ_JSON, indent=2) + "\n", encoding="utf-8")
    (out / "afz-conformance-manifest.json").write_text(json.dumps(MANIFEST, indent=2) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path, help="directory to render the project into")
    args = parser.parse_args()
    emit(args.out)
    print(args.out)


if __name__ == "__main__":
    main()

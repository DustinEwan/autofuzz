"""Tiered pricing, with one deliberate defect.

`tiered_price` indexes `prices` without guarding the bound, so any `quantity`
past the end raises `IndexError`: the finding the scan is meant to find.
`spin` sleeps, so a deadline the harness must cut is reachable at a `--deadline`
`check-harness` can pass.
"""

import time


def tiered_price(quantity: int, prices: list[float]) -> float:
    return prices[quantity]

def spin(seconds: float) -> float:
    # `abs`, not a guard: the generator draws `number` from a symmetric range, and a
    # negative draw would raise `ValueError` from `sleep` in microseconds. The point of
    # this symbol is that it runs past any deadline `check-harness` can pass.
    time.sleep(abs(seconds))
    return seconds

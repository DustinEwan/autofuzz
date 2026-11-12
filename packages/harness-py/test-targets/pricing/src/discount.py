"""Discount application behind a declared guard.

The `raise` is the refusal channel: `classify.py` reads the guard line from
`guardSites` and files an out-of-range `rate` as `rejected`, not as a finding.
"""


def apply_discount(amount: float, rate: float) -> float:
    if rate < 0 or rate > 1:
        raise ValueError("rate out of range")
    return amount * (1 - rate)

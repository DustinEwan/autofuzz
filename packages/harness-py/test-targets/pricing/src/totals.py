"""Three symbols that must be skipped, one per skip reason this fixture can reach.

`untyped` has no annotation, `_private` is not public, and `post` takes a
`Ledger` whose `__init__` demands a DSN the harness cannot invent — the
`fixture-required` blocker the inventory reports with `blockedBy`.
"""


class Ledger:
    def __init__(self, dsn: str) -> None:
        self.dsn = dsn
        self.entries: list[float] = []


def untyped(value): ...


def _private(x: int) -> int:
    return x


def post(ledger: Ledger, amount: float) -> float:
    ledger.entries.append(amount)
    return sum(ledger.entries)

"""A receiver the harness can build: every `__init__` parameter is defaulted."""


class Invoice:
    def __init__(self, currency: str = "USD") -> None:
        self.currency = currency
        self.lines: list[tuple[str, float]] = []

    def add_line(self, label: str, amount: float) -> None:
        self.lines.append((label, amount))

    def total(self) -> float:
        return sum(amount for _, amount in self.lines)

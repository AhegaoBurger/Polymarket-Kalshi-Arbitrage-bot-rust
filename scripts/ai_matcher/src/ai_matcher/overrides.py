"""Manual override application: blacklist > whitelist > AI."""

from __future__ import annotations

import enum
import json
from dataclasses import dataclass
from pathlib import Path


class OverrideOutcome(enum.Enum):
    NONE = "none"
    WHITELIST = "whitelist"
    BLACKLIST = "blacklist"


@dataclass
class OverrideSet:
    whitelist: set[tuple[str, str]]
    blacklist: set[tuple[str, str]]

    @classmethod
    def load(cls, path: Path) -> OverrideSet:
        if not path.exists():
            return cls(whitelist=set(), blacklist=set())
        try:
            data = json.loads(path.read_text())
        except json.JSONDecodeError:
            return cls(whitelist=set(), blacklist=set())
        return cls(
            whitelist={
                (e["kalshi_market_ticker"], e["poly_condition_id"])
                for e in data.get("whitelist", []) or []
            },
            blacklist={
                (e["kalshi_market_ticker"], e["poly_condition_id"])
                for e in data.get("blacklist", []) or []
            },
        )

    def lookup(self, kalshi_ticker: str, poly_condition_id: str) -> OverrideOutcome:
        key = (kalshi_ticker, poly_condition_id)
        if key in self.blacklist:
            return OverrideOutcome.BLACKLIST
        if key in self.whitelist:
            return OverrideOutcome.WHITELIST
        return OverrideOutcome.NONE

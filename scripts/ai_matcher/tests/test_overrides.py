from __future__ import annotations

import json
from pathlib import Path

from ai_matcher.overrides import OverrideOutcome, OverrideSet


def write_override_file(p: Path, payload: dict) -> Path:
    p.write_text(json.dumps(payload))
    return p


def test_blacklist_forces_reject(tmp_path: Path):
    f = write_override_file(tmp_path / "o.json", {
        "version": 1,
        "whitelist": [],
        "blacklist": [
            {"kalshi_market_ticker": "K1", "poly_condition_id": "0xC1", "reason": "stale"}
        ],
    })
    o = OverrideSet.load(f)
    assert o.lookup("K1", "0xC1") == OverrideOutcome.BLACKLIST
    assert o.lookup("K2", "0xC2") == OverrideOutcome.NONE


def test_whitelist_forces_accept(tmp_path: Path):
    f = write_override_file(tmp_path / "o.json", {
        "version": 1,
        "whitelist": [
            {"kalshi_market_ticker": "K1", "poly_condition_id": "0xC1",
             "category": "Economics", "reason": "verified"}
        ],
        "blacklist": [],
    })
    o = OverrideSet.load(f)
    assert o.lookup("K1", "0xC1") == OverrideOutcome.WHITELIST


def test_blacklist_wins_over_whitelist(tmp_path: Path):
    f = write_override_file(tmp_path / "o.json", {
        "version": 1,
        "whitelist": [{"kalshi_market_ticker": "K", "poly_condition_id": "0xC"}],
        "blacklist": [{"kalshi_market_ticker": "K", "poly_condition_id": "0xC", "reason": "x"}],
    })
    assert OverrideSet.load(f).lookup("K", "0xC") == OverrideOutcome.BLACKLIST


def test_missing_file_returns_empty_set(tmp_path: Path):
    o = OverrideSet.load(tmp_path / "missing.json")
    assert o.lookup("K", "0xC") == OverrideOutcome.NONE

"""Tests for categories.py — config loading + bucket resolution."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from ai_matcher.categories import CategoryConfig, load_category_config


def write_config(tmp_path: Path, body: dict) -> Path:
    p = tmp_path / "category_equivalence.json"
    p.write_text(json.dumps(body))
    return p


def test_loads_valid_config(tmp_path: Path):
    p = write_config(tmp_path, {
        "buckets": {
            "Politics": {"kalshi": ["Politics"], "poly": ["Politics"], "tolerance_days": 60},
        },
        "default_tolerance_days": 30,
    })
    cfg = load_category_config(p)
    assert isinstance(cfg, CategoryConfig)
    assert "Politics" in cfg.buckets
    assert cfg.buckets["Politics"].tolerance_days == 60
    assert cfg.buckets["Politics"].kalshi == ["Politics"]
    assert cfg.buckets["Politics"].poly == ["Politics"]
    assert cfg.default_tolerance_days == 30


def test_missing_file_returns_empty_config(tmp_path: Path):
    cfg = load_category_config(tmp_path / "does-not-exist.json")
    assert cfg.buckets == {}
    assert cfg.default_tolerance_days == 30  # falls back to safe default


def test_malformed_json_returns_empty_config(tmp_path: Path):
    p = tmp_path / "category_equivalence.json"
    p.write_text("this is not json")
    cfg = load_category_config(p)
    assert cfg.buckets == {}
    assert cfg.default_tolerance_days == 30

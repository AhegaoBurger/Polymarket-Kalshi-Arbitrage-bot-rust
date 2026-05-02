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


def test_missing_file_logs_at_info_not_warning(tmp_path: Path, caplog):
    """Missing file is the documented rollback path — INFO, not WARNING."""
    import logging
    caplog.set_level(logging.INFO, logger="ai_matcher.categories")
    load_category_config(tmp_path / "no.json")
    info_records = [r for r in caplog.records if r.levelname == "INFO"]
    warning_records = [r for r in caplog.records if r.levelname == "WARNING"]
    assert any("not found" in r.message for r in info_records)
    assert not any("not found" in r.message for r in warning_records)


def test_malformed_bucket_is_skipped_others_kept(tmp_path: Path, caplog):
    """One malformed bucket doesn't kill the whole config."""
    import logging
    caplog.set_level(logging.WARNING, logger="ai_matcher.categories")
    p = write_config(tmp_path, {
        "buckets": {
            "GoodBucket": {"kalshi": ["A"], "poly": ["B"], "tolerance_days": 5},
            "BadBucket": "this should be a dict",   # malformed
            "AnotherGood": {"kalshi": ["C"], "poly": ["D"], "tolerance_days": 10},
        },
        "default_tolerance_days": 30,
    })
    cfg = load_category_config(p)
    assert "GoodBucket" in cfg.buckets
    assert "AnotherGood" in cfg.buckets
    assert "BadBucket" not in cfg.buckets
    assert any("BadBucket" in r.message for r in caplog.records)


def test_non_int_tolerance_skips_just_that_bucket(tmp_path: Path):
    """A bucket with tolerance_days='abc' is skipped; other buckets survive."""
    p = write_config(tmp_path, {
        "buckets": {
            "Good": {"kalshi": ["A"], "poly": ["B"], "tolerance_days": 5},
            "Bad":  {"kalshi": ["X"], "poly": ["Y"], "tolerance_days": "abc"},
        },
        "default_tolerance_days": 30,
    })
    cfg = load_category_config(p)
    assert "Good" in cfg.buckets
    assert "Bad" not in cfg.buckets

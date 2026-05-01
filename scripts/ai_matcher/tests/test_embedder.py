"""Tests for embedder.py — uses the real sentence-transformers model.

The first run downloads ~80 MB; subsequent runs are cached in HF's local cache.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np

from ai_matcher.embedder import Embedder
from ai_matcher.ingestion import Market


def mk_market(title: str, platform: str = "kalshi") -> Market:
    return Market(platform=platform, ticker="t", title=title, description="d")


def test_embedder_produces_consistent_vectors(tmp_path: Path):
    cache_path = tmp_path / "embed_cache.json"
    e = Embedder(cache_path=cache_path)
    m = mk_market("Will the FOMC cut rates in May 2026?")
    v1 = e.embed(m)
    v2 = e.embed(m)
    assert np.allclose(v1, v2)
    assert v1.shape == (e.dim,)


def test_embedder_cache_hits_on_unchanged_content(tmp_path: Path):
    cache_path = tmp_path / "embed_cache.json"
    e = Embedder(cache_path=cache_path)
    m = mk_market("X")
    e.embed(m)
    e.flush()
    e2 = Embedder(cache_path=cache_path)
    assert e2.cache.size > 0
    e2.embed(m)
    assert e2.cache_hits == 1


def test_embedder_cache_misses_on_changed_content(tmp_path: Path):
    cache_path = tmp_path / "embed_cache.json"
    e = Embedder(cache_path=cache_path)
    e.embed(mk_market("A"))
    e.embed(mk_market("B"))
    assert e.cache_hits == 0
    assert e.cache_misses == 2

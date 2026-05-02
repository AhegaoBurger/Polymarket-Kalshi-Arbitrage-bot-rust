"""Tests for BucketedHnswRetrieval."""

from __future__ import annotations

import numpy as np
import pytest

from ai_matcher.retrieval import BucketedHnswRetrieval


def _orthogonal(seed: int, dim: int = 8) -> np.ndarray:
    rng = np.random.default_rng(seed)
    v = rng.standard_normal(dim).astype(np.float32)
    v /= np.linalg.norm(v)
    return v


def test_routes_known_bucket_to_bucket_index():
    politics_vecs = [(_orthogonal(0), "p1"), (_orthogonal(1), "p2")]
    sports_vecs = [(_orthogonal(2), "s1")]
    all_vecs = politics_vecs + sports_vecs
    r = BucketedHnswRetrieval(dim=8, top_k=2, min_cosine=-1.0)
    r.build({"Politics": politics_vecs, "Sports": sports_vecs}, all_vecs)
    results = r.query(politics_vecs[0][0], "Politics")
    ids = [i for i, _ in results]
    assert "p1" in ids
    assert "s1" not in ids


def test_routes_unknown_to_full_index():
    politics_vecs = [(_orthogonal(0), "p1")]
    sports_vecs = [(_orthogonal(2), "s1")]
    all_vecs = politics_vecs + sports_vecs
    r = BucketedHnswRetrieval(dim=8, top_k=10, min_cosine=-1.0)
    r.build({"Politics": politics_vecs, "Sports": sports_vecs}, all_vecs)
    results = r.query(_orthogonal(99), "Unknown")
    ids = {i for i, _ in results}
    assert ids == {"p1", "s1"}


def test_returns_empty_when_known_bucket_has_no_polys():
    politics_vecs = [(_orthogonal(0), "p1")]
    all_vecs = politics_vecs
    r = BucketedHnswRetrieval(dim=8, top_k=10, min_cosine=-1.0)
    r.build({"Politics": politics_vecs}, all_vecs)
    assert r.query(_orthogonal(99), "Sports") == []


def test_min_cosine_filters_low_similarity_results():
    v1 = np.array([1, 0, 0, 0, 0, 0, 0, 0], dtype=np.float32)
    v2 = np.array([-1, 0, 0, 0, 0, 0, 0, 0], dtype=np.float32)
    r = BucketedHnswRetrieval(dim=8, top_k=10, min_cosine=0.5)
    r.build({"Politics": [(v2, "p1")]}, [(v2, "p1")])
    assert r.query(v1, "Politics") == []

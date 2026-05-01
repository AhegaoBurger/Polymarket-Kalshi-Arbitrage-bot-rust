from __future__ import annotations

import numpy as np

from ai_matcher.retrieval import HnswRetrieval


def test_retrieval_returns_topk_candidates_above_threshold():
    rng = np.random.default_rng(0)
    poly_vecs = rng.standard_normal((100, 16)).astype(np.float32)
    poly_vecs /= np.linalg.norm(poly_vecs, axis=1, keepdims=True)
    poly_ids = [f"p{i}" for i in range(100)]
    r = HnswRetrieval(dim=16, top_k=5, min_cosine=0.0)
    r.build(poly_vecs, poly_ids)
    query = poly_vecs[42] + 0.01 * rng.standard_normal(16).astype(np.float32)
    query /= np.linalg.norm(query)
    hits = r.query(query)
    assert len(hits) == 5
    # The exact match should be the top hit.
    assert hits[0][0] == "p42"


def test_retrieval_filters_by_min_cosine():
    rng = np.random.default_rng(0)
    poly_vecs = rng.standard_normal((10, 16)).astype(np.float32)
    poly_vecs /= np.linalg.norm(poly_vecs, axis=1, keepdims=True)
    r = HnswRetrieval(dim=16, top_k=10, min_cosine=0.99)
    r.build(poly_vecs, [f"p{i}" for i in range(10)])
    query = poly_vecs[0]
    hits = r.query(query)
    # Only the exact match should clear the 0.99 threshold.
    assert all(score >= 0.99 for _, score in hits)
    assert hits[0][0] == "p0"

"""Bucketed HNSW retrieval over normalized embeddings.

Per-bucket indexes route Kalshi queries to category-matched Polymarket subsets.
A "_full" fallback index serves Unknown-category Kalshi queries.

Spec: docs/superpowers/specs/2026-05-02-matching-prefilter-and-llm-swap-design.md §3
"""

from __future__ import annotations

import hnswlib
import numpy as np


class BucketedHnswRetrieval:
    UNBUCKETED = "_full"

    def __init__(self, dim: int, top_k: int = 8, min_cosine: float = 0.55) -> None:
        self.dim = dim
        self.top_k = top_k
        self.min_cosine = min_cosine
        self._indexes: dict[str, hnswlib.Index] = {}
        self._ids: dict[str, list[str]] = {}

    def build(
        self,
        polys_by_bucket: dict[str, list[tuple[np.ndarray, str]]],
        all_polys: list[tuple[np.ndarray, str]],
    ) -> None:
        for bucket, items in polys_by_bucket.items():
            if items:
                self._build_one(bucket, items)
        if all_polys:
            self._build_one(self.UNBUCKETED, all_polys)

    def _build_one(self, name: str, items: list[tuple[np.ndarray, str]]) -> None:
        vecs = np.stack([v for v, _ in items])
        ids = [i for _, i in items]
        idx = hnswlib.Index(space="cosine", dim=self.dim)
        idx.init_index(max_elements=len(ids), ef_construction=200, M=16)
        idx.add_items(vecs, ids=np.arange(len(ids)))
        idx.set_ef(50)
        self._indexes[name] = idx
        self._ids[name] = ids

    def query(self, vector: np.ndarray, bucket: str) -> list[tuple[str, float]]:
        """Return [(poly_id, cosine), ...] from the index matching the Kalshi-side bucket.

        Routing:
          - bucket is known and the index exists → use that index
          - bucket is "Unknown" and _full exists → use _full (current pre-spec behavior)
          - bucket is known but no index for it → return empty (deliberate no-op)
        """
        if bucket != "Unknown" and bucket in self._indexes:
            target = bucket
        elif bucket == "Unknown" and self.UNBUCKETED in self._indexes:
            target = self.UNBUCKETED
        else:
            return []
        index, ids = self._indexes[target], self._ids[target]
        labels, distances = index.knn_query(vector, k=min(self.top_k, len(ids)))
        out: list[tuple[str, float]] = []
        for label, dist in zip(labels[0], distances[0], strict=False):
            cosine = 1.0 - float(dist)
            if cosine >= self.min_cosine:
                out.append((ids[label], cosine))
        return out

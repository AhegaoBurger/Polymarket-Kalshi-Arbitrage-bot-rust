"""HNSW retrieval over normalized embeddings.

`min_cosine` pre-filter strips obvious non-matches before the LLM stage spends tokens.
"""

from __future__ import annotations

import hnswlib
import numpy as np


class HnswRetrieval:
    def __init__(self, dim: int, top_k: int = 8, min_cosine: float = 0.55) -> None:
        self.dim = dim
        self.top_k = top_k
        self.min_cosine = min_cosine
        self._index: hnswlib.Index | None = None
        self._ids: list[str] = []

    def build(self, vectors: np.ndarray, ids: list[str]) -> None:
        assert vectors.shape[0] == len(ids)
        idx = hnswlib.Index(space="cosine", dim=self.dim)
        idx.init_index(max_elements=len(ids), ef_construction=200, M=16)
        idx.add_items(vectors, ids=np.arange(len(ids)))
        idx.set_ef(50)
        self._index = idx
        self._ids = list(ids)

    def query(self, vector: np.ndarray) -> list[tuple[str, float]]:
        assert self._index is not None, "build() must be called first"
        labels, distances = self._index.knn_query(
            vector, k=min(self.top_k, len(self._ids))
        )
        # hnswlib returns 1 - cosine_similarity for the cosine space.
        out: list[tuple[str, float]] = []
        for label, dist in zip(labels[0], distances[0], strict=False):
            cosine = 1.0 - float(dist)
            if cosine >= self.min_cosine:
                out.append((self._ids[label], cosine))
        return out

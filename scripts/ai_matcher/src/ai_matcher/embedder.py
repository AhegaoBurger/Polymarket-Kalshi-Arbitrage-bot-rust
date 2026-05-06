"""Local sentence-transformers embedder with a content-hash JSON cache.

Default model: `sentence-transformers/all-MiniLM-L6-v2` (384-dim, ~80 MB on disk).
Override with the `EMBEDDING_MODEL` environment variable.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from sentence_transformers import SentenceTransformer

from ai_matcher.ingestion import Market

DEFAULT_MODEL = "sentence-transformers/all-MiniLM-L6-v2"


@dataclass
class EmbeddingCache:
    """JSON-backed cache: { content_hash: [floats...] } keyed by model name to invalidate on bump."""
    path: Path
    model_name: str
    _by_hash: dict[str, list[float]]

    @classmethod
    def load(cls, path: Path, model_name: str) -> EmbeddingCache:
        data: dict[str, dict] = {}
        if path.exists():
            try:
                data = json.loads(path.read_text()) or {}
            except json.JSONDecodeError:
                data = {}
        section = data.get(model_name, {})
        return cls(path=path, model_name=model_name, _by_hash=section)

    def save(self) -> None:
        existing: dict[str, dict] = {}
        if self.path.exists():
            try:
                existing = json.loads(self.path.read_text()) or {}
            except json.JSONDecodeError:
                existing = {}
        existing[self.model_name] = self._by_hash
        self.path.write_text(json.dumps(existing))

    def get(self, content_hash: str) -> np.ndarray | None:
        v = self._by_hash.get(content_hash)
        return np.array(v, dtype=np.float32) if v is not None else None

    def put(self, content_hash: str, vec: np.ndarray) -> None:
        self._by_hash[content_hash] = vec.astype(float).tolist()

    @property
    def size(self) -> int:
        return len(self._by_hash)


class Embedder:
    def __init__(self, cache_path: Path, model_name: str | None = None) -> None:
        self.model_name = model_name or os.environ.get("EMBEDDING_MODEL", DEFAULT_MODEL)
        self._model = SentenceTransformer(self.model_name)
        # `get_embedding_dimension` is the new name; fall back for older versions.
        get_dim = getattr(self._model, "get_embedding_dimension", None) \
            or self._model.get_sentence_embedding_dimension
        self.dim: int = get_dim()
        self.cache = EmbeddingCache.load(cache_path, self.model_name)
        self.cache_hits = 0
        self.cache_misses = 0

    def embed(self, market: Market) -> np.ndarray:
        """Single-market embed. Thin wrapper over embed_many — prefer embed_many
        when handling >1 market so encoding is batched."""
        return self.embed_many([market])[0]

    def embed_many(self, markets: list[Market]) -> list[np.ndarray]:
        """Batch-embed a list of markets, returning vectors in the same order.

        Cache hits are resolved without touching the model. Cache misses are
        encoded in one batched call (with a tqdm progress bar) — orders of
        magnitude faster than a Python loop of single-item encodes on CPU.
        """
        n = len(markets)
        out: list[np.ndarray | None] = [None] * n
        miss_indices: list[int] = []
        miss_texts: list[str] = []
        miss_hashes: list[str] = []
        for i, m in enumerate(markets):
            h = m.content_hash()
            cached = self.cache.get(h)
            if cached is not None:
                out[i] = cached
                self.cache_hits += 1
            else:
                miss_indices.append(i)
                miss_texts.append(m.text_for_embedding())
                miss_hashes.append(h)
        if miss_texts:
            vecs = self._model.encode(
                miss_texts,
                batch_size=64,
                normalize_embeddings=True,
                show_progress_bar=True,
                convert_to_numpy=True,
            )
            for idx, h, vec in zip(miss_indices, miss_hashes, vecs):
                v = np.asarray(vec, dtype=np.float32)
                self.cache.put(h, v)
                out[idx] = v
            self.cache_misses += len(miss_texts)
        return [v for v in out if v is not None]  # type: ignore[misc]

    def flush(self) -> None:
        self.cache.save()

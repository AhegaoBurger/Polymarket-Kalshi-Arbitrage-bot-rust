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
        h = market.content_hash()
        cached = self.cache.get(h)
        if cached is not None:
            self.cache_hits += 1
            return cached
        vec = self._model.encode(market.text_for_embedding(), normalize_embeddings=True)
        vec = np.asarray(vec, dtype=np.float32)
        self.cache.put(h, vec)
        self.cache_misses += 1
        return vec

    def flush(self) -> None:
        self.cache.save()

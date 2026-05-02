from __future__ import annotations

import json
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import MagicMock

import numpy as np

from ai_matcher.categories import BucketDef, CategoryConfig
from ai_matcher.ingestion import IngestionResult, Market
from ai_matcher.pipeline import PipelineConfig, date_overlap_ok, run_pipeline
from ai_matcher.verifier import Decision, EmbeddingsOnlyVerifier


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _cfg() -> CategoryConfig:
    return CategoryConfig(
        buckets={
            "Politics": BucketDef(kalshi=["Politics"], poly=["Politics"], tolerance_days=60),
            "Sports":   BucketDef(kalshi=["Sports"],   poly=["Sports"],   tolerance_days=2),
        },
        default_tolerance_days=30,
    )


def _market(bucket: str, days_offset: int) -> Market:
    return Market(
        platform="kalshi", ticker="t", title="t",
        bucket=bucket,
        close_time_utc=datetime(2026, 6, 1, tzinfo=timezone.utc) + timedelta(days=days_offset),
    )


# ---------------------------------------------------------------------------
# date_overlap_ok unit tests
# ---------------------------------------------------------------------------

def test_within_tolerance_passes():
    k = _market("Politics", 0)
    p = _market("Politics", 30)
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is True


def test_beyond_tolerance_fails():
    k = _market("Politics", 0)
    p = _market("Politics", 90)
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is False


def test_scale_widens_tolerance():
    k = _market("Politics", 0)
    p = _market("Politics", 90)
    assert date_overlap_ok(k, p, _cfg(), scale=2.0) is True


def test_sports_tolerance_is_strict():
    k = _market("Sports", 0)
    p = _market("Sports", 5)
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is False


def test_unknown_bucket_uses_default_tolerance():
    """Both Unknown → default_tolerance_days (30 in fixture)."""
    k = Market(platform="kalshi", ticker="k", title="t",
               bucket="Unknown",
               close_time_utc=datetime(2026, 6, 1, tzinfo=timezone.utc))
    p = Market(platform="polymarket", ticker="p", title="t",
               bucket="Unknown", condition_id="0xC1",
               close_time_utc=datetime(2026, 6, 28, tzinfo=timezone.utc))
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is True
    p.close_time_utc = datetime(2026, 7, 5, tzinfo=timezone.utc)
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is False


# ---------------------------------------------------------------------------
# Pipeline handles empty ingestion
# ---------------------------------------------------------------------------

def test_pipeline_handles_empty_ingestion(tmp_path: Path):
    """Sidecar should still produce a valid (empty) outputs file."""
    project_root = tmp_path
    config_path = project_root / "config"
    config_path.mkdir()
    (config_path / "manual_overrides.json").write_text(
        json.dumps({"version": 1, "whitelist": [], "blacklist": []})
    )

    ingestion = MagicMock()
    ingestion.fetch_all.return_value = IngestionResult(kalshi=[], poly=[])

    embedder = MagicMock()
    embedder.dim = 4
    embedder.flush.return_value = None
    verifier = MagicMock()

    cfg = PipelineConfig(
        project_root=project_root,
        audit_dir=project_root / "audit",
        matches_path=project_root / ".ai_matches.json",
        audit_log_path=project_root / ".ai_matcher_audit.jsonl",
        overrides_path=config_path / "manual_overrides.json",
        embedding_model="m",
        llm_model="l",
    )
    summary = run_pipeline(cfg, ingestion=ingestion, embedder=embedder, verifier=verifier)
    matches = json.loads(cfg.matches_path.read_text())
    assert matches["pairs"] == []
    assert summary["accepted"] == 0
    assert summary["rejected"] == 0
    assert summary["rows"] == 0


# ---------------------------------------------------------------------------
# New integration test: funnel counters + bucket routing
# ---------------------------------------------------------------------------

class _OrthoEmbedder:
    """Deterministic embedder. Two markets with the same `ticker` get identical vectors."""

    dim = 8

    def __init__(self):
        self._by_ticker: dict[str, np.ndarray] = {}

    def embed(self, market: Market) -> np.ndarray:
        key = market.ticker
        if key not in self._by_ticker:
            i = len(self._by_ticker) % self.dim
            v = np.zeros(self.dim, dtype=np.float32)
            v[i] = 1.0
            self._by_ticker[key] = v
        return self._by_ticker[key]

    def flush(self) -> None:
        pass


def _fake_ingestion(kalshi: list[Market], poly: list[Market]) -> SimpleNamespace:
    return SimpleNamespace(fetch_all=lambda: IngestionResult(kalshi=kalshi, poly=poly))


def test_pipeline_funnel_counters_and_bucket_routing(tmp_path):
    cfg_obj = CategoryConfig(
        buckets={
            "Politics": BucketDef(kalshi=["Politics"], poly=["Politics"], tolerance_days=60),
            "Sports":   BucketDef(kalshi=["Sports"],   poly=["Sports"],   tolerance_days=2),
        },
        default_tolerance_days=30,
    )

    base_time = datetime(2026, 6, 1, tzinfo=timezone.utc)

    kalshi = [
        Market(platform="kalshi", ticker="k_pol_a", title="t",
               bucket="Politics", close_time_utc=base_time),
        Market(platform="kalshi", ticker="k_sports_a", title="t",
               bucket="Sports", close_time_utc=base_time),
    ]
    poly = [
        Market(platform="polymarket", ticker="k_pol_a", title="t",
               bucket="Politics", condition_id="0xPOL",
               close_time_utc=base_time),
        Market(platform="polymarket", ticker="k_pol_a", title="t",
               bucket="Politics", condition_id="0xPOL_FAR",
               close_time_utc=base_time + timedelta(days=90)),
        Market(platform="polymarket", ticker="k_sports_a", title="t",
               bucket="Sports", condition_id="0xSP",
               close_time_utc=base_time),
    ]

    config_path = tmp_path / "config"
    config_path.mkdir()
    (config_path / "manual_overrides.json").write_text(
        json.dumps({"version": 1, "whitelist": [], "blacklist": []})
    )

    pipeline_cfg = PipelineConfig(
        project_root=tmp_path,
        audit_dir=tmp_path / "audit",
        matches_path=tmp_path / "matches.json",
        audit_log_path=tmp_path / "audit.jsonl",
        overrides_path=config_path / "manual_overrides.json",
        embedding_model="test",
        llm_model="embeddings-only",
        category_config=cfg_obj,
        expiry_tolerance_scale=1.0,
        acceptance_min_confidence=0.5,
    )
    summary = run_pipeline(
        pipeline_cfg,
        ingestion=_fake_ingestion(kalshi, poly),
        embedder=_OrthoEmbedder(),
        verifier=EmbeddingsOnlyVerifier(accept_cosine=0.5),
    )
    assert summary["accepted"] == 2
    assert summary["drops_at_date_overlap"] == 1
    assert summary["candidates_after_retrieval"] == 3
    assert "ingested" in summary
    assert summary["ingested"]["kalshi"] == 2
    assert summary["ingested"]["poly"] == 3
    assert "bucketed" in summary

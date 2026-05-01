from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock

import numpy as np

from ai_matcher.ingestion import IngestionResult, Market
from ai_matcher.pipeline import PipelineConfig, run_pipeline
from ai_matcher.verifier import Decision


def mk_kalshi(t: str) -> Market:
    return Market(platform="kalshi", ticker=t, title=t, resolution_criteria=t)


def mk_poly(t: str) -> Market:
    return Market(
        platform="polymarket",
        ticker=t,
        title=t,
        condition_id=f"0x{t}",
        poly_yes_token=f"y{t}",
        poly_no_token=f"n{t}",
        resolution_criteria=t,
    )


def test_pipeline_writes_three_outputs_with_one_accepted_pair(tmp_path: Path):
    project_root = tmp_path
    config_path = project_root / "config"
    config_path.mkdir()
    (config_path / "manual_overrides.json").write_text(
        json.dumps({"version": 1, "whitelist": [], "blacklist": []})
    )

    ingestion = MagicMock()
    ingestion.fetch_all.return_value = IngestionResult(
        kalshi=[mk_kalshi("CPI")],
        poly=[mk_poly("CPI"), mk_poly("BTC")],
    )

    embedder = MagicMock()
    embedder.dim = 4
    # Vary embeddings so retrieval can rank them
    embeddings_by_ticker = {
        "CPI": np.array([1.0, 0.0, 0.0, 0.0], dtype=np.float32),
        "BTC": np.array([0.0, 1.0, 0.0, 0.0], dtype=np.float32),
    }
    embedder.embed.side_effect = lambda m: embeddings_by_ticker.get(
        m.ticker, np.array([0.5, 0.5, 0.0, 0.0], dtype=np.float32)
    )
    embedder.cache_hits = 0
    embedder.cache_misses = 0
    embedder.flush.return_value = None

    verifier = MagicMock()
    verifier.verify.side_effect = lambda k, p: Decision(
        confidence=0.95 if p.ticker == "CPI" else 0.4,
        resolution_match=p.ticker == "CPI",
        concerns=[],
        reasoning="x",
        category="Economics",
        event_type="Cpi",
    )

    cfg = PipelineConfig(
        project_root=project_root,
        audit_dir=project_root / "audit",
        matches_path=project_root / ".ai_matches.json",
        audit_log_path=project_root / ".ai_matcher_audit.jsonl",
        overrides_path=config_path / "manual_overrides.json",
        embedding_model="test-model",
        llm_model="claude-opus-4-7",
        min_cosine=0.0,  # admit both candidates so the verifier is exercised on both
    )

    summary = run_pipeline(cfg, ingestion=ingestion, embedder=embedder, verifier=verifier)

    matches = json.loads(cfg.matches_path.read_text())
    assert matches["model"] == "claude-opus-4-7"
    assert matches["embedding_model"] == "test-model"
    assert len(matches["pairs"]) == 1
    assert matches["pairs"][0]["kalshi_market_ticker"] == "CPI"
    assert matches["pairs"][0]["poly_condition_id"] == "0xCPI"

    assert (cfg.audit_dir / "report.html").exists()
    audit_lines = cfg.audit_log_path.read_text().splitlines()
    assert len(audit_lines) >= 1
    assert summary["accepted"] == 1
    assert summary["rejected"] == 1


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
    assert summary == {"accepted": 0, "rejected": 0, "rows": 0}
    matches = json.loads(cfg.matches_path.read_text())
    assert matches["pairs"] == []

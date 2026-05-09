"""Tests for the LiteLLM-backed Verifier."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from types import SimpleNamespace
from pathlib import Path

import pytest

from ai_matcher.ingestion import Market
from ai_matcher.verifier import Decision, Verifier


def _market(platform: str, ticker: str, title: str = "T") -> Market:
    return Market(
        platform=platform, ticker=ticker, title=title,
        bucket="Politics",
        close_time_utc=datetime(2026, 6, 1, tzinfo=timezone.utc),
        condition_id="0xC1" if platform == "polymarket" else "",
    )


def _fake_response(arguments: dict, cost: float = 0.0007) -> SimpleNamespace:
    return SimpleNamespace(
        choices=[SimpleNamespace(message=SimpleNamespace(
            tool_calls=[SimpleNamespace(function=SimpleNamespace(
                arguments=json.dumps(arguments),
            ))]
        ))],
        _hidden_params={"response_cost": cost},
    )


def test_verify_parses_litellm_response(monkeypatch, tmp_path: Path):
    captured: dict = {}
    def fake_completion(**kwargs):
        captured.update(kwargs)
        return _fake_response({
            "confidence": 0.95, "resolution_match": True,
            "concerns": [], "reasoning": "R",
            "category": "Politics", "event_type": "Election",
        })
    monkeypatch.setattr("litellm.completion", fake_completion)

    v = Verifier(model="gpt-4.1-mini", cache_path=tmp_path / "cache.json")
    decision = v.verify(_market("kalshi", "K1"), _market("polymarket", "P1"))
    assert decision.confidence == 0.95
    assert decision.resolution_match is True
    assert decision.category == "Politics"
    assert decision.event_type == "Election"
    assert captured["model"] == "gpt-4.1-mini"
    assert captured["tool_choice"]["type"] == "function"
    assert captured["tools"][0]["type"] == "function"
    assert captured["tools"][0]["function"]["name"] == "report_match_decision"


def test_verify_caches_by_provider_and_pair(monkeypatch, tmp_path: Path):
    calls = {"n": 0}
    def fake_completion(**kwargs):
        calls["n"] += 1
        return _fake_response({
            "confidence": 0.9, "resolution_match": True,
            "concerns": [], "reasoning": "", "category": "Politics", "event_type": "Other",
        })
    monkeypatch.setattr("litellm.completion", fake_completion)

    v = Verifier(model="gpt-4.1-mini", cache_path=tmp_path / "cache.json")
    k, p = _market("kalshi", "K1"), _market("polymarket", "P1")
    v.verify(k, p)
    v.verify(k, p)
    assert calls["n"] == 1
    assert v.cache_hits == 1


def test_cache_invalidates_on_model_change(monkeypatch, tmp_path: Path):
    monkeypatch.setattr("litellm.completion", lambda **kw: _fake_response({
        "confidence": 0.9, "resolution_match": True,
        "concerns": [], "reasoning": "", "category": "Politics", "event_type": "Other",
    }))
    cache_path = tmp_path / "cache.json"

    v1 = Verifier(model="gpt-4.1-mini", cache_path=cache_path)
    v1.verify(_market("kalshi", "K1"), _market("polymarket", "P1"))

    v2 = Verifier(model="deepseek/deepseek-chat", cache_path=cache_path)
    v2.verify(_market("kalshi", "K1"), _market("polymarket", "P1"))
    assert v2.cache_misses == 1
    assert v2.cache_hits == 0


def test_cost_field_threaded_into_cache(monkeypatch, tmp_path: Path):
    monkeypatch.setattr("litellm.completion", lambda **kw: _fake_response({
        "confidence": 0.9, "resolution_match": True,
        "concerns": [], "reasoning": "", "category": "Politics", "event_type": "Other",
    }, cost=0.0042))
    v = Verifier(model="gpt-4.1-mini", cache_path=tmp_path / "cache.json")
    decision = v.verify(_market("kalshi", "K1"), _market("polymarket", "P1"))
    assert decision.cost_usd == pytest.approx(0.0042)


def test_verify_handles_missing_required_field(monkeypatch, tmp_path: Path):
    """gpt-4.1-mini occasionally drops required fields from tool_call args.
    The verifier must default rather than crash, and reject the pair."""
    monkeypatch.setattr("litellm.completion", lambda **kw: _fake_response({
        # `resolution_match` deliberately missing — was causing KeyError
        "confidence": 0.3,
        "concerns": ["incomplete tool call"],
        "reasoning": "model dropped fields",
    }))
    v = Verifier(model="gpt-4.1-mini", cache_path=tmp_path / "cache.json")
    decision = v.verify(_market("kalshi", "K1"), _market("polymarket", "P1"))
    assert decision.resolution_match is False
    assert decision.is_accepted(0.9) is False  # safe default
    assert decision.confidence == pytest.approx(0.3)


def test_verify_handles_litellm_exception(monkeypatch, tmp_path: Path):
    """Network/auth errors bubble up from litellm — return rejection, don't cache."""
    def boom(**kw):
        raise RuntimeError("simulated 500 from upstream")
    monkeypatch.setattr("litellm.completion", boom)
    v = Verifier(model="gpt-4.1-mini", cache_path=tmp_path / "cache.json")
    decision = v.verify(_market("kalshi", "K1"), _market("polymarket", "P1"))
    assert decision.resolution_match is False
    assert any("verifier-error" in c for c in decision.concerns)
    # Not cached — next call should retry
    assert v.is_cached(_market("kalshi", "K1"), _market("polymarket", "P1")) is False


def test_averify_runs_concurrently(monkeypatch, tmp_path: Path):
    """Async path returns same shape as sync path; cache is populated; flush works."""
    import asyncio

    async def fake_acompletion(**kwargs):
        return _fake_response({
            "confidence": 0.95, "resolution_match": True,
            "concerns": [], "reasoning": "ok",
            "category": "Politics", "event_type": "Election",
        })
    monkeypatch.setattr("litellm.acompletion", fake_acompletion)

    v = Verifier(model="gpt-4.1-mini", cache_path=tmp_path / "cache.json")
    # save_every is the default 50 — flush manually after to exercise that path
    k, p = _market("kalshi", "K1"), _market("polymarket", "P1")

    async def run():
        results = await asyncio.gather(*[v.averify(k, p) for _ in range(5)])
        v.flush()
        return results

    decisions = asyncio.run(run())
    assert len(decisions) == 5
    assert all(d.resolution_match for d in decisions)
    # First call hits the LLM; subsequent four hit the (in-memory) cache
    assert v.cache_misses == 1
    assert v.cache_hits == 4
    # Cache file written via flush()
    assert v.is_cached(k, p)

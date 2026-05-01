from __future__ import annotations

from pathlib import Path
from unittest.mock import MagicMock

from ai_matcher.ingestion import Market
from ai_matcher.verifier import Verifier


def mk_pair():
    k = Market(
        platform="kalshi", ticker="KX", title="A?", description="d",
        resolution_criteria="r", outcomes=["Yes", "No"],
    )
    p = Market(
        platform="polymarket", ticker="poly", title="A?", description="d",
        resolution_criteria="r", outcomes=["Yes", "No"], condition_id="0xC",
    )
    return k, p


def fake_anthropic_response(json_blob: dict) -> MagicMock:
    """Mimic anthropic SDK: client.messages.create returns an object whose
    .content[0] is a tool_use block with .input == json_blob."""
    msg = MagicMock()
    msg.stop_reason = "tool_use"
    block = MagicMock()
    block.type = "tool_use"
    block.input = json_blob
    msg.content = [block]
    return msg


def test_verifier_accepts_high_confidence_no_concerns(tmp_path: Path):
    client = MagicMock()
    client.messages.create.return_value = fake_anthropic_response({
        "confidence": 0.97, "resolution_match": True, "concerns": [],
        "reasoning": "identical resolution", "category": "Economics",
        "event_type": "Cpi",
    })
    v = Verifier(client=client, model="claude-opus-4-7", cache_path=tmp_path / "v.json")
    k, p = mk_pair()
    d = v.verify(k, p)
    assert d.accepted
    assert d.confidence == 0.97
    assert d.category == "Economics"


def test_verifier_rejects_low_confidence(tmp_path: Path):
    client = MagicMock()
    client.messages.create.return_value = fake_anthropic_response({
        "confidence": 0.6, "resolution_match": False, "concerns": ["different dates"],
        "reasoning": "diverges", "category": "", "event_type": "Other",
    })
    v = Verifier(client=client, model="claude-opus-4-7", cache_path=tmp_path / "v.json")
    k, p = mk_pair()
    d = v.verify(k, p)
    assert not d.accepted


def test_verifier_caches_decision(tmp_path: Path):
    client = MagicMock()
    client.messages.create.return_value = fake_anthropic_response({
        "confidence": 0.97, "resolution_match": True, "concerns": [],
        "reasoning": "x", "category": "Economics", "event_type": "Cpi",
    })
    v = Verifier(client=client, model="claude-opus-4-7", cache_path=tmp_path / "v.json")
    k, p = mk_pair()
    v.verify(k, p)
    v.verify(k, p)
    assert client.messages.create.call_count == 1
    assert v.cache_hits == 1

"""Tests for ingestion.py — uses recorded JSON fixtures, no network."""

from __future__ import annotations

from ai_matcher.ingestion import (
    content_hash,
    parse_kalshi_event,
    parse_kalshi_markets_response,
    parse_poly_gamma_markets_response,
)


def test_kalshi_response_parses_to_markets():
    body = {
        "markets": [
            {
                "ticker": "KXCPIYOY-26APR-B3.0",
                "event_ticker": "KXCPIYOY-26APR",
                "title": "CPI YoY April 2026 above 3.0%",
                "subtitle": "BLS CPI release",
                "rules_primary": "Resolves YES if BLS CPI YoY > 3.0",
                "yes_sub_title": "Above 3.0%",
                "category": "Economics",
                "liquidity": 50_000,  # cents → $500
                "volume": 1_000_000,  # cents → $10,000
            }
        ]
    }
    markets = parse_kalshi_markets_response(body, event_title="CPI YoY April 2026")
    assert len(markets) == 1
    m = markets[0]
    assert m.platform == "kalshi"
    assert m.ticker == "KXCPIYOY-26APR-B3.0"
    assert "April 2026" in m.title
    assert m.category == "Economics"
    assert m.liquidity_usd == 500.0
    assert m.volume_usd == 10_000.0
    # Event title is folded into description so per-event context survives.
    assert "CPI YoY April 2026" in m.description


def test_kalshi_response_filters_below_liquidity_floor():
    body = {
        "markets": [
            {"ticker": "K1", "title": "rich", "liquidity": 50_000},   # $500
            {"ticker": "K2", "title": "poor", "liquidity": 1_000},    # $10
        ]
    }
    markets = parse_kalshi_markets_response(body, min_liquidity_usd=100.0)
    assert [m.ticker for m in markets] == ["K1"]


def test_kalshi_response_keeps_markets_with_unknown_liquidity():
    """The unauthenticated Kalshi browse endpoint always returns liquidity=null.
    We must not drop markets whose liquidity is *unknown* — only those whose
    liquidity is *known and below threshold*. Otherwise the entire Kalshi side
    disappears whenever the sidecar runs without Kalshi credentials."""
    body = {
        "markets": [
            {"ticker": "K1", "title": "unknown liq", "liquidity": None},
            {"ticker": "K2", "title": "no liq field at all"},  # missing key entirely
            {"ticker": "K3", "title": "low known", "liquidity": 1},  # $0.01
        ]
    }
    markets = parse_kalshi_markets_response(body, min_liquidity_usd=100.0)
    # K1 + K2 (unknown) survive. K3 (known, below floor) drops.
    assert sorted(m.ticker for m in markets) == ["K1", "K2"]


def test_parse_kalshi_event_skips_multivariate_parlays():
    assert parse_kalshi_event({"event_ticker": "KXMVESPORTSMULTIGAMEEXTENDED-S1"}) is None
    assert parse_kalshi_event({"event_ticker": "KXMVECROSSCATEGORY-S2"}) is None
    assert parse_kalshi_event({"event_ticker": ""}) is None
    ok = parse_kalshi_event({"event_ticker": "KXFED-26MAY", "title": "FOMC May"})
    assert ok is not None
    assert ok["event_ticker"] == "KXFED-26MAY"
    assert ok["title"] == "FOMC May"


def test_poly_gamma_response_parses_to_markets():
    body = [
        {
            "slug": "cpi-april-2026-above-3",
            "question": "CPI YoY April 2026 above 3.0%?",
            "description": "Resolves YES if BLS CPI YoY > 3.0",
            "outcomes": "[\"Yes\",\"No\"]",
            "clobTokenIds": "[\"0xtokA\",\"0xtokB\"]",
            "conditionId": "0xCID",
            "category": "Economics",
            "active": True,
            "closed": False,
            "liquidity": "1234.56",
            "volume": 50_000,
        }
    ]
    markets = parse_poly_gamma_markets_response(body)
    assert len(markets) == 1
    m = markets[0]
    assert m.platform == "polymarket"
    assert m.condition_id == "0xCID"
    assert m.outcomes == ["Yes", "No"]
    assert m.poly_yes_token == "0xtokA"
    assert m.poly_no_token == "0xtokB"
    # Polymarket reports liquidity natively in USD; we should accept both
    # numeric strings and floats.
    assert abs(m.liquidity_usd - 1234.56) < 1e-6
    assert m.volume_usd == 50_000.0


def test_poly_skips_closed_markets():
    body = [
        {
            "slug": "x",
            "question": "q",
            "outcomes": "[]",
            "clobTokenIds": "[]",
            "conditionId": "0xC",
            "active": True,
            "closed": True,
        }
    ]
    assert parse_poly_gamma_markets_response(body) == []


def test_poly_filters_below_liquidity_floor():
    body = [
        {
            "slug": "rich", "question": "rich", "conditionId": "0xR",
            "outcomes": "[\"Y\",\"N\"]", "clobTokenIds": "[\"a\",\"b\"]",
            "active": True, "closed": False,
            "liquidity": 5000.0,
        },
        {
            "slug": "poor", "question": "poor", "conditionId": "0xP",
            "outcomes": "[\"Y\",\"N\"]", "clobTokenIds": "[\"c\",\"d\"]",
            "active": True, "closed": False,
            "liquidity": 5.0,
        },
    ]
    markets = parse_poly_gamma_markets_response(body, min_liquidity_usd=100.0)
    assert [m.ticker for m in markets] == ["rich"]


def test_content_hash_is_stable():
    a = content_hash("alpha", "beta", "gamma")
    b = content_hash("alpha", "beta", "gamma")
    assert a == b
    c = content_hash("alpha", "beta", "delta")
    assert a != c

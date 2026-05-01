"""Tests for ingestion.py — uses recorded JSON fixtures, no network."""

from __future__ import annotations

from ai_matcher.ingestion import (
    content_hash,
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
            }
        ]
    }
    markets = parse_kalshi_markets_response(body)
    assert len(markets) == 1
    m = markets[0]
    assert m.platform == "kalshi"
    assert m.ticker == "KXCPIYOY-26APR-B3.0"
    assert "April 2026" in m.title
    assert m.category == "Economics"


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


def test_content_hash_is_stable():
    a = content_hash("alpha", "beta", "gamma")
    b = content_hash("alpha", "beta", "gamma")
    assert a == b
    c = content_hash("alpha", "beta", "delta")
    assert a != c

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
                "close_time": "2026-06-01T12:00:00Z",
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
            {"ticker": "K1", "title": "rich", "liquidity": 50_000, "close_time": "2026-06-01T12:00:00Z"},   # $500
            {"ticker": "K2", "title": "poor", "liquidity": 1_000, "close_time": "2026-06-01T12:00:00Z"},    # $10
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
            {"ticker": "K1", "title": "unknown liq", "liquidity": None, "close_time": "2026-06-01T12:00:00Z"},
            {"ticker": "K2", "title": "no liq field at all", "close_time": "2026-06-01T12:00:00Z"},  # missing key entirely
            {"ticker": "K3", "title": "low known", "liquidity": 1, "close_time": "2026-06-01T12:00:00Z"},  # $0.01
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
            "endDateIso": "2026-06-01T12:00:00Z",
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
            "endDateIso": "2026-06-01T12:00:00Z",
        },
        {
            "slug": "poor", "question": "poor", "conditionId": "0xP",
            "outcomes": "[\"Y\",\"N\"]", "clobTokenIds": "[\"c\",\"d\"]",
            "active": True, "closed": False,
            "liquidity": 5.0,
            "endDateIso": "2026-06-01T12:00:00Z",
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


from datetime import datetime, timezone

from ai_matcher.ingestion import Market


def test_market_dataclass_has_bucket_close_time_tags_fields():
    m = Market(
        platform="kalshi",
        ticker="K1",
        title="t",
        bucket="Politics",
        close_time_utc=datetime(2026, 6, 1, tzinfo=timezone.utc),
        tags=["Politics", "Election"],
    )
    assert m.bucket == "Politics"
    assert m.close_time_utc == datetime(2026, 6, 1, tzinfo=timezone.utc)
    assert m.tags == ["Politics", "Election"]


from ai_matcher.ingestion import parse_close_time_utc


def test_parses_iso_with_offset():
    raw = {"close_time": "2026-06-01T12:00:00+00:00"}
    dt = parse_close_time_utc(raw, "kalshi")
    assert dt is not None
    assert dt.tzinfo is not None
    assert dt == datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)


def test_parses_iso_with_z_suffix():
    raw = {"close_time": "2026-06-01T12:00:00Z"}
    dt = parse_close_time_utc(raw, "kalshi")
    assert dt == datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)


def test_returns_none_for_missing_field():
    assert parse_close_time_utc({}, "kalshi") is None
    assert parse_close_time_utc({"close_time": ""}, "kalshi") is None
    assert parse_close_time_utc({"close_time": None}, "kalshi") is None


def test_returns_none_for_naive_datetime():
    """Refuse to guess timezone — naive datetime is a parse failure."""
    raw = {"close_time": "2026-06-01T12:00:00"}
    assert parse_close_time_utc(raw, "kalshi") is None


def test_returns_none_for_garbage():
    raw = {"close_time": "not a date"}
    assert parse_close_time_utc(raw, "kalshi") is None


def test_polymarket_prefers_endDate_over_endDateIso():
    """endDate is tz-aware on Gamma; endDateIso is date-only. Prefer endDate."""
    raw = {"endDate": "2026-06-01T12:00:00Z", "endDateIso": "2026-06-01"}
    dt = parse_close_time_utc(raw, "polymarket")
    assert dt == datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)


def test_polymarket_falls_back_to_endDateIso_when_endDate_missing():
    """If endDate is absent but endDateIso happens to have a tz-aware string, use it."""
    raw = {"endDateIso": "2026-06-01T12:00:00Z"}
    dt = parse_close_time_utc(raw, "polymarket")
    assert dt == datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)


def test_polymarket_falls_back_to_endDate():
    raw = {"endDate": "2026-06-01T12:00:00Z"}
    dt = parse_close_time_utc(raw, "polymarket")
    assert dt == datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)


from ai_matcher.categories import BucketDef, CategoryConfig


def _kalshi_cfg() -> CategoryConfig:
    return CategoryConfig(
        buckets={
            "Politics":  BucketDef(kalshi=["Politics"],  poly=["Politics"],  tolerance_days=60),
            "Economics": BucketDef(kalshi=["Economics"], poly=["Finance"],   tolerance_days=14),
        },
        default_tolerance_days=30,
    )


def test_kalshi_parser_assigns_bucket():
    body = {
        "markets": [
            {
                "ticker": "K1", "title": "t", "category": "Politics",
                "close_time": "2026-06-01T12:00:00Z",
            }
        ]
    }
    markets = parse_kalshi_markets_response(body, category_config=_kalshi_cfg())
    assert len(markets) == 1
    assert markets[0].bucket == "Politics"
    assert markets[0].close_time_utc == datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)


def test_kalshi_parser_drops_market_with_missing_close_time():
    body = {
        "markets": [
            {"ticker": "K1", "title": "no date", "category": "Politics"},
            {"ticker": "K2", "title": "good",    "category": "Politics",
             "close_time": "2026-06-01T12:00:00Z"},
        ]
    }
    markets = parse_kalshi_markets_response(body, category_config=_kalshi_cfg())
    assert [m.ticker for m in markets] == ["K2"]


def test_kalshi_parser_drops_market_with_unparseable_close_time():
    body = {
        "markets": [
            {"ticker": "K1", "title": "bad", "category": "Politics",
             "close_time": "not a date"},
        ]
    }
    markets = parse_kalshi_markets_response(body, category_config=_kalshi_cfg())
    assert markets == []


def test_kalshi_parser_assigns_unknown_when_category_missing():
    body = {
        "markets": [
            {"ticker": "K1", "title": "t", "category": "Astronomy",
             "close_time": "2026-06-01T12:00:00Z"},
        ]
    }
    markets = parse_kalshi_markets_response(body, category_config=_kalshi_cfg())
    assert markets[0].bucket == "Unknown"


def test_kalshi_parser_works_without_category_config():
    """Backward compat: when no config is passed, bucket defaults to Unknown but markets still parse."""
    body = {
        "markets": [
            {"ticker": "K1", "title": "t", "category": "Politics",
             "close_time": "2026-06-01T12:00:00Z"},
        ]
    }
    markets = parse_kalshi_markets_response(body)
    assert markets[0].bucket == "Unknown"


def _poly_cfg() -> CategoryConfig:
    return CategoryConfig(
        buckets={
            "Politics":  BucketDef(kalshi=["Politics"], poly=["Politics"], tolerance_days=60),
            "Economics": BucketDef(kalshi=["Economics"], poly=["Finance", "Economics"], tolerance_days=14),
        },
        default_tolerance_days=30,
    )


def test_poly_parser_assigns_bucket_from_category():
    body = [{
        "conditionId": "0xC1", "slug": "p1", "question": "q",
        "category": "Politics",
        "endDateIso": "2026-06-01T12:00:00Z",
    }]
    markets = parse_poly_gamma_markets_response(body, category_config=_poly_cfg())
    assert markets[0].bucket == "Politics"


def test_poly_parser_falls_back_to_tags_when_category_empty():
    body = [{
        "conditionId": "0xC1", "slug": "p1", "question": "q",
        "category": "",
        "tags": ["Politics", "Election"],
        "endDateIso": "2026-06-01T12:00:00Z",
    }]
    markets = parse_poly_gamma_markets_response(body, category_config=_poly_cfg())
    assert markets[0].bucket == "Politics"
    assert markets[0].tags == ["Politics", "Election"]


def test_poly_parser_handles_object_shaped_tags():
    """Gamma sometimes returns tags as [{"label": "X"}, ...] instead of ["X", ...]."""
    body = [{
        "conditionId": "0xC1", "slug": "p1", "question": "q",
        "category": "",
        "tags": [{"label": "Politics"}, {"label": "Trump"}],
        "endDateIso": "2026-06-01T12:00:00Z",
    }]
    markets = parse_poly_gamma_markets_response(body, category_config=_poly_cfg())
    assert markets[0].bucket == "Politics"
    assert markets[0].tags == ["Politics", "Trump"]


def test_poly_parser_drops_market_with_missing_endDate():
    body = [
        {"conditionId": "0xC1", "slug": "p1", "question": "q1"},
        {"conditionId": "0xC2", "slug": "p2", "question": "q2",
         "endDateIso": "2026-06-01T12:00:00Z"},
    ]
    markets = parse_poly_gamma_markets_response(body, category_config=_poly_cfg())
    assert [m.condition_id for m in markets] == ["0xC2"]


def test_poly_parser_assigns_economics_bucket_from_finance_category():
    """Cross-platform alias: Polymarket 'Finance' maps to the Economics bucket."""
    body = [{
        "conditionId": "0xC1", "slug": "p1", "question": "q",
        "category": "Finance",
        "endDateIso": "2026-06-01T12:00:00Z",
    }]
    markets = parse_poly_gamma_markets_response(body, category_config=_poly_cfg())
    assert markets[0].bucket == "Economics"


import httpx

from ai_matcher.ingestion import Ingestion


class _StubHttp:
    """Minimal httpx.Client stand-in: maps URL → list-of-responses."""

    def __init__(self, responses_by_url: dict[str, list[dict | list]]):
        self._responses = {url: list(rs) for url, rs in responses_by_url.items()}
        self.call_log: list[str] = []

    def get(self, url: str):
        self.call_log.append(url)
        for prefix, queue in self._responses.items():
            if url.startswith(prefix) and queue:
                body = queue.pop(0)
                return _StubResponse(body)
        # Default: empty body so loops terminate.
        return _StubResponse([] if "polymarket" in url else {})


class _StubResponse:
    def __init__(self, body):
        self._body = body
    def raise_for_status(self):
        pass
    def json(self):
        return self._body


def test_poly_pagination_walks_offset():
    page1 = [{"conditionId": f"0xA{i}", "slug": f"a{i}", "question": "q",
              "endDateIso": "2026-06-01T12:00:00Z"} for i in range(500)]
    page2 = [{"conditionId": f"0xB{i}", "slug": f"b{i}", "question": "q",
              "endDateIso": "2026-06-01T12:00:00Z"} for i in range(500)]
    page3 = []
    stub = _StubHttp({
        "https://gamma-api.polymarket.com/markets": [page1, page2, page3],
    })
    ing = Ingestion(http=stub, poly_fetch_limit=2000, min_liquidity_usd=0.0)
    markets = ing.fetch_poly()
    assert len(markets) == 1000
    assert len(stub.call_log) == 3
    assert "offset=0" in stub.call_log[0]
    assert "offset=500" in stub.call_log[1]
    assert "offset=1000" in stub.call_log[2]


def test_poly_pagination_stops_when_cap_reached():
    page1 = [{"conditionId": f"0xA{i}", "slug": f"a{i}", "question": "q",
              "endDateIso": "2026-06-01T12:00:00Z"} for i in range(500)]
    stub = _StubHttp({
        "https://gamma-api.polymarket.com/markets": [page1, page1, page1],
    })
    ing = Ingestion(http=stub, poly_fetch_limit=500, min_liquidity_usd=0.0)
    markets = ing.fetch_poly()
    assert len(stub.call_log) == 1
    assert len(markets) == 500


def test_kalshi_pagination_walks_cursor():
    events_page1 = {
        "events": [{"event_ticker": f"E{i}", "title": "t"} for i in range(200)],
        "cursor": "CUR-A",
    }
    events_page2 = {
        "events": [{"event_ticker": f"F{i}", "title": "t"} for i in range(150)],
        "cursor": "",
    }
    market_page = {"markets": [
        {"ticker": "M1", "title": "t", "category": "Politics",
         "close_time": "2026-06-01T12:00:00Z"}
    ]}
    stub = _StubHttp({
        "https://api.elections.kalshi.com/trade-api/v2/events": [events_page1, events_page2],
        "https://api.elections.kalshi.com/trade-api/v2/markets": [market_page] * 350,
    })
    ing = Ingestion(http=stub, max_kalshi_events=500, min_liquidity_usd=0.0)
    markets = ing.fetch_kalshi()
    assert len(markets) == 350
    events_calls = [c for c in stub.call_log if "/events" in c]
    assert len(events_calls) == 2
    assert "cursor=" not in events_calls[0]
    assert "cursor=CUR-A" in events_calls[1]


def test_kalshi_pagination_stops_at_cap():
    events_page = {
        "events": [{"event_ticker": f"E{i}", "title": "t"} for i in range(200)],
        "cursor": "CUR-A",
    }
    market_page = {"markets": [
        {"ticker": "M1", "title": "t", "category": "Politics",
         "close_time": "2026-06-01T12:00:00Z"}
    ]}
    stub = _StubHttp({
        "https://api.elections.kalshi.com/trade-api/v2/events": [events_page] * 5,
        "https://api.elections.kalshi.com/trade-api/v2/markets": [market_page] * 200,
    })
    ing = Ingestion(http=stub, max_kalshi_events=200, min_liquidity_usd=0.0)
    ing.fetch_kalshi()
    events_calls = [c for c in stub.call_log if "/events" in c]
    assert len(events_calls) == 1


def test_kalshi_volume_proxy_drops_when_liquidity_unknown_and_volume_low():
    body = {"markets": [
        {"ticker": "K1", "title": "low vol", "category": "Politics",
         "close_time": "2026-06-01T12:00:00Z",
         "liquidity": None, "volume": 50_000},
        {"ticker": "K2", "title": "high vol", "category": "Politics",
         "close_time": "2026-06-01T12:00:00Z",
         "liquidity": None, "volume": 200_000},
    ]}
    markets = parse_kalshi_markets_response(
        body, min_liquidity_usd=0.0, min_volume_usd=1000.0,
        category_config=_kalshi_cfg(),
    )
    assert [m.ticker for m in markets] == ["K2"]


def test_kalshi_volume_proxy_passes_when_both_unknown():
    body = {"markets": [
        {"ticker": "K1", "title": "both unknown", "category": "Politics",
         "close_time": "2026-06-01T12:00:00Z",
         "liquidity": None, "volume": None},
    ]}
    markets = parse_kalshi_markets_response(
        body, min_liquidity_usd=0.0, min_volume_usd=1000.0,
        category_config=_kalshi_cfg(),
    )
    assert [m.ticker for m in markets] == ["K1"]


def test_kalshi_volume_proxy_does_not_apply_when_liquidity_known():
    """If liquidity is known and above floor, volume floor doesn't matter."""
    body = {"markets": [
        {"ticker": "K1", "title": "rich liquidity", "category": "Politics",
         "close_time": "2026-06-01T12:00:00Z",
         "liquidity": 50_000, "volume": 100},
    ]}
    markets = parse_kalshi_markets_response(
        body, min_liquidity_usd=100.0, min_volume_usd=1000.0,
        category_config=_kalshi_cfg(),
    )
    assert [m.ticker for m in markets] == ["K1"]


def test_kalshi_parser_uses_event_category_when_market_has_none():
    """Kalshi per-event /markets returns category=null on individual markets;
    the parser must fall back to the event-level category."""
    body = {"markets": [
        {"ticker": "K1", "title": "t",
         "close_time": "2026-06-01T12:00:00Z",
         "category": None},  # market-level category is null in the real API
    ]}
    cfg = CategoryConfig(
        buckets={"Politics": BucketDef(kalshi=["Elections"], poly=["Politics"], tolerance_days=60)},
        default_tolerance_days=30,
    )
    markets = parse_kalshi_markets_response(
        body, event_category="Elections", category_config=cfg,
    )
    assert markets[0].bucket == "Politics"
    assert markets[0].category == "Elections"


def test_kalshi_parser_market_category_wins_over_event_category():
    """If the market explicitly carries a category, prefer it over the event one."""
    body = {"markets": [
        {"ticker": "K1", "title": "t",
         "close_time": "2026-06-01T12:00:00Z",
         "category": "Sports"},
    ]}
    cfg = CategoryConfig(
        buckets={
            "Sports":   BucketDef(kalshi=["Sports"],   poly=["Sports"],   tolerance_days=2),
            "Politics": BucketDef(kalshi=["Elections"], poly=["Politics"], tolerance_days=60),
        },
        default_tolerance_days=30,
    )
    markets = parse_kalshi_markets_response(
        body, event_category="Elections", category_config=cfg,
    )
    assert markets[0].bucket == "Sports"


def test_parse_kalshi_event_extracts_category():
    raw = {"event_ticker": "E1", "title": "Some event", "category": "Elections"}
    parsed = parse_kalshi_event(raw)
    assert parsed["category"] == "Elections"

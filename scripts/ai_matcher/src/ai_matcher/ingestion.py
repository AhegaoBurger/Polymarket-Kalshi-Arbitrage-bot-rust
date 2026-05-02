"""Market ingestion: Kalshi v2 REST + Polymarket Gamma REST.

Direct httpx calls, no pmxt dependency. Mirrors the Rust ingestion path:
- Kalshi: `/events?status=open` → per-event `/markets?event_ticker=...` for full
  text fields. The bulk `/markets?limit=N` endpoint returns minimal info (no
  description, no resolution criteria) and is dominated by multivariate parlay
  markets (`KXMV*`); we skip those.
- Polymarket: `/markets?order=liquidity&ascending=false` to put the most liquid
  markets first.

Both sides apply a min-liquidity floor (env-configurable) — illiquid markets
aren't tradeable arb targets, and embedding them wastes the LLM-verifier budget.
"""

from __future__ import annotations

import hashlib
import json
import os
from dataclasses import dataclass, field
from datetime import datetime, timezone

import httpx

from ai_matcher.categories import CategoryConfig, resolve_bucket

KALSHI_API_BASE = "https://api.elections.kalshi.com/trade-api/v2"
GAMMA_API_BASE = "https://gamma-api.polymarket.com"

DEFAULT_TIMEOUT = 15.0

# Defaults (override via env vars in production):
DEFAULT_MIN_LIQUIDITY_USD = 100.0       # MIN_LIQUIDITY_USD
DEFAULT_MIN_VOLUME_USD = 1000.0         # MIN_VOLUME_USD — Kalshi liquidity proxy
DEFAULT_MAX_KALSHI_EVENTS = 2000        # was 200 — INGEST_KALSHI_MAX_EVENTS
DEFAULT_POLY_FETCH_LIMIT = 10000        # was 500 — INGEST_POLY_LIMIT


def parse_close_time_utc(raw: dict, platform: str) -> datetime | None:
    """Parse a raw market dict's expiry timestamp to a tz-aware UTC datetime.

    Returns None for missing, malformed, or naive (timezone-less) inputs.
    Caller drops the market when None is returned.
    """
    if platform == "kalshi":
        s = raw.get("close_time")
    else:  # polymarket — endDate is tz-aware; endDateIso is date-only on Gamma
        s = raw.get("endDate") or raw.get("endDateIso")
    if not s:
        return None
    try:
        dt = datetime.fromisoformat(s.replace("Z", "+00:00"))
    except (ValueError, TypeError):
        return None
    if dt.tzinfo is None:
        return None
    return dt.astimezone(timezone.utc)


@dataclass
class Market:
    """Normalized market record consumed by embedder + verifier."""
    platform: str  # "kalshi" | "polymarket"
    ticker: str  # kalshi market ticker, OR poly slug for polymarket
    title: str
    description: str = ""
    resolution_criteria: str = ""
    outcomes: list[str] = field(default_factory=list)
    category: str = ""
    tags: list[str] = field(default_factory=list)               # NEW: platform-side tag list (Polymarket-only meaningfully)
    bucket: str = "Unknown"                                      # NEW: cross-platform bucket name from resolve_bucket
    close_time_utc: datetime | None = None                       # NEW: tz-aware UTC expiry; None means "not parsed yet"
    # Liquidity in USD (normalized — Kalshi's native cents are divided by 100):
    liquidity_usd: float = 0.0
    volume_usd: float = 0.0
    # Poly-only fields:
    condition_id: str = ""
    poly_yes_token: str = ""
    poly_no_token: str = ""
    # Kalshi-only:
    event_ticker: str = ""

    def text_for_embedding(self) -> str:
        """Concatenated text used to compute the embedding + content hash."""
        return "\n".join([
            self.title,
            self.description,
            self.resolution_criteria,
            " | ".join(self.outcomes),
        ])

    def content_hash(self) -> str:
        return content_hash(
            self.title,
            self.description,
            self.resolution_criteria,
            "|".join(self.outcomes),
        )


@dataclass
class IngestionResult:
    kalshi: list[Market]
    poly: list[Market]


def content_hash(*parts: str) -> str:
    """Stable SHA-256 of joined parts. Used as the embedder + verifier cache key."""
    h = hashlib.sha256()
    for p in parts:
        h.update(p.encode("utf-8"))
        h.update(b"\x1f")  # unit separator
    return h.hexdigest()


# === Parsers (pure, testable without network) =============================

def parse_kalshi_event(raw: dict) -> dict | None:
    """Extract minimum fields from a Kalshi event JSON. Returns None for events
    we explicitly skip (multivariate parlay markets)."""
    ticker = raw.get("event_ticker", "") or ""
    if not ticker:
        return None
    # KXMV* are multivariate parlay markets — junk for arb pairing.
    if ticker.startswith("KXMV"):
        return None
    return {
        "event_ticker": ticker,
        "title": raw.get("title", "") or "",
        "sub_title": raw.get("sub_title", "") or "",
        "category": raw.get("category", "") or "",
    }


def parse_kalshi_markets_response(
    body: dict,
    event_title: str = "",
    event_category: str = "",
    min_liquidity_usd: float = 0.0,
    min_volume_usd: float = 0.0,
    category_config: CategoryConfig | None = None,
) -> list[Market]:
    """Parse a `/markets?event_ticker=...` response into our Market objects.

    Markets without a parseable UTC close_time are dropped. When `category_config`
    is None, every market is bucketed Unknown (prefilter disabled).
    Liquidity floor: drop if known and below; if unknown, fall back to volume floor.
    """
    out: list[Market] = []
    for m in body.get("markets", []) or []:
        if not m.get("ticker"):
            continue

        liq_cents = m.get("liquidity")
        vol_cents = m.get("volume")
        liq_usd = float(liq_cents) / 100.0 if liq_cents is not None else 0.0
        vol_usd = float(vol_cents) / 100.0 if vol_cents is not None else 0.0

        liq_known = liq_cents is not None
        vol_known = vol_cents is not None
        if liq_known and liq_usd < min_liquidity_usd:
            continue
        if not liq_known and vol_known and vol_usd < min_volume_usd:
            continue
        # both unknown → pass through (rare; verifier catches obvious junk)

        close_utc = parse_close_time_utc(m, platform="kalshi")
        if close_utc is None:
            continue

        title = m.get("title", "") or ""
        sub = m.get("subtitle") or m.get("yes_sub_title") or ""
        rules = m.get("rules_primary", "") or ""
        category = m.get("category", "") or event_category
        bucket = (
            resolve_bucket(category_config, platform="kalshi", category=category, tags=[])
            if category_config is not None
            else "Unknown"
        )

        out.append(Market(
            platform="kalshi",
            ticker=m["ticker"],
            event_ticker=m.get("event_ticker", "") or "",
            title=title,
            description=(event_title + ((" — " + sub) if sub else "")).strip(" —"),
            resolution_criteria=rules,
            outcomes=[sub] if sub else [],
            category=category,
            tags=[],
            bucket=bucket,
            close_time_utc=close_utc,
            liquidity_usd=liq_usd,
            volume_usd=vol_usd,
        ))
    return out


def _parse_poly_tags(raw: list | None) -> list[str]:
    """Tolerate either ['Politics', ...] or [{'label': 'Politics'}, ...]."""
    out: list[str] = []
    for t in raw or []:
        if isinstance(t, dict):
            label = t.get("label") or t.get("name")
            if label:
                out.append(str(label))
        elif isinstance(t, str) and t:
            out.append(t)
    return out


def parse_poly_gamma_markets_response(
    body: list[dict],
    min_liquidity_usd: float = 0.0,
    category_config: CategoryConfig | None = None,
) -> list[Market]:
    """Parse a Polymarket Gamma `/markets` response.

    Markets without a parseable UTC end date are dropped. When `category_config`
    is None, every market is bucketed Unknown (prefilter disabled).
    """
    out: list[Market] = []
    for m in body:
        if m.get("closed") is True or m.get("active") is False:
            continue
        cid = m.get("conditionId", "") or ""
        if not cid:
            continue
        liq = _to_float(m.get("liquidity") or m.get("liquidityNum") or 0)
        if liq < min_liquidity_usd:
            continue
        vol = _to_float(m.get("volume") or m.get("volumeNum") or 0)

        close_utc = parse_close_time_utc(m, platform="polymarket")
        if close_utc is None:
            continue

        outcomes_str = m.get("outcomes") or "[]"
        try:
            outcomes = json.loads(outcomes_str) if isinstance(outcomes_str, str) else outcomes_str
        except json.JSONDecodeError:
            outcomes = []
        toks_str = m.get("clobTokenIds") or "[]"
        try:
            toks = json.loads(toks_str) if isinstance(toks_str, str) else toks_str
        except json.JSONDecodeError:
            toks = []

        category = m.get("category", "") or ""
        tags = _parse_poly_tags(m.get("tags"))
        bucket = (
            resolve_bucket(category_config, platform="polymarket", category=category, tags=tags)
            if category_config is not None
            else "Unknown"
        )

        out.append(Market(
            platform="polymarket",
            ticker=m.get("slug", "") or "",
            title=m.get("question", "") or "",
            description=m.get("description", "") or "",
            resolution_criteria=m.get("description", "") or "",
            outcomes=outcomes if isinstance(outcomes, list) else [],
            category=category,
            tags=tags,
            bucket=bucket,
            close_time_utc=close_utc,
            liquidity_usd=liq,
            volume_usd=vol,
            condition_id=cid,
            poly_yes_token=toks[0] if len(toks) > 0 else "",
            poly_no_token=toks[1] if len(toks) > 1 else "",
        ))
    return out


def _to_float(v) -> float:
    """Coerce a JSON number-or-string to float, returning 0.0 on failure."""
    if v is None:
        return 0.0
    try:
        return float(v)
    except (TypeError, ValueError):
        return 0.0


# === Live fetchers (used only by `pipeline.run`, not by unit tests) ========

class Ingestion:
    """Live REST ingestion mirroring the Rust adapters' approach.

    Configurable via env (read at construction time):
      MIN_LIQUIDITY_USD           — default 100.0
      INGEST_KALSHI_MAX_EVENTS    — default 200
      INGEST_POLY_LIMIT           — default 500
    """

    def __init__(
        self,
        http: httpx.Client | None = None,
        min_liquidity_usd: float | None = None,
        min_volume_usd: float | None = None,
        max_kalshi_events: int | None = None,
        poly_fetch_limit: int | None = None,
        category_config: CategoryConfig | None = None,
    ) -> None:
        self._http = http or httpx.Client(timeout=DEFAULT_TIMEOUT)
        self.min_liquidity_usd = (
            min_liquidity_usd
            if min_liquidity_usd is not None
            else float(os.environ.get("MIN_LIQUIDITY_USD", DEFAULT_MIN_LIQUIDITY_USD))
        )
        self.min_volume_usd = (
            min_volume_usd
            if min_volume_usd is not None
            else float(os.environ.get("MIN_VOLUME_USD", DEFAULT_MIN_VOLUME_USD))
        )
        self.max_kalshi_events = (
            max_kalshi_events
            if max_kalshi_events is not None
            else int(os.environ.get("INGEST_KALSHI_MAX_EVENTS", DEFAULT_MAX_KALSHI_EVENTS))
        )
        self.poly_fetch_limit = (
            poly_fetch_limit
            if poly_fetch_limit is not None
            else int(os.environ.get("INGEST_POLY_LIMIT", DEFAULT_POLY_FETCH_LIMIT))
        )
        self.category_config = category_config

    def fetch_all(self) -> IngestionResult:
        return IngestionResult(
            kalshi=self.fetch_kalshi(),
            poly=self.fetch_poly(),
        )

    def fetch_kalshi(self) -> list[Market]:
        """Walk Kalshi events with cursor pagination, then per-event /markets walk."""
        cursor = ""
        raw_events: list[dict] = []
        while len(raw_events) < self.max_kalshi_events:
            url = f"{KALSHI_API_BASE}/events?limit=200&status=open"
            if cursor:
                url += f"&cursor={cursor}"
            try:
                resp = self._http.get(url)
                resp.raise_for_status()
            except httpx.HTTPError:
                break
            body = resp.json()
            page = body.get("events", []) or []
            if not page:
                break
            raw_events.extend(page)
            cursor = body.get("cursor", "") or ""
            if not cursor:
                break

        kept_events = [parse_kalshi_event(e) for e in raw_events]
        kept_events = [e for e in kept_events if e is not None]
        kept_events = kept_events[: self.max_kalshi_events]

        out: list[Market] = []
        for ev in kept_events:
            try:
                m_resp = self._http.get(
                    f"{KALSHI_API_BASE}/markets"
                    f"?event_ticker={ev['event_ticker']}&limit=200"
                )
                m_resp.raise_for_status()
            except httpx.HTTPError:
                continue
            out.extend(
                parse_kalshi_markets_response(
                    m_resp.json(),
                    event_title=ev["title"],
                    event_category=ev.get("category", "") or "",
                    min_liquidity_usd=self.min_liquidity_usd,
                    min_volume_usd=self.min_volume_usd,
                    category_config=self.category_config,
                )
            )
        return out

    def fetch_poly(self) -> list[Market]:
        """Fetch Polymarket markets sorted by liquidity desc, paginate via offset."""
        out: list[Market] = []
        page_size = 500
        for offset in range(0, self.poly_fetch_limit, page_size):
            resp = self._http.get(
                f"{GAMMA_API_BASE}/markets"
                f"?limit={page_size}&offset={offset}"
                f"&active=true&closed=false"
                f"&order=liquidity&ascending=false"
            )
            resp.raise_for_status()
            body = resp.json() if isinstance(resp.json(), list) else []
            if not body:
                break
            out.extend(parse_poly_gamma_markets_response(
                body,
                min_liquidity_usd=self.min_liquidity_usd,
                category_config=self.category_config,
            ))
        return out

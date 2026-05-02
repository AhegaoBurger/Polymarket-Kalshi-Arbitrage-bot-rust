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
from datetime import datetime

import httpx

KALSHI_API_BASE = "https://api.elections.kalshi.com/trade-api/v2"
GAMMA_API_BASE = "https://gamma-api.polymarket.com"

DEFAULT_TIMEOUT = 15.0

# Defaults (override via env vars in production):
DEFAULT_MIN_LIQUIDITY_USD = 100.0       # MIN_LIQUIDITY_USD
DEFAULT_MAX_KALSHI_EVENTS = 200         # INGEST_KALSHI_MAX_EVENTS
DEFAULT_POLY_FETCH_LIMIT = 500          # INGEST_POLY_LIMIT


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
    }


def parse_kalshi_markets_response(
    body: dict,
    event_title: str = "",
    min_liquidity_usd: float = 0.0,
) -> list[Market]:
    """Parse a `/markets?event_ticker=...` response into our Market objects.

    `event_title` is folded into description so per-event context survives
    embedding even when individual markets share a generic ticker template.
    """
    out: list[Market] = []
    for m in body.get("markets", []) or []:
        if not m.get("ticker"):
            continue
        # Kalshi returns liquidity/volume in cents — but only over authenticated
        # sessions. The public browse endpoint always returns null. When the
        # field is *known* and below threshold, drop; when unknown, pass through
        # (the Polymarket-side liquidity filter + cosine retrieval will still
        # bound the join).
        liq_cents = m.get("liquidity")
        vol_cents = m.get("volume")
        liq_usd = float(liq_cents) / 100.0 if liq_cents is not None else 0.0
        vol_usd = float(vol_cents) / 100.0 if vol_cents is not None else 0.0
        if liq_cents is not None and liq_usd < min_liquidity_usd:
            continue

        # Compose embedding text from every field that carries semantic content.
        title = m.get("title", "") or ""
        sub = m.get("subtitle") or m.get("yes_sub_title") or ""
        rules = m.get("rules_primary", "") or ""
        out.append(Market(
            platform="kalshi",
            ticker=m["ticker"],
            event_ticker=m.get("event_ticker", "") or "",
            title=title,
            description=(event_title + ((" — " + sub) if sub else "")).strip(" —"),
            resolution_criteria=rules,
            outcomes=[sub] if sub else [],
            category=m.get("category", "") or "",
            liquidity_usd=liq_usd,
            volume_usd=vol_usd,
        ))
    return out


def parse_poly_gamma_markets_response(
    body: list[dict],
    min_liquidity_usd: float = 0.0,
) -> list[Market]:
    """Parse a Polymarket Gamma `/markets` response. Polymarket reports
    liquidity and volume directly in USD (as floats or numeric strings)."""
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
        out.append(Market(
            platform="polymarket",
            ticker=m.get("slug", "") or "",
            title=m.get("question", "") or "",
            description=m.get("description", "") or "",
            resolution_criteria=m.get("description", "") or "",
            outcomes=outcomes if isinstance(outcomes, list) else [],
            category=m.get("category", "") or "",
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
        max_kalshi_events: int | None = None,
        poly_fetch_limit: int | None = None,
    ) -> None:
        self._http = http or httpx.Client(timeout=DEFAULT_TIMEOUT)
        self.min_liquidity_usd = (
            min_liquidity_usd
            if min_liquidity_usd is not None
            else float(os.environ.get("MIN_LIQUIDITY_USD", DEFAULT_MIN_LIQUIDITY_USD))
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

    def fetch_all(self) -> IngestionResult:
        return IngestionResult(
            kalshi=self.fetch_kalshi(),
            poly=self.fetch_poly(),
        )

    def fetch_kalshi(self) -> list[Market]:
        """Walk Kalshi events, then per-event markets — same path Rust takes.

        Skips multivariate parlay events (`KXMV*`) and applies the liquidity floor.
        """
        events_resp = self._http.get(
            f"{KALSHI_API_BASE}/events"
            f"?limit={min(self.max_kalshi_events, 200)}&status=open"
        )
        events_resp.raise_for_status()
        events_body = events_resp.json()
        raw_events = events_body.get("events", []) or []

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
                # One bad event shouldn't tank the whole ingestion.
                continue
            out.extend(
                parse_kalshi_markets_response(
                    m_resp.json(),
                    event_title=ev["title"],
                    min_liquidity_usd=self.min_liquidity_usd,
                )
            )
        return out

    def fetch_poly(self) -> list[Market]:
        """Fetch Polymarket markets sorted by liquidity desc, take top N.

        Gamma's `order=liquidity` puts the most liquid markets first; combined
        with `active=true&closed=false`, this gives us the arb-tradeable cohort
        without scanning the entire long tail.
        """
        resp = self._http.get(
            f"{GAMMA_API_BASE}/markets"
            f"?limit={self.poly_fetch_limit}"
            f"&active=true&closed=false"
            f"&order=liquidity&ascending=false"
        )
        resp.raise_for_status()
        body = resp.json() if isinstance(resp.json(), list) else []
        return parse_poly_gamma_markets_response(
            body, min_liquidity_usd=self.min_liquidity_usd
        )

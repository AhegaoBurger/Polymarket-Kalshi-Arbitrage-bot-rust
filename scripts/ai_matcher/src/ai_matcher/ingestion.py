"""Market ingestion: Kalshi v2 REST + Polymarket Gamma REST.

Direct httpx calls, no pmxt dependency. Documented as the spec's anticipated
"direct-REST fallback" (Appendix C).
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field

import httpx

KALSHI_API_BASE = "https://api.elections.kalshi.com/trade-api/v2"
GAMMA_API_BASE = "https://gamma-api.polymarket.com"

DEFAULT_TIMEOUT = 15.0


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

def parse_kalshi_markets_response(body: dict) -> list[Market]:
    out: list[Market] = []
    for m in body.get("markets", []) or []:
        if not m.get("ticker"):
            continue
        outcomes_raw = m.get("yes_sub_title") or m.get("subtitle") or ""
        out.append(Market(
            platform="kalshi",
            ticker=m["ticker"],
            event_ticker=m.get("event_ticker", ""),
            title=m.get("title", ""),
            description=m.get("subtitle", "") or "",
            resolution_criteria=m.get("rules_primary", "") or "",
            outcomes=[outcomes_raw] if outcomes_raw else [],
            category=m.get("category", "") or "",
        ))
    return out


def parse_poly_gamma_markets_response(body: list[dict]) -> list[Market]:
    out: list[Market] = []
    for m in body:
        if m.get("closed") is True or m.get("active") is False:
            continue
        cid = m.get("conditionId", "") or ""
        if not cid:
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
        out.append(Market(
            platform="polymarket",
            ticker=m.get("slug", ""),
            title=m.get("question", "") or "",
            description=m.get("description", "") or "",
            resolution_criteria=m.get("description", "") or "",
            outcomes=outcomes if isinstance(outcomes, list) else [],
            category=m.get("category", "") or "",
            condition_id=cid,
            poly_yes_token=toks[0] if len(toks) > 0 else "",
            poly_no_token=toks[1] if len(toks) > 1 else "",
        ))
    return out


# === Live fetchers (used only by `pipeline.run`, not by unit tests) ========

class Ingestion:
    """Live REST ingestion. Tests bypass this and call the parsers directly."""

    def __init__(self, http: httpx.Client | None = None) -> None:
        self._http = http or httpx.Client(timeout=DEFAULT_TIMEOUT)

    def fetch_all(self) -> IngestionResult:
        return IngestionResult(
            kalshi=self.fetch_kalshi(),
            poly=self.fetch_poly(),
        )

    def fetch_kalshi(self, limit: int = 1000) -> list[Market]:
        resp = self._http.get(f"{KALSHI_API_BASE}/markets?limit={limit}&status=open")
        resp.raise_for_status()
        return parse_kalshi_markets_response(resp.json())

    def fetch_poly(self, limit: int = 1000) -> list[Market]:
        resp = self._http.get(
            f"{GAMMA_API_BASE}/markets?limit={limit}&active=true&closed=false"
        )
        resp.raise_for_status()
        body = resp.json() if isinstance(resp.json(), list) else []
        return parse_poly_gamma_markets_response(body)

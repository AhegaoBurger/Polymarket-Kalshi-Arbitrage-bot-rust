# Matching Prefilter, Ingestion Expansion, and LLM Swap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add structural prefilters (category bucketing + date overlap) and ingestion expansion (pagination + volume proxy) to the AI matching sidecar, swap the verifier from Anthropic-direct to LiteLLM, and make the pipeline funnel observable end-to-end via per-stage counters and an interactive HTML report.

**Architecture:** Bucket assignment + UTC parsing happen at ingestion. Per-bucket HNSW indexes route Kalshi queries to category-matched Polymarket subsets. Date overlap predicate runs post-retrieval, pre-verifier. LiteLLM normalizes provider tool-use shapes. New per-stage counters surface the funnel; new audit fields (cosine, Δdays, bucket, cost) flow into both JSONL and HTML report. HTML report becomes interactively sortable + filterable.

**Tech Stack:** Python 3.11+, `uv` for env management, `httpx` for ingestion, `sentence-transformers` for embeddings, `hnswlib` for ANN, `litellm` for LLM provider abstraction (replaces `anthropic`), `Jinja2` for HTML templating, `pytest` + `monkeypatch` for tests.

**Working directory for all Python work:** `scripts/ai_matcher`. All `uv` and `pytest` commands run from there.

**Spec:** `docs/superpowers/specs/2026-05-02-matching-prefilter-and-llm-swap-design.md`.

---

## Task 1: Create `config/category_equivalence.json`

**Files:**
- Create: `config/category_equivalence.json`

- [ ] **Step 1: Write the config file**

Create `config/category_equivalence.json` with the initial bucket map agreed in §1 of the spec:

```json
{
  "buckets": {
    "Politics":     { "kalshi": ["Politics"],            "poly": ["Politics"],             "tolerance_days": 60 },
    "Sports":       { "kalshi": ["Sports"],              "poly": ["Sports"],               "tolerance_days": 2  },
    "Weather":      { "kalshi": ["Climate and Weather"], "poly": ["Weather"],              "tolerance_days": 2  },
    "Crypto":       { "kalshi": ["Crypto", "Bitcoin"],   "poly": ["Crypto", "Bitcoin"],    "tolerance_days": 1  },
    "Economics":    { "kalshi": ["Economics"],           "poly": ["Finance", "Economics"], "tolerance_days": 14 },
    "Tech":         { "kalshi": ["Tech"],                "poly": ["Tech"],                 "tolerance_days": 30 },
    "Mentions":     { "kalshi": ["Mentions"],            "poly": ["Mentions"],             "tolerance_days": 7  },
    "Culture":      { "kalshi": ["Culture"],             "poly": ["Culture"],              "tolerance_days": 30 },
    "Geopolitical": { "kalshi": ["World"],               "poly": ["Geopolitical"],         "tolerance_days": 30 }
  },
  "default_tolerance_days": 30
}
```

- [ ] **Step 2: Validate the JSON parses**

Run: `python -c "import json; json.load(open('config/category_equivalence.json'))"` from repo root.
Expected: no output, exit code 0.

- [ ] **Step 3: Commit**

```bash
git add config/category_equivalence.json
git commit -m "Add category equivalence config for matching prefilter"
```

---

## Task 2: Create `categories.py` with `load_category_config`

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/categories.py`
- Create: `scripts/ai_matcher/tests/test_categories.py`

- [ ] **Step 1: Write the failing tests**

Create `scripts/ai_matcher/tests/test_categories.py`:

```python
"""Tests for categories.py — config loading + bucket resolution."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from ai_matcher.categories import CategoryConfig, load_category_config


def write_config(tmp_path: Path, body: dict) -> Path:
    p = tmp_path / "category_equivalence.json"
    p.write_text(json.dumps(body))
    return p


def test_loads_valid_config(tmp_path: Path):
    p = write_config(tmp_path, {
        "buckets": {
            "Politics": {"kalshi": ["Politics"], "poly": ["Politics"], "tolerance_days": 60},
        },
        "default_tolerance_days": 30,
    })
    cfg = load_category_config(p)
    assert isinstance(cfg, CategoryConfig)
    assert "Politics" in cfg.buckets
    assert cfg.buckets["Politics"].tolerance_days == 60
    assert cfg.buckets["Politics"].kalshi == ["Politics"]
    assert cfg.buckets["Politics"].poly == ["Politics"]
    assert cfg.default_tolerance_days == 30


def test_missing_file_returns_empty_config(tmp_path: Path):
    cfg = load_category_config(tmp_path / "does-not-exist.json")
    assert cfg.buckets == {}
    assert cfg.default_tolerance_days == 30  # falls back to safe default


def test_malformed_json_returns_empty_config(tmp_path: Path):
    p = tmp_path / "category_equivalence.json"
    p.write_text("this is not json")
    cfg = load_category_config(p)
    assert cfg.buckets == {}
    assert cfg.default_tolerance_days == 30
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_categories.py -v`
Expected: FAIL with `ModuleNotFoundError: No module named 'ai_matcher.categories'`.

- [ ] **Step 3: Write the minimal implementation**

Create `scripts/ai_matcher/src/ai_matcher/categories.py`:

```python
"""Category equivalence config loading + bucket resolution.

Spec: docs/superpowers/specs/2026-05-02-matching-prefilter-and-llm-swap-design.md §1
"""

from __future__ import annotations

import json
import logging
from dataclasses import dataclass, field
from pathlib import Path

logger = logging.getLogger(__name__)


@dataclass
class BucketDef:
    """One bucket: which Kalshi/Poly category strings map to it, and the date tolerance."""
    kalshi: list[str] = field(default_factory=list)
    poly: list[str] = field(default_factory=list)
    tolerance_days: int = 30


@dataclass
class CategoryConfig:
    buckets: dict[str, BucketDef] = field(default_factory=dict)
    default_tolerance_days: int = 30


def load_category_config(path: Path) -> CategoryConfig:
    """Load the category equivalence JSON at `path`. Missing or malformed → empty config (no prefilter)."""
    if not path.exists():
        logger.warning("category_equivalence config not found at %s; prefilter disabled", path)
        return CategoryConfig()
    try:
        raw = json.loads(path.read_text())
    except json.JSONDecodeError as e:
        logger.warning("category_equivalence config malformed (%s); prefilter disabled", e)
        return CategoryConfig()

    buckets: dict[str, BucketDef] = {}
    for name, cfg in (raw.get("buckets") or {}).items():
        buckets[name] = BucketDef(
            kalshi=list(cfg.get("kalshi") or []),
            poly=list(cfg.get("poly") or []),
            tolerance_days=int(cfg.get("tolerance_days", 30)),
        )
    default_tol = int(raw.get("default_tolerance_days", 30))
    return CategoryConfig(buckets=buckets, default_tolerance_days=default_tol)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_categories.py -v`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/categories.py scripts/ai_matcher/tests/test_categories.py
git commit -m "Add category config loader for matching prefilter"
```

---

## Task 3: Add `resolve_bucket` to `categories.py`

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/categories.py`
- Modify: `scripts/ai_matcher/tests/test_categories.py`

- [ ] **Step 1: Write the failing tests**

Append to `scripts/ai_matcher/tests/test_categories.py`:

```python
from ai_matcher.categories import resolve_bucket


def _cfg() -> CategoryConfig:
    return CategoryConfig(
        buckets={
            "Politics": BucketDef(kalshi=["Politics"], poly=["Politics"], tolerance_days=60),
            "Crypto":   BucketDef(kalshi=["Crypto", "Bitcoin"], poly=["Crypto"], tolerance_days=1),
            "Economics": BucketDef(kalshi=["Economics"], poly=["Finance", "Economics"], tolerance_days=14),
        },
        default_tolerance_days=30,
    )


def test_resolves_exact_kalshi_category():
    assert resolve_bucket(_cfg(), platform="kalshi", category="Politics", tags=[]) == "Politics"


def test_resolves_exact_poly_category():
    assert resolve_bucket(_cfg(), platform="polymarket", category="Politics", tags=[]) == "Politics"


def test_case_insensitive():
    assert resolve_bucket(_cfg(), platform="kalshi", category="POLITICS", tags=[]) == "Politics"
    assert resolve_bucket(_cfg(), platform="kalshi", category="politics", tags=[]) == "Politics"


def test_whitespace_trimmed():
    assert resolve_bucket(_cfg(), platform="kalshi", category="  Politics  ", tags=[]) == "Politics"


def test_alias_match():
    """'Bitcoin' is an alias for the Crypto bucket on the Kalshi side."""
    assert resolve_bucket(_cfg(), platform="kalshi", category="Bitcoin", tags=[]) == "Crypto"


def test_poly_tag_fallback_when_category_empty():
    """When Polymarket category is empty, fall back to the first usable tag."""
    assert resolve_bucket(_cfg(), platform="polymarket", category="", tags=["Politics"]) == "Politics"


def test_kalshi_does_not_use_tag_fallback():
    """Tag fallback is Polymarket-only — Kalshi tags get folded into category upstream."""
    assert resolve_bucket(_cfg(), platform="kalshi", category="", tags=["Politics"]) == "Unknown"


def test_unknown_when_no_match():
    assert resolve_bucket(_cfg(), platform="kalshi", category="Astronomy", tags=[]) == "Unknown"


def test_unknown_when_category_and_tags_empty():
    assert resolve_bucket(_cfg(), platform="polymarket", category="", tags=[]) == "Unknown"


def test_cross_platform_alias_split():
    """Economics on Kalshi vs Finance on Poly both resolve to Economics bucket."""
    assert resolve_bucket(_cfg(), platform="kalshi", category="Economics", tags=[]) == "Economics"
    assert resolve_bucket(_cfg(), platform="polymarket", category="Finance", tags=[]) == "Economics"
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_categories.py -v`
Expected: 10 new tests fail with `ImportError` (function not defined). The 3 from Task 2 still pass.

- [ ] **Step 3: Add `resolve_bucket` to `categories.py`**

Append to `scripts/ai_matcher/src/ai_matcher/categories.py`:

```python
def resolve_bucket(
    config: CategoryConfig,
    *,
    platform: str,            # "kalshi" or "polymarket"
    category: str,
    tags: list[str],
) -> str:
    """Resolve a market's platform-specific category (and Polymarket tags) to a bucket name.

    Returns the bucket name (e.g., "Politics") or "Unknown" if no bucket matches.
    Case-insensitive, whitespace-trimmed. Polymarket falls back to tags when category is empty;
    Kalshi does not (Kalshi tags are folded into category upstream).
    """
    candidates: list[str] = []
    if category:
        candidates.append(category)
    if platform == "polymarket" and tags:
        candidates.extend(tags)
    candidates = [c.strip().lower() for c in candidates if c and c.strip()]
    if not candidates:
        return "Unknown"

    for bucket_name, bucket_def in config.buckets.items():
        platform_aliases = bucket_def.kalshi if platform == "kalshi" else bucket_def.poly
        aliases_lc = [a.strip().lower() for a in platform_aliases]
        if any(c in aliases_lc for c in candidates):
            return bucket_name
    return "Unknown"
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_categories.py -v`
Expected: 13 passed.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/categories.py scripts/ai_matcher/tests/test_categories.py
git commit -m "Add resolve_bucket for category prefilter"
```

---

## Task 4: Add `bucket`, `close_time_utc`, `tags` fields to `Market` dataclass

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/ingestion.py`
- Modify: `scripts/ai_matcher/tests/test_ingestion.py`

- [ ] **Step 1: Write the failing test**

Append to `scripts/ai_matcher/tests/test_ingestion.py`:

```python
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py::test_market_dataclass_has_bucket_close_time_tags_fields -v`
Expected: FAIL with `TypeError: Market.__init__() got an unexpected keyword argument 'bucket'`.

- [ ] **Step 3: Add fields to `Market`**

In `scripts/ai_matcher/src/ai_matcher/ingestion.py`, modify the `Market` dataclass (around line 36) to add three new fields. Add the import for `datetime` at the top:

```python
from datetime import datetime
```

Then update the dataclass:

```python
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
```

`close_time_utc` is `None` by default *as a type signature*, but Tasks 6/7 will enforce that any `Market` reaching downstream code has a non-None value (markets that fail to parse are dropped during ingestion).

- [ ] **Step 4: Run all ingestion tests to verify nothing broke**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py -v`
Expected: all existing tests + the new `test_market_dataclass_has_bucket_close_time_tags_fields` pass.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/ingestion.py scripts/ai_matcher/tests/test_ingestion.py
git commit -m "Add bucket, close_time_utc, tags fields to Market dataclass"
```

---

## Task 5: Add `parse_close_time_utc` helper

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/ingestion.py`
- Modify: `scripts/ai_matcher/tests/test_ingestion.py`

- [ ] **Step 1: Write the failing tests**

Append to `scripts/ai_matcher/tests/test_ingestion.py`:

```python
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


def test_polymarket_prefers_endDateIso_over_endDate():
    raw = {"endDateIso": "2026-06-01T12:00:00Z", "endDate": "2099-01-01"}
    dt = parse_close_time_utc(raw, "polymarket")
    assert dt == datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)


def test_polymarket_falls_back_to_endDate():
    raw = {"endDate": "2026-06-01T12:00:00Z"}
    dt = parse_close_time_utc(raw, "polymarket")
    assert dt == datetime(2026, 6, 1, 12, 0, 0, tzinfo=timezone.utc)
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py::test_parses_iso_with_offset -v`
Expected: FAIL with `ImportError: cannot import name 'parse_close_time_utc'`.

- [ ] **Step 3: Add the helper**

In `scripts/ai_matcher/src/ai_matcher/ingestion.py`, add `timezone` to the datetime import:

```python
from datetime import datetime, timezone
```

Then add this helper near the top of the file (after the constants block):

```python
def parse_close_time_utc(raw: dict, platform: str) -> datetime | None:
    """Parse a raw market dict's expiry timestamp to a tz-aware UTC datetime.

    Returns None for missing, malformed, or naive (timezone-less) inputs.
    Caller drops the market when None is returned.
    """
    if platform == "kalshi":
        s = raw.get("close_time")
    else:  # polymarket
        s = raw.get("endDateIso") or raw.get("endDate")
    if not s:
        return None
    try:
        dt = datetime.fromisoformat(s.replace("Z", "+00:00"))
    except (ValueError, TypeError):
        return None
    if dt.tzinfo is None:
        return None
    return dt.astimezone(timezone.utc)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py -v`
Expected: all new and existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/ingestion.py scripts/ai_matcher/tests/test_ingestion.py
git commit -m "Add parse_close_time_utc helper for date prefilter"
```

---

## Task 6: Wire bucket assignment + UTC parse + drop in Kalshi parser

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/ingestion.py`
- Modify: `scripts/ai_matcher/tests/test_ingestion.py`

- [ ] **Step 1: Write the failing tests**

Append to `scripts/ai_matcher/tests/test_ingestion.py`:

```python
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
            {"ticker": "K1", "title": "no date", "category": "Politics"},   # no close_time
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py::test_kalshi_parser_assigns_bucket -v`
Expected: FAIL with `TypeError: parse_kalshi_markets_response() got an unexpected keyword argument 'category_config'`.

- [ ] **Step 3: Update `parse_kalshi_markets_response`**

In `scripts/ai_matcher/src/ai_matcher/ingestion.py`, add an import:

```python
from ai_matcher.categories import CategoryConfig, resolve_bucket
```

Replace the `parse_kalshi_markets_response` function with this version:

```python
def parse_kalshi_markets_response(
    body: dict,
    event_title: str = "",
    min_liquidity_usd: float = 0.0,
    category_config: CategoryConfig | None = None,
) -> list[Market]:
    """Parse a `/markets?event_ticker=...` response into our Market objects.

    Markets without a parseable UTC close_time are dropped. When `category_config`
    is None, every market is bucketed Unknown (prefilter disabled).
    """
    out: list[Market] = []
    for m in body.get("markets", []) or []:
        if not m.get("ticker"):
            continue

        liq_cents = m.get("liquidity")
        vol_cents = m.get("volume")
        liq_usd = float(liq_cents) / 100.0 if liq_cents is not None else 0.0
        vol_usd = float(vol_cents) / 100.0 if vol_cents is not None else 0.0
        if liq_cents is not None and liq_usd < min_liquidity_usd:
            continue

        close_utc = parse_close_time_utc(m, platform="kalshi")
        if close_utc is None:
            continue  # drop on missing/malformed/naive expiry

        title = m.get("title", "") or ""
        sub = m.get("subtitle") or m.get("yes_sub_title") or ""
        rules = m.get("rules_primary", "") or ""
        category = m.get("category", "") or ""
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
```

- [ ] **Step 4: Update existing Kalshi parser tests to include `close_time`**

The existing tests construct bodies without `close_time`, so they'll now drop everything. Add a `close_time` field to every Kalshi market dict in `tests/test_ingestion.py`'s pre-existing tests. Use this sed-style helper or do it manually:

```bash
cd scripts/ai_matcher
# Open tests/test_ingestion.py and add `"close_time": "2026-06-01T12:00:00Z"` to every existing
# Kalshi market dict that's missing one. Use grep to find them:
grep -n '"ticker":' tests/test_ingestion.py
```

For each existing test that builds a Kalshi `body = {"markets": [...]}` and doesn't include `close_time`, append `"close_time": "2026-06-01T12:00:00Z"` to each market dict.

- [ ] **Step 5: Run all ingestion tests**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py -v`
Expected: all pass (existing + new bucket tests).

- [ ] **Step 6: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/ingestion.py scripts/ai_matcher/tests/test_ingestion.py
git commit -m "Wire bucket + UTC close_time into Kalshi parser, drop on missing date"
```

---

## Task 7: Wire bucket + UTC + tag parsing in Polymarket parser

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/ingestion.py`
- Modify: `scripts/ai_matcher/tests/test_ingestion.py`

- [ ] **Step 1: Write the failing tests**

Append to `scripts/ai_matcher/tests/test_ingestion.py`:

```python
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
        {"conditionId": "0xC1", "slug": "p1", "question": "q1"},  # no end date
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py::test_poly_parser_assigns_bucket_from_category -v`
Expected: FAIL with `TypeError: parse_poly_gamma_markets_response() got an unexpected keyword argument 'category_config'`.

- [ ] **Step 3: Update `parse_poly_gamma_markets_response`**

Replace the function in `scripts/ai_matcher/src/ai_matcher/ingestion.py` with this version:

```python
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
```

- [ ] **Step 4: Update existing Polymarket parser tests to include `endDateIso`**

For each pre-existing test that builds a Polymarket `body` list, append `"endDateIso": "2026-06-01T12:00:00Z"` to every market dict that doesn't already have one. Use grep to find them:

```bash
grep -n '"conditionId":' scripts/ai_matcher/tests/test_ingestion.py
```

- [ ] **Step 5: Run all ingestion tests**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py -v`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/ingestion.py scripts/ai_matcher/tests/test_ingestion.py
git commit -m "Wire bucket + UTC close_time + tags into Polymarket parser"
```

---

## Task 8: Polymarket offset pagination + raise `INGEST_POLY_LIMIT` default

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/ingestion.py`
- Modify: `scripts/ai_matcher/tests/test_ingestion.py`

- [ ] **Step 1: Write the failing test**

Append to `scripts/ai_matcher/tests/test_ingestion.py`:

```python
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
    page3 = []  # exhausted
    stub = _StubHttp({
        "https://gamma-api.polymarket.com/markets": [page1, page2, page3],
    })
    ing = Ingestion(http=stub, poly_fetch_limit=2000, min_liquidity_usd=0.0)
    markets = ing.fetch_poly()
    assert len(markets) == 1000
    # 3 calls: offset=0, offset=500, offset=1000 (returns empty → break)
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
    # Single call because limit (500) == page size; loop exits after offset=0.
    assert len(stub.call_log) == 1
    assert len(markets) == 500
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py::test_poly_pagination_walks_offset -v`
Expected: FAIL — current `fetch_poly` only does one request.

- [ ] **Step 3: Replace `fetch_poly` and bump default**

In `scripts/ai_matcher/src/ai_matcher/ingestion.py`:

a) Update the constants:

```python
DEFAULT_POLY_FETCH_LIMIT = 10000        # was 500 — INGEST_POLY_LIMIT
```

b) Replace `Ingestion.fetch_poly` with the paginating version:

```python
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
                break  # exhausted before hitting cap
            out.extend(parse_poly_gamma_markets_response(
                body,
                min_liquidity_usd=self.min_liquidity_usd,
                category_config=self.category_config,
            ))
        return out
```

c) Update `Ingestion.__init__` to accept and store `category_config`:

```python
    def __init__(
        self,
        http: httpx.Client | None = None,
        min_liquidity_usd: float | None = None,
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py -v`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/ingestion.py scripts/ai_matcher/tests/test_ingestion.py
git commit -m "Add Polymarket offset pagination, raise INGEST_POLY_LIMIT default to 10000"
```

---

## Task 9: Kalshi cursor pagination + raise `INGEST_KALSHI_MAX_EVENTS` default

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/ingestion.py`
- Modify: `scripts/ai_matcher/tests/test_ingestion.py`

- [ ] **Step 1: Write the failing test**

Append to `scripts/ai_matcher/tests/test_ingestion.py`:

```python
def test_kalshi_pagination_walks_cursor():
    events_page1 = {
        "events": [{"event_ticker": f"E{i}", "title": "t"} for i in range(200)],
        "cursor": "CUR-A",
    }
    events_page2 = {
        "events": [{"event_ticker": f"F{i}", "title": "t"} for i in range(150)],
        "cursor": "",  # empty → terminate
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
    # 350 events × 1 market each = 350 markets
    assert len(markets) == 350
    # First events call has no cursor; second includes cursor=CUR-A
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
    # Cap is 200; first /events call returns 200 events; loop should exit.
    events_calls = [c for c in stub.call_log if "/events" in c]
    assert len(events_calls) == 1
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py::test_kalshi_pagination_walks_cursor -v`
Expected: FAIL — current `fetch_kalshi` doesn't paginate via cursor.

- [ ] **Step 3: Replace `fetch_kalshi` and bump default**

In `scripts/ai_matcher/src/ai_matcher/ingestion.py`:

a) Update the constant:

```python
DEFAULT_MAX_KALSHI_EVENTS = 2000        # was 200 — INGEST_KALSHI_MAX_EVENTS
```

b) Replace `Ingestion.fetch_kalshi`:

```python
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
                    min_liquidity_usd=self.min_liquidity_usd,
                    category_config=self.category_config,
                )
            )
        return out
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py -v`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/ingestion.py scripts/ai_matcher/tests/test_ingestion.py
git commit -m "Add Kalshi cursor pagination, raise INGEST_KALSHI_MAX_EVENTS default to 2000"
```

---

## Task 10: `MIN_VOLUME_USD` volume-based liquidity proxy

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/ingestion.py`
- Modify: `scripts/ai_matcher/tests/test_ingestion.py`

- [ ] **Step 1: Write the failing tests**

Append to `scripts/ai_matcher/tests/test_ingestion.py`:

```python
def test_kalshi_volume_proxy_drops_when_liquidity_unknown_and_volume_low():
    body = {"markets": [
        {"ticker": "K1", "title": "low vol", "category": "Politics",
         "close_time": "2026-06-01T12:00:00Z",
         "liquidity": None, "volume": 50_000},  # liquidity unknown, volume = $500
        {"ticker": "K2", "title": "high vol", "category": "Politics",
         "close_time": "2026-06-01T12:00:00Z",
         "liquidity": None, "volume": 200_000},  # liquidity unknown, volume = $2000
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
         "liquidity": 50_000, "volume": 100},  # liquidity = $500, volume = $1
    ]}
    markets = parse_kalshi_markets_response(
        body, min_liquidity_usd=100.0, min_volume_usd=1000.0,
        category_config=_kalshi_cfg(),
    )
    assert [m.ticker for m in markets] == ["K1"]
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py::test_kalshi_volume_proxy_drops_when_liquidity_unknown_and_volume_low -v`
Expected: FAIL — `parse_kalshi_markets_response` doesn't accept `min_volume_usd`.

- [ ] **Step 3: Update parser + Ingestion to thread `min_volume_usd`**

In `scripts/ai_matcher/src/ai_matcher/ingestion.py`:

a) Add a constant:

```python
DEFAULT_MIN_VOLUME_USD = 1000.0         # MIN_VOLUME_USD — Kalshi liquidity proxy
```

b) Update `parse_kalshi_markets_response` signature and predicate (modify the existing function):

```python
def parse_kalshi_markets_response(
    body: dict,
    event_title: str = "",
    min_liquidity_usd: float = 0.0,
    min_volume_usd: float = 0.0,
    category_config: CategoryConfig | None = None,
) -> list[Market]:
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
        category = m.get("category", "") or ""
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
```

c) Update `Ingestion.__init__` to take and store `min_volume_usd`:

```python
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
```

d) Update the per-event call inside `fetch_kalshi` to pass `min_volume_usd`:

```python
            out.extend(
                parse_kalshi_markets_response(
                    m_resp.json(),
                    event_title=ev["title"],
                    min_liquidity_usd=self.min_liquidity_usd,
                    min_volume_usd=self.min_volume_usd,
                    category_config=self.category_config,
                )
            )
```

- [ ] **Step 4: Run all ingestion tests**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_ingestion.py -v`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/ingestion.py scripts/ai_matcher/tests/test_ingestion.py
git commit -m "Add MIN_VOLUME_USD proxy for Kalshi liquidity filtering"
```

---

## Task 11: Replace `HnswRetrieval` with `BucketedHnswRetrieval`

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/retrieval.py`
- Modify: `scripts/ai_matcher/tests/test_retrieval.py`

- [ ] **Step 1: Write the failing tests**

Replace the body of `scripts/ai_matcher/tests/test_retrieval.py` with this version (preserve existing imports/skeleton at top):

```python
"""Tests for BucketedHnswRetrieval."""

from __future__ import annotations

import numpy as np
import pytest

from ai_matcher.retrieval import BucketedHnswRetrieval


def _orthogonal(seed: int, dim: int = 8) -> np.ndarray:
    rng = np.random.default_rng(seed)
    v = rng.standard_normal(dim).astype(np.float32)
    v /= np.linalg.norm(v)
    return v


def test_routes_known_bucket_to_bucket_index():
    politics_vecs = [(_orthogonal(0), "p1"), (_orthogonal(1), "p2")]
    sports_vecs = [(_orthogonal(2), "s1")]
    all_vecs = politics_vecs + sports_vecs
    r = BucketedHnswRetrieval(dim=8, top_k=2, min_cosine=-1.0)  # min_cosine=-1 to keep everything
    r.build({"Politics": politics_vecs, "Sports": sports_vecs}, all_vecs)
    # Querying with the politics_vecs[0] vector against Politics bucket
    results = r.query(politics_vecs[0][0], "Politics")
    ids = [i for i, _ in results]
    assert "p1" in ids
    assert "s1" not in ids   # Sports market never appears for a Politics query


def test_routes_unknown_to_full_index():
    politics_vecs = [(_orthogonal(0), "p1")]
    sports_vecs = [(_orthogonal(2), "s1")]
    all_vecs = politics_vecs + sports_vecs
    r = BucketedHnswRetrieval(dim=8, top_k=10, min_cosine=-1.0)
    r.build({"Politics": politics_vecs, "Sports": sports_vecs}, all_vecs)
    # Query as Unknown — should hit _full and see both markets
    results = r.query(_orthogonal(99), "Unknown")
    ids = {i for i, _ in results}
    assert ids == {"p1", "s1"}


def test_returns_empty_when_known_bucket_has_no_polys():
    politics_vecs = [(_orthogonal(0), "p1")]
    all_vecs = politics_vecs
    r = BucketedHnswRetrieval(dim=8, top_k=10, min_cosine=-1.0)
    r.build({"Politics": politics_vecs}, all_vecs)
    # Query as Sports — bucket has no Polys, return empty (NOT fall through)
    assert r.query(_orthogonal(99), "Sports") == []


def test_min_cosine_filters_low_similarity_results():
    # Two anti-aligned vectors → cosine ≈ -1
    v1 = np.array([1, 0, 0, 0, 0, 0, 0, 0], dtype=np.float32)
    v2 = np.array([-1, 0, 0, 0, 0, 0, 0, 0], dtype=np.float32)
    r = BucketedHnswRetrieval(dim=8, top_k=10, min_cosine=0.5)
    r.build({"Politics": [(v2, "p1")]}, [(v2, "p1")])
    assert r.query(v1, "Politics") == []  # cosine ≈ -1 < 0.5
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_retrieval.py -v`
Expected: FAIL with `ImportError: cannot import name 'BucketedHnswRetrieval'`.

- [ ] **Step 3: Replace `retrieval.py`**

Replace the entire contents of `scripts/ai_matcher/src/ai_matcher/retrieval.py` with:

```python
"""Bucketed HNSW retrieval over normalized embeddings.

Per-bucket indexes route Kalshi queries to category-matched Polymarket subsets.
A "_full" fallback index serves Unknown-category Kalshi queries.

Spec: docs/superpowers/specs/2026-05-02-matching-prefilter-and-llm-swap-design.md §3
"""

from __future__ import annotations

import hnswlib
import numpy as np


class BucketedHnswRetrieval:
    UNBUCKETED = "_full"

    def __init__(self, dim: int, top_k: int = 8, min_cosine: float = 0.55) -> None:
        self.dim = dim
        self.top_k = top_k
        self.min_cosine = min_cosine
        self._indexes: dict[str, hnswlib.Index] = {}
        self._ids: dict[str, list[str]] = {}

    def build(
        self,
        polys_by_bucket: dict[str, list[tuple[np.ndarray, str]]],
        all_polys: list[tuple[np.ndarray, str]],
    ) -> None:
        for bucket, items in polys_by_bucket.items():
            if items:
                self._build_one(bucket, items)
        if all_polys:
            self._build_one(self.UNBUCKETED, all_polys)

    def _build_one(self, name: str, items: list[tuple[np.ndarray, str]]) -> None:
        vecs = np.stack([v for v, _ in items])
        ids = [i for _, i in items]
        idx = hnswlib.Index(space="cosine", dim=self.dim)
        idx.init_index(max_elements=len(ids), ef_construction=200, M=16)
        idx.add_items(vecs, ids=np.arange(len(ids)))
        idx.set_ef(50)
        self._indexes[name] = idx
        self._ids[name] = ids

    def query(self, vector: np.ndarray, bucket: str) -> list[tuple[str, float]]:
        """Return [(poly_id, cosine), ...] from the index matching the Kalshi-side bucket.

        Routing:
          - bucket is known and the index exists → use that index
          - bucket is "Unknown" and _full exists → use _full (current pre-spec behavior)
          - bucket is known but no index for it → return empty (deliberate no-op)
        """
        if bucket != "Unknown" and bucket in self._indexes:
            target = bucket
        elif bucket == "Unknown" and self.UNBUCKETED in self._indexes:
            target = self.UNBUCKETED
        else:
            return []
        index, ids = self._indexes[target], self._ids[target]
        labels, distances = index.knn_query(vector, k=min(self.top_k, len(ids)))
        out: list[tuple[str, float]] = []
        for label, dist in zip(labels[0], distances[0], strict=False):
            cosine = 1.0 - float(dist)
            if cosine >= self.min_cosine:
                out.append((ids[label], cosine))
        return out
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_retrieval.py -v`
Expected: all 4 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/retrieval.py scripts/ai_matcher/tests/test_retrieval.py
git commit -m "Replace HnswRetrieval with BucketedHnswRetrieval for category prefilter"
```

---

## Task 12: Update `pyproject.toml` (add `litellm`, drop `anthropic`)

**Files:**
- Modify: `scripts/ai_matcher/pyproject.toml`

- [ ] **Step 1: Edit `pyproject.toml`**

In `scripts/ai_matcher/pyproject.toml`, replace the `dependencies` block:

```toml
dependencies = [
    "hnswlib>=0.8.0",
    "httpx>=0.28.1",
    "jinja2>=3.1.6",
    "litellm>=1.50.0",
    "sentence-transformers>=5.4.1",
]
```

(Drops `anthropic`, adds `litellm`.)

- [ ] **Step 2: Run `uv sync` to update the lock file**

Run: `cd scripts/ai_matcher && uv sync`
Expected: lock file updates, no errors.

- [ ] **Step 3: Verify import works**

Run: `cd scripts/ai_matcher && uv run python -c "import litellm; print(litellm.__version__)"`
Expected: prints a version string like `1.50.x`.

- [ ] **Step 4: Run all existing tests**

Run: `cd scripts/ai_matcher && uv run pytest -v`
Expected: ingestion / retrieval / categories tests pass; verifier tests *fail* (still importing `anthropic`). That's expected — Task 13 fixes them.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/pyproject.toml scripts/ai_matcher/uv.lock
git commit -m "Swap anthropic for litellm in ai_matcher dependencies"
```

---

## Task 13: Migrate `Verifier` to LiteLLM with OpenAI tool schema

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/verifier.py`
- Modify: `scripts/ai_matcher/tests/test_verifier.py`

- [ ] **Step 1: Write the failing tests**

Replace the body of `scripts/ai_matcher/tests/test_verifier.py` (preserving any header) with:

```python
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
    # Sanity: the request used the OpenAI-shaped tool envelope.
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

    # Different model → different cache key → cache miss
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_verifier.py -v`
Expected: FAIL — current Verifier takes `client`, not `model`, and uses `anthropic`.

- [ ] **Step 3: Replace `verifier.py`**

Replace the entire contents of `scripts/ai_matcher/src/ai_matcher/verifier.py` with:

```python
"""LiteLLM-backed verifier for candidate market pairs.

Spec: docs/superpowers/specs/2026-05-02-matching-prefilter-and-llm-swap-design.md §4
"""

from __future__ import annotations

import json
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

import litellm

from ai_matcher.ingestion import Market

DEFAULT_MODEL = "gpt-4.1-mini"

VERIFIER_TOOL = {
    "type": "function",
    "function": {
        "name": "report_match_decision",
        "description": "Report whether two prediction markets resolve to identical outcomes.",
        "parameters": {
            "type": "object",
            "properties": {
                "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                "resolution_match": {"type": "boolean"},
                "concerns": {"type": "array", "items": {"type": "string"}},
                "reasoning": {"type": "string"},
                "category": {"type": "string"},
                "event_type": {"type": "string", "enum": [
                    "Sports", "Fomc", "Cpi", "NfpJobs", "Election", "Other"
                ]},
            },
            "required": ["confidence", "resolution_match", "concerns", "reasoning",
                         "category", "event_type"],
        },
    },
}

SYSTEM_PROMPT = """You are evaluating whether two prediction-market contracts — one on Kalshi,
one on Polymarket — resolve to IDENTICAL outcomes. This is for arbitrage
matching, so false positives cost real money.

Be paranoid about resolution criteria. Two markets that sound similar can
resolve differently on edge cases. Classic traps:
  - Different resolution dates / windows
  - Different data sources (e.g., which exchange's BTC price; which BLS series)
  - Qualitative vs. quantitative thresholds with different cutoffs
  - Scope differences (e.g., "any X" vs. "official X"; "primary" vs. "general")
  - Inclusive vs. exclusive range boundaries
  - Different underliers that share a topic (e.g., target rate vs. effective rate)

Score 1.0 only if you can articulate that both markets resolve YES on exactly
the same set of real-world outcomes. Any meaningful definition gap → confidence
below 0.9 and a clear `concerns[]` entry."""


@dataclass
class Decision:
    confidence: float
    resolution_match: bool
    concerns: list[str]
    reasoning: str
    category: str
    event_type: str
    cost_usd: float = 0.0   # populated by Verifier; 0.0 for cached/embeddings-only

    def is_accepted(self, min_confidence: float = 0.9) -> bool:
        return (
            self.resolution_match
            and self.confidence >= min_confidence
            and not self.concerns
        )

    @property
    def accepted(self) -> bool:
        return self.is_accepted(0.9)


def user_prompt(kalshi: Market, poly: Market) -> str:
    return (
        f"Kalshi market:\n  Title: {kalshi.title}\n  Description: {kalshi.description}\n"
        f"  Resolution: {kalshi.resolution_criteria}\n  Outcomes: {kalshi.outcomes}\n\n"
        f"Polymarket market:\n  Title: {poly.title}\n  Description: {poly.description}\n"
        f"  Resolution: {poly.resolution_criteria}\n  Outcomes: {poly.outcomes}\n\n"
        "Do these resolve identically? Score accordingly."
    )


class Verifier:
    def __init__(self, model: str = DEFAULT_MODEL, cache_path: Path | None = None) -> None:
        self.model = model
        self.cache_path = cache_path
        self._cache: dict[str, dict] = self._load_cache()
        self.cache_hits = 0
        self.cache_misses = 0

    def _load_cache(self) -> dict[str, dict]:
        if self.cache_path and self.cache_path.exists():
            try:
                return json.loads(self.cache_path.read_text())
            except json.JSONDecodeError:
                return {}
        return {}

    def _save_cache(self) -> None:
        if self.cache_path is None:
            return
        self.cache_path.write_text(json.dumps(self._cache))

    def _cache_key(self, k: Market, p: Market) -> str:
        return f"{self.model}|{k.content_hash()}|{p.content_hash()}"

    def verify(self, kalshi: Market, poly: Market) -> Decision:
        key = self._cache_key(kalshi, poly)
        if key in self._cache:
            self.cache_hits += 1
            return Decision(**self._cache[key])

        self.cache_misses += 1
        resp = litellm.completion(
            model=self.model,
            messages=[
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user",   "content": user_prompt(kalshi, poly)},
            ],
            tools=[VERIFIER_TOOL],
            tool_choice={"type": "function", "function": {"name": "report_match_decision"}},
            num_retries=3,
        )
        tool_input = self._extract_tool_input(resp)
        cost = float(getattr(resp, "_hidden_params", {}).get("response_cost", 0.0) or 0.0)
        decision = Decision(
            confidence=float(tool_input["confidence"]),
            resolution_match=bool(tool_input["resolution_match"]),
            concerns=list(tool_input.get("concerns", [])),
            reasoning=str(tool_input.get("reasoning", "")),
            category=str(tool_input.get("category", "")),
            event_type=str(tool_input.get("event_type", "Other")),
            cost_usd=cost,
        )
        self._cache[key] = asdict(decision)
        self._save_cache()
        return decision

    @staticmethod
    def _extract_tool_input(resp: Any) -> dict:
        choices = getattr(resp, "choices", None) or []
        for ch in choices:
            msg = getattr(ch, "message", None)
            if msg is None:
                continue
            tool_calls = getattr(msg, "tool_calls", None) or []
            for tc in tool_calls:
                fn = getattr(tc, "function", None)
                if fn is None:
                    continue
                args = getattr(fn, "arguments", None)
                if args:
                    return json.loads(args)
        raise ValueError("LiteLLM response missing tool_calls")


class EmbeddingsOnlyVerifier:
    """Verifier replacement that decides purely on cosine similarity (no LLM)."""

    def __init__(self, accept_cosine: float = 0.85) -> None:
        self.accept_cosine = accept_cosine
        self.model = "embeddings-only"
        self.cache_hits = 0
        self.cache_misses = 0

    def verify(self, kalshi: Market, poly: Market, cosine: float) -> Decision:
        accepted = cosine >= self.accept_cosine
        if accepted:
            concerns: list[str] = []
        else:
            concerns = [
                f"cosine {cosine:.3f} below embeddings-only threshold {self.accept_cosine}"
            ]
        return Decision(
            confidence=cosine,
            resolution_match=accepted,
            concerns=concerns,
            reasoning=(
                f"embeddings-only: cosine={cosine:.4f}, threshold={self.accept_cosine} "
                f"(no LLM verification — embeddings only catch topical similarity, "
                f"not resolution-criteria identity)"
            ),
            category="",
            event_type="Other",
            cost_usd=0.0,
        )
```

- [ ] **Step 4: Update `pipeline.run_pipeline_default` to construct the new `Verifier`**

In `scripts/ai_matcher/src/ai_matcher/pipeline.py`, find the LLM-mode branch in `run_pipeline_default` (around line 240). Replace:

```python
    else:
        import anthropic

        client = anthropic.Anthropic()
        verifier = Verifier(
            client=client,
            cache_path=project_root / ".ai_matcher_verifier_cache.json",
        )
        cfg.llm_model = verifier.model
```

with:

```python
    else:
        import os
        model = os.environ.get("LLM_MODEL", "gpt-4.1-mini")
        verifier = Verifier(
            model=model,
            cache_path=project_root / ".ai_matcher_verifier_cache.json",
        )
        cfg.llm_model = verifier.model
```

- [ ] **Step 5: Run all tests**

Run: `cd scripts/ai_matcher && uv run pytest -v`
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/verifier.py scripts/ai_matcher/src/ai_matcher/pipeline.py scripts/ai_matcher/tests/test_verifier.py
git commit -m "Migrate verifier to LiteLLM with OpenAI tool schema"
```

---

## Task 14: Add `date_overlap_ok` predicate to pipeline

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/pipeline.py`
- Modify: `scripts/ai_matcher/tests/test_pipeline.py`

- [ ] **Step 1: Write the failing tests**

Append to `scripts/ai_matcher/tests/test_pipeline.py`:

```python
from datetime import datetime, timedelta, timezone

from ai_matcher.categories import BucketDef, CategoryConfig
from ai_matcher.ingestion import Market
from ai_matcher.pipeline import date_overlap_ok


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


def test_within_tolerance_passes():
    k = _market("Politics", 0)
    p = _market("Politics", 30)
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is True


def test_beyond_tolerance_fails():
    k = _market("Politics", 0)
    p = _market("Politics", 90)  # 90 > 60
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is False


def test_scale_widens_tolerance():
    k = _market("Politics", 0)
    p = _market("Politics", 90)
    assert date_overlap_ok(k, p, _cfg(), scale=2.0) is True   # tolerance 60*2 = 120 ≥ 90


def test_sports_tolerance_is_strict():
    k = _market("Sports", 0)
    p = _market("Sports", 5)  # 5 > 2
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is False


def test_unknown_bucket_uses_default_tolerance():
    """Both Unknown → default_tolerance_days (30 in fixture)."""
    k = Market(platform="kalshi", ticker="k", title="t",
               bucket="Unknown",
               close_time_utc=datetime(2026, 6, 1, tzinfo=timezone.utc))
    p = Market(platform="polymarket", ticker="p", title="t",
               bucket="Unknown", condition_id="0xC1",
               close_time_utc=datetime(2026, 6, 28, tzinfo=timezone.utc))  # 27 days
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is True   # 27 ≤ 30
    p.close_time_utc = datetime(2026, 7, 5, tzinfo=timezone.utc)             # 34 days
    assert date_overlap_ok(k, p, _cfg(), scale=1.0) is False  # 34 > 30
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_pipeline.py::test_within_tolerance_passes -v`
Expected: FAIL with `ImportError: cannot import name 'date_overlap_ok'`.

- [ ] **Step 3: Add the predicate to `pipeline.py`**

In `scripts/ai_matcher/src/ai_matcher/pipeline.py`, add an import at the top:

```python
from ai_matcher.categories import CategoryConfig
```

Then add the predicate function (anywhere above `run_pipeline`):

```python
def date_overlap_ok(
    k: Market,
    p: Market,
    cfg: CategoryConfig,
    scale: float,
) -> bool:
    """Return True iff the two markets' UTC expiries are within the bucket's tolerance.

    Bucket selection: the Kalshi-side bucket if known; otherwise the Polymarket
    bucket; otherwise default_tolerance_days. Both Unknown → default_tolerance_days.
    """
    bucket = k.bucket if k.bucket != "Unknown" else p.bucket
    tol_days = (
        cfg.buckets[bucket].tolerance_days
        if bucket in cfg.buckets
        else cfg.default_tolerance_days
    )
    if k.close_time_utc is None or p.close_time_utc is None:
        return False  # safety: should never happen post-ingestion
    delta_seconds = abs((k.close_time_utc - p.close_time_utc).total_seconds())
    return delta_seconds <= tol_days * scale * 86_400
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_pipeline.py -v`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/pipeline.py scripts/ai_matcher/tests/test_pipeline.py
git commit -m "Add date_overlap_ok predicate for expiry prefilter"
```

---

## Task 15: Wire bucket-aware retrieval, date predicate, funnel counters into `run_pipeline`

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/pipeline.py`
- Modify: `scripts/ai_matcher/tests/test_pipeline.py`

- [ ] **Step 1: Write the failing integration test**

Append to `scripts/ai_matcher/tests/test_pipeline.py`:

```python
from collections import defaultdict
from types import SimpleNamespace

import numpy as np

from ai_matcher.ingestion import IngestionResult
from ai_matcher.pipeline import PipelineConfig, run_pipeline
from ai_matcher.verifier import EmbeddingsOnlyVerifier


class _OrthoEmbedder:
    """Deterministic 1-hot embedder. Two markets with the same `ticker` get identical vectors."""

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
        # Same ticker as the Kalshi politics market → identical embedding (cosine = 1.0)
        Market(platform="polymarket", ticker="k_pol_a", title="t",
               bucket="Politics", condition_id="0xPOL",
               close_time_utc=base_time),
        # Same Politics bucket but expiry 90 days off → fails date overlap (60d tol)
        Market(platform="polymarket", ticker="k_pol_a", title="t",
               bucket="Politics", condition_id="0xPOL_FAR",
               close_time_utc=base_time + timedelta(days=90)),
        # Sports market on the Polymarket side, identical embedding to Kalshi sports
        Market(platform="polymarket", ticker="k_sports_a", title="t",
               bucket="Sports", condition_id="0xSP",
               close_time_utc=base_time),
    ]

    pipeline_cfg = PipelineConfig(
        project_root=tmp_path,
        audit_dir=tmp_path / "audit",
        matches_path=tmp_path / "matches.json",
        audit_log_path=tmp_path / "audit.jsonl",
        overrides_path=tmp_path / "overrides.json",
        embedding_model="test",
        llm_model="embeddings-only",
        category_config=cfg_obj,
        expiry_tolerance_scale=1.0,
        acceptance_min_confidence=0.5,  # force accept everything that survives the filters
    )
    summary = run_pipeline(
        pipeline_cfg,
        ingestion=_fake_ingestion(kalshi, poly),
        embedder=_OrthoEmbedder(),
        verifier=EmbeddingsOnlyVerifier(accept_cosine=0.5),
    )
    assert summary["accepted"] == 2     # k_pol_a + k_sports_a — same-bucket same-time
    assert summary["drops_at_date_overlap"] == 1   # k_pol_a vs 0xPOL_FAR
    # Cross-bucket pairs never enter retrieval, so no audit row for them:
    assert summary["candidates_after_retrieval"] == 3   # 2 politics + 1 sports
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_pipeline.py::test_pipeline_funnel_counters_and_bucket_routing -v`
Expected: FAIL — `PipelineConfig` doesn't have `category_config` / `expiry_tolerance_scale` fields, and `run_pipeline` doesn't apply the date predicate or per-stage counters.

- [ ] **Step 3: Update `PipelineConfig` and `run_pipeline`**

In `scripts/ai_matcher/src/ai_matcher/pipeline.py`:

a) Update the import block:

```python
from collections import defaultdict
```

b) Update `PipelineConfig` to add new fields:

```python
@dataclass
class PipelineConfig:
    project_root: Path
    audit_dir: Path
    matches_path: Path
    audit_log_path: Path
    overrides_path: Path
    embedding_model: str
    llm_model: str
    top_k: int = 8
    min_cosine: float = 0.55
    acceptance_min_confidence: float = 0.9
    category_config: CategoryConfig | None = None
    expiry_tolerance_scale: float = 1.0
```

c) Replace the body of `run_pipeline` with this version (uses `BucketedHnswRetrieval`, applies date predicate, emits per-stage counters):

```python
def run_pipeline(
    cfg: PipelineConfig,
    ingestion: Any,
    embedder: Any,
    verifier: Any,
) -> dict:
    from ai_matcher.retrieval import BucketedHnswRetrieval

    result: IngestionResult = ingestion.fetch_all()

    counters_drops_ingest = {
        "kalshi_missing_date": 0, "poly_missing_date": 0,
        "kalshi_low_volume": 0, "poly_low_liquidity": 0,
    }
    bucketed_counts: dict[str, int] = defaultdict(int)

    # Group + embed Polymarket vectors by bucket
    polys_by_bucket: dict[str, list[tuple[np.ndarray, str]]] = defaultdict(list)
    all_polys: list[tuple[np.ndarray, str]] = []
    poly_by_ticker: dict[str, Market] = {}
    for m in result.poly:
        bucketed_counts[m.bucket] += 1
        vec = embedder.embed(m)
        polys_by_bucket[m.bucket].append((vec, m.ticker))
        all_polys.append((vec, m.ticker))
        poly_by_ticker[m.ticker] = m

    embedder.flush()

    retrieval = BucketedHnswRetrieval(
        dim=embedder.dim, top_k=cfg.top_k, min_cosine=cfg.min_cosine
    )
    if all_polys:
        retrieval.build(polys_by_bucket, all_polys)

    overrides = OverrideSet.load(cfg.overrides_path)
    rows: list[PairAuditRow] = []
    accepted_pairs: list[dict] = []
    audit_log_lines: list[str] = []

    accepted = 0
    rejected = 0
    candidates_after_retrieval = 0
    drops_at_date_overlap = 0
    verifier_calls = 0
    verifier_cost_usd = 0.0

    for k in result.kalshi:
        bucketed_counts[k.bucket] += 1
        k_vec = embedder.embed(k)
        candidates = retrieval.query(k_vec, k.bucket) if all_polys else []
        for poly_ticker, cosine in candidates:
            p = poly_by_ticker.get(poly_ticker)
            if p is None:
                continue
            candidates_after_retrieval += 1

            # Date-overlap prefilter
            if cfg.category_config is not None and not date_overlap_ok(
                k, p, cfg.category_config, cfg.expiry_tolerance_scale
            ):
                drops_at_date_overlap += 1
                tol = (
                    cfg.category_config.buckets[k.bucket].tolerance_days
                    if k.bucket in cfg.category_config.buckets
                    else cfg.category_config.default_tolerance_days
                )
                delta_days = abs((k.close_time_utc - p.close_time_utc).days) \
                    if (k.close_time_utc and p.close_time_utc) else None
                audit_log_lines.append(json.dumps({
                    "ts": dt.datetime.now(tz=dt.timezone.utc).isoformat(),
                    "kalshi": k.ticker, "poly": p.condition_id,
                    "decision": "reject", "reject_reason": "expiry-gap",
                    "bucket_kalshi": k.bucket, "bucket_poly": p.bucket,
                    "cosine": round(float(cosine), 4),
                    "delta_days": delta_days, "tolerance_days": tol,
                }))
                rejected += 1
                continue

            # Verifier
            verifier_calls += 1
            decision = _call_verifier(verifier, k, p, cosine)
            verifier_cost_usd += getattr(decision, "cost_usd", 0.0) or 0.0
            override = overrides.lookup(k.ticker, p.condition_id)
            ai_accept = decision.is_accepted(min_confidence=cfg.acceptance_min_confidence)
            if override == OverrideOutcome.BLACKLIST:
                final_accepted = False
            elif override == OverrideOutcome.WHITELIST:
                final_accepted = True
            else:
                final_accepted = ai_accept

            if final_accepted:
                accepted += 1
                accepted_pairs.append({
                    "kalshi_market_ticker": k.ticker,
                    "poly_condition_id": p.condition_id,
                    "poly_yes_token": p.poly_yes_token,
                    "poly_no_token": p.poly_no_token,
                    "category": decision.category,
                    "event_type": decision.event_type,
                    "confidence": decision.confidence,
                    "description": f"{k.title} ↔ {p.title}",
                })
            else:
                rejected += 1

            tol_resolved = (
                cfg.category_config.buckets[k.bucket].tolerance_days
                if cfg.category_config and k.bucket in cfg.category_config.buckets
                else (cfg.category_config.default_tolerance_days if cfg.category_config else None)
            )
            delta_days_resolved = abs((k.close_time_utc - p.close_time_utc).days) \
                if (k.close_time_utc and p.close_time_utc) else None
            rows.append(PairAuditRow(
                kalshi_ticker=k.ticker, kalshi_title=k.title,
                kalshi_description=k.description, kalshi_resolution=k.resolution_criteria,
                kalshi_outcomes=k.outcomes, kalshi_url=_kalshi_url(k.ticker),
                poly_slug=p.ticker, poly_title=p.title,
                poly_description=p.description, poly_resolution=p.resolution_criteria,
                poly_outcomes=p.outcomes, poly_url=_poly_url(p.ticker),
                decision=decision, accepted=final_accepted,
                override_snippet=_override_snippet(k.ticker, p.condition_id),
                override_outcome=override.value,
                bucket_kalshi=k.bucket, bucket_poly=p.bucket,
                cosine=float(cosine),
                delta_days=delta_days_resolved,
            ))

            audit_log_lines.append(json.dumps({
                "ts": dt.datetime.now(tz=dt.timezone.utc).isoformat(),
                "kalshi": k.ticker, "poly": p.condition_id,
                "decision": "accept" if final_accepted else "reject",
                "reject_reason": None if final_accepted else "verifier",
                "bucket_kalshi": k.bucket, "bucket_poly": p.bucket,
                "cosine": round(float(cosine), 4),
                "delta_days": delta_days_resolved, "tolerance_days": tol_resolved,
                "confidence": decision.confidence,
                "concerns": decision.concerns,
                "reasoning": decision.reasoning,
                "override": override.value,
                "model": getattr(verifier, "model", ""),
                "cost_usd": getattr(decision, "cost_usd", 0.0),
            }))

    payload = {
        "generated_at": dt.datetime.now(tz=dt.timezone.utc).isoformat(),
        "model": cfg.llm_model,
        "embedding_model": cfg.embedding_model,
        "version": 1,
        "pairs": accepted_pairs,
    }
    _atomic_write_json(cfg.matches_path, payload)
    render_report(rows, cfg.audit_dir)

    if audit_log_lines:
        cfg.audit_log_path.parent.mkdir(parents=True, exist_ok=True)
        with cfg.audit_log_path.open("a") as f:
            for line in audit_log_lines:
                f.write(line + "\n")

    return {
        "ingested": {"kalshi": len(result.kalshi), "poly": len(result.poly)},
        "drops_at_ingest": counters_drops_ingest,
        "bucketed": dict(bucketed_counts),
        "candidates_after_retrieval": candidates_after_retrieval,
        "drops_at_date_overlap": drops_at_date_overlap,
        "verifier_calls": verifier_calls,
        "verifier_cache_hits": getattr(verifier, "cache_hits", 0),
        "verifier_cost_usd": round(verifier_cost_usd, 4),
        "accepted": accepted, "rejected": rejected, "rows": len(rows),
    }
```

d) Update `run_pipeline_default` to wire the category config + scale env var. After the `cfg = PipelineConfig(...)` block, add:

```python
    from ai_matcher.categories import load_category_config
    cfg.category_config = load_category_config(project_root / "config" / "category_equivalence.json")
    cfg.expiry_tolerance_scale = float(os.environ.get("EXPIRY_TOLERANCE_SCALE", "1.0"))
    if cfg.expiry_tolerance_scale <= 0:
        print("[ai_matcher] EXPIRY_TOLERANCE_SCALE must be > 0; using 1.0")
        cfg.expiry_tolerance_scale = 1.0
```

Also update the `Ingestion()` construction to pass the category config:

```python
    ingestion = Ingestion(category_config=cfg.category_config)
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_pipeline.py -v`
Expected: all new and existing tests pass.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/pipeline.py scripts/ai_matcher/tests/test_pipeline.py
git commit -m "Wire bucket-aware retrieval, date predicate, funnel counters into pipeline"
```

---

## Task 16: Extend `PairAuditRow` and HTML template with new columns

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/report.py`
- Modify: `scripts/ai_matcher/src/ai_matcher/templates/report.html.j2`
- Modify: `scripts/ai_matcher/tests/test_report.py`

- [ ] **Step 1: Write the failing test**

Replace the body of `scripts/ai_matcher/tests/test_report.py` with:

```python
"""Tests for the audit HTML report."""

from __future__ import annotations

from pathlib import Path

from ai_matcher.report import PairAuditRow, render_report
from ai_matcher.verifier import Decision


def _row(**kwargs) -> PairAuditRow:
    base = dict(
        kalshi_ticker="K", kalshi_title="kt", kalshi_description="kd",
        kalshi_resolution="kr", kalshi_outcomes=["yes", "no"],
        kalshi_url="https://k.example",
        poly_slug="p", poly_title="pt", poly_description="pd",
        poly_resolution="pr", poly_outcomes=["yes", "no"],
        poly_url="https://p.example",
        decision=Decision(
            confidence=0.95, resolution_match=True, concerns=[],
            reasoning="r", category="Politics", event_type="Election",
            cost_usd=0.0007,
        ),
        accepted=True, override_snippet="{}", override_outcome="none",
        bucket_kalshi="Politics", bucket_poly="Politics",
        cosine=0.83, delta_days=12.0,
    )
    base.update(kwargs)
    return PairAuditRow(**base)


def test_report_contains_new_columns(tmp_path: Path):
    render_report([_row()], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert "Bucket pair" in html
    assert "Cosine" in html
    assert "Δdays" in html
    assert "Politics → Politics" in html
    assert "0.830" in html or "0.83" in html
    assert "12" in html


def test_report_has_sortable_headers(tmp_path: Path):
    render_report([_row()], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert 'data-sort="numeric"' in html
    assert 'data-sort="string"' in html


def test_report_has_filter_input_and_sticky_header(tmp_path: Path):
    render_report([_row()], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert 'id="filter"' in html
    assert "position: sticky" in html


def test_report_has_sort_filter_js(tmp_path: Path):
    render_report([_row()], tmp_path)
    html = (tmp_path / "report.html").read_text()
    assert "addEventListener('click'" in html
    assert "addEventListener('input'" in html
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_report.py -v`
Expected: FAIL — `PairAuditRow` doesn't have `bucket_kalshi`/`bucket_poly`/`cosine`/`delta_days` and the template lacks new columns.

- [ ] **Step 3: Add fields to `PairAuditRow`**

In `scripts/ai_matcher/src/ai_matcher/report.py`, add fields to the dataclass:

```python
@dataclass
class PairAuditRow:
    kalshi_ticker: str
    kalshi_title: str
    kalshi_description: str
    kalshi_resolution: str
    kalshi_outcomes: list[str]
    kalshi_url: str
    poly_slug: str
    poly_title: str
    poly_description: str
    poly_resolution: str
    poly_outcomes: list[str]
    poly_url: str
    decision: Decision
    accepted: bool
    override_snippet: str
    override_outcome: str = "none"
    # NEW fields surfaced by the prefilter / verifier:
    bucket_kalshi: str = "Unknown"
    bucket_poly: str = "Unknown"
    cosine: float = 0.0
    delta_days: float | None = None
```

- [ ] **Step 4: Update the Jinja template — header + body + sticky CSS**

Replace the contents of `scripts/ai_matcher/src/ai_matcher/templates/report.html.j2` with:

```jinja
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>ai_matcher — {{ title }}</title>
<style>
  body { font-family: -apple-system, sans-serif; max-width: 1500px; margin: 1em auto; padding: 0 1em; }
  table { border-collapse: collapse; width: 100%; }
  th, td { border: 1px solid #ccc; padding: 0.5em; vertical-align: top; font-size: 0.9em; }
  th { background: #f4f4f4; text-align: left; cursor: pointer;
       position: sticky; top: 0; z-index: 1; }
  tbody tr:nth-child(even) { background: #fafafa; }
  .accept { background: #e8f5e9; }
  .reject { background: #ffebee; }
  .override-whitelist { background: #fff3cd; border-left: 4px solid #ffa000; }
  .override-blacklist { background: #f0e0f8; border-left: 4px solid #6a1b9a; }
  .override-label { display: inline-block; padding: 0.1em 0.4em; border-radius: 3px;
                    background: #444; color: white; font-size: 0.75em;
                    font-weight: bold; margin-top: 0.3em; }
  .conf, .num { font-weight: bold; font-variant-numeric: tabular-nums; }
  pre.snippet { background: #fafafa; padding: 0.4em; font-size: 0.8em; white-space: pre-wrap; }
  .filters a { margin-right: 1em; }
  .toolbar { margin: 0.6em 0; display: flex; gap: 1em; align-items: center; }
  .toolbar input { padding: 0.4em 0.6em; font-size: 0.9em; min-width: 25em; }
  th.asc::after  { content: ' ▲'; }
  th.desc::after { content: ' ▼'; }
</style>
</head>
<body>
<h1>{{ title }}</h1>
<p class="filters">
  <a href="report.html">all</a>
  <a href="report-accepted.html">accepted only</a>
  <a href="report-rejected.html">rejected only</a>
  {% for cat in categories %}
  <a href="report-by-category-{{ cat | lower }}.html">{{ cat }}</a>
  {% endfor %}
</p>
<div class="toolbar">
  <input id="filter" placeholder="Filter (substring match across all columns)…" />
  <span>{{ rows | length }} pair(s).</span>
</div>
<table>
<thead>
<tr>
  <th data-sort="string">Decision</th>
  <th data-sort="string">Kalshi</th>
  <th data-sort="string">Polymarket</th>
  <th data-sort="string">Bucket pair</th>
  <th data-sort="numeric">Cosine</th>
  <th data-sort="numeric">Δdays</th>
  <th data-sort="string">LLM analysis</th>
  <th data-sort="string">Override snippet</th>
</tr>
</thead>
<tbody>
{% for row in rows %}
{% set base_class = 'accept' if row.accepted else 'reject' %}
{% set override_class = 'override-whitelist' if row.override_outcome == 'whitelist'
                        else ('override-blacklist' if row.override_outcome == 'blacklist' else '') %}
<tr class="{{ base_class }} {{ override_class }}">
  <td>
    {{ '✅' if row.accepted else '❌' }}
    <span class="conf">{{ '%.2f' | format(row.decision.confidence) }}</span><br>
    {{ row.decision.event_type }}
    {% if row.override_outcome == 'whitelist' %}
    <br><span class="override-label">⚠ WHITELIST OVERRIDE</span>
    {% elif row.override_outcome == 'blacklist' %}
    <br><span class="override-label">⚠ BLACKLIST OVERRIDE</span>
    {% endif %}
  </td>
  <td>
    <a href="{{ row.kalshi_url }}">{{ row.kalshi_ticker }}</a><br>
    <strong>{{ row.kalshi_title }}</strong><br>
    {{ row.kalshi_description }}<br>
    <em>{{ row.kalshi_resolution }}</em><br>
    Outcomes: {{ row.kalshi_outcomes | join(', ') }}
  </td>
  <td>
    <a href="{{ row.poly_url }}">{{ row.poly_slug }}</a><br>
    <strong>{{ row.poly_title }}</strong><br>
    {{ row.poly_description }}<br>
    <em>{{ row.poly_resolution }}</em><br>
    Outcomes: {{ row.poly_outcomes | join(', ') }}
  </td>
  <td class="num">{{ row.bucket_kalshi }} → {{ row.bucket_poly }}</td>
  <td class="num">{{ '%.3f' | format(row.cosine) }}</td>
  <td class="num">{{ '%.1f' | format(row.delta_days) if row.delta_days is not none else '—' }}</td>
  <td>
    Category: {{ row.decision.category }}<br>
    Concerns:
    <ul>
      {% for c in row.decision.concerns %}<li>{{ c }}</li>{% else %}<li>None</li>{% endfor %}
    </ul>
    Reasoning: {{ row.decision.reasoning }}
  </td>
  <td><pre class="snippet">{{ row.override_snippet }}</pre></td>
</tr>
{% endfor %}
</tbody>
</table>
<script>
(function() {
  const tbody = document.querySelector('tbody');
  if (!tbody) return;
  const rows = Array.from(tbody.querySelectorAll('tr'));
  document.querySelectorAll('th[data-sort]').forEach((th, i) => {
    th.addEventListener('click', () => {
      const numeric = th.dataset.sort === 'numeric';
      const dir = th.classList.contains('asc') ? -1 : 1;
      document.querySelectorAll('th').forEach(h => h.classList.remove('asc', 'desc'));
      th.classList.add(dir === 1 ? 'asc' : 'desc');
      rows.sort((a, b) => {
        const av = a.children[i].textContent.trim();
        const bv = b.children[i].textContent.trim();
        if (numeric) {
          const an = parseFloat(av);
          const bn = parseFloat(bv);
          if (isNaN(an) && isNaN(bn)) return 0;
          if (isNaN(an)) return 1;
          if (isNaN(bn)) return -1;
          return (an - bn) * dir;
        }
        return av.localeCompare(bv) * dir;
      });
      rows.forEach(r => tbody.appendChild(r));
    });
  });

  const filter = document.getElementById('filter');
  if (filter) {
    let timer;
    filter.addEventListener('input', () => {
      clearTimeout(timer);
      timer = setTimeout(() => {
        const q = filter.value.trim().toLowerCase();
        rows.forEach(r => {
          r.style.display = !q || r.textContent.toLowerCase().includes(q) ? '' : 'none';
        });
      }, 150);
    });
  }
})();
</script>
</body>
</html>
```

- [ ] **Step 5: Run report tests**

Run: `cd scripts/ai_matcher && uv run pytest tests/test_report.py -v`
Expected: 4 tests pass.

- [ ] **Step 6: Run all tests**

Run: `cd scripts/ai_matcher && uv run pytest -v`
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/report.py scripts/ai_matcher/src/ai_matcher/templates/report.html.j2 scripts/ai_matcher/tests/test_report.py
git commit -m "Add bucket/cosine/Δdays columns and sort+filter JS to audit report"
```

---

## Task 17: Update `_render_audit_sample` to use the same template

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/pipeline.py`

- [ ] **Step 1: Read the existing `_render_audit_sample` function**

It's at the bottom of `pipeline.py`. The current implementation builds raw HTML strings. We replace it with a Jinja-rendered version using the same template, so spot-checks get sort + filter for free.

- [ ] **Step 2: Replace `_render_audit_sample` with a Jinja-rendered version**

Replace the function body in `scripts/ai_matcher/src/ai_matcher/pipeline.py`:

```python
def _render_audit_sample(pairs: list[dict], payload: dict) -> str:
    """Render N spot-check pairs using the main report template — sort + filter for free."""
    from importlib.resources import files
    from jinja2 import Environment, FileSystemLoader, select_autoescape

    template_dir = files("ai_matcher").joinpath("templates")
    env = Environment(
        loader=FileSystemLoader(str(template_dir)),
        autoescape=select_autoescape(["html"]),
    )
    tpl = env.get_template("report.html.j2")
    # Build minimal PairAuditRow objects from the accepted-pairs JSON payload.
    rows = []
    for p in pairs:
        from ai_matcher.report import PairAuditRow
        from ai_matcher.verifier import Decision
        rows.append(PairAuditRow(
            kalshi_ticker=p.get("kalshi_market_ticker", ""),
            kalshi_title="", kalshi_description="", kalshi_resolution="",
            kalshi_outcomes=[], kalshi_url="",
            poly_slug=p.get("poly_condition_id", ""),
            poly_title="", poly_description="", poly_resolution="",
            poly_outcomes=[], poly_url="",
            decision=Decision(
                confidence=float(p.get("confidence", 0.0)),
                resolution_match=True, concerns=[],
                reasoning="", category=p.get("category", ""),
                event_type=p.get("event_type", "Other"),
                cost_usd=0.0,
            ),
            accepted=True, override_snippet="{}", override_outcome="none",
        ))
    return tpl.render(
        title=f"audit sample — model {payload.get('model')}",
        rows=rows, categories=[],
    )
```

- [ ] **Step 3: Run all tests**

Run: `cd scripts/ai_matcher && uv run pytest -v`
Expected: all pass (no test changes — just behavior preservation).

- [ ] **Step 4: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/pipeline.py
git commit -m "Render audit-sample HTML via report template for sort+filter"
```

---

## Task 18: Pre-merge live smoke checks

**Files:** none modified — manual verification only.

- [ ] **Step 1: Verify the embeddings-only path runs end-to-end against live APIs**

```bash
cd scripts/ai_matcher
INGEST_KALSHI_MAX_EVENTS=200 INGEST_POLY_LIMIT=500 EMBEDDINGS_ONLY=1 \
  uv run python -m ai_matcher run
```

Expected: a summary line that includes `"ingested"`, `"bucketed"`, `"candidates_after_retrieval"`, `"drops_at_date_overlap"`, `"accepted"` keys. No exceptions. `audit/report.html` exists.

- [ ] **Step 2: Verify sort + filter work in the browser**

```bash
open scripts/ai_matcher/audit/report.html
```

Click each column header — rows reorder ascending, then descending. Type into the filter input — rows narrow as you type, restore when cleared. Scroll long table — header stays visible.

- [ ] **Step 3: (Optional) Verify the LiteLLM path with a real provider**

Set the appropriate API key for your chosen provider (e.g., `export OPENAI_API_KEY=sk-...`), then:

```bash
cd scripts/ai_matcher
LLM_MODEL=gpt-4.1-mini INGEST_KALSHI_MAX_EVENTS=50 INGEST_POLY_LIMIT=100 \
  uv run python -m ai_matcher run
```

Expected: `verifier_cost_usd` is non-zero in the summary (a few cents). Spot-check `audit/report.html` for accept/reject decisions with reasoning.

- [ ] **Step 4: (Optional) Promote to full caps for a real run**

Once steps 1–3 look clean:

```bash
cd scripts/ai_matcher
LLM_MODEL=gpt-4.1-mini uv run python -m ai_matcher run
```

Expected: 5–10 minute first-run wall time; ~$10–15 in LLM cost (visible in summary); `accepted` count is materially > 0.

- [ ] **Step 5: Final commit (if any cleanup edits surfaced during smoke testing)**

```bash
git status
git diff
# Commit any small fixes the smoke run surfaced. If nothing changed, skip.
```

---

## Self-Review Checklist (run after writing the plan, before handing off)

**Spec coverage:**

- §1 Category equivalence config — Tasks 1–3 ✓
- §2 Date overlap predicate + UTC normalization — Tasks 5, 6, 7, 14 ✓
- §3 Bucket-aware retrieval — Task 11 ✓
- §4 LiteLLM verifier swap — Tasks 12, 13 ✓
- §5 Ingestion expansion — Tasks 8, 9, 10 ✓
- §6 Audit log + observability — Tasks 15, 16 ✓
- §7 Interactive HTML report — Task 16 (sort/filter/sticky), Task 17 (audit-sample) ✓
- Configuration recap (env vars) — defaults set in Tasks 8 (`INGEST_POLY_LIMIT=10000`), 9 (`INGEST_KALSHI_MAX_EVENTS=2000`), 10 (`MIN_VOLUME_USD=1000`), 13 (`LLM_MODEL=gpt-4.1-mini`), 15 (`EXPIRY_TOLERANCE_SCALE=1.0`) ✓
- Pre-merge live smoke — Task 18 ✓

**Type consistency:**

- `Market.bucket: str` (Task 4) — used in Tasks 6, 7, 11, 14, 15 with same shape ✓
- `Market.close_time_utc: datetime | None` (Task 4) — populated in Tasks 6, 7; checked-non-None in Task 14 ✓
- `Market.tags: list[str]` (Task 4) — populated in Task 7; consumed in Task 3's `resolve_bucket` ✓
- `CategoryConfig.buckets: dict[str, BucketDef]` (Task 2) — accessed identically in Tasks 6, 7, 14, 15 ✓
- `Decision.cost_usd: float` (Task 13) — read in Task 15 via `getattr(..., "cost_usd", 0.0)` ✓
- `BucketedHnswRetrieval.query(vector, bucket: str)` (Task 11) — called with `k.bucket` in Task 15 ✓
- `PairAuditRow` new fields (Task 16) — populated in Task 15's `rows.append(...)` ✓

**No placeholders:** every code step contains the full code change. No `TBD`/`TODO`/`fill in` markers.

**Files referenced exist or are created within the plan:**
- `config/category_equivalence.json` — created Task 1
- `scripts/ai_matcher/src/ai_matcher/categories.py` — created Task 2
- `scripts/ai_matcher/tests/test_categories.py` — created Task 2
- All other files are pre-existing in the repo per the spec's file-level change list.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-02-matching-prefilter-and-llm-swap.md`.

Two execution options:

1. **Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?

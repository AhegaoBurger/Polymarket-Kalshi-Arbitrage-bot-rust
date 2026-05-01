# PR 3 — AI Matcher Sidecar Implementation Plan

> **Executor note:** Inline execution. Each task ends with a green test run + a single commit. Use absolute paths for all tool calls — `cd` does not persist between Bash calls; chain commands with `&&` or use `git -C <abs-path>`. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a standalone Python sidecar (`scripts/ai_matcher/`) that uses local sentence-transformers embeddings + Claude LLM verification to pair Kalshi and Polymarket markets that the structured adapters can't reach. Produces a human-auditable HTML report + `.ai_matches.json` consumed by Rust behind a freshness gate and execution gate (default off).

**Architecture:**
- **Python (`uv` package).** `scripts/ai_matcher/` is a self-contained `uv` project with src layout. Runs as `uv run python -m ai_matcher run` from inside that directory. Two-layer matching: (1) local sentence-transformers embeddings + hnswlib retrieval pre-filter, (2) Claude `claude-opus-4-7` verification with structured output. Writes three outputs every run: `.ai_matches.json` (machine), `audit/report.html` (human), `.ai_matcher_audit.jsonl` (append-only log).
- **Rust integration.** `src/adapters/ai_reader.rs` loads `.ai_matches.json`, enforces a freshness gate (default 24h), dedupes against structured pairs (structured wins on collision), and emits `MarketPair`s with `MatchSource::Ai{...}`. `EXEC_ALLOW_AI_MATCHES=0` is the default — execution path drops these pairs unless overridden.

**Tech Stack:**
- Python 3.11+ via `uv` (already at 0.9.7).
- `sentence-transformers` (`all-MiniLM-L6-v2` default, 384-dim, ~80 MB, runs on CPU).
- `hnswlib` for in-memory ANN.
- `anthropic` SDK for Claude verification.
- `jinja2` for HTML report.
- `httpx` for direct Kalshi + Polymarket REST calls (pmxt deferred — see Task 3).
- Rust 2021, `serde_json` for `.ai_matches.json` parsing, `chrono` for freshness gate.

**Spec reference:** `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` §4.6 (sidecar), §4.6.1–4.6.4 (layers + audit), §4.7 (scheduling), §4.8 (safety gates), §7 PR 3 (acceptance), Appendix A (LLM prompt).

**Spec deviations recorded in Task 1:**
- Embedding model: **local sentence-transformers `all-MiniLM-L6-v2`** instead of OpenAI `text-embedding-3-small`. Spec §4.6.2 will be updated in Task 1.
- Python project: **`uv` package with src layout** instead of loose scripts. Spec §4.6 mentions `pip install -r requirements.txt`; we use `uv sync` instead.
- Ingestion: **direct httpx REST calls** (Kalshi + Polymarket Gamma) instead of `pmxt`. The facade boundary stays — if `pmxt` matures we can swap it in behind `Ingestion` in one place. Documented as the "direct-REST fallback" already anticipated by spec Appendix C.

**Pre-flight (executor, before Task 1):**
- On branch `pr3/ai-matcher` in worktree `.worktrees/pr3-ai-matcher`.
- `cargo test --lib` baseline green at `56cafe6` (118 tests).
- `which uv` — present at 0.9.7.
- Anthropic API key reachable as `ANTHROPIC_API_KEY` for live LLM tests in Task 6 (mock-based unit tests work without it).

---

## File Structure

**New — Python sidecar at `scripts/ai_matcher/`**
```
scripts/ai_matcher/
├── pyproject.toml          # uv project config, deps, ruff
├── uv.lock                 # uv-managed lock
├── README.md               # CLI usage cheatsheet
├── src/
│   └── ai_matcher/
│       ├── __init__.py
│       ├── __main__.py     # entry — `python -m ai_matcher`
│       ├── cli.py          # argparse + subcommand dispatch
│       ├── ingestion.py    # Kalshi + Gamma httpx fetchers, IngestionResult dataclass
│       ├── embedder.py     # sentence-transformers wrapper + content-hash cache
│       ├── retrieval.py    # hnswlib index + top-K filter
│       ├── verifier.py     # Anthropic Claude verification + cache
│       ├── overrides.py    # manual_overrides.json apply
│       ├── report.py       # Jinja2 audit/report.html generator
│       ├── scheduler.py    # per-category TTL loop
│       ├── pipeline.py     # `run` orchestration → outputs
│       └── templates/
│           └── report.html.j2
└── tests/
    ├── test_embedder.py
    ├── test_retrieval.py
    ├── test_verifier.py
    ├── test_overrides.py
    ├── test_report.py
    └── test_pipeline.py
```

**New — config / fixtures (project root)**
- `config/manual_overrides.json` — seeded empty `{ "version": 1, "whitelist": [], "blacklist": [] }`.
- `config/ai_categories.json` — category list with TTLs (spec §4.7 table).

**New — Rust**
- `src/adapters/ai_reader.rs` — loader, freshness gate, MarketPair emitter. Has unit tests against fixture JSON.

**Modified — Rust**
- `src/lib.rs` / `src/main.rs` — declare `pub mod ai_reader` (under `adapters`) + register reader in `DiscoveryClient`.
- `src/adapters/mod.rs` — `pub mod ai_reader;`.
- `src/discovery.rs` — merge AI pairs after structured pairs, dedupe on `(kalshi_market_ticker, poly_condition_id)` with structured-wins.
- `src/execution.rs` — extend `should_block_for_detection_only` to also block `MatchSource::Ai` unless `EXEC_ALLOW_AI_MATCHES=1`.
- `src/config.rs` — add `exec_allow_ai_matches()` + `ai_matches_max_age_secs()` env helpers.
- `.gitignore` — add `scripts/ai_matcher/.venv/`, `scripts/ai_matcher/.uv-cache/`, `.ai_matches.json`, `.ai_matches.json.tmp`, `.ai_matcher_cache.json`, `.ai_matcher_audit.jsonl`, `.ai_matcher_schedule.json`, `audit/`.

**Modified — spec**
- `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` — note local-sentence-transformers + uv + httpx-direct deviation in §4.6.

---

## Task 1: uv project skeleton + spec deviation note

**Files:**
- Create: `scripts/ai_matcher/pyproject.toml`, `scripts/ai_matcher/src/ai_matcher/__init__.py`, `scripts/ai_matcher/src/ai_matcher/__main__.py`, `scripts/ai_matcher/README.md`
- Create: `scripts/ai_matcher/tests/__init__.py`
- Modify: `.gitignore`, `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` (§4.6 deviation note)

- [ ] **Step 1: Create uv project**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher
uv init --package --name ai-matcher --no-readme scripts/ai_matcher
```

This creates the canonical uv layout: `scripts/ai_matcher/pyproject.toml`, `scripts/ai_matcher/src/ai_matcher/__init__.py`, plus a default `__init__.py`. Verify with `ls scripts/ai_matcher/`.

- [ ] **Step 2: Replace generated `__init__.py` with a hello-world module + `__main__.py` entrypoint**

Replace `scripts/ai_matcher/src/ai_matcher/__init__.py` with:

```python
"""ai_matcher — standalone Python sidecar that pairs Kalshi and Polymarket markets via embeddings + Claude verification.

See docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.6.
"""

__version__ = "0.1.0"
```

Create `scripts/ai_matcher/src/ai_matcher/__main__.py`:

```python
"""Entry point for `python -m ai_matcher`. Delegates to cli.main()."""

from ai_matcher.cli import main

if __name__ == "__main__":
    raise SystemExit(main())
```

Create `scripts/ai_matcher/src/ai_matcher/cli.py`:

```python
"""Command-line dispatcher for the ai_matcher sidecar."""

from __future__ import annotations

import argparse
import sys


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="ai_matcher", description="AI matcher sidecar")
    sub = p.add_subparsers(dest="command", required=True)

    run = sub.add_parser("run", help="One discovery pass")
    run.add_argument("--loop", dest="loop_mode", action="store_true",
                     help="Loop with per-category TTLs")
    run.add_argument("--category", help="Restrict to a single category")
    run.add_argument("--sample", type=int, help="Cap sample size per category")

    sub.add_parser("review", help="Open audit/report.html")
    audit = sub.add_parser("audit", help="Random spot-check accepted pairs")
    audit.add_argument("--sample", type=int, default=20)

    sub.add_parser("calibrate-fees", help="One-shot feeSchedule survey")
    return p


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command == "run":
        print(f"[ai_matcher] run (loop={args.loop_mode}, category={args.category}, sample={args.sample})")
        return 0
    if args.command == "review":
        print("[ai_matcher] review — not yet wired (Task 11)")
        return 0
    if args.command == "audit":
        print(f"[ai_matcher] audit --sample {args.sample} — not yet wired (Task 11)")
        return 0
    if args.command == "calibrate-fees":
        print("[ai_matcher] calibrate-fees — not yet wired")
        return 0
    return 0
```

- [ ] **Step 3: Add `pyproject.toml` deps section + ruff config**

Replace the generated `pyproject.toml` with:

```toml
[project]
name = "ai-matcher"
version = "0.1.0"
description = "Standalone sidecar that pairs Kalshi and Polymarket markets via embeddings + LLM verification"
readme = "README.md"
requires-python = ">=3.11"
dependencies = []

[project.scripts]
ai_matcher = "ai_matcher.cli:main"

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[tool.hatch.build.targets.wheel]
packages = ["src/ai_matcher"]

[tool.ruff]
line-length = 100
target-version = "py311"

[tool.ruff.lint]
select = ["E", "F", "W", "I", "B", "UP", "RUF"]
ignore = ["E501"]  # ruff format handles line length

[tool.pytest.ini_options]
pythonpath = ["src"]
testpaths = ["tests"]
```

- [ ] **Step 4: Create README.md stub**

Create `scripts/ai_matcher/README.md`:

```markdown
# ai_matcher — AI Matcher Sidecar

Standalone Python sidecar that pairs Kalshi and Polymarket markets via local
sentence-transformers embeddings + Claude LLM verification. Runs without the
Rust bot. Spec: `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` §4.6.

## Quickstart

```bash
cd scripts/ai_matcher
uv sync                                 # install deps into .venv
uv run python -m ai_matcher run         # one discovery pass
uv run python -m ai_matcher run --loop  # loop with per-category TTLs
uv run python -m ai_matcher review      # open audit/report.html
uv run python -m ai_matcher audit --sample 20
```

## Required env

- `ANTHROPIC_API_KEY` — for the LLM verification stage
- `KALSHI_API_KEY_ID` and a `kalshi_private_key.txt` at the project root — for Kalshi REST
- (No OpenAI key required — embeddings run locally on CPU.)

## Outputs

| Path | Format | Audience |
|---|---|---|
| `.ai_matches.json` | JSON | Rust `ai_reader` |
| `audit/report.html` | static HTML | human review |
| `.ai_matcher_audit.jsonl` | JSONL | append-only audit trail |
```

- [ ] **Step 5: Create empty tests package**

Create `scripts/ai_matcher/tests/__init__.py` as an empty file.

- [ ] **Step 6: First `uv sync` (no deps yet) + smoke test**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/scripts/ai_matcher && uv sync && uv run python -m ai_matcher run
```

Expected output:

```
[ai_matcher] run (loop=False, category=None, sample=None)
```

- [ ] **Step 7: Update spec §4.6 with deviation note**

Find the line in `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` that mentions OpenAI embeddings (currently §4.6.2 line ~371) and inject a deviation banner above §4.6:

Edit the section header `### 4.6 AI Matcher Sidecar` line, append directly under it (before the existing prose):

```
> **PR 3 deviations from this spec section** (resolved 2026-05-01 during impl):
> 1. Embedding model is local **`sentence-transformers/all-MiniLM-L6-v2`** instead of OpenAI `text-embedding-3-small`. Avoids a second vendor key; CPU-only inference is fast enough for our scale (tens of thousands of markets).
> 2. Python project uses **`uv`** (not raw `pip install -r requirements.txt`). Layout: `scripts/ai_matcher/` as a uv-managed package; entry is `uv run python -m ai_matcher`. The `requirements.txt` reference in the spec is superseded by `pyproject.toml` + `uv.lock`.
> 3. Ingestion uses **direct `httpx` REST calls** (Kalshi v2 + Polymarket Gamma) instead of `pmxt`. The Ingestion facade boundary stays — pmxt can be slotted in behind it later if the project matures. This is the "direct-REST fallback" that Appendix C already anticipated.
```

- [ ] **Step 8: Update root `.gitignore`**

Append to `.gitignore`:

```
# ai_matcher sidecar
scripts/ai_matcher/.venv/
scripts/ai_matcher/uv.lock
scripts/ai_matcher/.uv-cache/
scripts/ai_matcher/__pycache__/
scripts/ai_matcher/**/__pycache__/
.ai_matches.json
.ai_matches.json.tmp
.ai_matcher_cache.json
.ai_matcher_audit.jsonl
.ai_matcher_schedule.json
audit/
```

(`uv.lock` is gitignored for now — we'll commit it once the dep set is final, in Task 2 step 7.)

- [ ] **Step 9: Commit**

```bash
git add scripts/ai_matcher/ docs/superpowers/specs/2026-04-21-multi-category-matching-design.md .gitignore
git commit -m "Bootstrap ai_matcher uv package skeleton with CLI stub"
```

---

## Task 2: Add Python deps + first test

**Files:**
- Modify: `scripts/ai_matcher/pyproject.toml`
- Create: `scripts/ai_matcher/tests/test_cli.py`

- [ ] **Step 1: Add deps**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/scripts/ai_matcher && uv add sentence-transformers hnswlib anthropic jinja2 httpx
uv add --dev pytest
```

`sentence-transformers` brings transformers + torch; this is a few hundred MB. Expect the first sync to take ~1-2 min on a fresh venv.

- [ ] **Step 2: Write a failing test for the CLI dispatcher**

Create `scripts/ai_matcher/tests/test_cli.py`:

```python
"""Smoke tests for cli.main — ensures argparse wiring is intact."""

from __future__ import annotations

import io
import sys

from ai_matcher.cli import main


def test_run_subcommand_returns_zero(capsys):
    rc = main(["run"])
    captured = capsys.readouterr()
    assert rc == 0
    assert "[ai_matcher] run" in captured.out


def test_run_subcommand_with_flags(capsys):
    rc = main(["run", "--loop", "--category", "politics", "--sample", "50"])
    captured = capsys.readouterr()
    assert rc == 0
    assert "loop=True" in captured.out
    assert "category=politics" in captured.out
    assert "sample=50" in captured.out


def test_audit_subcommand_default_sample(capsys):
    rc = main(["audit"])
    captured = capsys.readouterr()
    assert rc == 0
    assert "--sample 20" in captured.out


def test_review_subcommand(capsys):
    rc = main(["review"])
    assert rc == 0


def test_no_subcommand_errors():
    with pytest.raises(SystemExit):
        main([])
```

The last test imports `pytest`. Add the import at the top:

```python
import pytest
```

- [ ] **Step 3: Run tests**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/scripts/ai_matcher && uv run pytest -v
```

Expected: 5 passing.

- [ ] **Step 4: Verify `uv lock` is reproducible**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/scripts/ai_matcher && uv sync --frozen
```

Expected: no changes to lockfile.

- [ ] **Step 5: Commit (commit the lockfile this time)**

Update `.gitignore` to **un-ignore** `uv.lock` — remove the `scripts/ai_matcher/uv.lock` line. The lockfile should be committed for reproducible installs.

```bash
git add scripts/ai_matcher/pyproject.toml scripts/ai_matcher/uv.lock scripts/ai_matcher/tests/ .gitignore
git commit -m "Pin ai_matcher deps via uv (sentence-transformers, anthropic, jinja2, httpx, hnswlib)"
```

---

## Task 3: Ingestion module — Kalshi + Polymarket via httpx

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/ingestion.py`
- Create: `scripts/ai_matcher/tests/test_ingestion.py`

**What's in:** `IngestionResult` dataclass, `Market` dataclass, `Ingestion` class with `fetch_kalshi()` + `fetch_poly()` methods, content-hash helper.
**What's out:** Live network calls in tests — those go through mocked httpx responses. Live integration is exercised only in the Task 13 acceptance smoke run.

- [ ] **Step 1: Define dataclasses + parser primitives + write failing tests**

Create `scripts/ai_matcher/tests/test_ingestion.py`:

```python
"""Tests for ingestion.py — uses recorded JSON fixtures, no network."""

from __future__ import annotations

from ai_matcher.ingestion import (
    Market,
    parse_kalshi_markets_response,
    parse_poly_gamma_markets_response,
    content_hash,
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
        {"slug": "x", "question": "q", "outcomes": "[]", "clobTokenIds": "[]",
         "conditionId": "0xC", "active": True, "closed": True}
    ]
    assert parse_poly_gamma_markets_response(body) == []


def test_content_hash_is_stable():
    a = content_hash("alpha", "beta", "gamma")
    b = content_hash("alpha", "beta", "gamma")
    assert a == b
    c = content_hash("alpha", "beta", "delta")
    assert a != c
```

- [ ] **Step 2: Implement `ingestion.py`**

Create `scripts/ai_matcher/src/ai_matcher/ingestion.py`:

```python
"""Market ingestion: Kalshi v2 REST + Polymarket Gamma REST.

Direct httpx calls, no pmxt dependency. Documented as the spec's anticipated
"direct-REST fallback" (Appendix C).
"""

from __future__ import annotations

import hashlib
import json
import os
from dataclasses import dataclass, field
from typing import Iterable

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
        return content_hash(self.title, self.description, self.resolution_criteria,
                            "|".join(self.outcomes))


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
        # Public Kalshi events endpoint accepts no auth for read-only browse.
        # If auth is needed for some surface, plumb credentials here.
        resp = self._http.get(f"{KALSHI_API_BASE}/markets?limit={limit}&status=open")
        resp.raise_for_status()
        return parse_kalshi_markets_response(resp.json())

    def fetch_poly(self, limit: int = 1000) -> list[Market]:
        resp = self._http.get(f"{GAMMA_API_BASE}/markets?limit={limit}&active=true&closed=false")
        resp.raise_for_status()
        body = resp.json() if isinstance(resp.json(), list) else []
        return parse_poly_gamma_markets_response(body)
```

- [ ] **Step 3: Run tests**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/scripts/ai_matcher && uv run pytest tests/test_ingestion.py -v
```

Expected: 4 passing.

- [ ] **Step 4: Run full Python suite + ruff**

```bash
uv run pytest -v && uv run ruff check src/ tests/
```

Expected: all passing, no ruff warnings.

- [ ] **Step 5: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/ingestion.py scripts/ai_matcher/tests/test_ingestion.py
git commit -m "Add Ingestion facade with httpx Kalshi/Gamma fetchers and pure parsers"
```

---

## Task 4: Embedder — sentence-transformers + content-hash cache

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/embedder.py`
- Create: `scripts/ai_matcher/tests/test_embedder.py`

`Embedder` wraps a `SentenceTransformer` model and a JSON cache keyed on content hash. Unchanged markets skip re-embedding.

- [ ] **Step 1: Failing tests**

Create `scripts/ai_matcher/tests/test_embedder.py`:

```python
"""Tests for embedder.py — uses the real sentence-transformers model.
The first run downloads ~80 MB; subsequent runs are cached."""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import pytest

from ai_matcher.embedder import Embedder, EmbeddingCache
from ai_matcher.ingestion import Market


@pytest.fixture
def cache_path(tmp_path: Path) -> Path:
    return tmp_path / "embed_cache.json"


def mk_market(title: str, platform: str = "kalshi") -> Market:
    return Market(platform=platform, ticker="t", title=title, description="d")


def test_embedder_produces_consistent_vectors(cache_path: Path):
    e = Embedder(cache_path=cache_path)
    m = mk_market("Will the FOMC cut rates in May 2026?")
    v1 = e.embed(m)
    v2 = e.embed(m)
    assert np.allclose(v1, v2)
    assert v1.shape == (e.dim,)


def test_embedder_cache_hits_on_unchanged_content(cache_path: Path):
    e = Embedder(cache_path=cache_path)
    m = mk_market("X")
    e.embed(m)
    e.flush()
    e2 = Embedder(cache_path=cache_path)
    assert e2.cache.size > 0
    # Same hash → cache hit
    e2.embed(m)
    assert e2.cache_hits == 1


def test_embedder_cache_misses_on_changed_content(cache_path: Path):
    e = Embedder(cache_path=cache_path)
    e.embed(mk_market("A"))
    e.embed(mk_market("B"))
    assert e.cache_hits == 0
    assert e.cache_misses == 2
```

- [ ] **Step 2: Implement `embedder.py`**

Create `scripts/ai_matcher/src/ai_matcher/embedder.py`:

```python
"""Local sentence-transformers embedder with a content-hash JSON cache.

Default model: `sentence-transformers/all-MiniLM-L6-v2` (384-dim, ~80 MB on disk).
Override with the `EMBEDDING_MODEL` environment variable.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from pathlib import Path

import numpy as np
from sentence_transformers import SentenceTransformer

from ai_matcher.ingestion import Market

DEFAULT_MODEL = "sentence-transformers/all-MiniLM-L6-v2"


@dataclass
class EmbeddingCache:
    """JSON-backed cache: { content_hash: [floats...] } keyed by model name to invalidate on bump."""
    path: Path
    model_name: str
    _by_hash: dict[str, list[float]]

    @classmethod
    def load(cls, path: Path, model_name: str) -> "EmbeddingCache":
        data: dict[str, dict] = {}
        if path.exists():
            try:
                data = json.loads(path.read_text()) or {}
            except json.JSONDecodeError:
                data = {}
        section = data.get(model_name, {})
        return cls(path=path, model_name=model_name, _by_hash=section)

    def save(self) -> None:
        # Read-modify-write so we don't clobber other models' caches.
        existing: dict[str, dict] = {}
        if self.path.exists():
            try:
                existing = json.loads(self.path.read_text()) or {}
            except json.JSONDecodeError:
                existing = {}
        existing[self.model_name] = self._by_hash
        self.path.write_text(json.dumps(existing))

    def get(self, content_hash: str) -> np.ndarray | None:
        v = self._by_hash.get(content_hash)
        return np.array(v, dtype=np.float32) if v is not None else None

    def put(self, content_hash: str, vec: np.ndarray) -> None:
        self._by_hash[content_hash] = vec.astype(float).tolist()

    @property
    def size(self) -> int:
        return len(self._by_hash)


class Embedder:
    def __init__(self, cache_path: Path, model_name: str | None = None) -> None:
        self.model_name = model_name or os.environ.get("EMBEDDING_MODEL", DEFAULT_MODEL)
        self._model = SentenceTransformer(self.model_name)
        self.dim: int = self._model.get_sentence_embedding_dimension()
        self.cache = EmbeddingCache.load(cache_path, self.model_name)
        self.cache_hits = 0
        self.cache_misses = 0

    def embed(self, market: Market) -> np.ndarray:
        h = market.content_hash()
        cached = self.cache.get(h)
        if cached is not None:
            self.cache_hits += 1
            return cached
        vec = self._model.encode(market.text_for_embedding(), normalize_embeddings=True)
        vec = np.asarray(vec, dtype=np.float32)
        self.cache.put(h, vec)
        self.cache_misses += 1
        return vec

    def flush(self) -> None:
        self.cache.save()
```

- [ ] **Step 3: Run tests**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/scripts/ai_matcher && uv run pytest tests/test_embedder.py -v
```

First run downloads the model (~80 MB). Subsequent runs reuse the HF cache.

Expected: 3 passing.

- [ ] **Step 4: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/embedder.py scripts/ai_matcher/tests/test_embedder.py
git commit -m "Add local sentence-transformers Embedder with content-hash JSON cache"
```

---

## Task 5: Retrieval — hnswlib top-K + cosine threshold

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/retrieval.py`
- Create: `scripts/ai_matcher/tests/test_retrieval.py`

Builds a per-run hnswlib index of Polymarket embeddings; for each Kalshi market, returns top-K candidates above `MIN_COSINE`.

- [ ] **Step 1: Failing test**

Create `scripts/ai_matcher/tests/test_retrieval.py`:

```python
from __future__ import annotations

import numpy as np

from ai_matcher.retrieval import HnswRetrieval


def test_retrieval_returns_topk_candidates_above_threshold():
    rng = np.random.default_rng(0)
    poly_vecs = rng.standard_normal((100, 16)).astype(np.float32)
    poly_vecs /= np.linalg.norm(poly_vecs, axis=1, keepdims=True)
    poly_ids = [f"p{i}" for i in range(100)]
    r = HnswRetrieval(dim=16, top_k=5, min_cosine=0.0)
    r.build(poly_vecs, poly_ids)
    query = poly_vecs[42] + 0.01 * rng.standard_normal(16).astype(np.float32)
    query /= np.linalg.norm(query)
    hits = r.query(query)
    assert len(hits) == 5
    # The exact match should be the top hit.
    assert hits[0][0] == "p42"


def test_retrieval_filters_by_min_cosine():
    rng = np.random.default_rng(0)
    poly_vecs = rng.standard_normal((10, 16)).astype(np.float32)
    poly_vecs /= np.linalg.norm(poly_vecs, axis=1, keepdims=True)
    r = HnswRetrieval(dim=16, top_k=10, min_cosine=0.99)
    r.build(poly_vecs, [f"p{i}" for i in range(10)])
    query = poly_vecs[0]
    hits = r.query(query)
    # Only the exact match should clear 0.99 threshold.
    assert all(score >= 0.99 for _, score in hits)
    assert hits[0][0] == "p0"
```

- [ ] **Step 2: Implement `retrieval.py`**

Create `scripts/ai_matcher/src/ai_matcher/retrieval.py`:

```python
"""HNSW retrieval over normalized embeddings.

`min_cosine` pre-filter strips obvious non-matches before the LLM stage spends tokens.
"""

from __future__ import annotations

import hnswlib
import numpy as np


class HnswRetrieval:
    def __init__(self, dim: int, top_k: int = 8, min_cosine: float = 0.55) -> None:
        self.dim = dim
        self.top_k = top_k
        self.min_cosine = min_cosine
        self._index: hnswlib.Index | None = None
        self._ids: list[str] = []

    def build(self, vectors: np.ndarray, ids: list[str]) -> None:
        assert vectors.shape[0] == len(ids)
        idx = hnswlib.Index(space="cosine", dim=self.dim)
        idx.init_index(max_elements=len(ids), ef_construction=200, M=16)
        idx.add_items(vectors, ids=np.arange(len(ids)))
        idx.set_ef(50)
        self._index = idx
        self._ids = list(ids)

    def query(self, vector: np.ndarray) -> list[tuple[str, float]]:
        assert self._index is not None, "build() must be called first"
        labels, distances = self._index.knn_query(vector, k=min(self.top_k, len(self._ids)))
        # hnswlib returns 1 - cosine_similarity for the cosine space.
        out: list[tuple[str, float]] = []
        for label, dist in zip(labels[0], distances[0], strict=False):
            cosine = 1.0 - float(dist)
            if cosine >= self.min_cosine:
                out.append((self._ids[label], cosine))
        return out
```

- [ ] **Step 3: Run tests**

```bash
uv run pytest tests/test_retrieval.py -v
```

Expected: 2 passing.

- [ ] **Step 4: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/retrieval.py scripts/ai_matcher/tests/test_retrieval.py
git commit -m "Add hnswlib retrieval with top-K + min_cosine pre-filter"
```

---

## Task 6: Verifier — Claude structured-output verification + cache

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/verifier.py`
- Create: `scripts/ai_matcher/tests/test_verifier.py`

The verifier receives a (kalshi, poly) candidate pair and asks Claude (`claude-opus-4-7`) to score *resolution-criteria identity*, returning structured JSON. Caches by `(kalshi_hash, poly_hash, model)`.

- [ ] **Step 1: Failing test (mocked SDK)**

Create `scripts/ai_matcher/tests/test_verifier.py`:

```python
from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock

import pytest

from ai_matcher.ingestion import Market
from ai_matcher.verifier import Decision, Verifier


def mk_pair():
    k = Market(platform="kalshi", ticker="KX", title="A?", description="d", resolution_criteria="r",
               outcomes=["Yes", "No"])
    p = Market(platform="polymarket", ticker="poly", title="A?", description="d", resolution_criteria="r",
               outcomes=["Yes", "No"], condition_id="0xC")
    return k, p


def fake_anthropic_response(json_blob: dict) -> MagicMock:
    """Mimic anthropic SDK: client.messages.create(...) returns an object with .content[0].input == json_blob.

    We use the tool-call shape for structured output."""
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
        "reasoning": "identical resolution", "category": "Economics", "event_type": "Cpi",
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
```

- [ ] **Step 2: Implement `verifier.py`**

Create `scripts/ai_matcher/src/ai_matcher/verifier.py`:

```python
"""Claude verification of candidate pairs.

Uses the Anthropic SDK's tool-use mode to enforce structured JSON output.
Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.6.3 + Appendix A.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Any

from ai_matcher.ingestion import Market

DEFAULT_MODEL = "claude-opus-4-7"

VERIFIER_TOOL = {
    "name": "report_match_decision",
    "description": "Report whether two prediction markets resolve to identical outcomes.",
    "input_schema": {
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

    @property
    def accepted(self) -> bool:
        return (
            self.resolution_match
            and self.confidence >= 0.9
            and not self.concerns
        )


class Verifier:
    def __init__(self, client: Any, model: str = DEFAULT_MODEL,
                 cache_path: Path | None = None) -> None:
        self.client = client
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
        user_prompt = (
            f"Kalshi market:\n  Title: {kalshi.title}\n  Description: {kalshi.description}\n"
            f"  Resolution: {kalshi.resolution_criteria}\n  Outcomes: {kalshi.outcomes}\n\n"
            f"Polymarket market:\n  Title: {poly.title}\n  Description: {poly.description}\n"
            f"  Resolution: {poly.resolution_criteria}\n  Outcomes: {poly.outcomes}\n\n"
            "Do these resolve identically? Score accordingly."
        )

        resp = self.client.messages.create(
            model=self.model,
            max_tokens=1024,
            system=SYSTEM_PROMPT,
            tools=[VERIFIER_TOOL],
            tool_choice={"type": "tool", "name": "report_match_decision"},
            messages=[{"role": "user", "content": user_prompt}],
        )

        tool_input = self._extract_tool_input(resp)
        decision = Decision(
            confidence=float(tool_input["confidence"]),
            resolution_match=bool(tool_input["resolution_match"]),
            concerns=list(tool_input.get("concerns", [])),
            reasoning=str(tool_input.get("reasoning", "")),
            category=str(tool_input.get("category", "")),
            event_type=str(tool_input.get("event_type", "Other")),
        )
        self._cache[key] = asdict(decision)
        self._save_cache()
        return decision

    @staticmethod
    def _extract_tool_input(resp: Any) -> dict:
        for block in getattr(resp, "content", []) or []:
            if getattr(block, "type", None) == "tool_use":
                return getattr(block, "input", {}) or {}
        raise ValueError("Anthropic response missing tool_use block")
```

- [ ] **Step 3: Run tests**

```bash
uv run pytest tests/test_verifier.py -v
```

Expected: 3 passing. (No live network — fully mocked.)

- [ ] **Step 4: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/verifier.py scripts/ai_matcher/tests/test_verifier.py
git commit -m "Add Claude Verifier with tool-use structured output and decision cache"
```

---

## Task 7: Overrides — manual_overrides.json apply

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/overrides.py`
- Create: `scripts/ai_matcher/tests/test_overrides.py`
- Create: `config/manual_overrides.json` (seeded empty)

Spec rule: blacklist > whitelist > AI decision. Whitelist forces accept; blacklist forces reject.

- [ ] **Step 1: Failing test**

Create `scripts/ai_matcher/tests/test_overrides.py`:

```python
from __future__ import annotations

import json
from pathlib import Path

from ai_matcher.overrides import OverrideSet, OverrideOutcome


def write_override_file(p: Path, payload: dict) -> Path:
    p.write_text(json.dumps(payload))
    return p


def test_blacklist_forces_reject(tmp_path: Path):
    f = write_override_file(tmp_path / "o.json", {
        "version": 1,
        "whitelist": [],
        "blacklist": [
            {"kalshi_market_ticker": "K1", "poly_condition_id": "0xC1", "reason": "stale"}
        ],
    })
    o = OverrideSet.load(f)
    assert o.lookup("K1", "0xC1") == OverrideOutcome.BLACKLIST
    assert o.lookup("K2", "0xC2") == OverrideOutcome.NONE


def test_whitelist_forces_accept(tmp_path: Path):
    f = write_override_file(tmp_path / "o.json", {
        "version": 1,
        "whitelist": [
            {"kalshi_market_ticker": "K1", "poly_condition_id": "0xC1",
             "category": "Economics", "reason": "verified"}
        ],
        "blacklist": [],
    })
    o = OverrideSet.load(f)
    assert o.lookup("K1", "0xC1") == OverrideOutcome.WHITELIST


def test_blacklist_wins_over_whitelist(tmp_path: Path):
    f = write_override_file(tmp_path / "o.json", {
        "version": 1,
        "whitelist": [{"kalshi_market_ticker": "K", "poly_condition_id": "0xC"}],
        "blacklist": [{"kalshi_market_ticker": "K", "poly_condition_id": "0xC", "reason": "x"}],
    })
    assert OverrideSet.load(f).lookup("K", "0xC") == OverrideOutcome.BLACKLIST


def test_missing_file_returns_empty_set(tmp_path: Path):
    o = OverrideSet.load(tmp_path / "missing.json")
    assert o.lookup("K", "0xC") == OverrideOutcome.NONE
```

- [ ] **Step 2: Implement `overrides.py`**

Create `scripts/ai_matcher/src/ai_matcher/overrides.py`:

```python
"""Manual override application: blacklist > whitelist > AI."""

from __future__ import annotations

import enum
import json
from dataclasses import dataclass
from pathlib import Path


class OverrideOutcome(enum.Enum):
    NONE = "none"
    WHITELIST = "whitelist"
    BLACKLIST = "blacklist"


@dataclass
class OverrideSet:
    whitelist: set[tuple[str, str]]
    blacklist: set[tuple[str, str]]

    @classmethod
    def load(cls, path: Path) -> "OverrideSet":
        if not path.exists():
            return cls(whitelist=set(), blacklist=set())
        try:
            data = json.loads(path.read_text())
        except json.JSONDecodeError:
            return cls(whitelist=set(), blacklist=set())
        return cls(
            whitelist={(e["kalshi_market_ticker"], e["poly_condition_id"])
                       for e in data.get("whitelist", []) or []},
            blacklist={(e["kalshi_market_ticker"], e["poly_condition_id"])
                       for e in data.get("blacklist", []) or []},
        )

    def lookup(self, kalshi_ticker: str, poly_condition_id: str) -> OverrideOutcome:
        key = (kalshi_ticker, poly_condition_id)
        if key in self.blacklist:
            return OverrideOutcome.BLACKLIST
        if key in self.whitelist:
            return OverrideOutcome.WHITELIST
        return OverrideOutcome.NONE
```

- [ ] **Step 3: Seed empty `config/manual_overrides.json`**

Create `config/manual_overrides.json` at the project root (NOT under scripts/):

```json
{
  "version": 1,
  "whitelist": [],
  "blacklist": []
}
```

- [ ] **Step 4: Run tests + commit**

```bash
uv run pytest tests/test_overrides.py -v
git add scripts/ai_matcher/src/ai_matcher/overrides.py scripts/ai_matcher/tests/test_overrides.py config/manual_overrides.json
git commit -m "Add OverrideSet with blacklist > whitelist precedence"
```

---

## Task 8: Report — Jinja2 audit/report.html generator

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/report.py`
- Create: `scripts/ai_matcher/src/ai_matcher/templates/report.html.j2`
- Create: `scripts/ai_matcher/tests/test_report.py`
- Modify: `scripts/ai_matcher/pyproject.toml` (include templates as package data)

`PairAuditRow` dataclass + `render_report(rows, out_dir)` writes `report.html` and per-filter variants (`report-accepted.html`, `report-rejected.html`, `report-by-category-economics.html`, etc.).

- [ ] **Step 1: Failing test**

Create `scripts/ai_matcher/tests/test_report.py`:

```python
from __future__ import annotations

from pathlib import Path

from ai_matcher.report import PairAuditRow, render_report
from ai_matcher.verifier import Decision


def mk_row(decision: str = "accept", confidence: float = 0.97, category: str = "Economics") -> PairAuditRow:
    return PairAuditRow(
        kalshi_ticker="KXCPIYOY-26APR-B3.0",
        kalshi_title="Kalshi market",
        kalshi_description="kdesc",
        kalshi_resolution="kr",
        kalshi_outcomes=["Yes", "No"],
        kalshi_url="https://kalshi.com/m/k1",
        poly_slug="cpi-may-2026",
        poly_title="Poly market",
        poly_description="pdesc",
        poly_resolution="pr",
        poly_outcomes=["Yes", "No"],
        poly_url="https://polymarket.com/event/poly1",
        decision=Decision(confidence=confidence, resolution_match=True, concerns=[],
                          reasoning="x", category=category, event_type="Cpi"),
        accepted=(decision == "accept"),
        override_snippet='{"kalshi_market_ticker":"KXCPIYOY-26APR-B3.0","poly_condition_id":"0xC"}',
    )


def test_renders_main_report(tmp_path: Path):
    rows = [mk_row("accept"), mk_row("reject", confidence=0.6, category="Politics")]
    render_report(rows, tmp_path)
    main = (tmp_path / "report.html").read_text()
    assert "Kalshi market" in main
    assert "Poly market" in main
    assert "0.97" in main
    assert "Politics" in main


def test_renders_filter_variants(tmp_path: Path):
    rows = [mk_row("accept"), mk_row("reject", confidence=0.6, category="Politics")]
    render_report(rows, tmp_path)
    accepted = (tmp_path / "report-accepted.html").read_text()
    rejected = (tmp_path / "report-rejected.html").read_text()
    assert "0.97" in accepted and "0.6" not in accepted
    assert "0.6" in rejected
```

- [ ] **Step 2: Implement template**

Create `scripts/ai_matcher/src/ai_matcher/templates/report.html.j2`:

```html
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>ai_matcher — {{ title }}</title>
<style>
  body { font-family: -apple-system, sans-serif; max-width: 1400px; margin: 1em auto; padding: 0 1em; }
  table { border-collapse: collapse; width: 100%; }
  th, td { border: 1px solid #ccc; padding: 0.5em; vertical-align: top; font-size: 0.9em; }
  th { background: #f4f4f4; text-align: left; }
  .accept { background: #e8f5e9; }
  .reject { background: #ffebee; }
  .conf { font-weight: bold; font-variant-numeric: tabular-nums; }
  pre.snippet { background: #fafafa; padding: 0.4em; font-size: 0.8em; white-space: pre-wrap; }
  .filters a { margin-right: 1em; }
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
<p>{{ rows | length }} pair(s).</p>
<table>
<thead>
<tr>
  <th>Decision</th>
  <th>Kalshi</th>
  <th>Polymarket</th>
  <th>LLM analysis</th>
  <th>Override snippet</th>
</tr>
</thead>
<tbody>
{% for row in rows %}
<tr class="{{ 'accept' if row.accepted else 'reject' }}">
  <td>
    {{ '✅' if row.accepted else '❌' }}
    <span class="conf">{{ '%.2f' | format(row.decision.confidence) }}</span><br>
    {{ row.decision.event_type }}
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
</body>
</html>
```

- [ ] **Step 3: Implement `report.py`**

Create `scripts/ai_matcher/src/ai_matcher/report.py`:

```python
"""Static HTML audit report generator (Jinja2)."""

from __future__ import annotations

import json
from dataclasses import dataclass
from importlib.resources import files
from pathlib import Path

from jinja2 import Environment, FileSystemLoader, select_autoescape

from ai_matcher.verifier import Decision


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


def _env() -> Environment:
    template_dir = files("ai_matcher").joinpath("templates")
    return Environment(
        loader=FileSystemLoader(str(template_dir)),
        autoescape=select_autoescape(["html"]),
    )


def render_report(rows: list[PairAuditRow], out_dir: Path) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    env = _env()
    tpl = env.get_template("report.html.j2")
    categories = sorted({r.decision.category for r in rows if r.decision.category})

    def render_to(name: str, title: str, subset: list[PairAuditRow]) -> None:
        (out_dir / name).write_text(tpl.render(title=title, rows=subset, categories=categories))

    render_to("report.html", "ai_matcher — all pairs", rows)
    render_to("report-accepted.html", "ai_matcher — accepted",
              [r for r in rows if r.accepted])
    render_to("report-rejected.html", "ai_matcher — rejected",
              [r for r in rows if not r.accepted])
    for cat in categories:
        render_to(
            f"report-by-category-{cat.lower()}.html",
            f"ai_matcher — {cat}",
            [r for r in rows if r.decision.category == cat],
        )
```

- [ ] **Step 4: Update `pyproject.toml` to include templates**

Add to the `pyproject.toml` `[tool.hatch.build.targets.wheel]` section:

```toml
[tool.hatch.build.targets.wheel.force-include]
"src/ai_matcher/templates" = "ai_matcher/templates"
```

- [ ] **Step 5: Run tests + commit**

```bash
uv run pytest tests/test_report.py -v
git add scripts/ai_matcher/src/ai_matcher/report.py scripts/ai_matcher/src/ai_matcher/templates/ scripts/ai_matcher/tests/test_report.py scripts/ai_matcher/pyproject.toml
git commit -m "Add Jinja2 audit report renderer with filter variants"
```

---

## Task 9: Scheduler — per-category TTL loop

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/scheduler.py`
- Create: `scripts/ai_matcher/tests/test_scheduler.py`
- Create: `config/ai_categories.json`

Spec table §4.7. Reads category TTLs, persists last-run-timestamps, returns the list of due categories.

- [ ] **Step 1: Failing test**

Create `scripts/ai_matcher/tests/test_scheduler.py`:

```python
from __future__ import annotations

import json
from pathlib import Path

from ai_matcher.scheduler import Scheduler


def test_scheduler_returns_all_categories_on_first_run(tmp_path: Path):
    cfg = tmp_path / "cats.json"
    cfg.write_text(json.dumps([
        {"name": "politics", "ttl_secs": 7200},
        {"name": "crypto", "ttl_secs": 900},
    ]))
    state = tmp_path / "sched.json"
    s = Scheduler(categories_path=cfg, state_path=state)
    due = s.due_categories(now_secs=1000)
    assert {c.name for c in due} == {"politics", "crypto"}


def test_scheduler_skips_recent_runs(tmp_path: Path):
    cfg = tmp_path / "cats.json"
    cfg.write_text(json.dumps([
        {"name": "politics", "ttl_secs": 100},
        {"name": "crypto", "ttl_secs": 100},
    ]))
    state = tmp_path / "sched.json"
    state.write_text(json.dumps({"politics": 950}))
    s = Scheduler(categories_path=cfg, state_path=state)
    due = s.due_categories(now_secs=1000)
    # politics ran at 950, ttl 100 → next eligible at 1050. Not due at 1000.
    assert {c.name for c in due} == {"crypto"}


def test_scheduler_marks_run_persists_state(tmp_path: Path):
    cfg = tmp_path / "cats.json"
    cfg.write_text(json.dumps([{"name": "politics", "ttl_secs": 100}]))
    state = tmp_path / "sched.json"
    s = Scheduler(categories_path=cfg, state_path=state)
    s.mark_ran("politics", now_secs=2000)
    s2 = Scheduler(categories_path=cfg, state_path=state)
    due = s2.due_categories(now_secs=2050)
    assert due == []
```

- [ ] **Step 2: Implement `scheduler.py`**

Create `scripts/ai_matcher/src/ai_matcher/scheduler.py`:

```python
"""Per-category TTL scheduler.

Categories live in `config/ai_categories.json`. Last-run state lives in
`.ai_matcher_schedule.json` at the project root.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path


@dataclass
class Category:
    name: str
    ttl_secs: int


class Scheduler:
    def __init__(self, categories_path: Path, state_path: Path) -> None:
        self.categories_path = categories_path
        self.state_path = state_path
        self._categories: list[Category] = self._load_categories()
        self._last_run: dict[str, int] = self._load_state()

    def _load_categories(self) -> list[Category]:
        if not self.categories_path.exists():
            return []
        data = json.loads(self.categories_path.read_text())
        return [Category(name=c["name"], ttl_secs=int(c["ttl_secs"])) for c in data]

    def _load_state(self) -> dict[str, int]:
        if not self.state_path.exists():
            return {}
        try:
            return {k: int(v) for k, v in json.loads(self.state_path.read_text()).items()}
        except (json.JSONDecodeError, ValueError):
            return {}

    def due_categories(self, now_secs: int) -> list[Category]:
        out: list[Category] = []
        for c in self._categories:
            last = self._last_run.get(c.name, 0)
            if now_secs - last >= c.ttl_secs:
                out.append(c)
        return out

    def mark_ran(self, name: str, now_secs: int) -> None:
        self._last_run[name] = now_secs
        self.state_path.write_text(json.dumps(self._last_run))
```

- [ ] **Step 3: Seed `config/ai_categories.json`**

Create `config/ai_categories.json` at project root:

```json
[
  { "name": "macro_econ",    "ttl_secs": 43200 },
  { "name": "elections",     "ttl_secs": 7200 },
  { "name": "crypto_hourly", "ttl_secs": 900 },
  { "name": "long_tail",     "ttl_secs": 21600 }
]
```

- [ ] **Step 4: Run tests + commit**

```bash
uv run pytest tests/test_scheduler.py -v
git add scripts/ai_matcher/src/ai_matcher/scheduler.py scripts/ai_matcher/tests/test_scheduler.py config/ai_categories.json
git commit -m "Add per-category TTL scheduler with persisted last-run state"
```

---

## Task 10: Pipeline — `run` command end-to-end + atomic outputs

**Files:**
- Create: `scripts/ai_matcher/src/ai_matcher/pipeline.py`
- Create: `scripts/ai_matcher/tests/test_pipeline.py`
- Modify: `scripts/ai_matcher/src/ai_matcher/cli.py`

`run_pipeline(...)` orchestrates ingestion → embedding → retrieval → verification → overrides → write `.ai_matches.json` (atomic), `audit/report.html`, append to `.ai_matcher_audit.jsonl`. Tests use mocked components.

- [ ] **Step 1: Failing test**

Create `scripts/ai_matcher/tests/test_pipeline.py`:

```python
from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import MagicMock

from ai_matcher.ingestion import Market, IngestionResult
from ai_matcher.pipeline import PipelineConfig, run_pipeline
from ai_matcher.verifier import Decision


def mk_kalshi(t: str) -> Market:
    return Market(platform="kalshi", ticker=t, title=t, resolution_criteria=t)


def mk_poly(t: str) -> Market:
    return Market(platform="polymarket", ticker=t, title=t, condition_id=f"0x{t}",
                  poly_yes_token=f"y{t}", poly_no_token=f"n{t}", resolution_criteria=t)


def test_pipeline_writes_three_outputs_with_one_accepted_pair(tmp_path: Path):
    project_root = tmp_path
    config_path = project_root / "config"
    config_path.mkdir()
    (config_path / "manual_overrides.json").write_text(json.dumps(
        {"version": 1, "whitelist": [], "blacklist": []}
    ))

    ingestion = MagicMock()
    ingestion.fetch_all.return_value = IngestionResult(
        kalshi=[mk_kalshi("CPI")],
        poly=[mk_poly("CPI"), mk_poly("BTC")],
    )

    embedder = MagicMock()
    import numpy as np
    embedder.dim = 4
    embedder.embed.return_value = np.array([1.0, 0.0, 0.0, 0.0], dtype=np.float32)
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
```

- [ ] **Step 2: Implement `pipeline.py`**

Create `scripts/ai_matcher/src/ai_matcher/pipeline.py`:

```python
"""End-to-end run pipeline: ingestion → embedding → retrieval → verification → outputs."""

from __future__ import annotations

import datetime as dt
import json
import os
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

from ai_matcher.ingestion import Ingestion, IngestionResult, Market
from ai_matcher.overrides import OverrideOutcome, OverrideSet
from ai_matcher.report import PairAuditRow, render_report
from ai_matcher.retrieval import HnswRetrieval
from ai_matcher.verifier import Decision, Verifier


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


def _atomic_write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=path.name + ".", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(payload, f, indent=2, sort_keys=True)
        os.replace(tmp_name, path)
    except Exception:
        if os.path.exists(tmp_name):
            os.unlink(tmp_name)
        raise


def _kalshi_url(ticker: str) -> str:
    return f"https://kalshi.com/markets/{ticker}"


def _poly_url(slug: str) -> str:
    return f"https://polymarket.com/event/{slug}"


def _override_snippet(k_ticker: str, poly_cid: str) -> str:
    return json.dumps({
        "kalshi_market_ticker": k_ticker,
        "poly_condition_id": poly_cid,
        "reason": "<fill in>",
    })


def run_pipeline(
    cfg: PipelineConfig,
    ingestion: Any,
    embedder: Any,
    verifier: Any,
) -> dict:
    result: IngestionResult = ingestion.fetch_all()

    poly_vecs = np.zeros((len(result.poly), embedder.dim), dtype=np.float32)
    for i, m in enumerate(result.poly):
        poly_vecs[i] = embedder.embed(m)
    embedder.flush()

    retrieval = HnswRetrieval(dim=embedder.dim, top_k=cfg.top_k, min_cosine=cfg.min_cosine)
    if len(result.poly) > 0:
        retrieval.build(poly_vecs, [m.ticker for m in result.poly])
    poly_by_ticker: dict[str, Market] = {m.ticker: m for m in result.poly}

    overrides = OverrideSet.load(cfg.overrides_path)
    rows: list[PairAuditRow] = []
    accepted_pairs: list[dict] = []
    audit_log_lines: list[str] = []

    accepted = 0
    rejected = 0

    for k in result.kalshi:
        k_vec = embedder.embed(k)
        candidates = retrieval.query(k_vec) if len(result.poly) > 0 else []
        for poly_ticker, _cosine in candidates:
            p = poly_by_ticker.get(poly_ticker)
            if p is None:
                continue
            decision = verifier.verify(k, p)
            override = overrides.lookup(k.ticker, p.condition_id)
            ai_accept = decision.accepted
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

            rows.append(PairAuditRow(
                kalshi_ticker=k.ticker, kalshi_title=k.title,
                kalshi_description=k.description, kalshi_resolution=k.resolution_criteria,
                kalshi_outcomes=k.outcomes, kalshi_url=_kalshi_url(k.ticker),
                poly_slug=p.ticker, poly_title=p.title,
                poly_description=p.description, poly_resolution=p.resolution_criteria,
                poly_outcomes=p.outcomes, poly_url=_poly_url(p.ticker),
                decision=decision,
                accepted=final_accepted,
                override_snippet=_override_snippet(k.ticker, p.condition_id),
            ))

            audit_log_lines.append(json.dumps({
                "ts": dt.datetime.now(tz=dt.timezone.utc).isoformat(),
                "kalshi": k.ticker,
                "poly": p.condition_id,
                "decision": "accept" if final_accepted else "reject",
                "confidence": decision.confidence,
                "concerns": decision.concerns,
                "override": override.value,
                "reasoning": decision.reasoning,
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

    return {"accepted": accepted, "rejected": rejected, "rows": len(rows)}
```

- [ ] **Step 3: Wire `cli.py` `run` to call `pipeline.run_pipeline`**

Replace the `run` arm in `cli.py`:

```python
    if args.command == "run":
        from ai_matcher.pipeline import run_pipeline_default
        return run_pipeline_default(loop_mode=args.loop_mode,
                                    category=args.category, sample=args.sample)
```

Add to `pipeline.py` (below `run_pipeline`):

```python
def run_pipeline_default(loop_mode: bool = False, category: str | None = None,
                         sample: int | None = None) -> int:
    """Construct real components and run once (or loop). Used by the CLI."""
    project_root = Path(__file__).resolve().parents[3].parent  # scripts/ai_matcher/src/ai_matcher → repo root
    audit_dir = project_root / "audit"
    matches_path = project_root / ".ai_matches.json"
    audit_log_path = project_root / ".ai_matcher_audit.jsonl"
    overrides_path = project_root / "config" / "manual_overrides.json"

    import anthropic
    from ai_matcher.embedder import Embedder

    embedder = Embedder(cache_path=project_root / ".ai_matcher_cache.json")
    client = anthropic.Anthropic()
    verifier = Verifier(
        client=client,
        cache_path=project_root / ".ai_matcher_verifier_cache.json",
    )
    ingestion = Ingestion()

    cfg = PipelineConfig(
        project_root=project_root,
        audit_dir=audit_dir,
        matches_path=matches_path,
        audit_log_path=audit_log_path,
        overrides_path=overrides_path,
        embedding_model=embedder.model_name,
        llm_model=verifier.model,
    )
    summary = run_pipeline(cfg, ingestion=ingestion, embedder=embedder, verifier=verifier)
    print(f"[ai_matcher] run complete: {summary}")
    return 0
```

- [ ] **Step 4: Run tests + commit**

```bash
uv run pytest tests/test_pipeline.py -v
git add scripts/ai_matcher/src/ai_matcher/pipeline.py scripts/ai_matcher/src/ai_matcher/cli.py scripts/ai_matcher/tests/test_pipeline.py
git commit -m "Add run pipeline orchestrating ingestion through outputs with atomic writes"
```

---

## Task 11: `review` and `audit` commands

**Files:**
- Modify: `scripts/ai_matcher/src/ai_matcher/cli.py`
- Modify: `scripts/ai_matcher/src/ai_matcher/pipeline.py` (add `audit_sample` helper)

`review` opens `audit/report.html` in the default browser. `audit --sample N` re-renders a single-page HTML containing N random *accepted* pairs (biased toward low-confidence) and opens it.

- [ ] **Step 1: Implement helpers**

Add to `pipeline.py`:

```python
import random
import webbrowser


def review_default() -> int:
    project_root = Path(__file__).resolve().parents[3].parent
    report = project_root / "audit" / "report.html"
    if not report.exists():
        print(f"[ai_matcher] no report found at {report}; run `python -m ai_matcher run` first")
        return 1
    webbrowser.open(report.as_uri())
    return 0


def audit_sample_default(sample: int) -> int:
    project_root = Path(__file__).resolve().parents[3].parent
    matches_path = project_root / ".ai_matches.json"
    if not matches_path.exists():
        print(f"[ai_matcher] no .ai_matches.json — run `python -m ai_matcher run` first")
        return 1
    payload = json.loads(matches_path.read_text())
    pairs = payload.get("pairs", [])
    if not pairs:
        print("[ai_matcher] no accepted pairs to audit")
        return 0
    pairs.sort(key=lambda p: p.get("confidence", 1.0))
    chosen = pairs[:sample] if len(pairs) <= sample else random.sample(pairs[:max(sample * 2, sample)], sample)
    out = project_root / "audit" / f"audit-sample-{sample}.html"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(_render_audit_sample(chosen, payload))
    webbrowser.open(out.as_uri())
    return 0


def _render_audit_sample(pairs: list[dict], payload: dict) -> str:
    lines = ["<!DOCTYPE html><html><body style='font-family:sans-serif;max-width:1000px;margin:1em auto;'>"]
    lines.append(f"<h1>ai_matcher audit — {len(pairs)} samples (model {payload.get('model')})</h1>")
    for p in pairs:
        lines.append("<hr>")
        lines.append(f"<h2>{p.get('description','')}</h2>")
        lines.append(f"<p>Kalshi: <code>{p.get('kalshi_market_ticker')}</code></p>")
        lines.append(f"<p>Polymarket conditionId: <code>{p.get('poly_condition_id')}</code></p>")
        lines.append(f"<p>Category: {p.get('category')} — Event: {p.get('event_type')} — Confidence: {p.get('confidence')}</p>")
    lines.append("</body></html>")
    return "\n".join(lines)
```

- [ ] **Step 2: Wire CLI**

Replace the `review` and `audit` arms in `cli.py`:

```python
    if args.command == "review":
        from ai_matcher.pipeline import review_default
        return review_default()
    if args.command == "audit":
        from ai_matcher.pipeline import audit_sample_default
        return audit_sample_default(args.sample)
```

- [ ] **Step 3: Smoke test (no auto-open in tests)**

Run `uv run pytest -v` to verify nothing broke. (`review` and `audit` aren't unit-tested; they're CLI conveniences exercised in Task 14's smoke run.)

- [ ] **Step 4: Commit**

```bash
git add scripts/ai_matcher/src/ai_matcher/cli.py scripts/ai_matcher/src/ai_matcher/pipeline.py
git commit -m "Wire review and audit --sample CLI commands"
```

---

## Task 12: Rust `ai_reader` — load `.ai_matches.json` with freshness gate

**Files:**
- Create: `src/adapters/ai_reader.rs`
- Modify: `src/adapters/mod.rs`, `src/lib.rs`, `src/main.rs`, `src/config.rs`
- Test: inline `mod tests` in `src/adapters/ai_reader.rs`

`AiReader::load(path) → Result<Vec<MarketPair>>`. Validates `generated_at` is within `AI_MATCHES_MAX_AGE_SEC` (default 24h, configurable). Uses content from the JSON to populate `MarketPair` with `MatchSource::Ai { model, embedding_model, confidence }`.

- [ ] **Step 1: Add config helpers**

Append to `src/config.rs` (above `mod tests`):

```rust
/// Detection-only gate for AI-matched pairs (PR 3). Default OFF.
pub fn exec_allow_ai_matches() -> bool {
    std::env::var("EXEC_ALLOW_AI_MATCHES")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false)
}

/// Max acceptable age of `.ai_matches.json` in seconds. Default 24h.
pub fn ai_matches_max_age_secs() -> u64 {
    std::env::var("AI_MATCHES_MAX_AGE_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(24 * 60 * 60)
}
```

Add corresponding tests inside `mod tests`:

```rust
    #[test]
    fn exec_allow_ai_matches_defaults_false() {
        env::remove_var("EXEC_ALLOW_AI_MATCHES");
        assert!(!exec_allow_ai_matches());
    }

    #[test]
    fn ai_matches_max_age_secs_defaults_to_24h() {
        env::remove_var("AI_MATCHES_MAX_AGE_SEC");
        assert_eq!(ai_matches_max_age_secs(), 86_400);
    }
```

- [ ] **Step 2: Failing test + implementation**

Append `pub mod ai_reader;` to `src/adapters/mod.rs`. Add `pub mod ai_reader;` to `src/lib.rs` if `adapters` re-exports it (it does — only `mod.rs` change needed).

Create `src/adapters/ai_reader.rs`:

```rust
//! Reads `.ai_matches.json` produced by the standalone Python sidecar and
//! emits `MarketPair` rows for AI-matched pairs.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.6.4 + §4.8.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;

use crate::fees::{MatchSource, PolyCategory};
use crate::types::{MarketPair, MarketType};

const DEFAULT_MATCHES_PATH: &str = ".ai_matches.json";

#[derive(Debug, Deserialize)]
struct AiMatchesFile {
    generated_at: DateTime<Utc>,
    model: String,
    embedding_model: String,
    pairs: Vec<AiMatch>,
}

#[derive(Debug, Deserialize)]
struct AiMatch {
    kalshi_market_ticker: String,
    poly_condition_id: String,
    poly_yes_token: String,
    poly_no_token: String,
    category: String,
    #[allow(dead_code)]
    event_type: String,
    confidence: f64,
    description: String,
}

/// Load and validate the AI matches file. Returns `Ok(vec![])` if the file
/// is missing — that's a normal "sidecar hasn't run yet" state, not an error.
/// Returns `Err` if the file exists but is older than `max_age_secs` or malformed.
pub fn load_ai_matches(
    path: Option<&Path>,
    max_age_secs: u64,
    now: DateTime<Utc>,
) -> Result<Vec<MarketPair>> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| Path::new(DEFAULT_MATCHES_PATH).to_path_buf());

    if !path.exists() {
        tracing::info!("[AI] no {} found; sidecar has not run yet", path.display());
        return Ok(vec![]);
    }

    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: AiMatchesFile = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let age_secs = now.signed_duration_since(parsed.generated_at).num_seconds();
    if age_secs < 0 {
        return Err(anyhow!(
            "{} generated_at is in the future ({})",
            path.display(),
            parsed.generated_at
        ));
    }
    if (age_secs as u64) > max_age_secs {
        return Err(anyhow!(
            "{} is stale: {}s old (max {}s)",
            path.display(),
            age_secs,
            max_age_secs
        ));
    }

    let mut out = Vec::with_capacity(parsed.pairs.len());
    for p in parsed.pairs {
        let category = parse_category(&p.category);
        out.push(MarketPair {
            pair_id: Arc::from(format!("{}-{}", p.kalshi_market_ticker, p.poly_condition_id)),
            league: Arc::from("ai"),
            market_type: MarketType::Moneyline,
            description: Arc::from(p.description),
            kalshi_event_ticker: Arc::from(""),
            kalshi_market_ticker: Arc::from(p.kalshi_market_ticker),
            poly_slug: Arc::from(""),
            poly_yes_token: Arc::from(p.poly_yes_token),
            poly_no_token: Arc::from(p.poly_no_token),
            poly_condition_id: Arc::from(p.poly_condition_id),
            line_value: None,
            team_suffix: None,
            category,
            match_source: MatchSource::Ai {
                model: parsed.model.clone(),
                embedding_model: parsed.embedding_model.clone(),
                confidence: p.confidence,
            },
        });
    }
    Ok(out)
}

fn parse_category(s: &str) -> PolyCategory {
    match s {
        "Crypto" => PolyCategory::Crypto,
        "Mentions" => PolyCategory::Mentions,
        "Economics" => PolyCategory::Economics,
        "Culture" => PolyCategory::Culture,
        "Weather" => PolyCategory::Weather,
        "Finance" => PolyCategory::Finance,
        "Politics" => PolyCategory::Politics,
        "Tech" => PolyCategory::Tech,
        "Sports" => PolyCategory::Sports,
        "Geopolitical" => PolyCategory::Geopolitical,
        _ => PolyCategory::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_fixture(body: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f
    }

    fn fresh_payload(generated_at: &str) -> String {
        format!(r#"{{
            "generated_at": "{generated_at}",
            "model": "claude-opus-4-7",
            "embedding_model": "sentence-transformers/all-MiniLM-L6-v2",
            "version": 1,
            "pairs": [
                {{
                    "kalshi_market_ticker": "KXPRES-USA-2028-DEM",
                    "poly_condition_id": "0xCONDA",
                    "poly_yes_token": "0xYES",
                    "poly_no_token": "0xNO",
                    "category": "Politics",
                    "event_type": "Election",
                    "confidence": 0.95,
                    "description": "2028 US presidential — Democratic candidate"
                }}
            ]
        }}"#)
    }

    #[test]
    fn loads_one_pair_when_file_is_fresh() {
        let now = Utc::now();
        let f = write_fixture(&fresh_payload(&now.to_rfc3339()));
        let pairs = load_ai_matches(Some(f.path()), 86_400, now).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].kalshi_market_ticker.as_ref(), "KXPRES-USA-2028-DEM");
        assert_eq!(pairs[0].category, PolyCategory::Politics);
        match &pairs[0].match_source {
            MatchSource::Ai { confidence, model, .. } => {
                assert!((*confidence - 0.95).abs() < 1e-6);
                assert_eq!(model, "claude-opus-4-7");
            }
            _ => panic!("expected MatchSource::Ai"),
        }
    }

    #[test]
    fn rejects_stale_file_beyond_max_age() {
        let now = Utc::now();
        let stale = now - Duration::seconds(86_500);
        let f = write_fixture(&fresh_payload(&stale.to_rfc3339()));
        let err = load_ai_matches(Some(f.path()), 86_400, now).unwrap_err();
        assert!(err.to_string().contains("stale"));
    }

    #[test]
    fn rejects_future_dated_file() {
        let now = Utc::now();
        let future = now + Duration::seconds(60);
        let f = write_fixture(&fresh_payload(&future.to_rfc3339()));
        let err = load_ai_matches(Some(f.path()), 86_400, now).unwrap_err();
        assert!(err.to_string().contains("future"));
    }

    #[test]
    fn missing_file_returns_empty_vec_not_error() {
        let pairs = load_ai_matches(
            Some(Path::new("/nonexistent/.ai_matches.json")),
            86_400,
            Utc::now(),
        ).unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn unknown_category_falls_back_to_unknown() {
        let now = Utc::now();
        let body = format!(r#"{{
            "generated_at": "{}",
            "model": "x",
            "embedding_model": "y",
            "version": 1,
            "pairs": [{{
                "kalshi_market_ticker": "K",
                "poly_condition_id": "0xC",
                "poly_yes_token": "y",
                "poly_no_token": "n",
                "category": "Astronomy",
                "event_type": "Other",
                "confidence": 0.91,
                "description": "x"
            }}]
        }}"#, now.to_rfc3339());
        let f = write_fixture(&body);
        let pairs = load_ai_matches(Some(f.path()), 86_400, now).unwrap();
        assert_eq!(pairs[0].category, PolyCategory::Unknown);
    }
}
```

- [ ] **Step 3: Add `tempfile` dev-dep + run**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher && cargo add --dev tempfile && cargo test --lib adapters::ai_reader::
```

Expected: 5 passing.

- [ ] **Step 4: Commit**

```bash
git add src/adapters/ai_reader.rs src/adapters/mod.rs src/config.rs Cargo.toml Cargo.lock
git commit -m "Add Rust ai_reader for .ai_matches.json with freshness gate"
```

---

## Task 13: Wire ai_reader into DiscoveryClient + execution gate

**Files:**
- Modify: `src/discovery.rs`, `src/main.rs`, `src/execution.rs`

After running structured adapters, load AI matches and merge them, deduping on `(kalshi_market_ticker, poly_condition_id)` with **structured wins on collision**. Extend the execution gate to also block `MatchSource::Ai`.

- [ ] **Step 1: Extend `should_block_for_detection_only` to also gate AI**

Edit `src/execution.rs`. Replace the function body:

```rust
pub(crate) fn should_block_for_detection_only(pair: &MarketPair) -> bool {
    use crate::fees::MatchSource;
    match &pair.match_source {
        MatchSource::Structured { adapter } => {
            if adapter == "fomc" && !crate::config::exec_allow_fomc() {
                return true;
            }
            false
        }
        MatchSource::Ai { .. } => !crate::config::exec_allow_ai_matches(),
        MatchSource::ManualOverride => false,
    }
}
```

Add a test in the existing `mod gate_tests`:

```rust
    #[test]
    fn ai_pair_is_blocked_by_default() {
        std::env::remove_var("EXEC_ALLOW_AI_MATCHES");
        let mut pair = mk_pair("ignored");
        pair.match_source = MatchSource::Ai {
            model: "x".into(),
            embedding_model: "y".into(),
            confidence: 0.95,
        };
        assert!(should_block_for_detection_only(&pair));
    }

    #[test]
    fn ai_pair_passes_when_gate_enabled() {
        std::env::set_var("EXEC_ALLOW_AI_MATCHES", "1");
        let mut pair = mk_pair("ignored");
        pair.match_source = MatchSource::Ai {
            model: "x".into(),
            embedding_model: "y".into(),
            confidence: 0.95,
        };
        assert!(!should_block_for_detection_only(&pair));
        std::env::remove_var("EXEC_ALLOW_AI_MATCHES");
    }
```

- [ ] **Step 2: Modify `DiscoveryClient::discover_full` to merge AI pairs**

Edit `src/discovery.rs`. Add at the top:

```rust
use crate::adapters::ai_reader;
```

After the structured-adapter loop in `discover_full` and before `result.kalshi_events_found = ...`, insert:

```rust
        // Merge AI-matched pairs from the standalone sidecar.
        let max_age = crate::config::ai_matches_max_age_secs();
        match ai_reader::load_ai_matches(None, max_age, chrono::Utc::now()) {
            Ok(ai_pairs) => {
                let structured_keys: rustc_hash::FxHashSet<(String, String)> = result.pairs.iter()
                    .map(|p| (p.kalshi_market_ticker.to_string(), p.poly_condition_id.to_string()))
                    .collect();
                let mut added = 0usize;
                let mut collisions = 0usize;
                for ai in ai_pairs {
                    let key = (ai.kalshi_market_ticker.to_string(), ai.poly_condition_id.to_string());
                    if structured_keys.contains(&key) {
                        collisions += 1;
                        tracing::info!(
                            "[AI] collision with structured pair, AI dropped: {}",
                            ai.pair_id
                        );
                        continue;
                    }
                    result.pairs.push(ai);
                    added += 1;
                }
                if added > 0 || collisions > 0 {
                    info!("[AI] merged {} pairs ({} collisions skipped)", added, collisions);
                }
            }
            Err(e) => warn!("[AI] failed to load .ai_matches.json: {}", e),
        }
```

Ensure `rustc_hash` is in `Cargo.toml` (it already is from PR1). If not in scope, add `use rustc_hash::FxHashSet;`.

- [ ] **Step 3: Run all tests**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher && cargo test --lib -- --test-threads=1
```

Expected: previous 118 + 6 new (5 ai_reader + 1 config + 2 gate) = 127 tests, all green.

- [ ] **Step 4: Build the bin too**

```bash
cargo build
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/discovery.rs src/execution.rs
git commit -m "Merge AI matches into DiscoveryClient and gate AI execution behind EXEC_ALLOW_AI_MATCHES"
```

---

## Task 14: Acceptance smoke run + spec close-out

**Files:**
- Create: `docs/notes/2026-04-21-ai-matcher-first-run.md`
- Modify: `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` (close §8 questions resolved here)

This task is human-driven verification. Acceptance per spec §7 PR 3:
- Sidecar runnable end-to-end without the Rust bot.
- `audit/report.html` with at least 10 accepted and 10 rejected pairs.
- At least one high-value non-sports pair flows through Rust detection (logged, not executed).
- `audit --sample 20` opens in the browser.

- [ ] **Step 1: Live sidecar smoke run**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/scripts/ai_matcher
export ANTHROPIC_API_KEY=...                  # required
uv run python -m ai_matcher run 2>&1 | tee /tmp/ai-matcher-smoke.log
```

The first run will:
- Download the sentence-transformers model (~80 MB, one-time).
- Fetch all open Kalshi + Polymarket markets (a few thousand each).
- Embed every market locally (CPU; minutes for thousands).
- Retrieve top-K poly candidates per Kalshi market.
- Call Claude on each candidate pair (this is the cost driver — expect ~$1-5 for the first uncached run).
- Write `.ai_matches.json`, `audit/report.html`, append to `.ai_matcher_audit.jsonl`.

- [ ] **Step 2: Inspect outputs**

```bash
ls -la /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/.ai_matches.json
ls -la /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/audit/
wc -l /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/.ai_matcher_audit.jsonl
```

Open `audit/report.html` in a browser — confirm at least 10 accepted + 10 rejected rows.

- [ ] **Step 3: Verify Rust freshness gate sees the file**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher
EXEC_ALLOW_AI_MATCHES=0 FORCE_DISCOVERY=1 cargo run --release 2>&1 | grep -E "AI|ai_match" | tee /tmp/ai-rust-smoke.log
```

Look for:
- `[AI] merged N pairs (M collisions skipped)` — N should be ≥ 1.
- For at least one AI pair where the bot detects an arb: `[EXEC] 🛑 detection-only: dropping pair ... (adapter=Ai{...})`.

- [ ] **Step 4: Run audit subcommand**

```bash
cd /Users/bity/personal/Polymarket-Kalshi-Arbitrage-bot-rust/.worktrees/pr3-ai-matcher/scripts/ai_matcher
uv run python -m ai_matcher audit --sample 20
```

Confirm browser opens `audit-sample-20.html` with 20 random accepted pairs.

- [ ] **Step 5: Record findings**

Create `docs/notes/2026-04-21-ai-matcher-first-run.md`:

```markdown
# ai_matcher first run (PR 3 acceptance)

**Date run:** YYYY-MM-DD
**Models:** embedding=sentence-transformers/all-MiniLM-L6-v2, llm=claude-opus-4-7

## Counts
- Kalshi markets ingested: K
- Polymarket markets ingested: P
- Pairs sent to LLM verifier: V
- Accepted by LLM (≥0.9 conf, no concerns): A
- Rejected: R
- Cache hits on second run: C / V

## Cost
- Anthropic spend, first run: $X.XX
- Anthropic spend, second run (cache test): $0.0X

## High-value non-sports examples discovered
- (List 1-3 with kalshi_market_ticker, poly_condition_id, category, brief reasoning)

## Rust freshness gate evidence
- `[AI] merged N pairs (M collisions skipped)` — verbatim from log
- One example arb signal that hit the detection-only gate

## Issues encountered
- (List)

## Next steps
- (Tune MIN_COSINE if too many / few candidates reach LLM)
- (Decide first category to flip EXEC_ALLOW_AI_MATCHES=1 for)
```

- [ ] **Step 6: Update spec § resolved questions**

In the spec's §8 list, mark question 4 (long-tail category whitelist) and question 7 (cost ceiling) with the empirical findings if observed. Otherwise leave them open.

- [ ] **Step 7: Commit**

```bash
git add docs/notes/2026-04-21-ai-matcher-first-run.md docs/superpowers/specs/2026-04-21-multi-category-matching-design.md
git commit -m "Record PR 3 acceptance smoke run findings"
```

---

## Self-Review

**Spec coverage check** (against §4.6 + §7 PR 3):
- ✅ §4.6 standalone runnable Python sidecar — Tasks 1-11
- ✅ §4.6.1 ingestion facade — Task 3 (httpx direct, deviation noted)
- ✅ §4.6.2 embeddings + content-hash cache — Task 4 (local sentence-transformers, deviation noted)
- ✅ §4.6.3 LLM verification + cache — Task 6
- ✅ §4.6.4 `.ai_matches.json` + `audit/report.html` + `.ai_matcher_audit.jsonl` — Tasks 8, 10
- ✅ Manual overrides apply blacklist > whitelist — Task 7
- ✅ §4.7 scheduler — Task 9 (loop integration deferred to a future cron/systemd setup)
- ✅ §4.8 freshness gate (Rust) — Task 12
- ✅ §4.8 EXEC_ALLOW_AI_MATCHES=0 default — Task 13
- ✅ §4.8 structured wins on collision — Task 13 dedupe logic
- ✅ §7 acceptance — Task 14

**Type consistency check:**
- `Decision` (Python) — declared Task 6, used Tasks 8 and 10.
- `Market` / `IngestionResult` — declared Task 3, used Tasks 4, 5, 10.
- `PairAuditRow` — declared Task 8, used Task 10.
- `OverrideOutcome` / `OverrideSet` — declared Task 7, used Task 10.
- `PipelineConfig` / `run_pipeline` — declared Task 10, used Task 11.
- `load_ai_matches` (Rust) — declared Task 12, used Task 13.

**Placeholder scan:** Each step has explicit code or shell commands. Task 14 step 5 has a fill-in-the-blank doc — that's intentional; it's a runtime template the executor fills with smoke-run results.

**Risk acknowledgments:**
- Task 14 requires real `ANTHROPIC_API_KEY` and live network. If we're rate-limited or the key is missing, the executor should report that and defer Task 14 — Tasks 1-13 are independently complete and shippable.
- pmxt deviation in Task 3 means we hand-rolled the REST surface. If Kalshi or Polymarket changes their API shape, the parsers in `ingestion.py` need updating.

---

## Notes for the executor

- **`uv` working directory.** Most Python commands need to run from `scripts/ai_matcher/`. Either prefix with `cd /abs/path/scripts/ai_matcher && uv run ...` in a single Bash call, or `cd` then chain. Don't trust persistence between Bash calls.
- **`--test-threads=1` for env-mutating Rust tests.** Tasks 12 and 13 add tests in `config::tests` and `execution::gate_tests`. Both already require single-threaded running (set up in PR 2).
- **Skip Task 14 if no Anthropic key is available.** Don't fabricate smoke-run results. Mark Task 14 deferred and ship the rest.
- **No silent placeholders in `.ai_matches.json`.** If the sidecar produces zero accepted pairs, the file should still be written with an empty `pairs: []` and a fresh timestamp; the Rust loader handles that case.

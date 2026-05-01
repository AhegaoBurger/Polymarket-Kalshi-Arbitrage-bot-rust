# ai_matcher — AI Matcher Sidecar

Standalone Python sidecar that pairs Kalshi and Polymarket markets via local
sentence-transformers embeddings + Claude LLM verification. Runs without the
Rust bot. Spec: `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` §4.6.

## Quickstart

```bash
cd scripts/ai_matcher
uv sync                                          # install deps into .venv
uv run python -m ai_matcher run                  # full pipeline (Claude verification)
uv run python -m ai_matcher run --no-llm         # cheap mode: cosine similarity only
uv run python -m ai_matcher run --loop           # loop with per-category TTLs
uv run python -m ai_matcher review               # open audit/report.html
uv run python -m ai_matcher audit --sample 20
```

## Required env

- `ANTHROPIC_API_KEY` — for the LLM verification stage (skip with `--no-llm`)
- (No Kalshi credentials required — sidecar uses public browse endpoints.
  Trade-off: liquidity/volume aren't returned without auth, so the Kalshi
  liquidity floor only fires when the field is populated; unknown values pass
  through. The Polymarket-side floor + cosine retrieval still bound the join.)
- (No OpenAI key required — embeddings run locally on CPU.)

## Tuning

| Env var | Default | What it does |
|---|---|---|
| `MIN_LIQUIDITY_USD` | `100.0` | Drops Polymarket markets below this. On Kalshi: drops only when liquidity is known and below; unknown values pass. |
| `INGEST_KALSHI_MAX_EVENTS` | `200` | Cap on Kalshi events walked per run. Each event = one extra HTTP call. |
| `INGEST_POLY_LIMIT` | `500` | Polymarket markets to fetch (already sorted by liquidity descending). |
| `EMBEDDINGS_ACCEPT_COSINE` | `0.85` | Cosine threshold for `--no-llm` acceptance. |
| `EMBEDDINGS_ONLY` | unset | `1` defaults to `--no-llm` without the flag. |

## Modes

The pipeline has two acceptance strategies:

**Default (LLM verification):** embeddings retrieve top-K candidates, then Claude
scores each pair on resolution-criteria identity. Catches edge cases like different
resolution dates or different data sources. Requires `ANTHROPIC_API_KEY`. Costs
~$1-5 on the first uncached run; subsequent runs hit the verifier cache.

**Embeddings-only (`--no-llm` or `EMBEDDINGS_ONLY=1`):** skip the LLM. Accept any
candidate whose cosine similarity clears `EMBEDDINGS_ACCEPT_COSINE` (default 0.85).
Free, fast, but **much weaker**: embeddings cluster by topical similarity, not by
resolution-criteria identity. Two BTC markets resolving on different dates will
embed nearly identically and both get accepted. Use for cheap dry runs and to
exercise the pipeline without a Claude bill — not as a production matcher.

## Outputs

| Path | Format | Audience |
|---|---|---|
| `.ai_matches.json` | JSON | Rust `ai_reader` |
| `audit/report.html` | static HTML | human review |
| `.ai_matcher_audit.jsonl` | JSONL | append-only audit trail |

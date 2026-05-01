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

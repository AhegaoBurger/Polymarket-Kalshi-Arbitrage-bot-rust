# Multi-Category Market Matching & Per-Category Fees

**Status:** Draft
**Date:** 2026-04-21
**Scope:** Extend the arbitrage bot from sports-only to multi-category (FOMC, macro econ, elections, and a long-tail fallback) with per-category Polymarket fee handling.

---

## 1. Background

Today the bot is hard-wired to sports in two places:

- **Discovery** (`src/discovery.rs`, `src/config.rs`): league configs drive a ticker parser that decomposes Kalshi events into `(date, team1, team2)` and builds Polymarket slugs of the form `prefix-team1-team2-date[-suffix]`. This shape is specific to sports and does not generalize to FOMC, elections, CPI, etc.
- **Fees** (`src/main.rs:192-201`): a single `SPORTS_FEE_RATE_PPM` is stamped onto every tracked market. Post-March 2026, Polymarket charges different taker fees by category (Crypto 1.80%, Economics 1.50%, Politics 1.00%, Sports 0.75%, Geopolitical 0%). Using the sports rate on, e.g., an FOMC market *underestimates fees by 2×* and can turn a detected arb into a loss.

The motivation for this work is that high-discrepancy categories — FOMC rate decisions, macro econ releases, elections — systematically show larger cross-platform spreads than sports because the participant bases diverge (CFTC-regulated US retail on Kalshi vs. crypto-native global on Polymarket). The spread opportunity is exactly where the fee differential matters most.

Note: `src/types.rs:11-16` comments `SPORTS_FEE_RATE_PPM = 30_000` as "3.00%", but the `poly_fee_cents` formula yields a peak of `rate_ppm / 40_000` cents per dollar, i.e. 0.75% at 30,000 ppm — which matches current Polymarket Sports. The value is correct; the comment is wrong and will be fixed as part of this work.

---

## 2. Goals / Non-Goals

**Goals**

1. Per-category Polymarket fee handling, driven by a category→ppm table with the CLOB `get_market_meta` fetch as authoritative override.
2. A structured `FomcMatcher` that pairs Kalshi `KXFED*` rate-band markets with the corresponding Polymarket neg-risk outcomes, using a current-rate anchor.
3. An AI-backed long-tail matcher (Python sidecar) using a two-layer embeddings + LLM pipeline. Runs as a scheduled job with category-specific TTLs.
4. Zero regression in the existing sports pipeline.
5. Discovery output remains a single `Vec<MarketPair>` consumable by `GlobalState` without changes to trading-hot-path code.

**Non-Goals**

- Replacing the existing sports discovery / `SportsMatcher`. It works and is deterministic — keep it.
- Supporting Polymarket's fee-free Geopolitical category (possible follow-up; adds no fee-math value).
- Live-updating existing `MarketPair` rows mid-session (matches are loaded at startup and on re-discovery, same as today).
- Fixing `tests/integration_tests.rs` (already broken per project memory; separate concern).
- Building embedded vector-search infra in Rust — Python sidecar owns that.

---

## 3. Design Overview

```
┌────────────────────────────────────────────────────────────────────────────┐
│                             Startup (Rust)                                  │
│                                                                             │
│  DiscoveryClient ──▶ merges:                                                │
│    1. SportsMatcher          (existing, unchanged)                          │
│    2. FomcMatcher            (new, rate-band join)                          │
│    3. AiMatcherReader        (new, reads JSON produced by Python sidecar)   │
│                                                                             │
│  Result: Vec<MarketPair> → GlobalState                                      │
│                                                                             │
│  FeeResolver.apply_to_markets(state):                                       │
│    for each market: fee_ppm = seed_from_clob_meta() ?? category_table()     │
└────────────────────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────────────────────┐
│                    scripts/ai_matcher.py (periodic sidecar)                 │
│                                                                             │
│  Every N minutes (category-dependent):                                      │
│    1. Fetch Kalshi + Polymarket catalogs (REST)                             │
│    2. Layer 1 — Embeddings:                                                 │
│         - content-hash each market; skip re-embed if unchanged              │
│         - cosine top-K=8 Poly candidates per Kalshi market                  │
│    3. Layer 2 — LLM verification:                                           │
│         - structured output {confidence, resolution_match, concerns[]}      │
│         - cache by (kalshi_hash, poly_hash); skip if both unchanged         │
│         - accept iff confidence ≥ 0.9 AND concerns is empty                 │
│    4. Apply manual_overrides.json (human-reviewed whitelist/blacklist)      │
│    5. Write .ai_matches.json for Rust to pick up                            │
└────────────────────────────────────────────────────────────────────────────┘
```

Structured matchers run first and their pairs win on ticker collision (AI matcher cannot override a structured match).

---

## 4. Detailed Design

### 4.1 Per-Category Fee Table + Per-Market Override

New module `src/fees.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PolyCategory {
    Sports,
    Crypto,
    Economics,   // FOMC, CPI, NFP, GDP
    Politics,
    Tech,
    Culture,
    Weather,
    Finance,
    Mentions,
    Geopolitical, // fee-free
    Unknown,      // AI-matched, category not confidently known
}

pub fn category_fee_ppm(c: PolyCategory) -> u32 {
    // Values encode peak fee via rate_ppm / 40_000 = peak_pct.
    // 0.75% peak → 30_000 ppm.
    match c {
        PolyCategory::Crypto       => 72_000, // 1.80%
        PolyCategory::Mentions     => 62_400, // 1.56%
        PolyCategory::Economics    => 60_000, // 1.50%
        PolyCategory::Culture      => 50_000, // 1.25%
        PolyCategory::Weather      => 50_000, // 1.25%
        PolyCategory::Finance      => 40_000, // 1.00%
        PolyCategory::Politics     => 40_000, // 1.00%
        PolyCategory::Tech         => 40_000, // 1.00%
        PolyCategory::Sports       => 30_000, // 0.75%
        PolyCategory::Geopolitical => 0,
        PolyCategory::Unknown      => 72_000, // conservative: use the highest
    }
}
```

`SPORTS_FEE_RATE_PPM` becomes `pub const SPORTS_FEE_RATE_PPM: u32 = category_fee_ppm(PolyCategory::Sports);` (or deleted, with callers switched to `category_fee_ppm`). The misleading "3.00%" comment in `types.rs:11-16` is corrected to "0.75% peak" in the same commit.

**Seeding per-market fees at startup** (replaces `main.rs:192-201`):

```rust
for i in 0..state.market_count() {
    let pair = &state.markets[i].pair.as_ref().unwrap();
    // 1. Prefer authoritative CLOB value (same call execution path uses).
    //    If it fails, fall back to category table.
    let ppm = match poly_async.get_market_meta(&pair.poly_yes_token, &pair.poly_condition_id).await {
        Ok((_, fee_bps)) => bps_to_ppm(fee_bps),
        Err(_) => category_fee_ppm(pair.category),
    };
    state.markets[i].set_poly_fee_rate_ppm(ppm);
}
```

This makes the category table a **fallback**, with the CLOB as source of truth. Cost: one REST call per market at startup (parallelizable via the existing `SharedAsyncClient.meta_cache`). Benefits:
- No drift when Polymarket changes per-category rates.
- Consistent with what the signing path already uses (`polymarket_clob.rs:795-802`).

`bps_to_ppm` is a small helper whose exact conversion must be calibrated during implementation, not asserted in this spec. The bot's internal `rate_ppm` encodes a pre-`p(1-p)` rate such that `peak_cents_per_dollar = rate_ppm / 40_000` (see `poly_fee_cents` in `types.rs:243`). The CLOB's published `taker_fee_rate_bps` may be either the peak percent-bps or the pre-scale rate — Polymarket has used both conventions historically. Implementation step: fetch `get_market_meta` for a known-rate market (e.g. a Sports market at 0.75%) and back-solve the conversion factor from observed `fee_bps`. Encode the result as a single constant in `fees.rs` with a unit test that asserts `bps_to_ppm(bps_for_sports) == 30_000`.

### 4.2 `MarketPair` Additions

```rust
pub struct MarketPair {
    // ... existing fields unchanged ...
    pub category: PolyCategory,         // NEW — drives fee table fallback
    pub match_source: MatchSource,      // NEW — who matched this pair
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MatchSource {
    Structured { matcher: &'static str },     // "sports", "fomc"
    Ai { confidence: f32, model: Arc<str> },  // "claude-opus-4-7" etc.
    ManualOverride,
}
```

Both fields are `#[serde(default)]`-friendly so existing `.discovery_cache.json` files continue to deserialize (defaulting to `PolyCategory::Sports` + `MatchSource::Structured { matcher: "sports" }`), avoiding a forced cache wipe.

Execution filtering (shipped with PR 3, see §7):
- A boolean env `EXEC_ALLOW_AI_MATCHES` (default `false`) gates whether AI-matched pairs are eligible for live trades. Detector still runs on them and logs opportunities; the execution engine checks `pair.match_source` against the gate and rejects AI pairs until the flag flips.
- This is the only place in the hot path that reads `match_source`; one branch in `execution.rs` at the start of order dispatch.

### 4.3 `SportsMatcher` (unchanged)

The current `discover_league` / `parse_kalshi_event_ticker` / `build_poly_slug` pipeline is repackaged without logic change into `src/matchers/sports.rs` behind a trait:

```rust
#[async_trait]
pub trait MarketMatcher: Send + Sync {
    fn name(&self) -> &'static str;
    async fn discover(&self) -> Result<Vec<MarketPair>>;
}
```

`DiscoveryClient` owns a `Vec<Arc<dyn MarketMatcher>>` and runs them in parallel, then merges results. Sports keeps its cache file; each matcher namespaces its cache (e.g. `.discovery_cache_sports.json`, `.discovery_cache_fomc.json`, `.ai_matches.json`).

### 4.4 `FomcMatcher`

**Input config** (new `src/config.rs` entry):
```rust
pub struct FomcSource {
    pub kalshi_series: &'static str,     // "KXFED"
    pub poly_event_search: &'static str, // Gamma search hint, e.g. "fomc-decision"
    pub anchor_source: FedFundsAnchor,   // see below
}
```

**Anchor** — the current fed-funds target mid-rate is needed to translate Kalshi's absolute bands to Polymarket's delta outcomes. Two resolution strategies, tried in order:

1. **From Kalshi event metadata** if the `KXFED` event exposes `current_rate` or similar. (To verify during implementation; if not, fall through.)
2. **From FRED API** (`https://api.stlouisfed.org/fred/series/observations?series_id=DFEDTARU`). Requires a `FRED_API_KEY` env var; documented as optional-but-recommended for FOMC support.

If both fail, the matcher logs an error and emits zero pairs rather than guessing (a wrong anchor silently mis-aligns every pair).

**Algorithm** (pseudocode):
```
for each open Kalshi event in series KXFED:
    anchor_bps = resolve_anchor(event)
    kalshi_markets = kalshi.get_markets(event.ticker)    // each has floor_strike (%)
    poly_event     = gamma.find_fomc_event(event.meeting_date)
    if poly_event is None: skip

    // Build lookup: canonical rate-band → Polymarket outcome token pair
    poly_by_band = {}
    for outcome in poly_event.outcomes:
        delta_bps = parse_fed_delta(outcome.title)  // "25 bps decrease" → -25
        if delta_bps is None: continue
        band_bps = anchor_bps + delta_bps
        poly_by_band[band_bps] = outcome

    for k_market in kalshi_markets:
        band_bps = (k_market.floor_strike * 100.0) as i32  // 4.25% → 425 bps
        if let Some(outcome) = poly_by_band.get(&band_bps):
            emit MarketPair {
                category: PolyCategory::Economics,
                match_source: MatchSource::Structured { matcher: "fomc" },
                ...
            }
```

Unmatched markets are skipped (not errors). Tail bands with no Poly counterpart (e.g. "above 6.00%") simply produce no pair.

### 4.5 AI Matcher Sidecar

**Location**: `scripts/ai_matcher.py` — consistent with the existing Python-produced `.clob_market_cache.json` pattern (`src/main.rs:104`).

**Inputs**:
- Kalshi REST: list open markets for non-sports, non-FOMC series/categories.
- Polymarket Gamma: list open markets with `active=true`.
- `.ai_matcher_cache.json` — persistent cache of `{(kalshi_hash, poly_hash): {decision, timestamp, model}}`.
- `config/manual_overrides.json` — human-curated whitelist (force match) and blacklist (force reject).

**Layer 1 — Embeddings**
- Model: OpenAI `text-embedding-3-small` (cheap, 1536-dim, well-benchmarked). Swappable via env `EMBEDDING_MODEL`.
- Embedded text: `title + \n + description + \n + resolution_criteria + \n + outcomes_joined`.
- Content-hash (SHA-256 over the input text) keyed in cache; unchanged markets skip re-embed.
- Index: `hnswlib` in-memory, rebuilt per run (fast enough for tens of thousands of markets; switch to on-disk `lancedb` if we exceed ~100k).
- Retrieval: cosine top-K=8 Poly candidates per Kalshi market; configurable threshold `MIN_COSINE=0.55` filters obvious non-matches before reaching the LLM.

**Layer 2 — LLM verification**
- Model: Claude `claude-opus-4-7` (or `claude-sonnet-4-6` for cost), structured JSON output.
- Prompt template (Appendix A) scores **resolution-criteria identity**, not topical similarity — the subtle-definition trap (e.g., "any BTC held" vs. "National BTC Reserve") is called out explicitly in the system prompt.
- Response schema:
  ```json
  {
    "confidence": 0.0,
    "resolution_match": true,
    "concerns": ["string"],
    "reasoning": "string",
    "category": "Politics"
  }
  ```
- Accept iff `confidence ≥ 0.9 AND resolution_match AND len(concerns) == 0`.
- Cache key: `(kalshi_hash, poly_hash)`. If both sides' hashes are unchanged since the last accepted decision, skip the LLM call.

**Output** — `.ai_matches.json`:
```json
{
  "generated_at": "2026-04-21T14:00:00Z",
  "model": "claude-opus-4-7",
  "embedding_model": "text-embedding-3-small",
  "version": 1,
  "pairs": [
    {
      "kalshi_event_ticker": "KXCPIYOY-26APR",
      "kalshi_market_ticker": "KXCPIYOY-26APR-B3.0",
      "poly_slug": "cpi-yoy-april-2026-above-3pct",
      "poly_yes_token": "0x...",
      "poly_no_token": "0x...",
      "poly_condition_id": "0x...",
      "category": "Economics",
      "description": "CPI YoY April 2026 > 3.0%",
      "confidence": 0.97
    }
  ]
}
```

The Rust-side reader (`src/matchers/ai.rs`) is deliberately thin: load JSON, filter by freshness (reject if `generated_at` older than configured TTL), emit `MarketPair { match_source: Ai { .. }, .. }`.

### 4.6 Scheduling

Python sidecar is invoked by:
- **Option A (chosen)**: `tokio::spawn` at Rust startup, runs the script as a subprocess on an interval. One Rust process, one cron-like loop. Logged through the same `tracing` infra.
- Option B (considered, rejected): external cron / systemd timer. Adds deploy complexity; harder to reason about in dev.

Per-category TTL passed as script argument:

| Category          | Interval  |
|-------------------|-----------|
| Macro econ (CPI, NFP, GDP) | 12h |
| Elections / politics       | 2h  |
| Crypto hourly              | 15m |
| Sports                     | n/a (structured matcher) |
| FOMC                       | n/a (structured matcher) |

The sidecar reads which categories to process from `config/ai_categories.json`.

### 4.7 Safety Gates

1. **Confidence floor**: AI pairs below 0.9 never reach `GlobalState`.
2. **`concerns[]` is a hard reject**: any non-empty concerns → no pair.
3. **Manual override file**: `config/manual_overrides.json` with `whitelist` (force match) and `blacklist` (force reject) keyed by `(kalshi_market_ticker, poly_condition_id)`. Blacklist wins over whitelist wins over AI.
4. **Execution gate**: AI-matched pairs are detected but not traded until `EXEC_ALLOW_AI_MATCHES=1`. First week after launch: keep off, eyeball the detection logs, build confidence, then flip.
5. **Audit log**: every AI decision (accepted *and* rejected) is appended to `.ai_matcher_audit.jsonl` with full reasoning — enables retroactive review and prompt iteration.
6. **Model version pinning**: embedding + LLM model names are part of the cache key. Changing either invalidates cache, forcing a fresh run. Stored in `.ai_matches.json` metadata.

---

## 5. File Changes

**New files**
- `src/fees.rs` — `PolyCategory`, `category_fee_ppm`, `bps_to_ppm`.
- `src/matchers/mod.rs` — `MarketMatcher` trait.
- `src/matchers/sports.rs` — existing sports logic lifted from `discovery.rs`.
- `src/matchers/fomc.rs` — `FomcMatcher` + anchor resolver.
- `src/matchers/ai.rs` — reads `.ai_matches.json` produced by sidecar.
- `scripts/ai_matcher.py` — sidecar orchestrator.
- `scripts/ai_matcher/embedder.py`, `scripts/ai_matcher/verifier.py`, `scripts/ai_matcher/cache.py` — broken out for testability.
- `config/ai_categories.json` — which categories run through AI and their TTLs.
- `config/manual_overrides.json` — whitelist/blacklist.
- `requirements.txt` (new) or `scripts/requirements.txt` — `anthropic`, `openai`, `hnswlib`, `requests`.

**Modified files**
- `src/types.rs` — `MarketPair` gets `category` and `match_source` fields; fix the "3.00%" comment on `SPORTS_FEE_RATE_PPM`.
- `src/config.rs` — add `FomcSource`; existing `LeagueConfig` unchanged.
- `src/discovery.rs` — becomes an orchestrator of `MarketMatcher` instances. Sports logic moves to `src/matchers/sports.rs` verbatim.
- `src/main.rs:192-201` — replace the sports-only fee loop with the per-market resolver described in §4.1.
- `src/lib.rs` — wire new modules.
- `.gitignore` — add `.ai_matches.json`, `.ai_matcher_cache.json`, `.ai_matcher_audit.jsonl`.

**Untouched**
- Hot-path trading / execution (`execution.rs`, `polymarket_clob.rs` signing path, WebSocket handlers). The fee field is already `AtomicU32` per-market; we're just populating it smarter.

---

## 6. Failure Modes & Mitigations

| Failure                                         | Detection                             | Mitigation                                                                 |
|-------------------------------------------------|---------------------------------------|----------------------------------------------------------------------------|
| CLOB `get_market_meta` times out at startup     | per-call timeout + err log            | Fall back to category-table ppm; continue. One failure ≠ abort.            |
| FRED anchor unavailable for FOMC                | HTTP error                            | Emit zero FOMC pairs, log error. Never guess anchor.                       |
| LLM hallucinates a match                        | post-hoc audit log review             | Blacklist via `manual_overrides.json`; confidence threshold; execution gate |
| Embedding model change silently accepted        | cache version mismatch                | Cache key includes model name; mismatch forces rerun.                      |
| Definition drift (BTC Reserve vs. any BTC)      | LLM `concerns[]` non-empty → reject   | Prompt explicitly checks resolution criteria, not topic.                   |
| Python sidecar crashes mid-run                  | exit code ≠ 0 in Rust subprocess hook | Keep prior `.ai_matches.json` in place; alert via `tracing::warn!`.        |
| Stale `.ai_matches.json` loaded after long pause | file mtime vs. configured TTL         | Rust reader rejects if older than `AI_MATCHES_MAX_AGE_SEC`.                |
| Duplicate pair across structured + AI matchers  | merge pass                            | Structured wins; AI-only pairs added. Dedupe by `(kalshi_market_ticker, poly_condition_id)`. |

---

## 7. Rollout Plan

Ship in three independently-valuable PRs, so any can be reverted without the others:

**PR 1 — Per-category fees** (low risk, immediate correctness win)
- Introduce `PolyCategory`, fee table, `bps_to_ppm`.
- Add `category` to `MarketPair` (defaulting to `Sports` for back-compat).
- Replace `main.rs` loop with CLOB-seeded fees + table fallback.
- Fix the `types.rs` "3.00%" comment.
- Sports pipeline keeps working; fee values become more accurate.

**PR 2 — Matcher trait + FomcMatcher**
- Extract `SportsMatcher` behind `MarketMatcher` trait (no behavior change).
- Add `FomcMatcher` with FRED anchor.
- `DiscoveryClient` runs both matchers in parallel.
- Gates: detection-only for FOMC pairs for one week, then enable execution.

**PR 3 — AI matcher sidecar**
- `scripts/ai_matcher.py` with embeddings + LLM layers.
- `src/matchers/ai.rs` reader.
- `MatchSource::Ai` + `EXEC_ALLOW_AI_MATCHES` gate (default off).
- Manual overrides file + audit log.

Each PR has acceptance criteria:
- PR 1: every existing sports market gets the correct fee (verified against CLOB); existing integration behavior unchanged.
- PR 2: FOMC pairs discovered for next scheduled FOMC meeting; anchor correctly resolved; rate-band join produces N-1 pairs for N Kalshi bands where Poly has the intervening delta.
- PR 3: sidecar produces JSON, Rust reader loads it; end-to-end dry run on a 50-market sample; at least 1 high-value non-sports pair flows through detection.

---

## 8. Open Questions

1. **Embedding provider** — OpenAI `text-embedding-3-small` vs. Voyage/Cohere. Defaulting to OpenAI on cost/familiarity; swappable.
2. **FOMC anchor fallback** — if FRED requires an API key the user doesn't want to manage, is there a Kalshi-hosted alternative exposing the current target rate? To resolve during PR 2 implementation.
3. **AI matcher on sports** — currently excluded. Worth running *as a shadow / verification layer* (disagreement = alert) once PR 3 stabilizes? Deferred.
4. **Live re-matching of AI pairs** — today matches are loaded at startup. AI pairs change as markets change; do we hot-reload `.ai_matches.json` into `GlobalState` on file change? Deferred; first cut restarts the process.
5. **Cost ceiling** — should we hard-cap monthly LLM spend with a token-counter kill switch in the sidecar? Likely yes, but tunable after observing real-run costs.

---

## Appendix A — LLM Verification Prompt (draft)

```
System:
You are evaluating whether two prediction-market contracts — one on Kalshi,
one on Polymarket — resolve to IDENTICAL outcomes. This is for arbitrage
matching, so false positives cost real money.

Be paranoid about resolution criteria. Two markets that sound similar can
resolve differently on edge cases. Classic traps:
  - Different resolution dates / windows
  - Different data sources (e.g., which exchange's BTC price)
  - Qualitative vs. quantitative thresholds with different cutoffs
  - Definitional scope differences (e.g., "any X" vs. "official X")
  - Inclusive vs. exclusive range boundaries

Respond with a JSON object and nothing else:
{
  "confidence": float in [0, 1],
  "resolution_match": bool,
  "concerns": [list of specific concerns; empty if none],
  "reasoning": "one-paragraph explanation",
  "category": one of ["Sports","Crypto","Economics","Politics","Tech",
                      "Culture","Weather","Finance","Mentions",
                      "Geopolitical","Unknown"]
}

User:
KALSHI MARKET:
  Title: {kalshi.title}
  Description: {kalshi.description}
  Resolution criteria: {kalshi.resolution_criteria}
  Outcomes: {kalshi.outcomes}

POLYMARKET MARKET:
  Title: {poly.title}
  Description: {poly.description}
  Resolution criteria: {poly.resolution_criteria}
  Outcomes: {poly.outcomes}

Do these resolve identically? Score accordingly.
```

---

## Appendix B — Numeric Examples

**FOMC mapping, April 2026 meeting**
Anchor: fed funds target 4.25–4.50% (midpoint 4.375%, lower bound 425 bps).

| Kalshi market       | floor_strike | band (bps) | Poly outcome           | delta (bps) | Match? |
|---------------------|--------------|------------|------------------------|-------------|--------|
| `KXFED-26APR-T375`  | 3.75         | 375        | "50 bps decrease"      | −50         | ✅     |
| `KXFED-26APR-T400`  | 4.00         | 400        | "25 bps decrease"      | −25         | ✅     |
| `KXFED-26APR-T425`  | 4.25         | 425        | "No change"            |   0         | ✅     |
| `KXFED-26APR-T450`  | 4.50         | 450        | "25 bps increase"      | +25         | ✅     |
| `KXFED-26APR-T475`  | 4.75         | 475        | (no outcome)           | —           | ❌ skip |

**Fee sanity check**
A 50¢ FOMC market with the old sports rate (30,000 ppm, 0.75% peak):
- fee per dollar = 30,000 × 50 × 50 / 100,000,000 = 0.75¢

Same market at the correct Economics rate (60,000 ppm, 1.50% peak):
- fee per dollar = 60,000 × 50 × 50 / 100,000,000 = 1.50¢

Delta: **0.75¢ per Polymarket leg** at p=0.5. For a cross-platform arb (one Poly leg + one Kalshi leg) the detector would under-estimate total cost by 0.75¢/$ — any detected arb with <0.75¢ of modeled profit is actually a loss. For a Poly-only arb (both legs on Polymarket), the under-estimate doubles to 1.5¢/$. At `ARB_THRESHOLD = 0.995` (0.5¢ of modeled profit) the current threshold has zero headroom for this error, so PR 1 is a correctness fix, not just hygiene.

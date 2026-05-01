# Multi-Category Market Matching, Canonical Schema & Auditable AI Matcher

**Status:** Draft v2 (regenerated 2026-04-21)
**Scope:** Extend the bot from sports-only to multi-category (FOMC, macro econ, elections, long-tail) with (a) a canonical-event-schema + per-event-type adapter architecture, (b) per-category Polymarket fees driven by the CLOB `feeSchedule`, and (c) a standalone human-auditable AI matcher sidecar.

---

## 1. Background

Two bottlenecks are blocking expansion beyond sports:

**Discovery is tightly coupled to sports.** `src/discovery.rs` parses Kalshi tickers as `date+team1+team2` (see `parse_kalshi_event_ticker` at `discovery.rs:537`) and builds Polymarket slugs of the form `prefix-team1-team2-date[-suffix]` (`build_poly_slug` at `discovery.rs:468`). This shape does not generalize: FOMC markets encode a target-rate band, elections encode jurisdiction + candidate, CPI encodes a release date + value range.

**Fees are stamped as a single sports constant.** `src/main.rs:192-201` applies `SPORTS_FEE_RATE_PPM` to every tracked market. After Polymarket's March 2026 rollout, categories charge different taker fees (Crypto 1.80%, Economics 1.50%, Politics 1.00%, Sports 0.75%, Geopolitical 0%). Using the sports rate on an FOMC market under-estimates fees by 2× and can convert a detected arb into a guaranteed loss — the current `ARB_THRESHOLD = 0.995` has zero headroom for this error.

**Motivation.** The highest-alpha categories for cross-venue arbitrage are the ones where the participant bases diverge most — FOMC rate decisions, macro econ releases, and elections. These are precisely the categories where fee misestimation hurts the most.

**Note on the existing `SPORTS_FEE_RATE_PPM = 30_000` comment.** `types.rs:11-16` labels this "3.00%", but `poly_fee_cents` yields a peak of `rate_ppm / 40_000` cents per dollar, i.e. **0.75% at 30,000 ppm** — which matches Polymarket's current Sports rate. The value is correct; the comment is wrong and will be fixed in PR 1.

---

## 2. Goals / Non-Goals

**Goals**

1. Per-category Polymarket fee handling, with the CLOB as source of truth and a category→ppm fallback table.
2. A **canonical event schema** and per-event-type adapter layer (`SportsAdapter`, `FomcAdapter`, future: `CpiAdapter`, `ElectionAdapter`).
3. A structured `FomcAdapter` that pairs Kalshi `KXFED*` rate-band markets with Polymarket neg-risk outcomes using a current-rate anchor.
4. An AI matcher that is **runnable entirely independently of the Rust binary** and produces human-auditable output (static HTML review UI + `manual_overrides.json` for sign-off).
5. Zero regression in the existing sports pipeline.
6. Discovery output remains a single `Vec<MarketPair>` consumable by `GlobalState`; the hot path (execution, WebSocket, orderbook) is untouched.

**Non-Goals**

- Replacing the existing sports logic with AI matching. Sports stays deterministic.
- Polymarket fee-free Geopolitical category support (follow-up).
- Maker/taker routing in the execution engine — out of scope; keep taker-only behavior.
- Full versioned mapping-registry database. Extending the existing `.discovery_cache.json` with adapter-version tags is sufficient.
- Live hot-reloading of `MarketPair` set mid-session.
- Fixing `tests/integration_tests.rs` (pre-existing breakage; separate concern).
- Any dependency from the Rust hot path on pmxt or any Python library.

---

## 3. Design Overview

```
┌─── Rust process ──────────────────────────────────────────────────────────┐
│                                                                            │
│  DiscoveryClient orchestrates a Vec<Box<dyn EventAdapter>>:                │
│                                                                            │
│    SportsAdapter ─┐                                                        │
│                   │                                                        │
│    FomcAdapter ───┼─▶ CanonicalEvent ─▶ pair-join ─▶ Vec<MarketPair>       │
│                   │                                                        │
│    AiMatcherRead ─┘  (consumes .ai_matches.json produced by sidecar)       │
│                                                                            │
│  FeeResolver.apply_to_markets(state):                                      │
│    for each market:                                                        │
│      fee_ppm = seed_from_clob_meta()               // source of truth      │
│             ?? category_fee_ppm(pair.category)     // fallback             │
│                                                                            │
└────────────────────────────────────────────────────────────────────────────┘

┌─── scripts/ai_matcher.py — runs entirely standalone ──────────────────────┐
│                                                                            │
│   python scripts/ai_matcher.py run      # periodic discovery run           │
│   python scripts/ai_matcher.py review   # opens static HTML audit report   │
│   python scripts/ai_matcher.py audit    # random-sample verification pass  │
│                                                                            │
│  Pipeline:                                                                 │
│    1. Ingest via pmxt (Kalshi + Polymarket catalogs)                       │
│    2. Normalize → CanonicalEventDraft (Python mirror of the Rust type)     │
│    3. Apply event-type classifiers (FOMC / CPI / Elections / long-tail)    │
│    4. Layer 1: embed + cosine top-K retrieval                              │
│    5. Layer 2: LLM verification with structured output                     │
│    6. Apply manual_overrides.json (whitelist/blacklist)                    │
│    7. Emit:                                                                │
│         - .ai_matches.json         (consumed by Rust)                      │
│         - audit/report.html        (human review UI, static file)          │
│         - .ai_matcher_audit.jsonl  (append-only full decision log)         │
└────────────────────────────────────────────────────────────────────────────┘
```

Design rules:
- **Structured adapters win over AI pairs.** If Sports or FOMC already produced a pair for a given Kalshi market, AI pairs for the same ticker are discarded during merge.
- **Human overrides win over both.** `manual_overrides.json` blacklists reject any adapter's pair; whitelists force a specific pair.
- **Sidecar has zero Rust dependency.** It can be run, reviewed, and trusted without the bot running. Rust only reads its output JSON.

---

## 4. Detailed Design

### 4.1 Per-Category Fee Table + Per-Market CLOB Override

New module `src/fees.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PolyCategory {
    Sports, Crypto, Economics, Politics, Tech, Culture,
    Weather, Finance, Mentions, Geopolitical, Unknown,
}

pub fn category_fee_ppm(c: PolyCategory) -> u32 {
    // ppm encodes the bot's internal pre-scale rate such that
    // peak_cents_per_dollar at p=0.5 equals rate_ppm / 40_000.
    // Verify this convention against feeSchedule during §4.1.5.
    match c {
        PolyCategory::Crypto       => 72_000, // 1.80% peak
        PolyCategory::Mentions     => 62_400, // 1.56%
        PolyCategory::Economics    => 60_000, // 1.50%
        PolyCategory::Culture      => 50_000, // 1.25%
        PolyCategory::Weather      => 50_000, // 1.25%
        PolyCategory::Finance      => 40_000, // 1.00%
        PolyCategory::Politics     => 40_000, // 1.00%
        PolyCategory::Tech         => 40_000, // 1.00%
        PolyCategory::Sports       => 30_000, // 0.75%
        PolyCategory::Geopolitical => 0,
        PolyCategory::Unknown      => 72_000, // conservative: highest rate
    }
}
```

`SPORTS_FEE_RATE_PPM` either becomes `category_fee_ppm(PolyCategory::Sports)` or is deleted with callers migrated. The misleading "3.00%" comment in `types.rs:11-16` is corrected.

**Per-market fee seeding** (replaces `main.rs:192-201`):

```rust
for i in 0..state.market_count() {
    let pair = state.markets[i].pair.as_ref().unwrap();
    let ppm = match poly_async.get_market_meta(&pair.poly_yes_token,
                                                &pair.poly_condition_id).await {
        Ok((_, fee_bps)) => fees::bps_to_ppm(fee_bps),
        Err(e) => {
            warn!("fee meta fetch failed for {}: {}; falling back to category",
                  pair.pair_id, e);
            fees::category_fee_ppm(pair.category)
        }
    };
    state.markets[i].set_poly_fee_rate_ppm(ppm);
}
```

Benefits:
- CLOB is authoritative (no drift if Polymarket changes rates).
- Reuses the existing `SharedAsyncClient.meta_cache` (`polymarket_clob.rs:737-802`); same cache also serves order signing.
- Category table exists for offline use and as the guaranteed fallback.

### 4.1.5 `feeSchedule` Verification + Fee Formula Verification

**Two calibration tasks land before PR 1 is frozen.** Both are small (< 1 day) but they are blocking because getting either wrong invalidates fee math across every category.

**Task A — `feeSchedule` field survey.** The current `fetch_market_meta` (`polymarket_clob.rs:635-712`) loops over eight legacy fee field names. Recent reporting suggests Polymarket now publishes a structured `feeSchedule` object per market. Action: fetch `GET /markets/{condition_id}` for one market per category, dump the raw JSON, confirm whether `feeSchedule` exists and what it contains. If present, it becomes the primary path in `fetch_market_meta`; the legacy key loop stays as fallback.

**Task B — fee formula calibration.** The bot's `poly_fee_cents` uses `fee = rate × p × (1-p) / 10^8` (`types.rs:243`). Polymarket's published formula may be `p × (1-p)`-scaled or `min(p, 1-p)`-scaled — historically both have appeared in docs. Action: fetch meta for a known-rate market (a Sports market, quoted at 0.75%), observe the `fee_bps` (or `feeSchedule`) value, and back-solve the conversion:

```rust
// In fees.rs — encoded as a single calibrated constant + unit test.
pub fn bps_to_ppm(bps: i64) -> u32 {
    // To be calibrated during Task B. Must satisfy:
    //   bps_to_ppm(<observed Sports bps>) == 30_000
    // The unit test below locks this invariant.
    unimplemented!("calibrate after feeSchedule survey")
}

#[cfg(test)]
mod tests {
    #[test]
    fn sports_conversion_is_30k_ppm() {
        // <OBSERVED_SPORTS_BPS> replaced during calibration.
        assert_eq!(bps_to_ppm(OBSERVED_SPORTS_BPS), 30_000);
    }
}
```

If the formula convention itself differs (e.g. Polymarket switches to `min(p, 1-p)`), `poly_fee_cents` gains an alternate branch and a comment noting which convention applies. The test catches regressions.

### 4.2 `MarketPair` Additions

```rust
pub struct MarketPair {
    // ... existing fields unchanged ...
    pub category: PolyCategory,         // NEW
    pub match_source: MatchSource,      // NEW
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MatchSource {
    Structured { adapter: &'static str },       // "sports", "fomc"
    Ai { confidence: f32, model: Arc<str> },    // e.g. "claude-opus-4-7"
    ManualOverride,
}
```

Both fields use `#[serde(default)]` so existing `.discovery_cache.json` files deserialize without wipe — defaulting to `PolyCategory::Sports` + `MatchSource::Structured { adapter: "sports" }`.

**Execution gate** (`execution.rs`, start of order dispatch — one branch):
```rust
if matches!(pair.match_source, MatchSource::Ai { .. })
    && !config::exec_allow_ai_matches() {
    return Err(anyhow!("AI-matched pair execution disabled"));
}
```
Default `EXEC_ALLOW_AI_MATCHES=0`. Detector still runs and logs AI-matched opportunities; execution simply refuses them until the gate flips.

### 4.3 Canonical Schema + Adapter Architecture

The core insight from the research: instead of each matcher returning `MarketPair` directly (which collapses normalization into pairing), introduce an intermediate canonical representation. Every adapter produces `CanonicalMarket`s; pairing is a single join that all categories share.

```rust
// src/canonical.rs
#[derive(Debug, Clone)]
pub struct CanonicalMarket {
    pub event_type: EventType,        // Sports | Fomc | Cpi | Election | Other
    pub underlier: Underlier,         // what is being predicted
    pub parameters: Parameters,       // outcome-specific params (enum variant)
    pub time_window: TimeWindow,      // event_at, settles_at
    pub venue: Venue,                 // Kalshi | Polymarket + venue-specific IDs
    pub category: PolyCategory,       // drives fees
    pub raw_title: Arc<str>,          // preserved for audit logs
    pub raw_description: Arc<str>,
    pub adapter_version: u32,         // cache key
}

pub enum EventType { Sports, Fomc, Cpi, NfpJobs, Election, Other }

pub enum Underlier {
    SportsGame { league: Arc<str>, home: Arc<str>, away: Arc<str> },
    FomcRateBand { meeting_date: NaiveDate, floor_bps: i32 },
    CpiValue { release_date: NaiveDate, series: CpiSeries, threshold: f32,
               cmp: Comparison },
    ElectionCandidate { race_id: Arc<str>, candidate_normalized: Arc<str> },
    Other,  // AI-matched markets don't project onto a structured underlier
}

pub struct Venue {
    pub platform: Platform,                // Kalshi | Polymarket
    pub kalshi_market_ticker: Option<Arc<str>>,
    pub kalshi_event_ticker: Option<Arc<str>>,
    pub poly_slug: Option<Arc<str>>,
    pub poly_yes_token: Option<Arc<str>>,
    pub poly_no_token: Option<Arc<str>>,
    pub poly_condition_id: Option<Arc<str>>,
}
```

**The `EventAdapter` trait** owns both normalization and candidate generation, but pairing is shared:

```rust
#[async_trait]
pub trait EventAdapter: Send + Sync {
    fn name(&self) -> &'static str;                    // "sports", "fomc"
    fn event_type(&self) -> EventType;
    fn version(&self) -> u32;

    /// Fetch raw markets from both venues and normalize them.
    async fn normalize(&self) -> Result<NormalizedBatch>;
}

pub struct NormalizedBatch {
    pub kalshi: Vec<CanonicalMarket>,
    pub poly: Vec<CanonicalMarket>,
}
```

**Shared pairing logic** (no per-adapter code):

```rust
// src/matchers/mod.rs
pub fn pair_batch(batch: NormalizedBatch,
                  adapter_name: &'static str) -> Vec<MarketPair> {
    let poly_by_key: HashMap<(EventType, Underlier), &CanonicalMarket> =
        batch.poly.iter().map(|m| ((m.event_type, m.underlier.clone()), m)).collect();

    batch.kalshi.iter().filter_map(|k| {
        poly_by_key.get(&(k.event_type, k.underlier.clone()))
                   .map(|p| build_pair(k, p, adapter_name))
    }).collect()
}
```

The adapter's job shrinks to one verb: **normalize into a canonical form**. Pairing is automatic once both sides project onto the same `Underlier`. This is what the research report calls "two-phase discovery": normalization is candidate generation; the shared `pair_batch` does verification via equality on the canonical key.

### 4.4 `SportsAdapter` (Existing Logic, Refactored)

Lives in `src/adapters/sports.rs`. Logic is lifted verbatim from `discovery.rs`:
- Fetches Kalshi events by series (`KXEPLGAME`, `KXNBAGAME`, etc.).
- Parses `date+teams` → structured fields via `parse_kalshi_event_ticker`.
- Builds Polymarket slug via `build_poly_slug` and fetches via Gamma.

The only change: instead of returning `MarketPair` directly, it returns `NormalizedBatch`:
- Each Kalshi market becomes `CanonicalMarket { underlier: Underlier::SportsGame {...}, ... }`.
- Each matched Polymarket market becomes a `CanonicalMarket` with the same `Underlier`.

`pair_batch` then joins them. Behavior is byte-identical to today's output; this refactor is mechanical and should produce an unchanged `.discovery_cache.json`.

### 4.5 `FomcAdapter` (New, Structured)

Lives in `src/adapters/fomc.rs`. Algorithm:

1. **Fetch Kalshi events** in series `KXFED`. Each event corresponds to one FOMC meeting (e.g. `KXFED-26MAY`).
2. **Resolve anchor** — the current fed-funds target lower-bound (bps). Two strategies, tried in order:
   1. Read from Kalshi event metadata if exposed (to verify during impl; may not exist).
   2. Fetch from FRED: `https://api.stlouisfed.org/fred/series/observations?series_id=DFEDTARL` (target lower bound). Requires optional `FRED_API_KEY`.
   3. If both fail → log error, emit zero pairs for this meeting. Never guess.
3. **Normalize Kalshi side.** For each market in the event with a `floor_strike`:
   ```
   CanonicalMarket {
     underlier: FomcRateBand { meeting_date, floor_bps: (floor_strike * 100) as i32 },
     ...
   }
   ```
4. **Fetch Polymarket neg-risk event** via Gamma search (hint: slug pattern `fomc-decision-<month>-<year>`). Iterate its outcomes.
5. **Normalize Polymarket side.** For each outcome, parse delta from title (`"25 bps cut"` → `-25`, `"No change"` → `0`, `"25 bps hike"` → `+25`), compute `floor_bps = anchor_bps + delta_bps`:
   ```
   CanonicalMarket {
     underlier: FomcRateBand { meeting_date, floor_bps: anchor + delta },
     ...
   }
   ```
6. `pair_batch` joins on `(Fomc, FomcRateBand { meeting_date, floor_bps })`.

Tail bands with no Polymarket counterpart (e.g. "above 6.00%") are simply skipped — no error, they produce no pair.

**Parser** for Polymarket outcome labels handles common variants: `"\d+\s*bps?\s*(cut|decrease|lower)"`, `"no change|hold"`, `"\d+\s*bps?\s*(hike|increase|raise)"`. Unrecognized labels are logged and skipped.

### 4.6 AI Matcher Sidecar

Located at `scripts/ai_matcher.py`. **Fully standalone**: runs without any Rust process, without the bot's Rust data files, and without any service from the rest of the codebase. It reads configs, hits APIs, writes outputs. Rust reads its output file and trusts it.

Command-line surface:
```
python scripts/ai_matcher.py run                # one discovery pass
python scripts/ai_matcher.py run --loop         # loop with per-category TTLs
python scripts/ai_matcher.py run --category politics --sample 50
python scripts/ai_matcher.py review             # open audit/report.html
python scripts/ai_matcher.py audit --sample 20  # random spot-check of accepted pairs
python scripts/ai_matcher.py calibrate-fees     # one-shot: survey feeSchedule (Task A/B)
```

Dependencies (`scripts/requirements.txt`): `pmxt`, `anthropic`, `openai` (for embeddings), `hnswlib`, `jinja2`, `requests`. No Flask, no web server — review UI is static HTML.

#### 4.6.1 Ingestion via pmxt

`scripts/ai_matcher/ingestion.py` is a thin facade over pmxt:

```python
import pmxt

class Ingestion:
    def __init__(self):
        self._kalshi = pmxt.Kalshi(api_key=..., api_secret=...)
        self._poly   = pmxt.Polymarket()

    def fetch_all(self) -> IngestionResult:
        kalshi = self._kalshi.fetch_markets()   # list[pmxt.Market]
        poly   = self._poly.fetch_markets()
        # If pmxt drops fields we need (e.g. Polymarket condition_id, neg_risk),
        # fall back to direct REST for those specific fields — documented per-field
        # below as they are discovered during PR 3 implementation.
        return IngestionResult(kalshi=kalshi, poly=poly)
```

**Why facade:** pmxt is pre-1.0 (dependency risk). Everything downstream consumes `IngestionResult` with known fields; if pmxt breaks or omits a field, we change the facade only. Swap-to-direct-REST is a one-day rollback.

**Rate-limit hygiene.** pmxt has its own Kalshi rate limiter. The sidecar and the Rust discovery loop must not hit Kalshi concurrently. Rule: Rust's `DiscoveryClient` runs once at startup + on-demand; sidecar schedules so its runs don't overlap with Rust's. If it becomes a real issue, we share a `kalshi_rate.lock` file.

#### 4.6.2 Layer 1 — Embeddings

- **Model:** OpenAI `text-embedding-3-small` (swappable via `EMBEDDING_MODEL` env). Cheap, 1536-dim.
- **Input text:** `title + "\n" + description + "\n" + resolution_criteria + "\n" + outcomes_joined`.
- **Content hash:** SHA-256 over the input text. Stored in `.ai_matcher_cache.json` as the re-embed cache key. Unchanged markets → skip re-embed → ~free incremental runs.
- **Index:** `hnswlib` in-memory, rebuilt per run. Fine for tens of thousands of markets; move to `lancedb` if >100k.
- **Retrieval:** for each Kalshi market, top-K=8 Polymarket candidates by cosine similarity. Pre-filter: `MIN_COSINE=0.55` (configurable) strips obvious non-matches before spending LLM tokens.

#### 4.6.3 Layer 2 — LLM Verification

- **Model:** Claude `claude-opus-4-7` by default (swappable; Sonnet 4.6 if cost-sensitive).
- **Structured output** (enforced via `response_format`):
  ```json
  {
    "confidence": 0.97,
    "resolution_match": true,
    "concerns": [],
    "reasoning": "Both markets resolve on …",
    "category": "Economics",
    "event_type": "Cpi"
  }
  ```
- **Cache:** keyed by `(kalshi_content_hash, poly_content_hash, llm_model)`. If both sides' hashes are unchanged since the last accepted decision, skip the LLM call entirely.
- **Acceptance rule:** `confidence >= 0.9 AND resolution_match AND len(concerns) == 0`. Any rejection, including borderline ones, is still written to the audit log with full reasoning.
- **Prompt** (Appendix A) is explicit that we are scoring *resolution-criteria identity*, not topical similarity — the trap of "any BTC held" vs "National BTC Reserve" is called out by name.

#### 4.6.4 Human Audit Surface (the critical deliverable)

All three outputs are always produced on every `run`:

**1. `.ai_matches.json`** — machine-readable, consumed by Rust:
```json
{
  "generated_at": "2026-04-21T14:00:00Z",
  "model": "claude-opus-4-7",
  "embedding_model": "text-embedding-3-small",
  "pmxt_version": "0.7.3",
  "version": 1,
  "pairs": [
    {
      "kalshi_market_ticker": "KXCPIYOY-26APR-B3.0",
      "poly_condition_id": "0x…",
      "poly_yes_token": "0x…",
      "poly_no_token": "0x…",
      "category": "Economics",
      "event_type": "Cpi",
      "confidence": 0.97,
      "description": "CPI YoY April 2026 > 3.0%"
    }
  ]
}
```
Rust's `AiMatcherReader` loads this, filters by freshness (reject if `generated_at` older than `AI_MATCHES_MAX_AGE_SEC`, default 24h), and emits `MarketPair` rows.

**2. `audit/report.html`** — static HTML, one pair per row, zero JS server required. Generated via Jinja2. Columns per row:

| Decision | Kalshi side | Polymarket side | LLM analysis | Override action |
|---|---|---|---|---|
| ✅ accepted (0.97) | Title, description, resolution criteria, outcomes — each with a `→` link to `kalshi.com/markets/...` | Same, linking to `polymarket.com/event/...` | `confidence`, `concerns[]`, `reasoning` paragraph, parsed `category` and `event_type` | Pre-filled JSON snippet to paste into `manual_overrides.json` for reject/keep |

The user opens `audit/report.html` in a browser (`file://`), reads rows side-by-side, and — if they disagree with a decision — copies the pre-filled snippet into `config/manual_overrides.json` (manual JSON editing, as you asked for).

The report also supports filters rendered as simple HTML anchors (no JS): `report.html?only=accepted`, `?only=rejected`, `?category=economics`, `?confidence_below=0.95`. Filters work by generating separate static files (`report-accepted.html`, `report-rejected.html`, etc.) alongside the main one.

**3. `.ai_matcher_audit.jsonl`** — append-only full decision log (accepted AND rejected), human-grep-able:
```jsonl
{"ts":"2026-04-21T14:00:01Z","kalshi":"KXCPIYOY-26APR-B3.0","poly":"0x…","decision":"accept","confidence":0.97,"concerns":[],"reasoning":"…"}
{"ts":"2026-04-21T14:00:02Z","kalshi":"KXFED-26MAY-T425","poly":"0x…","decision":"reject","confidence":0.62,"concerns":["Different meeting dates"],"reasoning":"…"}
```

**`config/manual_overrides.json`** — human-authored ground truth:
```json
{
  "version": 1,
  "whitelist": [
    { "kalshi_market_ticker": "...", "poly_condition_id": "...",
      "category": "Politics", "reason": "verified 2026-04-21" }
  ],
  "blacklist": [
    { "kalshi_market_ticker": "...", "poly_condition_id": "...",
      "reason": "different resolution dates (Kalshi: Jun 5, Poly: Jun 30)" }
  ]
}
```
Blacklist wins over whitelist wins over AI decision. Sidecar applies overrides in-memory during every `run`; overrides persist across runs because the file is source-controlled.

**`python scripts/ai_matcher.py audit --sample 20`** — spot-check workflow. Picks 20 random *accepted* pairs (biased toward low-confidence if available), renders a single-page HTML with each pair expanded, opens it in the browser. Use case: "randomly verify the bot isn't hallucinating matches" — exactly what you asked for.

**Running the sidecar without the Rust code.** Everything above — ingestion, embeddings, LLM, audit reports, override application — runs purely in Python. The user can inspect the bot's matching *without* trusting any Rust state.

### 4.7 Scheduling

A small scheduler inside `scripts/ai_matcher.py --loop` runs each category on its own TTL:

| Category                      | Interval |
|-------------------------------|----------|
| Macro econ (CPI, NFP, GDP)    | 12h      |
| Elections / politics          | 2h       |
| Crypto hourly                 | 15m      |
| Long-tail / other             | 6h       |
| Sports                        | n/a (structured) |
| FOMC                          | n/a (structured) |

Categories are listed in `config/ai_categories.json` with their TTLs. A run ticker persists last-run timestamps in `.ai_matcher_schedule.json`.

**Deployment options** (user picks later):
- Cron / systemd timer running `run --loop` — simplest.
- `tokio::spawn` subprocess launch from Rust at startup (lost independence benefit — not recommended given the "runnable without Rust" requirement).

Default: user runs `python scripts/ai_matcher.py run --loop` in a separate terminal / tmux pane / systemd unit. This keeps the sidecar cleanly decoupled.

### 4.8 Safety Gates

1. **Confidence floor** (≥0.9) and non-empty `concerns[]` rejection, enforced inside the sidecar.
2. **Manual override file** wins over any AI decision (blacklist > whitelist > AI).
3. **Execution gate** `EXEC_ALLOW_AI_MATCHES=0` by default — detector still runs on AI pairs, execution refuses them.
4. **Freshness gate** in Rust reader — reject `.ai_matches.json` if older than `AI_MATCHES_MAX_AGE_SEC` (default 24h). Protects against stale matches after a long pause.
5. **Model / embedding version pinning** — cache keys include model names; bumping either invalidates cache. `.ai_matches.json` metadata records which models produced it.
6. **Audit log retention** — `.ai_matcher_audit.jsonl` is append-only and never auto-pruned. Retained for retrospective review.
7. **Structured matchers win on collision** — AI pair for the same `kalshi_market_ticker` as a Sports/FOMC pair is discarded during merge, with a log entry.

---

## 5. File Changes

**New — Rust side**
- `src/fees.rs` — `PolyCategory`, `category_fee_ppm`, `bps_to_ppm` (calibrated).
- `src/canonical.rs` — `CanonicalMarket`, `EventType`, `Underlier`, `TimeWindow`, `Venue`.
- `src/adapters/mod.rs` — `EventAdapter` trait + shared `pair_batch` logic.
- `src/adapters/sports.rs` — existing discovery logic, refactored to produce `NormalizedBatch`.
- `src/adapters/fomc.rs` — `FomcAdapter` + FRED anchor resolver.
- `src/adapters/ai_reader.rs` — loads `.ai_matches.json`, emits `MarketPair`s with freshness check.

**New — Python sidecar**
- `scripts/ai_matcher.py` — CLI entrypoint (`run | review | audit | calibrate-fees`).
- `scripts/ai_matcher/ingestion.py` — pmxt facade.
- `scripts/ai_matcher/embedder.py` — OpenAI embeddings + hnswlib index + content-hash cache.
- `scripts/ai_matcher/verifier.py` — Claude structured-output verification + cache.
- `scripts/ai_matcher/overrides.py` — apply `manual_overrides.json`.
- `scripts/ai_matcher/report.py` — Jinja2 static HTML generator.
- `scripts/ai_matcher/scheduler.py` — per-category TTL loop.
- `scripts/ai_matcher/templates/report.html.j2` — HTML template.
- `scripts/requirements.txt`.
- `config/ai_categories.json`.
- `config/manual_overrides.json` (seeded empty).

**Modified**
- `src/types.rs` — `MarketPair` gets `category` + `match_source` (both `#[serde(default)]`); fix `SPORTS_FEE_RATE_PPM` comment.
- `src/config.rs` — add `FomcSource`; existing `LeagueConfig` unchanged.
- `src/discovery.rs` — becomes an orchestrator that runs `EventAdapter`s in parallel, calls `pair_batch`, merges results, and reads `ai_reader`. Sports logic is gone from this file (moved to `adapters/sports.rs`).
- `src/main.rs:192-201` — per-market fee resolver (§4.1).
- `src/execution.rs` — one-line AI-execution gate (§4.2).
- `src/polymarket_clob.rs:635-712` — `feeSchedule` primary path after Task A survey; legacy key loop as fallback.
- `src/lib.rs` — wire new modules.
- `.gitignore` — `.ai_matches.json`, `.ai_matcher_cache.json`, `.ai_matcher_audit.jsonl`, `.ai_matcher_schedule.json`, `audit/`.

**Untouched**
- Hot-path trading/execution (WebSocket handlers, orderbook atomics, order signing).
- Kalshi / Polymarket Rust clients (they continue to serve the Rust discovery + hot path).

---

## 6. Failure Modes & Mitigations

| Failure | Detection | Mitigation |
|---|---|---|
| CLOB `get_market_meta` times out at startup | per-call timeout + err log | Fall back to `category_fee_ppm`; continue. |
| `feeSchedule` absent on some markets | JSON field missing at runtime | Legacy key loop remains as fallback. |
| `bps_to_ppm` calibrated wrong | unit test `sports_conversion_is_30k_ppm` fails | Blocking — can't ship PR 1 until green. |
| FRED anchor unavailable for FOMC | HTTP error | Emit zero FOMC pairs; log error. Never guess anchor. |
| LLM hallucinates a match | post-hoc review of `audit/report.html` | Blacklist via `manual_overrides.json`; confidence floor; execution gate. |
| Definition drift (scope mismatch) | LLM `concerns[]` non-empty | Prompt explicitly checks resolution criteria. |
| pmxt drops a field we need | facade test + manual verify on real markets | Direct-REST fallback in `ingestion.py` for the affected field. |
| pmxt breaking release | pinned version in `requirements.txt` | Swap-out behind the facade (~1 day of work). |
| Sidecar crashes mid-run | exit code ≠ 0; partial outputs not swapped in | Atomic rename (`.ai_matches.json.tmp` → `.ai_matches.json`). Prior file stays valid. |
| Stale `.ai_matches.json` loaded after long pause | `AI_MATCHES_MAX_AGE_SEC` freshness check | Rust reader refuses; logs `warn`. |
| Duplicate pair across structured + AI | merge pass dedupes by `(kalshi_market_ticker, poly_condition_id)` | Structured wins; AI-only pairs added. |
| Rust discovery + sidecar both hitting Kalshi | Kalshi 429s | Schedule sidecar outside Rust's discovery window. Add shared lockfile if it recurs. |
| User edits `manual_overrides.json` while sidecar is mid-run | Sidecar reads overrides at start of run | Current run uses pre-edit state; next run picks up edits. Documented, acceptable. |

---

## 7. Rollout Plan

Three independently-valuable PRs, each reversible without the others:

**PR 1 — Per-category fees + `feeSchedule` + canonical schema foundation**
- Land Tasks A and B (feeSchedule survey + fee-formula calibration) first.
- Introduce `PolyCategory`, `category_fee_ppm`, calibrated `bps_to_ppm` + unit test.
- Add `category` + `match_source` to `MarketPair` (serde-defaulted).
- Replace `main.rs:192-201` fee loop with CLOB-seeded + category fallback.
- Introduce `src/canonical.rs` types and `EventAdapter` trait (no new adapter yet).
- Refactor existing sports logic behind `SportsAdapter` — behavior-preserving.
- Fix `types.rs` "3.00%" comment.
- **Acceptance:** existing sports discovery produces the same set of `MarketPair`s (same pair_id, tickers, tokens) as the pre-PR cache; pre-existing `.discovery_cache.json` loads cleanly via the new serde defaults; fees match CLOB values on spot-check of 5 markets per category; `SPORTS_FEE_RATE_PPM` callers migrated; `bps_to_ppm` unit test green.

**PR 2 — FomcAdapter**
- Add `FomcAdapter` + FRED anchor resolver + rate-band parser.
- Wire it into `DiscoveryClient`'s adapter list.
- **Detection-only** for the first scheduled FOMC meeting: no execution on FOMC pairs (separate env or temporary code gate).
- **Acceptance:** for the next FOMC meeting, discover N-1 pairs for N Kalshi bands where Polymarket has the intervening delta; anchor correctly resolved from FRED; unmatched tail bands skipped cleanly.

**PR 3 — AI matcher sidecar (standalone, auditable)**
- `scripts/ai_matcher.py` + submodules.
- pmxt ingestion facade.
- Embeddings + LLM pipeline with content-hash cache.
- `audit/report.html` generator, `manual_overrides.json` application.
- `src/adapters/ai_reader.rs` + Rust freshness gate.
- `EXEC_ALLOW_AI_MATCHES=0` default.
- **Acceptance:** sidecar runnable end-to-end without the Rust bot; produces `report.html` with at least 10 accepted and 10 rejected pairs for inspection; at least one high-value non-sports pair flows through Rust detection (logged, not executed); `audit --sample 20` opens in browser and shows random accepted pairs.

---

## 8. Open Questions

1. **`feeSchedule` shape.** To resolve in Task A. Sample response JSON will dictate the exact field path.
2. **Fed anchor via Kalshi metadata.** ✅ Resolved 2026-04-29 during PR 2 implementation: the `KXFED` event schema does not currently expose a current-rate field. `try_anchor_from_kalshi_event` returns None as a stub and FRED `DFEDTARL` is the load-bearing anchor source. Re-evaluate if Kalshi's event schema gains a rate field; the stub is intentionally kept so a future change is one-line.
3. **pmxt field completeness.** Confirm pmxt exposes Polymarket `condition_id`, neg-risk flag, and full outcome tokens. If any are missing we'll fall through the facade to direct REST for those fields — flag and document per-field during PR 3.
4. **Long-tail category whitelist.** Which non-structured categories does the AI matcher actually attempt? Start narrow (Politics + Elections + Mentions) to contain cost and audit surface; widen once the review UI is being used.
5. **AI matcher on sports as a shadow verifier.** Run AI in parallel over sports and alert on disagreement with `SportsAdapter`? Out of scope for PR 3; potentially valuable in follow-up.
6. **Live hot-reload of AI matches into `GlobalState`.** First cut requires Rust restart. If sidecar cadence becomes tight (e.g. 15m for crypto), revisit.
7. **Cost ceiling.** Hard-cap monthly LLM token spend with a killswitch in the sidecar? Defer until we see real-run costs; structured-output + content-hash cache should keep it cheap (<$10/mo at initial category scope).

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
  - Different data sources (e.g., which exchange's BTC price; which BLS series)
  - Qualitative vs. quantitative thresholds with different cutoffs
  - Scope differences (e.g., "any X" vs. "official X"; "primary" vs. "general")
  - Inclusive vs. exclusive range boundaries
  - Different underliers that share a topic (e.g., target rate vs. effective rate)

Respond with a JSON object and nothing else:
{
  "confidence": float in [0, 1],
  "resolution_match": bool,
  "concerns": [list of specific concerns; empty if none],
  "reasoning": "one-paragraph explanation",
  "category": one of ["Sports","Crypto","Economics","Politics","Tech",
                      "Culture","Weather","Finance","Mentions",
                      "Geopolitical","Unknown"],
  "event_type": one of ["Sports","Fomc","Cpi","NfpJobs","Election","Other"]
}

Confidence calibration:
  0.95–1.0  identical resolution; safe to arbitrage
  0.85–0.94 very likely identical; flag any concerns
  0.70–0.84 plausibly identical; concerns must be investigated
  <0.70     too uncertain; reject

User:
KALSHI MARKET:
  Title:                {kalshi.title}
  Description:          {kalshi.description}
  Resolution criteria:  {kalshi.resolution_criteria}
  Outcomes:             {kalshi.outcomes}

POLYMARKET MARKET:
  Title:                {poly.title}
  Description:          {poly.description}
  Resolution criteria:  {poly.resolution_criteria}
  Outcomes:             {poly.outcomes}

Do these resolve identically? Score accordingly.
```

---

## Appendix B — Numeric Examples

**FOMC mapping, April 2026 meeting**
Anchor: fed funds target lower bound 425 bps (4.25%). Polymarket outcomes resolve relative to anchor.

| Kalshi market       | floor_strike | floor_bps | Poly outcome        | delta | Match? |
|---------------------|--------------|-----------|---------------------|-------|--------|
| `KXFED-26APR-T375`  | 3.75         | 375       | "50 bps decrease"   | −50   | ✅     |
| `KXFED-26APR-T400`  | 4.00         | 400       | "25 bps decrease"   | −25   | ✅     |
| `KXFED-26APR-T425`  | 4.25         | 425       | "No change"         |   0   | ✅     |
| `KXFED-26APR-T450`  | 4.50         | 450       | "25 bps increase"   | +25   | ✅     |
| `KXFED-26APR-T475`  | 4.75         | 475       | (no outcome)        |   —   | ❌ skip |

**Fee impact at 50¢ market price**

| Rate                      | ppm    | fee/$ at p=0.5 |
|---------------------------|--------|----------------|
| Sports (0.75% peak)       | 30,000 | 0.75¢          |
| Economics (1.50% peak)    | 60,000 | 1.50¢          |
| Crypto (1.80% peak)       | 72,000 | 1.80¢          |

**Impact at `ARB_THRESHOLD = 0.995` (0.5¢ modeled profit).** Using the Sports rate on an Economics market under-estimates fee by 0.75¢/$ per Polymarket leg. A cross-platform arb (one Poly + one Kalshi leg) would show 0.5¢ of modeled profit while actually costing 0.25¢ — *a guaranteed loss*. PR 1 is a correctness fix.

---

## Appendix C — pmxt Risk Mitigations

- **Pin** exact version in `scripts/requirements.txt` (e.g. `pmxt==0.7.3`). No `^` or `~` ranges.
- **Facade** in `scripts/ai_matcher/ingestion.py` — all downstream code consumes a stable internal dataclass, not pmxt types directly.
- **Facade contract test** — a small fixture-based test (`scripts/tests/test_ingestion.py`) that snapshot-tests the shape of `IngestionResult` for a handful of markets. If pmxt changes behavior, the test catches it before a run hits production.
- **Direct-REST fallback** — for any field pmxt doesn't surface (discovered during PR 3), `ingestion.py` enriches via direct `requests.get` calls. The facade boundary stays stable.
- **Swap-out time estimate** — ~1 day to replace pmxt with direct Kalshi + Gamma REST calls inside the facade. Acceptable dependency risk given the code we save.

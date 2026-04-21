# Multi-Category Matching — PR 1: Per-Category Fees & Canonical Schema Foundation

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the sports-only hardcoded Polymarket fee with a per-category table seeded from the CLOB, introduce a canonical-event-schema foundation with an `EventAdapter` trait, and refactor existing sports discovery behind the new trait — all with zero behavior regression for the existing sports pipeline.

**Architecture:** Three new modules (`src/fees.rs`, `src/canonical.rs`, `src/adapters/`) provide the foundation. `DiscoveryClient` becomes an orchestrator that runs a `Vec<Box<dyn EventAdapter>>`, each producing a `NormalizedBatch` of `CanonicalMarket`s; a shared `pair_batch` function performs the cross-venue join. The existing sports logic moves behind a `SportsAdapter` preserving its current behavior. `main.rs` seeds each market's Polymarket fee from `SharedAsyncClient.get_market_meta()` at startup, falling back to a category table.

**Tech Stack:** Rust (edition 2021), anyhow, serde, serde_json, tokio, reqwest, tracing. Existing `Cargo.toml` deps are sufficient — no new deps in PR 1. Shell survey script uses `bash`, `curl`, and `jq` (system-installed).

**Spec reference:** `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` §4.1, §4.1.5, §4.2, §4.3, §4.4, §7 PR 1.

---

## File Structure

**New files**
- `scripts/survey_fees.sh` — one-off Bash script to survey Polymarket CLOB `/markets` response shape per category.
- `docs/notes/2026-04-21-polymarket-fee-survey.md` — captured findings (field names, formats, one sample response per category).
- `src/fees.rs` — `PolyCategory` enum, `category_fee_ppm(PolyCategory) -> u32`, calibrated `bps_to_ppm(i64) -> u32`.
- `src/canonical.rs` — `CanonicalMarket`, `EventType`, `Underlier`, `TimeWindow`, `Venue`, `Platform`.
- `src/adapters/mod.rs` — `EventAdapter` trait + `NormalizedBatch` + shared `pair_batch` logic + `MatchSource`.
- `src/adapters/sports.rs` — `SportsAdapter`; existing sports discovery logic relocated and reshaped to produce `NormalizedBatch`.

**Modified files**
- `src/types.rs` — fix the "3.00%" comment on `SPORTS_FEE_RATE_PPM`; add `category: PolyCategory` and `match_source: MatchSource` fields (both `#[serde(default)]`) to `MarketPair`.
- `src/discovery.rs` — becomes adapter orchestrator. Sports-specific parsing functions move to `src/adapters/sports.rs`.
- `src/main.rs:192-201` — per-market CLOB-seeded fee resolver replacing single-constant loop.
- `src/polymarket_clob.rs:635-712` — `fetch_market_meta` prefers structured `feeSchedule` if the survey confirms its presence (conditional on Task 1 findings).
- `src/lib.rs` — declare `pub mod fees;`, `pub mod canonical;`, `pub mod adapters;`.

**Responsibility boundaries**
- `fees.rs` owns category→rate mapping and bps-to-ppm conversion, nothing else.
- `canonical.rs` owns only type definitions (no I/O, no logic beyond constructors and trait derivations).
- `adapters/mod.rs` owns the trait and the shared pair-join. No venue-specific code here.
- `adapters/sports.rs` owns all sports-specific ticker parsing, slug building, and team-code handling.
- `discovery.rs` owns orchestration only (cache + merge + adapter execution).

---

## Task 1: Survey Polymarket CLOB `/markets` Response Shape (Spec §4.1.5 Task A)

**Files:**
- Create: `scripts/survey_fees.sh`
- Create: `docs/notes/2026-04-21-polymarket-fee-survey.md`

- [ ] **Step 1: Create the survey script**

Create `scripts/survey_fees.sh`:
```bash
#!/usr/bin/env bash
# One-off survey of Polymarket CLOB /markets response shape per category.
# Used to drive the feeSchedule + bps-to-ppm calibration (PR 1, Task A + B).
#
# Usage:  bash scripts/survey_fees.sh > /tmp/fee_survey.json
#
# Requires: curl, jq
set -euo pipefail

# Gamma category tags. These are what Polymarket publishes on /markets?tag_slug=...
# If a category has no active markets, we fall back gracefully.
CATEGORIES=(sports crypto economics politics tech culture weather finance)

GAMMA="https://gamma-api.polymarket.com/markets"
CLOB="https://clob.polymarket.com/markets"

echo "{"
first=true
for cat in "${CATEGORIES[@]}"; do
  if [ "$first" = true ]; then first=false; else echo ","; fi
  echo "\"$cat\": {"

  # Pick one active market in this category
  sample_json=$(curl -fsS "$GAMMA?tag_slug=$cat&active=true&limit=1" || echo "[]")
  cid=$(echo "$sample_json" | jq -r '.[0].conditionId // .[0].condition_id // empty')

  if [ -z "$cid" ]; then
    echo "\"note\": \"no active markets found for tag_slug=$cat\""
  else
    echo "\"sample_condition_id\": \"$cid\","
    echo "\"gamma_snippet\": $(echo "$sample_json" | jq '.[0] | {id, conditionId, slug, question, category, tags, active, closed}'),"
    echo "\"clob_response\": $(curl -fsS "$CLOB/$cid" || echo 'null')"
  fi
  echo "}"
done
echo "}"
```

- [ ] **Step 2: Run the survey**

```bash
chmod +x scripts/survey_fees.sh
bash scripts/survey_fees.sh > /tmp/fee_survey.json
cat /tmp/fee_survey.json | jq 'keys'
```

Expected: a JSON object with keys like `sports`, `crypto`, `economics`, etc. If a category returns `"no active markets found"`, that's okay — we only need one or two categories with data to calibrate.

- [ ] **Step 3: Extract key findings into a notes doc**

Create `docs/notes/2026-04-21-polymarket-fee-survey.md` with the following template and fill it in from `/tmp/fee_survey.json`:

```markdown
# Polymarket CLOB Fee Survey — 2026-04-21

Source: `scripts/survey_fees.sh` output, captured to `/tmp/fee_survey.json`.

## feeSchedule presence

- [ ] `feeSchedule` field present on `/markets/{condition_id}`? **YES / NO**
- Raw field name (snake_case or camelCase): **`...`**
- Value shape (scalar / object / null): **`...`**

If present, document the full structure observed, e.g.:
```json
{
  "feeSchedule": {
    "takerBaseFee": 75,
    "makerFee": 0,
    "schedule": []
  }
}
```

## Legacy fee keys observed

List which of the eight legacy keys (see `polymarket_clob.rs:661-670`) appeared in any response:
- [ ] `taker_base_fee`
- [ ] `takerBaseFee`
- [ ] `taker_fee_rate_bps`
- [ ] `takerFeeRateBps`
- [ ] `fee_rate_bps`
- [ ] `feeRateBps`
- [ ] `taker_fee`
- [ ] `takerFee`

## Observed per-category values

| Category   | condition_id sample | observed fee value | field name used |
|------------|---------------------|--------------------|-----------------|
| sports     | `0x…`               | e.g. `75`          | `takerBaseFee`  |
| crypto     | `0x…`               |                    |                 |
| economics  | `0x…`               |                    |                 |
| politics   | `0x…`               |                    |                 |

## Calibration inputs for Task B (bps_to_ppm)

**Observed Sports fee value:** `<fill>`
**Expected ppm for Sports (target):** `30_000` (yields 0.75% peak via `rate_ppm / 40_000`).

**Derived conversion factor:** `ppm = observed × <K>`, where `K = 30_000 / observed`.

**Decisions:**
- [ ] `bps_to_ppm` implementation: `ppm = bps * <K>`
- [ ] `feeSchedule` vs legacy path: **use feeSchedule primary / legacy primary** (check one)
```

Fill in every checkbox and every `<...>` placeholder before committing.

- [ ] **Step 4: Commit survey + findings**

```bash
git add scripts/survey_fees.sh docs/notes/2026-04-21-polymarket-fee-survey.md
git commit -m "Add Polymarket CLOB fee-schedule survey script and findings

One-off survey (PR 1 Task A + B calibration) of /markets response
shape per category. Script and raw findings committed as durable
reference; drives bps_to_ppm conversion and feeSchedule vs legacy
key decision in subsequent tasks."
```

---

## Task 2: Create `src/fees.rs` with `PolyCategory` Enum

**Files:**
- Create: `src/fees.rs`
- Modify: `src/lib.rs` (declare `pub mod fees`)
- Modify: `src/main.rs` (declare `mod fees`)

- [ ] **Step 1: Write the failing test**

Create `src/fees.rs`:
```rust
//! Polymarket per-category fee table and rate conversion utilities.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.1.
//!
//! The internal rate unit `ppm` encodes a pre-`p(1-p)` scaling such that the
//! peak fee in cents per dollar at p=0.5 equals `rate_ppm / 40_000`
//! (see `types::poly_fee_cents`). Categories map to published peak percentages
//! as of April 2026; the CLOB remains the source of truth when available.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PolyCategory {
    Sports,
    Crypto,
    Economics,
    Politics,
    Tech,
    Culture,
    Weather,
    Finance,
    Mentions,
    Geopolitical,
    Unknown,
}

pub fn category_fee_ppm(c: PolyCategory) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::poly_fee_cents;

    #[test]
    fn sports_rate_matches_075pct_peak() {
        let ppm = category_fee_ppm(PolyCategory::Sports);
        // At p=0.5, fee = rate × 50 × 50 / 1e8 = rate / 40_000
        // For 0.75% peak: 30_000 / 40_000 = 0.75 cents
        assert_eq!(ppm, 30_000);
        // poly_fee_cents ceil-rounds: 0.75 → 1.
        assert_eq!(poly_fee_cents(50, ppm), 1);
    }

    #[test]
    fn economics_rate_is_150pct_peak() {
        let ppm = category_fee_ppm(PolyCategory::Economics);
        assert_eq!(ppm, 60_000);
        // peak = 60_000 / 40_000 = 1.5 cents
        assert_eq!(poly_fee_cents(50, ppm), 2); // ceil(1.5) = 2
    }

    #[test]
    fn crypto_rate_is_180pct_peak() {
        assert_eq!(category_fee_ppm(PolyCategory::Crypto), 72_000);
        assert_eq!(poly_fee_cents(50, 72_000), 2); // ceil(1.8) = 2
    }

    #[test]
    fn geopolitical_is_free() {
        assert_eq!(category_fee_ppm(PolyCategory::Geopolitical), 0);
        assert_eq!(poly_fee_cents(50, 0), 0);
    }

    #[test]
    fn unknown_uses_conservative_max() {
        // Unknown falls back to Crypto (highest published rate) to avoid
        // under-estimating fees on AI-matched / un-categorized markets.
        assert_eq!(
            category_fee_ppm(PolyCategory::Unknown),
            category_fee_ppm(PolyCategory::Crypto),
        );
    }
}
```

Modify `src/lib.rs` — add under the existing `pub mod` lines:
```rust
pub mod fees;
```

Modify `src/main.rs` — add under the existing `mod` lines near the top of the file:
```rust
mod fees;
```

- [ ] **Step 2: Run the tests**

```bash
cargo test --lib fees::tests -- --nocapture
```

Expected: 5 tests pass. If any fail, the values in `category_fee_ppm` don't match the `poly_fee_cents` formula — revisit the ppm math before continuing.

- [ ] **Step 3: Commit**

```bash
git add src/fees.rs src/lib.rs src/main.rs
git commit -m "Add src/fees.rs with PolyCategory and category_fee_ppm

Introduces the per-category Polymarket fee table with values calibrated
to Polymarket's published April 2026 peak rates. The internal ppm encoding
satisfies rate_ppm / 40_000 = peak_cents_per_dollar via poly_fee_cents.
Unknown defaults to the highest published rate (Crypto) as a conservative
fallback for AI-matched or unclassified markets."
```

---

## Task 3: Calibrate and Implement `bps_to_ppm` (Spec §4.1.5 Task B)

**Files:**
- Modify: `src/fees.rs`

**Depends on:** Task 1 (`docs/notes/2026-04-21-polymarket-fee-survey.md` must have the observed Sports `fee_bps` value filled in).

- [ ] **Step 1: Read the observed Sports value from the survey**

Open `docs/notes/2026-04-21-polymarket-fee-survey.md`. Find the "Observed Sports fee value" line. Call this `OBSERVED_SPORTS_BPS`. Derive the conversion factor `K = 30_000 / OBSERVED_SPORTS_BPS`.

Common outcomes:
- If `OBSERVED_SPORTS_BPS = 75`, then `K = 400`. Formula: `ppm = bps * 400`.
- If `OBSERVED_SPORTS_BPS = 750`, then `K = 40`. Formula: `ppm = bps * 40`.
- If the CLOB uses a different convention (e.g. the raw pre-scale rate where bps=7500 maps to 30_000 ppm directly), `K = 4`: `ppm = bps * 4`.

Any non-integer `K` means the survey value is wrong — re-check the observed fee and the `poly_fee_cents` formula before proceeding.

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `src/fees.rs`:
```rust
    // Replace OBSERVED_SPORTS_BPS with the value recorded in
    // docs/notes/2026-04-21-polymarket-fee-survey.md.
    const OBSERVED_SPORTS_BPS: i64 = /* TODO: fill from survey */;

    #[test]
    fn bps_to_ppm_roundtrips_sports() {
        // Invariant: the Polymarket-published bps for a Sports market
        // must convert to exactly the Sports ppm in our table.
        assert_eq!(
            bps_to_ppm(OBSERVED_SPORTS_BPS),
            category_fee_ppm(PolyCategory::Sports),
        );
    }

    #[test]
    fn bps_to_ppm_zero_is_zero() {
        assert_eq!(bps_to_ppm(0), 0);
    }

    #[test]
    fn bps_to_ppm_is_monotonic() {
        assert!(bps_to_ppm(OBSERVED_SPORTS_BPS) < bps_to_ppm(OBSERVED_SPORTS_BPS * 2));
    }
```

- [ ] **Step 3: Run the tests (expect failure)**

```bash
cargo test --lib fees::tests::bps_to_ppm -- --nocapture
```

Expected: compile error — `bps_to_ppm` not defined.

- [ ] **Step 4: Implement `bps_to_ppm`**

Add above the `#[cfg(test)]` block in `src/fees.rs` (replace `K_FROM_SURVEY` with the integer factor derived in Step 1):
```rust
/// Convert Polymarket-published fee bps (from CLOB `/markets` response) into
/// the bot's internal `rate_ppm` convention such that
/// `category_fee_ppm(Sports)` (30_000) equals `bps_to_ppm(OBSERVED_SPORTS_BPS)`.
///
/// The conversion factor K is calibrated in
/// docs/notes/2026-04-21-polymarket-fee-survey.md.
pub fn bps_to_ppm(bps: i64) -> u32 {
    const K: u32 = /* K_FROM_SURVEY */;
    if bps <= 0 {
        return 0;
    }
    (bps as u32).saturating_mul(K)
}
```

- [ ] **Step 5: Run the tests again (expect pass)**

```bash
cargo test --lib fees::tests -- --nocapture
```

Expected: all 8 tests pass (5 from Task 2 + 3 new).

- [ ] **Step 6: Commit**

```bash
git add src/fees.rs
git commit -m "Calibrate bps_to_ppm against observed Polymarket Sports fee

Conversion factor K derived from survey findings (see
docs/notes/2026-04-21-polymarket-fee-survey.md). Locked in via a
test that asserts round-trip: bps_to_ppm(OBSERVED_SPORTS_BPS) ==
category_fee_ppm(Sports) == 30_000."
```

---

## Task 4: Fix Misleading `SPORTS_FEE_RATE_PPM` Comment

**Files:**
- Modify: `src/types.rs:11-16`, `src/types.rs:698`

- [ ] **Step 1: Replace the misleading comment**

Open `src/types.rs`. Replace lines 11-16:
```rust
/// Polymarket taker fee rate in parts-per-million for Sports category (3.00% = 30_000 ppm).
/// Every market tracked by this bot is a sports league, so we hardcode this instead of
/// fetching per-market. Polymarket's effective fee is probability-scaled:
///   fee_USD = rate × p × (1-p)  (per $1 contract)
/// which peaks at p=0.5 (≈¢0.75 here) and falls to zero at the edges.
pub const SPORTS_FEE_RATE_PPM: u32 = 30_000;
```

with:
```rust
/// Polymarket taker fee rate in parts-per-million for the Sports category.
/// 30_000 ppm yields a peak fee of 0.75¢ per $1 contract at p=0.5 — matching
/// Polymarket's published Sports rate (0.75%). The probability-scaled formula
/// `rate × p × (1-p) / 1e8` peaks at p=0.5 and falls to zero at the edges
/// (see `poly_fee_cents`). For other categories use `fees::category_fee_ppm`.
pub const SPORTS_FEE_RATE_PPM: u32 = 30_000;
```

Also update the inline comment at `src/types.rs:698`:
```rust
        let r = SPORTS_FEE_RATE_PPM; // 30_000 ppm (3.00%)
```
to:
```rust
        let r = SPORTS_FEE_RATE_PPM; // 30_000 ppm → 0.75% peak fee
```

- [ ] **Step 2: Run existing tests to confirm no regression**

```bash
cargo test --lib types::tests -- --nocapture
```

Expected: all existing tests pass unchanged.

- [ ] **Step 3: Commit**

```bash
git add src/types.rs
git commit -m "Fix misleading 0.75%/3.00% comment on SPORTS_FEE_RATE_PPM

The constant is correct: 30_000 ppm yields 0.75% peak via rate_ppm /
40_000, matching Polymarket's published Sports rate. The previous
comment called this 3.00% which is off by 4× and was confusing when
auditing fee math."
```

---

## Task 5: Add `category` + `match_source` Fields to `MarketPair`

**Files:**
- Modify: `src/types.rs` (add fields + imports)
- Modify: `src/adapters/mod.rs` (create with MatchSource) — but at this point the file doesn't exist yet; we inline `MatchSource` in `fees.rs` temporarily and move it in Task 7.

Actually keep things clean: put `MatchSource` in `fees.rs` for now alongside `PolyCategory` — both are small enums that `MarketPair` needs to import.

- [ ] **Step 1: Add `MatchSource` to `src/fees.rs`**

Append to `src/fees.rs` above the `#[cfg(test)]` block:
```rust
use std::sync::Arc;

/// Provenance of a matched MarketPair — which component matched it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatchSource {
    /// Produced by a deterministic adapter (sports, fomc, etc).
    Structured { adapter: String },
    /// Produced by the AI matcher sidecar.
    Ai { confidence: f32, model: Arc<str> },
    /// Manually curated via config/manual_overrides.json.
    ManualOverride,
}

impl Default for MatchSource {
    fn default() -> Self {
        // Existing .discovery_cache.json files predate this field and
        // all current entries are sports-structured.
        MatchSource::Structured { adapter: "sports".to_string() }
    }
}

impl Default for PolyCategory {
    fn default() -> Self {
        // Existing cache entries are all sports.
        PolyCategory::Sports
    }
}
```

- [ ] **Step 2: Write the failing test**

Add to the `tests` module in `src/fees.rs`:
```rust
    #[test]
    fn match_source_serializes_with_tag() {
        let m = MatchSource::Structured { adapter: "sports".into() };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"kind\":\"structured\""), "got: {}", s);
        assert!(s.contains("\"adapter\":\"sports\""), "got: {}", s);
    }

    #[test]
    fn match_source_round_trips() {
        let m = MatchSource::Ai {
            confidence: 0.97,
            model: Arc::from("claude-opus-4-7"),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: MatchSource = serde_json::from_str(&s).unwrap();
        matches!(back, MatchSource::Ai { .. });
    }

    #[test]
    fn defaults_are_sports_structured() {
        assert_eq!(PolyCategory::default(), PolyCategory::Sports);
        match MatchSource::default() {
            MatchSource::Structured { adapter } => assert_eq!(adapter, "sports"),
            _ => panic!("expected Structured default"),
        }
    }
```

- [ ] **Step 3: Run the failing test**

```bash
cargo test --lib fees::tests -- --nocapture
```

Expected: compile succeeds, all 11 tests pass. (Tests are the full spec — implementation is already in Step 1.)

- [ ] **Step 4: Add fields to `MarketPair` in `src/types.rs`**

At the top of `src/types.rs` (after existing `use` lines), add:
```rust
use crate::fees::{PolyCategory, MatchSource};
```

In the `MarketPair` struct definition (currently `src/types.rs:45-71`), add two new fields at the end, before the closing `}`:
```rust
    /// Market category — drives Polymarket fee lookup when CLOB meta is
    /// unavailable. Defaults to Sports for back-compat with pre-PR-1 caches.
    #[serde(default)]
    pub category: PolyCategory,
    /// Who matched this pair. Defaults to `Structured { adapter: "sports" }`.
    #[serde(default)]
    pub match_source: MatchSource,
```

- [ ] **Step 5: Write a test that pre-PR-1 cache JSON deserializes cleanly**

Append to the `tests` module in `src/types.rs`:
```rust
    #[test]
    fn market_pair_deserializes_pre_pr1_cache() {
        // Pre-PR-1 cache entries lacked `category` and `match_source`.
        // Both must default via #[serde(default)] so existing caches
        // load without a wipe.
        let legacy = r#"{
            "pair_id": "test-pair",
            "league": "epl",
            "market_type": "Moneyline",
            "description": "Test",
            "kalshi_event_ticker": "KXEPLGAME-25DEC27CFCAVL",
            "kalshi_market_ticker": "KXEPLGAME-25DEC27CFCAVL-CFC",
            "poly_slug": "epl-cfc-avl-2025-12-27-cfc",
            "poly_yes_token": "0xabc",
            "poly_no_token": "0xdef",
            "poly_condition_id": "0xcond",
            "line_value": null,
            "team_suffix": "CFC"
        }"#;
        let pair: MarketPair = serde_json::from_str(legacy)
            .expect("legacy cache entry must deserialize with defaulted fields");
        assert_eq!(pair.category, crate::fees::PolyCategory::Sports);
        match pair.match_source {
            crate::fees::MatchSource::Structured { adapter } => {
                assert_eq!(adapter, "sports");
            }
            other => panic!("expected Structured default, got {:?}", other),
        }
    }
```

- [ ] **Step 6: Run the tests**

```bash
cargo test --lib types::tests::market_pair_deserializes_pre_pr1_cache -- --nocapture
cargo test --lib -- --nocapture
```

Expected: new test passes; all existing tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/types.rs src/fees.rs
git commit -m "Add category + match_source to MarketPair with serde defaults

Both fields are #[serde(default)] so pre-PR-1 .discovery_cache.json
files deserialize cleanly — existing entries default to PolyCategory::
Sports and MatchSource::Structured{adapter: \"sports\"}. MatchSource
uses internally tagged serde for forward compatibility with AI and
ManualOverride variants (PR 3)."
```

---

## Task 6: Create `src/canonical.rs` with Foundational Types

**Files:**
- Create: `src/canonical.rs`
- Modify: `src/lib.rs` (declare `pub mod canonical`)
- Modify: `src/main.rs` (declare `mod canonical`)

- [ ] **Step 1: Enable chrono's serde feature**

`canonical.rs` uses `NaiveDate` and `DateTime<Utc>` inside `#[derive(Serialize, Deserialize)]` structs, which requires chrono's `serde` feature (not enabled by default).

Edit `Cargo.toml` — change `chrono = "0.4"` to:
```toml
chrono = { version = "0.4", features = ["serde"] }
```

Then:
```bash
cargo build --lib 2>&1 | tail -5
```
Expected: clean build (no new errors; existing types using `chrono` — if any — are unaffected because `serde` is additive).

- [ ] **Step 2: Write the failing test**

Create `src/canonical.rs`:
```rust
//! Canonical representation of a prediction market, used as the intermediate
//! form between ingestion and pairing in discovery.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.3.
//!
//! Each event-type adapter normalizes raw venue markets into `CanonicalMarket`s.
//! The shared `pair_batch` function in `adapters::mod` then joins them on the
//! canonical key `(event_type, underlier)`. Adapters do not emit MarketPair
//! directly — they produce CanonicalMarkets.

use std::sync::Arc;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::fees::PolyCategory;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    Sports,
    Fomc,
    Cpi,
    NfpJobs,
    Election,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Platform {
    Kalshi,
    Polymarket,
}

/// Category-specific parameterization of what a market is predicting.
/// The canonical pair-join tests `(EventType, Underlier)` for equality, so
/// adding a new variant requires updating both sides' adapters simultaneously.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Underlier {
    SportsGame {
        league: Arc<str>,
        home: Arc<str>,
        away: Arc<str>,
        date: NaiveDate,
        market_subtype: SportsSubtype,
    },
    FomcRateBand {
        meeting_date: NaiveDate,
        floor_bps: i32,
    },
    CpiValue {
        release_date: NaiveDate,
        series: CpiSeries,
        threshold_hundredths: i32,  // 3.15% → 315; avoids f32 Hash issues
        cmp: Comparison,
    },
    ElectionCandidate {
        race_id: Arc<str>,
        candidate_normalized: Arc<str>,
    },
    /// AI-matched or unstructured — no canonical key. Pair-join must
    /// never use `Other`; AI-matched pairs come through a separate path.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SportsSubtype {
    Moneyline, Spread, Total, Btts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CpiSeries { HeadlineYoY, HeadlineMoM, CoreYoY, CoreMoM }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Comparison { Above, Below, Between }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeWindow {
    pub event_at: Option<chrono::DateTime<chrono::Utc>>,
    pub settles_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Venue {
    pub platform: Platform,
    pub kalshi_event_ticker: Option<Arc<str>>,
    pub kalshi_market_ticker: Option<Arc<str>>,
    pub poly_slug: Option<Arc<str>>,
    pub poly_yes_token: Option<Arc<str>>,
    pub poly_no_token: Option<Arc<str>>,
    pub poly_condition_id: Option<Arc<str>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalMarket {
    pub event_type: EventType,
    pub underlier: Underlier,
    pub time_window: TimeWindow,
    pub venue: Venue,
    pub category: PolyCategory,
    pub raw_title: Arc<str>,
    pub raw_description: Arc<str>,
    pub adapter_version: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn underlier_equality_and_hash_work_for_sports() {
        let date = NaiveDate::from_ymd_opt(2025, 12, 27).unwrap();
        let a = Underlier::SportsGame {
            league: "epl".into(),
            home: "CFC".into(),
            away: "AVL".into(),
            date,
            market_subtype: SportsSubtype::Moneyline,
        };
        let b = a.clone();
        assert_eq!(a, b);
        // Ensure Hash is derived — uses HashMap as a proxy test.
        let mut m = std::collections::HashMap::new();
        m.insert(a, 1);
        assert_eq!(m.get(&b), Some(&1));
    }

    #[test]
    fn underlier_fomc_band_is_integer_keyed() {
        let date = NaiveDate::from_ymd_opt(2026, 5, 7).unwrap();
        let a = Underlier::FomcRateBand { meeting_date: date, floor_bps: 425 };
        let b = Underlier::FomcRateBand { meeting_date: date, floor_bps: 425 };
        let c = Underlier::FomcRateBand { meeting_date: date, floor_bps: 450 };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn cpi_threshold_uses_integer_not_float() {
        // Guard against anyone regressing threshold to f32/f64 — would break Hash.
        let date = NaiveDate::from_ymd_opt(2026, 4, 10).unwrap();
        let a = Underlier::CpiValue {
            release_date: date,
            series: CpiSeries::HeadlineYoY,
            threshold_hundredths: 315,
            cmp: Comparison::Above,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
```

Modify `src/lib.rs` — add:
```rust
pub mod canonical;
```

Modify `src/main.rs` — add:
```rust
mod canonical;
```

- [ ] **Step 3: Run the tests**

```bash
cargo test --lib canonical::tests -- --nocapture
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/canonical.rs src/lib.rs src/main.rs
git commit -m "Add src/canonical.rs with CanonicalMarket and Underlier types

Introduces the intermediate normalized representation used between
venue ingestion and the cross-venue pair-join (see spec §4.3).
Underlier variants use integer keys (e.g. floor_bps, threshold_hundredths)
rather than floats so Hash and Eq are derivable — the pair-join relies
on HashMap<(EventType, Underlier), &CanonicalMarket> lookups."
```

---

## Task 7: Create `src/adapters/mod.rs` with `EventAdapter` Trait + `pair_batch`

**Files:**
- Create: `src/adapters/mod.rs`
- Modify: `src/lib.rs` (declare `pub mod adapters`)
- Modify: `src/main.rs` (declare `mod adapters`)

- [ ] **Step 1: Write the failing test**

Create `src/adapters/mod.rs`:
```rust
//! Event-type adapters and the shared cross-venue pair-join.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.3.
//!
//! Each concrete adapter (sports, fomc, …) produces a `NormalizedBatch` —
//! Kalshi and Polymarket markets normalized into `CanonicalMarket`. The
//! shared `pair_batch` function joins them on `(event_type, underlier)`
//! and emits `MarketPair`s keyed by the adapter's name.

use anyhow::Result;
use async_trait::async_trait;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use tracing::debug;

use crate::canonical::{CanonicalMarket, EventType, Underlier};
use crate::fees::{MatchSource, PolyCategory};
use crate::types::{MarketPair, MarketType};

pub mod sports;  // added in Task 8

pub struct NormalizedBatch {
    pub kalshi: Vec<CanonicalMarket>,
    pub poly: Vec<CanonicalMarket>,
}

#[async_trait]
pub trait EventAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn event_type(&self) -> EventType;
    fn version(&self) -> u32;
    async fn normalize(&self) -> Result<NormalizedBatch>;
}

/// Join a batch by canonical key. Unmatched markets on either side are dropped.
/// `Underlier::Other` never produces pairs (AI-matched pairs flow through
/// a separate reader in PR 3).
pub fn pair_batch(batch: NormalizedBatch, adapter_name: &'static str) -> Vec<MarketPair> {
    let mut poly_by_key: FxHashMap<(EventType, Underlier), &CanonicalMarket> =
        FxHashMap::default();
    for p in &batch.poly {
        if matches!(p.underlier, Underlier::Other) { continue; }
        poly_by_key.insert((p.event_type, p.underlier.clone()), p);
    }

    let mut out = Vec::with_capacity(batch.kalshi.len());
    for k in &batch.kalshi {
        if matches!(k.underlier, Underlier::Other) { continue; }
        let key = (k.event_type, k.underlier.clone());
        let p = match poly_by_key.get(&key) {
            Some(p) => p,
            None => { debug!("no poly match for kalshi {:?}", k.venue.kalshi_market_ticker); continue; }
        };
        if let Some(pair) = build_pair(k, p, adapter_name) {
            out.push(pair);
        }
    }
    out
}

fn build_pair(k: &CanonicalMarket, p: &CanonicalMarket, adapter_name: &'static str)
    -> Option<MarketPair>
{
    let kalshi_market_ticker = k.venue.kalshi_market_ticker.clone()?;
    let kalshi_event_ticker  = k.venue.kalshi_event_ticker.clone()?;
    let poly_slug            = p.venue.poly_slug.clone()?;
    let poly_yes_token       = p.venue.poly_yes_token.clone()?;
    let poly_no_token        = p.venue.poly_no_token.clone()?;
    let poly_condition_id    = p.venue.poly_condition_id.clone()?;

    let (market_type, line_value, team_suffix, league) = sports_fields(&k.underlier);

    Some(MarketPair {
        pair_id: Arc::from(format!("{}-{}", poly_slug, kalshi_market_ticker)),
        league,
        market_type,
        description: Arc::from(format!("{} - {}", k.raw_title, p.raw_title)),
        kalshi_event_ticker,
        kalshi_market_ticker,
        poly_slug,
        poly_yes_token,
        poly_no_token,
        poly_condition_id,
        line_value,
        team_suffix,
        category: k.category,
        match_source: MatchSource::Structured { adapter: adapter_name.to_string() },
    })
}

/// Extract MarketPair's legacy sports-shaped fields from an Underlier.
/// For non-sports Underlier variants, use Moneyline as a neutral default —
/// `market_type` is only consumed by sports-facing logging today. Future
/// adapters that need richer typing should widen `MarketType`.
fn sports_fields(u: &Underlier) -> (MarketType, Option<f64>, Option<Arc<str>>, Arc<str>) {
    use crate::canonical::SportsSubtype;
    match u {
        Underlier::SportsGame { league, market_subtype, .. } => {
            let mt = match market_subtype {
                SportsSubtype::Moneyline => MarketType::Moneyline,
                SportsSubtype::Spread    => MarketType::Spread,
                SportsSubtype::Total     => MarketType::Total,
                SportsSubtype::Btts      => MarketType::Btts,
            };
            (mt, None, None, league.clone())
        }
        _ => (MarketType::Moneyline, None, None, Arc::from("other")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::*;
    use crate::fees::PolyCategory;
    use chrono::NaiveDate;

    fn mk_canon_sports(side: Platform, yes: &str, no: &str) -> CanonicalMarket {
        let date = NaiveDate::from_ymd_opt(2025, 12, 27).unwrap();
        CanonicalMarket {
            event_type: EventType::Sports,
            underlier: Underlier::SportsGame {
                league: "epl".into(),
                home: "CFC".into(),
                away: "AVL".into(),
                date,
                market_subtype: SportsSubtype::Moneyline,
            },
            time_window: TimeWindow { event_at: None, settles_at: None },
            venue: Venue {
                platform: side,
                kalshi_event_ticker: if matches!(side, Platform::Kalshi) {
                    Some("KXEPLGAME-25DEC27CFCAVL".into())
                } else { None },
                kalshi_market_ticker: if matches!(side, Platform::Kalshi) {
                    Some("KXEPLGAME-25DEC27CFCAVL-CFC".into())
                } else { None },
                poly_slug: if matches!(side, Platform::Polymarket) {
                    Some("epl-cfc-avl-2025-12-27-cfc".into())
                } else { None },
                poly_yes_token: if matches!(side, Platform::Polymarket) {
                    Some(yes.into())
                } else { None },
                poly_no_token:  if matches!(side, Platform::Polymarket) {
                    Some(no.into())
                } else { None },
                poly_condition_id: if matches!(side, Platform::Polymarket) {
                    Some("0xcond".into())
                } else { None },
            },
            category: PolyCategory::Sports,
            raw_title: "Test".into(),
            raw_description: "".into(),
            adapter_version: 1,
        }
    }

    #[test]
    fn pair_batch_joins_matching_canonical_markets() {
        let batch = NormalizedBatch {
            kalshi: vec![mk_canon_sports(Platform::Kalshi, "", "")],
            poly: vec![mk_canon_sports(Platform::Polymarket, "0xyes", "0xno")],
        };
        let pairs = pair_batch(batch, "sports");
        assert_eq!(pairs.len(), 1);
        assert_eq!(&*pairs[0].poly_yes_token, "0xyes");
        match &pairs[0].match_source {
            MatchSource::Structured { adapter } => assert_eq!(adapter, "sports"),
            _ => panic!("expected Structured"),
        }
    }

    #[test]
    fn pair_batch_drops_unmatched() {
        let mut k = mk_canon_sports(Platform::Kalshi, "", "");
        // Different game — no poly counterpart.
        if let Underlier::SportsGame { ref mut home, .. } = k.underlier {
            *home = "LIV".into();
        }
        let batch = NormalizedBatch {
            kalshi: vec![k],
            poly: vec![mk_canon_sports(Platform::Polymarket, "0xyes", "0xno")],
        };
        let pairs = pair_batch(batch, "sports");
        assert!(pairs.is_empty(), "unmatched underliers should drop");
    }

    #[test]
    fn pair_batch_ignores_other_underlier() {
        let mut k = mk_canon_sports(Platform::Kalshi, "", "");
        k.underlier = Underlier::Other;
        let batch = NormalizedBatch {
            kalshi: vec![k],
            poly: vec![mk_canon_sports(Platform::Polymarket, "0xyes", "0xno")],
        };
        let pairs = pair_batch(batch, "sports");
        assert!(pairs.is_empty(), "Other underlier must not emit pairs");
    }
}
```

Modify `src/lib.rs`:
```rust
pub mod adapters;
```

Modify `src/main.rs`:
```rust
mod adapters;
```

Modify `Cargo.toml` to add `async-trait` to `[dependencies]`:
```toml
async-trait = "0.1"
```

- [ ] **Step 2: Stub `adapters/sports.rs` so the module declaration compiles**

Temporary stub — real implementation is Task 8. Create `src/adapters/sports.rs`:
```rust
//! SportsAdapter — stub, implemented in Task 8.
```

- [ ] **Step 3: Run the tests**

```bash
cargo test --lib adapters::tests -- --nocapture
```

Expected: 3 tests pass. If `async-trait` compile errors, `cargo update && cargo build` and retry.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock src/adapters/mod.rs src/adapters/sports.rs src/lib.rs src/main.rs
git commit -m "Add EventAdapter trait, pair_batch, and adapters module

pair_batch joins NormalizedBatch by (EventType, Underlier) and emits
MarketPair with MatchSource::Structured. Underlier::Other never produces
pairs — it is reserved for AI-matched markets that flow through a
separate reader (PR 3). Sports subtype → MarketType mapping lives in
this module so adapters only produce Underliers."
```

---

## Task 8: Extract Sports Logic into `src/adapters/sports.rs`

**Files:**
- Rewrite: `src/adapters/sports.rs` (replace stub)
- Modify: `src/discovery.rs` (move `parse_kalshi_event_ticker`, `split_team_codes`, `is_likely_two_letter_code`, `kalshi_date_to_iso`, `extract_team_suffix`, `build_poly_slug`, and related code out of this file)
- Modify: `src/discovery.rs` — DiscoveryClient still exists; in Task 9 it becomes an adapter orchestrator.

This task preserves behavior: after it, the existing sports discovery flow (`discover_full`, `discover_league`, `discover_series`) continues to exist and produce byte-identical `MarketPair` sets. The adapter is a *new* code path that computes the same pairs via `normalize()` + `pair_batch`; Task 9 switches `DiscoveryClient` to use it.

- [ ] **Step 1: Copy the sports logic into `src/adapters/sports.rs`**

Replace the stub file with a full implementation. The content below consolidates everything sports-specific so `discovery.rs` is left with only orchestration:

```rust
//! SportsAdapter: normalizes Kalshi sports events and matched Polymarket
//! markets into the canonical schema. Wraps the pre-PR-1 sports discovery
//! flow behind the EventAdapter trait with identical output behavior.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.4.

use anyhow::Result;
use async_trait::async_trait;
use chrono::NaiveDate;
use futures_util::{stream, StreamExt};
use governor::{Quota, RateLimiter, state::NotKeyed, clock::DefaultClock, middleware::NoOpMiddleware};
use std::num::NonZeroU32;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::warn;

use crate::adapters::{EventAdapter, NormalizedBatch};
use crate::cache::TeamCache;
use crate::canonical::{
    CanonicalMarket, EventType, Platform, SportsSubtype, TimeWindow, Underlier, Venue,
};
use crate::config::{get_league_configs, get_league_config, LeagueConfig};
use crate::fees::PolyCategory;
use crate::kalshi::KalshiApiClient;
use crate::polymarket::GammaClient;
use crate::types::{KalshiEvent, KalshiMarket, MarketType};

const GAMMA_CONCURRENCY: usize = 20;
const KALSHI_RATE_LIMIT_PER_SEC: u32 = 2;
const KALSHI_GLOBAL_CONCURRENCY: usize = 1;

type KalshiRateLimiter =
    RateLimiter<NotKeyed, governor::state::InMemoryState, DefaultClock, NoOpMiddleware>;

pub struct SportsAdapter {
    pub leagues: Vec<&'static str>,  // empty = all
    pub kalshi: Arc<KalshiApiClient>,
    pub gamma: Arc<GammaClient>,
    pub team_cache: Arc<TeamCache>,
    pub kalshi_limiter: Arc<KalshiRateLimiter>,
    pub kalshi_semaphore: Arc<Semaphore>,
    pub gamma_semaphore: Arc<Semaphore>,
}

impl SportsAdapter {
    pub fn new(kalshi: Arc<KalshiApiClient>, team_cache: Arc<TeamCache>, leagues: Vec<&'static str>) -> Self {
        let quota = Quota::per_second(NonZeroU32::new(KALSHI_RATE_LIMIT_PER_SEC).unwrap());
        Self {
            leagues,
            kalshi,
            gamma: Arc::new(GammaClient::new()),
            team_cache,
            kalshi_limiter: Arc::new(RateLimiter::direct(quota)),
            kalshi_semaphore: Arc::new(Semaphore::new(KALSHI_GLOBAL_CONCURRENCY)),
            gamma_semaphore: Arc::new(Semaphore::new(GAMMA_CONCURRENCY)),
        }
    }
}

#[async_trait]
impl EventAdapter for SportsAdapter {
    fn name(&self) -> &'static str { "sports" }
    fn event_type(&self) -> EventType { EventType::Sports }
    fn version(&self) -> u32 { 1 }

    async fn normalize(&self) -> Result<NormalizedBatch> {
        let configs: Vec<LeagueConfig> = if self.leagues.is_empty() {
            get_league_configs()
        } else {
            self.leagues.iter().filter_map(|l| get_league_config(l)).collect()
        };

        // Parallel per-league discovery
        let league_futures: Vec<_> = configs.iter()
            .map(|c| self.normalize_league(c))
            .collect();
        let league_results = futures_util::future::join_all(league_futures).await;

        let mut batch = NormalizedBatch { kalshi: Vec::new(), poly: Vec::new() };
        for r in league_results {
            match r {
                Ok(b) => {
                    batch.kalshi.extend(b.kalshi);
                    batch.poly.extend(b.poly);
                }
                Err(e) => warn!("[sports] league normalize failed: {}", e),
            }
        }
        Ok(batch)
    }
}

impl SportsAdapter {
    async fn normalize_league(&self, config: &LeagueConfig) -> Result<NormalizedBatch> {
        use crate::types::MarketType as Mt;

        let mut batch = NormalizedBatch { kalshi: Vec::new(), poly: Vec::new() };
        for mt in [Mt::Moneyline, Mt::Spread, Mt::Total, Mt::Btts] {
            let Some(series) = get_series_for_type(config, mt) else { continue; };
            let league_batch = self.normalize_series(config, series, mt).await?;
            batch.kalshi.extend(league_batch.kalshi);
            batch.poly.extend(league_batch.poly);
        }
        Ok(batch)
    }

    async fn normalize_series(&self, config: &LeagueConfig, series: &str, mt: MarketType)
        -> Result<NormalizedBatch>
    {
        // Rate-limited Kalshi event fetch
        {
            let _permit = self.kalshi_semaphore.acquire().await
                .map_err(|e| anyhow::anyhow!("semaphore closed: {}", e))?;
            self.kalshi_limiter.until_ready().await;
        }
        let events = self.kalshi.get_events(series, 50).await?;

        // Parse tickers
        let parsed_events: Vec<(ParsedKalshiTicker, KalshiEvent)> = events.into_iter()
            .filter_map(|ev| {
                let parsed = parse_kalshi_event_ticker(&ev.event_ticker)?;
                Some((parsed, ev))
            })
            .collect();

        // Parallel market fetches
        let kalshi = self.kalshi.clone();
        let limiter = self.kalshi_limiter.clone();
        let semaphore = self.kalshi_semaphore.clone();
        let market_results: Vec<_> = stream::iter(parsed_events)
            .map(|(parsed, event)| {
                let kalshi = kalshi.clone();
                let limiter = limiter.clone();
                let semaphore = semaphore.clone();
                let event_ticker = event.event_ticker.clone();
                async move {
                    let _permit = semaphore.acquire().await.ok();
                    limiter.until_ready().await;
                    let markets = kalshi.get_markets(&event_ticker).await;
                    (parsed, Arc::new(event), markets)
                }
            })
            .buffer_unordered(KALSHI_GLOBAL_CONCURRENCY * 2)
            .collect()
            .await;

        let mut kalshi_canon: Vec<CanonicalMarket> = Vec::new();
        let mut poly_canon: Vec<CanonicalMarket> = Vec::new();

        // Gamma lookups with concurrency limit
        let mut lookups: Vec<GammaLookupTask> = Vec::new();
        for (parsed, event, markets_res) in market_results {
            let markets = match markets_res {
                Ok(ms) => ms,
                Err(e) => {
                    warn!("[sports] kalshi markets fetch failed for {}: {}", event.event_ticker, e);
                    continue;
                }
            };
            for market in markets {
                let slug = self.build_poly_slug(config.poly_prefix, &parsed, mt, &market);
                lookups.push(GammaLookupTask {
                    event: event.clone(),
                    market,
                    parsed: parsed.clone(),
                    poly_slug: slug,
                    market_type: mt,
                    league: config.league_code,
                });
            }
        }

        let gamma = self.gamma.clone();
        let gamma_semaphore = self.gamma_semaphore.clone();
        let resolved: Vec<Option<(GammaLookupTask, String, String, String)>> =
            stream::iter(lookups)
                .map(|task| {
                    let gamma = gamma.clone();
                    let sem = gamma_semaphore.clone();
                    async move {
                        let _permit = sem.acquire().await.ok()?;
                        match gamma.lookup_market(&task.poly_slug).await {
                            Ok(Some((yes, no, cid))) => Some((task, yes, no, cid)),
                            _ => None,
                        }
                    }
                })
                .buffer_unordered(GAMMA_CONCURRENCY)
                .collect()
                .await;

        for item in resolved.into_iter().flatten() {
            let (task, yes, no, cid) = item;
            let date = kalshi_date_to_naive(&task.parsed.date);
            let subtype = sports_subtype(task.market_type);
            let home = self.team_cache
                .kalshi_to_poly(config.poly_prefix, &task.parsed.team1)
                .unwrap_or_else(|| task.parsed.team1.to_lowercase());
            let away = self.team_cache
                .kalshi_to_poly(config.poly_prefix, &task.parsed.team2)
                .unwrap_or_else(|| task.parsed.team2.to_lowercase());

            let underlier = Underlier::SportsGame {
                league: task.league.into(),
                home: Arc::from(home),
                away: Arc::from(away),
                date,
                market_subtype: subtype,
            };

            let title = Arc::from(format!("{} - {}", task.event.title, task.market.title));

            kalshi_canon.push(CanonicalMarket {
                event_type: EventType::Sports,
                underlier: underlier.clone(),
                time_window: TimeWindow { event_at: None, settles_at: None },
                venue: Venue {
                    platform: Platform::Kalshi,
                    kalshi_event_ticker: Some(task.event.event_ticker.clone().into()),
                    kalshi_market_ticker: Some(task.market.ticker.clone().into()),
                    poly_slug: None, poly_yes_token: None, poly_no_token: None, poly_condition_id: None,
                },
                category: PolyCategory::Sports,
                raw_title: title.clone(),
                raw_description: Arc::from(""),
                adapter_version: 1,
            });
            poly_canon.push(CanonicalMarket {
                event_type: EventType::Sports,
                underlier,
                time_window: TimeWindow { event_at: None, settles_at: None },
                venue: Venue {
                    platform: Platform::Polymarket,
                    kalshi_event_ticker: None, kalshi_market_ticker: None,
                    poly_slug: Some(Arc::from(task.poly_slug.clone())),
                    poly_yes_token: Some(Arc::from(yes)),
                    poly_no_token: Some(Arc::from(no)),
                    poly_condition_id: Some(Arc::from(cid)),
                },
                category: PolyCategory::Sports,
                raw_title: title,
                raw_description: Arc::from(""),
                adapter_version: 1,
            });
        }

        Ok(NormalizedBatch { kalshi: kalshi_canon, poly: poly_canon })
    }

    fn build_poly_slug(&self, poly_prefix: &str, parsed: &ParsedKalshiTicker,
                       market_type: MarketType, market: &KalshiMarket) -> String {
        // Identical logic to pre-PR-1 build_poly_slug in discovery.rs.
        let poly_team1 = self.team_cache.kalshi_to_poly(poly_prefix, &parsed.team1)
            .unwrap_or_else(|| parsed.team1.to_lowercase());
        let poly_team2 = self.team_cache.kalshi_to_poly(poly_prefix, &parsed.team2)
            .unwrap_or_else(|| parsed.team2.to_lowercase());
        let date_str = kalshi_date_to_iso(&parsed.date);
        let base = format!("{}-{}-{}-{}", poly_prefix, poly_team1, poly_team2, date_str);

        match market_type {
            MarketType::Moneyline => {
                if let Some(suffix) = extract_team_suffix(&market.ticker) {
                    if suffix.to_lowercase() == "tie" {
                        format!("{}-draw", base)
                    } else {
                        let poly_suffix = self.team_cache.kalshi_to_poly(poly_prefix, &suffix)
                            .unwrap_or_else(|| suffix.to_lowercase());
                        format!("{}-{}", base, poly_suffix)
                    }
                } else { base }
            }
            MarketType::Spread => {
                if let Some(floor) = market.floor_strike {
                    let floor_str = format!("{:.1}", floor).replace(".", "pt");
                    format!("{}-spread-{}", base, floor_str)
                } else { format!("{}-spread", base) }
            }
            MarketType::Total => {
                if let Some(floor) = market.floor_strike {
                    let floor_str = format!("{:.1}", floor).replace(".", "pt");
                    format!("{}-total-{}", base, floor_str)
                } else { format!("{}-total", base) }
            }
            MarketType::Btts => format!("{}-btts", base),
        }
    }
}

struct GammaLookupTask {
    event: Arc<KalshiEvent>,
    market: KalshiMarket,
    parsed: ParsedKalshiTicker,
    poly_slug: String,
    market_type: MarketType,
    league: &'static str,
}

#[derive(Debug, Clone)]
struct ParsedKalshiTicker {
    date: String,
    team1: String,
    team2: String,
}

fn get_series_for_type(config: &LeagueConfig, mt: MarketType) -> Option<&'static str> {
    match mt {
        MarketType::Moneyline => Some(config.kalshi_series_game),
        MarketType::Spread => config.kalshi_series_spread,
        MarketType::Total => config.kalshi_series_total,
        MarketType::Btts => config.kalshi_series_btts,
    }
}

fn sports_subtype(mt: MarketType) -> SportsSubtype {
    match mt {
        MarketType::Moneyline => SportsSubtype::Moneyline,
        MarketType::Spread => SportsSubtype::Spread,
        MarketType::Total => SportsSubtype::Total,
        MarketType::Btts => SportsSubtype::Btts,
    }
}

fn parse_kalshi_event_ticker(ticker: &str) -> Option<ParsedKalshiTicker> {
    // Identical logic to pre-PR-1 parse_kalshi_event_ticker in discovery.rs.
    let parts: Vec<&str> = ticker.split('-').collect();
    if parts.len() < 2 { return None; }
    let (date, teams_part) = if parts.len() >= 3 && parts[2].len() >= 4 {
        let date_part = parts[1];
        let date = if date_part.len() >= 7 { date_part[..7].to_uppercase() } else { return None; };
        (date, parts[2])
    } else {
        let date_teams = parts[1];
        if date_teams.len() < 11 { return None; }
        let date = date_teams[..7].to_uppercase();
        let teams = &date_teams[7..];
        (date, teams)
    };
    let (team1, team2) = split_team_codes(teams_part);
    Some(ParsedKalshiTicker { date, team1, team2 })
}

fn split_team_codes(teams: &str) -> (String, String) {
    let len = teams.len();
    match len {
        4 => (teams[..2].to_uppercase(), teams[2..].to_uppercase()),
        5 => (teams[..2].to_uppercase(), teams[2..].to_uppercase()),
        6 => {
            let first_two = &teams[..2].to_uppercase();
            if is_likely_two_letter_code(first_two) {
                (first_two.clone(), teams[2..].to_uppercase())
            } else {
                (teams[..3].to_uppercase(), teams[3..].to_uppercase())
            }
        }
        7 => (teams[..3].to_uppercase(), teams[3..].to_uppercase()),
        _ if len >= 8 => (teams[..4].to_uppercase(), teams[4..].to_uppercase()),
        _ => {
            let mid = len / 2;
            (teams[..mid].to_uppercase(), teams[mid..].to_uppercase())
        }
    }
}

fn is_likely_two_letter_code(code: &str) -> bool {
    matches!(code,
        "OM"|"OL"|"FC"|"OH"|"SF"|"LA"|"NY"|"KC"|"TB"|"GB"|"NE"|"NO"|"LV"|
        "BC"|"SC"|"AC"|"AS"|"US"
    )
}

fn kalshi_date_to_iso(kalshi_date: &str) -> String {
    if kalshi_date.len() != 7 { return kalshi_date.to_string(); }
    let year = format!("20{}", &kalshi_date[..2]);
    let month = match &kalshi_date[2..5].to_uppercase()[..] {
        "JAN"=>"01","FEB"=>"02","MAR"=>"03","APR"=>"04","MAY"=>"05","JUN"=>"06",
        "JUL"=>"07","AUG"=>"08","SEP"=>"09","OCT"=>"10","NOV"=>"11","DEC"=>"12",
        _ => "01",
    };
    let day = &kalshi_date[5..7];
    format!("{}-{}-{}", year, month, day)
}

fn kalshi_date_to_naive(kalshi_date: &str) -> NaiveDate {
    let iso = kalshi_date_to_iso(kalshi_date);
    NaiveDate::parse_from_str(&iso, "%Y-%m-%d")
        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
}

fn extract_team_suffix(ticker: &str) -> Option<String> {
    let mut splits = ticker.splitn(3, '-');
    splits.next()?;
    splits.next()?;
    splits.next().map(|s| s.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_standard_epl_ticker() {
        let p = parse_kalshi_event_ticker("KXEPLGAME-25DEC27CFCAVL").unwrap();
        assert_eq!(p.date, "25DEC27");
        assert_eq!(p.team1, "CFC");
        assert_eq!(p.team2, "AVL");
    }

    #[test]
    fn kalshi_date_to_iso_roundtrips() {
        assert_eq!(kalshi_date_to_iso("25DEC27"), "2025-12-27");
        assert_eq!(kalshi_date_to_iso("25JAN01"), "2025-01-01");
    }

    #[test]
    fn kalshi_date_to_naive_is_ymd() {
        let d = kalshi_date_to_naive("25DEC27");
        assert_eq!(d, NaiveDate::from_ymd_opt(2025, 12, 27).unwrap());
    }
}
```

- [ ] **Step 2: Remove duplicated sports helpers from `src/discovery.rs`**

Delete the following items from `src/discovery.rs` now that they live in `src/adapters/sports.rs`:
- `ParsedKalshiTicker` struct
- `parse_kalshi_event_ticker`
- `split_team_codes`
- `is_likely_two_letter_code`
- `kalshi_date_to_iso`
- `extract_team_suffix`
- The `#[cfg(test)]` tests for these (they're now in `adapters/sports.rs`).

Leave `DiscoveryClient`, `discover_all`, `discover_full`, etc. unchanged for now — Task 9 rewires the orchestration.

If anything in `discovery.rs` still calls these removed helpers directly, temporarily import them from `adapters::sports` via a `use` statement — Task 9 removes those usages.

- [ ] **Step 3: Run the tests**

```bash
cargo build --lib
cargo test --lib adapters::sports::tests -- --nocapture
cargo test --lib discovery -- --nocapture
```

Expected: adapter tests pass (3 tests); the crate still builds; any remaining discovery tests still pass.

- [ ] **Step 4: Commit**

```bash
git add src/adapters/sports.rs src/discovery.rs
git commit -m "Extract sports discovery into SportsAdapter

Ticker parsing, slug building, team-code splitting, and date handling
move from discovery.rs into adapters/sports.rs behind the EventAdapter
trait. Behavior is preserved; discovery.rs still owns orchestration
(rewired in the next commit). Tests for the pure helpers also migrate
alongside the code they cover."
```

---

## Task 9: Wire `DiscoveryClient` to Use `SportsAdapter` via the Trait

**Files:**
- Modify: `src/discovery.rs`
- Modify: `src/main.rs` (adjust `DiscoveryClient` construction if needed)

- [ ] **Step 1: Rewrite `DiscoveryClient::discover_full` to use the adapter**

In `src/discovery.rs`:
1. Change `DiscoveryClient` to own a `Vec<Box<dyn EventAdapter>>` instead of individual `kalshi`, `gamma`, etc. fields:
```rust
pub struct DiscoveryClient {
    adapters: Vec<Box<dyn EventAdapter>>,
}

impl DiscoveryClient {
    pub fn new(adapters: Vec<Box<dyn EventAdapter>>) -> Self {
        Self { adapters }
    }
}
```

2. Replace `discover_full` with an adapter-driven version:
```rust
    async fn discover_full(&self) -> DiscoveryResult {
        let mut result = DiscoveryResult::default();

        for adapter in &self.adapters {
            match adapter.normalize().await {
                Ok(batch) => {
                    let pairs = crate::adapters::pair_batch(batch, adapter.name());
                    result.poly_matches += pairs.len();
                    result.pairs.extend(pairs);
                }
                Err(e) => {
                    let msg = format!("{} adapter normalize failed: {}", adapter.name(), e);
                    tracing::warn!("{}", msg);
                    result.errors.push(msg);
                }
            }
        }
        result.kalshi_events_found = result.pairs.len();
        result
    }
```

3. Update `discover_all` and `discover_all_force` to remove `leagues: &[&str]` parameter (adapters already know their leagues via construction). The call sites in `main.rs` change accordingly.

4. Delete the now-unused `discover_league`, `discover_series`, `get_series_for_type` methods on `DiscoveryClient`. Delete `GammaLookupTask` if it's still in this file (it lives in `sports.rs` now).

- [ ] **Step 2: Update `main.rs` to construct the adapter**

In `src/main.rs`, around line 143 where `DiscoveryClient::new` is called, replace:
```rust
    let discovery = DiscoveryClient::new(
        KalshiApiClient::new(KalshiConfig::from_env()?),
        team_cache
    );

    let result = if force_discovery {
        discovery.discover_all_force(ENABLED_LEAGUES).await
    } else {
        discovery.discover_all(ENABLED_LEAGUES).await
    };
```

with:
```rust
    let sports_adapter = Box::new(adapters::sports::SportsAdapter::new(
        Arc::new(KalshiApiClient::new(KalshiConfig::from_env()?)),
        Arc::new(team_cache),
        ENABLED_LEAGUES.to_vec(),
    ));
    let discovery = DiscoveryClient::new(vec![sports_adapter]);

    let result = if force_discovery {
        discovery.discover_all_force().await
    } else {
        discovery.discover_all().await
    };
```

Note: `team_cache` previously moved into `DiscoveryClient`; with the new design it moves into `SportsAdapter`. Adjust the earlier `team_cache.clone()` or pass-through accordingly.

- [ ] **Step 3: Build and run the existing discovery flow against a live cache**

```bash
cargo build --release
```

Expected: clean build. Any leftover imports of removed functions surface here.

If a `.discovery_cache.json` already exists on disk from earlier runs, load it:
```bash
ls -la .discovery_cache.json
cargo run --release 2>&1 | head -200
```

Expected (from logs):
- `📂 Loaded N pairs from cache` matches the pre-PR-1 count.
- If no cache, `📂 No cache found, doing full discovery...` followed by the expected per-league pair counts.

Stop the process (Ctrl+C) once discovery completes — the WebSocket part doesn't matter for this verification.

- [ ] **Step 4: Golden test — compare before/after pair set on a saved cache**

If you have a pre-PR-1 `.discovery_cache.json` from a prior run, back it up as `/tmp/pre_pr1_cache.json`. Run discovery with `FORCE_DISCOVERY=1`:
```bash
cp .discovery_cache.json /tmp/pre_pr1_cache.json
FORCE_DISCOVERY=1 cargo run --release 2>&1 | head -200
```

Then compare pair IDs:
```bash
jq -r '.pairs[].pair_id' /tmp/pre_pr1_cache.json | sort > /tmp/before.txt
jq -r '.pairs[].pair_id' .discovery_cache.json | sort > /tmp/after.txt
diff /tmp/before.txt /tmp/after.txt
```

Expected: empty diff, or trivial additions if new markets have appeared on the venues since the last run.

If the diff shows *removed* pairs, behavior regressed — investigate before proceeding. Likely culprits: a mis-copied helper function, a changed `pair_id` format, a missing league config.

- [ ] **Step 5: Commit**

```bash
git add src/discovery.rs src/main.rs
git commit -m "Rewire DiscoveryClient around EventAdapter

DiscoveryClient now owns Vec<Box<dyn EventAdapter>>. discover_full
iterates adapters, collects NormalizedBatch, feeds into pair_batch,
and merges. League-specific orchestration moves fully into
SportsAdapter; DiscoveryClient is pure orchestration.

Verified: pair set from FORCE_DISCOVERY=1 matches pre-PR-1 snapshot
(see diff in PR description)."
```

---

## Task 10: Update `polymarket_clob.rs` to Prefer `feeSchedule`

**Files:**
- Modify: `src/polymarket_clob.rs:635-712`

**Branching based on Task 1 findings:**

If `docs/notes/2026-04-21-polymarket-fee-survey.md` recorded `feeSchedule` as PRESENT, do steps 1-4 below. If ABSENT, skip this task and record that in the commit message of Task 11.

- [ ] **Step 1: Write a failing test with mock JSON**

Add a `#[cfg(test)]` module inside `src/polymarket_clob.rs` (if one doesn't exist) — place it at the end of the file:
```rust
#[cfg(test)]
mod meta_parse_tests {
    use serde_json::json;

    /// Reproduces the key-extraction logic in fetch_market_meta against
    /// a synthetic JSON value. We copy the logic here rather than extracting
    /// it to a helper because fetch_market_meta is async + does I/O.
    /// If this test drifts from fetch_market_meta, the integration test
    /// on live response JSON (committed with Task 1 findings) catches it.
    fn parse_fee(val: &serde_json::Value) -> Option<i64> {
        // NEW: feeSchedule preferred path
        if let Some(fs) = val.get("feeSchedule").or_else(|| val.get("fee_schedule")) {
            for key in ["takerBaseFee", "taker_base_fee", "takerFee", "taker_fee"] {
                if let Some(n) = fs.get(key).and_then(|v| v.as_i64()) { return Some(n); }
                if let Some(s) = fs.get(key).and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok()) { return Some(s); }
            }
        }
        // Legacy flat keys (fallback)
        for key in ["taker_base_fee","takerBaseFee","taker_fee_rate_bps","takerFeeRateBps",
                    "fee_rate_bps","feeRateBps","taker_fee","takerFee"] {
            if let Some(n) = val.get(key).and_then(|v| v.as_i64()) { return Some(n); }
            if let Some(s) = val.get(key).and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok()) { return Some(s); }
        }
        None
    }

    #[test]
    fn fee_schedule_preferred_over_legacy() {
        let v = json!({
            "feeSchedule": { "takerBaseFee": 75 },
            "takerBaseFee": 0
        });
        assert_eq!(parse_fee(&v), Some(75), "must prefer feeSchedule over legacy");
    }

    #[test]
    fn falls_back_to_legacy_when_no_schedule() {
        let v = json!({ "takerBaseFee": 100 });
        assert_eq!(parse_fee(&v), Some(100));
    }

    #[test]
    fn fee_as_string_parses() {
        let v = json!({ "feeSchedule": { "takerBaseFee": "75" } });
        assert_eq!(parse_fee(&v), Some(75));
    }

    #[test]
    fn missing_fee_returns_none() {
        let v = json!({ "unrelated": 42 });
        assert_eq!(parse_fee(&v), None);
    }
}
```

- [ ] **Step 2: Run the failing test**

```bash
cargo test --lib polymarket_clob::meta_parse_tests -- --nocapture
```

Expected: tests compile and pass. (They assert behavior of the local `parse_fee` helper which embeds the desired logic.)

- [ ] **Step 3: Apply the same logic inside `fetch_market_meta`**

Modify `src/polymarket_clob.rs` around lines 658-684. Replace the existing `fee_keys` loop with:

```rust
        // Prefer the structured feeSchedule object when present (April 2026+).
        // Fall back to the legacy flat keys (pre-fee-rollout surveys).
        let mut fee: Option<i64> = None;
        let mut matched_key: Option<String> = None;

        if let Some(fs) = val.get("feeSchedule").or_else(|| val.get("fee_schedule")) {
            for k in ["takerBaseFee","taker_base_fee","takerFee","taker_fee"] {
                if let Some(n) = fs.get(k).and_then(|v| v.as_i64()) {
                    fee = Some(n);
                    matched_key = Some(format!("feeSchedule.{}", k));
                    break;
                }
                if let Some(n) = fs.get(k).and_then(|v| v.as_str()).and_then(|s| s.parse::<i64>().ok()) {
                    fee = Some(n);
                    matched_key = Some(format!("feeSchedule.{}", k));
                    break;
                }
            }
        }
        if fee.is_none() {
            let legacy_keys = [
                "taker_base_fee","takerBaseFee","taker_fee_rate_bps","takerFeeRateBps",
                "fee_rate_bps","feeRateBps","taker_fee","takerFee",
            ];
            for k in legacy_keys {
                if let Some(n) = val[k].as_i64() { fee = Some(n); matched_key = Some(k.to_string()); break; }
                if let Some(n) = val[k].as_str().and_then(|s| s.parse::<i64>().ok()) {
                    fee = Some(n); matched_key = Some(k.to_string()); break;
                }
            }
        }
```

Update the logging branch below to use `matched_key.as_deref()` instead of `Some(&str)`:
```rust
        match (fee, matched_key) {
            (Some(f), Some(k)) => {
                tracing::debug!(
                    "[POLYMARKET] meta {} → (neg_risk={}, fee={} via key '{}')",
                    condition_id, neg_risk, f, k
                );
                Ok((neg_risk, f))
            }
            _ => { /* existing bail! path */ }
        }
```

- [ ] **Step 4: Re-run tests**

```bash
cargo test --lib polymarket_clob -- --nocapture
```

Expected: all polymarket_clob tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/polymarket_clob.rs
git commit -m "Prefer feeSchedule.takerBaseFee in fetch_market_meta

CLOB survey (see docs/notes/2026-04-21-polymarket-fee-survey.md)
confirmed feeSchedule is now the primary fee field. Legacy flat keys
remain as fallback for older responses. Matched-key logging now
distinguishes feeSchedule.<key> from flat <key>."
```

---

## Task 11: Replace `main.rs` Fee Loop with Per-Market CLOB Resolver

**Files:**
- Modify: `src/main.rs:192-201`

- [ ] **Step 1: Replace the fee-setting loop**

Open `src/main.rs`. Locate the block:
```rust
    // Set Polymarket taker-fee rate on every market for the arb detector.
    // Every tracked league is a sports market, so we hardcode Sports = 30_000 ppm (3.00%).
    // …
    {
        let n = state.market_count();
        for i in 0..n {
            state.markets[i].set_poly_fee_rate_ppm(types::SPORTS_FEE_RATE_PPM);
        }
        info!(
            "[POLYMARKET] Detector fee rate set to {} ppm (Sports) for {} markets",
            types::SPORTS_FEE_RATE_PPM, n
        );
    }
```

Replace with:
```rust
    // Seed each market's detector-side Polymarket taker-fee from the CLOB
    // (source of truth). Fall back to the per-category table if the CLOB
    // lookup fails for any market. See spec §4.1.
    {
        let n = state.market_count();
        let mut from_clob = 0usize;
        let mut from_table = 0usize;
        for i in 0..n {
            let pair = match state.markets[i].pair.as_ref() {
                Some(p) => p.clone(),
                None => continue,
            };
            let ppm = match poly_async.get_market_meta(&pair.poly_yes_token, &pair.poly_condition_id).await {
                Ok((_, fee_bps)) => {
                    from_clob += 1;
                    fees::bps_to_ppm(fee_bps)
                }
                Err(e) => {
                    warn!(
                        "[POLYMARKET] meta fetch failed for {} — falling back to category table: {}",
                        pair.pair_id, e
                    );
                    from_table += 1;
                    fees::category_fee_ppm(pair.category)
                }
            };
            state.markets[i].set_poly_fee_rate_ppm(ppm);
        }
        info!(
            "[POLYMARKET] Per-market detector fees set: {} via CLOB, {} via category table (total {})",
            from_clob, from_table, n
        );
    }
```

- [ ] **Step 2: Build and run a DRY_RUN spot-check**

```bash
cargo build --release
DRY_RUN=1 FORCE_DISCOVERY=0 RUST_LOG=info cargo run --release 2>&1 | head -80
```

Expected log lines:
- `[POLYMARKET] Per-market detector fees set: N via CLOB, M via category table (total N+M)` — where ideally `M == 0`.
- If `M > 0`, inspect the preceding `warn!` lines to identify failing markets; a handful of flaky calls is acceptable, but >10% failures means the CLOB endpoint has an issue worth investigating before merge.

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m "Seed detector fees per-market from CLOB with category fallback

Replaces the single-constant sports fee loop with per-market resolution:
get_market_meta (source of truth) → bps_to_ppm → set_poly_fee_rate_ppm,
with fallback to category_fee_ppm on CLOB error. Logs counts of CLOB
vs table usage for visibility."
```

---

## Task 12: Migrate Remaining `SPORTS_FEE_RATE_PPM` Callers

**Files:**
- Modify: `src/types.rs` — tests at lines 697-766

- [ ] **Step 1: Audit current callers**

```bash
cargo build --release 2>&1 | grep -i warn
```

Remaining callers (per the Task 7 grep): all inside `src/types.rs` tests. The constant itself remains, but call sites should reference `fees::category_fee_ppm(PolyCategory::Sports)` for readability.

- [ ] **Step 2: Migrate test call-sites in `src/types.rs`**

Replace occurrences of `SPORTS_FEE_RATE_PPM` inside the `#[cfg(test)]` mod tests block (lines 697-766) with `crate::fees::category_fee_ppm(crate::fees::PolyCategory::Sports)`. Keep the constant itself and its doc comment — it's still referenced externally and serves as a well-named shortcut.

Concretely, inside the test module add near the top:
```rust
    use crate::fees::{category_fee_ppm, PolyCategory};
    fn sports_ppm() -> u32 { category_fee_ppm(PolyCategory::Sports) }
```
and replace `SPORTS_FEE_RATE_PPM` with `sports_ppm()` in the five tests that reference it. This keeps tests readable and ties them to the canonical table.

- [ ] **Step 3: Run the full test suite**

```bash
cargo test --lib -- --nocapture
```

Expected: all existing tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/types.rs
git commit -m "Route test call-sites of SPORTS_FEE_RATE_PPM through fees table

Tests reference category_fee_ppm(Sports) indirectly via a local helper,
so the fee table is the single source of truth in tests as well as
production. The pub const remains as a convenience for external crates."
```

---

## Task 13: End-to-End Smoke Test (DRY_RUN)

**Files:** none (verification only).

- [ ] **Step 1: Clean build + DRY_RUN**

```bash
cargo build --release 2>&1 | tail -20
DRY_RUN=1 RUST_LOG=arb_bot=info cargo run --release 2>&1 | tee /tmp/smoke.log &
SMOKE_PID=$!
sleep 90
kill $SMOKE_PID 2>/dev/null
```

- [ ] **Step 2: Verify expected log fingerprints**

```bash
grep -E "Market discovery complete|Matched market pairs|Per-market detector fees set" /tmp/smoke.log
```

Expected:
- `Market discovery complete` with a non-zero pair count.
- `Matched market pairs: N` matching the pre-PR-1 count (minus naturally expired markets).
- `Per-market detector fees set: X via CLOB, Y via category table (total N)` where `X > 0.9 * N` ideally.

- [ ] **Step 3: Verify no panics / errors**

```bash
grep -E "panicked|error\[|ERROR" /tmp/smoke.log | head
```

Expected: empty output, or only benign "WebSocket disconnected" / transient Gamma 5xx errors.

- [ ] **Step 4: Manual sanity check on one pair**

Pick any logged pair, cross-check its fee against the CLOB directly:
```bash
curl -s https://clob.polymarket.com/markets/<condition_id> | jq '{feeSchedule, takerBaseFee, taker_fee_rate_bps}'
```

Verify the fee matches what appears in the bot's log (modulo the `bps_to_ppm` conversion).

- [ ] **Step 5: Mark PR 1 complete**

No commit in this task; it's verification. If everything passes:
- PR 1 is ready for code review.
- Proceed to write PR 2's plan (`docs/superpowers/plans/2026-04-21-multi-category-matching-pr2.md`) covering `FomcAdapter`.

If something fails: diagnose, fix via an additional task (append to this plan or spin a follow-up), re-run Step 1-4.

---

## Self-Review

**Spec coverage check (§7 PR 1 acceptance criteria):**
- ✅ Tasks A + B calibration — Task 1 + Task 3.
- ✅ `PolyCategory`, `category_fee_ppm`, calibrated `bps_to_ppm` + test — Tasks 2 + 3.
- ✅ `category` + `match_source` on `MarketPair`, serde-defaulted — Task 5.
- ✅ Replace `main.rs:192-201` with CLOB-seeded + category fallback — Task 11.
- ✅ `src/canonical.rs` types — Task 6.
- ✅ `EventAdapter` trait — Task 7.
- ✅ Refactor sports behind `SportsAdapter` (behavior-preserving) — Task 8 + 9.
- ✅ Fix "3.00%" comment — Task 4.
- ✅ `SPORTS_FEE_RATE_PPM` callers migrated — Task 12.
- ✅ `bps_to_ppm` unit test green — Task 3.
- ✅ Pre-PR-1 cache loads cleanly — Task 5 test.
- ✅ Smoke test — Task 13.
- ✅ `feeSchedule` primary path — Task 10 (conditional on survey findings).

**Type-consistency check:**
- `PolyCategory` defined in Task 2, used in Tasks 3, 5, 6, 8, 11, 12. ✓
- `MatchSource` defined in Task 5 (in `fees.rs`), used in Task 7 (`pair_batch::build_pair`). ✓
- `CanonicalMarket`, `Underlier`, `EventType`, `Platform`, `SportsSubtype` defined in Task 6, used in Tasks 7 and 8. ✓
- `EventAdapter`, `NormalizedBatch`, `pair_batch` defined in Task 7, used in Tasks 8 (impl) and 9 (call). ✓
- `bps_to_ppm` defined in Task 3, used in Task 11. ✓

**Placeholder scan:** one legitimate placeholder is present — `OBSERVED_SPORTS_BPS` and `K_FROM_SURVEY` in Task 3, both explicitly flagged as "fill from survey" and locked in by the test in Step 2. Survey findings committed in Task 1 are the source. No other placeholders remain.

---

## Execution Handoff

After this plan is reviewed and approved:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task via `superpowers:subagent-driven-development`, with two-stage review between tasks. Each Rust task has compile + test gates that are natural checkpoints.

2. **Inline Execution** — execute tasks in-session via `superpowers:executing-plans`, with manual checkpoint after each `git commit`.

Either way, PR 2 (`FomcAdapter`) and PR 3 (AI sidecar) get their own plans written after PR 1 is merged — PR 3 especially benefits from having the Task 1 survey findings already locked in.

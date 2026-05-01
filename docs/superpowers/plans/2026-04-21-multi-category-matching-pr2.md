# PR 2 — FomcAdapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a structured `FomcAdapter` so the bot discovers cross-venue arbitrage on FOMC rate-decision markets, gated to detection-only for the first live meeting.

**Architecture:** New file `src/adapters/fomc.rs` implements the `EventAdapter` trait shipped in PR 1. It walks Kalshi `KXFED` events → markets, resolves the current fed-funds target lower bound (Kalshi metadata first, then FRED `DFEDTARL`), parses Polymarket neg-risk outcome labels (`"25 bps cut"` → −25 bps), and emits `CanonicalMarket`s keyed on `(EventType::Fomc, Underlier::FomcRateBand { meeting_date, floor_bps })`. PR 1's `pair_batch` does the join. Execution of FOMC pairs is gated behind `EXEC_ALLOW_FOMC=1` (default off).

**Tech Stack:** Rust 2021, tokio, async-trait, anyhow, reqwest, chrono (with `serde`), serde, the canonical-schema types from `src/canonical.rs`, the `EventAdapter` trait from `src/adapters/mod.rs`.

**Spec reference:** `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` §4.5 (FomcAdapter), §7 PR 2 (acceptance criteria), Appendix B (worked example), §8 open question 2 (Kalshi-anchor-vs-FRED).

**Pre-flight (executor, before starting Task 1):**
- Confirm you are on branch `pr2/fomc-adapter` in worktree `.worktrees/pr2-fomc-adapter`.
- Confirm `cargo test` passes on a clean tree against `main` (= commit `8fafffb`).
- Confirm `src/adapters/mod.rs` already exposes `EventAdapter`, `NormalizedBatch`, and `pair_batch_from`. Confirm `src/canonical.rs` already defines `EventType::Fomc` and `Underlier::FomcRateBand { meeting_date, floor_bps }`. Both shipped in PR 1.

---

## File Structure

**New files**
- `src/fred.rs` — minimal async FRED client. One public fn: `fetch_fed_lower_bound_bps(http: &reqwest::Client, api_key: Option<&str>) -> Result<i32>`.
- `src/adapters/fomc.rs` — the adapter itself, plus its parser helpers and tests.

**Modified files**
- `src/lib.rs` — declare `pub mod fred;`.
- `src/adapters/mod.rs` — declare `pub mod fomc;`.
- `src/polymarket.rs` — add `GammaClient::lookup_event(slug)` for fetching neg-risk events with their child markets.
- `src/main.rs` — register `FomcAdapter` in `DiscoveryClient::new(...)` (around the existing line 152).
- `src/execution.rs` — add the FOMC detection-only gate inside `process_request`.
- `src/config.rs` — surface `FRED_API_KEY`, `EXEC_ALLOW_FOMC`, `FOMC_ENABLED` env helpers.
- `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` — resolve open question #2 with the empirical Kalshi-metadata finding.

**Cache compatibility**
PR 2 adds no new fields to `MarketPair`. `.discovery_cache.json` files written by PR 1 stay readable as-is. No serde-defaults work needed.

---

## Task 1: Add config helpers for FRED key, FOMC enable, and execution gate

**Files:**
- Modify: `src/config.rs:1-181`
- Test: `src/config.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Read `src/config.rs` end-to-end**

Read the file once before adding anything. Match its existing patterns (top-level `pub const` for static URLs, `pub fn ...() -> bool` helpers that read env vars and parse them).

- [ ] **Step 2: Write the failing test**

Append to the bottom of `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn fomc_enabled_defaults_to_true() {
        env::remove_var("FOMC_ENABLED");
        assert!(fomc_enabled());
    }

    #[test]
    fn fomc_enabled_respects_zero() {
        env::set_var("FOMC_ENABLED", "0");
        assert!(!fomc_enabled());
        env::remove_var("FOMC_ENABLED");
    }

    #[test]
    fn exec_allow_fomc_defaults_to_false() {
        env::remove_var("EXEC_ALLOW_FOMC");
        assert!(!exec_allow_fomc());
    }

    #[test]
    fn exec_allow_fomc_true_when_set_to_one() {
        env::set_var("EXEC_ALLOW_FOMC", "1");
        assert!(exec_allow_fomc());
        env::remove_var("EXEC_ALLOW_FOMC");
    }

    #[test]
    fn fred_api_key_returns_none_when_unset() {
        env::remove_var("FRED_API_KEY");
        assert!(fred_api_key().is_none());
    }

    #[test]
    fn fred_api_key_returns_some_when_set() {
        env::set_var("FRED_API_KEY", "abc123");
        assert_eq!(fred_api_key().as_deref(), Some("abc123"));
        env::remove_var("FRED_API_KEY");
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib config::tests -- --test-threads=1`
Expected: compile errors — `fomc_enabled`, `exec_allow_fomc`, `fred_api_key` not defined.

(`--test-threads=1` is mandatory because the tests mutate process-wide env. Mention this in a comment above the `mod tests` line.)

- [ ] **Step 4: Implement the helpers**

Add inside `src/config.rs` near the top (above the `mod tests` block):

```rust
/// FOMC adapter master switch. Default ON; set `FOMC_ENABLED=0` to disable
/// (e.g. if FRED is down and we want to roll back without redeploying).
pub fn fomc_enabled() -> bool {
    std::env::var("FOMC_ENABLED")
        .map(|v| v != "0" && v.to_lowercase() != "false")
        .unwrap_or(true)
}

/// Detection-only gate for FOMC pairs. Default OFF — the first live meeting
/// is a soak test. Flip to `1` once we've verified pair quality post-meeting.
pub fn exec_allow_fomc() -> bool {
    std::env::var("EXEC_ALLOW_FOMC")
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false)
}

/// Optional FRED API key. Without it the FRED endpoint still works but is
/// rate-limited; with it we get a per-key quota. See spec §4.5.
pub fn fred_api_key() -> Option<String> {
    std::env::var("FRED_API_KEY").ok().filter(|s| !s.is_empty())
}
```

Add a `// NOTE: tests mutate env; require --test-threads=1` line above `mod tests`.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib config::tests -- --test-threads=1`
Expected: PASS, all six tests green.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "Add FOMC_ENABLED, EXEC_ALLOW_FOMC, and FRED_API_KEY config helpers"
```

---

## Task 2: FRED anchor resolver

**Files:**
- Create: `src/fred.rs`
- Modify: `src/lib.rs:1-19`
- Test: `src/fred.rs` (inline `#[cfg(test)] mod tests`)

The FRED `DFEDTARL` series is "Federal Funds Target Range — Lower Limit", expressed as a percent (e.g. `"4.25"` for 4.25%). We convert that to integer basis points (425) so it lines up with the Kalshi `floor_strike`-derived bps.

- [ ] **Step 1: Write the failing test**

Create `src/fred.rs` with this content (test only, no impl yet):

```rust
//! FRED API client for the fed-funds target lower-bound anchor used by
//! `FomcAdapter`. Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.5.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_observation_to_bps() {
        // Synthesized to mirror FRED's actual response shape.
        let body = r#"{
            "observations": [
                { "date": "2026-04-28", "value": "4.25" }
            ]
        }"#;
        let bps = parse_lower_bound_bps(body).unwrap();
        assert_eq!(bps, 425);
    }

    #[test]
    fn parses_zero_lower_bound() {
        let body = r#"{
            "observations": [
                { "date": "2020-04-01", "value": "0.00" }
            ]
        }"#;
        assert_eq!(parse_lower_bound_bps(body).unwrap(), 0);
    }

    #[test]
    fn rejects_missing_observation_with_period_value() {
        // FRED uses "." for missing numeric values.
        let body = r#"{ "observations": [ { "date": "2026-04-28", "value": "." } ] }"#;
        assert!(parse_lower_bound_bps(body).is_err());
    }

    #[test]
    fn rejects_empty_observations_array() {
        let body = r#"{ "observations": [] }"#;
        assert!(parse_lower_bound_bps(body).is_err());
    }

    #[test]
    fn rounds_half_to_nearest_bps() {
        // 4.255% → 425.5 bps. FRED only publishes to two decimals so this
        // shouldn't happen in practice, but document the rounding rule anyway.
        let body = r#"{ "observations": [ { "date": "x", "value": "4.255" } ] }"#;
        assert_eq!(parse_lower_bound_bps(body).unwrap(), 426);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib fred::`
Expected: compile error — `parse_lower_bound_bps` not defined; `src/fred.rs` not declared in `lib.rs`.

- [ ] **Step 3: Add the module to `lib.rs`**

Edit `src/lib.rs`. Insert `pub mod fred;` alphabetically (between `pub mod fees;` and `pub mod kalshi;`):

```rust
pub mod fees;
pub mod fred;
pub mod kalshi;
```

- [ ] **Step 4: Implement `parse_lower_bound_bps`**

Add to `src/fred.rs` above the `mod tests` block:

```rust
const FRED_OBSERVATIONS_URL: &str =
    "https://api.stlouisfed.org/fred/series/observations";
const SERIES_ID: &str = "DFEDTARL";

#[derive(Debug, Deserialize)]
struct Observations {
    observations: Vec<Observation>,
}

#[derive(Debug, Deserialize)]
struct Observation {
    #[allow(dead_code)]
    date: String,
    value: String,
}

/// Parse a FRED observations JSON body into integer basis points.
/// Public for testing; production code goes through `fetch_fed_lower_bound_bps`.
pub(crate) fn parse_lower_bound_bps(body: &str) -> Result<i32> {
    let parsed: Observations =
        serde_json::from_str(body).context("FRED observations JSON malformed")?;
    let latest = parsed
        .observations
        .last()
        .ok_or_else(|| anyhow!("FRED observations array empty"))?;
    if latest.value == "." {
        return Err(anyhow!("FRED returned missing-value '.' for latest observation"));
    }
    let pct: f64 = latest
        .value
        .parse()
        .with_context(|| format!("FRED value not a float: {:?}", latest.value))?;
    Ok((pct * 100.0).round() as i32)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib fred::`
Expected: PASS, all five parser tests green.

- [ ] **Step 6: Add the live-fetch wrapper (no test — it hits the network)**

Append to `src/fred.rs`:

```rust
/// Fetch the latest fed-funds target lower-bound from FRED and return it
/// in integer basis points. The endpoint is public; the API key is optional
/// and only buys a per-key rate-limit budget.
///
/// Returns Err on:
/// - HTTP non-2xx
/// - Empty observations array (FRED outage)
/// - Latest value is "." (missing observation)
pub async fn fetch_fed_lower_bound_bps(
    http: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<i32> {
    let mut req = http
        .get(FRED_OBSERVATIONS_URL)
        .query(&[
            ("series_id", SERIES_ID),
            ("file_type", "json"),
            ("sort_order", "desc"),
            ("limit", "1"),
        ]);
    if let Some(key) = api_key {
        req = req.query(&[("api_key", key)]);
    }
    let resp = req.send().await.context("FRED request failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!("FRED HTTP {}: {}", resp.status(), resp.text().await.unwrap_or_default()));
    }
    let body = resp.text().await.context("FRED body not UTF-8")?;
    parse_lower_bound_bps(&body)
}
```

- [ ] **Step 7: Verify the module compiles cleanly**

Run: `cargo build --lib`
Expected: no warnings beyond pre-existing ones.

- [ ] **Step 8: Commit**

```bash
git add src/fred.rs src/lib.rs
git commit -m "Add FRED DFEDTARL anchor resolver returning integer basis points"
```

---

## Task 3: Polymarket FOMC outcome label parser

**Files:**
- Create: `src/adapters/fomc.rs` (parser only — adapter struct comes in Task 6)
- Modify: `src/adapters/mod.rs:20-21`

Polymarket neg-risk events label outcomes as deltas from the current rate (`"25 bps cut"`, `"No change"`, `"50 bps hike"`, etc.). We need to parse those into signed bps.

- [ ] **Step 1: Declare the new module**

Edit `src/adapters/mod.rs`. Add a `pub mod fomc;` line right after the existing `pub mod sports;` (line 20):

```rust
pub mod sports;
pub mod fomc;
```

- [ ] **Step 2: Write the failing test**

Create `src/adapters/fomc.rs` with this content (parser test only):

```rust
//! FOMC rate-decision adapter — pairs Kalshi `KXFED*` markets with Polymarket
//! neg-risk outcomes via a current-rate anchor.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.5.

#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn parses_25_bps_cut() {
        assert_eq!(parse_fomc_delta_bps("25 bps cut"), Some(-25));
    }

    #[test]
    fn parses_50_bps_decrease() {
        assert_eq!(parse_fomc_delta_bps("50 bps decrease"), Some(-50));
    }

    #[test]
    fn parses_25_bps_hike() {
        assert_eq!(parse_fomc_delta_bps("25 bps hike"), Some(25));
    }

    #[test]
    fn parses_no_change() {
        assert_eq!(parse_fomc_delta_bps("No change"), Some(0));
    }

    #[test]
    fn parses_hold_synonym() {
        assert_eq!(parse_fomc_delta_bps("hold"), Some(0));
    }

    #[test]
    fn parses_75_bps_increase() {
        assert_eq!(parse_fomc_delta_bps("75 bps increase"), Some(75));
    }

    #[test]
    fn parses_with_extra_whitespace() {
        assert_eq!(parse_fomc_delta_bps("  25  bps   cut  "), Some(-25));
    }

    #[test]
    fn parses_case_insensitive() {
        assert_eq!(parse_fomc_delta_bps("25 BPS HIKE"), Some(25));
        assert_eq!(parse_fomc_delta_bps("NO CHANGE"), Some(0));
    }

    #[test]
    fn parses_bp_singular() {
        // Some events use "bp" instead of "bps".
        assert_eq!(parse_fomc_delta_bps("25 bp cut"), Some(-25));
    }

    #[test]
    fn rejects_unknown_label() {
        assert_eq!(parse_fomc_delta_bps("rates go to the moon"), None);
    }

    #[test]
    fn rejects_label_without_direction() {
        assert_eq!(parse_fomc_delta_bps("25 bps"), None);
    }

    #[test]
    fn rejects_empty_string() {
        assert_eq!(parse_fomc_delta_bps(""), None);
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib adapters::fomc::`
Expected: compile error — `parse_fomc_delta_bps` undefined.

- [ ] **Step 4: Implement the parser**

Prepend to `src/adapters/fomc.rs` (above the `mod parser_tests` block):

```rust
/// Parse a Polymarket FOMC outcome label like `"25 bps cut"` or `"No change"`
/// into a signed delta in basis points. Returns `None` for labels we don't
/// recognize so the caller can log + skip rather than silently default to 0.
///
/// Recognized shapes (case-insensitive, whitespace-tolerant):
///   - `"<N> bps? (cut|decrease|lower)"`   → −N
///   - `"<N> bps? (hike|increase|raise)"`  → +N
///   - `"no change"` | `"hold"`            →  0
pub(crate) fn parse_fomc_delta_bps(label: &str) -> Option<i32> {
    let lower = label.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower == "no change" || lower == "hold" {
        return Some(0);
    }

    // Tokenize on whitespace, normalizing internal runs.
    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }

    // Expect: <number> <bp|bps> <direction>
    let n: i32 = tokens[0].parse().ok()?;
    let unit_ok = tokens[1] == "bp" || tokens[1] == "bps";
    if !unit_ok {
        return None;
    }
    let direction = tokens[2];
    let signed = match direction {
        "cut" | "decrease" | "lower" => -n,
        "hike" | "increase" | "raise" => n,
        _ => return None,
    };
    Some(signed)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib adapters::fomc::`
Expected: PASS, twelve parser tests green.

- [ ] **Step 6: Commit**

```bash
git add src/adapters/mod.rs src/adapters/fomc.rs
git commit -m "Add Polymarket FOMC outcome label parser with bp/bps + cut/hike/hold variants"
```

---

## Task 4: Extend `GammaClient` with event-by-slug lookup

**Files:**
- Modify: `src/polymarket.rs:60-141`
- Test: `src/polymarket.rs` (inline `mod gamma_event_tests`)

The existing `lookup_market(slug)` returns one yes/no/condition_id triple. FOMC neg-risk events have N child markets — one per outcome — and we need all of them. Polymarket Gamma exposes them at `/events?slug=<event_slug>`, where each event JSON has a `markets: [...]` array.

- [ ] **Step 1: Write the failing test**

Append to `src/polymarket.rs` at the end of the file:

```rust
#[cfg(test)]
mod gamma_event_tests {
    use super::*;

    #[test]
    fn parses_event_with_negrisk_markets() {
        // Synthesized to mirror the real shape from
        // GET https://gamma-api.polymarket.com/events?slug=fomc-decision-april-2026
        let body = r#"[
            {
                "slug": "fomc-decision-april-2026",
                "title": "FOMC Decision — April 2026",
                "neg_risk": true,
                "closed": false,
                "active": true,
                "markets": [
                    {
                        "slug": "fomc-25-bps-cut-april-2026",
                        "question": "25 bps cut",
                        "clobTokenIds": "[\"tokA1\",\"tokA2\"]",
                        "conditionId": "0xCONDA",
                        "active": true,
                        "closed": false
                    },
                    {
                        "slug": "fomc-no-change-april-2026",
                        "question": "No change",
                        "clobTokenIds": "[\"tokB1\",\"tokB2\"]",
                        "conditionId": "0xCONDB",
                        "active": true,
                        "closed": false
                    }
                ]
            }
        ]"#;

        let event = parse_gamma_event_response(body).unwrap().expect("event present");
        assert_eq!(event.slug, "fomc-decision-april-2026");
        assert_eq!(event.markets.len(), 2);
        assert_eq!(event.markets[0].question, "25 bps cut");
        assert_eq!(event.markets[0].yes_token, "tokA1");
        assert_eq!(event.markets[0].no_token, "tokA2");
        assert_eq!(event.markets[0].condition_id, "0xCONDA");
    }

    #[test]
    fn returns_none_for_empty_response() {
        assert!(parse_gamma_event_response("[]").unwrap().is_none());
    }

    #[test]
    fn skips_closed_child_markets() {
        let body = r#"[
            {
                "slug": "fomc-decision-april-2026",
                "title": "x",
                "neg_risk": true,
                "closed": false,
                "active": true,
                "markets": [
                    {
                        "slug": "fomc-25-bps-cut-april-2026",
                        "question": "25 bps cut",
                        "clobTokenIds": "[\"tokA1\",\"tokA2\"]",
                        "conditionId": "0xCONDA",
                        "active": true,
                        "closed": true
                    }
                ]
            }
        ]"#;

        let event = parse_gamma_event_response(body).unwrap().unwrap();
        assert!(event.markets.is_empty(), "closed child markets must be filtered out");
    }

    #[test]
    fn skips_child_markets_with_missing_condition_id() {
        let body = r#"[
            {
                "slug": "x",
                "title": "x",
                "neg_risk": true,
                "closed": false,
                "active": true,
                "markets": [
                    {
                        "slug": "no-cid",
                        "question": "25 bps cut",
                        "clobTokenIds": "[\"tokA1\",\"tokA2\"]",
                        "active": true,
                        "closed": false
                    }
                ]
            }
        ]"#;

        let event = parse_gamma_event_response(body).unwrap().unwrap();
        assert!(event.markets.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib polymarket::gamma_event_tests`
Expected: compile error — `parse_gamma_event_response`, `GammaEvent`, `GammaEventMarket` undefined.

- [ ] **Step 3: Add the types and parser**

Insert after the existing `struct GammaMarket { ... }` definition (around line 141 of `src/polymarket.rs`):

```rust
/// A Gamma event with its child markets — used for neg-risk events like FOMC
/// where each rate outcome is its own market.
#[derive(Debug, Clone)]
pub struct GammaEvent {
    pub slug: String,
    pub title: String,
    pub markets: Vec<GammaEventMarket>,
}

#[derive(Debug, Clone)]
pub struct GammaEventMarket {
    pub slug: String,
    pub question: String,
    pub yes_token: String,
    pub no_token: String,
    pub condition_id: String,
}

#[derive(Debug, Deserialize)]
struct GammaEventRaw {
    slug: Option<String>,
    title: Option<String>,
    #[allow(dead_code)]
    neg_risk: Option<bool>,
    closed: Option<bool>,
    active: Option<bool>,
    markets: Option<Vec<GammaEventMarketRaw>>,
}

#[derive(Debug, Deserialize)]
struct GammaEventMarketRaw {
    slug: Option<String>,
    question: Option<String>,
    #[serde(rename = "clobTokenIds")]
    clob_token_ids: Option<String>,
    #[serde(rename = "conditionId")]
    condition_id: Option<String>,
    active: Option<bool>,
    closed: Option<bool>,
}

/// Parse a Gamma `/events?slug=...` response body. Returns `Ok(None)` when
/// the array is empty (event not found or not yet published). Filters out
/// closed/inactive/incomplete child markets defensively.
pub(crate) fn parse_gamma_event_response(body: &str) -> Result<Option<GammaEvent>> {
    let events: Vec<GammaEventRaw> =
        serde_json::from_str(body).context("Gamma event response not valid JSON")?;
    let raw = match events.into_iter().next() {
        Some(e) => e,
        None => return Ok(None),
    };
    if raw.closed == Some(true) || raw.active == Some(false) {
        return Ok(None);
    }

    let markets: Vec<GammaEventMarket> = raw
        .markets
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| {
            if m.closed == Some(true) || m.active == Some(false) {
                return None;
            }
            let cid = m.condition_id.as_ref().filter(|s| !s.is_empty())?.clone();
            let toks: Vec<String> = m
                .clob_token_ids
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            if toks.len() < 2 {
                return None;
            }
            Some(GammaEventMarket {
                slug: m.slug.unwrap_or_default(),
                question: m.question.unwrap_or_default(),
                yes_token: toks[0].clone(),
                no_token: toks[1].clone(),
                condition_id: cid,
            })
        })
        .collect();

    Ok(Some(GammaEvent {
        slug: raw.slug.unwrap_or_default(),
        title: raw.title.unwrap_or_default(),
        markets,
    }))
}
```

- [ ] **Step 4: Add the live-fetch wrapper on `GammaClient`**

Append a method inside the `impl GammaClient { ... }` block (right after `try_lookup_slug`):

```rust
/// Fetch a Gamma event by slug, returning the event with its child markets.
/// Used by `FomcAdapter` to walk neg-risk outcomes.
pub async fn lookup_event(&self, slug: &str) -> Result<Option<GammaEvent>> {
    let url = format!("{}/events?slug={}", crate::config::GAMMA_API_BASE, slug);
    let resp = self.http.get(&url).send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let body = resp.text().await?;
    parse_gamma_event_response(&body)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib polymarket::gamma_event_tests`
Expected: PASS, four tests green.

- [ ] **Step 6: Verify nothing else broke**

Run: `cargo test --lib`
Expected: all pre-existing tests still pass (PR 1 added 81 tests; total should be 81 + 6 + 5 + 12 + 4 = 108 unit tests).

- [ ] **Step 7: Commit**

```bash
git add src/polymarket.rs
git commit -m "Add GammaClient::lookup_event for fetching neg-risk events with child markets"
```

---

## Task 5: FomcAdapter skeleton — name, event_type, version

**Files:**
- Modify: `src/adapters/fomc.rs` (append)

This task lands the `FomcAdapter` struct with its trait identity but no fetch logic yet. Subsequent tasks fill in normalization. We do this first so later tasks can refer to a concrete type.

- [ ] **Step 1: Write the failing test**

Append to `src/adapters/fomc.rs` (after the existing `parser_tests` module):

```rust
#[cfg(test)]
mod adapter_identity_tests {
    use super::*;
    use crate::adapters::EventAdapter;
    use crate::canonical::EventType;

    fn mk_adapter() -> FomcAdapter {
        FomcAdapter::new_for_tests()
    }

    #[test]
    fn name_is_fomc() {
        assert_eq!(mk_adapter().name(), "fomc");
    }

    #[test]
    fn event_type_is_fomc() {
        assert_eq!(mk_adapter().event_type(), EventType::Fomc);
    }

    #[test]
    fn version_is_one() {
        assert_eq!(mk_adapter().version(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib adapters::fomc::adapter_identity_tests`
Expected: compile error — `FomcAdapter` undefined.

- [ ] **Step 3: Implement the skeleton**

Insert near the top of `src/adapters/fomc.rs`, just under the file header doc-comment and above the `parse_fomc_delta_bps` definition:

```rust
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

use crate::adapters::{EventAdapter, NormalizedBatch};
use crate::canonical::EventType;
use crate::kalshi::KalshiClient;
use crate::polymarket::GammaClient;

const FOMC_KALSHI_SERIES: &str = "KXFED";

pub struct FomcAdapter {
    kalshi: Arc<KalshiClient>,
    gamma: Arc<GammaClient>,
    http: reqwest::Client,
    fred_api_key: Option<String>,
}

impl FomcAdapter {
    pub fn new(
        kalshi: Arc<KalshiClient>,
        gamma: Arc<GammaClient>,
        http: reqwest::Client,
        fred_api_key: Option<String>,
    ) -> Self {
        Self { kalshi, gamma, http, fred_api_key }
    }

    /// Test-only ctor with throwaway clients. The trait-identity tests don't
    /// hit the network, so the inner clients are never used.
    #[cfg(test)]
    pub(crate) fn new_for_tests() -> Self {
        Self {
            kalshi: Arc::new(KalshiClient::new_unauthenticated()),
            gamma: Arc::new(GammaClient::new()),
            http: reqwest::Client::new(),
            fred_api_key: None,
        }
    }
}

#[async_trait]
impl EventAdapter for FomcAdapter {
    fn name(&self) -> &'static str {
        "fomc"
    }

    fn event_type(&self) -> EventType {
        EventType::Fomc
    }

    fn version(&self) -> u32 {
        1
    }

    async fn normalize(&self) -> Result<NormalizedBatch> {
        // Filled in over Tasks 6–8.
        Ok(NormalizedBatch { kalshi: vec![], poly: vec![] })
    }
}
```

- [ ] **Step 4: Add `KalshiClient::new_unauthenticated` if it doesn't already exist**

Run: `grep -n "new_unauthenticated\|pub fn new" src/kalshi.rs`

If `new_unauthenticated` does not exist, add a minimal helper inside the `impl KalshiClient` block. The test ctor only needs a struct that compiles; the test does not call any method on it. If the existing `KalshiClient::new(...)` requires real credentials and panics on bad input, prefer adding:

```rust
/// Test-only constructor — produces a client with empty creds. Calls that
/// hit the network will fail at the auth layer; do not use in production.
#[cfg(test)]
pub fn new_unauthenticated() -> Self {
    // Mirror the real ctor's structure but with placeholder values.
    // (Exact body depends on KalshiClient's fields — read the existing
    //  `pub fn new(...)` body and substitute empty strings for keys.)
    todo!("inspect existing ctor and substitute empty creds")
}
```

If reading the existing ctor reveals it can already be constructed without secrets, skip this step entirely and just call that ctor in `new_for_tests`. The goal is minimum-viable test instantiation, not a public API change.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib adapters::fomc::adapter_identity_tests`
Expected: PASS, three tests green.

- [ ] **Step 6: Run the full test suite to catch unrelated breakage**

Run: `cargo test --lib`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add src/adapters/fomc.rs src/kalshi.rs
git commit -m "Add FomcAdapter skeleton implementing EventAdapter (name=fomc, version=1)"
```

---

## Task 6: Kalshi-side normalization in FomcAdapter

**Files:**
- Modify: `src/adapters/fomc.rs`

Walk `KXFED` events → markets, parse the meeting date out of the event ticker, and emit a `CanonicalMarket` per market with `floor_strike` set. `floor_bps = (floor_strike * 100).round() as i32` (`4.25` → `425`).

Kalshi event tickers for FOMC look like `KXFED-26MAY` (year-month). The meeting day is not in the ticker — we approximate as the 1st of the month for now and revisit if pair-join ever needs the precise day. (The pair-join key only uses meeting_date for collision avoidance across meetings; same-month FOMC events do not exist.)

- [ ] **Step 1: Write the failing test**

Append to `src/adapters/fomc.rs`:

```rust
#[cfg(test)]
mod kalshi_normalize_tests {
    use super::*;
    use crate::canonical::{EventType, Underlier};
    use crate::types::{KalshiEvent, KalshiMarket};

    #[test]
    fn parses_meeting_date_from_event_ticker() {
        let date = parse_meeting_date_from_event_ticker("KXFED-26MAY").unwrap();
        assert_eq!(date.year(), 2026);
        assert_eq!(date.month(), 5);
        assert_eq!(date.day(), 1);
    }

    #[test]
    fn rejects_event_ticker_without_year_month() {
        assert!(parse_meeting_date_from_event_ticker("KXFED").is_none());
    }

    #[test]
    fn rejects_event_ticker_with_unknown_month() {
        assert!(parse_meeting_date_from_event_ticker("KXFED-26ZZZ").is_none());
    }

    #[test]
    fn normalizes_one_kalshi_market_per_floor_strike() {
        let event = KalshiEvent {
            event_ticker: "KXFED-26APR".into(),
            title: "Federal Reserve Decision — April 2026".into(),
            sub_title: None,
        };
        let markets = vec![
            mk_kalshi_market("KXFED-26APR-T425", "Target rate at 4.25%", Some(4.25)),
            mk_kalshi_market("KXFED-26APR-T450", "Target rate at 4.50%", Some(4.50)),
            mk_kalshi_market("KXFED-26APR-NOFLOOR", "Bad row", None),
        ];

        let canon = normalize_kalshi_markets(&event, &markets);
        assert_eq!(canon.len(), 2, "rows without floor_strike must be skipped");

        match &canon[0].underlier {
            Underlier::FomcRateBand { floor_bps, meeting_date } => {
                assert_eq!(*floor_bps, 425);
                assert_eq!(meeting_date.year(), 2026);
                assert_eq!(meeting_date.month(), 4);
            }
            other => panic!("expected FomcRateBand, got {:?}", other),
        }
        assert_eq!(canon[0].event_type, EventType::Fomc);
        assert_eq!(canon[0].venue.kalshi_market_ticker.as_deref().map(|s| s as &str), Some("KXFED-26APR-T425"));
        assert_eq!(canon[0].venue.kalshi_event_ticker.as_deref().map(|s| s as &str), Some("KXFED-26APR"));
        assert!(canon[0].venue.poly_slug.is_none());
    }

    fn mk_kalshi_market(ticker: &str, title: &str, floor: Option<f64>) -> KalshiMarket {
        KalshiMarket {
            ticker: ticker.into(),
            title: title.into(),
            yes_ask: None,
            yes_bid: None,
            no_ask: None,
            no_bid: None,
            yes_sub_title: None,
            floor_strike: floor,
            volume: None,
            liquidity: None,
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib adapters::fomc::kalshi_normalize_tests`
Expected: compile errors — `parse_meeting_date_from_event_ticker`, `normalize_kalshi_markets` undefined.

- [ ] **Step 3: Implement the helpers**

Add to `src/adapters/fomc.rs` (use additions at top of file as needed):

```rust
use chrono::{Datelike, NaiveDate};

use crate::canonical::{CanonicalMarket, Platform, TimeWindow, Underlier, Venue};
use crate::fees::PolyCategory;
use crate::types::{KalshiEvent, KalshiMarket};

/// Parse `"KXFED-YYMMM"` into a NaiveDate at the 1st of that month.
/// Returns None for any other shape.
pub(crate) fn parse_meeting_date_from_event_ticker(ticker: &str) -> Option<NaiveDate> {
    // Accepted: "<prefix>-<YY><MMM>" where MMM is JAN..DEC. Day is unknown
    // from the ticker so we anchor at the 1st — pair-join keys only use the
    // date to disambiguate meetings, and FOMC meetings never share a month.
    let suffix = ticker.rsplit('-').next()?;
    if suffix.len() != 5 {
        return None;
    }
    let year_2d: i32 = suffix.get(..2)?.parse().ok()?;
    let month = match &suffix.get(2..5)?.to_ascii_uppercase()[..] {
        "JAN" => 1, "FEB" => 2, "MAR" => 3, "APR" => 4, "MAY" => 5, "JUN" => 6,
        "JUL" => 7, "AUG" => 8, "SEP" => 9, "OCT" => 10, "NOV" => 11, "DEC" => 12,
        _ => return None,
    };
    NaiveDate::from_ymd_opt(2000 + year_2d, month, 1)
}

/// Build CanonicalMarkets from a Kalshi `KXFED` event + its markets.
/// Markets without `floor_strike` are skipped (e.g. summary or category rows).
pub(crate) fn normalize_kalshi_markets(
    event: &KalshiEvent,
    markets: &[KalshiMarket],
) -> Vec<CanonicalMarket> {
    let Some(meeting_date) = parse_meeting_date_from_event_ticker(&event.event_ticker) else {
        return vec![];
    };

    let mut out = Vec::with_capacity(markets.len());
    for m in markets {
        let Some(strike_pct) = m.floor_strike else { continue };
        let floor_bps = (strike_pct * 100.0).round() as i32;
        let title: Arc<str> =
            Arc::from(format!("{} - {}", event.title, m.title).as_str());

        out.push(CanonicalMarket {
            event_type: EventType::Fomc,
            underlier: Underlier::FomcRateBand { meeting_date, floor_bps },
            time_window: TimeWindow { event_at: None, settles_at: None },
            venue: Venue {
                platform: Platform::Kalshi,
                kalshi_event_ticker: Some(event.event_ticker.clone().into()),
                kalshi_market_ticker: Some(m.ticker.clone().into()),
                poly_slug: None,
                poly_yes_token: None,
                poly_no_token: None,
                poly_condition_id: None,
            },
            category: PolyCategory::Economics,
            raw_title: title,
            raw_description: Arc::from(""),
            adapter_version: 1,
        });
    }
    out
}
```

(Note: `PolyCategory::Economics` because per spec §2 FOMC is in the Economics tier — 1.50% peak fee. If `Economics` doesn't exist in the enum, use the closest existing variant and add a note for the implementer to confirm via `cargo check`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib adapters::fomc::kalshi_normalize_tests`
Expected: PASS, four tests green.

- [ ] **Step 5: Wire `normalize_kalshi_markets` into `FomcAdapter::normalize`**

Replace the `async fn normalize` body in `FomcAdapter` with:

```rust
async fn normalize(&self) -> Result<NormalizedBatch> {
    let events = self.kalshi.get_events(FOMC_KALSHI_SERIES, 50).await?;
    let mut kalshi_canon: Vec<CanonicalMarket> = Vec::new();
    for ev in &events {
        let markets = match self.kalshi.get_markets(&ev.event_ticker).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("[FOMC] get_markets for {} failed: {}", ev.event_ticker, e);
                continue;
            }
        };
        kalshi_canon.extend(normalize_kalshi_markets(ev, &markets));
    }
    // Polymarket side fills in during Task 8.
    Ok(NormalizedBatch { kalshi: kalshi_canon, poly: vec![] })
}
```

- [ ] **Step 6: Run the full test suite**

Run: `cargo test --lib`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add src/adapters/fomc.rs
git commit -m "Implement Kalshi-side FOMC normalization (KXFED events → FomcRateBand canonical markets)"
```

---

## Task 7: Anchor resolution — Kalshi metadata probe + FRED fallback

**Files:**
- Modify: `src/adapters/fomc.rs`

Add `resolve_anchor_bps` that tries Kalshi event metadata first (returns `None` today), then falls back to FRED. The Kalshi probe is intentionally a stub that compiles in the right shape — leaving it here lets a future Kalshi schema change auto-upgrade us off FRED without touching the call site. Update spec open question §8.2 in Task 12.

- [ ] **Step 1: Write the failing test**

Append to `src/adapters/fomc.rs`:

```rust
#[cfg(test)]
mod anchor_tests {
    use super::*;
    use crate::types::KalshiEvent;

    #[test]
    fn kalshi_event_anchor_returns_none_today() {
        // The Kalshi `KXFED` event schema does not currently expose a
        // current-rate field. This test pins that fact — when Kalshi adds
        // such a field and we wire it up, this test changes shape.
        let ev = KalshiEvent {
            event_ticker: "KXFED-26APR".into(),
            title: "FOMC April 2026".into(),
            sub_title: Some("Target rate".into()),
        };
        assert_eq!(try_anchor_from_kalshi_event(&ev), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib adapters::fomc::anchor_tests`
Expected: compile error — `try_anchor_from_kalshi_event` undefined.

- [ ] **Step 3: Implement the probe and the resolver**

Add to `src/adapters/fomc.rs`:

```rust
/// Probe Kalshi event metadata for a current-rate field (bps).
///
/// As of 2026-04-29 the `KXFED` event schema does not expose a current-rate
/// field. This stub returns None and the caller falls back to FRED. When
/// Kalshi adds the field, replace the body and the FRED dependency becomes
/// best-effort instead of load-bearing. Spec §8 open question 2.
pub(crate) fn try_anchor_from_kalshi_event(_ev: &KalshiEvent) -> Option<i32> {
    None
}

impl FomcAdapter {
    /// Resolve the current fed-funds target lower bound in basis points.
    /// Tries Kalshi metadata first; falls back to FRED.
    async fn resolve_anchor_bps(&self, events: &[KalshiEvent]) -> Result<i32> {
        if let Some(ev) = events.first() {
            if let Some(bps) = try_anchor_from_kalshi_event(ev) {
                tracing::info!("[FOMC] anchor {} bps from Kalshi metadata", bps);
                return Ok(bps);
            }
        }
        let bps = crate::fred::fetch_fed_lower_bound_bps(&self.http, self.fred_api_key.as_deref())
            .await?;
        tracing::info!("[FOMC] anchor {} bps from FRED DFEDTARL", bps);
        Ok(bps)
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib adapters::fomc::anchor_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/adapters/fomc.rs
git commit -m "Add FOMC anchor resolver (Kalshi metadata probe → FRED fallback)"
```

---

## Task 8: Polymarket-side normalization in FomcAdapter

**Files:**
- Modify: `src/adapters/fomc.rs`

For each Kalshi meeting, fetch the matching Polymarket neg-risk event by slug pattern `fomc-decision-<month>-<year>` (e.g. `fomc-decision-april-2026`), parse each child market's `question` via `parse_fomc_delta_bps`, compute `floor_bps = anchor + delta`, and emit a `CanonicalMarket` per child market.

- [ ] **Step 1: Write the failing test**

Append to `src/adapters/fomc.rs`:

```rust
#[cfg(test)]
mod poly_normalize_tests {
    use super::*;
    use crate::canonical::{Platform, Underlier};
    use crate::polymarket::{GammaEvent, GammaEventMarket};
    use chrono::NaiveDate;

    #[test]
    fn builds_event_slug_from_meeting_date() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        assert_eq!(poly_event_slug_for_meeting(date), "fomc-decision-april-2026");
        let dec = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        assert_eq!(poly_event_slug_for_meeting(dec), "fomc-decision-december-2026");
    }

    #[test]
    fn normalizes_poly_outcomes_to_floor_bps() {
        let meeting_date = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let anchor_bps = 425;
        let event = GammaEvent {
            slug: "fomc-decision-april-2026".into(),
            title: "FOMC April 2026".into(),
            markets: vec![
                GammaEventMarket {
                    slug: "fomc-25-bps-cut-april-2026".into(),
                    question: "25 bps cut".into(),
                    yes_token: "tA1".into(),
                    no_token: "tA2".into(),
                    condition_id: "0xA".into(),
                },
                GammaEventMarket {
                    slug: "fomc-no-change-april-2026".into(),
                    question: "No change".into(),
                    yes_token: "tB1".into(),
                    no_token: "tB2".into(),
                    condition_id: "0xB".into(),
                },
                GammaEventMarket {
                    slug: "fomc-rates-on-vacation".into(),
                    question: "rates go to the moon".into(),
                    yes_token: "tC1".into(),
                    no_token: "tC2".into(),
                    condition_id: "0xC".into(),
                },
            ],
        };

        let canon = normalize_poly_event(&event, meeting_date, anchor_bps);
        assert_eq!(canon.len(), 2, "unparseable label must be skipped, not faked to 0");

        // 25 bps cut → 425 - 25 = 400
        match &canon[0].underlier {
            Underlier::FomcRateBand { floor_bps, .. } => assert_eq!(*floor_bps, 400),
            _ => panic!(),
        }
        assert_eq!(canon[0].venue.platform, Platform::Polymarket);
        assert_eq!(canon[0].venue.poly_slug.as_deref().map(|s| s as &str), Some("fomc-25-bps-cut-april-2026"));
        assert_eq!(canon[0].venue.poly_condition_id.as_deref().map(|s| s as &str), Some("0xA"));

        // No change → 425
        match &canon[1].underlier {
            Underlier::FomcRateBand { floor_bps, .. } => assert_eq!(*floor_bps, 425),
            _ => panic!(),
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib adapters::fomc::poly_normalize_tests`
Expected: compile errors — `poly_event_slug_for_meeting`, `normalize_poly_event` undefined.

- [ ] **Step 3: Implement the helpers**

Add to `src/adapters/fomc.rs`:

```rust
use crate::polymarket::{GammaEvent, GammaEventMarket};

/// Build the Polymarket Gamma event slug for an FOMC meeting.
/// Pattern: `fomc-decision-<month-name>-<year>` (lowercase month name).
pub(crate) fn poly_event_slug_for_meeting(date: NaiveDate) -> String {
    let month = match date.month() {
        1 => "january", 2 => "february", 3 => "march", 4 => "april",
        5 => "may", 6 => "june", 7 => "july", 8 => "august",
        9 => "september", 10 => "october", 11 => "november", 12 => "december",
        _ => "unknown",
    };
    format!("fomc-decision-{}-{}", month, date.year())
}

/// Build CanonicalMarkets from a Gamma neg-risk event for a given meeting,
/// using `anchor_bps` to convert each outcome's delta into an absolute floor_bps.
/// Outcomes whose label `parse_fomc_delta_bps` rejects are logged + skipped.
pub(crate) fn normalize_poly_event(
    event: &GammaEvent,
    meeting_date: NaiveDate,
    anchor_bps: i32,
) -> Vec<CanonicalMarket> {
    let mut out = Vec::with_capacity(event.markets.len());
    for m in &event.markets {
        let Some(delta) = parse_fomc_delta_bps(&m.question) else {
            tracing::warn!("[FOMC] unparseable poly outcome label: {:?}", m.question);
            continue;
        };
        let floor_bps = anchor_bps + delta;
        let title: Arc<str> = Arc::from(m.question.as_str());

        out.push(CanonicalMarket {
            event_type: EventType::Fomc,
            underlier: Underlier::FomcRateBand { meeting_date, floor_bps },
            time_window: TimeWindow { event_at: None, settles_at: None },
            venue: Venue {
                platform: Platform::Polymarket,
                kalshi_event_ticker: None,
                kalshi_market_ticker: None,
                poly_slug: Some(Arc::from(m.slug.as_str())),
                poly_yes_token: Some(Arc::from(m.yes_token.as_str())),
                poly_no_token: Some(Arc::from(m.no_token.as_str())),
                poly_condition_id: Some(Arc::from(m.condition_id.as_str())),
            },
            category: PolyCategory::Economics,
            raw_title: title,
            raw_description: Arc::from(""),
            adapter_version: 1,
        });
    }
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --lib adapters::fomc::poly_normalize_tests`
Expected: PASS, two tests green.

- [ ] **Step 5: Wire poly-side fetch into `FomcAdapter::normalize`**

Replace the `normalize` body again:

```rust
async fn normalize(&self) -> Result<NormalizedBatch> {
    let events = self.kalshi.get_events(FOMC_KALSHI_SERIES, 50).await?;
    if events.is_empty() {
        tracing::info!("[FOMC] no open KXFED events; skipping");
        return Ok(NormalizedBatch { kalshi: vec![], poly: vec![] });
    }

    let anchor_bps = match self.resolve_anchor_bps(&events).await {
        Ok(bps) => bps,
        Err(e) => {
            tracing::error!("[FOMC] anchor unavailable, emitting zero pairs: {}", e);
            return Ok(NormalizedBatch { kalshi: vec![], poly: vec![] });
        }
    };

    let mut kalshi_canon: Vec<CanonicalMarket> = Vec::new();
    let mut poly_canon: Vec<CanonicalMarket> = Vec::new();

    for ev in &events {
        let markets = match self.kalshi.get_markets(&ev.event_ticker).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("[FOMC] get_markets {} failed: {}", ev.event_ticker, e);
                continue;
            }
        };
        let canon = normalize_kalshi_markets(ev, &markets);
        // Reuse the meeting_date from the first emitted market to drive the slug.
        let Some(meeting_date) = canon.first().and_then(|c| match &c.underlier {
            Underlier::FomcRateBand { meeting_date, .. } => Some(*meeting_date),
            _ => None,
        }) else {
            kalshi_canon.extend(canon);
            continue;
        };
        kalshi_canon.extend(canon);

        let slug = poly_event_slug_for_meeting(meeting_date);
        match self.gamma.lookup_event(&slug).await {
            Ok(Some(poly_ev)) => {
                poly_canon.extend(normalize_poly_event(&poly_ev, meeting_date, anchor_bps));
            }
            Ok(None) => tracing::warn!("[FOMC] no poly event at slug {}", slug),
            Err(e) => tracing::warn!("[FOMC] poly event lookup {} failed: {}", slug, e),
        }
    }

    tracing::info!(
        "[FOMC] normalized: {} kalshi markets, {} poly outcomes (anchor {} bps)",
        kalshi_canon.len(),
        poly_canon.len(),
        anchor_bps
    );
    Ok(NormalizedBatch { kalshi: kalshi_canon, poly: poly_canon })
}
```

- [ ] **Step 6: Run the full test suite**

Run: `cargo test --lib`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add src/adapters/fomc.rs
git commit -m "Implement Polymarket-side FOMC normalization with anchor + delta math"
```

---

## Task 9: Wire `FomcAdapter` into `DiscoveryClient`

**Files:**
- Modify: `src/main.rs:139-152`

- [ ] **Step 1: Read `src/main.rs:139-160` to confirm the current adapter wiring**

Run: `sed -n '139,160p' src/main.rs`
Expected: see `let sports_adapter: Arc<dyn adapters::EventAdapter> = ...; let discovery = DiscoveryClient::new(vec![sports_adapter]);`.

- [ ] **Step 2: Modify the adapter wiring**

Replace the block (around lines 145-152) with:

```rust
let sports_adapter: Arc<dyn adapters::EventAdapter> =
    Arc::new(adapters::sports::SportsAdapter::new(
        kalshi_api.clone(),
        Arc::new(team_cache),
        ENABLED_LEAGUES.to_vec(),
    ));

let mut active_adapters: Vec<Arc<dyn adapters::EventAdapter>> = vec![sports_adapter];

if config::fomc_enabled() {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client for FomcAdapter");
    let fomc_adapter: Arc<dyn adapters::EventAdapter> =
        Arc::new(adapters::fomc::FomcAdapter::new(
            kalshi_api.clone(),
            Arc::new(polymarket::GammaClient::new()),
            http,
            config::fred_api_key(),
        ));
    active_adapters.push(fomc_adapter);
    info!("🏛️ FomcAdapter enabled (set FOMC_ENABLED=0 to disable)");
} else {
    info!("🏛️ FomcAdapter disabled via FOMC_ENABLED=0");
}

let discovery = DiscoveryClient::new(active_adapters);
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build`
Expected: clean build. If `polymarket::GammaClient` is not in scope at `main.rs`, add the appropriate `use` near the top.

- [ ] **Step 4: Smoke-test the adapter loads (no network)**

Run: `FOMC_ENABLED=0 cargo build --release` and confirm the disabled branch compiles.
Run: `FOMC_ENABLED=1 cargo build --release` and confirm the enabled branch compiles.

- [ ] **Step 5: Commit**

```bash
git add src/main.rs
git commit -m "Register FomcAdapter in DiscoveryClient behind FOMC_ENABLED flag"
```

---

## Task 10: Detection-only execution gate

**Files:**
- Modify: `src/execution.rs`

Drop FOMC pairs at the execution boundary unless `EXEC_ALLOW_FOMC=1`. The pair carries `MatchSource::Structured { adapter }` (PR 1), so the gate is a one-line check.

- [ ] **Step 1: Locate the execution entry point**

Run: `grep -n "fn process_request\|FastExecutionRequest\|MarketPair" src/execution.rs | head -20`

Find the function that receives a `FastExecutionRequest` and looks up the `MarketPair`. The gate goes immediately after the lookup, before any order construction.

- [ ] **Step 2: Write the failing test**

Append to `src/execution.rs` (inside an existing or new `#[cfg(test)] mod tests`):

```rust
#[cfg(test)]
mod gate_tests {
    use super::*;
    use crate::fees::{MatchSource, PolyCategory};

    fn mk_pair(adapter: &str) -> crate::types::MarketPair {
        crate::types::MarketPair {
            pair_id: std::sync::Arc::from("test-pair"),
            league: std::sync::Arc::from("fomc"),
            market_type: crate::types::MarketType::Moneyline,
            description: std::sync::Arc::from("desc"),
            kalshi_event_ticker: std::sync::Arc::from("KXFED-26APR"),
            kalshi_market_ticker: std::sync::Arc::from("KXFED-26APR-T425"),
            poly_slug: std::sync::Arc::from("slug"),
            poly_yes_token: std::sync::Arc::from("yes"),
            poly_no_token: std::sync::Arc::from("no"),
            poly_condition_id: std::sync::Arc::from("0xCID"),
            line_value: None,
            team_suffix: None,
            category: PolyCategory::Economics,
            match_source: MatchSource::Structured { adapter: adapter.into() },
        }
    }

    #[test]
    fn fomc_pair_is_blocked_by_default() {
        std::env::remove_var("EXEC_ALLOW_FOMC");
        let pair = mk_pair("fomc");
        assert!(should_block_for_detection_only(&pair));
    }

    #[test]
    fn fomc_pair_passes_when_gate_enabled() {
        std::env::set_var("EXEC_ALLOW_FOMC", "1");
        let pair = mk_pair("fomc");
        assert!(!should_block_for_detection_only(&pair));
        std::env::remove_var("EXEC_ALLOW_FOMC");
    }

    #[test]
    fn sports_pair_is_never_blocked() {
        std::env::remove_var("EXEC_ALLOW_FOMC");
        let pair = mk_pair("sports");
        assert!(!should_block_for_detection_only(&pair));
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib execution::gate_tests -- --test-threads=1`
Expected: compile error — `should_block_for_detection_only` undefined.

- [ ] **Step 4: Implement the gate function**

Add to `src/execution.rs` (free function, near the top of the file):

```rust
/// Returns true if a pair should be silently dropped at execution time
/// because it comes from an adapter still in detection-only rollout.
///
/// Today: `fomc` is detection-only unless `EXEC_ALLOW_FOMC=1`.
pub(crate) fn should_block_for_detection_only(pair: &crate::types::MarketPair) -> bool {
    use crate::fees::MatchSource;
    let MatchSource::Structured { adapter } = &pair.match_source else {
        return false;
    };
    if adapter == "fomc" && !crate::config::exec_allow_fomc() {
        return true;
    }
    false
}
```

- [ ] **Step 5: Wire the gate into `process_request`**

Inside `process_request` (or whichever function currently invokes the circuit breaker for an `ExecutionRequest`), insert immediately after the pair is looked up and before any order is constructed:

```rust
if should_block_for_detection_only(&pair) {
    tracing::info!(
        "[EXEC] 🛑 detection-only: dropping pair {} (adapter={:?}). Set EXEC_ALLOW_FOMC=1 to enable.",
        pair.pair_id, pair.match_source
    );
    return Ok(());
}
```

(The `return Ok(())` shape assumes the caller's signature; adapt to the actual return type.)

- [ ] **Step 6: Run tests**

Run: `cargo test --lib execution::gate_tests -- --test-threads=1`
Expected: PASS, three tests green.
Run: `cargo test --lib`
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add src/execution.rs
git commit -m "Gate FOMC pair execution behind EXEC_ALLOW_FOMC (detection-only by default)"
```

---

## Task 11: Live acceptance smoke test + acceptance log

**Files:**
- Create: `docs/notes/2026-04-21-fomc-first-meeting.md`

This task is human-driven verification, not automation. The acceptance criterion from spec §7 PR 2 is: *"for the next FOMC meeting, discover N-1 pairs for N Kalshi bands where Polymarket has the intervening delta; anchor correctly resolved from FRED; unmatched tail bands skipped cleanly."*

The next FOMC meeting after 2026-04-29 is the May meeting (typically late May / early June). Capture the run output as evidence.

- [ ] **Step 1: Run the bot in detection-only mode against live data**

Set the environment:

```bash
export FOMC_ENABLED=1
export EXEC_ALLOW_FOMC=0      # detection only
export FORCE_DISCOVERY=1
# export FRED_API_KEY=...      # optional
```

Then run:

```bash
cargo run --release 2>&1 | tee /tmp/fomc-smoke.log
```

Let it run until "📊 Market discovery complete" and the per-market fee resolver prints. Then `Ctrl-C`.

- [ ] **Step 2: Extract acceptance evidence**

```bash
grep -E "Adapter 'fomc'|FOMC|fomc-decision|KXFED" /tmp/fomc-smoke.log
```

You're looking for these lines:
- `🔍 Adapter 'fomc' normalizing...`
- `[FOMC] anchor <NNN> bps from FRED DFEDTARL` (or from Kalshi metadata, if Kalshi adds the field)
- `[FOMC] normalized: K kalshi markets, P poly outcomes (anchor <NNN> bps)`
- `✅ Adapter 'fomc' produced X pairs`
- For each unparseable poly outcome: `[FOMC] unparseable poly outcome label: ...`
- For each Kalshi tail band with no poly counterpart: silent (correct behavior — no log spam).

- [ ] **Step 3: Record findings**

Create `docs/notes/2026-04-21-fomc-first-meeting.md`:

```markdown
# FOMC first-live-meeting smoke test (PR 2 acceptance)

**Date run:** YYYY-MM-DD
**Meeting under test:** KXFED-<YYMMM>
**Anchor source:** Kalshi metadata | FRED DFEDTARL  ← circle one
**Anchor value:** NNN bps (X.XX%)

## Pair counts
- Kalshi `KXFED-...` markets discovered: K
- Polymarket child markets in `fomc-decision-<month>-<year>`: M
- Pairs emitted by `pair_batch`: P
- Expected per spec: P == M (every poly outcome should match exactly one Kalshi floor band; trailing Kalshi bands without a poly counterpart drop silently)

## Tail-band check
- Kalshi bands above the highest poly outcome: list them — these should not appear in the pair list.

## Unparseable labels (if any)
- List of poly outcome `question` strings that failed `parse_fomc_delta_bps`. If any are real outcomes (not test/garbage rows), file a follow-up to extend the parser.

## Anchor sanity
- Cross-check the resolved anchor against https://fred.stlouisfed.org/series/DFEDTARL — should match the most recent observation to within 1 bp.

## Detection-only gate
- Confirm: with `EXEC_ALLOW_FOMC=0` (default), execution log shows "🛑 detection-only: dropping pair ..." for any FOMC pair that crosses ARB_THRESHOLD.
- Toggle `EXEC_ALLOW_FOMC=1` and confirm gate releases. (Do not actually execute — exit before the order goes through.)
```

Fill in the placeholders from the run output.

- [ ] **Step 4: Commit**

```bash
git add docs/notes/2026-04-21-fomc-first-meeting.md
git commit -m "Record FOMC PR 2 acceptance smoke test results"
```

---

## Task 12: Resolve spec open question §8.2 + final docs

**Files:**
- Modify: `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md`

- [ ] **Step 1: Update spec open question 2**

In `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md`, section §8 Open Questions, replace question 2 with the empirical finding:

```markdown
2. **Fed anchor via Kalshi metadata.** ✅ Resolved 2026-04-29 during PR 2 implementation: the `KXFED` event schema does not currently expose a current-rate field. `try_anchor_from_kalshi_event` returns None as a stub and FRED `DFEDTARL` is the load-bearing anchor source. Re-evaluate if Kalshi's event schema gains a rate field; the stub is intentionally kept so a future change is one-line.
```

- [ ] **Step 2: Verify the full test suite is green and the workspace is clean**

```bash
cargo test --lib
cargo build --release
git status
```

Expected: all tests pass, clean release build, only the spec doc is staged.

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-04-21-multi-category-matching-design.md
git commit -m "Resolve spec §8.2: Kalshi event metadata does not expose fed-funds anchor"
```

---

## Self-Review

**Spec coverage check (against §4.5 + §7 PR 2):**
- ✅ "Fetch Kalshi events in series KXFED" — Task 6
- ✅ "Resolve anchor (Kalshi meta first, FRED fallback)" — Tasks 2, 7
- ✅ "Normalize Kalshi side with floor_bps = (floor_strike * 100) as i32" — Task 6
- ✅ "Fetch Polymarket neg-risk event by slug `fomc-decision-<month>-<year>`" — Tasks 4, 8
- ✅ "Parse '25 bps cut'/'No change'/'25 bps hike'" — Task 3
- ✅ "pair_batch joins on (Fomc, FomcRateBand{meeting_date, floor_bps})" — already in PR 1, exercised in Task 11
- ✅ "Detection-only for first scheduled FOMC meeting" — Task 10
- ✅ "Acceptance: discover N-1 pairs for N Kalshi bands; tail bands skip cleanly" — Task 11

**Type consistency check:**
- `FomcAdapter::new(kalshi, gamma, http, fred_api_key)` — same signature in Task 5 (declaration) and Task 9 (caller).
- `parse_fomc_delta_bps(&str) -> Option<i32>` — declared Task 3, used Task 8.
- `parse_meeting_date_from_event_ticker(&str) -> Option<NaiveDate>` — declared Task 6, used internally.
- `poly_event_slug_for_meeting(NaiveDate) -> String` — declared Task 8, used in normalize.
- `try_anchor_from_kalshi_event(&KalshiEvent) -> Option<i32>` — declared Task 7.
- `should_block_for_detection_only(&MarketPair) -> bool` — declared Task 10, used inside `process_request`.
- `parse_lower_bound_bps(&str) -> Result<i32>` and `fetch_fed_lower_bound_bps(http, key) -> Result<i32>` — declared Task 2, used Task 7.
- `parse_gamma_event_response(&str) -> Result<Option<GammaEvent>>` and `GammaClient::lookup_event(slug) -> Result<Option<GammaEvent>>` — declared Task 4, used Task 8.

**Placeholder scan:** All steps contain explicit code or explicit shell commands. The one judgement call left to the executor is in Task 5 step 4 (whether `KalshiClient` already has a no-creds ctor) — this is intentionally flexible because the existing ctor's shape isn't pinned by the plan.

**Cache compatibility:** PR 2 adds no fields to `MarketPair`. Pre-PR-2 caches load fine.

---

## Notes for the executor

- **Always use `--test-threads=1`** for tests in `config::tests`, `execution::gate_tests`, and any future test that mutates env vars. Three tasks (1, 10, parts of 11) flag this; respect it.
- **Run `cargo test --lib` after every code-modifying task.** This plan only spells out the focused test name in each task; running the full suite after each task catches accidental regressions cheaply.
- **Do not execute live FOMC orders during Task 11.** The whole point of detection-only rollout is to not flip a switch on uncalibrated code. If your run accidentally has `EXEC_ALLOW_FOMC=1` and `DRY_RUN=0`, kill it before the first arb fires.
- **If a step asks you to add code that conflicts with what's already there**, prefer reading the existing code and adapting the new code to fit. The plan is detailed but cannot anticipate every line drift since the spec was written.

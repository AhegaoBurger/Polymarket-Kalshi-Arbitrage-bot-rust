# Manual Polymarket Balance Override Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow operating with a manually-supplied Polymarket balance via `POLY_BALANCE_USDC` env var, decremented locally on each submitted order, while leaving Kalshi's live refresh untouched.

**Architecture:** A pure `parse_poly_balance_env` helper turns the env var into USDC micros. `refresh_once` and `spawn_refresh_task` gain a `refresh_poly: bool` flag — when false, the Polymarket fetch is skipped and the cache value is owned exclusively by `commit_poly`. `main.rs` reads the env var once at startup, primes the cache directly via `set_poly_usdc_micros`, and passes the flag to both calls. Unsetting the env var fully restores live-refresh behaviour.

**Tech Stack:** Rust, Tokio, anyhow, tracing.

---

## File Structure

- **Modify:** `src/balance.rs` — add `parse_poly_balance_env` helper + `#[cfg(test)] mod tests`, add `refresh_poly: bool` parameter to `refresh_once` and `spawn_refresh_task`.
- **Modify:** `src/main.rs` — read `POLY_BALANCE_USDC`, prime `BalanceCache::poly_usdc_micros` if set, log mode, pass flag to both balance functions.

No new files. No new dependencies.

---

### Task 1: Add `parse_poly_balance_env` helper with unit tests

**Files:**
- Modify: `src/balance.rs` (append to end of file, before any existing `#[cfg(test)] mod tests` if one exists)

- [ ] **Step 1: Write the failing tests**

Append to `src/balance.rs`:

```rust
/// Parse the `POLY_BALANCE_USDC` env-var value into USDC micros.
///
/// Returns `None` for missing or unparseable values. Whitespace around the
/// number is tolerated. Negative values are clamped to 0 so the cache never
/// holds a value that would underflow `commit_poly`.
pub fn parse_poly_balance_env(raw: Option<&str>) -> Option<u64> {
    let s = raw?.trim();
    if s.is_empty() {
        return None;
    }
    let usdc = s.parse::<f64>().ok()?;
    if !usdc.is_finite() {
        return None;
    }
    let micros = (usdc.max(0.0) * 1_000_000.0).round();
    Some(micros as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_none_when_unset() {
        assert_eq!(parse_poly_balance_env(None), None);
    }

    #[test]
    fn parse_none_when_empty_or_whitespace() {
        assert_eq!(parse_poly_balance_env(Some("")), None);
        assert_eq!(parse_poly_balance_env(Some("   ")), None);
    }

    #[test]
    fn parse_decimal_dollars_to_micros() {
        assert_eq!(parse_poly_balance_env(Some("23.87")), Some(23_870_000));
        assert_eq!(parse_poly_balance_env(Some("10")), Some(10_000_000));
        assert_eq!(parse_poly_balance_env(Some("0.000001")), Some(1));
    }

    #[test]
    fn parse_tolerates_surrounding_whitespace() {
        assert_eq!(parse_poly_balance_env(Some("  10.00  ")), Some(10_000_000));
    }

    #[test]
    fn parse_returns_none_for_garbage() {
        assert_eq!(parse_poly_balance_env(Some("nope")), None);
        assert_eq!(parse_poly_balance_env(Some("1.2.3")), None);
    }

    #[test]
    fn parse_clamps_negative_to_zero() {
        assert_eq!(parse_poly_balance_env(Some("-5.00")), Some(0));
    }

    #[test]
    fn parse_rejects_nan_and_inf() {
        assert_eq!(parse_poly_balance_env(Some("NaN")), None);
        assert_eq!(parse_poly_balance_env(Some("inf")), None);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib balance::tests`

Expected: compile error — `parse_poly_balance_env` not found (if you wrote tests first) OR all tests pass (if you wrote helper + tests together; that's also fine). The point is to prove the tests *exercise* the helper. If tests pass on first run, deliberately break the helper (e.g. multiply by 100 instead of 1_000_000), re-run to confirm failure, then restore it.

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test --lib balance::tests`

Expected: all 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add src/balance.rs
git commit -m "$(cat <<'EOF'
Add parse_poly_balance_env helper with unit tests

Pure function that converts the POLY_BALANCE_USDC env var
(decimal dollars) to USDC micros, handling missing/empty/garbage
input and clamping negatives to zero.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Add `refresh_poly` flag to `refresh_once` and `spawn_refresh_task`

**Files:**
- Modify: `src/balance.rs:121-165` (`refresh_once` + `spawn_refresh_task`)
- Modify: `src/main.rs:148` (existing `refresh_once` call) and `src/main.rs:156-160` (existing `spawn_refresh_task` call)

This task changes signatures and updates *both* callsites in the same commit so the build stays green. No behaviour change yet — both callsites pass `true` to preserve current behaviour.

- [ ] **Step 1: Update `refresh_once` signature and body**

Replace `src/balance.rs:121-146` with:

```rust
/// Fetch both exchange balances and store into the cache. Used for the startup
/// priming call (must succeed) and each tick of the background refresh task
/// (best-effort — failures are logged but don't abort the loop).
///
/// `refresh_poly` controls whether the Polymarket leg is fetched. Pass `false`
/// when the Polymarket value is being managed manually (via
/// `POLY_BALANCE_USDC`) — the cache value is then owned exclusively by
/// `commit_poly` and survives across refresh ticks.
pub async fn refresh_once(
    cache: &BalanceCache,
    kalshi: &KalshiApiClient,
    poly: &SharedAsyncClient,
    refresh_poly: bool,
) -> Result<()> {
    if refresh_poly {
        let (kalshi_res, poly_res) = tokio::join!(
            kalshi.fetch_balance_cents(),
            poly.fetch_poly_balance_usdc_micros(),
        );

        match kalshi_res {
            Ok(cents) => {
                cache.set_kalshi_cents(cents);
                debug!("[BALANCE] Kalshi: {} cents (${:.2})", cents, cents as f64 / 100.0);
            }
            Err(e) => warn!("[BALANCE] Kalshi fetch failed: {}", e),
        }
        match poly_res {
            Ok(micros) => {
                cache.set_poly_usdc_micros(micros);
                debug!("[BALANCE] Poly: {} micros (${:.2})", micros, micros as f64 / 1_000_000.0);
            }
            Err(e) => warn!("[BALANCE] Poly fetch failed: {}", e),
        }
    } else {
        match kalshi.fetch_balance_cents().await {
            Ok(cents) => {
                cache.set_kalshi_cents(cents);
                debug!("[BALANCE] Kalshi: {} cents (${:.2})", cents, cents as f64 / 100.0);
            }
            Err(e) => warn!("[BALANCE] Kalshi fetch failed: {}", e),
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Update `spawn_refresh_task` signature and body**

Replace `src/balance.rs:148-165` with:

```rust
/// Spawn a background task that refreshes the balance cache every
/// `REFRESH_INTERVAL`. Non-fatal: failures are warned and the loop continues.
///
/// `refresh_poly` is forwarded to `refresh_once` on every tick — see its docs
/// for manual-mode semantics.
pub fn spawn_refresh_task(
    cache: Arc<BalanceCache>,
    kalshi: Arc<KalshiApiClient>,
    poly: Arc<SharedAsyncClient>,
    refresh_poly: bool,
) {
    tokio::spawn(async move {
        info!(
            "[BALANCE] Refresh task started ({:?} interval, refresh_poly={})",
            REFRESH_INTERVAL, refresh_poly,
        );
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let _ = refresh_once(&cache, &kalshi, &poly, refresh_poly).await;
        }
    });
}
```

- [ ] **Step 3: Update `main.rs` callsites to pass `true`**

In `src/main.rs`, find the line:

```rust
    match balance::refresh_once(&balance_cache, &kalshi_api, &poly_async).await {
```

Replace with:

```rust
    match balance::refresh_once(&balance_cache, &kalshi_api, &poly_async, true).await {
```

And find:

```rust
    balance::spawn_refresh_task(
        balance_cache.clone(),
        kalshi_api.clone(),
        poly_async.clone(),
    );
```

Replace with:

```rust
    balance::spawn_refresh_task(
        balance_cache.clone(),
        kalshi_api.clone(),
        poly_async.clone(),
        true,
    );
```

- [ ] **Step 4: Verify the project builds**

Run: `cargo build`

Expected: builds cleanly with no errors. (Warnings about unused `refresh_poly` parameter are fine if any appear — the next task wires it up.)

- [ ] **Step 5: Re-run the balance unit tests**

Run: `cargo test --lib balance::`

Expected: all 7 tests from Task 1 still pass — no regressions.

- [ ] **Step 6: Commit**

```bash
git add src/balance.rs src/main.rs
git commit -m "$(cat <<'EOF'
Thread refresh_poly flag through balance refresh

Add refresh_poly: bool to refresh_once and spawn_refresh_task. When
false, the Polymarket fetch is skipped and the cache value is left
to be managed by commit_poly. Both callsites in main.rs pass true to
preserve current behaviour; manual-mode wiring lands in the next
commit.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Wire `POLY_BALANCE_USDC` env var into startup

**Files:**
- Modify: `src/main.rs:147-160` (the balance prime + spawn block)

- [ ] **Step 1: Replace the balance startup block**

In `src/main.rs`, find the existing block:

```rust
    // Balance cache: prime at startup (blocking) so the first opportunity
    // doesn't see zeros; then spawn a background refresh task.
    let balance_cache = Arc::new(balance::BalanceCache::new());
    match balance::refresh_once(&balance_cache, &kalshi_api, &poly_async, true).await {
        Ok(()) => info!(
            "[BALANCE] Primed at startup: Kalshi=${:.2}, Poly=${:.2}",
            balance_cache.kalshi_cents() as f64 / 100.0,
            balance_cache.poly_usdc_micros() as f64 / 1_000_000.0,
        ),
        Err(e) => warn!("[BALANCE] Startup prime failed: {} (continuing with zero cache)", e),
    }
    balance::spawn_refresh_task(
        balance_cache.clone(),
        kalshi_api.clone(),
        poly_async.clone(),
        true,
    );
```

Replace with:

```rust
    // Balance cache: prime at startup (blocking) so the first opportunity
    // doesn't see zeros; then spawn a background refresh task.
    //
    // POLY_BALANCE_USDC is an escape hatch for the CLOB v2 auth outage: when
    // set, the Polymarket value is taken from the env var and never refreshed
    // — commit_poly owns it for the rest of the process. Unset to restore
    // live refresh.
    let balance_cache = Arc::new(balance::BalanceCache::new());
    let manual_poly_micros = balance::parse_poly_balance_env(
        std::env::var("POLY_BALANCE_USDC").ok().as_deref(),
    );
    let refresh_poly = manual_poly_micros.is_none();

    if let Some(micros) = manual_poly_micros {
        balance_cache.set_poly_usdc_micros(micros);
        info!(
            "[BALANCE] Polymarket: manual mode from POLY_BALANCE_USDC = ${:.2} (auto-refresh disabled)",
            micros as f64 / 1_000_000.0,
        );
    }

    match balance::refresh_once(&balance_cache, &kalshi_api, &poly_async, refresh_poly).await {
        Ok(()) => info!(
            "[BALANCE] Primed at startup: Kalshi=${:.2}, Poly=${:.2}",
            balance_cache.kalshi_cents() as f64 / 100.0,
            balance_cache.poly_usdc_micros() as f64 / 1_000_000.0,
        ),
        Err(e) => warn!("[BALANCE] Startup prime failed: {} (continuing with zero cache)", e),
    }
    balance::spawn_refresh_task(
        balance_cache.clone(),
        kalshi_api.clone(),
        poly_async.clone(),
        refresh_poly,
    );
```

- [ ] **Step 2: Build the project**

Run: `cargo build`

Expected: builds cleanly with no errors and no new warnings.

- [ ] **Step 3: Run the full test suite**

Run: `cargo test`

Expected: all tests pass — no regressions in unit tests, integration tests, or doctests. The Task 1 `balance::tests::*` cases must all be green.

- [ ] **Step 4: Manual smoke test — manual mode**

Run with the env var set (DRY_RUN=1 to keep it safe):

```bash
DRY_RUN=1 POLY_BALANCE_USDC=10.00 cargo run 2>&1 | grep -E "BALANCE|POLYMARKET" | head -20
```

Expected log lines (order may vary):
- `[BALANCE] Polymarket: manual mode from POLY_BALANCE_USDC = $10.00 (auto-refresh disabled)`
- `[BALANCE] Primed at startup: Kalshi=$X.XX, Poly=$10.00`
- `[BALANCE] Refresh task started (30s interval, refresh_poly=false)`

Confirm the Poly value in the "Primed at startup" line is exactly `$10.00` (not zero, not the broken-API value).

Kill with Ctrl-C after confirming.

- [ ] **Step 5: Manual smoke test — live mode regression check**

Run *without* the env var:

```bash
DRY_RUN=1 cargo run 2>&1 | grep -E "BALANCE" | head -10
```

Expected:
- No `manual mode` line.
- `[BALANCE] Refresh task started (30s interval, refresh_poly=true)`.
- The Polymarket fetch attempt happens (will likely log the existing CLOB v2 warning — that is the bug we are routing around, not something to fix here).

Kill with Ctrl-C.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "$(cat <<'EOF'
Wire POLY_BALANCE_USDC env var into startup

When set, prime the balance cache from the env var, skip the
Polymarket leg of refresh, and let commit_poly own the value for the
rest of the process. Unsetting restores live-refresh behaviour with
no code changes — useful once CLOB v2 auth is fixed upstream.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review

**Spec coverage:**
- "New env var `POLY_BALANCE_USDC` decimal USD" → Task 1 helper + Task 3 wiring. ✓
- "Add `refresh_poly: bool` to `refresh_once` and `spawn_refresh_task`" → Task 2. ✓
- "Skip `fetch_poly_balance_usdc_micros` when flag is false; Kalshi unchanged" → Task 2 Step 1 (the `else` branch only awaits Kalshi). ✓
- "Prime cache from env var on startup" → Task 3 Step 1 (`set_poly_usdc_micros(micros)` before the `refresh_once` prime). ✓
- "Log a single info line on startup naming the active mode" → Task 3 Step 1 (`manual mode from POLY_BALANCE_USDC = $X.XX`). ✓
- "Reversible: unsetting restores live refresh" → Task 3 Step 1 (`refresh_poly = manual_poly_micros.is_none()`). ✓
- "Existing `BalanceCache` unit tests unaffected" → No changes to `BalanceCache` struct or methods. ✓
- "Manual verification (set env, observe primed value, observe refresh skipping)" → Task 3 Steps 4–5. ✓

**Placeholder scan:** No "TBD" / "TODO" / "implement later". Every code step contains the actual code. Every command has expected output. ✓

**Type consistency:**
- `parse_poly_balance_env(raw: Option<&str>) -> Option<u64>` — defined in Task 1, called in Task 3 Step 1 with `std::env::var("POLY_BALANCE_USDC").ok().as_deref()` (which is `Option<&str>`). ✓
- `set_poly_usdc_micros(micros: u64)` — already exists in `BalanceCache` (src/balance.rs:69), called in Task 3 with `micros: u64` from helper return. ✓
- `refresh_poly: bool` parameter name identical in Task 2 (both function signatures + log line) and Task 3 (variable name + both callsites). ✓
- `refresh_once` signature: `(cache, kalshi, poly, refresh_poly)` — same order in Task 2 definition and Task 3 callsite. ✓
- `spawn_refresh_task` signature: `(cache, kalshi, poly, refresh_poly)` — same order in Task 2 definition and Task 3 callsite. ✓

//! Shared, lock-free cache of spendable balances on both exchanges.
//!
//! The execution hot path reads this cache to cap `max_contracts` by what the
//! wallet can actually afford, not just by orderbook depth. Without this cap,
//! the bot submits orders that the exchange 400s with "insufficient balance" —
//! wasted round-trips and noisy logs. With it, under-funded opportunities are
//! skipped cleanly before any network call.
//!
//! Concurrency model:
//!   - Atomic reads/writes, no locks → safe to hit from every execution task.
//!   - Background refresh task owns truth: every 30s, it calls the authoritative
//!     exchange endpoints and overwrites the atomics.
//!   - Between refreshes, execution tasks `commit_*` (fetch_sub) to reserve the
//!     dollars they're about to spend, so two concurrent arbs can't both see
//!     the same balance and each try to spend it.
//!   - Pessimism: we decrement on submit, not on fill. An FAK that misses
//!     "overcharges" the cache until the next refresh overwrites it. Safer
//!     than under-charging (which would allow over-commit).

use crate::kalshi::KalshiApiClient;
use crate::polymarket_clob::SharedAsyncClient;
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Polymarket minimum order value. Orders below this get 400'd by the CLOB
/// with "order size is less than minimum." Currently $5 on Sports markets.
pub const POLY_MIN_ORDER_CENTS: i64 = 500;

/// Kalshi minimum order: at least 1 contract.
pub const KALSHI_MIN_CONTRACTS: i64 = 1;

/// Refresh cadence for background balance fetch. 30s balances rate-limit risk
/// (Kalshi) against staleness after fills.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

pub struct BalanceCache {
    /// Kalshi cash available to trade, in USD cents. 326 = $3.26.
    kalshi_cents: AtomicI64,
    /// Polymarket USDC available on the collateral address, in 6-decimal micros.
    /// 23_870_001 = $23.870001.
    poly_usdc_micros: AtomicU64,
}

impl BalanceCache {
    pub fn new() -> Self {
        Self {
            kalshi_cents: AtomicI64::new(0),
            poly_usdc_micros: AtomicU64::new(0),
        }
    }

    #[inline]
    pub fn kalshi_cents(&self) -> i64 {
        self.kalshi_cents.load(Ordering::Relaxed)
    }

    #[inline]
    pub fn poly_usdc_micros(&self) -> u64 {
        self.poly_usdc_micros.load(Ordering::Relaxed)
    }

    pub fn set_kalshi_cents(&self, cents: i64) {
        self.kalshi_cents.store(cents, Ordering::Relaxed);
    }

    pub fn set_poly_usdc_micros(&self, micros: u64) {
        self.poly_usdc_micros.store(micros, Ordering::Relaxed);
    }

    /// How many contracts the Kalshi balance can afford at `price_cents` per contract.
    #[inline]
    pub fn kalshi_max_contracts(&self, price_cents: i64) -> i64 {
        if price_cents <= 0 { return 0; }
        self.kalshi_cents() / price_cents
    }

    /// How many contracts the Polymarket balance can afford at `price_cents` per contract.
    /// Internally converts ¢ to USDC micros (1¢ = 10_000 micros).
    #[inline]
    pub fn poly_max_contracts(&self, price_cents: i64) -> i64 {
        if price_cents <= 0 { return 0; }
        let cost_per_contract_micros = (price_cents as u64) * 10_000;
        if cost_per_contract_micros == 0 { return 0; }
        (self.poly_usdc_micros() / cost_per_contract_micros) as i64
    }

    /// Reserve `cents` on Kalshi before submitting an order. Subtracts from the
    /// cache; the next refresh will overwrite with ground truth.
    pub fn commit_kalshi(&self, cents: i64) {
        if cents <= 0 { return; }
        let prev = self.kalshi_cents.fetch_sub(cents, Ordering::Relaxed);
        if prev < cents {
            // Guard against sliding into negative territory if refresh hasn't
            // caught up. Clamp back to zero so max_contracts returns 0.
            self.kalshi_cents.store(0, Ordering::Relaxed);
        }
    }

    /// Reserve `micros` on Polymarket before submitting an order.
    pub fn commit_poly(&self, micros: u64) {
        if micros == 0 { return; }
        let prev = self.poly_usdc_micros.fetch_sub(micros, Ordering::Relaxed);
        if prev < micros {
            self.poly_usdc_micros.store(0, Ordering::Relaxed);
        }
    }
}

impl Default for BalanceCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Fetch both exchange balances and store into the cache. Used for the startup
/// priming call (must succeed) and each tick of the background refresh task
/// (best-effort — failures are logged but don't abort the loop).
pub async fn refresh_once(
    cache: &BalanceCache,
    kalshi: &KalshiApiClient,
    poly: &SharedAsyncClient,
) -> Result<()> {
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
    Ok(())
}

/// Spawn a background task that refreshes the balance cache every
/// `REFRESH_INTERVAL`. Non-fatal: failures are warned and the loop continues.
pub fn spawn_refresh_task(
    cache: Arc<BalanceCache>,
    kalshi: Arc<KalshiApiClient>,
    poly: Arc<SharedAsyncClient>,
) {
    tokio::spawn(async move {
        info!("[BALANCE] Refresh task started ({:?} interval)", REFRESH_INTERVAL);
        let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let _ = refresh_once(&cache, &kalshi, &poly).await;
        }
    });
}

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

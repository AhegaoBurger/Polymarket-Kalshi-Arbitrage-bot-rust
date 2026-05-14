//! Market discovery — 5-minute window detection + Gamma event lookup.
//! Reference: market.py.

use anyhow::Result;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::polymarket::gamma::{GammaClient, ActiveMarket};

/// Period in seconds. Only 300 (5-min) and 900 (15-min) are valid per market.py:15.
pub fn period_seconds(period_minutes: u32) -> u32 {
    period_minutes * 60
}

/// Compute the window-open timestamp for the current period boundary.
/// `now` is unix-seconds. The window opens at `now - (now mod period_seconds)`.
pub fn current_window_ts(now: i64, period_secs: u32) -> i64 {
    now - (now.rem_euclid(period_secs as i64))
}

/// `now()` returns the current unix-seconds.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Seconds remaining until the window resolves.
pub fn seconds_remaining(now: i64, window_open_ts: i64, period_secs: u32) -> u64 {
    let elapsed = (now - window_open_ts).max(0) as u64;
    (period_secs as u64).saturating_sub(elapsed)
}

/// Fetch the current active market for the given period.
pub async fn fetch_current(
    client: &GammaClient,
    period_minutes: u32,
) -> Result<Option<ActiveMarket>> {
    let period_secs = period_seconds(period_minutes);
    let window_ts = current_window_ts(now(), period_secs);
    client.fetch_active_market(period_minutes, window_ts).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_aligns_to_period() {
        // 1748000123 with 5-min period → 1748000100 (rounded down by mod 300)
        assert_eq!(current_window_ts(1_748_000_123, 300), 1_748_000_100);
        assert_eq!(current_window_ts(1_748_000_100, 300), 1_748_000_100);
    }

    #[test]
    fn seconds_remaining_decreases() {
        let w = 1_748_000_000;
        assert_eq!(seconds_remaining(w + 0,   w, 300), 300);
        assert_eq!(seconds_remaining(w + 240, w, 300), 60);
        assert_eq!(seconds_remaining(w + 300, w, 300), 0);
        assert_eq!(seconds_remaining(w + 350, w, 300), 0); // saturating
    }

    #[test]
    fn period_seconds_for_supported_values() {
        assert_eq!(period_seconds(5), 300);
        assert_eq!(period_seconds(15), 900);
    }
}

//! Strategy: Brownian-motion probability model + Quarter-Kelly sizing + entry gate.
//! Reference: strategy.py:158-317.

use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct StrategyConfig {
    pub min_edge: f64,                  // GENGAR_MIN_EDGE       (default 0.05)
    pub min_prob: f64,                  // GENGAR_MIN_PROB       (default 0.80)
    pub min_btc_delta: f64,             // GENGAR_MIN_BTC_DELTA  (default 0.06)
    pub entry_window_start: u64,        // GENGAR_ENTRY_WINDOW_START (240)
    pub entry_window_end: u64,          // GENGAR_ENTRY_WINDOW_END   (10)
    pub kelly_fraction: f64,            // GENGAR_KELLY_FRACTION (0.25)
    pub min_bet: f64,                   // GENGAR_MIN_BET        (5.0)
    pub max_bet: f64,                   // GENGAR_MAX_BET        (25.0)
    pub min_price: f64,                 //                       (hardcoded 0.50)
    pub max_price: f64,                 //                       (hardcoded 0.90)
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            min_edge: 0.05, min_prob: 0.80, min_btc_delta: 0.06,
            entry_window_start: 240, entry_window_end: 10,
            kelly_fraction: 0.25, min_bet: 5.0, max_bet: 25.0,
            min_price: 0.50, max_price: 0.90,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum SkipReason {
    OutsideEntryWindow, DeltaTooSmall, PriceOutOfRange, ProbBelowMin, EdgeBelowMin,
}

#[derive(Debug, Clone, Serialize)]
pub struct Signal {
    pub side: &'static str,          // "UP" or "DOWN"
    pub btc_delta_pct: f64,
    pub true_prob: f64,
    pub market_price: f64,
    pub edge: f64,
    pub bet_usd: f64,
    pub confidence: f64,             // edge/0.10 clamped to [0,1]
}

/// Brownian-motion CDF for the directional-win probability.
/// Reference: strategy.py:187-206.
pub fn estimate_true_probability(btc_delta_pct: f64, seconds_remaining: f64, vol: f64) -> f64 {
    let time_factor = seconds_remaining.max(1.0) / 300.0;
    let effective_vol = vol * time_factor.sqrt();
    if effective_vol <= 0.0 { return 0.5; }
    let z = btc_delta_pct.abs() / effective_vol;
    let prob = 0.5 * (1.0 + libm::erf(z / std::f64::consts::SQRT_2));
    prob.clamp(0.01, 0.99)
}

/// Quarter-Kelly bet size. Returns 0.0 if Kelly is non-positive or price is degenerate.
/// Reference: strategy.py:158-184.
pub fn kelly_bet(bankroll: f64, true_prob: f64, market_price: f64, fraction: f64, min_bet: f64, max_bet: f64) -> f64 {
    if market_price <= 0.0 || market_price >= 1.0 { return 0.0; }
    let b = (1.0 - market_price) / market_price;
    let q = 1.0 - true_prob;
    let kelly_f = (b * true_prob - q) / b;
    if kelly_f <= 0.0 { return 0.0; }
    let bet = bankroll * kelly_f * fraction;
    bet.max(min_bet).min(max_bet)
}

/// Evaluate entry. Returns a Signal if all gates pass; otherwise None.
/// Reference: strategy.py:246-317.
/// `side` is "UP" if btc_delta_pct > 0, else "DOWN".
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    btc_delta_pct: f64,
    seconds_remaining: u64,
    market_price: f64,         // YES (the directional-win side) price
    bankroll: f64,
    vol: f64,
    cfg: &StrategyConfig,
) -> Result<Signal, SkipReason> {
    if !(cfg.entry_window_end..=cfg.entry_window_start).contains(&seconds_remaining) {
        return Err(SkipReason::OutsideEntryWindow);
    }
    if btc_delta_pct.abs() < cfg.min_btc_delta { return Err(SkipReason::DeltaTooSmall); }
    if market_price < cfg.min_price || market_price > cfg.max_price { return Err(SkipReason::PriceOutOfRange); }
    let true_prob = estimate_true_probability(btc_delta_pct, seconds_remaining as f64, vol);
    if true_prob < cfg.min_prob { return Err(SkipReason::ProbBelowMin); }
    let edge = true_prob - market_price;
    if edge < cfg.min_edge { return Err(SkipReason::EdgeBelowMin); }
    let bet_usd = kelly_bet(bankroll, true_prob, market_price, cfg.kelly_fraction, cfg.min_bet, cfg.max_bet);
    Ok(Signal {
        side: if btc_delta_pct > 0.0 { "UP" } else { "DOWN" },
        btc_delta_pct, true_prob, market_price, edge, bet_usd,
        confidence: (edge / 0.10).clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// strategy.py:187-206 known values: with vol=0.12, delta=0.10, seconds=150:
    ///   time_factor = 150/300 = 0.5
    ///   effective_vol = 0.12 * sqrt(0.5) ≈ 0.0849
    ///   z = 0.10 / 0.0849 ≈ 1.178
    ///   prob = 0.5 * (1 + erf(1.178/sqrt(2))) ≈ 0.5 * (1 + 0.7615) ≈ 0.881
    #[test]
    fn brownian_prob_matches_python_reference() {
        let p = estimate_true_probability(0.10, 150.0, 0.12);
        assert!((p - 0.881).abs() < 0.01, "got {}", p);
    }

    #[test]
    fn brownian_prob_clamps_to_99_max() {
        let p = estimate_true_probability(100.0, 150.0, 0.12);
        assert_eq!(p, 0.99);
    }

    #[test]
    fn kelly_returns_zero_on_negative_edge() {
        // true_prob 0.45, market 0.50 → negative kelly_f → 0.
        assert_eq!(kelly_bet(100.0, 0.45, 0.50, 0.25, 5.0, 25.0), 0.0);
    }

    #[test]
    fn kelly_clamps_to_min_bet() {
        // tiny edge → kelly tiny → clamped to min_bet
        let bet = kelly_bet(100.0, 0.51, 0.50, 0.25, 5.0, 25.0);
        assert!(bet >= 5.0);
    }

    #[test]
    fn entry_gate_rejects_outside_window() {
        let r = evaluate(0.10, 5, 0.70, 100.0, 0.12, &Default::default());
        assert!(matches!(r, Err(SkipReason::OutsideEntryWindow)));
    }

    #[test]
    fn entry_gate_accepts_valid() {
        // strategy.py defaults: delta 0.10, secs 150, price 0.70, bankroll 100, vol 0.12
        // → prob ~0.88, edge ~0.18, all gates pass
        let r = evaluate(0.10, 150, 0.70, 100.0, 0.12, &Default::default());
        let s = r.unwrap();
        assert_eq!(s.side, "UP");
        assert!(s.edge > 0.05);
        assert!(s.bet_usd >= 5.0);
    }
}

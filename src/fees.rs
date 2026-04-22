//! Polymarket per-category fee table and rate conversion utilities.
//!
//! Spec: `docs/superpowers/specs/2026-04-21-multi-category-matching-design.md` §4.1.
//! Survey: `docs/notes/2026-04-21-polymarket-fee-survey.md`.
//!
//! The internal rate unit `ppm` encodes a pre-`p(1-p)` scaling such that the
//! peak fee in cents per dollar at p=0.5 equals `rate_ppm / 40_000`
//! (see `types::poly_fee_cents`). Categories map to published peak percentages
//! as of April 2026; the CLOB remains the source of truth when available
//! (see `bps_to_ppm`).

use serde::{Deserialize, Serialize};

/// Polymarket market category. Drives the fallback fee table when the per-market
/// CLOB meta lookup fails or the category is unknown. Ordered by published
/// peak rate (high→low) for readability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PolyCategory {
    Crypto,
    Mentions,
    Economics,
    Culture,
    Weather,
    Finance,
    Politics,
    Tech,
    Sports,
    Geopolitical,
    Unknown,
}

impl Default for PolyCategory {
    /// Existing `.discovery_cache.json` entries predate this field and are all
    /// sports — defaulting via `#[serde(default)]` keeps pre-PR-1 caches loadable.
    fn default() -> Self {
        PolyCategory::Sports
    }
}

/// Fallback peak-rate table. See `types::poly_fee_cents` for the formula;
/// `peak_cents_per_dollar = rate_ppm / 40_000`. Values are the April 2026
/// Polymarket-published peaks. The CLOB is authoritative when reachable —
/// this table is the conservative ceiling used on lookup failure.
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
        PolyCategory::Unknown      => 72_000, // conservative: highest published rate
    }
}

/// Convert Polymarket-published fee bps (from CLOB `/markets` `taker_base_fee`)
/// into the bot's internal `rate_ppm` convention.
///
/// Calibrated from `docs/notes/2026-04-21-polymarket-fee-survey.md`: three
/// independent Bundesliga Sports markets return `taker_base_fee = 1000`, which
/// must map to `category_fee_ppm(Sports) = 30_000`. Conversion factor K = 30.
///
/// Negative or zero input yields 0 (Polymarket occasionally publishes 0 for
/// fee-free markets even within fee-bearing categories).
pub fn bps_to_ppm(bps: i64) -> u32 {
    const K: u32 = 30;
    if bps <= 0 {
        return 0;
    }
    (bps as u32).saturating_mul(K)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::poly_fee_cents;

    #[test]
    fn sports_rate_matches_075pct_peak() {
        let ppm = category_fee_ppm(PolyCategory::Sports);
        assert_eq!(ppm, 30_000);
        // At p=50 the formula yields 0.75 cents; poly_fee_cents ceil-rounds → 1.
        assert_eq!(poly_fee_cents(50, ppm), 1);
    }

    #[test]
    fn economics_rate_is_150pct_peak() {
        let ppm = category_fee_ppm(PolyCategory::Economics);
        assert_eq!(ppm, 60_000);
        // peak = 60_000 / 40_000 = 1.5 cents → ceil → 2
        assert_eq!(poly_fee_cents(50, ppm), 2);
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
        // Unknown falls back to Crypto (highest published rate) so AI-matched
        // or unclassified markets never under-estimate fees.
        assert_eq!(
            category_fee_ppm(PolyCategory::Unknown),
            category_fee_ppm(PolyCategory::Crypto),
        );
    }

    #[test]
    fn bps_to_ppm_roundtrips_sports() {
        // Invariant locked in by the 2026-04-21 survey: bps=1000 is the
        // canonical Sports value and must convert to exactly 30_000 ppm.
        const OBSERVED_SPORTS_BPS: i64 = 1000;
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
    fn bps_to_ppm_negative_is_zero() {
        // Defensive: CLOB should never return negative, but if it does we
        // treat it as "no fee known" rather than underflowing.
        assert_eq!(bps_to_ppm(-1), 0);
    }

    #[test]
    fn bps_to_ppm_is_monotonic() {
        const SPORTS: i64 = 1000;
        assert!(bps_to_ppm(SPORTS) < bps_to_ppm(SPORTS * 2));
    }

    #[test]
    fn default_category_is_sports() {
        assert_eq!(PolyCategory::default(), PolyCategory::Sports);
    }
}

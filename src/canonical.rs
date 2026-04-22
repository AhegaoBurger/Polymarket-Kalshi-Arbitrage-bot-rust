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

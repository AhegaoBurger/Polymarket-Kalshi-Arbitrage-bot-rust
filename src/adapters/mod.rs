//! Event-type adapters and the shared cross-venue pair-join.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.3.
//!
//! Each concrete adapter (sports, fomc, …) produces a `NormalizedBatch` —
//! Kalshi and Polymarket markets normalized into `CanonicalMarket`. The
//! shared `pair_batch` function joins them on `(event_type, underlier)`
//! and emits `MarketPair`s stamped with the adapter's name.

use anyhow::Result;
use async_trait::async_trait;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use tracing::debug;

use crate::canonical::{CanonicalMarket, EventType, Underlier};
use crate::fees::MatchSource;
use crate::types::{MarketPair, MarketType};

pub mod sports;
pub mod fomc;
pub mod ai_reader;

/// A pair of CanonicalMarket vectors ready for the shared pair-join.
/// Adapters produce this; they never emit `MarketPair` directly.
pub struct NormalizedBatch {
    pub kalshi: Vec<CanonicalMarket>,
    pub poly: Vec<CanonicalMarket>,
}

/// Per-event-type normalizer. Fetches raw venue markets, parses them into
/// the canonical schema, and hands them off. Pairing is not the adapter's job.
#[async_trait]
pub trait EventAdapter: Send + Sync {
    fn name(&self) -> &'static str;
    fn event_type(&self) -> EventType;
    fn version(&self) -> u32;
    async fn normalize(&self) -> Result<NormalizedBatch>;
}

/// Cross-venue pair-join by canonical key `(event_type, underlier)`.
///
/// - Unmatched markets on either side are dropped without error.
/// - `Underlier::Other` never produces pairs — AI-matched markets flow through
///   a separate reader (PR 3). This guard is the T6-flagged trap-door fix.
/// - `MatchSource` on every emitted pair is `Structured { adapter: adapter_name }`.
/// Convenience wrapper: threads `adapter.name()` into `pair_batch` so callers
/// can't accidentally pass a name that drifts from what the adapter reports.
/// Prefer this over `pair_batch` when you already hold a `&dyn EventAdapter`.
pub fn pair_batch_from(adapter: &dyn EventAdapter, batch: NormalizedBatch) -> Vec<MarketPair> {
    pair_batch(batch, adapter.name())
}

pub fn pair_batch(batch: NormalizedBatch, adapter_name: &'static str) -> Vec<MarketPair> {
    let mut poly_by_key: FxHashMap<(EventType, Underlier), &CanonicalMarket> =
        FxHashMap::default();
    for p in &batch.poly {
        if matches!(p.underlier, Underlier::Other) {
            continue;
        }
        poly_by_key.insert((p.event_type, p.underlier.clone()), p);
    }

    let mut out = Vec::with_capacity(batch.kalshi.len());
    for k in &batch.kalshi {
        if matches!(k.underlier, Underlier::Other) {
            continue;
        }
        let key = (k.event_type, k.underlier.clone());
        let Some(p) = poly_by_key.get(&key) else {
            debug!(
                "no poly match for kalshi {:?}",
                k.venue.kalshi_market_ticker
            );
            continue;
        };
        if let Some(pair) = build_pair(k, p, adapter_name) {
            out.push(pair);
        }
    }
    out
}

/// Construct a `MarketPair` from matched Kalshi and Polymarket `CanonicalMarket`s.
/// Returns `None` if any required venue field is missing (defensive; a properly
/// constructed adapter should never return `None`). Each missing field emits a
/// `debug!` line naming the specific field so an adapter bug is traceable
/// instead of silently producing fewer pairs than expected.
fn build_pair(
    k: &CanonicalMarket,
    p: &CanonicalMarket,
    adapter_name: &'static str,
) -> Option<MarketPair> {
    macro_rules! required {
        ($opt:expr, $field:literal) => {
            match $opt.clone() {
                Some(v) => v,
                None => {
                    debug!(
                        adapter = adapter_name,
                        field = $field,
                        "build_pair: missing venue field — adapter bug, dropping pair"
                    );
                    return None;
                }
            }
        };
    }
    let kalshi_market_ticker = required!(k.venue.kalshi_market_ticker, "kalshi_market_ticker");
    let kalshi_event_ticker  = required!(k.venue.kalshi_event_ticker,  "kalshi_event_ticker");
    let poly_slug            = required!(p.venue.poly_slug,            "poly_slug");
    let poly_yes_token       = required!(p.venue.poly_yes_token,       "poly_yes_token");
    let poly_no_token        = required!(p.venue.poly_no_token,        "poly_no_token");
    let poly_condition_id    = required!(p.venue.poly_condition_id,    "poly_condition_id");

    let (market_type, line_value, team_suffix, league) = legacy_shape_fields(&k.underlier);

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
        match_source: MatchSource::Structured {
            adapter: adapter_name.to_string(),
        },
    })
}

/// Project the canonical underlier back onto the legacy MarketPair shape
/// (market_type, line_value, team_suffix, league) that existing trading code
/// consumes. Non-sports Underliers use `MarketType::Moneyline` as a neutral
/// placeholder today; widen `MarketType` if future adapters need richer typing.
fn legacy_shape_fields(
    u: &Underlier,
) -> (MarketType, Option<f64>, Option<Arc<str>>, Arc<str>) {
    use crate::canonical::SportsSubtype;
    match u {
        Underlier::SportsGame { league, market_subtype, .. } => {
            let mt = match market_subtype {
                SportsSubtype::Moneyline => MarketType::Moneyline,
                SportsSubtype::Spread => MarketType::Spread,
                SportsSubtype::Total => MarketType::Total,
                SportsSubtype::Btts => MarketType::Btts,
            };
            (mt, None, None, league.clone())
        }
        _ => (MarketType::Moneyline, None, None, Arc::from("other")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{CanonicalMarket, EventType, Platform, SportsSubtype, TimeWindow, Underlier, Venue};
    use crate::fees::PolyCategory;
    use chrono::NaiveDate;

    /// Build a CanonicalMarket for a sports game. `side` decides which venue's
    /// fields are populated; the Underlier is identical on both sides so the
    /// pair-join keys match.
    fn mk_canon_sports(side: Platform, yes: &str, no: &str) -> CanonicalMarket {
        let date = NaiveDate::from_ymd_opt(2025, 12, 27).unwrap();
        let kalshi = matches!(side, Platform::Kalshi);
        let poly = matches!(side, Platform::Polymarket);
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
                kalshi_event_ticker: kalshi.then(|| "KXEPLGAME-25DEC27CFCAVL".into()),
                kalshi_market_ticker: kalshi.then(|| "KXEPLGAME-25DEC27CFCAVL-CFC".into()),
                poly_slug: poly.then(|| "epl-cfc-avl-2025-12-27-cfc".into()),
                poly_yes_token: poly.then(|| yes.into()),
                poly_no_token: poly.then(|| no.into()),
                poly_condition_id: poly.then(|| "0xcond".into()),
            },
            category: PolyCategory::Sports,
            raw_title: "Test match".into(),
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
    fn pair_batch_drops_unmatched_kalshi_side() {
        let mut k = mk_canon_sports(Platform::Kalshi, "", "");
        // Mutate to a different game — no poly counterpart.
        if let Underlier::SportsGame { ref mut home, .. } = k.underlier {
            *home = "LIV".into();
        }
        let batch = NormalizedBatch {
            kalshi: vec![k],
            poly: vec![mk_canon_sports(Platform::Polymarket, "0xyes", "0xno")],
        };
        let pairs = pair_batch(batch, "sports");
        assert!(pairs.is_empty(), "unmatched underliers must drop");
    }

    #[test]
    fn pair_batch_ignores_other_underlier_on_kalshi_side() {
        // T6 code-review guard: Underlier::Other must never produce a pair,
        // even if there is a matching poly entry.
        let mut k = mk_canon_sports(Platform::Kalshi, "", "");
        k.underlier = Underlier::Other;
        let batch = NormalizedBatch {
            kalshi: vec![k],
            poly: vec![mk_canon_sports(Platform::Polymarket, "0xyes", "0xno")],
        };
        let pairs = pair_batch(batch, "sports");
        assert!(pairs.is_empty(), "Other underlier on kalshi must not emit pairs");
    }

    #[test]
    fn pair_batch_ignores_other_underlier_on_poly_side() {
        // Same guard but with Other on the poly side — ensures the insert-side
        // filter is in place too, not just the lookup-side filter.
        let mut p = mk_canon_sports(Platform::Polymarket, "0xyes", "0xno");
        p.underlier = Underlier::Other;
        let batch = NormalizedBatch {
            kalshi: vec![mk_canon_sports(Platform::Kalshi, "", "")],
            poly: vec![p],
        };
        let pairs = pair_batch(batch, "sports");
        assert!(pairs.is_empty(), "Other underlier on poly must not emit pairs");
    }

    #[test]
    fn pair_batch_empty_input_yields_empty_output() {
        let batch = NormalizedBatch { kalshi: vec![], poly: vec![] };
        assert!(pair_batch(batch, "sports").is_empty());
    }

    #[test]
    fn pair_batch_drops_unmatched_poly_side() {
        // Poly has a market with no Kalshi counterpart — must be silently
        // dropped. The join iterates Kalshi and looks up Poly; poly-only
        // entries are implicitly excluded. This test documents that intent.
        let mut p = mk_canon_sports(Platform::Polymarket, "0xy", "0xn");
        if let Underlier::SportsGame { ref mut home, .. } = p.underlier {
            *home = "LIV".into();
        }
        let batch = NormalizedBatch {
            kalshi: vec![mk_canon_sports(Platform::Kalshi, "", "")],
            poly: vec![p],
        };
        let pairs = pair_batch(batch, "sports");
        assert!(pairs.is_empty(), "poly-only entries must drop");
    }

    #[test]
    fn pair_batch_stamps_adapter_name_on_match_source() {
        let batch = NormalizedBatch {
            kalshi: vec![mk_canon_sports(Platform::Kalshi, "", "")],
            poly: vec![mk_canon_sports(Platform::Polymarket, "0xy", "0xn")],
        };
        let pairs = pair_batch(batch, "fomc");
        assert_eq!(pairs.len(), 1);
        match &pairs[0].match_source {
            MatchSource::Structured { adapter } => assert_eq!(adapter, "fomc"),
            _ => panic!("expected Structured"),
        }
    }
}

//! FOMC rate-decision adapter — pairs Kalshi `KXFED*` markets with Polymarket
//! neg-risk outcomes via a current-rate anchor.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.5.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{Datelike, NaiveDate};
use std::sync::Arc;

use crate::adapters::{EventAdapter, NormalizedBatch};
use crate::canonical::{CanonicalMarket, EventType, Platform, TimeWindow, Underlier, Venue};
use crate::fees::PolyCategory;
use crate::kalshi::KalshiApiClient;
use crate::polymarket::{GammaClient, GammaEvent};
use crate::types::{KalshiEvent, KalshiMarket};

const FOMC_KALSHI_SERIES: &str = "KXFED";

pub struct FomcAdapter {
    kalshi: Arc<KalshiApiClient>,
    gamma: Arc<GammaClient>,
    http: reqwest::Client,
    fred_api_key: Option<String>,
}

impl FomcAdapter {
    pub fn new(
        kalshi: Arc<KalshiApiClient>,
        gamma: Arc<GammaClient>,
        http: reqwest::Client,
        fred_api_key: Option<String>,
    ) -> Self {
        Self { kalshi, gamma, http, fred_api_key }
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
            let meeting_date = canon.first().and_then(|c| match &c.underlier {
                Underlier::FomcRateBand { meeting_date, .. } => Some(*meeting_date),
                _ => None,
            });
            kalshi_canon.extend(canon);
            let Some(meeting_date) = meeting_date else { continue };

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
}

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

/// Parse `"KXFED-YYMMM"` into a NaiveDate at the 1st of that month.
/// Returns None for any other shape. Day is unknown from the ticker so we
/// anchor at the 1st — pair-join keys only use the date to disambiguate
/// meetings, and FOMC meetings never share a month.
pub(crate) fn parse_meeting_date_from_event_ticker(ticker: &str) -> Option<NaiveDate> {
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

    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }

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

#[cfg(test)]
mod kalshi_normalize_tests {
    use super::*;

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
        assert_eq!(
            canon[0].venue.kalshi_market_ticker.as_deref().map(|s| s as &str),
            Some("KXFED-26APR-T425")
        );
        assert_eq!(
            canon[0].venue.kalshi_event_ticker.as_deref().map(|s| s as &str),
            Some("KXFED-26APR")
        );
        assert!(canon[0].venue.poly_slug.is_none());
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::*;

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

#[cfg(test)]
mod poly_normalize_tests {
    use super::*;
    use crate::polymarket::{GammaEvent, GammaEventMarket};

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
        assert_eq!(
            canon[0].venue.poly_slug.as_deref().map(|s| s as &str),
            Some("fomc-25-bps-cut-april-2026")
        );
        assert_eq!(
            canon[0].venue.poly_condition_id.as_deref().map(|s| s as &str),
            Some("0xA")
        );

        // No change → 425
        match &canon[1].underlier {
            Underlier::FomcRateBand { floor_bps, .. } => assert_eq!(*floor_bps, 425),
            _ => panic!(),
        }
    }
}

//! SportsAdapter: normalizes Kalshi sports events and matched Polymarket
//! markets into the canonical schema. Wraps the pre-PR-1 sports discovery
//! flow behind the EventAdapter trait with identical output behavior.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.4.

use anyhow::Result;
use async_trait::async_trait;
use chrono::NaiveDate;
use futures_util::{stream, StreamExt};
use governor::{
    Quota, RateLimiter,
    clock::DefaultClock,
    middleware::NoOpMiddleware,
    state::NotKeyed,
};
use std::num::NonZeroU32;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::warn;

use crate::adapters::{EventAdapter, NormalizedBatch};
use crate::cache::TeamCache;
use crate::canonical::{
    CanonicalMarket, EventType, Platform, SportsSubtype, TimeWindow, Underlier, Venue,
};
use crate::config::{get_league_config, get_league_configs, LeagueConfig};
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
    pub leagues: Vec<&'static str>, // empty = all
    pub kalshi: Arc<KalshiApiClient>,
    pub gamma: Arc<GammaClient>,
    pub team_cache: Arc<TeamCache>,
    pub kalshi_limiter: Arc<KalshiRateLimiter>,
    pub kalshi_semaphore: Arc<Semaphore>,
    pub gamma_semaphore: Arc<Semaphore>,
}

impl SportsAdapter {
    pub fn new(
        kalshi: Arc<KalshiApiClient>,
        team_cache: Arc<TeamCache>,
        leagues: Vec<&'static str>,
    ) -> Self {
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
    fn name(&self) -> &'static str {
        "sports"
    }
    fn event_type(&self) -> EventType {
        EventType::Sports
    }
    fn version(&self) -> u32 {
        1
    }

    async fn normalize(&self) -> Result<NormalizedBatch> {
        let configs: Vec<LeagueConfig> = if self.leagues.is_empty() {
            get_league_configs()
        } else {
            self.leagues
                .iter()
                .filter_map(|l| get_league_config(l))
                .collect()
        };

        // Parallel per-league normalize
        let league_futures: Vec<_> = configs.iter().map(|c| self.normalize_league(c)).collect();
        let league_results = futures_util::future::join_all(league_futures).await;

        let mut batch = NormalizedBatch {
            kalshi: Vec::new(),
            poly: Vec::new(),
        };
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
        let mut batch = NormalizedBatch {
            kalshi: Vec::new(),
            poly: Vec::new(),
        };
        for mt in [Mt::Moneyline, Mt::Spread, Mt::Total, Mt::Btts] {
            let Some(series) = get_series_for_type(config, mt) else {
                continue;
            };
            let league_batch = self.normalize_series(config, series, mt).await?;
            batch.kalshi.extend(league_batch.kalshi);
            batch.poly.extend(league_batch.poly);
        }
        Ok(batch)
    }

    async fn normalize_series(
        &self,
        config: &LeagueConfig,
        series: &str,
        mt: MarketType,
    ) -> Result<NormalizedBatch> {
        // Rate-limited Kalshi event fetch
        {
            let _permit = self
                .kalshi_semaphore
                .acquire()
                .await
                .map_err(|e| anyhow::anyhow!("semaphore closed: {}", e))?;
            self.kalshi_limiter.until_ready().await;
        }
        let events = self.kalshi.get_events(series, 50).await?;

        // Parse tickers
        let parsed_events: Vec<(ParsedKalshiTicker, KalshiEvent)> = events
            .into_iter()
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

        // Build lookup tasks
        let mut lookups: Vec<GammaLookupTask> = Vec::new();
        for (parsed, event, markets_res) in market_results {
            let markets = match markets_res {
                Ok(ms) => ms,
                Err(e) => {
                    warn!(
                        "[sports] kalshi markets fetch failed for {}: {}",
                        event.event_ticker, e
                    );
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
            let home_str = self
                .team_cache
                .kalshi_to_poly(config.poly_prefix, &task.parsed.team1)
                .unwrap_or_else(|| task.parsed.team1.to_lowercase());
            let away_str = self
                .team_cache
                .kalshi_to_poly(config.poly_prefix, &task.parsed.team2)
                .unwrap_or_else(|| task.parsed.team2.to_lowercase());

            let underlier = Underlier::SportsGame {
                league: Arc::from(task.league),
                home: Arc::from(home_str),
                away: Arc::from(away_str),
                date,
                market_subtype: subtype,
            };

            let title: Arc<str> = Arc::from(format!("{} - {}", task.event.title, task.market.title).as_str());

            kalshi_canon.push(CanonicalMarket {
                event_type: EventType::Sports,
                underlier: underlier.clone(),
                time_window: TimeWindow {
                    event_at: None,
                    settles_at: None,
                },
                venue: Venue {
                    platform: Platform::Kalshi,
                    kalshi_event_ticker: Some(task.event.event_ticker.clone().into()),
                    kalshi_market_ticker: Some(task.market.ticker.clone().into()),
                    poly_slug: None,
                    poly_yes_token: None,
                    poly_no_token: None,
                    poly_condition_id: None,
                },
                category: PolyCategory::Sports,
                raw_title: title.clone(),
                raw_description: Arc::from(""),
                adapter_version: 1,
            });
            poly_canon.push(CanonicalMarket {
                event_type: EventType::Sports,
                underlier,
                time_window: TimeWindow {
                    event_at: None,
                    settles_at: None,
                },
                venue: Venue {
                    platform: Platform::Polymarket,
                    kalshi_event_ticker: None,
                    kalshi_market_ticker: None,
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

        Ok(NormalizedBatch {
            kalshi: kalshi_canon,
            poly: poly_canon,
        })
    }

    fn build_poly_slug(
        &self,
        poly_prefix: &str,
        parsed: &ParsedKalshiTicker,
        market_type: MarketType,
        market: &KalshiMarket,
    ) -> String {
        // Identical logic to pre-PR-1 build_poly_slug in discovery.rs.
        let poly_team1 = self
            .team_cache
            .kalshi_to_poly(poly_prefix, &parsed.team1)
            .unwrap_or_else(|| parsed.team1.to_lowercase());
        let poly_team2 = self
            .team_cache
            .kalshi_to_poly(poly_prefix, &parsed.team2)
            .unwrap_or_else(|| parsed.team2.to_lowercase());
        let date_str = kalshi_date_to_iso(&parsed.date);
        let base = format!("{}-{}-{}-{}", poly_prefix, poly_team1, poly_team2, date_str);

        match market_type {
            MarketType::Moneyline => {
                if let Some(suffix) = extract_team_suffix(&market.ticker) {
                    if suffix.to_lowercase() == "tie" {
                        format!("{}-draw", base)
                    } else {
                        let poly_suffix = self
                            .team_cache
                            .kalshi_to_poly(poly_prefix, &suffix)
                            .unwrap_or_else(|| suffix.to_lowercase());
                        format!("{}-{}", base, poly_suffix)
                    }
                } else {
                    base
                }
            }
            MarketType::Spread => {
                if let Some(floor) = market.floor_strike {
                    let floor_str = format!("{:.1}", floor).replace(".", "pt");
                    format!("{}-spread-{}", base, floor_str)
                } else {
                    format!("{}-spread", base)
                }
            }
            MarketType::Total => {
                if let Some(floor) = market.floor_strike {
                    let floor_str = format!("{:.1}", floor).replace(".", "pt");
                    format!("{}-total-{}", base, floor_str)
                } else {
                    format!("{}-total", base)
                }
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
pub(crate) struct ParsedKalshiTicker {
    pub date: String,  // "25DEC27"
    pub team1: String, // "CFC"
    pub team2: String, // "AVL"
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

// === Pure helpers, re-exported so discovery.rs (and anyone else) can use them ===

/// Parse a Kalshi event ticker like `KXEPLGAME-25DEC27CFCAVL` into its parts.
/// Handles both the 2-part and 3-part formats (see discovery.rs original
/// implementation notes).
pub(crate) fn parse_kalshi_event_ticker(ticker: &str) -> Option<ParsedKalshiTicker> {
    let parts: Vec<&str> = ticker.split('-').collect();
    if parts.len() < 2 {
        return None;
    }
    let (date, teams_part) = if parts.len() >= 3 && parts[2].len() >= 4 {
        let date_part = parts[1];
        let date = if date_part.len() >= 7 {
            date_part[..7].to_uppercase()
        } else {
            return None;
        };
        (date, parts[2])
    } else {
        let date_teams = parts[1];
        if date_teams.len() < 11 {
            return None;
        }
        let date = date_teams[..7].to_uppercase();
        let teams = &date_teams[7..];
        (date, teams)
    };
    let (team1, team2) = split_team_codes(teams_part);
    Some(ParsedKalshiTicker { date, team1, team2 })
}

pub(crate) fn split_team_codes(teams: &str) -> (String, String) {
    let len = teams.len();
    match len {
        4 => (teams[..2].to_uppercase(), teams[2..].to_uppercase()),
        5 => (teams[..2].to_uppercase(), teams[2..].to_uppercase()),
        6 => {
            let first_two = teams[..2].to_uppercase();
            if is_likely_two_letter_code(&first_two) {
                (first_two, teams[2..].to_uppercase())
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

pub(crate) fn is_likely_two_letter_code(code: &str) -> bool {
    matches!(
        code,
        "OM" | "OL"
            | "FC"
            | "OH"
            | "SF"
            | "LA"
            | "NY"
            | "KC"
            | "TB"
            | "GB"
            | "NE"
            | "NO"
            | "LV"
            | "BC"
            | "SC"
            | "AC"
            | "AS"
            | "US"
    )
}

pub(crate) fn kalshi_date_to_iso(kalshi_date: &str) -> String {
    if kalshi_date.len() != 7 {
        return kalshi_date.to_string();
    }
    let year = format!("20{}", &kalshi_date[..2]);
    let month = match &kalshi_date[2..5].to_uppercase()[..] {
        "JAN" => "01",
        "FEB" => "02",
        "MAR" => "03",
        "APR" => "04",
        "MAY" => "05",
        "JUN" => "06",
        "JUL" => "07",
        "AUG" => "08",
        "SEP" => "09",
        "OCT" => "10",
        "NOV" => "11",
        "DEC" => "12",
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

pub(crate) fn extract_team_suffix(ticker: &str) -> Option<String> {
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

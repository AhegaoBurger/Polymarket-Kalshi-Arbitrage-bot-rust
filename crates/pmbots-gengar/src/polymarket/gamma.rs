//! Gamma REST API client — used for active-market lookup by slug.
//! Reference: market.py:14-113.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::time::Duration;

pub const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
pub const USER_AGENT: &str = "PolyBot/1.0";

#[derive(Debug, Clone)]
pub struct GammaClient {
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct GammaEvent {
    pub id: String,
    pub slug: String,
    pub markets: Vec<GammaMarket>,
}

#[derive(Debug, Deserialize)]
pub struct GammaMarket {
    /// JSON-string-encoded array of token IDs: "[\"123\", \"456\"]"
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: String,
    /// JSON-string-encoded array of outcome names: "[\"Up\", \"Down\"]"
    pub outcomes: String,
    /// JSON-string-encoded array of last-prices: "[\"0.52\", \"0.48\"]"
    #[serde(rename = "outcomePrices")]
    pub outcome_prices: Option<String>,
}

/// Resolved active market: token IDs for Up and Down.
#[derive(Debug, Clone)]
pub struct ActiveMarket {
    pub event_id: String,
    pub slug: String,
    pub token_id_up: String,
    pub token_id_down: String,
    pub last_price_up: Option<f64>,
    pub last_price_down: Option<f64>,
}

impl GammaClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()
            .context("build gamma http client")?;
        Ok(Self { http })
    }

    /// Fetch the event for the given window. `period_minutes` must be 5 or 15.
    /// `window_ts` is the Unix timestamp of the window OPEN, aligned to the
    /// period boundary (`window_ts % (period_minutes * 60) == 0`).
    pub async fn fetch_active_market(
        &self,
        period_minutes: u32,
        window_ts: i64,
    ) -> Result<Option<ActiveMarket>> {
        let slug = format!("btc-updown-{}m-{}", period_minutes, window_ts);
        let url = format!("{}/events?slug={}", GAMMA_BASE, slug);

        let events: Vec<GammaEvent> = self
            .http.get(&url).send().await
            .with_context(|| format!("GET {}", url))?
            .error_for_status()?
            .json().await.context("parse gamma events")?;

        if events.is_empty() {
            return Ok(None);
        }
        let event = &events[0];
        let market = event.markets.first()
            .ok_or_else(|| anyhow!("event {} has no markets", event.id))?;

        let token_ids: Vec<String> = serde_json::from_str(&market.clob_token_ids)
            .context("parse clobTokenIds")?;
        let outcomes: Vec<String> = serde_json::from_str(&market.outcomes)
            .context("parse outcomes")?;
        let prices: Vec<f64> = match &market.outcome_prices {
            Some(s) => serde_json::from_str::<Vec<String>>(s)?
                .iter().map(|p| p.parse::<f64>().unwrap_or(0.0)).collect(),
            None => vec![0.0, 0.0],
        };

        // outcomes is ["Up","Down"] in canonical order; tolerate reversed too
        let (idx_up, idx_down) = match (
            outcomes.iter().position(|o| o.eq_ignore_ascii_case("Up")),
            outcomes.iter().position(|o| o.eq_ignore_ascii_case("Down")),
        ) {
            (Some(u), Some(d)) => (u, d),
            _ => (0, 1),
        };

        Ok(Some(ActiveMarket {
            event_id: event.id.clone(),
            slug: event.slug.clone(),
            token_id_up: token_ids.get(idx_up).cloned().unwrap_or_default(),
            token_id_down: token_ids.get(idx_down).cloned().unwrap_or_default(),
            last_price_up: prices.get(idx_up).copied(),
            last_price_down: prices.get(idx_down).copied(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gamma_event_response_shape() {
        let payload = r#"[{
            "id": "12345",
            "slug": "btc-updown-5m-1748000000",
            "markets": [{
                "clobTokenIds": "[\"token-up\", \"token-down\"]",
                "outcomes": "[\"Up\", \"Down\"]",
                "outcomePrices": "[\"0.52\", \"0.48\"]"
            }]
        }]"#;

        let events: Vec<GammaEvent> = serde_json::from_str(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].markets[0].clob_token_ids, "[\"token-up\", \"token-down\"]");
    }
}

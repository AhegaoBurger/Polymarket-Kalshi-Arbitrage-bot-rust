//! Binance BTC/USDT trade-stream price feed with REST fallback.
//! Reference: price_feed.py.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration, Instant};
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

pub const WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@trade";
pub const REST_URL: &str = "https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT";
pub const STALE_AFTER: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub struct PriceState {
    pub price: f64,
    pub last_update: Option<Instant>,
}

impl PriceState {
    pub fn is_fresh(&self) -> bool {
        self.price > 0.0 && self.last_update.map_or(false, |t| t.elapsed() < STALE_AFTER)
    }
}

pub type SharedPrice = Arc<RwLock<PriceState>>;

pub fn new_state() -> SharedPrice { Arc::new(RwLock::new(PriceState::default())) }

#[derive(Debug, Deserialize)]
struct TradeMsg { p: String }

/// Run the WS loop forever (caller spawns this in a Tokio task).
/// Reconnects after a 3-second backoff on any error.
pub async fn run_ws(state: SharedPrice) {
    loop {
        match connect_one(state.clone()).await {
            Ok(_)  => warn!("[GENGAR][PRICE-WS] stream ended, reconnecting"),
            Err(e) => error!("[GENGAR][PRICE-WS] {}, reconnecting in 3s", e),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn connect_one(state: SharedPrice) -> Result<()> {
    let (ws, _) = connect_async(WS_URL).await.context("ws connect")?;
    info!("[GENGAR][PRICE-WS] connected");
    let (_w, mut r) = ws.split();
    while let Some(Ok(msg)) = r.next().await {
        if let Some(t) = msg.into_text().ok() {
            if let Ok(parsed) = serde_json::from_str::<TradeMsg>(&t) {
                if let Ok(p) = parsed.p.parse::<f64>() {
                    let mut s = state.write().await;
                    s.price = p;
                    s.last_update = Some(Instant::now());
                }
            }
        }
    }
    Ok(())
}

/// REST-fallback poller. Spawn this in a Tokio task alongside `run_ws`.
/// Polls every 2s; only writes the cache when WS is stale.
pub async fn run_rest_fallback(state: SharedPrice) {
    let http = reqwest::Client::new();
    let mut tick = interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let stale = !state.read().await.is_fresh();
        if !stale { continue; }
        match fetch_rest(&http).await {
            Ok(p) => {
                let mut s = state.write().await;
                s.price = p;
                s.last_update = Some(Instant::now());
            }
            Err(e) => warn!("[GENGAR][PRICE-REST] {}", e),
        }
    }
}

async fn fetch_rest(http: &reqwest::Client) -> Result<f64> {
    let v: serde_json::Value = http.get(REST_URL).send().await?
        .error_for_status()?.json().await?;
    v.get("price").and_then(|p| p.as_str()).and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| anyhow::anyhow!("missing price"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_state_is_fresh_when_recent() {
        let mut s = PriceState::default();
        s.price = 67000.0;
        s.last_update = Some(Instant::now());
        assert!(s.is_fresh());
    }

    #[test]
    fn price_state_stale_when_zero_price() {
        let s = PriceState { price: 0.0, last_update: Some(Instant::now()) };
        assert!(!s.is_fresh());
    }

    #[test]
    fn trade_msg_parses() {
        let m: TradeMsg = serde_json::from_str(r#"{"e":"trade","p":"68234.50"}"#).unwrap();
        assert_eq!(m.p, "68234.50");
    }
}

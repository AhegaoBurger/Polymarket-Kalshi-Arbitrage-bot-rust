//! Polymarket WS client — current-token orderbook subscription.
//!
//! Subscribes to the two token IDs for the active window's Up/Down market and
//! maintains a `TokenPriceCache` of best bid/ask for each. `bot.rs` initiates
//! and tears down the subscription at every window boundary.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{warn, error, info};

pub const POLY_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
pub const PING_INTERVAL_SECS: u64 = 30;

#[derive(Debug, Serialize)]
struct SubscribeCmd<'a> {
    assets_ids: Vec<String>,
    #[serde(rename = "type")]
    sub_type: &'a str,
}

#[derive(Debug, Deserialize)]
pub struct BookSnapshot {
    pub asset_id: String,
    pub bids: Vec<PriceLevel>,
    pub asks: Vec<PriceLevel>,
}

#[derive(Debug, Deserialize)]
pub struct PriceLevel { pub price: String, pub size: String }

#[derive(Debug, Deserialize)]
pub struct PriceChangeEvent {
    pub event_type: Option<String>,
    pub price_changes: Option<Vec<PriceChangeItem>>,
}

#[derive(Debug, Deserialize)]
pub struct PriceChangeItem {
    pub asset_id: String,
    pub price: String,
    pub size: String,
    pub side: String,                 // "BUY" or "SELL"
}

/// Per-token best bid/ask, updated by the WS task, read by strategy.
#[derive(Debug, Clone, Default)]
pub struct TokenPrice {
    pub best_bid: f64,
    pub best_ask: f64,
    pub last_update: Option<Instant>,
}

pub type TokenPriceCache = Arc<RwLock<HashMap<String, TokenPrice>>>;

pub fn new_cache() -> TokenPriceCache {
    Arc::new(RwLock::new(HashMap::new()))
}

/// Run a WS subscription for the given token IDs. Updates `cache` until
/// the connection drops, then returns (caller restarts at window boundary).
pub async fn run_ws(token_ids: Vec<String>, cache: TokenPriceCache) -> Result<()> {
    let (ws, _) = connect_async(POLY_WS_URL).await
        .context("connect polymarket ws")?;
    info!("[GENGAR][POLY-WS] connected");

    let (mut write, mut read) = ws.split();

    let sub = SubscribeCmd { assets_ids: token_ids.clone(), sub_type: "market" };
    write.send(Message::Text(serde_json::to_string(&sub)?)).await
        .context("subscribe")?;

    let mut ping = interval(Duration::from_secs(PING_INTERVAL_SECS));
    let mut last = Instant::now();
    loop {
        tokio::select! {
            _ = ping.tick() => {
                if write.send(Message::Ping(vec![])).await.is_err() { break; }
            }
            msg = read.next() => match msg {
                Some(Ok(Message::Text(t))) => {
                    last = Instant::now();
                    if let Ok(snaps) = serde_json::from_str::<Vec<BookSnapshot>>(&t) {
                        let mut c = cache.write().await;
                        for snap in snaps {
                            let best_bid = snap.bids.iter()
                                .filter_map(|l| l.price.parse::<f64>().ok())
                                .fold(0.0_f64, f64::max);
                            let best_ask = snap.asks.iter()
                                .filter_map(|l| l.price.parse::<f64>().ok())
                                .fold(f64::MAX, f64::min);
                            c.insert(snap.asset_id.clone(), TokenPrice {
                                best_bid, best_ask, last_update: Some(Instant::now()),
                            });
                        }
                    } else if let Ok(ev) = serde_json::from_str::<PriceChangeEvent>(&t) {
                        if let Some(changes) = ev.price_changes {
                            let mut c = cache.write().await;
                            for ch in changes {
                                let entry = c.entry(ch.asset_id.clone()).or_default();
                                if let Ok(p) = ch.price.parse::<f64>() {
                                    match ch.side.as_str() {
                                        "BUY"  => entry.best_bid = entry.best_bid.max(p),
                                        "SELL" => entry.best_ask = entry.best_ask.min(p),
                                        _ => {}
                                    }
                                    entry.last_update = Some(Instant::now());
                                }
                            }
                        }
                    }
                }
                Some(Ok(Message::Ping(d))) => { let _ = write.send(Message::Pong(d)).await; last = Instant::now(); }
                Some(Ok(Message::Pong(_))) => { last = Instant::now(); }
                Some(Ok(Message::Close(f))) => { warn!("[GENGAR][POLY-WS] server closed: {:?}", f); break; }
                Some(Err(e)) => { error!("[GENGAR][POLY-WS] error: {}", e); break; }
                None => { warn!("[GENGAR][POLY-WS] stream ended"); break; }
                _ => {}
            }
        }
        if last.elapsed() > Duration::from_secs(120) {
            warn!("[GENGAR][POLY-WS] stale, reconnecting");
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn book_snapshot_parses() {
        let s = r#"[{"asset_id":"t1","bids":[{"price":"0.50","size":"100"}],"asks":[{"price":"0.51","size":"80"}]}]"#;
        let snaps: Vec<BookSnapshot> = serde_json::from_str(s).unwrap();
        assert_eq!(snaps[0].asset_id, "t1");
        assert_eq!(snaps[0].asks[0].price, "0.51");
    }
}

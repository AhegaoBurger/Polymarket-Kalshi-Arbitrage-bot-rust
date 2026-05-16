//! Polymarket WS client — current-token orderbook subscription.
//!
//! Subscribes to the two token IDs for the active window's Up/Down market and
//! maintains a `TokenPriceCache` of best bid/ask for each. `bot.rs` initiates
//! and tears down the subscription at every window boundary.
//!
//! Bookkeeping: we maintain a full per-side price ladder per asset (price → size)
//! and derive best_bid/best_ask from the top of the ladder. `price_change` events
//! with `size = 0` represent level removals — the ladder pulls them and the next
//! best level becomes the top. The naive `max(best_bid, p)` / `min(best_ask, p)`
//! approach can never pull the top back when a level is removed; it gets stuck
//! at stale top-of-book values until the next `BookSnapshot`.

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration, Instant};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{error, info, warn};

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
pub struct PriceLevel {
    pub price: String,
    pub size: String,
}

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
    pub side: String, // "BUY" or "SELL"
}

/// Full orderbook per asset, keyed by price-in-bps (1¢ = 100 bps) to avoid
/// f64 hash issues and quantize to Polymarket's 1¢ tick.
/// Bids: top of book = max key. Asks: top of book = min key.
#[derive(Debug, Default, Clone)]
pub struct TokenBook {
    pub bids: BTreeMap<u64, f64>,
    pub asks: BTreeMap<u64, f64>,
    pub last_update: Option<Instant>,
}

impl TokenBook {
    pub fn best_bid(&self) -> f64 {
        self.bids
            .keys()
            .next_back()
            .map(|bps| *bps as f64 / 10_000.0)
            .unwrap_or(0.0)
    }
    pub fn best_ask(&self) -> f64 {
        self.asks
            .keys()
            .next()
            .map(|bps| *bps as f64 / 10_000.0)
            .unwrap_or(0.0)
    }
}

pub type TokenPriceCache = Arc<RwLock<HashMap<String, TokenBook>>>;

pub fn new_cache() -> TokenPriceCache {
    Arc::new(RwLock::new(HashMap::new()))
}

fn parse_price_bps(s: &str) -> Option<u64> {
    let n = s.parse::<f64>().ok()?;
    let bps = (n * 10_000.0).round();
    if bps < 0.0 { None } else { Some(bps as u64) }
}

/// Run a WS subscription for the given token IDs. Updates `cache` until
/// the connection drops, then returns (caller restarts at window boundary).
pub async fn run_ws(token_ids: Vec<String>, cache: TokenPriceCache) -> Result<()> {
    let (ws, _) = connect_async(POLY_WS_URL).await.context("connect polymarket ws")?;
    info!("[GENGAR][POLY-WS] connected");

    let (mut write, mut read) = ws.split();

    let sub = SubscribeCmd {
        assets_ids: token_ids.clone(),
        sub_type: "market",
    };
    write
        .send(Message::Text(serde_json::to_string(&sub)?))
        .await
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
                    // BookSnapshot: full re-state of one or more assets.
                    if let Ok(snaps) = serde_json::from_str::<Vec<BookSnapshot>>(&t) {
                        let mut c = cache.write().await;
                        for snap in snaps {
                            let mut book = TokenBook::default();
                            for lvl in &snap.bids {
                                if let (Some(bps), Ok(size)) = (parse_price_bps(&lvl.price), lvl.size.parse::<f64>()) {
                                    if size > 0.0 { book.bids.insert(bps, size); }
                                }
                            }
                            for lvl in &snap.asks {
                                if let (Some(bps), Ok(size)) = (parse_price_bps(&lvl.price), lvl.size.parse::<f64>()) {
                                    if size > 0.0 { book.asks.insert(bps, size); }
                                }
                            }
                            book.last_update = Some(Instant::now());
                            c.insert(snap.asset_id.clone(), book);
                        }
                    }
                    // PriceChangeEvent: incremental level updates. `size = 0`
                    // means the level was removed (cancelled or fully consumed)
                    // — we must REMOVE it from the ladder, not keep the stale
                    // top-of-book value.
                    else if let Ok(ev) = serde_json::from_str::<PriceChangeEvent>(&t) {
                        if let Some(changes) = ev.price_changes {
                            let mut c = cache.write().await;
                            for ch in changes {
                                let bps = match parse_price_bps(&ch.price) {
                                    Some(b) => b,
                                    None => continue,
                                };
                                let size = ch.size.parse::<f64>().unwrap_or(0.0);
                                let book = c.entry(ch.asset_id.clone()).or_default();
                                let ladder = match ch.side.as_str() {
                                    "BUY"  => &mut book.bids,
                                    "SELL" => &mut book.asks,
                                    _ => continue,
                                };
                                if size <= 0.0 { ladder.remove(&bps); }
                                else           { ladder.insert(bps, size); }
                                book.last_update = Some(Instant::now());
                            }
                        }
                    }
                }
                Some(Ok(Message::Ping(d))) => {
                    let _ = write.send(Message::Pong(d)).await;
                    last = Instant::now();
                }
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

    #[test]
    fn book_derives_best_from_ladder() {
        let mut book = TokenBook::default();
        book.bids.insert(4000, 10.0);
        book.bids.insert(4500, 20.0);
        book.bids.insert(4900, 5.0);
        book.asks.insert(5100, 30.0);
        book.asks.insert(5500, 15.0);
        book.asks.insert(6000, 8.0);
        assert!((book.best_bid() - 0.49).abs() < 1e-9);
        assert!((book.best_ask() - 0.51).abs() < 1e-9);
    }

    #[test]
    fn level_removal_via_size_zero_pulls_top_back() {
        let mut book = TokenBook::default();
        book.asks.insert(5100, 30.0);
        book.asks.insert(5500, 15.0);
        assert!((book.best_ask() - 0.51).abs() < 1e-9);
        // Simulate the size=0 removal of the 0.51 level.
        book.asks.remove(&5100);
        // Top now 0.55, not stuck at 0.51 like the old min-only handler.
        assert!((book.best_ask() - 0.55).abs() < 1e-9);
    }

    #[test]
    fn empty_book_returns_zero() {
        let book = TokenBook::default();
        assert_eq!(book.best_bid(), 0.0);
        assert_eq!(book.best_ask(), 0.0);
    }

    #[test]
    fn parse_price_bps_handles_decimals_and_rounding() {
        assert_eq!(parse_price_bps("0.68").unwrap(), 6800);
        assert_eq!(parse_price_bps("0.5").unwrap(), 5000);
        assert_eq!(parse_price_bps("0.685").unwrap(), 6850);
        assert_eq!(parse_price_bps("0.6800000004").unwrap(), 6800);
        assert!(parse_price_bps("abc").is_none());
    }
}

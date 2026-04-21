//! Kalshi platform integration client.
//!
//! This module provides REST API and WebSocket clients for interacting with
//! the Kalshi prediction market platform, including order execution and
//! real-time price feed management.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use pkcs1::DecodeRsaPrivateKey;
use rsa::{
    pss::SigningKey,
    sha2::Sha256,
    signature::{RandomizedSigner, SignatureEncoding},
    RsaPrivateKey,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::{http::Request, Message}};
use tracing::{debug, error, info};

use crate::config::{KALSHI_WS_URL, KALSHI_API_BASE, KALSHI_API_DELAY_MS};
use crate::execution::NanoClock;
use crate::types::{
    KalshiEventsResponse, KalshiMarketsResponse, KalshiEvent, KalshiMarket,
    GlobalState, FastExecutionRequest, ArbType, PriceCents, SizeCents, fxhash_str,
};

// === Order Types ===

use std::borrow::Cow;
use std::fmt::Write;
use arrayvec::ArrayString;

#[derive(Debug, Clone, Serialize)]
pub struct KalshiOrderRequest<'a> {
    pub ticker: Cow<'a, str>,
    pub action: &'static str,
    pub side: &'static str,
    #[serde(rename = "type")]
    pub order_type: &'static str,
    pub count: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yes_price: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_price: Option<i64>,
    pub client_order_id: Cow<'a, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration_ts: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_in_force: Option<&'static str>,
}

impl<'a> KalshiOrderRequest<'a> {
    /// Create an IOC (immediate-or-cancel) buy order
    pub fn ioc_buy(ticker: Cow<'a, str>, side: &'static str, price_cents: i64, count: i64, client_order_id: Cow<'a, str>) -> Self {
        let (yes_price, no_price) = if side == "yes" {
            (Some(price_cents), None)
        } else {
            (None, Some(price_cents))
        };

        Self {
            ticker,
            action: "buy",
            side,
            order_type: "limit",
            count,
            yes_price,
            no_price,
            client_order_id,
            expiration_ts: None,
            time_in_force: Some("immediate_or_cancel"),
        }
    }

    /// Create an IOC (immediate-or-cancel) sell order
    pub fn ioc_sell(ticker: Cow<'a, str>, side: &'static str, price_cents: i64, count: i64, client_order_id: Cow<'a, str>) -> Self {
        let (yes_price, no_price) = if side == "yes" {
            (Some(price_cents), None)
        } else {
            (None, Some(price_cents))
        };

        Self {
            ticker,
            action: "sell",
            side,
            order_type: "limit",
            count,
            yes_price,
            no_price,
            client_order_id,
            expiration_ts: None,
            time_in_force: Some("immediate_or_cancel"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct KalshiOrderResponse {
    pub order: KalshiOrderDetails,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct KalshiOrderDetails {
    pub order_id: String,
    pub ticker: String,
    pub status: String,        // "resting", "canceled", "executed", "pending"
    #[serde(default)]
    pub remaining_count: Option<i64>,
    #[serde(default)]
    pub queue_position: Option<i64>,
    pub action: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String,
    pub yes_price: Option<i64>,
    pub no_price: Option<i64>,
    pub created_time: Option<String>,
    #[serde(default)]
    pub taker_fill_count: Option<i64>,
    #[serde(default)]
    pub maker_fill_count: Option<i64>,
    #[serde(default)]
    pub place_count: Option<i64>,
    #[serde(default)]
    pub taker_fill_cost: Option<i64>,
    #[serde(default)]
    pub maker_fill_cost: Option<i64>,
}

#[allow(dead_code)]
impl KalshiOrderDetails {
    /// Total filled contracts
    pub fn filled_count(&self) -> i64 {
        self.taker_fill_count.unwrap_or(0) + self.maker_fill_count.unwrap_or(0)
    }

    /// Check if order was fully filled
    pub fn is_filled(&self) -> bool {
        self.status == "executed" || self.remaining_count == Some(0)
    }

    /// Check if order was partially filled
    pub fn is_partial(&self) -> bool {
        self.filled_count() > 0 && !self.is_filled()
    }
}

// === Kalshi Auth Config ===

pub struct KalshiConfig {
    pub api_key_id: String,
    pub private_key: RsaPrivateKey,
}

impl KalshiConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        let api_key_id = std::env::var("KALSHI_API_KEY_ID").context("KALSHI_API_KEY_ID not set")?;
        // Support both KALSHI_PRIVATE_KEY_PATH and KALSHI_PRIVATE_KEY_FILE for compatibility
        let key_path = std::env::var("KALSHI_PRIVATE_KEY_PATH")
            .or_else(|_| std::env::var("KALSHI_PRIVATE_KEY_FILE"))
            .unwrap_or_else(|_| "kalshi_private_key.txt".to_string());
        let private_key_pem = std::fs::read_to_string(&key_path)
            .with_context(|| format!("Failed to read private key from {}", key_path))?
            .trim()
            .to_owned();
        let private_key = RsaPrivateKey::from_pkcs1_pem(&private_key_pem)
            .context("Failed to parse private key PEM")?;
        Ok(Self { api_key_id, private_key })
    }

    pub fn sign(&self, message: &str) -> Result<String> {
        tracing::debug!("[KALSHI-DEBUG] Signing message: {}", message);
        let signing_key = SigningKey::<Sha256>::new(self.private_key.clone());
        let signature = signing_key.sign_with_rng(&mut rand::thread_rng(), message.as_bytes());
        let sig_b64 = BASE64.encode(signature.to_bytes());
        tracing::debug!("[KALSHI-DEBUG] Signature (first 50 chars): {}...", &sig_b64[..50.min(sig_b64.len())]);
        Ok(sig_b64)
    }
}

// === Kalshi REST API Client ===

/// Timeout for order requests (shorter than general API timeout)
const ORDER_TIMEOUT: Duration = Duration::from_secs(5);

use std::sync::atomic::{AtomicU32, Ordering};

/// Global order counter for unique client_order_id generation
static ORDER_COUNTER: AtomicU32 = AtomicU32::new(0);

pub struct KalshiApiClient {
    http: reqwest::Client,
    pub config: KalshiConfig,
}

impl KalshiApiClient {
    pub fn new(config: KalshiConfig) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("Failed to build HTTP client"),
            config,
        }
    }

    #[inline]
    fn next_order_id() -> ArrayString<24> {
        let counter = ORDER_COUNTER.fetch_add(1, Ordering::Relaxed);
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let mut buf = ArrayString::<24>::new();
        let _ = write!(&mut buf, "a{}{}", ts, counter);
        buf
    }
    
    /// Generic authenticated GET request with retry on rate limit
    async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let mut retries = 0;
        const MAX_RETRIES: u32 = 5;

        loop {
            let url = format!("{}{}", KALSHI_API_BASE, path);
            let timestamp_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            // Kalshi signature uses FULL path including /trade-api/v2 prefix
            let full_path = format!("/trade-api/v2{}", path);
            let signature = self.config.sign(&format!("{}GET{}", timestamp_ms, full_path))?;
            
            let resp = self.http
                .get(&url)
                .header("KALSHI-ACCESS-KEY", &self.config.api_key_id)
                .header("KALSHI-ACCESS-SIGNATURE", &signature)
                .header("KALSHI-ACCESS-TIMESTAMP", timestamp_ms.to_string())
                .send()
                .await?;
            
            let status = resp.status();
            
            // Handle rate limit with exponential backoff
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                retries += 1;
                if retries > MAX_RETRIES {
                    anyhow::bail!("Kalshi API rate limited after {} retries", MAX_RETRIES);
                }
                let backoff_ms = 2000 * (1 << retries); // 4s, 8s, 16s, 32s, 64s
                debug!("[KALSHI] Rate limited, backing off {}ms (retry {}/{})", 
                       backoff_ms, retries, MAX_RETRIES);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                continue;
            }
            
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                anyhow::bail!("Kalshi API error {}: {}", status, body);
            }
            
            let data: T = resp.json().await?;
            tokio::time::sleep(Duration::from_millis(KALSHI_API_DELAY_MS)).await;
            return Ok(data);
        }
    }
    
    pub async fn get_events(&self, series_ticker: &str, limit: u32) -> Result<Vec<KalshiEvent>> {
        let path = format!("/events?series_ticker={}&limit={}&status=open", series_ticker, limit);
        let resp: KalshiEventsResponse = self.get(&path).await?;
        Ok(resp.events)
    }
    
    pub async fn get_markets(&self, event_ticker: &str) -> Result<Vec<KalshiMarket>> {
        let path = format!("/markets?event_ticker={}", event_ticker);
        let resp: KalshiMarketsResponse = self.get(&path).await?;
        Ok(resp.markets)
    }

    /// Fetch cash available to trade (in USD cents) from `/portfolio/balance`.
    /// Confirmed shape: `{balance: i64, portfolio_value: i64, updated_ts: u64}`.
    /// `balance` is what we want — cash that can be spent on new orders.
    /// `portfolio_value` is cash + mark-to-market of open positions and is
    /// *not* spendable, despite the name.
    pub async fn fetch_balance_cents(&self) -> Result<i64> {
        #[derive(serde::Deserialize)]
        struct BalanceResp {
            balance: i64,
        }
        let resp: BalanceResp = self.get("/portfolio/balance").await?;
        Ok(resp.balance)
    }

    /// Generic authenticated POST request
    async fn post<T: serde::de::DeserializeOwned, B: Serialize>(&self, path: &str, body: &B) -> Result<T> {
        let url = format!("{}{}", KALSHI_API_BASE, path);
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        // Kalshi signature uses FULL path including /trade-api/v2 prefix
        let full_path = format!("/trade-api/v2{}", path);
        let msg = format!("{}POST{}", timestamp_ms, full_path);
        let signature = self.config.sign(&msg)?;

        let resp = self.http
            .post(&url)
            .header("KALSHI-ACCESS-KEY", &self.config.api_key_id)
            .header("KALSHI-ACCESS-SIGNATURE", &signature)
            .header("KALSHI-ACCESS-TIMESTAMP", timestamp_ms.to_string())
            .header("Content-Type", "application/json")
            .timeout(ORDER_TIMEOUT)
            .json(body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Kalshi API error {}: {}", status, body);
        }
        
        let data: T = resp.json().await?;
        Ok(data)
    }
    
    /// Create an order on Kalshi
    pub async fn create_order(&self, order: &KalshiOrderRequest<'_>) -> Result<KalshiOrderResponse> {
        let path = "/portfolio/orders";
        self.post(path, order).await
    }
    
    /// Create an IOC buy order (convenience method)
    pub async fn buy_ioc(
        &self,
        ticker: &str,
        side: &str,  // "yes" or "no"
        price_cents: i64,
        count: i64,
    ) -> Result<KalshiOrderResponse> {
        debug_assert!(!ticker.is_empty(), "ticker must not be empty");
        debug_assert!(price_cents >= 1 && price_cents <= 99, "price must be 1-99");
        debug_assert!(count >= 1, "count must be >= 1");

        let side_static: &'static str = if side == "yes" { "yes" } else { "no" };
        let order_id = Self::next_order_id();
        let order = KalshiOrderRequest::ioc_buy(
            Cow::Borrowed(ticker),
            side_static,
            price_cents,
            count,
            Cow::Borrowed(&order_id)
        );
        debug!("[KALSHI] IOC {} {} @{}¢ x{}", side, ticker, price_cents, count);

        let resp = self.create_order(&order).await?;
        debug!("[KALSHI] {} filled={}", resp.order.status, resp.order.filled_count());
        Ok(resp)
    }

    pub async fn sell_ioc(
        &self,
        ticker: &str,
        side: &str,
        price_cents: i64,
        count: i64,
    ) -> Result<KalshiOrderResponse> {
        debug_assert!(!ticker.is_empty(), "ticker must not be empty");
        debug_assert!(price_cents >= 1 && price_cents <= 99, "price must be 1-99");
        debug_assert!(count >= 1, "count must be >= 1");

        let side_static: &'static str = if side == "yes" { "yes" } else { "no" };
        let order_id = Self::next_order_id();
        let order = KalshiOrderRequest::ioc_sell(
            Cow::Borrowed(ticker),
            side_static,
            price_cents,
            count,
            Cow::Borrowed(&order_id)
        );
        debug!("[KALSHI] SELL {} {} @{}¢ x{}", side, ticker, price_cents, count);

        let resp = self.create_order(&order).await?;
        debug!("[KALSHI] {} filled={}", resp.order.status, resp.order.filled_count());
        Ok(resp)
    }
}

// === WebSocket Message Types ===

#[derive(Deserialize, Debug)]
pub struct KalshiWsMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub msg: Option<KalshiWsMsgBody>,
}

#[allow(dead_code)]
#[derive(Deserialize, Debug)]
pub struct KalshiWsMsgBody {
    pub market_ticker: Option<String>,
    pub market_id: Option<String>,
    // Snapshot fields (new schema): [price_dollars_str, qty_str]
    pub yes_dollars_fp: Option<Vec<Vec<String>>>,
    pub no_dollars_fp: Option<Vec<Vec<String>>>,
    // Delta fields (new schema)
    pub price_dollars: Option<String>,
    pub delta_fp: Option<String>,
    pub side: Option<String>,
}

#[inline]
fn dollar_str_to_cents(s: &str) -> i64 {
    s.parse::<f64>().map(|d| (d * 100.0).round() as i64).unwrap_or(0)
}

#[inline]
fn qty_str_to_int(s: &str) -> i64 {
    s.parse::<f64>().map(|q| q.round() as i64).unwrap_or(0)
}

/// Per-market orderbook maintained by the WS reader task.
///
/// Kalshi's feed publishes BIDS only (offers to buy YES and offers to buy NO).
/// To buy YES we must cross the best NO bid: yes_ask = 100 - best_no_bid.
/// To buy NO we must cross the best YES bid: no_ask = 100 - best_yes_bid.
///
/// Keys are price in cents (1..=99). Values are quantity in contracts.
/// Only the single WS task mutates this, so no synchronization is needed.
#[derive(Default, Debug)]
pub struct KalshiBook {
    pub yes_bids: BTreeMap<i64, i64>,
    pub no_bids: BTreeMap<i64, i64>,
}

impl KalshiBook {
    /// Replace all levels from a fresh snapshot.
    pub fn apply_snapshot(&mut self, body: &KalshiWsMsgBody) {
        self.yes_bids.clear();
        self.no_bids.clear();

        let ingest = |levels: &Vec<Vec<String>>, dest: &mut BTreeMap<i64, i64>| {
            for l in levels {
                if l.len() < 2 { continue; }
                let price = dollar_str_to_cents(&l[0]);
                let qty = qty_str_to_int(&l[1]);
                if qty > 0 && (1..=99).contains(&price) {
                    dest.insert(price, qty);
                }
            }
        };

        if let Some(levels) = &body.yes_dollars_fp { ingest(levels, &mut self.yes_bids); }
        if let Some(levels) = &body.no_dollars_fp { ingest(levels, &mut self.no_bids); }
    }

    /// Apply a single-level delta. Returns true iff the book changed.
    pub fn apply_delta(&mut self, body: &KalshiWsMsgBody) -> bool {
        let (Some(price_str), Some(delta_str), Some(side_str)) = (
            body.price_dollars.as_deref(),
            body.delta_fp.as_deref(),
            body.side.as_deref(),
        ) else { return false; };

        let price = dollar_str_to_cents(price_str);
        let delta = qty_str_to_int(delta_str);
        if !(1..=99).contains(&price) || delta == 0 { return false; }

        let levels = match side_str {
            "yes" => &mut self.yes_bids,
            "no" => &mut self.no_bids,
            _ => return false,
        };

        let current = levels.get(&price).copied().unwrap_or(0);
        let new_qty = current.saturating_add(delta);
        if new_qty <= 0 {
            levels.remove(&price);
        } else {
            levels.insert(price, new_qty);
        }
        true
    }

    /// Publish derived top-of-book into the lock-free `AtomicOrderbook`.
    /// Kalshi publishes BIDS, so asks are computed from the opposite side.
    pub fn publish_top(&self, market: &crate::types::AtomicMarketState) {
        // To buy NO we cross the best YES bid.
        let (no_ask, no_size) = self.yes_bids.iter().next_back()
            .map(|(&p, &q)| ((100 - p) as PriceCents, (q * p / 100) as SizeCents))
            .unwrap_or((0, 0));

        // To buy YES we cross the best NO bid.
        let (yes_ask, yes_size) = self.no_bids.iter().next_back()
            .map(|(&p, &q)| ((100 - p) as PriceCents, (q * p / 100) as SizeCents))
            .unwrap_or((0, 0));

        market.kalshi.store(yes_ask, no_ask, yes_size, no_size);
    }
}

#[derive(Serialize)]
struct SubscribeCmd {
    id: i32,
    cmd: &'static str,
    params: SubscribeParams,
}

#[derive(Serialize)]
struct SubscribeParams {
    channels: Vec<&'static str>,
    market_tickers: Vec<String>,
}

// =============================================================================
// WebSocket Runner
// =============================================================================

/// WebSocket runner
pub async fn run_ws(
    config: &KalshiConfig,
    state: Arc<GlobalState>,
    exec_tx: mpsc::Sender<FastExecutionRequest>,
    threshold_cents: PriceCents,
) -> Result<()> {
    let tickers: Vec<String> = state.markets.iter()
        .take(state.market_count())
        .filter_map(|m| m.pair.as_ref().map(|p| p.kalshi_market_ticker.to_string()))
        .collect();

    if tickers.is_empty() {
        info!("[KALSHI] No markets to monitor");
        tokio::time::sleep(Duration::from_secs(u64::MAX)).await;
        return Ok(());
    }

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis()
        .to_string();

    let signature = config.sign(&format!("{}GET/trade-api/ws/v2", timestamp))?;

    let request = Request::builder()
        .uri(KALSHI_WS_URL)
        .header("KALSHI-ACCESS-KEY", &config.api_key_id)
        .header("KALSHI-ACCESS-SIGNATURE", &signature)
        .header("KALSHI-ACCESS-TIMESTAMP", &timestamp)
        .header("Host", "api.elections.kalshi.com")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
        .body(())?;

    let (ws_stream, _) = connect_async(request).await.context("Failed to connect to Kalshi")?;
    info!("[KALSHI] Connected");

    let (mut write, mut read) = ws_stream.split();

    // Subscribe to all tickers
    let subscribe_msg = SubscribeCmd {
        id: 1,
        cmd: "subscribe",
        params: SubscribeParams {
            channels: vec!["orderbook_delta"],
            market_tickers: tickers.clone(),
        },
    };

    write.send(Message::Text(serde_json::to_string(&subscribe_msg)?)).await?;
    info!("[KALSHI] Subscribed to {} markets", tickers.len());

    let clock = NanoClock::new();

    // Per-market orderbooks. Keyed by market_id (our internal u16). The WS task
    // is the only writer; arb consumers read the derived top-of-book via the
    // lock-free AtomicOrderbook that `publish_top` stores into.
    let mut books: HashMap<u16, KalshiBook> = HashMap::with_capacity(tickers.len());

    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                match serde_json::from_str::<KalshiWsMessage>(&text) {
                    Ok(kalshi_msg) => {
                        let ticker = kalshi_msg.msg.as_ref()
                            .and_then(|m| m.market_ticker.as_ref());

                        let Some(ticker) = ticker else {
                            tracing::info!("[KALSHI-WS] {}: {}", kalshi_msg.msg_type, &text[..text.len().min(400)]);
                            continue;
                        };
                        let ticker_hash = fxhash_str(ticker);

                        let Some(&market_id) = state.kalshi_to_id.get(&ticker_hash) else { continue };
                        let market = &state.markets[market_id as usize];

                        match kalshi_msg.msg_type.as_str() {
                            "orderbook_snapshot" => {
                                if let Some(body) = &kalshi_msg.msg {
                                    let book = books.entry(market_id).or_default();
                                    book.apply_snapshot(body);
                                    book.publish_top(market);

                                    let arb_mask = market.check_arbs(threshold_cents);
                                    if arb_mask != 0 {
                                        send_kalshi_arb_request(market_id, market, arb_mask, &exec_tx, &clock).await;
                                    }
                                }
                            }
                            "orderbook_delta" => {
                                if let Some(body) = &kalshi_msg.msg {
                                    // Only apply deltas after a snapshot has seeded the book.
                                    // A pre-snapshot delta would silently diverge from Kalshi's ground truth.
                                    if let Some(book) = books.get_mut(&market_id) {
                                        if book.apply_delta(body) {
                                            book.publish_top(market);

                                            let arb_mask = market.check_arbs(threshold_cents);
                                            if arb_mask != 0 {
                                                send_kalshi_arb_request(market_id, market, arb_mask, &exec_tx, &clock).await;
                                            }
                                        }
                                    }
                                }
                            }
                            other => {
                                tracing::info!("[KALSHI-WS] {}: {}", other, &text[..text.len().min(300)]);
                            }
                        }
                    }
                    Err(e) => {
                        // Log at trace level - unknown message types are normal
                        tracing::trace!("[KALSHI] WS parse error: {} (msg: {}...)", e, &text[..text.len().min(100)]);
                    }
                }
            }
            Ok(Message::Ping(data)) => {
                let _ = write.send(Message::Pong(data)).await;
            }
            Err(e) => {
                error!("[KALSHI] WebSocket error: {}", e);
                break;
            }
            _ => {}
        }
    }

    Ok(())
}

/// Send arb request from Kalshi handler
#[inline]
async fn send_kalshi_arb_request(
    market_id: u16,
    market: &crate::types::AtomicMarketState,
    arb_mask: u8,
    exec_tx: &mpsc::Sender<FastExecutionRequest>,
    clock: &NanoClock,
) {
    let (k_yes, k_no, k_yes_size, k_no_size) = market.kalshi.load();
    let (p_yes, p_no, p_yes_size, p_no_size) = market.poly.load();

    let (yes_price, no_price, yes_size, no_size, arb_type) = if arb_mask & 1 != 0 {
        (p_yes, k_no, p_yes_size, k_no_size, ArbType::PolyYesKalshiNo)
    } else if arb_mask & 2 != 0 {
        (k_yes, p_no, k_yes_size, p_no_size, ArbType::KalshiYesPolyNo)
    } else if arb_mask & 4 != 0 {
        (p_yes, p_no, p_yes_size, p_no_size, ArbType::PolyOnly)
    } else if arb_mask & 8 != 0 {
        (k_yes, k_no, k_yes_size, k_no_size, ArbType::KalshiOnly)
    } else {
        return;
    };

    let req = FastExecutionRequest {
        market_id,
        yes_price,
        no_price,
        yes_size,
        no_size,
        arb_type,
        detected_ns: clock.now_ns(),
    };

    let _ = exec_tx.try_send(req);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_body(yes: &[(&str, &str)], no: &[(&str, &str)]) -> KalshiWsMsgBody {
        let to_levels = |v: &[(&str, &str)]| -> Vec<Vec<String>> {
            v.iter().map(|(p, q)| vec![p.to_string(), q.to_string()]).collect()
        };
        KalshiWsMsgBody {
            market_ticker: None,
            market_id: None,
            yes_dollars_fp: Some(to_levels(yes)),
            no_dollars_fp: Some(to_levels(no)),
            price_dollars: None,
            delta_fp: None,
            side: None,
        }
    }

    fn delta_body(price: &str, delta: &str, side: &str) -> KalshiWsMsgBody {
        KalshiWsMsgBody {
            market_ticker: None,
            market_id: None,
            yes_dollars_fp: None,
            no_dollars_fp: None,
            price_dollars: Some(price.to_string()),
            delta_fp: Some(delta.to_string()),
            side: Some(side.to_string()),
        }
    }

    #[test]
    fn snapshot_then_deltas_derive_correct_top_of_book() {
        let mut book = KalshiBook::default();

        book.apply_snapshot(&snapshot_body(
            &[("0.40", "10"), ("0.41", "5"), ("0.42", "3")],
            &[("0.55", "7"), ("0.56", "2")],
        ));
        assert_eq!(book.yes_bids.iter().next_back(), Some((&42, &3)));
        assert_eq!(book.no_bids.iter().next_back(), Some((&56, &2)));

        // Add to an existing level.
        assert!(book.apply_delta(&delta_body("0.42", "4", "yes")));
        assert_eq!(book.yes_bids.get(&42), Some(&7));

        // Reduce a level but keep it.
        assert!(book.apply_delta(&delta_body("0.42", "-5", "yes")));
        assert_eq!(book.yes_bids.get(&42), Some(&2));

        // Remove the level entirely — new top becomes 41.
        assert!(book.apply_delta(&delta_body("0.42", "-2", "yes")));
        assert_eq!(book.yes_bids.get(&42), None);
        assert_eq!(book.yes_bids.iter().next_back(), Some((&41, &5)));

        // New level added to NO side via delta.
        assert!(book.apply_delta(&delta_body("0.57", "8", "no")));
        assert_eq!(book.no_bids.iter().next_back(), Some((&57, &8)));

        // Over-reduction still removes (saturating behavior — no negative qty).
        assert!(book.apply_delta(&delta_body("0.55", "-999", "no")));
        assert_eq!(book.no_bids.get(&55), None);
    }

    #[test]
    fn malformed_deltas_are_rejected() {
        let mut book = KalshiBook::default();
        book.apply_snapshot(&snapshot_body(&[("0.40", "10")], &[("0.55", "5")]));

        // Zero-delta: no-op.
        assert!(!book.apply_delta(&delta_body("0.40", "0", "yes")));
        // Price out of range: no-op.
        assert!(!book.apply_delta(&delta_body("1.50", "1", "yes")));
        // Unknown side: no-op.
        assert!(!book.apply_delta(&delta_body("0.40", "1", "maybe")));

        assert_eq!(book.yes_bids.get(&40), Some(&10));
        assert_eq!(book.no_bids.get(&55), Some(&5));
    }
}
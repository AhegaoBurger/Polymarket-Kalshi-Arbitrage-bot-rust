//! Polymarket CLOB (Central Limit Order Book) order execution client.
//!
//! This module provides high-performance order execution for the Polymarket CLOB,
//! including pre-computed authentication credentials and optimized request handling.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE;
use ethers::signers::{LocalWallet, Signer};
use ethers::types::H256;
use ethers::types::transaction::eip712::{Eip712, TypedData};
use ethers::types::U256;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const USER_AGENT: &str = "py_clob_client";
const MSG_TO_SIGN: &str = "This message attests that I control the given wallet";
const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

// ============================================================================
// PRE-COMPUTED EIP712 CONSTANTS
// ============================================================================

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiCreds {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    #[serde(rename = "secret")]
    pub api_secret: String,
    #[serde(rename = "passphrase")]
    pub api_passphrase: String,
}

// ============================================================================
// PREPARED CREDENTIALS
// ============================================================================

#[derive(Clone)]
pub struct PreparedCreds {
    pub api_key: String,
    hmac_template: HmacSha256,
    api_key_header: HeaderValue,
    passphrase_header: HeaderValue,
}

impl PreparedCreds {
    pub fn from_api_creds(creds: &ApiCreds) -> Result<Self> {
        let decoded_secret = URL_SAFE.decode(&creds.api_secret)?;
        let hmac_template = HmacSha256::new_from_slice(&decoded_secret)
            .map_err(|e| anyhow!("Invalid HMAC key: {}", e))?;

        let api_key_header = HeaderValue::from_str(&creds.api_key)
            .map_err(|e| anyhow!("Invalid API key for header: {}", e))?;
        let passphrase_header = HeaderValue::from_str(&creds.api_passphrase)
            .map_err(|e| anyhow!("Invalid passphrase for header: {}", e))?;

        Ok(Self {
            api_key: creds.api_key.clone(),
            hmac_template,
            api_key_header,
            passphrase_header,
        })
    }

    /// Sign message using prewarmed HMAC
    #[inline]
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let mut mac = self.hmac_template.clone();
        mac.update(message);
        mac.finalize().into_bytes().to_vec()
    }

    /// Sign and return base64 (for L2 headers)
    #[inline]
    pub fn sign_b64(&self, message: &[u8]) -> String {
        URL_SAFE.encode(self.sign(message))
    }

    /// Get cached API key header
    #[inline]
    pub fn api_key_header(&self) -> HeaderValue {
        self.api_key_header.clone()
    }

    /// Get cached passphrase header
    #[inline]
    pub fn passphrase_header(&self) -> HeaderValue {
        self.passphrase_header.clone()
    }
}

fn add_default_headers(headers: &mut HeaderMap) {
    headers.insert("User-Agent", HeaderValue::from_static(USER_AGENT));
    headers.insert("Accept", HeaderValue::from_static("*/*"));
    headers.insert("Connection", HeaderValue::from_static("keep-alive"));
    headers.insert("Content-Type", HeaderValue::from_static("application/json"));
}

#[inline(always)]
fn current_unix_ts() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

/// CLOB V2 order `timestamp` is in milliseconds (replaces V1's `nonce` for
/// per-address uniqueness). The L1 auth `POLY_TIMESTAMP` header stays seconds.
fn current_unix_ts_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// L1-auth single-use nonce. Polymarket V2's `derive-api-key` and `api-key`
/// endpoints reject reused (signer, nonce) pairs — using `0` always (as the
/// bot did pre-V2) yields a generic "Could not derive api key!" 400 after
/// the first call. Unix-nanoseconds is monotonically increasing across
/// process restarts and unique enough that collisions are negligible.
fn fresh_nonce() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

/// V2 EIP-712 zero-bytes32 used for `metadata` and `builder` when no value
/// is being attached (which is the bot's default — we're not a builder).
const ZERO_BYTES32: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

fn clob_auth_digest(chain_id: u64, address_str: &str, timestamp: u64, nonce: u64) -> Result<H256> {
    let typed_json = json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"}
            ],
            "ClobAuth": [
                {"name": "address", "type": "address"},
                {"name": "timestamp", "type": "string"},
                {"name": "nonce", "type": "uint256"},
                {"name": "message", "type": "string"}
            ]
        },
        "primaryType": "ClobAuth",
        "domain": { "name": "ClobAuthDomain", "version": "1", "chainId": chain_id },
        "message": { "address": address_str, "timestamp": timestamp.to_string(), "nonce": nonce, "message": MSG_TO_SIGN }
    });
    let typed: TypedData = serde_json::from_value(typed_json)?;
    Ok(typed.encode_eip712()?.into())
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OrderArgs {
    pub token_id: String,
    pub price: f64,
    pub size: f64,
    pub side: String,
    pub fee_rate_bps: Option<i64>,
    pub nonce: Option<i64>,
    pub expiration: Option<String>,
    pub taker: Option<String>,
}

/// CLOB V2 EIP-712 sign payload. References to avoid clones in the hot path.
///
/// V2 (vs V1) drops `taker`, `expiration`, `nonce`, `feeRateBps` from the
/// signed struct and adds `timestamp` (ms), `metadata` (bytes32), `builder`
/// (bytes32). See spec at docs.polymarket.com/migration §"For API users".
struct OrderData<'a> {
    maker: &'a str,
    signer: &'a str,
    token_id: &'a str,
    maker_amount: &'a str,
    taker_amount: &'a str,
    side: i32,
    signature_type: i32,
    salt: u128,
    /// Order creation time in **milliseconds**. Replaces V1's `nonce` for
    /// per-address uniqueness. Not an expiration.
    timestamp_ms: u64,
    /// V2 bytes32. Default `ZERO_BYTES32` — no semantics attached.
    metadata: &'a str,
    /// V2 bytes32 builder code. Default `ZERO_BYTES32` — bot is not a builder.
    builder: &'a str,
}

/// CLOB V2 wire-body order struct. V2 keeps `expiration` in the POST body for
/// GTD/order-expiry handling but it is NOT part of the EIP-712 signed struct.
/// `nonce` and `feeRateBps` are gone; `timestamp` (ms), `metadata`, `builder`
/// are added.
#[derive(Debug, Clone, Serialize)]
pub struct OrderStruct {
    pub salt: u128,
    pub maker: String,
    pub signer: String,
    pub taker: String,
    #[serde(rename = "tokenId")]
    pub token_id: String,
    #[serde(rename = "makerAmount")]
    pub maker_amount: String,
    #[serde(rename = "takerAmount")]
    pub taker_amount: String,
    /// Order expiration timestamp (seconds). `"0"` means GTC.
    /// Stays in the wire body but no longer in the EIP-712 sign payload.
    pub expiration: String,
    pub side: i32,
    #[serde(rename = "signatureType")]
    pub signature_type: i32,
    /// V2: order creation time in milliseconds (replaces V1 `nonce`).
    pub timestamp: String,
    /// V2 bytes32. Default zeros.
    pub metadata: String,
    /// V2 bytes32 builder code. Default zeros.
    pub builder: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignedOrder { 
    pub order: OrderStruct, 
    pub signature: String 
}

impl SignedOrder {
    /// Render a V2 POST /order body. `taker` and `expiration` stay in the body
    /// (per docs) but are absent from the EIP-712 sign struct. `nonce` and
    /// `feeRateBps` are gone entirely. `timestamp`/`metadata`/`builder` are new.
    pub fn post_body(&self, owner: &str, order_type: &str) -> String {
        let side_str = if self.order.side == 0 { "BUY" } else { "SELL" };
        let mut buf = String::with_capacity(640);
        buf.push_str(r#"{"order":{"salt":"#);
        buf.push_str(&self.order.salt.to_string());
        buf.push_str(r#","maker":""#);
        buf.push_str(&self.order.maker);
        buf.push_str(r#"","signer":""#);
        buf.push_str(&self.order.signer);
        buf.push_str(r#"","taker":""#);
        buf.push_str(&self.order.taker);
        buf.push_str(r#"","tokenId":""#);
        buf.push_str(&self.order.token_id);
        buf.push_str(r#"","makerAmount":""#);
        buf.push_str(&self.order.maker_amount);
        buf.push_str(r#"","takerAmount":""#);
        buf.push_str(&self.order.taker_amount);
        buf.push_str(r#"","expiration":""#);
        buf.push_str(&self.order.expiration);
        buf.push_str(r#"","side":""#);
        buf.push_str(side_str);
        buf.push_str(r#"","signatureType":"#);
        buf.push_str(&self.order.signature_type.to_string());
        buf.push_str(r#","timestamp":""#);
        buf.push_str(&self.order.timestamp);
        buf.push_str(r#"","metadata":""#);
        buf.push_str(&self.order.metadata);
        buf.push_str(r#"","builder":""#);
        buf.push_str(&self.order.builder);
        buf.push_str(r#"","signature":""#);
        buf.push_str(&self.signature);
        buf.push_str(r#""},"owner":""#);
        buf.push_str(owner);
        buf.push_str(r#"","orderType":""#);
        buf.push_str(order_type);
        buf.push_str(r#""}"#);
        buf
    }
}

#[inline(always)]
fn generate_seed() -> u128 {
    (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() % u128::from(u32::MAX)) as u128
}

// ============================================================================
// ORDER CALCULATIONS
// ============================================================================

/// Convert f64 price (0.0-1.0) to basis points (0-10000)
/// e.g., 0.65 -> 6500
#[inline(always)]
pub fn price_to_bps(price: f64) -> u64 {
    ((price * 10000.0).round() as i64).max(0) as u64
}

/// Convert f64 size to micro-units (6 decimal places)
/// e.g., 100.5 -> 100_500_000
#[inline(always)]
pub fn size_to_micro(size: f64) -> u64 {
    ((size * 1_000_000.0).floor() as i64).max(0) as u64
}

/// BUY order calculation
/// Input: size in micro-units, price in basis points
/// Output: (side=0, maker_amount, taker_amount) in token decimals (6 dp)
#[inline(always)]
pub fn get_order_amounts_buy(size_micro: u64, price_bps: u64) -> (i32, u128, u128) {
    // For BUY: taker = size (what we receive), maker = size * price (what we pay)
    let taker = size_micro as u128;
    // maker = size * price / 10000 (convert bps to ratio)
    let maker = (size_micro as u128 * price_bps as u128) / 10000;
    (0, maker, taker)
}

/// SELL order calculation
/// Input: size in micro-units, price in basis points
/// Output: (side=1, maker_amount, taker_amount) in token decimals (6 dp)
#[inline(always)]
pub fn get_order_amounts_sell(size_micro: u64, price_bps: u64) -> (i32, u128, u128) {
    // For SELL: maker = size (what we give), taker = size * price (what we receive)
    let maker = size_micro as u128;
    // taker = size * price / 10000 (convert bps to ratio)
    let taker = (size_micro as u128 * price_bps as u128) / 10000;
    (1, maker, taker)
}

/// Validate price is within allowed range for tick=0.01
#[inline(always)]
pub fn price_valid(price_bps: u64) -> bool {
    // For tick=0.01: price must be >= 0.01 (100 bps) and <= 0.99 (9900 bps)
    price_bps >= 100 && price_bps <= 9900
}

fn order_typed_data(chain_id: u64, exchange: &str, data: &OrderData<'_>) -> Result<TypedData> {
    // CLOB V2 EIP-712 — Exchange domain version "2" (V1 was "1"). The Order
    // struct drops taker/expiration/nonce/feeRateBps and adds
    // timestamp/metadata/builder. See: docs.polymarket.com/migration.
    let typed_json = json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"},
                {"name": "verifyingContract", "type": "address"}
            ],
            "Order": [
                {"name":"salt","type":"uint256"},
                {"name":"maker","type":"address"},
                {"name":"signer","type":"address"},
                {"name":"tokenId","type":"uint256"},
                {"name":"makerAmount","type":"uint256"},
                {"name":"takerAmount","type":"uint256"},
                {"name":"side","type":"uint8"},
                {"name":"signatureType","type":"uint8"},
                {"name":"timestamp","type":"uint256"},
                {"name":"metadata","type":"bytes32"},
                {"name":"builder","type":"bytes32"}
            ]
        },
        "primaryType": "Order",
        "domain": {
            "name": "Polymarket CTF Exchange",
            "version": "2",
            "chainId": chain_id,
            "verifyingContract": exchange
        },
        "message": {
            "salt": U256::from(data.salt),
            "maker": data.maker,
            "signer": data.signer,
            "tokenId": U256::from_dec_str(data.token_id)?,
            "makerAmount": U256::from_dec_str(data.maker_amount)?,
            "takerAmount": U256::from_dec_str(data.taker_amount)?,
            "side": data.side,
            "signatureType": data.signature_type,
            "timestamp": U256::from(data.timestamp_ms),
            "metadata": data.metadata,
            "builder": data.builder,
        }
    });
    Ok(serde_json::from_value(typed_json)?)
}

/// CLOB V2 Exchange `verifyingContract` addresses. V1 addresses are no longer
/// accepted by the V2 backend (live since 2026-04-28).
/// See: docs.polymarket.com/resources/contracts.
fn get_exchange_address(chain_id: u64, neg_risk: bool) -> Result<String> {
    match (chain_id, neg_risk) {
        // Polygon mainnet — V2
        (137, false) => Ok("0xE111180000d2663C0091e4f400237545B87B996B".into()),
        (137, true)  => Ok("0xe2222d279d744050d28e00520010520000310F59".into()),
        // Amoy testnet — V2 addresses not yet documented; fall back to error
        // until Polymarket publishes them. The bot doesn't run on testnet today.
        (80002, _) => Err(anyhow!(
            "Polygon Amoy V2 Exchange address not configured — see docs.polymarket.com/resources/contracts"
        )),
        _ => Err(anyhow!("unsupported chain")),
    }
}

// ============================================================================
// ORDER TYPES FOR FAK/FOK
// ============================================================================

/// Order type for Polymarket
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum PolyOrderType {
    /// Good Till Cancelled (default)
    GTC,
    /// Good Till Time
    GTD,
    /// Fill Or Kill - must fill entirely or cancel
    FOK,
    /// Fill And Kill - fill what you can, cancel rest
    FAK,
}

impl PolyOrderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PolyOrderType::GTC => "GTC",
            PolyOrderType::GTD => "GTD",
            PolyOrderType::FOK => "FOK",
            PolyOrderType::FAK => "FAK",
        }
    }
}

// ============================================================================
// GET ORDER RESPONSE
// ============================================================================

/// Response from GET /data/order/{order_id}
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PolymarketOrderResponse {
    pub id: String,
    pub status: String,
    pub market: Option<String>,
    pub outcome: Option<String>,
    pub price: String,
    pub side: String,
    pub size_matched: String,
    pub original_size: String,
    pub maker_address: Option<String>,
    pub asset_id: Option<String>,
    #[serde(default)]
    pub associate_trades: Vec<serde_json::Value>,
    #[serde(default)]
    pub created_at: Option<serde_json::Value>,  // Can be string or integer
    #[serde(default)]
    pub expiration: Option<serde_json::Value>,  // Can be string or integer
    #[serde(rename = "type")]
    pub order_type: Option<String>,
    pub owner: Option<String>,
}

// ============================================================================
// ASYNC CLIENT
// ============================================================================

/// Async Polymarket client for execution
pub struct PolymarketAsyncClient {
    host: String,
    chain_id: u64,
    http: reqwest::Client,  // Async client with connection pooling
    wallet: Arc<LocalWallet>,
    funder: String,
    wallet_address_str: String,
    address_header: HeaderValue,
    /// EIP-712 `signatureType` for the configured wallet. Auto-detected at
    /// startup: 0 if funder==signer (EOA), else 1 (POLY_PROXY).
    signature_type: i32,
    /// One-shot guard so we log the raw CLOB `/markets/{id}` response exactly
    /// once per process — used to diagnose which JSON key carries the taker fee.
    logged_meta_shape: AtomicBool,
}

/// Polymarket CLOB signature types.
///   0 = EOA               (funder == signer, raw external wallet)
///   1 = POLY_PROXY        (funder is a Polymarket email/Magic login proxy)
///   2 = POLY_GNOSIS_SAFE  (funder is a Gnosis Safe — MetaMask / external wallet login)
/// Picking the wrong one produces a valid ECDSA signature that fails server-side
/// verification with "invalid signature" — the server runs different EIP-1271
/// logic per type against different proxy contract shapes.
///
/// Detection rules:
///   1. `POLY_SIG_TYPE` env var overrides if set to 0/1/2.
///   2. funder == signer             → 0 (EOA).
///   3. funder != signer (the rest)  → 2 (Gnosis Safe), because MetaMask /
///      external-wallet login is the default Polymarket account type for
///      users who hold their own private keys. Magic/email users are rare in
///      an API trading context and can set POLY_SIG_TYPE=1 explicitly.
#[inline]
fn detect_signature_type(funder: &str, wallet_address: &str) -> i32 {
    if let Ok(s) = std::env::var("POLY_SIG_TYPE") {
        if let Ok(n) = s.trim().parse::<i32>() {
            if (0..=2).contains(&n) {
                return n;
            }
        }
    }
    if funder.trim().eq_ignore_ascii_case(wallet_address.trim()) {
        0
    } else {
        2
    }
}

impl PolymarketAsyncClient {
    pub fn new(host: &str, chain_id: u64, private_key: &str, funder: &str) -> Result<Self> {
        let wallet = private_key.parse::<LocalWallet>()?.with_chain_id(chain_id);
        let wallet_address_str = format!("{:?}", wallet.address());
        let address_header = HeaderValue::from_str(&wallet_address_str)
            .map_err(|e| anyhow!("Invalid wallet address for header: {}", e))?;

        let sig_type = detect_signature_type(funder, &wallet_address_str);
        let sig_type_name = match sig_type {
            0 => "EOA",
            1 => "POLY_PROXY",
            2 => "POLY_GNOSIS_SAFE",
            _ => "UNKNOWN",
        };
        tracing::info!(
            "[POLYMARKET] Wallet signer={} funder={} → signatureType={} ({})",
            wallet_address_str, funder, sig_type, sig_type_name
        );

        // Build async client with connection pooling and keepalive
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .tcp_nodelay(true)
            .timeout(std::time::Duration::from_secs(10))
            .build()?;

        Ok(Self {
            host: host.trim_end_matches('/').to_string(),
            chain_id,
            http,
            wallet: Arc::new(wallet),
            funder: funder.to_string(),
            wallet_address_str,
            address_header,
            signature_type: sig_type,
            logged_meta_shape: AtomicBool::new(false),
        })
    }

    /// Build L1 headers for authentication (derive-api-key)
    /// wallet.sign_hash() is CPU-bound (~1ms), safe to call in async context
    fn build_l1_headers(&self, nonce: u64) -> Result<HeaderMap> {
        let timestamp = current_unix_ts();
        let digest = clob_auth_digest(self.chain_id, &self.wallet_address_str, timestamp, nonce)?;
        let sig = self.wallet.sign_hash(digest)?;
        let mut headers = HeaderMap::new();
        headers.insert("POLY_ADDRESS", self.address_header.clone());
        headers.insert("POLY_SIGNATURE", HeaderValue::from_str(&format!("0x{}", sig))?);
        headers.insert("POLY_TIMESTAMP", HeaderValue::from_str(&timestamp.to_string())?);
        headers.insert("POLY_NONCE", HeaderValue::from_str(&nonce.to_string())?);
        add_default_headers(&mut headers);
        Ok(headers)
    }

    /// Derive API credentials from L1 wallet signature.
    ///
    /// On CLOB V2, this endpoint may return 400 if the wallet already has
    /// API creds — it now strictly creates new ones. Use `get_api_creds`
    /// first to check for existing creds, then fall back to derive only when
    /// the wallet has none.
    pub async fn derive_api_key(&self, nonce: u64) -> Result<ApiCreds> {
        let url = format!("{}/auth/derive-api-key", self.host);
        let headers = self.build_l1_headers(nonce)?;
        let resp = self.http.get(&url).headers(headers).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("derive-api-key failed: {} {}", status, body));
        }
        Ok(resp.json().await?)
    }

    /// Create a new API key for this wallet via L1 auth.
    ///
    /// POST `/auth/api-key` (this is what py-clob-client calls `create_api_key`).
    /// Returns the full ApiCreds (api_key + secret + passphrase). Always
    /// creates a fresh key — does not return existing ones. The wallet can
    /// hold multiple API keys; orphaned ones can be deleted via the
    /// Polymarket UI or DELETE `/auth/api-key`.
    pub async fn create_api_key(&self, nonce: u64) -> Result<ApiCreds> {
        let url = format!("{}/auth/api-key", self.host);
        let headers = self.build_l1_headers(nonce)?;
        let resp = self.http.post(&url).headers(headers).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("create_api_key failed: {} {}", status, body));
        }
        Ok(resp.json().await?)
    }

    /// Resolve API creds via the V2-friendly path: try `create_api_key` first
    /// (POST /auth/api-key — works on V2), fall back to `derive_api_key`
    /// (GET /auth/derive-api-key — V1-idempotent fallback).
    ///
    /// **Each call uses a fresh unique nonce.** Polymarket's L1 auth treats
    /// every (signer, nonce) as single-use; reusing nonce 0 (as the bot did
    /// pre-V2) returned the same creds in V1 but returns "Could not derive
    /// api key!" 400 in V2. Nonce is the current unix nanoseconds — unique
    /// per call across process restarts.
    pub async fn get_or_derive_api_key(&self) -> Result<ApiCreds> {
        let nonce = fresh_nonce();
        match self.create_api_key(nonce).await {
            Ok(creds) => {
                tracing::info!(
                    "[POLYMARKET] Created new API creds via POST /auth/api-key (nonce={})",
                    nonce
                );
                Ok(creds)
            }
            Err(e) => {
                let nonce2 = fresh_nonce();
                tracing::warn!(
                    "[POLYMARKET] create_api_key failed ({}), falling back to derive-api-key with new nonce={}",
                    e,
                    nonce2
                );
                self.derive_api_key(nonce2).await
            }
        }
    }

    /// Build L2 headers for authenticated requests
    fn build_l2_headers(&self, method: &str, path: &str, body: Option<&str>, creds: &PreparedCreds) -> Result<HeaderMap> {
        let timestamp = current_unix_ts();
        let mut message = format!("{}{}{}", timestamp, method, path);
        if let Some(b) = body { message.push_str(b); }

        let sig_b64 = creds.sign_b64(message.as_bytes());

        let mut headers = HeaderMap::with_capacity(9);
        headers.insert("POLY_ADDRESS", self.address_header.clone());
        headers.insert("POLY_SIGNATURE", HeaderValue::from_str(&sig_b64)?);
        headers.insert("POLY_TIMESTAMP", HeaderValue::from_str(&timestamp.to_string())?);
        headers.insert("POLY_API_KEY", creds.api_key_header());
        headers.insert("POLY_PASSPHRASE", creds.passphrase_header());
        add_default_headers(&mut headers);
        Ok(headers)
    }

    /// Post order 
    pub async fn post_order_async(&self, body: String, creds: &PreparedCreds) -> Result<reqwest::Response> {
        let path = "/order";
        let url = format!("{}{}", self.host, path);
        let headers = self.build_l2_headers("POST", path, Some(&body), creds)?;

        let resp = self.http
            .post(&url)
            .headers(headers)
            .body(body)
            .send()
            .await?;

        Ok(resp)
    }

    /// Get order by ID.
    ///
    /// Returns `Ok(None)` when the CLOB responds with JSON `null` — this happens
    /// for FAK orders that didn't match any resting liquidity (no persistent row
    /// to return). Callers should treat `None` as a zero-fill, not an error.
    pub async fn get_order_async(&self, order_id: &str, creds: &PreparedCreds) -> Result<Option<PolymarketOrderResponse>> {
        let path = format!("/data/order/{}", order_id);
        let url = format!("{}{}", self.host, path);
        let headers = self.build_l2_headers("GET", &path, None, creds)?;

        let resp = self.http
            .get(&url)
            .headers(headers)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("get_order failed {}: {}", status, body));
        }

        let val: serde_json::Value = resp.json().await?;
        if val.is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(val)?))
    }

    /// Fetch USDC collateral balance from `/balance-allowance?asset_type=COLLATERAL`.
    /// Returns the raw 6-decimal USDC micro value (23_870_001 = $23.870001).
    ///
    /// Allowances are ignored here: for typical Polymarket accounts the UI
    /// pre-approves MAX_UINT256 to all three exchange spenders on first deposit,
    /// so the binding cap is balance. If you ever hit a partially-revoked
    /// allowance, the order will 400 at submit time — rare enough to not be
    /// worth a per-spender min check on the hot path.
    ///
    /// NOTE 1: Polymarket's L2 HMAC signs the *bare* request path. The query
    /// is appended to the URL but excluded from the signed message — mirroring
    /// py-clob-client. Signing the full path+query produces a misleading 401
    /// "Unauthorized/Invalid api key" (actually a signature mismatch).
    ///
    /// NOTE 2: The `signature_type` query param tells the CLOB which account
    /// shape to resolve the balance for (0=EOA, 1=Magic proxy, 2=Safe). Without
    /// it, the CLOB defaults to the signer EOA — for Safe users that's the
    /// wrong address and returns a spurious "$0 balance."
    pub async fn fetch_balance_allowance_usdc_micros(&self, creds: &PreparedCreds) -> Result<u64> {
        let sign_path = "/balance-allowance";
        let query = format!("?asset_type=COLLATERAL&signature_type={}", self.signature_type);
        let url = format!("{}{}{}", self.host, sign_path, query);
        let headers = self.build_l2_headers("GET", sign_path, None, creds)?;

        let resp = self.http
            .get(&url)
            .headers(headers)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("balance-allowance failed {}: {}", status, text));
        }

        #[derive(serde::Deserialize)]
        struct BalanceResp {
            balance: String,
        }
        let parsed: BalanceResp = resp.json().await?;
        parsed.balance.parse::<u64>()
            .map_err(|e| anyhow!("balance-allowance: could not parse balance '{}': {}", parsed.balance, e))
    }

    /// Fetch market metadata from CLOB: returns (neg_risk, taker_fee_bps).
    /// Polymarket's `/markets/{condition_id}` is the authoritative source for fees
    /// after the 2026 fee rollout (sports, crypto, politics all have non-zero fees).
    /// We bundle neg_risk + fee into one call since both are signing-critical.
    ///
    /// The CLOB rejects orders signed with the wrong `feeRateBps`, so reading the
    /// right JSON key here is critical — if the field is missing, we bail rather
    /// than silently submit a known-bad order.
    pub async fn fetch_market_meta(&self, condition_id: &str) -> Result<(bool, i64)> {
        let url = format!("{}/markets/{}", self.host, condition_id);
        let resp = self.http
            .get(&url)
            .header("User-Agent", USER_AGENT)
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("fetch_market_meta {} failed: {}", condition_id, resp.status());
        }

        let val: serde_json::Value = resp.json().await?;

        // One-shot: log the raw response the first time so we can verify which
        // key the CLOB uses for the taker fee on this account/endpoint.
        if !self.logged_meta_shape.swap(true, Ordering::Relaxed) {
            tracing::info!(
                "[POLYMARKET] First CLOB /markets response (key survey): {}",
                val
            );
        }

        // Fee extraction has two paths:
        //   1. Structured `feeSchedule` object (forward-compat per March 2026
        //      Polymarket reporting; absent on all April 2026 surveyed markets
        //      — see docs/notes/2026-04-21-polymarket-fee-survey.md).
        //   2. Flat legacy keys; `taker_base_fee` is the only one observed live.
        // Values may arrive as number or string — handle both.
        let mut fee: Option<i64> = None;
        let mut matched_key: Option<String> = None;

        if let Some(fs) = val.get("feeSchedule").or_else(|| val.get("fee_schedule")) {
            for k in ["takerBaseFee", "taker_base_fee", "takerFee", "taker_fee"] {
                if let Some(n) = fs.get(k).and_then(|v| v.as_i64()) {
                    fee = Some(n);
                    matched_key = Some(format!("feeSchedule.{}", k));
                    break;
                }
                if let Some(n) = fs.get(k)
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    fee = Some(n);
                    matched_key = Some(format!("feeSchedule.{}", k));
                    break;
                }
            }
        }
        if fee.is_none() {
            let legacy_keys = [
                "taker_base_fee",
                "takerBaseFee",
                "taker_fee_rate_bps",
                "takerFeeRateBps",
                "fee_rate_bps",
                "feeRateBps",
                "taker_fee",
                "takerFee",
            ];
            for k in legacy_keys {
                if let Some(n) = val[k].as_i64() {
                    fee = Some(n);
                    matched_key = Some(k.to_string());
                    break;
                }
                if let Some(n) = val[k].as_str().and_then(|s| s.parse::<i64>().ok()) {
                    fee = Some(n);
                    matched_key = Some(k.to_string());
                    break;
                }
            }
        }

        let neg_risk = val["neg_risk"].as_bool()
            .or_else(|| val["negRisk"].as_bool())
            .unwrap_or(false);

        match (fee, matched_key) {
            (Some(f), Some(k)) => {
                tracing::debug!(
                    "[POLYMARKET] meta {} → (neg_risk={}, fee={} via key '{}')",
                    condition_id, neg_risk, f, k
                );
                Ok((neg_risk, f))
            }
            _ => {
                // No fee field matched. Bail — signing with 0 against a non-zero
                // market fee produces the 400 "invalid fee rate" error we've been
                // chasing. Better to fail the single order than spam bad signatures.
                tracing::warn!(
                    "[POLYMARKET] fetch_market_meta {}: no known taker-fee key in response \
                     (checked feeSchedule.{{taker_base_fee,takerBaseFee,taker_fee,takerFee}} \
                     and flat taker_base_fee/takerBaseFee/taker_fee_rate_bps/takerFeeRateBps/\
                     fee_rate_bps/feeRateBps/taker_fee/takerFee); raw={}",
                    condition_id, val
                );
                anyhow::bail!(
                    "fetch_market_meta {}: no taker-fee key found (see logged response)",
                    condition_id
                )
            }
        }
    }

    #[allow(dead_code)]
    pub fn wallet_address(&self) -> &str {
        &self.wallet_address_str
    }

    #[allow(dead_code)]
    pub fn funder(&self) -> &str {
        &self.funder
    }

    #[allow(dead_code)]
    pub fn wallet(&self) -> &LocalWallet {
        &self.wallet
    }
}

/// Shared async client wrapper for use in execution engine
pub struct SharedAsyncClient {
    inner: Arc<PolymarketAsyncClient>,
    creds: PreparedCreds,
    chain_id: u64,
    /// Per-token cache of (neg_risk, taker_fee_bps), keyed by token_id.
    /// Populated lazily on first order; subsequent orders hit cache in O(1).
    meta_cache: std::sync::RwLock<HashMap<String, (bool, i64)>>,
}

impl SharedAsyncClient {
    pub fn new(client: PolymarketAsyncClient, creds: PreparedCreds, chain_id: u64) -> Self {
        Self {
            inner: Arc::new(client),
            creds,
            chain_id,
            meta_cache: std::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Load meta cache from JSON file. Accepts either:
    ///   - legacy format: {token_id: bool} (treats fee as 0)
    ///   - new format:    {token_id: [neg_risk, fee_bps]}
    pub fn load_cache(&self, path: &str) -> Result<usize> {
        let data = std::fs::read_to_string(path)?;
        // Try new format first
        let map: HashMap<String, (bool, i64)> =
            if let Ok(m) = serde_json::from_str::<HashMap<String, (bool, i64)>>(&data) {
                m
            } else {
                let legacy: HashMap<String, bool> = serde_json::from_str(&data)?;
                legacy.into_iter().map(|(k, v)| (k, (v, 0i64))).collect()
            };
        let count = map.len();
        let mut cache = self.meta_cache.write().unwrap();
        *cache = map;
        Ok(count)
    }

    /// Fetch USDC collateral balance (6-decimal micros) from the CLOB.
    pub async fn fetch_poly_balance_usdc_micros(&self) -> Result<u64> {
        self.inner.fetch_balance_allowance_usdc_micros(&self.creds).await
    }

    /// Execute FAK buy order. `condition_id` is required to look up the market's
    /// taker fee for signing — the CLOB rejects orders whose signed `feeRateBps`
    /// does not match the market's current fee.
    pub async fn buy_fak(&self, token_id: &str, condition_id: &str, price: f64, size: f64) -> Result<PolyFillAsync> {
        debug_assert!(!token_id.is_empty(), "token_id must not be empty");
        debug_assert!(!condition_id.is_empty(), "condition_id must not be empty");
        debug_assert!(price > 0.0 && price < 1.0, "price must be 0 < p < 1");
        debug_assert!(size >= 1.0, "size must be >= 1");
        self.execute_order(token_id, condition_id, price, size, "BUY").await
    }

    /// Execute FAK sell order.
    pub async fn sell_fak(&self, token_id: &str, condition_id: &str, price: f64, size: f64) -> Result<PolyFillAsync> {
        debug_assert!(!token_id.is_empty(), "token_id must not be empty");
        debug_assert!(!condition_id.is_empty(), "condition_id must not be empty");
        debug_assert!(price > 0.0 && price < 1.0, "price must be 0 < p < 1");
        debug_assert!(size >= 1.0, "size must be >= 1");
        self.execute_order(token_id, condition_id, price, size, "SELL").await
    }

    /// Look up (neg_risk, fee_bps) for a token, fetching and caching on miss.
    pub async fn get_market_meta(&self, token_id: &str, condition_id: &str) -> Result<(bool, i64)> {
        if let Some(meta) = self.meta_cache.read().unwrap().get(token_id).copied() {
            return Ok(meta);
        }
        let meta = self.inner.fetch_market_meta(condition_id).await?;
        self.meta_cache.write().unwrap().insert(token_id.to_string(), meta);
        Ok(meta)
    }

    async fn execute_order(&self, token_id: &str, condition_id: &str, price: f64, size: f64, side: &str) -> Result<PolyFillAsync> {
        // V2: only `neg_risk` is consulted at signing time (chooses the
        // verifyingContract). Fees are operator-set at match time and don't
        // flow into the signed order. `fee_bps` from `get_market_meta` is
        // still useful elsewhere for arb-threshold math, but not here.
        let (neg_risk, _fee_bps_unused_in_v2) = self.get_market_meta(token_id, condition_id).await?;

        let signed = self.build_signed_order(token_id, price, size, side, neg_risk)?;
        // Owner must be the API key (not wallet address or funder!)
        let body = signed.post_body(&self.creds.api_key, PolyOrderType::FAK.as_str());

        // Post order
        let resp = self.inner.post_order_async(body, &self.creds).await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Polymarket order failed {}: {}", status, body));
        }

        let resp_json: serde_json::Value = resp.json().await?;
        let order_id = resp_json["orderID"].as_str().unwrap_or("unknown").to_string();

        // Query fill status. FAK orders that don't match leave no persistent
        // record — the CLOB returns literal `null`, which we interpret as a
        // zero-fill rather than an error.
        let order_info = self.inner.get_order_async(&order_id, &self.creds).await?;
        let (filled_size, order_price) = match order_info.as_ref() {
            Some(info) => (
                info.size_matched.parse::<f64>().unwrap_or(0.0),
                info.price.parse::<f64>().unwrap_or(price),
            ),
            None => (0.0, price),
        };

        match order_info.as_ref() {
            Some(info) => tracing::debug!(
                "[POLY-ASYNC] FAK {} {}: status={}, filled={:.2}/{:.2}, price={:.4}",
                side, order_id, info.status, filled_size, size, order_price
            ),
            None => tracing::debug!(
                "[POLY-ASYNC] FAK {} {}: no fill (order not found — unmatched FAK)",
                side, order_id
            ),
        }

        Ok(PolyFillAsync {
            order_id,
            filled_size,
            fill_cost: filled_size * order_price,
        })
    }

    /// Build a signed CLOB V2 order.
    ///
    /// V2 removes `feeRateBps` from the signed struct — fees are operator-set
    /// at match time. Per-market fees are still tracked elsewhere for arb-math
    /// but no longer flow into the order body.
    fn build_signed_order(
        &self,
        token_id: &str,
        price: f64,
        size: f64,
        side: &str,
        neg_risk: bool,
    ) -> Result<SignedOrder> {
        let price_bps = price_to_bps(price);
        let size_micro = size_to_micro(size);

        if !price_valid(price_bps) {
            return Err(anyhow!("price {} ({}bps) outside allowed range", price, price_bps));
        }

        let (side_code, maker_amt, taker_amt) = if side.eq_ignore_ascii_case("BUY") {
            get_order_amounts_buy(size_micro, price_bps)
        } else if side.eq_ignore_ascii_case("SELL") {
            get_order_amounts_sell(size_micro, price_bps)
        } else {
            return Err(anyhow!("side must be BUY or SELL"));
        };

        let salt = generate_seed();
        let maker_amount_str = maker_amt.to_string();
        let taker_amount_str = taker_amt.to_string();
        let timestamp_ms = current_unix_ts_ms();
        let timestamp_ms_str = timestamp_ms.to_string();

        // EIP-712 sign payload. References to avoid clones in the hot path.
        let data = OrderData {
            maker: &self.inner.funder,
            signer: &self.inner.wallet_address_str,
            token_id,
            maker_amount: &maker_amount_str,
            taker_amount: &taker_amount_str,
            side: side_code,
            signature_type: self.inner.signature_type,
            salt,
            timestamp_ms,
            metadata: ZERO_BYTES32,
            builder: ZERO_BYTES32,
        };
        let exchange = get_exchange_address(self.chain_id, neg_risk)?;
        let typed = order_typed_data(self.chain_id, &exchange, &data)?;
        let digest = typed.encode_eip712()?;

        let sig = self.inner.wallet.sign_hash(H256::from(digest))?;

        // Allocate owned strings once for the final OrderStruct (the JSON
        // post body needs owned data).
        Ok(SignedOrder {
            order: OrderStruct {
                salt,
                maker: self.inner.funder.clone(),
                signer: self.inner.wallet_address_str.clone(),
                taker: ZERO_ADDRESS.to_string(),
                token_id: token_id.to_string(),
                maker_amount: maker_amount_str,
                taker_amount: taker_amount_str,
                expiration: "0".to_string(),
                side: side_code,
                signature_type: self.inner.signature_type,
                timestamp: timestamp_ms_str,
                metadata: ZERO_BYTES32.to_string(),
                builder: ZERO_BYTES32.to_string(),
            },
            signature: format!("0x{}", sig),
        })
    }
}

/// Async fill result
#[derive(Debug, Clone)]
pub struct PolyFillAsync {
    pub order_id: String,
    pub filled_size: f64,
    pub fill_cost: f64,
}

#[cfg(test)]
mod meta_parse_tests {
    //! Unit tests for the fee-key extraction logic in `fetch_market_meta`.
    //!
    //! Reproduces the parsing precedence (`feeSchedule` first, legacy keys
    //! second) against synthetic JSON. If this drifts from the live
    //! `fetch_market_meta` body, a future change will fail to update both
    //! and the next forward-compat survey will catch it.
    use serde_json::json;

    fn parse_fee(val: &serde_json::Value) -> Option<i64> {
        if let Some(fs) = val.get("feeSchedule").or_else(|| val.get("fee_schedule")) {
            for k in ["takerBaseFee", "taker_base_fee", "takerFee", "taker_fee"] {
                if let Some(n) = fs.get(k).and_then(|v| v.as_i64()) { return Some(n); }
                if let Some(n) = fs.get(k).and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<i64>().ok()) { return Some(n); }
            }
        }
        for k in ["taker_base_fee","takerBaseFee","taker_fee_rate_bps","takerFeeRateBps",
                  "fee_rate_bps","feeRateBps","taker_fee","takerFee"]
        {
            if let Some(n) = val.get(k).and_then(|v| v.as_i64()) { return Some(n); }
            if let Some(n) = val.get(k).and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok()) { return Some(n); }
        }
        None
    }

    #[test]
    fn fee_schedule_preferred_over_legacy() {
        let v = json!({ "feeSchedule": { "takerBaseFee": 75 }, "takerBaseFee": 0 });
        assert_eq!(parse_fee(&v), Some(75));
    }

    #[test]
    fn legacy_used_when_no_schedule() {
        // Today's actual surveyed shape: feeSchedule absent, taker_base_fee present.
        let v = json!({ "taker_base_fee": 1000, "feeSchedule": null });
        assert_eq!(parse_fee(&v), Some(1000));
    }

    #[test]
    fn fee_as_string_parses() {
        let v = json!({ "feeSchedule": { "takerBaseFee": "75" } });
        assert_eq!(parse_fee(&v), Some(75));
    }

    #[test]
    fn snake_case_fee_schedule_variant_accepted() {
        let v = json!({ "fee_schedule": { "taker_base_fee": 60 } });
        assert_eq!(parse_fee(&v), Some(60));
    }

    #[test]
    fn missing_fee_returns_none() {
        let v = json!({ "unrelated": 42 });
        assert_eq!(parse_fee(&v), None);
    }

    #[test]
    fn zero_fee_legacy_recognized() {
        // Politics markets in the 2026-04-21 survey returned taker_base_fee: 0.
        // Zero is a valid fee value, not "missing".
        let v = json!({ "taker_base_fee": 0 });
        assert_eq!(parse_fee(&v), Some(0));
    }
}

#[cfg(test)]
mod v2_sign_payload_tests {
    //! Tests pin the V2 EIP-712 sign payload shape and the wire body shape so
    //! a future regression to V1 fields fails loudly. Live live signing is
    //! exercised in PR5-T5 smoke; these are pure unit checks.

    use super::*;

    #[test]
    fn v2_exchange_address_polygon_standard() {
        // Live since 2026-04-28 — see docs.polymarket.com/resources/contracts.
        let addr = get_exchange_address(137, false).unwrap();
        assert_eq!(addr, "0xE111180000d2663C0091e4f400237545B87B996B");
    }

    #[test]
    fn v2_exchange_address_polygon_neg_risk() {
        let addr = get_exchange_address(137, true).unwrap();
        assert_eq!(addr, "0xe2222d279d744050d28e00520010520000310F59");
    }

    #[test]
    fn v2_typed_data_uses_domain_version_2() {
        let data = OrderData {
            maker: "0x0000000000000000000000000000000000000001",
            signer: "0x0000000000000000000000000000000000000002",
            token_id: "100",
            maker_amount: "1000000",
            taker_amount: "2000000",
            side: 0,
            signature_type: 2,
            salt: 12345,
            timestamp_ms: 1714000000_000,
            metadata: ZERO_BYTES32,
            builder: ZERO_BYTES32,
        };
        let exchange = get_exchange_address(137, false).unwrap();
        let typed = order_typed_data(137, &exchange, &data).unwrap();
        // Round-trip via the underlying serde value to reach the domain.
        let serialized = serde_json::to_value(&typed).unwrap();
        assert_eq!(serialized["domain"]["version"], "2",
                   "V2 Exchange domain version must be \"2\" — V1 (\"1\") will be rejected");
        // ethers-rs normalizes addresses to lowercase during EIP-712 encoding;
        // they're equivalent to the EIP-55 mixed-case form for hashing purposes.
        assert_eq!(
            serialized["domain"]["verifyingContract"].as_str().unwrap().to_lowercase(),
            "0xe111180000d2663c0091e4f400237545b87b996b"
        );
        let order_fields: Vec<String> = serialized["types"]["Order"].as_array().unwrap()
            .iter().map(|f| f["name"].as_str().unwrap().to_string()).collect();
        // V2: timestamp/metadata/builder added; nonce/feeRateBps/taker/expiration gone.
        assert!(order_fields.contains(&"timestamp".to_string()));
        assert!(order_fields.contains(&"metadata".to_string()));
        assert!(order_fields.contains(&"builder".to_string()));
        assert!(!order_fields.contains(&"nonce".to_string()),
                "V2 Order struct must not carry `nonce`");
        assert!(!order_fields.contains(&"feeRateBps".to_string()),
                "V2 Order struct must not carry `feeRateBps`");
        assert!(!order_fields.contains(&"taker".to_string()),
                "V2 Order struct must not carry `taker`");
        assert!(!order_fields.contains(&"expiration".to_string()),
                "V2 EIP-712 sign struct drops `expiration` (it stays in the body only)");
    }

    #[test]
    fn v2_post_body_includes_v2_fields_and_omits_v1_fields() {
        let signed = SignedOrder {
            order: OrderStruct {
                salt: 12345,
                maker: "0xMAKER".into(),
                signer: "0xSIGNER".into(),
                taker: ZERO_ADDRESS.to_string(),
                token_id: "100".into(),
                maker_amount: "1000000".into(),
                taker_amount: "2000000".into(),
                expiration: "0".into(),
                side: 0,
                signature_type: 2,
                timestamp: "1714000000000".into(),
                metadata: ZERO_BYTES32.into(),
                builder: ZERO_BYTES32.into(),
            },
            signature: "0xDEAD".into(),
        };
        let body = signed.post_body("api-key", "FAK");
        // V2 wire fields present:
        assert!(body.contains(r#""timestamp":"1714000000000""#));
        assert!(body.contains(r#""metadata":"0x000"#));
        assert!(body.contains(r#""builder":"0x000"#));
        assert!(body.contains(r#""side":"BUY""#));
        // V1 fields absent:
        assert!(!body.contains("\"nonce\""),
                "V2 wire body must not include `nonce`");
        assert!(!body.contains("feeRateBps"),
                "V2 wire body must not include `feeRateBps`");
        // Owner + orderType envelope unchanged:
        assert!(body.contains(r#""owner":"api-key""#));
        assert!(body.contains(r#""orderType":"FAK""#));
    }

    #[test]
    fn v2_amoy_chain_returns_helpful_error() {
        let err = get_exchange_address(80002, false).unwrap_err().to_string();
        assert!(err.contains("Amoy"), "Amoy error should name the chain explicitly");
    }
}
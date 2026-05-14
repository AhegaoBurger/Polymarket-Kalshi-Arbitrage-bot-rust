//! Polymarket CLOB client — hand-rolled for gengar, V2 schema.
//!
//! Reference: arb's working V2 implementation at
//! `crates/pmbots-arb/src/polymarket_clob.rs` (commit 7c890dd and follow-ups).
//! Per spec §Out of Scope, gengar duplicates rather than depends on arb.
//!
//! Key V2 differences from the V1 spec we originally implemented:
//!   - EIP-712 `domain.version = "2"`, new exchange verifyingContract,
//!     `Order` struct drops `taker`/`expiration`/`nonce`/`feeRateBps` and adds
//!     `timestamp` (ms), `metadata` (bytes32), `builder` (bytes32).
//!   - API credentials are derived server-side via `POST /auth/api-key` or
//!     `GET /auth/derive-api-key` with L1 EIP-712 headers — NOT locally hashed.
//!   - L2 HMAC uses the URL_SAFE-base64-decoded `secret` as the key, not the
//!     raw string bytes.
//!   - `/balance-allowance` requires `signature_type` query param so Safe
//!     users get their proxy balance, not a spurious $0 from the EOA address.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE;
use base64::Engine;
use ethers::signers::{LocalWallet, Signer};
use ethers::types::transaction::eip712::{Eip712, TypedData};
use ethers::types::{Address, H256, U256};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::polymarket::types::{PriceCents, Side};

pub const CLOB_BASE: &str = "https://clob.polymarket.com";

/// Cloudflare WAF (post 2026-04-28 V2 cutover) requires a full browser bundle
/// of headers, not just a Chrome UA. Mirrors py-clob-client-v2 PR #42 — the
/// only confirmed-working set against the post-cutover WAF.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) \
                          AppleWebKit/537.36 (KHTML, like Gecko) \
                          Chrome/126.0.0.0 Safari/537.36";
const MSG_TO_SIGN: &str = "This message attests that I control the given wallet";
const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";
const ZERO_BYTES32: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000000";

type HmacSha256 = Hmac<Sha256>;

// ============================================================================
// HEADERS
// ============================================================================

fn browser_bundle_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("User-Agent", HeaderValue::from_static(USER_AGENT));
    h.insert(
        "Accept",
        HeaderValue::from_static("application/json, text/plain, */*"),
    );
    h.insert("Accept-Language", HeaderValue::from_static("en-US,en;q=0.9"));
    h.insert("Connection", HeaderValue::from_static("keep-alive"));
    h.insert("Content-Type", HeaderValue::from_static("application/json"));
    h.insert(
        "sec-ch-ua",
        HeaderValue::from_static(
            r#""Chromium";v="124", "Google Chrome";v="124", "Not-A.Brand";v="99""#,
        ),
    );
    h.insert("sec-ch-ua-mobile", HeaderValue::from_static("?0"));
    h.insert("sec-ch-ua-platform", HeaderValue::from_static(r#""macOS""#));
    h.insert("sec-fetch-dest", HeaderValue::from_static("empty"));
    h.insert("sec-fetch-mode", HeaderValue::from_static("cors"));
    h.insert("sec-fetch-site", HeaderValue::from_static("same-site"));
    h
}

// ============================================================================
// TIME / NONCE
// ============================================================================

fn current_unix_ts() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn current_unix_ts_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// L1-auth single-use nonce. V2 rejects reused `(signer, nonce)` pairs.
fn fresh_nonce() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64
}

fn generate_salt() -> u128 {
    (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        % u128::from(u32::MAX)) as u128
}

// ============================================================================
// API CREDENTIALS
// ============================================================================

/// API credentials returned by the Polymarket server (NOT locally derived).
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
// SIGNATURE TYPE
// ============================================================================

/// Polymarket CLOB signature types.
///   0 = EOA               (funder == signer)
///   1 = POLY_PROXY        (Magic / email login proxy)
///   2 = POLY_GNOSIS_SAFE  (MetaMask / external-wallet Safe)
#[derive(Debug, Clone, Copy)]
pub enum SignatureType {
    Eoa = 0,
    PolyProxy = 1,
    Safe = 2,
}

// ============================================================================
// ORDER TYPES
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderType {
    Gtc,
    Fok,
    Gtd,
    Fak,
}

impl OrderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OrderType::Gtc => "GTC",
            OrderType::Fok => "FOK",
            OrderType::Gtd => "GTD",
            OrderType::Fak => "FAK",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OrderArgs {
    pub token_id: String,
    pub price: PriceCents,
    pub size_shares: i64,
    pub side: Side,
    /// EOA address that signs the EIP-712 payload.
    pub maker_address: Address,
    /// Optional Safe proxy address. If set, becomes the `maker`; signer stays
    /// `maker_address`. If None, `maker == signer` (EOA flow).
    pub funder: Option<Address>,
}

/// V2 wire-body order struct. `taker`/`expiration` stay in the body but
/// `expiration` is NOT in the EIP-712 sign payload. `timestamp` (ms),
/// `metadata`, `builder` are new in V2.
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
    pub expiration: String,
    pub side: i32,
    #[serde(rename = "signatureType")]
    pub signature_type: i32,
    pub timestamp: String,
    pub metadata: String,
    pub builder: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignedOrder {
    pub order: OrderStruct,
    pub signature: String,
}

impl SignedOrder {
    /// Render the POST /order body. Side is serialized as the string "BUY" or
    /// "SELL" in the wire body (V2 spec) even though it's `uint8` in EIP-712.
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

#[derive(Debug, Deserialize)]
pub struct OrderResponse {
    #[serde(default)]
    pub success: bool,
    #[serde(rename = "orderID", default)]
    pub order_id: Option<String>,
    #[serde(rename = "errorMsg", default)]
    pub error_msg: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrderStatus {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub size_matched: Option<String>,
}

// ============================================================================
// EIP-712
// ============================================================================

/// Polymarket Polygon V2 Exchange `verifyingContract`s.
/// Live since 2026-04-28. See docs.polymarket.com/resources/contracts.
fn get_exchange_address(chain_id: u64, neg_risk: bool) -> Result<&'static str> {
    match (chain_id, neg_risk) {
        (137, false) => Ok("0xE111180000d2663C0091e4f400237545B87B996B"),
        (137, true) => Ok("0xe2222d279d744050d28e00520010520000310F59"),
        _ => Err(anyhow!(
            "unsupported chain {} (V2 only supports Polygon mainnet)",
            chain_id
        )),
    }
}

fn clob_auth_digest(
    chain_id: u64,
    address_str: &str,
    timestamp: u64,
    nonce: u64,
) -> Result<H256> {
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
        "message": {
            "address": address_str,
            "timestamp": timestamp.to_string(),
            "nonce": nonce,
            "message": MSG_TO_SIGN
        }
    });
    let typed: TypedData = serde_json::from_value(typed_json)?;
    Ok(typed.encode_eip712()?.into())
}

struct OrderTypedDataInput<'a> {
    maker: &'a str,
    signer: &'a str,
    token_id: &'a str,
    maker_amount: &'a str,
    taker_amount: &'a str,
    side: i32,
    signature_type: i32,
    salt: u128,
    timestamp_ms: u64,
}

fn order_typed_data(
    chain_id: u64,
    exchange: &str,
    data: &OrderTypedDataInput<'_>,
) -> Result<TypedData> {
    let typed_json = json!({
        "types": {
            "EIP712Domain": [
                {"name": "name", "type": "string"},
                {"name": "version", "type": "string"},
                {"name": "chainId", "type": "uint256"},
                {"name": "verifyingContract", "type": "address"}
            ],
            "Order": [
                {"name": "salt", "type": "uint256"},
                {"name": "maker", "type": "address"},
                {"name": "signer", "type": "address"},
                {"name": "tokenId", "type": "uint256"},
                {"name": "makerAmount", "type": "uint256"},
                {"name": "takerAmount", "type": "uint256"},
                {"name": "side", "type": "uint8"},
                {"name": "signatureType", "type": "uint8"},
                {"name": "timestamp", "type": "uint256"},
                {"name": "metadata", "type": "bytes32"},
                {"name": "builder", "type": "bytes32"}
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
            "metadata": ZERO_BYTES32,
            "builder": ZERO_BYTES32
        }
    });
    Ok(serde_json::from_value(typed_json)?)
}

// ============================================================================
// CLIENT
// ============================================================================

#[derive(Debug, Clone)]
pub struct ClobClient {
    http: reqwest::Client,
}

impl ClobClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .default_headers(browser_bundle_headers())
            .pool_max_idle_per_host(10)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .tcp_nodelay(true)
            .timeout(Duration::from_secs(20))
            .build()
            .context("build CLOB http client")?;
        Ok(Self { http })
    }

    /// `GET /ok` — unauthenticated health check.
    pub async fn get_ok(&self) -> Result<()> {
        let resp = self
            .http
            .get(format!("{}/ok", CLOB_BASE))
            .send()
            .await
            .context("GET /ok")?;
        if !resp.status().is_success() {
            anyhow::bail!("CLOB /ok returned {}", resp.status());
        }
        Ok(())
    }

    // -- L1 auth (for /auth/api-key, /auth/derive-api-key) --

    fn build_l1_headers(
        wallet: &LocalWallet,
        chain_id: u64,
        nonce: u64,
    ) -> Result<HeaderMap> {
        let address_str = format!("{:?}", wallet.address());
        let timestamp = current_unix_ts();
        let digest = clob_auth_digest(chain_id, &address_str, timestamp, nonce)?;
        let sig = wallet.sign_hash(digest)?;
        let mut h = HeaderMap::new();
        h.insert("POLY_ADDRESS", HeaderValue::from_str(&address_str)?);
        h.insert(
            "POLY_SIGNATURE",
            HeaderValue::from_str(&format!("0x{}", sig))?,
        );
        h.insert(
            "POLY_TIMESTAMP",
            HeaderValue::from_str(&timestamp.to_string())?,
        );
        h.insert("POLY_NONCE", HeaderValue::from_str(&nonce.to_string())?);
        Ok(h)
    }

    /// Try `POST /auth/api-key` first (V2-friendly: always creates fresh creds),
    /// fall back to `GET /auth/derive-api-key` (V1-idempotent). Each call uses
    /// a fresh nanosecond nonce — V2 rejects reused (signer, nonce) pairs.
    pub async fn get_or_derive_api_creds(
        &self,
        wallet: &LocalWallet,
        chain_id: u64,
    ) -> Result<ApiCreds> {
        let nonce1 = fresh_nonce();
        let headers1 = Self::build_l1_headers(wallet, chain_id, nonce1)?;
        let resp1 = self
            .http
            .post(format!("{}/auth/api-key", CLOB_BASE))
            .headers(headers1)
            .send()
            .await
            .context("POST /auth/api-key")?;
        if resp1.status().is_success() {
            let creds: ApiCreds = resp1.json().await.context("parse create_api_key json")?;
            tracing::info!(
                "[GENGAR][CLOB] created new API creds via POST /auth/api-key (nonce={})",
                nonce1
            );
            return Ok(creds);
        }
        let status1 = resp1.status();
        let body1 = resp1.text().await.unwrap_or_default();
        tracing::warn!(
            "[GENGAR][CLOB] POST /auth/api-key {} → falling back to derive-api-key. body={}",
            status1,
            body1
        );

        let nonce2 = fresh_nonce();
        let headers2 = Self::build_l1_headers(wallet, chain_id, nonce2)?;
        let resp2 = self
            .http
            .get(format!("{}/auth/derive-api-key", CLOB_BASE))
            .headers(headers2)
            .send()
            .await
            .context("GET /auth/derive-api-key")?;
        if !resp2.status().is_success() {
            let status = resp2.status();
            let body = resp2.text().await.unwrap_or_default();
            anyhow::bail!("derive-api-key failed: {} {}", status, body);
        }
        let creds: ApiCreds = resp2.json().await.context("parse derive_api_key json")?;
        tracing::info!(
            "[GENGAR][CLOB] derived existing API creds via GET /auth/derive-api-key (nonce={})",
            nonce2
        );
        Ok(creds)
    }

    // -- L2 auth (for /order, /balance-allowance, /data/order/{id}) --

    /// L2 HMAC of `timestamp + method + path + body`. The `secret` MUST be
    /// URL_SAFE-base64-decoded before being used as the HMAC key — the raw
    /// string IS NOT the key bytes.
    pub fn build_l2_headers(
        creds: &ApiCreds,
        eoa_addr: Address,
        method: &str,
        path: &str,
        body: &str,
    ) -> Result<HeaderMap> {
        let timestamp = current_unix_ts();
        let message = format!("{}{}{}{}", timestamp, method, path, body);

        let secret_bytes = URL_SAFE
            .decode(&creds.api_secret)
            .context("decode api_secret as URL_SAFE base64")?;
        let mut mac = HmacSha256::new_from_slice(&secret_bytes)
            .map_err(|e| anyhow!("invalid HMAC key length: {}", e))?;
        mac.update(message.as_bytes());
        let sig_b64 = URL_SAFE.encode(mac.finalize().into_bytes());

        let mut h = HeaderMap::new();
        h.insert(
            "POLY_ADDRESS",
            HeaderValue::from_str(&format!("{:?}", eoa_addr))?,
        );
        h.insert("POLY_SIGNATURE", HeaderValue::from_str(&sig_b64)?);
        h.insert(
            "POLY_TIMESTAMP",
            HeaderValue::from_str(&timestamp.to_string())?,
        );
        h.insert("POLY_API_KEY", HeaderValue::from_str(&creds.api_key)?);
        h.insert(
            "POLY_PASSPHRASE",
            HeaderValue::from_str(&creds.api_passphrase)?,
        );
        Ok(h)
    }

    /// GET /balance-allowance (USDC, 6 decimals, returned as a string).
    ///
    /// The `signature_type` query param is CRITICAL: without it, Safe-proxy
    /// users get a spurious $0 because the CLOB resolves the balance against
    /// the signer EOA instead of the proxy. Sign the bare path, append query
    /// to the URL only.
    pub async fn get_balance_allowance(
        &self,
        creds: &ApiCreds,
        eoa_addr: Address,
        sig_type: SignatureType,
    ) -> Result<f64> {
        let sign_path = "/balance-allowance";
        let url = format!(
            "{}{}?asset_type=COLLATERAL&signature_type={}",
            CLOB_BASE, sign_path, sig_type as i32
        );
        let headers = Self::build_l2_headers(creds, eoa_addr, "GET", sign_path, "")?;

        let resp = self
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .context("GET /balance-allowance")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET /balance-allowance → {} {}", status, body);
        }
        let v: serde_json::Value = resp.json().await?;
        let raw = v
            .get("balance")
            .and_then(|b| b.as_str())
            .ok_or_else(|| anyhow!("missing balance field in response: {}", v))?;
        let micro: u64 = raw.parse().context("parse balance micros")?;
        Ok(micro as f64 / 1_000_000.0)
    }

    /// Public unauthenticated endpoint — server-computed expected fill price
    /// for a market order of `amount_usd` notional on the given side.
    pub async fn calculate_market_price(
        &self,
        token_id: &str,
        side: Side,
        amount_usd: f64,
    ) -> Result<f64> {
        let url = format!(
            "{}/price?token_id={}&side={}&amount={}",
            CLOB_BASE,
            token_id,
            match side {
                Side::Buy => "BUY",
                Side::Sell => "SELL",
            },
            amount_usd
        );
        let resp = self.http.get(&url).send().await.context("GET /price")?;
        if !resp.status().is_success() {
            anyhow::bail!("GET /price → {}", resp.status());
        }
        let v: serde_json::Value = resp.json().await?;
        v.get("price")
            .and_then(|p| p.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| anyhow!("price field missing/invalid in response: {}", v))
    }

    /// Build + sign a V2 EIP-712 order. The `neg_risk` flag selects the right
    /// `verifyingContract`. BTC Up/Down 5-min markets are NOT neg-risk; the
    /// gengar caller should pass `false`.
    pub async fn create_order(
        &self,
        args: OrderArgs,
        sig_type: SignatureType,
        chain_id: u64,
        wallet: &LocalWallet,
        neg_risk: bool,
    ) -> Result<SignedOrder> {
        let salt = generate_salt();
        let timestamp_ms = current_unix_ts_ms();
        let maker = args.funder.unwrap_or(args.maker_address);

        // Integer-cents arithmetic. Reference: arb's get_order_amounts_buy/sell
        // (polymarket_clob.rs:338-356). price_bps = cents * 100; size_micro =
        // shares * 1_000_000; (size_micro * price_bps) / 10000 = USDC micros.
        let size_micro = (args.size_shares as u64) * 1_000_000;
        let price_bps = (args.price.0 as u64) * 100;
        let (side_int, maker_amount, taker_amount): (i32, u128, u128) = match args.side {
            Side::Buy => (
                0,
                (size_micro as u128 * price_bps as u128) / 10000,
                size_micro as u128,
            ),
            Side::Sell => (
                1,
                size_micro as u128,
                (size_micro as u128 * price_bps as u128) / 10000,
            ),
        };

        let exchange = get_exchange_address(chain_id, neg_risk)?;
        let maker_str = format!("{:?}", maker);
        let signer_str = format!("{:?}", args.maker_address);
        let maker_amount_str = maker_amount.to_string();
        let taker_amount_str = taker_amount.to_string();

        let typed = order_typed_data(
            chain_id,
            exchange,
            &OrderTypedDataInput {
                maker: &maker_str,
                signer: &signer_str,
                token_id: &args.token_id,
                maker_amount: &maker_amount_str,
                taker_amount: &taker_amount_str,
                side: side_int,
                signature_type: sig_type as i32,
                salt,
                timestamp_ms,
            },
        )?;
        let digest: H256 = typed
            .encode_eip712()
            .map_err(|e| anyhow!("eip712 encode: {:?}", e))?
            .into();
        let sig = wallet.sign_hash(digest).context("sign order")?;

        Ok(SignedOrder {
            order: OrderStruct {
                salt,
                maker: maker_str,
                signer: signer_str,
                taker: ZERO_ADDRESS.to_string(),
                token_id: args.token_id,
                maker_amount: maker_amount_str,
                taker_amount: taker_amount_str,
                expiration: "0".into(),
                side: side_int,
                signature_type: sig_type as i32,
                timestamp: timestamp_ms.to_string(),
                metadata: ZERO_BYTES32.into(),
                builder: ZERO_BYTES32.into(),
            },
            signature: format!("0x{}", hex::encode(sig.to_vec())),
        })
    }

    pub async fn post_order(
        &self,
        creds: &ApiCreds,
        eoa_addr: Address,
        signed: &SignedOrder,
        otype: OrderType,
    ) -> Result<OrderResponse> {
        let path = "/order";
        let body = signed.post_body(&creds.api_key, otype.as_str());
        let headers = Self::build_l2_headers(creds, eoa_addr, "POST", path, &body)?;
        let resp = self
            .http
            .post(format!("{}{}", CLOB_BASE, path))
            .headers(headers)
            .body(body)
            .send()
            .await
            .context("POST /order")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("POST /order {} → {}", status, text);
        }
        serde_json::from_str(&text).with_context(|| format!("parse /order response: {}", text))
    }

    /// GET /data/order/{id}. Returns Ok(None) on JSON null (FAK orders with no
    /// matching liquidity have no persistent row).
    pub async fn get_order(
        &self,
        creds: &ApiCreds,
        eoa_addr: Address,
        order_id: &str,
    ) -> Result<Option<OrderStatus>> {
        let path = format!("/data/order/{}", order_id);
        let headers = Self::build_l2_headers(creds, eoa_addr, "GET", &path, "")?;
        let resp = self
            .http
            .get(format!("{}{}", CLOB_BASE, path))
            .headers(headers)
            .send()
            .await
            .context("GET /data/order/{id}")?;
        if !resp.status().is_success() {
            anyhow::bail!("GET /data/order/{} → {}", order_id, resp.status());
        }
        let val: serde_json::Value = resp.json().await?;
        if val.is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(val)?))
    }

    /// Cancel a single resting order. Used by the sell-path to clean up
    /// `UNVERIFIED_SELL` orders so they don't fill asynchronously after the
    /// strategy has moved on.
    pub async fn cancel_order(
        &self,
        creds: &ApiCreds,
        eoa_addr: Address,
        order_id: &str,
    ) -> Result<()> {
        let path = "/order";
        let body = format!(r#"{{"orderID":"{}"}}"#, order_id);
        let headers = Self::build_l2_headers(creds, eoa_addr, "DELETE", path, &body)?;
        let resp = self
            .http
            .delete(format!("{}{}", CLOB_BASE, path))
            .headers(headers)
            .body(body)
            .send()
            .await
            .context("DELETE /order")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("DELETE /order {} → {}", status, text);
        }
        Ok(())
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builds() {
        let _client = ClobClient::new().unwrap();
    }

    #[test]
    fn browser_bundle_headers_have_chrome_ua() {
        let h = browser_bundle_headers();
        let ua = h.get("User-Agent").unwrap().to_str().unwrap();
        assert!(ua.contains("Chrome/"));
        assert!(ua.contains("Safari/"));
        assert!(!ua.to_lowercase().contains("python"));
    }

    #[test]
    fn v2_exchange_addresses() {
        assert_eq!(
            get_exchange_address(137, false).unwrap(),
            "0xE111180000d2663C0091e4f400237545B87B996B"
        );
        assert_eq!(
            get_exchange_address(137, true).unwrap(),
            "0xe2222d279d744050d28e00520010520000310F59"
        );
        assert!(get_exchange_address(1, false).is_err());
    }

    #[test]
    fn order_type_string_serialization() {
        assert_eq!(OrderType::Gtc.as_str(), "GTC");
        assert_eq!(OrderType::Fok.as_str(), "FOK");
        assert_eq!(OrderType::Fak.as_str(), "FAK");
        assert_eq!(OrderType::Gtd.as_str(), "GTD");
    }

    #[test]
    fn calculate_market_price_url_format() {
        let url = format!(
            "{}/price?token_id={}&side={}&amount={}",
            CLOB_BASE, "tok-123", "BUY", 25.0
        );
        assert_eq!(
            url,
            "https://clob.polymarket.com/price?token_id=tok-123&side=BUY&amount=25"
        );
    }

    #[tokio::test]
    async fn buy_order_v2_wire_fields() {
        // Test wallet — DO NOT use for real funds.
        let wallet: LocalWallet =
            "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                .parse()
                .unwrap();
        let client = ClobClient::new().unwrap();
        let args = OrderArgs {
            token_id: "12345".into(),
            price: PriceCents(68),
            size_shares: 10,
            side: Side::Buy,
            maker_address: wallet.address(),
            funder: None,
        };
        let signed = client
            .create_order(args, SignatureType::Eoa, 137, &wallet, false)
            .await
            .unwrap();

        assert_eq!(signed.order.side, 0);
        assert_eq!(signed.order.signature_type, 0);
        assert!(signed.signature.starts_with("0x"));
        assert_eq!(signed.order.token_id, "12345");

        // Integer-cents math: 10 shares × $0.68 = $6.80 = 6_800_000 USDC micros.
        // Taker = size = 10_000_000 (10 shares × 1e6).
        assert_eq!(signed.order.maker_amount, "6800000");
        assert_eq!(signed.order.taker_amount, "10000000");

        // V2 wire fields present.
        assert_eq!(signed.order.taker, ZERO_ADDRESS);
        assert_eq!(signed.order.metadata, ZERO_BYTES32);
        assert_eq!(signed.order.builder, ZERO_BYTES32);
        assert_eq!(signed.order.expiration, "0");
        let ts: u64 = signed.order.timestamp.parse().unwrap();
        assert!(ts > 1_700_000_000_000); // sanity: post-2023 in ms

        // V1 fields absent: the post_body must not contain `nonce` or `feeRateBps`.
        let body = signed.post_body("test-owner-key", "GTC");
        assert!(
            !body.contains("\"nonce\""),
            "V2 body should not contain nonce: {}",
            body
        );
        assert!(
            !body.contains("\"feeRateBps\""),
            "V2 body should not contain feeRateBps: {}",
            body
        );
        // V2 fields present in body with camelCase keys.
        assert!(body.contains("\"tokenId\":\"12345\""));
        assert!(body.contains("\"makerAmount\":\"6800000\""));
        assert!(body.contains("\"takerAmount\":\"10000000\""));
        assert!(body.contains("\"timestamp\":"));
        assert!(body.contains("\"metadata\":"));
        assert!(body.contains("\"builder\":"));
        // Side is the string "BUY" in the wire body, not the EIP-712 uint8.
        assert!(body.contains("\"side\":\"BUY\""));
        assert!(body.contains("\"signatureType\":0"));
    }

    #[test]
    fn l2_auth_decodes_secret_as_base64() {
        // Server returns secret as URL_SAFE base64. The HMAC key must be the
        // raw bytes, NOT the base64 string. Confirm decoding works.
        let raw_key: [u8; 32] = [42u8; 32];
        let encoded = URL_SAFE.encode(raw_key);
        let decoded = URL_SAFE.decode(&encoded).unwrap();
        assert_eq!(decoded, raw_key);

        // Building L2 headers with a base64-encoded secret should succeed.
        let creds = ApiCreds {
            api_key: "k".into(),
            api_secret: encoded,
            api_passphrase: "p".into(),
        };
        let eoa = "0x0000000000000000000000000000000000000001"
            .parse::<Address>()
            .unwrap();
        let h = ClobClient::build_l2_headers(&creds, eoa, "GET", "/balance-allowance", "")
            .unwrap();
        assert!(h.contains_key("POLY_SIGNATURE"));
        assert!(h.contains_key("POLY_API_KEY"));
        assert!(h.contains_key("POLY_PASSPHRASE"));
        assert!(h.contains_key("POLY_TIMESTAMP"));
        assert!(h.contains_key("POLY_ADDRESS"));
    }
}

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

// ============================================================================
// POLY_1271 (sig_type=3) — Solady EIP-1271 wrapped signature constants.
// Ported verbatim from rs-clob-client-v2 src/clob/client.rs:68-84.
// Used only for sig_type=3 (deposit wallet flow). For sig_type 0/1/2 the
// standard EIP-712 path remains untouched.
// ============================================================================
const DEPOSIT_WALLET_NAME: &str = "DepositWallet";
const DEPOSIT_WALLET_VERSION: &str = "1";
const ORDER_TYPE_STRING: &str = concat!(
    "Order(uint256 salt,address maker,address signer,uint256 tokenId,",
    "uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,",
    "uint256 timestamp,bytes32 metadata,bytes32 builder)"
);
const SOLADY_TYPE_STRING: &str = concat!(
    "TypedDataSign(Order contents,string name,string version,uint256 chainId,",
    "address verifyingContract,bytes32 salt)",
    "Order(uint256 salt,address maker,address signer,uint256 tokenId,",
    "uint256 makerAmount,uint256 takerAmount,uint8 side,uint8 signatureType,",
    "uint256 timestamp,bytes32 metadata,bytes32 builder)"
);
const DOMAIN_TYPE_STRING: &str =
    "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
const CTF_EXCHANGE_NAME: &str = "Polymarket CTF Exchange";
const CTF_EXCHANGE_VERSION_V2: &str = "2";

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
///   0 = EOA               (funder == signer, direct trading)
///   1 = POLY_PROXY        (Magic / email login proxy)
///   2 = POLY_GNOSIS_SAFE  (MetaMask / browser-wallet Gnosis Safe, V2 legacy)
///   3 = POLY_1271         (Polymarket-managed deposit wallet, Solady wrapped
///                          EIP-1271 signing. V2-only, **required** by V2
///                          production for accounts created via polymarket.com
///                          UI per "deposit wallet flow" error.)
#[derive(Debug, Clone, Copy)]
pub enum SignatureType {
    Eoa = 0,
    PolyProxy = 1,
    Safe = 2,
    Poly1271 = 3,
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
// POLY_1271 HASH HELPERS
//
// Ports from rs-clob-client-v2's `sign_poly1271_order` (src/clob/client.rs:1794).
// Standard EIP-712 with a Solady `TypedDataSign` wrapper. The outer digest is
// signed by the EOA's private key; the wallet contract verifies it on-chain
// via EIP-1271's `isValidSignature`.
//
// Wire format of the final signature in the order body:
//   "0x" || inner_sig_hex(130) || domain_sep_hex(64) || contents_hash_hex(64)
//        || ORDER_TYPE_STRING_hex(2*N) || contents_type_len_u16_be_hex(4)
// ============================================================================

fn keccak256(bytes: &[u8]) -> [u8; 32] {
    use tiny_keccak::{Hasher, Keccak};
    let mut k = Keccak::v256();
    let mut out = [0u8; 32];
    k.update(bytes);
    k.finalize(&mut out);
    out
}

fn pad32_address(addr: Address) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(addr.as_bytes());
    out
}

fn pad32_u8(n: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[31] = n;
    out
}

fn pad32_u64(n: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&n.to_be_bytes());
    out
}

fn pad32_u128(n: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&n.to_be_bytes());
    out
}

fn pad32_u256(n: U256) -> [u8; 32] {
    let mut out = [0u8; 32];
    n.to_big_endian(&mut out);
    out
}

/// `hashStruct(Order)` per EIP-712. Order of fields matches `ORDER_TYPE_STRING`.
#[allow(clippy::too_many_arguments)]
fn hash_struct_order(
    salt: u128,
    maker: Address,
    signer_addr: Address,
    token_id_u256: U256,
    maker_amount: u128,
    taker_amount: u128,
    side: u8,
    signature_type: u8,
    timestamp_ms: u64,
) -> [u8; 32] {
    let type_hash = keccak256(ORDER_TYPE_STRING.as_bytes());
    let zero32 = [0u8; 32]; // metadata + builder = zero

    let mut buf = Vec::with_capacity(32 * 12);
    buf.extend_from_slice(&type_hash);
    buf.extend_from_slice(&pad32_u128(salt));
    buf.extend_from_slice(&pad32_address(maker));
    buf.extend_from_slice(&pad32_address(signer_addr));
    buf.extend_from_slice(&pad32_u256(token_id_u256));
    buf.extend_from_slice(&pad32_u128(maker_amount));
    buf.extend_from_slice(&pad32_u128(taker_amount));
    buf.extend_from_slice(&pad32_u8(side));
    buf.extend_from_slice(&pad32_u8(signature_type));
    buf.extend_from_slice(&pad32_u64(timestamp_ms));
    buf.extend_from_slice(&zero32); // metadata
    buf.extend_from_slice(&zero32); // builder
    keccak256(&buf)
}

/// `hashStruct(EIP712Domain)` for the CTF Exchange V2 domain.
fn hash_struct_ctf_domain_v2(chain_id: u64, verifying_contract: Address) -> [u8; 32] {
    let type_hash = keccak256(DOMAIN_TYPE_STRING.as_bytes());
    let name_hash = keccak256(CTF_EXCHANGE_NAME.as_bytes());
    let version_hash = keccak256(CTF_EXCHANGE_VERSION_V2.as_bytes());

    let mut buf = Vec::with_capacity(32 * 5);
    buf.extend_from_slice(&type_hash);
    buf.extend_from_slice(&name_hash);
    buf.extend_from_slice(&version_hash);
    buf.extend_from_slice(&pad32_u64(chain_id));
    buf.extend_from_slice(&pad32_address(verifying_contract));
    keccak256(&buf)
}

/// `hashStruct(TypedDataSign)` per Solady. Order of fields matches
/// `SOLADY_TYPE_STRING`. The `signer` here is the EIP-712 Order's `signer`
/// field — for Poly1271 that's the funder (deposit wallet), NOT the EOA.
fn hash_struct_typed_data_sign(
    contents_hash: &[u8; 32],
    chain_id: u64,
    order_signer: Address,
) -> [u8; 32] {
    let type_hash = keccak256(SOLADY_TYPE_STRING.as_bytes());
    let dw_name_hash = keccak256(DEPOSIT_WALLET_NAME.as_bytes());
    let dw_version_hash = keccak256(DEPOSIT_WALLET_VERSION.as_bytes());
    let zero32 = [0u8; 32]; // salt = 0

    let mut buf = Vec::with_capacity(32 * 7);
    buf.extend_from_slice(&type_hash);
    buf.extend_from_slice(contents_hash);
    buf.extend_from_slice(&dw_name_hash);
    buf.extend_from_slice(&dw_version_hash);
    buf.extend_from_slice(&pad32_u64(chain_id));
    buf.extend_from_slice(&pad32_address(order_signer));
    buf.extend_from_slice(&zero32);
    keccak256(&buf)
}

/// Compute the outer EIP-712 digest `keccak256(0x1901 || domain_sep || sign_struct_hash)`.
fn compute_poly1271_digest(domain_sep: &[u8; 32], sign_struct_hash: &[u8; 32]) -> [u8; 32] {
    let mut input = [0u8; 66];
    input[0] = 0x19;
    input[1] = 0x01;
    input[2..34].copy_from_slice(domain_sep);
    input[34..66].copy_from_slice(sign_struct_hash);
    keccak256(&input)
}

/// Build the Solady wrapped signature for the wire body. Format:
///   "0x" || inner_sig(65 bytes hex) || app_domain_separator(32 bytes hex)
///        || contents_hash(32 bytes hex) || ORDER_TYPE_STRING(bytes hex)
///        || contents_type_len(u16 big-endian, 2 bytes hex)
fn build_poly1271_wire_signature(
    inner_sig_bytes: &[u8],
    domain_sep: &[u8; 32],
    contents_hash: &[u8; 32],
) -> String {
    let order_type_hex = hex::encode(ORDER_TYPE_STRING.as_bytes());
    let mut wrapped = String::with_capacity(2 + 130 + 64 + 64 + order_type_hex.len() + 4);
    wrapped.push_str("0x");
    wrapped.push_str(&hex::encode(inner_sig_bytes));
    wrapped.push_str(&hex::encode(domain_sep));
    wrapped.push_str(&hex::encode(contents_hash));
    wrapped.push_str(&order_type_hex);
    let len = u16::try_from(ORDER_TYPE_STRING.len())
        .expect("ORDER_TYPE_STRING length fits in u16");
    wrapped.push_str(&hex::encode(len.to_be_bytes()));
    wrapped
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

    /// Build + sign a V2 order. The `neg_risk` flag selects the right
    /// `verifyingContract`. BTC Up/Down 5-min markets are NOT neg-risk; the
    /// gengar caller should pass `false`.
    ///
    /// For `sig_type` 0/1/2 this uses standard EIP-712 signing. For `sig_type=3`
    /// (Poly1271 / deposit wallet flow) it uses Solady's `TypedDataSign`
    /// wrapped scheme — the EOA still produces the inner ECDSA signature, but
    /// the EIP-712 Order's `signer` field is the funder (deposit wallet) and
    /// the wire signature is a Solady wrap that the wallet contract verifies
    /// on-chain via `isValidSignature`.
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

        // Maker/signer mapping per signature type. Matches rs-clob-client-v2
        // src/clob/order_builder.rs:160-169.
        let maker = args.funder.unwrap_or(args.maker_address);
        let order_signer = match sig_type {
            SignatureType::Poly1271 => args.funder.ok_or_else(|| {
                anyhow!("Poly1271 (sig_type=3) requires a funder (deposit wallet) address")
            })?,
            _ => args.maker_address,
        };

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

        let exchange_str = get_exchange_address(chain_id, neg_risk)?;
        let exchange_addr: Address = exchange_str
            .parse()
            .context("parse exchange contract address")?;
        let maker_str = format!("{:?}", maker);
        let signer_str = format!("{:?}", order_signer);
        let maker_amount_str = maker_amount.to_string();
        let taker_amount_str = taker_amount.to_string();

        let signature_hex = if matches!(sig_type, SignatureType::Poly1271) {
            // Poly1271 wrapped Solady signing. Reference:
            // rs-clob-client-v2 src/clob/client.rs:1794-1837.
            let token_id_u256 = U256::from_dec_str(&args.token_id)
                .context("parse token_id as u256 for Poly1271 hash")?;
            let contents_hash = hash_struct_order(
                salt,
                maker,
                order_signer,
                token_id_u256,
                maker_amount,
                taker_amount,
                side_int as u8,
                sig_type as u8,
                timestamp_ms,
            );
            let domain_sep = hash_struct_ctf_domain_v2(chain_id, exchange_addr);
            let sign_struct_hash =
                hash_struct_typed_data_sign(&contents_hash, chain_id, order_signer);
            let outer_digest = compute_poly1271_digest(&domain_sep, &sign_struct_hash);

            let inner_sig = wallet
                .sign_hash(H256::from(outer_digest))
                .context("sign Poly1271 outer digest with EOA")?;
            build_poly1271_wire_signature(&inner_sig.to_vec(), &domain_sep, &contents_hash)
        } else {
            // Standard EIP-712 path for sig_type 0/1/2.
            let typed = order_typed_data(
                chain_id,
                exchange_str,
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
            format!("0x{}", hex::encode(sig.to_vec()))
        };

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
            signature: signature_hex,
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

    #[tokio::test]
    async fn poly1271_order_uses_funder_as_signer_and_wraps_signature() {
        // Test wallet — DO NOT use for real funds.
        let wallet: LocalWallet =
            "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
                .parse()
                .unwrap();
        let client = ClobClient::new().unwrap();
        let funder: Address = "0x95edf54270e16bb15f59229a65c4c9bfbccc189c"
            .parse()
            .unwrap();
        let args = OrderArgs {
            token_id: "12345".into(),
            price: PriceCents(68),
            size_shares: 10,
            side: Side::Buy,
            maker_address: wallet.address(),
            funder: Some(funder),
        };
        let signed = client
            .create_order(args, SignatureType::Poly1271, 137, &wallet, false)
            .await
            .unwrap();

        // For Poly1271 the order's `signer` field is the FUNDER, not the EOA.
        // Both `maker` and `signer` are the deposit wallet.
        assert_eq!(signed.order.signature_type, 3);
        assert_eq!(signed.order.maker.to_lowercase(), format!("{:?}", funder));
        assert_eq!(signed.order.signer.to_lowercase(), format!("{:?}", funder));

        // Integer-cents math still applies: 10 shares × $0.68 = 6_800_000 USDC micros.
        assert_eq!(signed.order.maker_amount, "6800000");
        assert_eq!(signed.order.taker_amount, "10000000");

        // The wire signature is the Solady wrap, NOT a plain 65-byte ECDSA:
        //   2 (0x) + 130 (inner sig hex) + 64 (domain sep hex) + 64 (contents hash hex)
        //     + 2*ORDER_TYPE_STRING.len() + 4 (u16 length hex) = 264 + 2*N + 4
        // ORDER_TYPE_STRING is the literal Solady V2 Order type, length 287.
        let expected_len = 2 + 130 + 64 + 64 + (ORDER_TYPE_STRING.len() * 2) + 4;
        assert_eq!(
            signed.signature.len(),
            expected_len,
            "wrapped signature wire length should be {} chars (sig 65 || domain 32 || contents 32 || type_string {} || len 2 bytes)",
            expected_len,
            ORDER_TYPE_STRING.len()
        );
        assert!(signed.signature.starts_with("0x"));

        // The trailing 4 hex chars (2 bytes big-endian) must encode the
        // ORDER_TYPE_STRING length so the deposit wallet contract can parse it.
        let len_hex = &signed.signature[signed.signature.len() - 4..];
        let len_bytes = hex::decode(len_hex).unwrap();
        let parsed_len = u16::from_be_bytes([len_bytes[0], len_bytes[1]]) as usize;
        assert_eq!(parsed_len, ORDER_TYPE_STRING.len());
    }

    #[test]
    fn poly1271_requires_funder() {
        // sig_type=3 without funder should error rather than silently using
        // the EOA address (which would always fail server-side).
        // Synthetic check: the precondition is enforced inside create_order;
        // we test by calling synchronously via a runtime.
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
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
            let result = client
                .create_order(args, SignatureType::Poly1271, 137, &wallet, false)
                .await;
            assert!(result.is_err());
            let err_msg = result.unwrap_err().to_string();
            assert!(
                err_msg.contains("Poly1271") && err_msg.contains("funder"),
                "expected Poly1271/funder error, got: {}",
                err_msg
            );
        });
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

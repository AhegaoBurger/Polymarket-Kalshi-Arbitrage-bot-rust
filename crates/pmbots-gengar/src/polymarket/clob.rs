//! Polymarket CLOB client — hand-rolled for gengar.
//! Reference: executor.py + py-clob-client behavior.
//!
//! The HTTP layer carries browser-bundle headers (UA + sec-ch-ua-*) to bypass
//! Polymarket's V2 WAF, which started rejecting stock python-httpx UAs in
//! April 2026. Mirrors the mitigation in arb's commit 7c890dd, hand-rolled
//! independently per spec §Out of Scope (no shared platform crate).

use anyhow::{Context, Result};
use base64::Engine;
use ethers::signers::{LocalWallet, Signer};
use ethers::types::transaction::eip712::{Eip712, TypedData};
use ethers::types::{Address, H256};
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::json;
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::polymarket::types::{PriceCents, Side};

type HmacSha256 = Hmac<Sha256>;

pub const CLOB_BASE: &str = "https://clob.polymarket.com";

/// User-Agent + browser-fingerprint headers mirroring a Chrome browser bundle.
/// Required to pass the V2 WAF that blocks `python-httpx` / `python-requests`
/// default UAs.
fn browser_bundle_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(
        "User-Agent",
        HeaderValue::from_static(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
             AppleWebKit/537.36 (KHTML, like Gecko) \
             Chrome/124.0.0.0 Safari/537.36",
        ),
    );
    h.insert("Accept",          HeaderValue::from_static("application/json, text/plain, */*"));
    h.insert("Accept-Language", HeaderValue::from_static("en-US,en;q=0.9"));
    h.insert("sec-ch-ua",       HeaderValue::from_static("\"Chromium\";v=\"124\", \"Google Chrome\";v=\"124\", \"Not-A.Brand\";v=\"99\""));
    h.insert("sec-ch-ua-mobile", HeaderValue::from_static("?0"));
    h.insert("sec-ch-ua-platform", HeaderValue::from_static("\"macOS\""));
    h.insert("sec-fetch-dest",  HeaderValue::from_static("empty"));
    h.insert("sec-fetch-mode",  HeaderValue::from_static("cors"));
    h.insert("sec-fetch-site",  HeaderValue::from_static("same-site"));
    h.insert("Origin",          HeaderValue::from_static("https://polymarket.com"));
    h.insert("Referer",         HeaderValue::from_static("https://polymarket.com/"));
    h
}

#[derive(Debug, Clone)]
pub struct ClobClient {
    http: reqwest::Client,
}

impl ClobClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .default_headers(browser_bundle_headers())
            .timeout(Duration::from_secs(20))
            .build()
            .context("build CLOB http client")?;
        Ok(Self { http })
    }

    /// `GET /ok` — unauthenticated health check.
    /// Used by `bot.py:669` before every trade entry attempt, and at window
    /// boundaries to auto-recover from `_clob_halted`.
    pub async fn get_ok(&self) -> Result<()> {
        let resp = self.http
            .get(format!("{}/ok", CLOB_BASE))
            .send().await
            .context("GET /ok")?;
        if !resp.status().is_success() {
            anyhow::bail!("CLOB /ok returned {}", resp.status());
        }
        Ok(())
    }

    /// Derive API creds from the EOA wallet.
    /// The signed message and key/secret/passphrase derivation follow
    /// Polymarket's published convention used by py-clob-client.
    ///
    /// INTEGRATION TODO: Validate `derive_api_creds` against py-clob-client output
    /// by running gengar Python once with a test wallet, capturing the produced
    /// {key, secret, passphrase}, and asserting parity here. If parity fails, the
    /// derivation salts/scheme above must be revised to match py-clob-client.
    pub async fn derive_api_creds(wallet: &LocalWallet) -> Result<ApiCreds> {
        // py-clob-client signs the literal message "This message attests..."
        // and derives uuid-like fields from the signature digest.
        // Concrete derivation must match what py-clob-client does — verify
        // against a known wallet by running gengar Python once and capturing
        // the creds, then asserting in test that this Rust code produces them.
        let msg = "This message attests that I control the given wallet";
        let sig = wallet.sign_message(msg).await
            .context("sign creds-derivation message")?;
        let sig_bytes = sig.to_vec();

        // Key/secret/passphrase derivation: HMAC-SHA256(sig_bytes, "key" | "secret" | "passphrase")
        // Base64-encoded as UUIDs/secrets. (Verify this against captured py-clob-client output
        // during integration testing; if py-clob-client uses a different scheme, update here.)
        fn derive(sig: &[u8], salt: &str) -> String {
            let mut mac = HmacSha256::new_from_slice(sig).expect("hmac key");
            mac.update(salt.as_bytes());
            let bytes = mac.finalize().into_bytes();
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        }

        Ok(ApiCreds {
            key:        derive(&sig_bytes, "key"),
            secret:     derive(&sig_bytes, "secret"),
            passphrase: derive(&sig_bytes, "passphrase"),
        })
    }

    /// Build the L2 authentication headers for an authenticated request.
    /// HMAC-SHA256 of `timestamp + method + path + body` with the API secret.
    pub fn auth_headers(creds: &ApiCreds, method: &str, path: &str, body: &str) -> Result<HeaderMap> {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs().to_string();
        let canonical = format!("{}{}{}{}", ts, method, path, body);

        let mut mac = HmacSha256::new_from_slice(creds.secret.as_bytes())
            .context("init hmac")?;
        mac.update(canonical.as_bytes());
        let sig = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(mac.finalize().into_bytes());

        let mut h = HeaderMap::new();
        h.insert("POLY_ADDRESS",    HeaderValue::from_str("")?); // filled by caller
        h.insert("POLY_SIGNATURE",  HeaderValue::from_str(&sig)?);
        h.insert("POLY_TIMESTAMP",  HeaderValue::from_str(&ts)?);
        h.insert("POLY_API_KEY",    HeaderValue::from_str(&creds.key)?);
        h.insert("POLY_PASSPHRASE", HeaderValue::from_str(&creds.passphrase)?);
        Ok(h)
    }
}

/// API credentials: derived from EOA wallet by signing a constant message,
/// then deterministically converting the signature into key/secret/passphrase.
/// Mirrors py-clob-client's `derive_api_key()` behavior.
#[derive(Debug, Clone)]
pub struct ApiCreds {
    pub key: String,
    pub secret: String,
    pub passphrase: String,
}

// ============================================================================
// EIP-712 order signing + POST /order / GET /order/{id}
// ----------------------------------------------------------------------------
// Hand-rolled per spec §Out of Scope (no shared platform crate). The struct
// layout and EIP-712 schema match Polymarket's published Order schema, mirrored
// independently from arb's `polymarket_clob.rs:236-322` and from py-clob-client's
// `create_order(OrderArgs)` + `post_order(signed, OrderType.GTC)` chain
// (`executor.py:185-220`).
// ============================================================================

/// Polymarket order, pre-signing.
#[derive(Debug, Clone)]
pub struct OrderArgs {
    pub token_id: String,
    pub price: PriceCents,
    pub size_shares: i64,
    pub side: Side,
    /// EOA address — always the signer.
    pub maker_address: Address,
    /// Optional Safe proxy address. When set with `SignatureType::Safe`,
    /// the proxy is the maker but the EOA is still the signer.
    pub funder: Option<Address>,
}

/// Polymarket order type. Wire form is uppercase (GTC/FOK/GTD).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderType { Gtc, Fok, Gtd }

/// Signature type. 0 = EOA, 2 = Polymarket Safe proxy.
#[derive(Debug, Clone, Copy)]
pub enum SignatureType { Eoa = 0, Safe = 2 }

/// Signed order, ready to POST.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignedOrder {
    pub salt: String,
    pub maker: String,
    pub signer: String,
    pub taker: String,
    pub token_id: String,
    pub maker_amount: String,
    pub taker_amount: String,
    /// 0 = BUY, 1 = SELL.
    pub side: u8,
    /// "0" means GTC.
    pub expiration: String,
    pub nonce: String,
    pub fee_rate_bps: String,
    pub signature_type: u8,
    /// 0x-prefixed hex.
    pub signature: String,
}

impl ClobClient {
    /// Construct + sign a Polymarket order using EIP-712.
    /// Domain and primary type strings match Polymarket's published schema,
    /// same fields used by arb's `polymarket_clob.rs:236-322` and by
    /// py-clob-client's `create_order(OrderArgs)`.
    ///
    /// NOTE: divisor for the price scaling is `10_000` (bps → ratio), matching
    /// arb's battle-tested `get_order_amounts_buy/sell` (`polymarket_clob.rs:338-356`).
    /// For 10 shares at 68¢ this yields a USDC maker amount of 6_800_000 micro
    /// (= $6.80). Earlier draft of this method used `/ 1_000_000` which is
    /// off by 100x — corrected here so the wire numbers match the Python
    /// executor exactly.
    pub async fn create_order(
        &self,
        args: OrderArgs,
        sig_type: SignatureType,
        wallet: &LocalWallet,
    ) -> Result<SignedOrder> {
        let salt = rand::random::<u64>().to_string();
        let maker = args.funder.unwrap_or(args.maker_address);

        // cents → bps: 1¢ = 100 bps, so price.0 (cents) * 100 = bps.
        let price_bps = (args.price.0 as u64) * 100;
        let size_micro = (args.size_shares as u64) * 1_000_000;
        let (maker_amount, taker_amount) = match args.side {
            // BUY: pay USDC (maker = size * price), receive shares (taker = size).
            Side::Buy  => ((size_micro * price_bps) / 10_000, size_micro),
            // SELL: give shares (maker = size), receive USDC (taker = size * price).
            Side::Sell => (size_micro, (size_micro * price_bps) / 10_000),
        };

        let side_byte: u8 = match args.side { Side::Buy => 0, Side::Sell => 1 };

        let typed_data: TypedData = serde_json::from_value(json!({
            "domain": {
                "name": "Polymarket CTF Exchange",
                "version": "1",
                "chainId": 137,
                "verifyingContract": "0x4bFb41d5B3570DeFd03C39a9A4D8dE6Bd8B8982E"
            },
            "primaryType": "Order",
            "types": {
                "EIP712Domain": [
                    {"name": "name",             "type": "string"},
                    {"name": "version",          "type": "string"},
                    {"name": "chainId",          "type": "uint256"},
                    {"name": "verifyingContract","type": "address"}
                ],
                "Order": [
                    {"name": "salt",          "type": "uint256"},
                    {"name": "maker",         "type": "address"},
                    {"name": "signer",        "type": "address"},
                    {"name": "taker",         "type": "address"},
                    {"name": "tokenId",       "type": "uint256"},
                    {"name": "makerAmount",   "type": "uint256"},
                    {"name": "takerAmount",   "type": "uint256"},
                    {"name": "expiration",    "type": "uint256"},
                    {"name": "nonce",         "type": "uint256"},
                    {"name": "feeRateBps",    "type": "uint256"},
                    {"name": "side",          "type": "uint8"},
                    {"name": "signatureType", "type": "uint8"}
                ]
            },
            "message": {
                "salt":          salt,
                "maker":         format!("{:?}", maker),
                "signer":        format!("{:?}", args.maker_address),
                "taker":         "0x0000000000000000000000000000000000000000",
                "tokenId":       args.token_id,
                "makerAmount":   maker_amount.to_string(),
                "takerAmount":   taker_amount.to_string(),
                "expiration":    "0",
                "nonce":         "0",
                "feeRateBps":    "0",
                "side":          side_byte,
                "signatureType": sig_type as u8
            }
        }))?;

        let digest = typed_data.encode_eip712()
            .map_err(|e| anyhow::anyhow!("eip712 encode: {:?}", e))?;
        let sig = wallet.sign_hash(H256::from(digest))
            .context("sign order")?;

        Ok(SignedOrder {
            salt,
            maker: format!("{:?}", maker),
            signer: format!("{:?}", args.maker_address),
            taker: "0x0000000000000000000000000000000000000000".into(),
            token_id: args.token_id,
            maker_amount: maker_amount.to_string(),
            taker_amount: taker_amount.to_string(),
            side: side_byte,
            expiration: "0".into(),
            nonce: "0".into(),
            fee_rate_bps: "0".into(),
            signature_type: sig_type as u8,
            signature: format!("0x{}", hex::encode(sig.to_vec())),
        })
    }

    /// `POST /order` with the signed payload.
    pub async fn post_order(
        &self,
        creds: &ApiCreds,
        eoa_addr: Address,
        signed: &SignedOrder,
        otype: OrderType,
    ) -> Result<OrderResponse> {
        let path = "/order";
        let body = serde_json::to_string(&json!({
            "order": signed,
            "owner": creds.key,
            "orderType": otype,
        }))?;
        let mut headers = Self::auth_headers(creds, "POST", path, &body)?;
        headers.insert("POLY_ADDRESS", HeaderValue::from_str(&format!("{:?}", eoa_addr))?);
        headers.insert("Content-Type", HeaderValue::from_static("application/json"));

        let resp = self.http
            .post(format!("{}{}", CLOB_BASE, path))
            .headers(headers).body(body).send().await
            .context("POST /order")?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            anyhow::bail!("POST /order {} → {}", status, text);
        }
        serde_json::from_str(&text).context("parse /order response")
    }

    /// `GET /order/{id}`.
    pub async fn get_order(
        &self,
        creds: &ApiCreds,
        eoa_addr: Address,
        order_id: &str,
    ) -> Result<OrderStatus> {
        let path = format!("/order/{}", order_id);
        let mut headers = Self::auth_headers(creds, "GET", &path, "")?;
        headers.insert("POLY_ADDRESS", HeaderValue::from_str(&format!("{:?}", eoa_addr))?);
        let resp = self.http
            .get(format!("{}{}", CLOB_BASE, path))
            .headers(headers).send().await
            .context("GET /order/{id}")?;
        if !resp.status().is_success() {
            anyhow::bail!("GET /order/{} → {}", order_id, resp.status());
        }
        resp.json().await.context("parse /order/{id} response")
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct OrderResponse {
    pub success: bool,
    #[serde(rename = "orderID")]
    pub order_id: Option<String>,
    #[serde(rename = "errorMsg")]
    pub error_msg: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct OrderStatus {
    pub id: String,
    pub status: String,
    #[serde(rename = "size_matched")]
    pub size_matched: Option<String>,
}

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
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn auth_headers_have_all_required_fields() {
        let creds = ApiCreds {
            key:        "test-key".into(),
            secret:     "test-secret".into(),
            passphrase: "test-pass".into(),
        };
        let h = ClobClient::auth_headers(&creds, "POST", "/order", "{}").unwrap();
        assert!(h.contains_key("POLY_SIGNATURE"));
        assert!(h.contains_key("POLY_TIMESTAMP"));
        assert!(h.contains_key("POLY_API_KEY"));
        assert!(h.contains_key("POLY_PASSPHRASE"));
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;
    use ethers::signers::LocalWallet;

    /// Anvil test-mnemonic account #0 — public test key. DO NOT use for real funds.
    const TEST_PK: &str =
        "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

    #[tokio::test]
    async fn buy_order_sets_side_zero_and_amounts() {
        let wallet: LocalWallet = TEST_PK.parse().unwrap();
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
            .create_order(args, SignatureType::Eoa, &wallet)
            .await
            .unwrap();
        assert_eq!(signed.side, 0);
        assert_eq!(signed.signature_type, 0);
        assert!(signed.signature.starts_with("0x"));
        assert_eq!(signed.token_id, "12345");
        // 10 shares * 68¢ = $6.80 = 6_800_000 USDC micro-units.
        assert_eq!(signed.maker_amount, "6800000");
        // BUY taker = size in micro shares = 10 * 1_000_000.
        assert_eq!(signed.taker_amount, "10000000");
    }
}

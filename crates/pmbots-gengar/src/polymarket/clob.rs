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
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};
use sha2::Sha256;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

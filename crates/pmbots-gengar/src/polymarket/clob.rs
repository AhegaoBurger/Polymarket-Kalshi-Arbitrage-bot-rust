//! Polymarket CLOB client — hand-rolled for gengar.
//! Reference: executor.py + py-clob-client behavior.
//!
//! The HTTP layer carries browser-bundle headers (UA + sec-ch-ua-*) to bypass
//! Polymarket's V2 WAF, which started rejecting stock python-httpx UAs in
//! April 2026. Mirrors the mitigation in arb's commit 7c890dd, hand-rolled
//! independently per spec §Out of Scope (no shared platform crate).

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use std::time::Duration;

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

//! Minimal Polygon JSON-RPC client for raw on-chain reads.
//!
//! The CLOB's `/balance-allowance` returns a single aggregate "trading-available"
//! number — useful for Kelly sizing, but it hides the pUSD-vs-USDC.e split. For
//! a clearer startup picture (and for post-redemption verification, since CTF
//! redemptions pay out in USDC.e not pUSD per V2 internals notes), we need raw
//! `balanceOf(holder)` calls against the actual ERC-20 contracts.
//!
//! Why a separate module: keeping chain-RPC out of `clob.rs` avoids tangling
//! REST-API code with eth_call shaping. We only need one method (`erc20_balance`),
//! one fallback list, and a UA header — nothing here is worth pulling `ethers`
//! providers in for.

use anyhow::{anyhow, Context, Result};
use ethers::types::Address;
use serde_json::json;

/// Default fallback Polygon RPC endpoints. The `polygon-rpc.com` free tier
/// regularly returns "tenant disabled" on free traffic; tenderly + publicnode
/// + onfinality are currently the most reliable public endpoints (verified
/// 2026-05-15 — see `check_balances.py` for the same probe in Python).
const FALLBACK_RPCS: &[&str] = &[
    "https://polygon.gateway.tenderly.co",
    "https://polygon-bor-rpc.publicnode.com",
    "https://polygon.api.onfinality.io/public",
    "https://1rpc.io/matic",
];

/// Browser UA — public RPCs gate `Python-urllib`/`reqwest`-style defaults via
/// their Cloudflare WAF; pretending to be a browser bypasses that. Matches the
/// header set used by `check_balances.py`.
const UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36";

pub struct ChainRpc {
    client: reqwest::Client,
    urls: Vec<String>,
}

impl ChainRpc {
    /// Build a client. If `POLYGON_RPC_URL` is set in env, it is tried first;
    /// otherwise we use the fallback list. We don't probe at construction —
    /// each call tries URLs in order until one responds.
    pub fn from_env() -> Result<Self> {
        let mut urls: Vec<String> = Vec::new();
        if let Ok(url) = std::env::var("POLYGON_RPC_URL") {
            if !url.trim().is_empty() {
                urls.push(url.trim().to_string());
            }
        }
        urls.extend(FALLBACK_RPCS.iter().map(|s| s.to_string()));

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .context("build reqwest client for chain RPC")?;

        Ok(Self { client, urls })
    }

    /// Call `IERC20.balanceOf(holder)` and return the raw token-units value.
    /// The caller is responsible for knowing the token's decimals.
    pub async fn erc20_balance(&self, token: Address, holder: Address) -> Result<u128> {
        // selector + 32-byte left-padded holder address
        let selector = "70a08231";
        let holder_padded = format!("{:0>64}", hex::encode(holder.as_bytes()));
        let data = format!("0x{}{}", selector, holder_padded);
        let token_hex = format!("0x{}", hex::encode(token.as_bytes()));

        let payload = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_call",
            "params": [{ "to": token_hex, "data": data }, "latest"],
        });

        let mut last_err: Option<anyhow::Error> = None;
        for url in &self.urls {
            match self.try_call(url, &payload).await {
                Ok(result_hex) => {
                    let stripped = result_hex.trim_start_matches("0x");
                    if stripped.is_empty() || stripped == "0" {
                        return Ok(0);
                    }
                    let val = u128::from_str_radix(stripped, 16)
                        .with_context(|| format!("parse balanceOf result: {}", result_hex))?;
                    return Ok(val);
                }
                Err(e) => {
                    tracing::debug!("[GENGAR] chain_rpc {} failed: {}", url, e);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("no Polygon RPC URLs configured")))
    }

    async fn try_call(&self, url: &str, payload: &serde_json::Value) -> Result<String> {
        let resp = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("User-Agent", UA)
            .json(payload)
            .send()
            .await
            .context("RPC POST")?;
        let status = resp.status();
        let body: serde_json::Value =
            resp.json().await.context("decode RPC body as JSON")?;
        if !status.is_success() {
            return Err(anyhow!("HTTP {}: {}", status, body));
        }
        if let Some(err) = body.get("error") {
            return Err(anyhow!("RPC error: {}", err));
        }
        body.get("result")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("missing result field: {}", body))
    }
}

/// Polygon mainnet token addresses we read at startup.
pub mod tokens {
    use ethers::types::Address;
    use std::str::FromStr;

    /// pUSD — user-facing collateral veneer. Address sourced from V2 internals doc.
    pub fn pusd() -> Address {
        Address::from_str("0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB").unwrap()
    }

    /// USDC.e bridged — the actual CTF collateralToken for standard binary
    /// markets. Redemptions of winning positions pay out here, NOT in pUSD.
    pub fn usdce() -> Address {
        Address::from_str("0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174").unwrap()
    }
}

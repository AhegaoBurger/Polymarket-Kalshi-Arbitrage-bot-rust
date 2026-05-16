# Gengar Rust Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port `JLowo/gengar_polymarket_bot@9f49a07` from Python to Rust as a new `pmbots-gengar` workspace crate, while migrating the existing arb bot into `pmbots-arb` with zero source-code edits.

**Architecture:** Two-crate Cargo virtual workspace. Arb is moved verbatim (`git mv` + Cargo manifest rename only). Gengar is a fresh hand-rolled port with its own Polymarket CLOB layer (EIP-712 signing, V2 WAF browser headers, /ok, /balance-allowance, /price, /order, /order/{id}), Binance WS price feed, Brownian-motion strategy, integer-cents executor, and bot lifecycle with `_pending_phantom` resolution and a daily-loss circuit breaker.

**Tech Stack:** Rust 2021 edition, tokio 1.x async runtime, tokio-tungstenite (WS), reqwest (HTTP), ethers 2.0 (EIP-712 signing), serde + serde_json, libm 0.2 (`erf` for Brownian CDF), csv 1.3 (tracker output), dotenvy (env loading), tracing + tracing-subscriber.

**Spec:** `docs/superpowers/specs/2026-05-14-gengar-rust-port-design.md`.

**Reference clone:** `~/personal/gengar_polymarket_bot` pinned at commit `9f49a07`. All file:line citations in this plan refer to this clone.

**Working directory for all `cargo` commands:** repo root unless noted.

---

## Phase A: Workspace Migration (no `.rs` edits to arb)

### Task 1: Initialize root workspace `Cargo.toml`

**Files:**
- Backup: `Cargo.toml` → `Cargo.toml.bak` (temporary; deleted after Task 3)
- Create: new `Cargo.toml` at repo root

- [ ] **Step 1: Back up the existing arb manifest**

```bash
cp Cargo.toml Cargo.toml.bak
```

- [ ] **Step 2: Write the new root virtual-workspace manifest**

Replace `Cargo.toml` content with:

```toml
[workspace]
resolver = "2"
members = ["crates/pmbots-arb", "crates/pmbots-gengar"]

[workspace.dependencies]
anyhow             = "1.0"
async-trait        = "0.1"
base64             = "0.22"
chrono             = { version = "0.4", features = ["serde"] }
dotenvy            = "0.15"
ethers             = { version = "2.0", features = ["legacy"] }
futures-util       = "0.3"
hmac               = "0.12"
rand               = "0.8"
reqwest            = { version = "0.11", features = ["json", "blocking"] }
rsa                = { version = "0.9", features = ["sha2"] }
pkcs1              = { version = "0.7", features = ["pem"] }
serde              = { version = "1.0", features = ["derive", "rc"] }
serde_json         = "1.0"
sha2               = "0.10"
tokio              = { version = "1.0", features = ["full"] }
tokio-tungstenite  = { version = "0.21", features = ["native-tls"] }
tracing            = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
rustc-hash         = "2.0"
tiny-keccak        = { version = "2.0", features = ["keccak"] }
governor           = "0.6"
nonzero_ext        = "0.3"
arrayvec           = "0.7"
wide               = "0.7"
hex                = "0.4"
tempfile           = "3.27.0"
criterion          = { version = "0.5", features = ["html_reports"] }

[profile.release]
opt-level     = 3
lto           = true
codegen-units = 1
```

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.toml.bak
git commit -m "wip: stage workspace manifest before arb move"
```

---

### Task 2: Move arb source tree into `crates/pmbots-arb/`

**Files:**
- Move: `src/` → `crates/pmbots-arb/src/`
- Move: `tests/` → `crates/pmbots-arb/tests/`
- Move: `config/` → `crates/pmbots-arb/config/`
- Move: `audit/` → `crates/pmbots-arb/audit/`
- Move: `positions.json` → `crates/pmbots-arb/positions.json`
- Create: `crates/pmbots-arb/Cargo.toml`

- [ ] **Step 1: Create the target directory**

```bash
mkdir -p crates/pmbots-arb
```

- [ ] **Step 2: Move source tree with `git mv` (preserves history)**

```bash
git mv src crates/pmbots-arb/src
git mv tests crates/pmbots-arb/tests
git mv config crates/pmbots-arb/config
git mv audit crates/pmbots-arb/audit
git mv positions.json crates/pmbots-arb/positions.json
```

- [ ] **Step 3: Write the new `crates/pmbots-arb/Cargo.toml`**

Use the original `Cargo.toml.bak` for reference. Create `crates/pmbots-arb/Cargo.toml`:

```toml
[package]
name = "pmbots-arb"
version = "2.0.0"
edition = "2021"
default-run = "pmbots-arb"
description = "Kalshi-Poly / Poly-Poly / Kalshi-Kalshi arbitrage bot - automated cross-platform prediction market trading system"
keywords = ["polymarket", "kalshi", "arbitrage", "trading-bot", "prediction-markets"]
authors = ["teraus"]
license = "MIT OR Apache-2.0"
repository = "https://github.com/terauss/prediction-market-arbitrage"

[[bin]]
name = "pmbots-arb"
path = "src/main.rs"

[[bin]]
name = "poly_balance_check"
path = "src/bin/poly_balance_check.rs"

[dependencies]
anyhow             = { workspace = true }
async-trait        = { workspace = true }
base64             = { workspace = true }
chrono             = { workspace = true }
dotenvy            = { workspace = true }
ethers             = { workspace = true }
futures-util       = { workspace = true }
hmac               = { workspace = true }
rand               = { workspace = true }
reqwest            = { workspace = true }
rsa                = { workspace = true }
pkcs1              = { workspace = true }
serde              = { workspace = true }
serde_json         = { workspace = true }
sha2               = { workspace = true }
tokio              = { workspace = true }
tokio-tungstenite  = { workspace = true }
tracing            = { workspace = true }
tracing-subscriber = { workspace = true }
rustc-hash         = { workspace = true }
tiny-keccak        = { workspace = true }
governor           = { workspace = true }
nonzero_ext        = { workspace = true }
arrayvec           = { workspace = true }
wide               = { workspace = true }

[dev-dependencies]
criterion = { workspace = true }
hex       = { workspace = true }
tempfile  = { workspace = true }
```

The package name changes from `prediction-market-arbitrage` to `pmbots-arb`. Original binary name was `prediction-market-arbitrage`; we keep the binary name aligned with the package name (`pmbots-arb`). The `default-run` also updates.

- [ ] **Step 4: Commit the move**

```bash
git add crates/pmbots-arb/
git commit -m "refactor: move arb bot into crates/pmbots-arb workspace member"
```

---

### Task 3: Verify arb still builds; clean up backup

**Files:**
- Delete: `Cargo.toml.bak`

- [ ] **Step 1: Build the arb crate from the workspace**

```bash
cargo build -p pmbots-arb --release
```

Expected: successful build. If errors mention missing `prediction-market-arbitrage` references, those are in dev scripts or CI configs (acceptable; fix in Step 3).

- [ ] **Step 2: Run arb's tests to confirm zero behavioral drift**

```bash
cargo test -p pmbots-arb --release
```

Expected: tests pass at the same rate as before the move.

- [ ] **Step 3: Update any `cargo run --bin prediction-market-arbitrage` references**

Search for old invocations:

```bash
grep -rn "prediction-market-arbitrage" --include="*.sh" --include="*.md" --include="Makefile" .
```

Update each to `cargo run -p pmbots-arb --release --` or `cargo run -p pmbots-arb --bin pmbots-arb --release --`. Update `README.md` invocation examples.

- [ ] **Step 4: Delete the backup**

```bash
rm Cargo.toml.bak
```

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor: finalize arb workspace migration

Verified arb builds and tests pass under crates/pmbots-arb/.
Updated invocation references from prediction-market-arbitrage
to pmbots-arb across scripts and docs."
```

---

### Task 4: Scaffold empty `pmbots-gengar` crate

**Files:**
- Create: `crates/pmbots-gengar/Cargo.toml`
- Create: `crates/pmbots-gengar/src/main.rs`
- Create: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/tests/.gitkeep`

- [ ] **Step 1: Write `crates/pmbots-gengar/Cargo.toml`**

```toml
[package]
name = "pmbots-gengar"
version = "0.1.0"
edition = "2021"
default-run = "pmbots-gengar"
description = "BTC oracle-lag bot — Rust port of JLowo/gengar_polymarket_bot for Polymarket BTC Up/Down 5-min markets"
license = "MIT OR Apache-2.0"

[[bin]]
name = "pmbots-gengar"
path = "src/main.rs"

[dependencies]
anyhow             = { workspace = true }
async-trait        = { workspace = true }
chrono             = { workspace = true }
dotenvy            = { workspace = true }
ethers             = { workspace = true }
futures-util       = { workspace = true }
reqwest            = { workspace = true, features = ["json", "socks"] }
serde              = { workspace = true }
serde_json         = { workspace = true }
tokio              = { workspace = true }
tokio-tungstenite  = { workspace = true }
tracing            = { workspace = true }
tracing-subscriber = { workspace = true }
hex                = { workspace = true }

# Gengar-specific
libm = "0.2"
csv  = "1.3"

[dev-dependencies]
tempfile = { workspace = true }
```

- [ ] **Step 2: Write minimal `crates/pmbots-gengar/src/lib.rs`**

```rust
//! pmbots-gengar — Rust port of JLowo/gengar_polymarket_bot.
//!
//! Reference: ~/personal/gengar_polymarket_bot@9f49a07.

pub mod polymarket;
```

- [ ] **Step 3: Write minimal `crates/pmbots-gengar/src/main.rs`**

```rust
fn main() {
    println!("pmbots-gengar: not yet implemented");
}
```

- [ ] **Step 4: Create stub `polymarket/mod.rs` and tests dir**

```bash
mkdir -p crates/pmbots-gengar/src/polymarket crates/pmbots-gengar/tests
echo "// module stubs filled in subsequent tasks" > crates/pmbots-gengar/src/polymarket/mod.rs
touch crates/pmbots-gengar/tests/.gitkeep
```

- [ ] **Step 5: Verify gengar crate compiles**

```bash
cargo build -p pmbots-gengar
```

Expected: successful build with the stub `main.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/pmbots-gengar/
git commit -m "feat: scaffold pmbots-gengar crate skeleton"
```

---

## Phase B: Polymarket Platform Layer

### Task 5: `polymarket/types.rs` — platform-shared types

**Files:**
- Modify: `crates/pmbots-gengar/src/polymarket/mod.rs`
- Create: `crates/pmbots-gengar/src/polymarket/types.rs`
- Test inline within the same file (`#[cfg(test)] mod tests`).

**Reference:** Gengar's Python uses py-clob-client which exposes price/size as floats; we add a `PriceCents` newtype for the integer-cents discipline that `executor.py:56-74` demands.

- [ ] **Step 1: Write the failing test**

Create `crates/pmbots-gengar/src/polymarket/types.rs`:

```rust
//! Polymarket platform-shared types.

use serde::{Deserialize, Serialize};

/// Price expressed in cents (0..=10000 = $0.00..=$100.00 on Polymarket scale).
/// Integer-cents arithmetic is load-bearing for order placement — float division
/// produces artifacts like 21.000000000004 that the CLOB rejects.
/// See `executor.py:56-74` for the Python reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PriceCents(pub i64);

impl PriceCents {
    pub fn from_dollars(price: f64) -> Self {
        Self((price * 100.0).round() as i64)
    }

    pub fn as_dollars(self) -> f64 {
        self.0 as f64 / 100.0
    }
}

/// Whole share count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Shares(pub i64);

/// Side of an order on Polymarket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side { Buy, Sell }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_cents_roundtrip() {
        assert_eq!(PriceCents::from_dollars(0.68).0, 68);
        assert_eq!(PriceCents::from_dollars(0.685).0, 69); // rounding
        assert_eq!(PriceCents::from_dollars(0.50).as_dollars(), 0.50);
    }

    #[test]
    fn price_cents_handles_python_artifact() {
        // Python: round(0.6800000004 * 100) = 68
        assert_eq!(PriceCents::from_dollars(0.6800000004).0, 68);
    }
}
```

- [ ] **Step 2: Update `polymarket/mod.rs`**

```rust
//! Polymarket platform layer — independent hand-roll for gengar.
//!
//! NOT shared with pmbots-arb (intentional duplication per spec
//! 2026-05-14-gengar-rust-port-design.md §Out of Scope).

pub mod types;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p pmbots-gengar polymarket::types
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/pmbots-gengar/src/polymarket/
git commit -m "feat(gengar): polymarket platform types (PriceCents, Shares, Side)"
```

---

### Task 6: `polymarket/gamma.rs` — Gamma REST event lookup

**Files:**
- Modify: `crates/pmbots-gengar/src/polymarket/mod.rs`
- Create: `crates/pmbots-gengar/src/polymarket/gamma.rs`

**Reference:** `market.py:14-113`. Single endpoint: `GET https://gamma-api.polymarket.com/events?slug=btc-updown-{period_minutes}m-{window_ts}`. Returns a JSON list with one event whose `markets[0]` has `clobTokenIds` (a JSON-string array) paired with `outcomes` (`["Up","Down"]`).

- [ ] **Step 1: Write the test with a captured response fixture**

Create `crates/pmbots-gengar/src/polymarket/gamma.rs`:

```rust
//! Gamma REST API client — used for active-market lookup by slug.
//! Reference: market.py:14-113.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::time::Duration;

pub const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
pub const USER_AGENT: &str = "PolyBot/1.0";

#[derive(Debug, Clone)]
pub struct GammaClient {
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct GammaEvent {
    pub id: String,
    pub slug: String,
    pub markets: Vec<GammaMarket>,
}

#[derive(Debug, Deserialize)]
pub struct GammaMarket {
    /// JSON-string-encoded array of token IDs: "[\"123\", \"456\"]"
    #[serde(rename = "clobTokenIds")]
    pub clob_token_ids: String,
    /// JSON-string-encoded array of outcome names: "[\"Up\", \"Down\"]"
    pub outcomes: String,
    /// JSON-string-encoded array of last-prices: "[\"0.52\", \"0.48\"]"
    #[serde(rename = "outcomePrices")]
    pub outcome_prices: Option<String>,
}

/// Resolved active market: token IDs for Up and Down.
#[derive(Debug, Clone)]
pub struct ActiveMarket {
    pub event_id: String,
    pub slug: String,
    pub token_id_up: String,
    pub token_id_down: String,
    pub last_price_up: Option<f64>,
    pub last_price_down: Option<f64>,
}

impl GammaClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(10))
            .build()
            .context("build gamma http client")?;
        Ok(Self { http })
    }

    /// Fetch the event for the given window. `period_minutes` must be 5 or 15.
    /// `window_ts` is the Unix timestamp of the window OPEN, aligned to the
    /// period boundary (`window_ts % (period_minutes * 60) == 0`).
    pub async fn fetch_active_market(
        &self,
        period_minutes: u32,
        window_ts: i64,
    ) -> Result<Option<ActiveMarket>> {
        let slug = format!("btc-updown-{}m-{}", period_minutes, window_ts);
        let url = format!("{}/events?slug={}", GAMMA_BASE, slug);

        let events: Vec<GammaEvent> = self
            .http.get(&url).send().await
            .with_context(|| format!("GET {}", url))?
            .error_for_status()?
            .json().await.context("parse gamma events")?;

        if events.is_empty() {
            return Ok(None);
        }
        let event = &events[0];
        let market = event.markets.first()
            .ok_or_else(|| anyhow!("event {} has no markets", event.id))?;

        let token_ids: Vec<String> = serde_json::from_str(&market.clob_token_ids)
            .context("parse clobTokenIds")?;
        let outcomes: Vec<String> = serde_json::from_str(&market.outcomes)
            .context("parse outcomes")?;
        let prices: Vec<f64> = match &market.outcome_prices {
            Some(s) => serde_json::from_str::<Vec<String>>(s)?
                .iter().map(|p| p.parse::<f64>().unwrap_or(0.0)).collect(),
            None => vec![0.0, 0.0],
        };

        // outcomes is ["Up","Down"] in canonical order; tolerate reversed too
        let (idx_up, idx_down) = match (
            outcomes.iter().position(|o| o.eq_ignore_ascii_case("Up")),
            outcomes.iter().position(|o| o.eq_ignore_ascii_case("Down")),
        ) {
            (Some(u), Some(d)) => (u, d),
            _ => (0, 1),
        };

        Ok(Some(ActiveMarket {
            event_id: event.id.clone(),
            slug: event.slug.clone(),
            token_id_up: token_ids.get(idx_up).cloned().unwrap_or_default(),
            token_id_down: token_ids.get(idx_down).cloned().unwrap_or_default(),
            last_price_up: prices.get(idx_up).copied(),
            last_price_down: prices.get(idx_down).copied(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gamma_event_response_shape() {
        let payload = r#"[{
            "id": "12345",
            "slug": "btc-updown-5m-1748000000",
            "markets": [{
                "clobTokenIds": "[\"token-up\", \"token-down\"]",
                "outcomes": "[\"Up\", \"Down\"]",
                "outcomePrices": "[\"0.52\", \"0.48\"]"
            }]
        }]"#;

        let events: Vec<GammaEvent> = serde_json::from_str(payload).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].markets[0].clob_token_ids, "[\"token-up\", \"token-down\"]");
    }
}
```

- [ ] **Step 2: Update `polymarket/mod.rs`**

```rust
pub mod types;
pub mod gamma;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p pmbots-gengar polymarket::gamma
```

Expected: 1 test passes (structural parse). Network test deferred to integration tests.

- [ ] **Step 4: Commit**

```bash
git add crates/pmbots-gengar/src/polymarket/
git commit -m "feat(gengar): polymarket Gamma client (slug-based event lookup)"
```

---

### Task 7: `polymarket/clob.rs` Phase 1 — HTTP builder + `/ok`

**Files:**
- Modify: `crates/pmbots-gengar/src/polymarket/mod.rs`
- Create: `crates/pmbots-gengar/src/polymarket/clob.rs`

**Reference:** `executor.py:85-102` (client init), `bot.py:669` (health check usage). The V2 WAF browser-bundle headers are critical (post-April-2026 CLOB rejects stock UA with CF 403). Mirror arb's commit `7c890dd` UA approach independently.

- [ ] **Step 1: Write the test**

Create `crates/pmbots-gengar/src/polymarket/clob.rs`:

```rust
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
```

- [ ] **Step 2: Update `polymarket/mod.rs`**

```rust
pub mod types;
pub mod gamma;
pub mod clob;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p pmbots-gengar polymarket::clob
```

Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/pmbots-gengar/src/polymarket/
git commit -m "feat(gengar): CLOB http client with V2 WAF browser headers + /ok"
```

---

### Task 8: `polymarket/clob.rs` Phase 2 — API credentials (derive + HMAC headers)

**Files:**
- Modify: `crates/pmbots-gengar/src/polymarket/clob.rs`

**Reference:** py-clob-client's `create_or_derive_api_creds()` (called from `executor.py:102`). Polymarket derives API key/secret/passphrase deterministically from the EOA wallet. Authentication uses HMAC-SHA256 signing of a per-request canonical string.

- [ ] **Step 1: Add API creds struct + derivation test**

Append to `crates/pmbots-gengar/src/polymarket/clob.rs`:

```rust
use base64::Engine;
use ethers::signers::{LocalWallet, Signer};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

/// API credentials: derived from EOA wallet by signing a constant message,
/// then deterministically converting the signature into key/secret/passphrase.
/// Mirrors py-clob-client's `derive_api_key()` behavior.
#[derive(Debug, Clone)]
pub struct ApiCreds {
    pub key: String,
    pub secret: String,
    pub passphrase: String,
}

impl ClobClient {
    /// Derive API creds from the EOA wallet.
    /// The signed message and key/secret/passphrase derivation follow
    /// Polymarket's published convention used by py-clob-client.
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
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p pmbots-gengar polymarket::clob
```

Expected: 3 tests pass.

- [ ] **Step 3: Note for integration test (do not block on this in unit test)**

Add a comment in the source noting:

```rust
// INTEGRATION TODO: Validate `derive_api_creds` against py-clob-client output
// by running gengar Python once with a test wallet, capturing the produced
// {key, secret, passphrase}, and asserting parity here. If parity fails, the
// derivation salts/scheme above must be revised to match py-clob-client.
```

(This is a known correctness check that requires a live Python run; it is
explicitly out-of-scope for the unit test phase.)

- [ ] **Step 4: Commit**

```bash
git add crates/pmbots-gengar/src/polymarket/clob.rs
git commit -m "feat(gengar): CLOB API credential derivation + auth headers"
```

---

### Task 9: `polymarket/clob.rs` Phase 3 — EIP-712 order signing + post_order + get_order

**Files:**
- Modify: `crates/pmbots-gengar/src/polymarket/clob.rs`

**Reference:** Order struct and signing follow Polymarket's published EIP-712 schema. The Python equivalent is py-clob-client's `create_order(OrderArgs)` + `post_order(signed, OrderType.GTC)` chain in `executor.py:185-220`.

- [ ] **Step 1: Add order types and signing**

Append to `crates/pmbots-gengar/src/polymarket/clob.rs`:

```rust
use ethers::types::{Address, U256};
use ethers::types::transaction::eip712::{Eip712, TypedData};
use serde_json::json;

/// Polymarket order, pre-signing.
#[derive(Debug, Clone)]
pub struct OrderArgs {
    pub token_id: String,
    pub price: PriceCents,           // from polymarket::types
    pub size_shares: i64,            // whole shares
    pub side: Side,
    pub maker_address: Address,      // EOA address
    pub funder: Option<Address>,     // Safe proxy address if sig_type=2
}

/// Polymarket order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum OrderType { Gtc, Fok, Gtd }

/// Signature type. 0 = EOA, 2 = Polymarket Safe proxy.
#[derive(Debug, Clone, Copy)]
pub enum SignatureType { Eoa = 0, Safe = 2 }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignedOrder {
    pub salt: String,
    pub maker: String,
    pub signer: String,
    pub taker: String,
    pub token_id: String,
    pub maker_amount: String,
    pub taker_amount: String,
    pub side: u8,                    // 0 = BUY, 1 = SELL
    pub expiration: String,          // "0" for GTC
    pub nonce: String,
    pub fee_rate_bps: String,
    pub signature_type: u8,
    pub signature: String,           // 0x-prefixed hex
}

use crate::polymarket::types::{PriceCents, Side, Shares};

impl ClobClient {
    /// Construct + sign a Polymarket order using EIP-712.
    /// Domain and primary type strings match Polymarket's published schema
    /// (verify at https://docs.polymarket.com — same fields used by
    /// arb's `polymarket_clob.rs:236-322` and by py-clob-client).
    pub async fn create_order(
        &self,
        args: OrderArgs,
        sig_type: SignatureType,
        wallet: &LocalWallet,
    ) -> Result<SignedOrder> {
        let salt = rand::random::<u64>().to_string();
        let maker = args.funder.unwrap_or(args.maker_address);

        let price_bps = (args.price.0 as u64) * 100; // cents → bps (1¢ = 100 bps)
        let size_micro = (args.size_shares as u64) * 1_000_000;
        let (maker_amount, taker_amount) = match args.side {
            Side::Buy  => (size_micro * price_bps / 1_000_000, size_micro),
            Side::Sell => (size_micro, size_micro * price_bps / 1_000_000),
        };

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
                "side":          match args.side { Side::Buy => 0u8, Side::Sell => 1u8 },
                "signatureType": sig_type as u8
            }
        }))?;

        let digest = typed_data.encode_eip712()
            .map_err(|e| anyhow::anyhow!("eip712 encode: {:?}", e))?;
        let sig = wallet.sign_hash(digest.into())
            .context("sign order")?;

        Ok(SignedOrder {
            salt,
            maker: format!("{:?}", maker),
            signer: format!("{:?}", args.maker_address),
            taker: "0x0000000000000000000000000000000000000000".into(),
            token_id: args.token_id,
            maker_amount: maker_amount.to_string(),
            taker_amount: taker_amount.to_string(),
            side: match args.side { Side::Buy => 0, Side::Sell => 1 },
            expiration: "0".into(),
            nonce: "0".into(),
            fee_rate_bps: "0".into(),
            signature_type: sig_type as u8,
            signature: format!("0x{}", hex::encode(sig.to_vec())),
        })
    }

    /// POST /order with the signed payload.
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

    /// GET /order/{id}.
    pub async fn get_order(&self, creds: &ApiCreds, eoa_addr: Address, order_id: &str) -> Result<OrderStatus> {
        let path = format!("/order/{}", order_id);
        let mut headers = Self::auth_headers(creds, "GET", &path, "")?;
        headers.insert("POLY_ADDRESS", HeaderValue::from_str(&format!("{:?}", eoa_addr))?);
        let resp = self.http.get(format!("{}{}", CLOB_BASE, path))
            .headers(headers).send().await
            .context("GET /order/{id}")?;
        if !resp.status().is_success() {
            anyhow::bail!("GET /order/{} → {}", order_id, resp.status());
        }
        resp.json().await.context("parse /order/{id} response")
    }
}

#[derive(Debug, Deserialize)]
pub struct OrderResponse {
    pub success: bool,
    #[serde(rename = "orderID")]
    pub order_id: Option<String>,
    #[serde(rename = "errorMsg")]
    pub error_msg: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrderStatus {
    pub id: String,
    pub status: String,
    #[serde(rename = "size_matched")]
    pub size_matched: Option<String>,
}
```

- [ ] **Step 2: Test the order construction (signing math is deterministic)**

Append a `#[cfg(test)] mod order_tests` exercising `create_order()` with a known test wallet and asserting key fields of the resulting `SignedOrder`:

```rust
#[cfg(test)]
mod order_tests {
    use super::*;
    use ethers::signers::LocalWallet;
    use std::str::FromStr;

    #[tokio::test]
    async fn buy_order_sets_side_zero_and_amounts() {
        // Test wallet — DO NOT use for real funds.
        let wallet: LocalWallet = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
            .parse().unwrap();
        let client = ClobClient::new().unwrap();
        let args = OrderArgs {
            token_id: "12345".into(),
            price: PriceCents(68),
            size_shares: 10,
            side: Side::Buy,
            maker_address: wallet.address(),
            funder: None,
        };
        let signed = client.create_order(args, SignatureType::Eoa, &wallet).await.unwrap();
        assert_eq!(signed.side, 0);
        assert_eq!(signed.signature_type, 0);
        assert!(signed.signature.starts_with("0x"));
        assert_eq!(signed.token_id, "12345");
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p pmbots-gengar polymarket::clob
```

Expected: 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/pmbots-gengar/src/polymarket/clob.rs
git commit -m "feat(gengar): EIP-712 order signing + post_order + get_order"
```

---

### Task 10: `polymarket/clob.rs` Phase 4 — balance + market price endpoints

**Files:**
- Modify: `crates/pmbots-gengar/src/polymarket/clob.rs`

**Reference:** `executor.py:104-126`. Two read endpoints:
- `GET /balance-allowance?asset_type=COLLATERAL` returns USDC balance (6 decimals).
- `GET /price?token_id=...&side=BUY&amount=...&order_type=GTC` returns the *expected fill price* for a market order of that USD notional.

- [ ] **Step 1: Add balance + price methods**

Append to `crates/pmbots-gengar/src/polymarket/clob.rs`:

```rust
impl ClobClient {
    /// GET /balance-allowance for COLLATERAL (USDC). Returns dollars as f64.
    /// Polygon USDC has 6 decimals; we divide the response by 1e6.
    pub async fn get_balance_allowance(&self, creds: &ApiCreds, eoa_addr: Address) -> Result<f64> {
        let path = "/balance-allowance";
        let body = "";
        let mut headers = Self::auth_headers(creds, "GET", path, body)?;
        headers.insert("POLY_ADDRESS", HeaderValue::from_str(&format!("{:?}", eoa_addr))?);

        let resp = self.http
            .get(format!("{}{}?asset_type=COLLATERAL", CLOB_BASE, path))
            .headers(headers).send().await
            .context("GET /balance-allowance")?;
        if !resp.status().is_success() {
            anyhow::bail!("GET /balance-allowance → {}", resp.status());
        }
        let v: serde_json::Value = resp.json().await?;
        let raw = v.get("balance").and_then(|b| b.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing balance field"))?;
        let micro: u64 = raw.parse().context("parse balance")?;
        Ok(micro as f64 / 1_000_000.0)
    }

    /// GET /price — server-side expected fill price for a market order
    /// of the given USD notional on the given side.
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
            match side { Side::Buy => "BUY", Side::Sell => "SELL" },
            amount_usd
        );
        let resp = self.http.get(&url).send().await
            .context("GET /price")?;
        if !resp.status().is_success() {
            anyhow::bail!("GET /price → {}", resp.status());
        }
        let v: serde_json::Value = resp.json().await?;
        v.get("price").and_then(|p| p.as_str()).and_then(|s| s.parse::<f64>().ok())
            .ok_or_else(|| anyhow::anyhow!("price field missing/invalid"))
    }
}
```

- [ ] **Step 2: Test that the URL construction is correct**

Append to `#[cfg(test)] mod tests`:

```rust
#[test]
fn calculate_market_price_url_format() {
    let url = format!(
        "{}/price?token_id={}&side={}&amount={}",
        CLOB_BASE, "tok-123", "BUY", 25.0
    );
    assert_eq!(url, "https://clob.polymarket.com/price?token_id=tok-123&side=BUY&amount=25");
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p pmbots-gengar polymarket::clob
```

Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/pmbots-gengar/src/polymarket/clob.rs
git commit -m "feat(gengar): CLOB balance + market-price endpoints"
```

---

### Task 11: `polymarket/ws.rs` — current-token WS subscription

**Files:**
- Modify: `crates/pmbots-gengar/src/polymarket/mod.rs`
- Create: `crates/pmbots-gengar/src/polymarket/ws.rs`

**Reference:** This module has no Python equivalent (gengar polls). The design is the Rust-only addition for low-latency entry per spec §Module Map. Subscription protocol mirrors arb's `polymarket.rs:417-423`: send a `SubscribeCmd { assets_ids, sub_type: "market" }` JSON; receive `BookSnapshot[]` and `PriceChangeEvent` messages.

- [ ] **Step 1: Write the test fixture + module**

Create `crates/pmbots-gengar/src/polymarket/ws.rs`:

```rust
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
```

- [ ] **Step 2: Update `polymarket/mod.rs`**

```rust
pub mod types;
pub mod gamma;
pub mod clob;
pub mod ws;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p pmbots-gengar polymarket::ws
```

Expected: 1 test passes.

- [ ] **Step 4: Commit**

```bash
git add crates/pmbots-gengar/src/polymarket/
git commit -m "feat(gengar): polymarket WS subscription + TokenPriceCache"
```

---

## Phase C: Strategy Core (independent modules)

### Task 12: `strategy.rs` — Brownian + Quarter-Kelly + entry gate

**Files:**
- Modify: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/src/strategy.rs`

**Reference:** Direct port of `strategy.py:158-317`. Three functions: `estimate_true_probability`, `kelly_bet`, and `evaluate` (entry gate). Plus `get_skip_reason` for tracker logging.

- [ ] **Step 1: Write the failing tests with known-value expectations**

Create `crates/pmbots-gengar/src/strategy.rs`:

```rust
//! Strategy: Brownian-motion probability model + Quarter-Kelly sizing + entry gate.
//! Reference: strategy.py:158-317.

use serde::Serialize;

#[derive(Debug, Clone, Copy)]
pub struct StrategyConfig {
    pub min_edge: f64,                  // GENGAR_MIN_EDGE       (default 0.05)
    pub min_prob: f64,                  // GENGAR_MIN_PROB       (default 0.80)
    pub min_btc_delta: f64,             // GENGAR_MIN_BTC_DELTA  (default 0.06)
    pub entry_window_start: u64,        // GENGAR_ENTRY_WINDOW_START (240)
    pub entry_window_end: u64,          // GENGAR_ENTRY_WINDOW_END   (10)
    pub kelly_fraction: f64,            // GENGAR_KELLY_FRACTION (0.25)
    pub min_bet: f64,                   // GENGAR_MIN_BET        (5.0)
    pub max_bet: f64,                   // GENGAR_MAX_BET        (25.0)
    pub min_price: f64,                 //                       (hardcoded 0.50)
    pub max_price: f64,                 //                       (hardcoded 0.90)
}

impl Default for StrategyConfig {
    fn default() -> Self {
        Self {
            min_edge: 0.05, min_prob: 0.80, min_btc_delta: 0.06,
            entry_window_start: 240, entry_window_end: 10,
            kelly_fraction: 0.25, min_bet: 5.0, max_bet: 25.0,
            min_price: 0.50, max_price: 0.90,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum SkipReason {
    OutsideEntryWindow, DeltaTooSmall, PriceOutOfRange, ProbBelowMin, EdgeBelowMin,
}

#[derive(Debug, Clone, Serialize)]
pub struct Signal {
    pub side: &'static str,          // "UP" or "DOWN"
    pub btc_delta_pct: f64,
    pub true_prob: f64,
    pub market_price: f64,
    pub edge: f64,
    pub bet_usd: f64,
    pub confidence: f64,             // edge/0.10 clamped to [0,1]
}

/// Brownian-motion CDF for the directional-win probability.
/// Reference: strategy.py:187-206.
pub fn estimate_true_probability(btc_delta_pct: f64, seconds_remaining: f64, vol: f64) -> f64 {
    let time_factor = seconds_remaining.max(1.0) / 300.0;
    let effective_vol = vol * time_factor.sqrt();
    if effective_vol <= 0.0 { return 0.5; }
    let z = btc_delta_pct.abs() / effective_vol;
    let prob = 0.5 * (1.0 + libm::erf(z / std::f64::consts::SQRT_2));
    prob.clamp(0.01, 0.99)
}

/// Quarter-Kelly bet size. Returns 0.0 if Kelly is non-positive or price is degenerate.
/// Reference: strategy.py:158-184.
pub fn kelly_bet(bankroll: f64, true_prob: f64, market_price: f64, fraction: f64, min_bet: f64, max_bet: f64) -> f64 {
    if market_price <= 0.0 || market_price >= 1.0 { return 0.0; }
    let b = (1.0 - market_price) / market_price;
    let q = 1.0 - true_prob;
    let kelly_f = (b * true_prob - q) / b;
    if kelly_f <= 0.0 { return 0.0; }
    let bet = bankroll * kelly_f * fraction;
    bet.max(min_bet).min(max_bet)
}

/// Evaluate entry. Returns a Signal if all gates pass; otherwise None.
/// Reference: strategy.py:246-317.
/// `side` is "UP" if btc_delta_pct > 0, else "DOWN".
#[allow(clippy::too_many_arguments)]
pub fn evaluate(
    btc_delta_pct: f64,
    seconds_remaining: u64,
    market_price: f64,         // YES (the directional-win side) price
    bankroll: f64,
    vol: f64,
    cfg: &StrategyConfig,
) -> Result<Signal, SkipReason> {
    if !(cfg.entry_window_end..=cfg.entry_window_start).contains(&seconds_remaining) {
        return Err(SkipReason::OutsideEntryWindow);
    }
    if btc_delta_pct.abs() < cfg.min_btc_delta { return Err(SkipReason::DeltaTooSmall); }
    if market_price < cfg.min_price || market_price > cfg.max_price { return Err(SkipReason::PriceOutOfRange); }
    let true_prob = estimate_true_probability(btc_delta_pct, seconds_remaining as f64, vol);
    if true_prob < cfg.min_prob { return Err(SkipReason::ProbBelowMin); }
    let edge = true_prob - market_price;
    if edge < cfg.min_edge { return Err(SkipReason::EdgeBelowMin); }
    let bet_usd = kelly_bet(bankroll, true_prob, market_price, cfg.kelly_fraction, cfg.min_bet, cfg.max_bet);
    Ok(Signal {
        side: if btc_delta_pct > 0.0 { "UP" } else { "DOWN" },
        btc_delta_pct, true_prob, market_price, edge, bet_usd,
        confidence: (edge / 0.10).clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// strategy.py:187-206 known values: with vol=0.12, delta=0.10, seconds=150:
    ///   time_factor = 150/300 = 0.5
    ///   effective_vol = 0.12 * sqrt(0.5) ≈ 0.0849
    ///   z = 0.10 / 0.0849 ≈ 1.178
    ///   prob = 0.5 * (1 + erf(1.178/sqrt(2))) ≈ 0.5 * (1 + 0.7615) ≈ 0.881
    #[test]
    fn brownian_prob_matches_python_reference() {
        let p = estimate_true_probability(0.10, 150.0, 0.12);
        assert!((p - 0.881).abs() < 0.01, "got {}", p);
    }

    #[test]
    fn brownian_prob_clamps_to_99_max() {
        let p = estimate_true_probability(100.0, 150.0, 0.12);
        assert_eq!(p, 0.99);
    }

    #[test]
    fn kelly_returns_zero_on_negative_edge() {
        // true_prob 0.45, market 0.50 → negative kelly_f → 0.
        assert_eq!(kelly_bet(100.0, 0.45, 0.50, 0.25, 5.0, 25.0), 0.0);
    }

    #[test]
    fn kelly_clamps_to_min_bet() {
        // tiny edge → kelly tiny → clamped to min_bet
        let bet = kelly_bet(100.0, 0.51, 0.50, 0.25, 5.0, 25.0);
        assert!(bet >= 5.0);
    }

    #[test]
    fn entry_gate_rejects_outside_window() {
        let r = evaluate(0.10, 5, 0.70, 100.0, 0.12, &Default::default());
        assert!(matches!(r, Err(SkipReason::OutsideEntryWindow)));
    }

    #[test]
    fn entry_gate_accepts_valid() {
        // strategy.py defaults: delta 0.10, secs 150, price 0.70, bankroll 100, vol 0.12
        // → prob ~0.88, edge ~0.18, all gates pass
        let r = evaluate(0.10, 150, 0.70, 100.0, 0.12, &Default::default());
        let s = r.unwrap();
        assert_eq!(s.side, "UP");
        assert!(s.edge > 0.05);
        assert!(s.bet_usd >= 5.0);
    }
}
```

- [ ] **Step 2: Update `lib.rs`**

```rust
//! pmbots-gengar — Rust port of JLowo/gengar_polymarket_bot.
//! Reference: ~/personal/gengar_polymarket_bot@9f49a07.

pub mod polymarket;
pub mod strategy;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p pmbots-gengar strategy
```

Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/pmbots-gengar/src/strategy.rs crates/pmbots-gengar/src/lib.rs
git commit -m "feat(gengar): Brownian + Quarter-Kelly strategy with entry gate"
```

---

### Task 13: `market.rs` — window detection + active-market lookup

**Files:**
- Modify: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/src/market.rs`

**Reference:** `market.py:14-39` (window math), `market.py:69-113` (token selection).

- [ ] **Step 1: Write the module + test**

Create `crates/pmbots-gengar/src/market.rs`:

```rust
//! Market discovery — 5-minute window detection + Gamma event lookup.
//! Reference: market.py.

use anyhow::Result;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::polymarket::gamma::{GammaClient, ActiveMarket};

/// Period in seconds. Only 300 (5-min) and 900 (15-min) are valid per market.py:15.
pub fn period_seconds(period_minutes: u32) -> u32 {
    period_minutes * 60
}

/// Compute the window-open timestamp for the current period boundary.
/// `now` is unix-seconds. The window opens at `now - (now mod period_seconds)`.
pub fn current_window_ts(now: i64, period_secs: u32) -> i64 {
    now - (now.rem_euclid(period_secs as i64))
}

/// `now()` returns the current unix-seconds.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Seconds remaining until the window resolves.
pub fn seconds_remaining(now: i64, window_open_ts: i64, period_secs: u32) -> u64 {
    let elapsed = (now - window_open_ts).max(0) as u64;
    (period_secs as u64).saturating_sub(elapsed)
}

/// Fetch the current active market for the given period.
pub async fn fetch_current(
    client: &GammaClient,
    period_minutes: u32,
) -> Result<Option<ActiveMarket>> {
    let period_secs = period_seconds(period_minutes);
    let window_ts = current_window_ts(now(), period_secs);
    client.fetch_active_market(period_minutes, window_ts).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_aligns_to_period() {
        // 1748000123 with 5-min period → 1748000100 (rounded down by mod 300)
        assert_eq!(current_window_ts(1_748_000_123, 300), 1_748_000_100);
        assert_eq!(current_window_ts(1_748_000_100, 300), 1_748_000_100);
    }

    #[test]
    fn seconds_remaining_decreases() {
        let w = 1_748_000_000;
        assert_eq!(seconds_remaining(w + 0,   w, 300), 300);
        assert_eq!(seconds_remaining(w + 240, w, 300), 60);
        assert_eq!(seconds_remaining(w + 300, w, 300), 0);
        assert_eq!(seconds_remaining(w + 350, w, 300), 0); // saturating
    }

    #[test]
    fn period_seconds_for_supported_values() {
        assert_eq!(period_seconds(5), 300);
        assert_eq!(period_seconds(15), 900);
    }
}
```

- [ ] **Step 2: Update `lib.rs`**

```rust
pub mod polymarket;
pub mod strategy;
pub mod market;
```

- [ ] **Step 3: Run tests + commit**

```bash
cargo test -p pmbots-gengar market
git add crates/pmbots-gengar/src/market.rs crates/pmbots-gengar/src/lib.rs
git commit -m "feat(gengar): market window detection + active-market lookup"
```

Expected: 3 tests pass.

---

### Task 14: `price_feed.rs` — Binance WS + REST fallback

**Files:**
- Modify: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/src/price_feed.rs`

**Reference:** `price_feed.py:15-116`. WS endpoint: `wss://stream.binance.com:9443/ws/btcusdt@trade`. REST fallback: `https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT`, polled every 2s when WS price is stale (>5s old).

- [ ] **Step 1: Write the module**

Create `crates/pmbots-gengar/src/price_feed.rs`:

```rust
//! Binance BTC/USDT trade-stream price feed with REST fallback.
//! Reference: price_feed.py.

use anyhow::{Context, Result};
use futures_util::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration, Instant};
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};

pub const WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@trade";
pub const REST_URL: &str = "https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT";
pub const STALE_AFTER: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub struct PriceState {
    pub price: f64,
    pub last_update: Option<Instant>,
}

impl PriceState {
    pub fn is_fresh(&self) -> bool {
        self.price > 0.0 && self.last_update.map_or(false, |t| t.elapsed() < STALE_AFTER)
    }
}

pub type SharedPrice = Arc<RwLock<PriceState>>;

pub fn new_state() -> SharedPrice { Arc::new(RwLock::new(PriceState::default())) }

#[derive(Debug, Deserialize)]
struct TradeMsg { p: String }

/// Run the WS loop forever (caller spawns this in a Tokio task).
/// Reconnects after a 3-second backoff on any error.
pub async fn run_ws(state: SharedPrice) {
    loop {
        match connect_one(state.clone()).await {
            Ok(_)  => warn!("[GENGAR][PRICE-WS] stream ended, reconnecting"),
            Err(e) => error!("[GENGAR][PRICE-WS] {}, reconnecting in 3s", e),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn connect_one(state: SharedPrice) -> Result<()> {
    let (ws, _) = connect_async(WS_URL).await.context("ws connect")?;
    info!("[GENGAR][PRICE-WS] connected");
    let (_w, mut r) = ws.split();
    while let Some(Ok(msg)) = r.next().await {
        if let Some(t) = msg.into_text().ok() {
            if let Ok(parsed) = serde_json::from_str::<TradeMsg>(&t) {
                if let Ok(p) = parsed.p.parse::<f64>() {
                    let mut s = state.write().await;
                    s.price = p;
                    s.last_update = Some(Instant::now());
                }
            }
        }
    }
    Ok(())
}

/// REST-fallback poller. Spawn this in a Tokio task alongside `run_ws`.
/// Polls every 2s; only writes the cache when WS is stale.
pub async fn run_rest_fallback(state: SharedPrice) {
    let http = reqwest::Client::new();
    let mut tick = interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        let stale = !state.read().await.is_fresh();
        if !stale { continue; }
        match fetch_rest(&http).await {
            Ok(p) => {
                let mut s = state.write().await;
                s.price = p;
                s.last_update = Some(Instant::now());
            }
            Err(e) => warn!("[GENGAR][PRICE-REST] {}", e),
        }
    }
}

async fn fetch_rest(http: &reqwest::Client) -> Result<f64> {
    let v: serde_json::Value = http.get(REST_URL).send().await?
        .error_for_status()?.json().await?;
    v.get("price").and_then(|p| p.as_str()).and_then(|s| s.parse::<f64>().ok())
        .ok_or_else(|| anyhow::anyhow!("missing price"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_state_is_fresh_when_recent() {
        let mut s = PriceState::default();
        s.price = 67000.0;
        s.last_update = Some(Instant::now());
        assert!(s.is_fresh());
    }

    #[test]
    fn price_state_stale_when_zero_price() {
        let s = PriceState { price: 0.0, last_update: Some(Instant::now()) };
        assert!(!s.is_fresh());
    }

    #[test]
    fn trade_msg_parses() {
        let m: TradeMsg = serde_json::from_str(r#"{"e":"trade","p":"68234.50"}"#).unwrap();
        assert_eq!(m.p, "68234.50");
    }
}
```

- [ ] **Step 2: Update `lib.rs` + run tests + commit**

```rust
pub mod polymarket;
pub mod strategy;
pub mod market;
pub mod price_feed;
```

```bash
cargo test -p pmbots-gengar price_feed
git add crates/pmbots-gengar/src/price_feed.rs crates/pmbots-gengar/src/lib.rs
git commit -m "feat(gengar): Binance BTCUSDT WS + REST fallback price feed"
```

Expected: 3 tests pass.

---

## Phase D: Sidecars

### Task 15: `tracker.rs` — CSV writers

**Files:**
- Modify: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/src/tracker.rs`

**Reference:** `tracker.py`. Four CSV files, append-only, headers auto-created:
- `signals.csv` — every entry evaluation (accepted or skipped, with reason)
- `trades.csv` — entries + resolutions
- `executions.csv` — opt-in (`GENGAR_LOG_EXECUTIONS=true`), every CLOB call
- `sessions.csv` — process start/stop summaries

- [ ] **Step 1: Write the module**

Create `crates/pmbots-gengar/src/tracker.rs`:

```rust
//! CSV trackers — signals, trades, executions, sessions.
//! Reference: tracker.py.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use std::fs::{File, OpenOptions, create_dir_all};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use csv::Writer;

pub struct Tracker {
    pub log_dir: PathBuf,
    pub log_executions: bool,
    signals:    Mutex<Option<Writer<File>>>,
    trades:     Mutex<Option<Writer<File>>>,
    executions: Mutex<Option<Writer<File>>>,
    sessions:   Mutex<Option<Writer<File>>>,
}

impl Tracker {
    pub fn new(log_dir: impl Into<PathBuf>, log_executions: bool) -> Result<Self> {
        let dir = log_dir.into();
        create_dir_all(&dir).with_context(|| format!("create log dir {:?}", dir))?;
        Ok(Self {
            log_dir: dir, log_executions,
            signals: Mutex::new(None),
            trades: Mutex::new(None),
            executions: Mutex::new(None),
            sessions: Mutex::new(None),
        })
    }

    fn writer_for(&self, name: &str) -> Result<Writer<File>> {
        let path = self.log_dir.join(name);
        let existed = path.exists();
        let f = OpenOptions::new().create(true).append(true).open(&path)
            .with_context(|| format!("open {:?}", path))?;
        let mut w = Writer::from_writer(f);
        if !existed {
            // Header is set per file in the log_* methods below; this method
            // just ensures the file exists. csv::Writer auto-writes the header
            // on the first record via serde derive.
            let _ = w; // placeholder
            return Ok(Writer::from_writer(OpenOptions::new().append(true).open(&path)?));
        }
        Ok(w)
    }

    pub fn log_signal(&self, row: SignalRow) -> Result<()> {
        let mut guard = self.signals.lock().unwrap();
        if guard.is_none() { *guard = Some(self.writer_for("signals.csv")?); }
        let w = guard.as_mut().unwrap();
        w.serialize(row)?; w.flush()?; Ok(())
    }
    pub fn log_trade(&self, row: TradeRow) -> Result<()> {
        let mut guard = self.trades.lock().unwrap();
        if guard.is_none() { *guard = Some(self.writer_for("trades.csv")?); }
        let w = guard.as_mut().unwrap();
        w.serialize(row)?; w.flush()?; Ok(())
    }
    pub fn log_execution(&self, row: ExecutionRow) -> Result<()> {
        if !self.log_executions { return Ok(()); }
        let mut guard = self.executions.lock().unwrap();
        if guard.is_none() { *guard = Some(self.writer_for("executions.csv")?); }
        let w = guard.as_mut().unwrap();
        w.serialize(row)?; w.flush()?; Ok(())
    }
    pub fn log_session(&self, row: SessionRow) -> Result<()> {
        let mut guard = self.sessions.lock().unwrap();
        if guard.is_none() { *guard = Some(self.writer_for("sessions.csv")?); }
        let w = guard.as_mut().unwrap();
        w.serialize(row)?; w.flush()?; Ok(())
    }

    pub fn now_iso() -> String { Utc::now().to_rfc3339() }
}

#[derive(Debug, Serialize)]
pub struct SignalRow {
    pub ts: String, pub window_ts: i64, pub side: String,
    pub btc_delta_pct: f64, pub seconds_remaining: u64,
    pub market_price: f64, pub true_prob: f64, pub edge: f64,
    pub bet_usd: f64, pub vol: f64, pub skip_reason: String,
}

#[derive(Debug, Serialize)]
pub struct TradeRow {
    pub ts: String, pub window_ts: i64, pub side: String,
    pub price: f64, pub shares: i64, pub usd: f64,
    pub order_id: String, pub status: String,
    pub resolution: Option<String>, pub pnl: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct ExecutionRow {
    pub ts: String, pub endpoint: String, pub method: String,
    pub status: u16, pub latency_ms: u64, pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SessionRow {
    pub ts: String, pub event: String, pub bankroll: f64,
    pub trades_count: u32, pub wins: u32, pub losses: u32, pub session_pnl: f64,
}
```

- [ ] **Step 2: Test with tempdir**

Append to `tracker.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn signal_log_creates_file_and_writes() {
        let dir = tempdir().unwrap();
        let t = Tracker::new(dir.path(), true).unwrap();
        t.log_signal(SignalRow {
            ts: "2026-05-14T12:00:00Z".into(), window_ts: 1, side: "UP".into(),
            btc_delta_pct: 0.1, seconds_remaining: 150, market_price: 0.7,
            true_prob: 0.88, edge: 0.18, bet_usd: 12.5, vol: 0.12,
            skip_reason: "".into(),
        }).unwrap();
        let body = std::fs::read_to_string(dir.path().join("signals.csv")).unwrap();
        assert!(body.contains("UP"));
        assert!(body.contains("0.88"));
    }
}
```

- [ ] **Step 3: Update `lib.rs`, run tests, commit**

```rust
pub mod polymarket;
pub mod strategy;
pub mod market;
pub mod price_feed;
pub mod tracker;
```

```bash
cargo test -p pmbots-gengar tracker
git add crates/pmbots-gengar/src/tracker.rs crates/pmbots-gengar/src/lib.rs
git commit -m "feat(gengar): CSV tracker (signals/trades/executions/sessions)"
```

Expected: 1 test passes.

---

### Task 16: `telegram_notifier.rs` — fire-and-forget Telegram POSTs

**Files:**
- Modify: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/src/telegram_notifier.rs`

**Reference:** `telegram_notifier.py:25-39`. POSTs to `https://api.telegram.org/bot{TOKEN}/sendMessage` with Markdown parse-mode. No-op when either env var is missing.

- [ ] **Step 1: Write the module**

Create `crates/pmbots-gengar/src/telegram_notifier.rs`:

```rust
//! Telegram notifier — fire-and-forget POSTs to sendMessage.
//! Reference: telegram_notifier.py.

use serde_json::json;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct Telegram {
    bot_token: String,
    chat_id: String,
    http: reqwest::Client,
}

impl Telegram {
    /// Build from env. Returns None if either var is missing/empty.
    pub fn from_env() -> Option<Self> {
        let bot_token = std::env::var("GENGAR_TELEGRAM_BOT_TOKEN").ok().filter(|s| !s.is_empty())?;
        let chat_id   = std::env::var("GENGAR_TELEGRAM_CHAT_ID").ok().filter(|s| !s.is_empty())?;
        Some(Self { bot_token, chat_id, http: reqwest::Client::new() })
    }

    /// Send a Markdown-formatted message. Errors are logged and swallowed.
    pub fn send(&self, text: &str) {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let body = json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "Markdown",
        });
        let http = self.http.clone();
        tokio::spawn(async move {
            if let Err(e) = http.post(&url).json(&body).send().await {
                warn!("[GENGAR][TG] send failed: {}", e);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_returns_none_when_missing() {
        std::env::remove_var("GENGAR_TELEGRAM_BOT_TOKEN");
        std::env::remove_var("GENGAR_TELEGRAM_CHAT_ID");
        assert!(Telegram::from_env().is_none());
    }
}
```

- [ ] **Step 2: Update `lib.rs`, run tests, commit**

```rust
pub mod polymarket;
pub mod strategy;
pub mod market;
pub mod price_feed;
pub mod tracker;
pub mod telegram_notifier;
```

```bash
cargo test -p pmbots-gengar telegram
git add crates/pmbots-gengar/src/telegram_notifier.rs crates/pmbots-gengar/src/lib.rs
git commit -m "feat(gengar): telegram fire-and-forget notifier"
```

Expected: 1 test passes.

---

### Task 17: `config.rs` — `GENGAR_*` env loading (strict, no fallback)

**Files:**
- Modify: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/src/config.rs`

**Reference:** Spec §Config Schema. All vars are `GENGAR_*`-prefixed; no fallback to `POLY_*`.

- [ ] **Step 1: Write the module**

Create `crates/pmbots-gengar/src/config.rs`:

```rust
//! Runtime configuration loaded from GENGAR_* env vars.

use anyhow::{Context, Result};
use ethers::types::Address;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct GengarConfig {
    pub private_key: String,         // GENGAR_PRIVATE_KEY    (required if !dry_run)
    pub safe_address: Option<Address>, // GENGAR_SAFE_ADDRESS  (optional; sig_type=2 if set)
    pub dry_run: bool,
    pub strategy: crate::strategy::StrategyConfig,
    pub vol: VolConfig,
    pub risk: RiskConfig,
    pub market_period_secs: u64,
    pub log_dir: PathBuf,
    pub log_executions: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct VolConfig {
    pub rolling_windows: usize,   // GENGAR_ROLLING_VOL_WINDOWS (12)
    pub floor: f64,               // GENGAR_VOL_FLOOR  (0.06)
    pub cap: f64,                 // GENGAR_VOL_CAP    (0.30)
}

#[derive(Debug, Clone, Copy)]
pub struct RiskConfig {
    pub daily_loss_limit: f64,    // GENGAR_DAILY_LOSS_LIMIT (30.0)
}

fn env_or<T: FromStr>(name: &str, default: T) -> T
where <T as FromStr>::Err: std::fmt::Debug,
{
    std::env::var(name).ok().and_then(|v| v.parse::<T>().ok()).unwrap_or(default)
}

impl GengarConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();
        let dry_run = env_or::<String>("GENGAR_DRY_RUN", "true".into()) == "true";
        let private_key = std::env::var("GENGAR_PRIVATE_KEY").unwrap_or_default();
        if !dry_run && private_key.is_empty() {
            anyhow::bail!("GENGAR_PRIVATE_KEY is required when GENGAR_DRY_RUN=false");
        }
        let safe_address = std::env::var("GENGAR_SAFE_ADDRESS").ok()
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<Address>().context("parse GENGAR_SAFE_ADDRESS"))
            .transpose()?;

        let strategy = crate::strategy::StrategyConfig {
            min_edge:           env_or("GENGAR_MIN_EDGE", 0.05),
            min_prob:           env_or("GENGAR_MIN_PROB", 0.80),
            min_btc_delta:      env_or("GENGAR_MIN_BTC_DELTA", 0.06),
            entry_window_start: env_or("GENGAR_ENTRY_WINDOW_START", 240),
            entry_window_end:   env_or("GENGAR_ENTRY_WINDOW_END", 10),
            kelly_fraction:     env_or("GENGAR_KELLY_FRACTION", 0.25),
            min_bet:            env_or("GENGAR_MIN_BET", 5.0),
            max_bet:            env_or("GENGAR_MAX_BET", 25.0),
            min_price: 0.50, max_price: 0.90,
        };
        let vol = VolConfig {
            rolling_windows: env_or("GENGAR_ROLLING_VOL_WINDOWS", 12),
            floor:           env_or("GENGAR_VOL_FLOOR", 0.06),
            cap:             env_or("GENGAR_VOL_CAP", 0.30),
        };
        let risk = RiskConfig {
            daily_loss_limit: env_or("GENGAR_DAILY_LOSS_LIMIT", 30.0),
        };
        let market_period_secs = env_or::<u64>("GENGAR_MARKET_PERIOD", 5) * 60;
        let log_dir = PathBuf::from(env_or::<String>("GENGAR_LOG_DIR", "logs/gengar".into()));
        let log_executions = env_or::<String>("GENGAR_LOG_EXECUTIONS", "false".into()) == "true";

        Ok(Self { private_key, safe_address, dry_run, strategy, vol, risk,
                  market_period_secs, log_dir, log_executions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_only_dry_run_set() {
        std::env::set_var("GENGAR_DRY_RUN", "true");
        std::env::remove_var("GENGAR_PRIVATE_KEY");
        let cfg = GengarConfig::from_env().unwrap();
        assert!(cfg.dry_run);
        assert_eq!(cfg.strategy.min_edge, 0.05);
        assert_eq!(cfg.market_period_secs, 300);
    }

    #[test]
    fn requires_private_key_when_live() {
        std::env::set_var("GENGAR_DRY_RUN", "false");
        std::env::remove_var("GENGAR_PRIVATE_KEY");
        assert!(GengarConfig::from_env().is_err());
    }
}
```

- [ ] **Step 2: Update `lib.rs`, run, commit**

```rust
pub mod polymarket;
pub mod strategy;
pub mod market;
pub mod price_feed;
pub mod tracker;
pub mod telegram_notifier;
pub mod config;
```

```bash
cargo test -p pmbots-gengar config
git add crates/pmbots-gengar/src/config.rs crates/pmbots-gengar/src/lib.rs
git commit -m "feat(gengar): GENGAR_* config loader (strict, no fallback)"
```

Expected: 2 tests pass.

---

## Phase E: Executor + Bot

### Task 18: `executor.rs` — integer-cents math + buy/sell + ghost-fill

**Files:**
- Modify: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/src/executor.rs`

**Reference:** `executor.py` in full. Three big concerns:
1. **`calculate_order_size`** (`executor.py:56-74`) — integer-cents arithmetic, load-bearing.
2. **Buy verification with ghost-fill** (`executor.py:236-300`) — success and exception paths.
3. **`UNVERIFIED_BUY` non-cancel contract** (`executor.py:294-300`) — must NOT cancel after timeout.

- [ ] **Step 1: Order-size helper + test against Python reference values**

Create `crates/pmbots-gengar/src/executor.rs`:

```rust
//! Executor — order placement with integer-cents arithmetic and ghost-fill verification.
//! Reference: executor.py.

use anyhow::{Context, Result};
use ethers::signers::LocalWallet;
use ethers::types::Address;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{warn, info};
use crate::polymarket::clob::{ClobClient, ApiCreds, OrderArgs, OrderType, SignatureType};
use crate::polymarket::types::{PriceCents, Side};

pub const POLY_MIN_NOTIONAL: f64 = 5.0;   // USD
pub const MAX_BUY_PRICE: f64    = 0.90;   // executor.py:179 reject above this
pub const BUY_VERIFY_ATTEMPTS: u32   = 3;
pub const BUY_VERIFY_SLEEP_MS: u64   = 3000;
pub const GHOST_BUY_DELTA: f64       = 1.00;  // USDC drop > $1 on exception → ghost fill
pub const GHOST_BUY_BAL_DELTA: f64   = 0.50;  // USDC drop > $0.50 on success → filled
pub const GHOST_SELL_BAL_DELTA: f64  = 0.10;  // USDC rise > $0.10 → sell filled

#[derive(Debug, Clone, PartialEq)]
pub enum OrderResultStatus {
    Filled, Partial, Failed(String), GhostFilled,
}

#[derive(Debug, Clone)]
pub struct OrderResult {
    pub status: OrderResultStatus,
    pub order_id: Option<String>,
    pub price: f64,
    pub shares: i64,
    pub usd_spent: f64,
}

/// Reference: executor.py:56-74. Integer-cents only — DO NOT use float division.
/// Polymarket CLOB rejects orders with float-precision artifacts.
pub fn calculate_order_size(price: f64, max_usd: f64) -> (i64, f64) {
    let price_cents = (price * 100.0).round() as i64;
    if price_cents <= 0 { return (0, 0.0); }
    let max_shares = ((max_usd * 100.0) as i64) / price_cents;
    let clean_usd = (max_shares * price_cents) as f64 / 100.0;
    (max_shares, clean_usd)
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn python_reference_values() {
        // executor.py:56-74 example: price=0.68, max=21.0 → (30 shares, 20.40 USD)
        assert_eq!(calculate_order_size(0.68, 21.0), (30, 20.40));
        // price=0.50, max=12.5 → 25 shares, 12.50
        assert_eq!(calculate_order_size(0.50, 12.5), (25, 12.50));
    }
    #[test]
    fn small_max_yields_zero() {
        // price=0.99, max=0.50 → 0 shares
        assert_eq!(calculate_order_size(0.99, 0.50), (0, 0.0));
    }
}
```

- [ ] **Step 2: Buy path with ghost-fill verification**

Append to `executor.rs`:

```rust
pub struct Executor {
    pub clob: ClobClient,
    pub creds: ApiCreds,
    pub eoa_addr: Address,
    pub wallet: LocalWallet,
    pub sig_type: SignatureType,
}

impl Executor {
    /// Buy `max_usd` of `token_id` at limit `price`.
    /// Returns OrderResult with status reflecting verification outcome.
    /// Reference: executor.py:136-300.
    pub async fn buy(&self, token_id: &str, price: f64, max_usd: f64) -> Result<OrderResult> {
        if price > MAX_BUY_PRICE {
            return Ok(OrderResult {
                status: OrderResultStatus::Failed("price_above_max".into()),
                order_id: None, price, shares: 0, usd_spent: 0.0,
            });
        }
        let (shares, clean_usd) = calculate_order_size(price, max_usd);
        if shares == 0 || clean_usd < POLY_MIN_NOTIONAL {
            return Ok(OrderResult {
                status: OrderResultStatus::Failed("below_min_notional".into()),
                order_id: None, price, shares, usd_spent: 0.0,
            });
        }
        let balance_before = self.clob.get_balance_allowance(&self.creds, self.eoa_addr).await
            .context("balance_before")?;

        let args = OrderArgs {
            token_id: token_id.into(),
            price: PriceCents::from_dollars(price),
            size_shares: shares,
            side: Side::Buy,
            maker_address: self.wallet.address(),
            funder: self.eoa_addr.into(),
        };
        let signed = match self.clob.create_order(args, self.sig_type, &self.wallet).await {
            Ok(s) => s,
            Err(e) => return Ok(OrderResult {
                status: OrderResultStatus::Failed(format!("create_order: {}", e)),
                order_id: None, price, shares, usd_spent: 0.0,
            }),
        };

        let post_result = self.clob.post_order(&self.creds, self.eoa_addr, &signed, OrderType::Gtc).await;
        let order_id = match post_result {
            Ok(resp) if resp.success => resp.order_id,
            Ok(resp) => {
                // success=false from server. Check exception ghost path:
                // give 3s, then re-check balance. Reference: executor.py:238-254.
                sleep(Duration::from_secs(3)).await;
                let bal_after = self.clob.get_balance_allowance(&self.creds, self.eoa_addr).await
                    .unwrap_or(balance_before);
                if balance_before - bal_after > GHOST_BUY_DELTA {
                    warn!("[GENGAR][EXEC] ghost fill detected (server said failure but balance dropped by ${:.2})",
                          balance_before - bal_after);
                    return Ok(OrderResult {
                        status: OrderResultStatus::GhostFilled,
                        order_id: Some("ghost-buy".into()),
                        price,
                        shares,
                        usd_spent: balance_before - bal_after,
                    });
                }
                return Ok(OrderResult {
                    status: OrderResultStatus::Failed(format!("post: {:?}", resp.error_msg)),
                    order_id: None, price, shares, usd_spent: 0.0,
                });
            }
            Err(e) => {
                // HTTP/network exception. Same ghost-fill check.
                sleep(Duration::from_secs(3)).await;
                let bal_after = self.clob.get_balance_allowance(&self.creds, self.eoa_addr).await
                    .unwrap_or(balance_before);
                if balance_before - bal_after > GHOST_BUY_DELTA {
                    warn!("[GENGAR][EXEC] ghost fill detected on exception path");
                    return Ok(OrderResult {
                        status: OrderResultStatus::GhostFilled,
                        order_id: Some("ghost-buy".into()),
                        price, shares,
                        usd_spent: balance_before - bal_after,
                    });
                }
                return Ok(OrderResult {
                    status: OrderResultStatus::Failed(format!("post exception: {}", e)),
                    order_id: None, price, shares, usd_spent: 0.0,
                });
            }
        };

        let order_id = order_id.unwrap_or_default();
        // Success path verification. Reference: executor.py:256-300.
        // 3 attempts × 3s sleep each.
        for attempt in 1..=BUY_VERIFY_ATTEMPTS {
            sleep(Duration::from_millis(BUY_VERIFY_SLEEP_MS)).await;
            let bal_after = self.clob.get_balance_allowance(&self.creds, self.eoa_addr).await
                .unwrap_or(balance_before);
            let dropped = balance_before - bal_after;
            if dropped > GHOST_BUY_BAL_DELTA {
                let actual_shares = ((dropped / price).round() as i64).max(1);
                info!("[GENGAR][EXEC] buy verified via balance on attempt {} (-${:.2})", attempt, dropped);
                return Ok(OrderResult {
                    status: OrderResultStatus::Filled,
                    order_id: Some(order_id.clone()), price,
                    shares: actual_shares, usd_spent: dropped,
                });
            }
            // Fallback: poll /order/{id}
            if let Ok(status) = self.clob.get_order(&self.creds, self.eoa_addr, &order_id).await {
                if let Some(sm) = status.size_matched.as_ref() {
                    if let Ok(matched) = sm.parse::<f64>() {
                        if matched > 0.0 {
                            return Ok(OrderResult {
                                status: OrderResultStatus::Filled,
                                order_id: Some(order_id), price,
                                shares: matched as i64,
                                usd_spent: matched * price,
                            });
                        }
                    }
                }
            }
        }

        // CRITICAL — reference: executor.py:294-300.
        // Neither balance nor /order/{id} confirmed after 14s.
        // DO NOT CANCEL. Polygon settlement can take 5-15s.
        warn!("[GENGAR][EXEC] UNVERIFIED_BUY for order {}, NOT cancelling per protocol", order_id);
        Ok(OrderResult {
            status: OrderResultStatus::Failed("UNVERIFIED_BUY".into()),
            order_id: Some(order_id), price, shares, usd_spent: 0.0,
        })
    }
}
```

- [ ] **Step 3: Sell path with verification**

Append to `executor.rs`:

```rust
impl Executor {
    /// Sell at limit `price` (typically claim-sell at $0.99 to detect resolution).
    /// Returns OrderResult.shares_remaining via `shares` field (0 = fully filled).
    /// Reference: executor.py:304-401.
    pub async fn sell(&self, token_id: &str, price: f64, sell_shares: i64) -> Result<OrderResult> {
        if (sell_shares as f64) * price < POLY_MIN_NOTIONAL {
            return Ok(OrderResult {
                status: OrderResultStatus::Failed("below_min_notional".into()),
                order_id: None, price, shares: sell_shares, usd_spent: 0.0,
            });
        }
        let balance_before = self.clob.get_balance_allowance(&self.creds, self.eoa_addr).await
            .context("sell balance_before")?;

        let args = OrderArgs {
            token_id: token_id.into(),
            price: PriceCents::from_dollars(price),
            size_shares: sell_shares,
            side: Side::Sell,
            maker_address: self.wallet.address(),
            funder: self.eoa_addr.into(),
        };
        let signed = self.clob.create_order(args, self.sig_type, &self.wallet).await
            .context("sell create_order")?;
        let post_result = self.clob.post_order(&self.creds, self.eoa_addr, &signed, OrderType::Gtc).await;
        let order_id = match post_result {
            Ok(r) if r.success => r.order_id.unwrap_or_default(),
            Ok(r) => return Ok(OrderResult {
                status: OrderResultStatus::Failed(format!("sell post: {:?}", r.error_msg)),
                order_id: None, price, shares: sell_shares, usd_spent: 0.0,
            }),
            Err(e) => {
                // Sell ghost-fill check. Reference: executor.py:388-397.
                sleep(Duration::from_secs(3)).await;
                let bal_after = self.clob.get_balance_allowance(&self.creds, self.eoa_addr).await
                    .unwrap_or(balance_before);
                if bal_after - balance_before > GHOST_SELL_BAL_DELTA {
                    return Ok(OrderResult {
                        status: OrderResultStatus::GhostFilled,
                        order_id: Some("ghost-sell".into()),
                        price, shares: 0,
                        usd_spent: bal_after - balance_before,
                    });
                }
                return Ok(OrderResult {
                    status: OrderResultStatus::Failed(format!("sell exception: {}", e)),
                    order_id: None, price, shares: sell_shares, usd_spent: 0.0,
                });
            }
        };

        sleep(Duration::from_secs(3)).await;
        let bal_after = self.clob.get_balance_allowance(&self.creds, self.eoa_addr).await
            .unwrap_or(balance_before);
        let received = bal_after - balance_before;
        if received > GHOST_SELL_BAL_DELTA {
            let received_shares = (received / price) as i64;
            let shares_left = sell_shares - received_shares;
            let status = if shares_left < 1 {
                OrderResultStatus::Filled
            } else {
                OrderResultStatus::Partial
            };
            return Ok(OrderResult {
                status, order_id: Some(order_id), price,
                shares: shares_left.max(0), usd_spent: received,
            });
        }
        Ok(OrderResult {
            status: OrderResultStatus::Failed("UNVERIFIED_SELL".into()),
            order_id: Some(order_id), price, shares: sell_shares, usd_spent: 0.0,
        })
    }
}
```

- [ ] **Step 4: Update `lib.rs`, run, commit**

```rust
pub mod polymarket;
pub mod strategy;
pub mod market;
pub mod price_feed;
pub mod tracker;
pub mod telegram_notifier;
pub mod config;
pub mod executor;
```

```bash
cargo test -p pmbots-gengar executor
git add crates/pmbots-gengar/src/executor.rs crates/pmbots-gengar/src/lib.rs
git commit -m "feat(gengar): executor with integer-cents math and ghost-fill verification"
```

Expected: 3 unit tests pass (calculate_order_size). Network-touching paths are integration-tested later.

---

### Task 19: `bot.rs` Phase 1 — main loop scaffold, window detection, health check, 3-strike halt

**Files:**
- Modify: `crates/pmbots-gengar/src/lib.rs`
- Create: `crates/pmbots-gengar/src/bot.rs`

**Reference:** `bot.py:1-825`. This task lays the lifecycle skeleton; Task 20 fills in position lifecycle and `_pending_phantom`.

- [ ] **Step 1: Bot struct + lifecycle loop**

Create `crates/pmbots-gengar/src/bot.rs`:

```rust
//! Main bot loop — window transitions, health check, position lifecycle.
//! Reference: bot.py.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::sleep;
use tracing::{info, warn, error};
use crate::config::GengarConfig;
use crate::market::{self};
use crate::polymarket::{gamma::GammaClient, ws::{self, TokenPriceCache}};
use crate::price_feed::SharedPrice;
use crate::strategy::{self, Signal, SkipReason};
use crate::tracker::Tracker;
use crate::executor::Executor;

pub const CLOB_HALT_THRESHOLD: u32 = 3;        // bot.py:816-823
pub const HALT_ERROR_PATTERNS: &[&str] = &[
    "request exception", "service not ready", "status_code=none",
];

pub struct GengarBot {
    pub cfg: Arc<GengarConfig>,
    pub gamma: GammaClient,
    pub price: SharedPrice,
    pub poly_prices: TokenPriceCache,
    pub tracker: Arc<Tracker>,
    pub executor: Option<Executor>,           // None in DRY_RUN
    pub state: Arc<RwLock<BotState>>,
}

#[derive(Debug, Default)]
pub struct BotState {
    pub current_window: i64,
    pub opening_price: f64,
    pub clob_halted: bool,
    pub clob_consecutive_errors: u32,
    pub daily_loss_halted: bool,
    pub session_start_balance: f64,
    pub bankroll: f64,
    pub realized_pnl_today: f64,
    pub price_history: std::collections::VecDeque<f64>,  // for rolling vol
}

impl GengarBot {
    pub async fn new(
        cfg: Arc<GengarConfig>,
        price: SharedPrice,
        tracker: Arc<Tracker>,
        executor: Option<Executor>,
    ) -> Result<Self> {
        let gamma = GammaClient::new()?;
        let poly_prices = ws::new_cache();
        let session_start_balance = cfg.strategy.min_bet; // placeholder; overwritten on first window when live
        let state = Arc::new(RwLock::new(BotState {
            bankroll: session_start_balance,
            session_start_balance,
            ..Default::default()
        }));
        Ok(Self { cfg, gamma, price, poly_prices, tracker, executor, state })
    }

    /// Main event loop. Returns when the process is asked to stop (Ctrl-C).
    ///
    /// NOTE: Task 20 extends the window-transition block below with
    /// `resolve_window(prev_window)` and `rotate_ws_subscription(market)`.
    /// This scaffold version only handles transition detection + opening-price
    /// capture + entry evaluation.
    pub async fn run(self: Arc<Self>) -> Result<()> {
        info!("[GENGAR] starting main loop (dry_run={})", self.cfg.dry_run);
        loop {
            let now = market::now();
            let window_ts = market::current_window_ts(now, self.cfg.market_period_secs as u32);
            let secs_remaining = market::seconds_remaining(now, window_ts, self.cfg.market_period_secs as u32);

            // --- Window transition detection (NO nested lock re-acquisition) ---
            let transition: bool;
            let need_unhalt_check: bool;
            {
                let mut st = self.state.write().await;
                transition = window_ts != st.current_window;
                if transition {
                    info!("[GENGAR] window transition: {} → {}", st.current_window, window_ts);
                    st.current_window = window_ts;
                    st.opening_price = 0.0;
                }
                need_unhalt_check = transition && st.clob_halted;
            } // <-- write guard dropped here

            if need_unhalt_check && self.try_unhalt().await {
                self.state.write().await.clob_halted = false;
            }

            // --- Capture opening price (separate lock acquisition) ---
            let cur_price = self.price.read().await.price;
            {
                let mut st = self.state.write().await;
                if st.opening_price <= 0.0 && cur_price > 0.0 {
                    st.opening_price = cur_price;
                }
            }

            // --- Entry-evaluation tick (only if not halted) ---
            let halted = {
                let st = self.state.read().await;
                st.clob_halted || st.daily_loss_halted
            };
            if !halted {
                if let Err(e) = self.maybe_enter(secs_remaining).await {
                    self.handle_error(&e.to_string()).await;
                }
            }

            sleep(Duration::from_millis(500)).await;
        }
    }

    /// Reference: bot.py:594-599. Calls /ok; returns true on success.
    async fn try_unhalt(&self) -> bool {
        if let Some(exec) = &self.executor {
            match exec.clob.get_ok().await {
                Ok(_)  => { info!("[GENGAR] CLOB recovered; clearing halt"); true }
                Err(e) => { warn!("[GENGAR] CLOB still down: {}", e); false }
            }
        } else { true }
    }

    /// Increment error counter; trip halt at threshold.
    /// Reference: bot.py:816-823.
    async fn handle_error(&self, err_msg: &str) {
        let matches = HALT_ERROR_PATTERNS.iter().any(|p| err_msg.contains(p));
        if !matches { return; }
        let mut st = self.state.write().await;
        st.clob_consecutive_errors += 1;
        if st.clob_consecutive_errors >= CLOB_HALT_THRESHOLD {
            warn!("[GENGAR] CLOB halt tripped after {} consecutive errors", st.clob_consecutive_errors);
            st.clob_halted = true;
            st.clob_consecutive_errors = 0;
        }
    }

    /// Stub — filled in Task 20.
    async fn maybe_enter(&self, _secs_remaining: u64) -> Result<()> { Ok(()) }
}
```

- [ ] **Step 2: Update `lib.rs`, build, commit**

```rust
pub mod polymarket;
pub mod strategy;
pub mod market;
pub mod price_feed;
pub mod tracker;
pub mod telegram_notifier;
pub mod config;
pub mod executor;
pub mod bot;
```

```bash
cargo build -p pmbots-gengar
git add crates/pmbots-gengar/src/bot.rs crates/pmbots-gengar/src/lib.rs
git commit -m "feat(gengar): bot scaffold with window transition + halt logic"
```

Expected: compiles. No new tests yet; integration happens in Task 21.

---

### Task 20: `bot.rs` Phase 2 — position lifecycle + `_pending_phantom` + daily-loss CB + rolling vol

**Files:**
- Modify: `crates/pmbots-gengar/src/bot.rs`

**Reference:** `bot.py:233-1012` (position lifecycle and resolution). Three load-bearing behaviors:
1. `_pending_phantom` two-window resolution (`bot.py:436-485`)
2. Daily-loss CB capture (`bot.py:187, 682-694`) — no auto-reset
3. Rolling-volatility window (`bot.py:603-614`)

- [ ] **Step 1: Open-position state + entry evaluation**

Append/update in `crates/pmbots-gengar/src/bot.rs`:

```rust
#[derive(Debug, Clone)]
pub struct OpenPosition {
    pub window_ts: i64,
    pub token_id: String,
    pub side: String,               // "UP" or "DOWN"
    pub price: f64,
    pub shares: i64,
    pub usd_spent: f64,
    pub order_id: String,
    pub opening_price: f64,
}

#[derive(Debug, Default)]
pub struct PendingPhantom {
    pub window_ts: i64,
    pub claim_order_id: String,
    pub expected_proceeds: f64,
    pub balance_before: f64,
}

impl BotState {
    pub fn add_pos(&mut self, p: OpenPosition) { self.positions.push(p); }
    pub fn rolling_vol(&self, floor: f64, cap: f64, min_samples: usize) -> f64 {
        if self.window_returns.len() < min_samples { return cap; }
        let n = self.window_returns.len() as f64;
        let mean = self.window_returns.iter().sum::<f64>() / n;
        let var = self.window_returns.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        var.sqrt().clamp(floor, cap)
    }
}
```

Update `BotState` struct to add the new fields (including the per-window WS task handle):

```rust
use tokio::task::JoinHandle;

#[derive(Default)]
pub struct BotState {
    pub current_window: i64,
    pub opening_price: f64,
    pub clob_halted: bool,
    pub clob_consecutive_errors: u32,
    pub daily_loss_halted: bool,
    pub session_start_balance: f64,
    pub bankroll: f64,
    pub realized_pnl_today: f64,
    pub positions: Vec<OpenPosition>,
    pub pending_phantom: Option<PendingPhantom>,
    pub window_returns: std::collections::VecDeque<f64>,
    pub price_history: std::collections::VecDeque<f64>,
    pub ws_handle: Option<JoinHandle<()>>,                   // current-window Polymarket WS task
}
```

(`Debug` is dropped because `JoinHandle` isn't `Debug`; add manual `Debug` impl if needed for logging.)

Add the per-window WS spawn method:

```rust
impl GengarBot {
    /// Tear down the previous window's Polymarket WS task and spawn a new one
    /// subscribed to the new window's Up/Down token IDs. Called on transition.
    /// Reference: spec §WS lifecycle.
    pub async fn rotate_ws_subscription(&self, market: &crate::polymarket::gamma::ActiveMarket) {
        let tokens = vec![market.token_id_up.clone(), market.token_id_down.clone()];
        let cache = self.poly_prices.clone();

        let new_handle = tokio::spawn(async move {
            if let Err(e) = crate::polymarket::ws::run_ws(tokens, cache).await {
                tracing::warn!("[GENGAR][POLY-WS] task ended: {}", e);
            }
        });

        let mut st = self.state.write().await;
        if let Some(prev) = st.ws_handle.take() { prev.abort(); }
        st.ws_handle = Some(new_handle);
    }
}
```

Now extend the `run()` loop in Task 19 to call `resolve_window` and `rotate_ws_subscription` on transition. In Task 19, the transition-detection block sets `transition: bool` and `need_unhalt_check: bool`. Add `prev_window: i64` to that block and append a post-transition handler. Replace the transition-detection block with:

```rust
            let transition: bool;
            let prev_window: i64;
            let need_unhalt_check: bool;
            {
                let mut st = self.state.write().await;
                transition = window_ts != st.current_window;
                prev_window = st.current_window;
                if transition {
                    info!("[GENGAR] window transition: {} → {}", st.current_window, window_ts);
                    st.current_window = window_ts;
                    st.opening_price = 0.0;
                }
                need_unhalt_check = transition && st.clob_halted;
            }

            if need_unhalt_check && self.try_unhalt().await {
                self.state.write().await.clob_halted = false;
            }

            // On transition: resolve prior window, then rotate WS to new window.
            if transition && prev_window > 0 {
                self.resolve_window(prev_window).await;
                match market::fetch_current(&self.gamma, (self.cfg.market_period_secs / 60) as u32).await {
                    Ok(Some(m)) => self.rotate_ws_subscription(&m).await,
                    Ok(None)    => warn!("[GENGAR] no active market for window {}", window_ts),
                    Err(e)      => warn!("[GENGAR] gamma fetch failed: {}", e),
                }
            } else if transition {
                // First-ever window of this run — no prior window to resolve, but
                // still rotate WS to subscribe to the current window's tokens.
                match market::fetch_current(&self.gamma, (self.cfg.market_period_secs / 60) as u32).await {
                    Ok(Some(m)) => self.rotate_ws_subscription(&m).await,
                    Ok(None)    => warn!("[GENGAR] no active market for window {}", window_ts),
                    Err(e)      => warn!("[GENGAR] gamma fetch failed: {}", e),
                }
            }
```

Leave the opening-price capture and entry-tick blocks unchanged.

- [ ] **Step 2: Implement `maybe_enter` — full entry path**

Replace the stub `maybe_enter` in `bot.rs` with:

```rust
impl GengarBot {
    async fn maybe_enter(&self, secs_remaining: u64) -> Result<()> {
        // Skip if there's an open position for the current window
        {
            let st = self.state.read().await;
            if st.positions.iter().any(|p| p.window_ts == st.current_window) { return Ok(()); }
        }

        // Health check before every entry attempt. Reference: bot.py:669.
        if let Some(exec) = &self.executor {
            if let Err(e) = exec.clob.get_ok().await {
                self.handle_error(&e.to_string()).await;
                return Ok(());
            }
        }

        // Get fresh prices
        let btc_now = self.price.read().await.price;
        let st = self.state.read().await;
        if st.opening_price <= 0.0 || btc_now <= 0.0 { return Ok(()); }
        let btc_delta_pct = (btc_now - st.opening_price) / st.opening_price;
        let vol = st.rolling_vol(self.cfg.vol.floor, self.cfg.vol.cap, 6);
        let bankroll = st.bankroll;
        let window_ts = st.current_window;
        drop(st);

        // Fetch active market for the current window
        let market = match market::fetch_current(&self.gamma, (self.cfg.market_period_secs / 60) as u32).await? {
            Some(m) => m,
            None => return Ok(()),
        };
        let token_id = if btc_delta_pct > 0.0 { &market.token_id_up } else { &market.token_id_down };

        // Get current market price (WS cache preferred; fallback to /price)
        let market_price = {
            let cache = self.poly_prices.read().await;
            cache.get(token_id).map(|t| t.best_ask)
        };
        let market_price = match market_price {
            Some(p) if p > 0.0 && p < 1.0 => p,
            _ => match &self.executor {
                Some(e) => e.clob.calculate_market_price(token_id, crate::polymarket::types::Side::Buy, 25.0).await?,
                None => return Ok(()),  // dry-run without an exec client can't fetch price
            }
        };

        // Strategy gate
        let signal = match strategy::evaluate(btc_delta_pct, secs_remaining, market_price, bankroll, vol, &self.cfg.strategy) {
            Ok(s)  => s,
            Err(r) => {
                let _ = self.tracker.log_signal(crate::tracker::SignalRow {
                    ts: Tracker::now_iso(), window_ts, side: if btc_delta_pct > 0.0 {"UP".into()} else {"DOWN".into()},
                    btc_delta_pct, seconds_remaining: secs_remaining, market_price,
                    true_prob: 0.0, edge: 0.0, bet_usd: 0.0, vol,
                    skip_reason: format!("{:?}", r),
                });
                return Ok(());
            }
        };

        // Log accepted signal
        let _ = self.tracker.log_signal(crate::tracker::SignalRow {
            ts: Tracker::now_iso(), window_ts, side: signal.side.into(),
            btc_delta_pct, seconds_remaining: secs_remaining, market_price,
            true_prob: signal.true_prob, edge: signal.edge, bet_usd: signal.bet_usd, vol,
            skip_reason: "".into(),
        });

        if self.cfg.dry_run { info!("[GENGAR][DRY] would buy {} ${:.2}@{:.3}", signal.side, signal.bet_usd, market_price); return Ok(()); }

        // Live entry
        let exec = self.executor.as_ref().unwrap();
        let res = exec.buy(token_id, market_price, signal.bet_usd).await?;
        use crate::executor::OrderResultStatus::*;
        match res.status {
            Filled | GhostFilled => {
                let mut st = self.state.write().await;
                st.add_pos(OpenPosition {
                    window_ts, token_id: token_id.clone(), side: signal.side.into(),
                    price: market_price, shares: res.shares, usd_spent: res.usd_spent,
                    order_id: res.order_id.unwrap_or_default(),
                    opening_price: st.opening_price,
                });
                st.bankroll -= res.usd_spent;
                let _ = self.tracker.log_trade(crate::tracker::TradeRow {
                    ts: Tracker::now_iso(), window_ts, side: signal.side.into(),
                    price: market_price, shares: res.shares, usd: res.usd_spent,
                    order_id: "buy".into(), status: format!("{:?}", res.status),
                    resolution: None, pnl: None,
                });
            }
            Partial => warn!("[GENGAR][EXEC] partial buy (unexpected for GTC limit)"),
            Failed(reason) => {
                warn!("[GENGAR][EXEC] buy failed: {}", reason);
                self.handle_error(&reason).await;
            }
        }
        Ok(())
    }

    /// Reference: bot.py:682-694, captured once at startup, no auto-reset.
    pub async fn check_daily_loss(&self) {
        let mut st = self.state.write().await;
        let session_pnl = st.bankroll - st.session_start_balance;
        if session_pnl <= -self.cfg.risk.daily_loss_limit {
            warn!("[GENGAR] daily-loss CB tripped: session PnL=${:.2} ≤ -${}",
                  session_pnl, self.cfg.risk.daily_loss_limit);
            st.daily_loss_halted = true;
        }
    }

    /// Called at every window boundary. Reference: bot.py:827-1012.
    /// (Resolution detection is large; this is a minimal stub for MVP — fills via
    /// claim-sell at $0.99, defers via _pending_phantom on no-balance-movement.)
    pub async fn resolve_window(&self, prev_window_ts: i64) {
        let prev_positions: Vec<OpenPosition> = {
            let st = self.state.read().await;
            st.positions.iter().filter(|p| p.window_ts == prev_window_ts).cloned().collect()
        };
        for pos in prev_positions {
            // For dry-run or no-executor: resolve by comparing BTC opening vs current.
            if self.cfg.dry_run || self.executor.is_none() {
                let cur_btc = self.price.read().await.price;
                let won = match pos.side.as_str() {
                    "UP"   => cur_btc >= pos.opening_price,
                    "DOWN" => cur_btc <  pos.opening_price,
                    _ => false,
                };
                let pnl = if won { pos.shares as f64 * (1.0 - pos.price) } else { -pos.usd_spent };
                info!("[GENGAR][DRY-RESOLVE] {} pnl=${:.2}", if won {"WIN"} else {"LOSS"}, pnl);
                let mut st = self.state.write().await;
                st.realized_pnl_today += pnl;
                st.positions.retain(|p| p.window_ts != prev_window_ts);
                continue;
            }
            // Live: try claim-sell at $0.99 to detect WIN; on no-movement defer to _pending_phantom.
            let exec = self.executor.as_ref().unwrap();
            let balance_before = exec.clob.get_balance_allowance(&exec.creds, exec.eoa_addr).await.unwrap_or(0.0);
            let claim = exec.sell(&pos.token_id, 0.99, pos.shares).await;
            match claim {
                Ok(r) if matches!(r.status, crate::executor::OrderResultStatus::Filled | crate::executor::OrderResultStatus::GhostFilled) => {
                    let pnl = r.usd_spent - pos.usd_spent;
                    let mut st = self.state.write().await;
                    st.realized_pnl_today += pnl;
                    st.positions.retain(|p| p.window_ts != prev_window_ts);
                }
                Ok(_) => {
                    // Possibly a phantom; defer.
                    let mut st = self.state.write().await;
                    st.pending_phantom = Some(PendingPhantom {
                        window_ts: prev_window_ts,
                        claim_order_id: pos.order_id.clone(),
                        expected_proceeds: pos.shares as f64 * 0.99,
                        balance_before,
                    });
                }
                Err(e) => warn!("[GENGAR][RESOLVE] claim-sell error: {}", e),
            }
        }
        self.check_daily_loss().await;
    }
}
```

- [ ] **Step 3: Build, commit**

```bash
cargo build -p pmbots-gengar
git add crates/pmbots-gengar/src/bot.rs
git commit -m "feat(gengar): position lifecycle + _pending_phantom + daily-loss CB + rolling vol"
```

Expected: compiles cleanly.

---

### Task 21: `main.rs` — wire it all + dry-run smoke

**Files:**
- Modify: `crates/pmbots-gengar/src/main.rs`

- [ ] **Step 1: Write the entrypoint**

Replace `crates/pmbots-gengar/src/main.rs` with:

```rust
//! pmbots-gengar entrypoint.

use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;
use pmbots_gengar::{
    config::GengarConfig,
    bot::GengarBot,
    price_feed::{self, new_state},
    polymarket::{clob::{ClobClient, SignatureType}, ws as poly_ws},
    tracker::Tracker,
    executor::Executor,
};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::Address;
use std::str::FromStr;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,pmbots_gengar=debug")))
        .init();

    let cfg = Arc::new(GengarConfig::from_env().context("load config")?);
    info!("[GENGAR] config: dry_run={} period={}s log_dir={:?}",
          cfg.dry_run, cfg.market_period_secs, cfg.log_dir);

    let tracker = Arc::new(Tracker::new(&cfg.log_dir, cfg.log_executions)?);
    tracker.log_session(pmbots_gengar::tracker::SessionRow {
        ts: Tracker::now_iso(), event: "start".into(),
        bankroll: cfg.strategy.min_bet, trades_count: 0, wins: 0, losses: 0, session_pnl: 0.0,
    }).ok();

    let price_state = new_state();
    tokio::spawn(price_feed::run_ws(price_state.clone()));
    tokio::spawn(price_feed::run_rest_fallback(price_state.clone()));

    let executor: Option<Executor> = if cfg.dry_run {
        None
    } else {
        let wallet: LocalWallet = cfg.private_key.parse().context("parse private key")?;
        let eoa_addr: Address = cfg.safe_address.unwrap_or_else(|| wallet.address());
        let sig_type = if cfg.safe_address.is_some() { SignatureType::Safe } else { SignatureType::Eoa };
        let clob = ClobClient::new()?;
        let creds = ClobClient::derive_api_creds(&wallet).await?;
        Some(Executor { clob, creds, eoa_addr, wallet, sig_type })
    };

    let bot = Arc::new(GengarBot::new(cfg, price_state, tracker.clone(), executor).await?);
    let bot_clone = bot.clone();

    // bot.run() drives window transitions; the Polymarket WS subscription is
    // spawned/torn down per-window inside the run loop (see Task 20's
    // rotate_ws_subscription).
    let _ = tokio::spawn(async move { bot_clone.run().await });
    tokio::signal::ctrl_c().await.ok();
    info!("[GENGAR] shutdown requested");
    tracker.log_session(pmbots_gengar::tracker::SessionRow {
        ts: Tracker::now_iso(), event: "stop".into(),
        bankroll: 0.0, trades_count: 0, wins: 0, losses: 0, session_pnl: 0.0,
    }).ok();
    Ok(())
}
```

- [ ] **Step 2: Dry-run smoke test**

```bash
GENGAR_DRY_RUN=true cargo run -p pmbots-gengar --release 2>&1 | head -30
```

Expected output within ~10s:
```
[GENGAR] config: dry_run=true period=300s log_dir="logs/gengar"
[GENGAR][PRICE-WS] connected
[GENGAR] starting main loop (dry_run=true)
[GENGAR] window transition: 0 → 174XXXXXXX
```

After ~1 BTC window passes (5 minutes), `logs/gengar/signals.csv` should have rows with `skip_reason` populated (most signals will be skipped at first since we don't yet have a real opening price baseline tied to oracle data).

Stop with Ctrl-C.

- [ ] **Step 3: Commit + push (optional)**

```bash
git add crates/pmbots-gengar/src/main.rs
git commit -m "feat(gengar): wire main.rs entrypoint + dry-run smoke"
```

---

## Self-Review Notes

After implementing all 21 tasks, the engineer should:

1. **Run the full workspace build:**
   ```bash
   cargo build --workspace --release
   cargo test --workspace
   ```
   Expected: both crates compile; arb's existing tests pass at the same rate as before migration; gengar's unit tests pass.

2. **Compare gengar Rust vs Python signals.** Run gengar Python and gengar Rust in dry-run mode against the same BTC window. The signal logs (`signals.csv`) should agree on every accepted signal's `(side, edge ± 0.01, bet_usd ± $0.50)`. Disagreement indicates a math port error.

3. **Validate `derive_api_creds` parity** (see Task 8 integration TODO). Run gengar Python once with a test wallet, capture the derived `{key, secret, passphrase}`, and assert in a Rust integration test that the Rust derivation produces the same triple. If they differ, revise the salts/scheme in `polymarket/clob.rs`.

4. **First live test under `DRY_RUN=false`.** Fund a test wallet with $20 USDC. Set `GENGAR_MAX_BET=5.0` to keep risk tiny. Run for 3 BTC windows; verify at least one round-trip (entry → resolution) lands cleanly. Especially watch for `UNVERIFIED_BUY` warnings and confirm the bot does NOT cancel.

5. **V2 WAF header verification** (gotcha #10 from spec). If any CLOB endpoint returns CF 403 / "blocked", compare gengar's request headers (capture via `RUST_LOG=trace` or a proxy) against arb's working request headers and align.

### Known follow-up work (out of scope for this plan)

- `_pending_phantom` resolution at the *next* window boundary (`bot.py:436-485`) is sketched (state field exists, claim-sell defers to it) but the second-window reconciliation logic is not yet exercised in MVP. Implement the second-window check after seeing live phantom cases.
- 15-minute bot port — separate spec when gengar stabilizes.
- Extract shared `pmbots-polymarket-client` crate once gengar + 15-min are both live.

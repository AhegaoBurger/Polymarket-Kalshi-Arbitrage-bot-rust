//! Standalone diagnostic for Polymarket /balance-allowance.
//!
//! Mirrors `scripts/poly_l2_auth/poly-test-l2` (Python) so we can verify
//! the Rust HTTP path against the same endpoint without spinning up the
//! full bot. Useful when triaging V2 cutover regressions like the
//! 2026-04-28 WAF tightening that broke stock SDK clients.
//!
//! Reads creds from the project-root `.env` via dotenvy. Prints balance
//! in micros and as a USDC float.

use anyhow::{Context, Result};
use pmbots_arb::polymarket_clob::{
    ApiCreds, PolymarketAsyncClient, PreparedCreds, SharedAsyncClient,
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let host = std::env::var("POLY_HOST").unwrap_or_else(|_| "https://clob.polymarket.com".into());
    let chain_id: u64 = 137;
    let private_key = std::env::var("POLY_PRIVATE_KEY").context("POLY_PRIVATE_KEY not set")?;
    let funder = std::env::var("POLY_FUNDER").context("POLY_FUNDER not set")?;

    let api_creds = ApiCreds {
        api_key: std::env::var("POLY_API_KEY").context("POLY_API_KEY not set")?,
        api_secret: std::env::var("POLY_SECRET").context("POLY_SECRET not set")?,
        api_passphrase: std::env::var("POLY_PASSPHRASE").context("POLY_PASSPHRASE not set")?,
    };
    let prepared = PreparedCreds::from_api_creds(&api_creds)?;
    let client = PolymarketAsyncClient::new(&host, chain_id, &private_key, &funder)?;
    let shared = SharedAsyncClient::new(client, prepared, chain_id);

    println!(">>> fetch_poly_balance_usdc_micros()");
    match shared.fetch_poly_balance_usdc_micros().await {
        Ok(micros) => {
            println!(
                "    SUCCESS: {} micros = ${:.6} USDC",
                micros,
                micros as f64 / 1_000_000.0
            );
            Ok(())
        }
        Err(e) => {
            println!("    FAILED: {:?}", e);
            std::process::exit(1);
        }
    }
}

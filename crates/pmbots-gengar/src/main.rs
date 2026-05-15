//! pmbots-gengar entrypoint.

use anyhow::{Context, Result};
use std::sync::Arc;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use pmbots_gengar::{
    bot::GengarBot,
    config::GengarConfig,
    executor::Executor,
    polymarket::clob::{ClobClient, SignatureType},
    price_feed::{self, new_state},
    tracker::Tracker,
};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::Address;

/// Polygon mainnet chain ID. V2 only supports Polygon (per arb's working code).
const POLYGON_CHAIN_ID: u64 = 137;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,pmbots_gengar=debug")),
        )
        .init();

    let cfg = Arc::new(GengarConfig::from_env().context("load config")?);
    info!(
        "[GENGAR] config: dry_run={} period={}s log_dir={:?}",
        cfg.dry_run, cfg.market_period_secs, cfg.log_dir
    );

    let tracker = Arc::new(Tracker::new(&cfg.log_dir, cfg.log_executions)?);

    let price_state = new_state();
    tokio::spawn(price_feed::run_ws(price_state.clone()));
    tokio::spawn(price_feed::run_rest_fallback(price_state.clone()));

    // Build executor + fetch starting balance from chain (Batch 3 I1+I4).
    let (executor, initial_balance): (Option<Executor>, f64) = if cfg.dry_run {
        // Dry-run: use a notional starting bankroll so Kelly sizing works.
        // Matches gengar Python's behavior — bot.py:71 `BANKROLL` env var
        // defaults to $100 in dry-run.
        (None, 100.0)
    } else {
        let wallet: LocalWallet = cfg.private_key.parse().context("parse private key")?;
        // signer_addr = the EOA that signs all CLOB requests. The API key was
        // (or will be) issued against this address; POLY_ADDRESS in L2 headers
        // MUST be this address, even when funds live in a Safe proxy.
        let signer_addr: Address = wallet.address();
        // funder = the Safe proxy that holds the USDC. None for EOA flow.
        let funder: Option<Address> = cfg.safe_address;

        // Signature type detection. Order of precedence:
        //   1. GENGAR_SIG_TYPE env var override (0/1/2) — needed when the
        //      auto-detect picks the wrong proxy type. Polymarket V2's "maker
        //      address not allowed, please use the deposit wallet flow" error
        //      means the auto-detected sig_type doesn't match how the funder
        //      was registered server-side. Email/Magic logins → 1 (POLY_PROXY).
        //   2. funder == signer or no funder set → 0 (EOA, direct trading).
        //   3. funder != signer → 2 (Safe / browser wallet) by default. Users
        //      who connected via Polymarket's email/Magic flow should set
        //      GENGAR_SIG_TYPE=1 explicitly.
        let sig_type = if let Ok(raw) = std::env::var("GENGAR_SIG_TYPE") {
            match raw.trim() {
                "0" => SignatureType::Eoa,
                "1" => SignatureType::PolyProxy,
                "2" => SignatureType::Safe,
                "3" => SignatureType::Poly1271,
                _ => anyhow::bail!("GENGAR_SIG_TYPE must be 0, 1, 2, or 3 (got {:?})", raw),
            }
        } else if funder.is_some() {
            SignatureType::Safe
        } else {
            SignatureType::Eoa
        };
        info!(
            "[GENGAR] signature type: {:?} (override via GENGAR_SIG_TYPE if Polymarket rejects orders)",
            sig_type
        );
        let clob = ClobClient::new()?;

        // For Poly1271 (sig_type=3), the API key must be associated with the
        // FUNDER (deposit wallet) on the server side because order.signer ==
        // funder. If the API key were bound to the EOA, V2 production rejects
        // every order with "the order signer address has to be the address of
        // the API KEY" (Issue #58). For all other sig types the API key is
        // bound to the EOA (which is also order.signer).
        let auth_address = match sig_type {
            SignatureType::Poly1271 => funder.ok_or_else(|| {
                anyhow::anyhow!("Poly1271 requires GENGAR_SAFE_ADDRESS (deposit wallet)")
            })?,
            _ => signer_addr,
        };
        info!(
            "[GENGAR] API auth address: {:?} (signer EOA produces signatures; \
             server verifies via EIP-1271 when this differs from the EOA)",
            auth_address
        );

        // V2 path: get/create API creds via SERVER (not local HMAC).
        let creds = clob
            .get_or_derive_api_creds(&wallet, POLYGON_CHAIN_ID, auth_address)
            .await
            .context("get_or_derive_api_creds")?;

        // Seed the CLOB's internal balance cache from on-chain state. REQUIRED
        // for Poly1271 deposit wallets (sig_type=3) before the first order.
        // Harmless no-op for legacy Proxy/Safe accounts.
        if let Err(e) = clob
            .update_balance_allowance(&creds, auth_address, sig_type)
            .await
        {
            warn!(
                "[GENGAR] update_balance_allowance failed: {} — continuing; balance read may be stale",
                e
            );
        }

        // L2 POLY_ADDRESS must match the API key's address (auth_address).
        let bal = clob
            .get_balance_allowance(&creds, auth_address, sig_type)
            .await
            .context("fetch starting USDC balance")?;
        info!(
            "[GENGAR] startup balance: ${:.2} (signer={:?}, funder={:?})",
            bal, signer_addr, funder
        );
        if bal < cfg.strategy.min_bet {
            warn!(
                "[GENGAR] startup balance ${:.2} is below GENGAR_MIN_BET=${} — \
                 entries will be filtered by Kelly's min-bet floor",
                bal, cfg.strategy.min_bet
            );
        }

        let exec = Executor {
            clob,
            creds,
            signer_addr,
            auth_address,
            funder,
            wallet,
            sig_type,
            chain_id: POLYGON_CHAIN_ID,
        };
        (Some(exec), bal)
    };

    tracker
        .log_session(pmbots_gengar::tracker::SessionRow {
            ts: Tracker::now_iso(),
            event: "start".into(),
            bankroll: initial_balance,
            trades_count: 0,
            wins: 0,
            losses: 0,
            session_pnl: 0.0,
        })
        .ok();

    let bot = Arc::new(
        GengarBot::new(cfg, price_state, tracker.clone(), executor, initial_balance).await?,
    );
    let bot_clone = bot.clone();

    // bot.run() drives window transitions; the Polymarket WS subscription is
    // spawned/torn down per-window inside the run loop (see Task 20's
    // rotate_ws_subscription).
    let _ = tokio::spawn(async move { bot_clone.run().await });
    tokio::signal::ctrl_c().await.ok();
    info!("[GENGAR] shutdown requested");
    tracker
        .log_session(pmbots_gengar::tracker::SessionRow {
            ts: Tracker::now_iso(),
            event: "stop".into(),
            bankroll: 0.0,
            trades_count: 0,
            wins: 0,
            losses: 0,
            session_pnl: 0.0,
        })
        .ok();
    Ok(())
}

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

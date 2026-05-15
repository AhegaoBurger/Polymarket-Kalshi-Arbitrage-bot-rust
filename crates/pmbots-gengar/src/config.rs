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
        // `set_var` to empty (not `remove_var`) so `dotenvy::dotenv()` inside
        // `from_env` doesn't repopulate from a real `.env` file in the repo.
        std::env::set_var("GENGAR_PRIVATE_KEY", "");
        assert!(GengarConfig::from_env().is_err());
    }
}

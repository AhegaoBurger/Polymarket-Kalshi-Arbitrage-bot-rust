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

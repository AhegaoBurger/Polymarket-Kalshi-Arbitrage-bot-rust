//! Main bot loop — window transitions, health check, position lifecycle.
//! Reference: bot.py.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::{info, warn};
use crate::config::GengarConfig;
use crate::market::{self};
use crate::polymarket::{gamma::GammaClient, ws::{self, TokenPriceCache}};
use crate::price_feed::SharedPrice;
use crate::strategy;
use crate::tracker::Tracker;
use crate::executor::Executor;

pub const CLOB_HALT_THRESHOLD: u32 = 3;        // bot.py:816-823
pub const HALT_ERROR_PATTERNS: &[&str] = &[
    "request exception", "service not ready", "status_code=none",
];

/// Brownian-model volatility fallback used until enough rolling samples accumulate.
/// Reference: gengar bot.py:76 `_vol_fallback = 0.12`. This is the v13 calibrated
/// value — the README documents that 0.08 was 2x overconfident and 0.15 was too
/// conservative; 0.12 is the sweet spot.
pub const VOL_FALLBACK: f64 = 0.12;

pub struct GengarBot {
    pub cfg: Arc<GengarConfig>,
    pub gamma: GammaClient,
    pub price: SharedPrice,
    pub poly_prices: TokenPriceCache,
    pub tracker: Arc<Tracker>,
    pub executor: Option<Executor>,           // None in DRY_RUN
    pub state: Arc<RwLock<BotState>>,
}

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

impl BotState {
    pub fn add_pos(&mut self, p: OpenPosition) { self.positions.push(p); }

    /// Rolling stddev of `window_returns` in percentage points, clamped to
    /// `[floor, cap]`. Falls back to `VOL_FALLBACK` (0.12) when fewer than
    /// `max(6, rolling_windows / 2)` samples are present. Reference: gengar
    /// bot.py:603-614 `_compute_realized_vol`.
    pub fn rolling_vol(&self, floor: f64, cap: f64, rolling_windows: usize) -> f64 {
        let min_samples = std::cmp::max(6, rolling_windows / 2);
        if self.window_returns.len() < min_samples { return VOL_FALLBACK; }
        let n = self.window_returns.len() as f64;
        let mean = self.window_returns.iter().sum::<f64>() / n;
        let var = self.window_returns.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        var.sqrt().clamp(floor, cap)
    }
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
            let prev_window: i64;
            let prev_opening_price: f64;
            let need_unhalt_check: bool;
            {
                let mut st = self.state.write().await;
                transition = window_ts != st.current_window;
                prev_window = st.current_window;
                prev_opening_price = st.opening_price;
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

            // On transition: resolve prior window, then rotate WS to new window.
            if transition && prev_window > 0 {
                self.resolve_window(prev_window, prev_opening_price).await;
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

    /// Tear down the previous window's Polymarket WS task and spawn a new one
    /// subscribed to the new window's Up/Down token IDs. Called on transition.
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
        // Percentage points (matches gengar bot.py:317 `* 100`). strategy::evaluate
        // expects the same units as vol — both in percent points.
        let btc_delta_pct = (btc_now - st.opening_price) / st.opening_price * 100.0;
        let vol = st.rolling_vol(self.cfg.vol.floor, self.cfg.vol.cap, self.cfg.vol.rolling_windows);
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
                let opening_price = st.opening_price;
                st.add_pos(OpenPosition {
                    window_ts, token_id: token_id.clone(), side: signal.side.into(),
                    price: market_price, shares: res.shares, usd_spent: res.usd_spent,
                    order_id: res.order_id.unwrap_or_default(),
                    opening_price,
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

    /// Resolve any `pending_phantom` from a prior window's deferred claim-sell.
    /// Reference: gengar bot.py:436-485. Compares current real balance to the
    /// captured `balance_before` snapshot: if it grew by ≥ 50% of expected
    /// proceeds, the claim landed (WIN); otherwise it's confirmed LOSS. Either
    /// way the phantom and the originating position are cleared from state.
    pub async fn resolve_pending_phantom(&self) {
        let phantom = {
            let mut st = self.state.write().await;
            st.pending_phantom.take()
        };
        let Some(pp) = phantom else { return; };

        // Dry-run / no executor: matches bot.py:482-485 (treat as loss).
        if self.cfg.dry_run || self.executor.is_none() {
            let mut st = self.state.write().await;
            let cost: f64 = st.positions.iter()
                .filter(|p| p.window_ts == pp.window_ts)
                .map(|p| p.usd_spent).sum();
            st.realized_pnl_today -= cost;
            st.positions.retain(|p| p.window_ts != pp.window_ts);
            warn!("[GENGAR] phantom dry-resolved as LOSS (${:.2})", cost);
            return;
        }

        let exec = self.executor.as_ref().unwrap();
        let real_bal = match exec.clob.get_balance_allowance(&exec.creds, exec.eoa_addr).await {
            Ok(b) => b,
            Err(e) => {
                warn!("[GENGAR] phantom balance check failed: {} (leaving phantom unresolved)", e);
                // Put phantom back so the next transition can retry.
                self.state.write().await.pending_phantom = Some(pp);
                return;
            }
        };
        let balance_increase = (real_bal - pp.balance_before).max(0.0);

        let mut st = self.state.write().await;
        let cost: f64 = st.positions.iter()
            .filter(|p| p.window_ts == pp.window_ts)
            .map(|p| p.usd_spent).sum();
        if balance_increase > pp.expected_proceeds * 0.50 {
            let pnl = balance_increase - cost;
            st.realized_pnl_today += pnl;
            st.bankroll = real_bal;
            info!("[GENGAR] phantom resolved: WIN +${:.2}", pnl);
        } else {
            st.realized_pnl_today -= cost;
            st.bankroll = real_bal;
            warn!("[GENGAR] phantom confirmed: LOSS -${:.2}", cost);
        }
        st.positions.retain(|p| p.window_ts != pp.window_ts);
    }

    /// Push the just-closed window's |return%| into the rolling-vol buffer.
    /// Reference: gengar bot.py:487-492.
    pub async fn push_closing_delta(&self, prev_opening_price: f64, closing_btc_price: f64) {
        if prev_opening_price <= 0.0 || closing_btc_price <= 0.0 { return; }
        let closing_delta = ((closing_btc_price - prev_opening_price) / prev_opening_price).abs() * 100.0;
        let mut st = self.state.write().await;
        st.window_returns.push_back(closing_delta);
        let cap = self.cfg.vol.rolling_windows;
        while st.window_returns.len() > cap {
            st.window_returns.pop_front();
        }
    }

    /// Called at every window boundary. Reference: bot.py:827-1012.
    /// Order: (1) resolve any deferred phantom from the PRIOR window, (2) push
    /// the just-closed window's |return%| into the rolling-vol buffer, (3) try
    /// claim-selling open positions at $0.99 — fills count as WIN, no-balance-
    /// movement gets deferred into `pending_phantom` for the next transition.
    pub async fn resolve_window(&self, prev_window_ts: i64, prev_opening_price: f64) {
        // (1) Resolve any pending phantom captured at a previous transition.
        self.resolve_pending_phantom().await;

        // (2) Record closing delta for rolling-vol producer.
        let closing_btc = self.price.read().await.price;
        self.push_closing_delta(prev_opening_price, closing_btc).await;

        // (3) Resolve open positions from the just-closed window.
        let prev_positions: Vec<OpenPosition> = {
            let st = self.state.read().await;
            st.positions.iter().filter(|p| p.window_ts == prev_window_ts).cloned().collect()
        };
        for pos in prev_positions {
            // For dry-run or no-executor: resolve by comparing BTC opening vs current.
            if self.cfg.dry_run || self.executor.is_none() {
                let won = match pos.side.as_str() {
                    "UP"   => closing_btc >= pos.opening_price,
                    "DOWN" => closing_btc <  pos.opening_price,
                    _ => false,
                };
                let pnl = if won { pos.shares as f64 * (1.0 - pos.price) } else { -pos.usd_spent };
                info!("[GENGAR][DRY-RESOLVE] {} pnl=${:.2}", if won {"WIN"} else {"LOSS"}, pnl);
                let mut st = self.state.write().await;
                st.realized_pnl_today += pnl;
                st.positions.retain(|p| p.window_ts != prev_window_ts);
                continue;
            }
            // Live: try claim-sell at $0.99 to detect WIN; on no-movement defer to pending_phantom.
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
                    // Possibly a phantom; defer (position stays in state.positions until phantom
                    // is resolved at the NEXT transition — see resolve_pending_phantom).
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

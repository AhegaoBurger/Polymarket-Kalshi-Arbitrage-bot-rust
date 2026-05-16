//! Executor — order placement with integer-cents arithmetic and ghost-fill verification.
//! Reference: executor.py.
//!
//! Three load-bearing concerns ported verbatim from the Python:
//!   1. `calculate_order_size` — integer-cents arithmetic. Float division
//!      produces artifacts like 21.000000000004 that the CLOB rejects.
//!   2. Buy verification with ghost-fill — success AND exception paths both
//!      re-check balance because Polymarket can fill an order while returning
//!      an error or HTTP exception.
//!   3. `UNVERIFIED_BUY` non-cancel contract — after 3 verification attempts
//!      fail we return `Failed("UNVERIFIED_BUY")` but DO NOT cancel the order.
//!      Polygon settlement can take 5-15s; cancelling here can race the fill
//!      and leave the bot with phantom shares.

use anyhow::{Context, Result};
use ethers::signers::LocalWallet;
use ethers::types::Address;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::polymarket::clob::{ApiCreds, ClobClient, OrderArgs, OrderType, SignatureType};
use crate::polymarket::types::{PriceCents, Side};

pub const POLY_MIN_NOTIONAL: f64 = 5.0; // USD
pub const MAX_BUY_PRICE: f64 = 0.90; // executor.py:181 — reject above this
pub const BUY_VERIFY_ATTEMPTS: u32 = 3;
pub const BUY_VERIFY_SLEEP_MS: u64 = 3000;
pub const GHOST_BUY_DELTA: f64 = 1.00; // USDC drop > $1 on exception → ghost fill
pub const GHOST_BUY_BAL_DELTA: f64 = 0.50; // USDC drop > $0.50 on success → filled
pub const GHOST_SELL_BAL_DELTA: f64 = 0.10; // USDC rise > $0.10 → sell filled

#[derive(Debug, Clone, PartialEq)]
pub enum OrderResultStatus {
    Filled,
    Partial,
    Failed(String),
    GhostFilled,
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
/// Polymarket CLOB rejects orders with float-precision artifacts like
/// `21.000000000004` shares produced by naive `amount / price`.
///
/// Returns `(shares, clean_usd)` where `clean_usd == shares * price` to 2 decimals.
pub fn calculate_order_size(price: f64, max_usd: f64) -> (i64, f64) {
    if price <= 0.0 || max_usd <= 0.0 {
        return (0, 0.0);
    }
    let price_cents = (price * 100.0).round() as i64;
    if price_cents <= 0 {
        return (0, 0.0);
    }
    let max_usd_cents = (max_usd * 100.0) as i64;
    let max_shares = max_usd_cents / price_cents;
    if max_shares < 1 {
        return (0, 0.0);
    }
    let clean_usd = (max_shares * price_cents) as f64 / 100.0;
    (max_shares, clean_usd)
}

#[cfg(test)]
mod size_tests {
    use super::*;

    #[test]
    fn python_reference_values() {
        // executor.py:56-74 example: price=0.68, max=21.0 → (30 shares, 20.40 USD)
        // Python: 2100 // 68 = 30, spend = 30*68/100 = 20.40.
        assert_eq!(calculate_order_size(0.68, 21.0), (30, 20.40));
        // price=0.50, max=12.5 → 25 shares, 12.50.
        assert_eq!(calculate_order_size(0.50, 12.5), (25, 12.50));
    }

    #[test]
    fn small_max_yields_zero() {
        // price=0.99, max=0.50 → 0 shares.
        assert_eq!(calculate_order_size(0.99, 0.50), (0, 0.0));
    }

    #[test]
    fn zero_or_negative_inputs() {
        assert_eq!(calculate_order_size(0.0, 10.0), (0, 0.0));
        assert_eq!(calculate_order_size(0.5, 0.0), (0, 0.0));
        assert_eq!(calculate_order_size(-0.5, 10.0), (0, 0.0));
    }
}

pub struct Executor {
    pub clob: ClobClient,
    pub creds: ApiCreds,
    /// The EOA address that produces ECDSA signatures.
    pub signer_addr: Address,
    /// The address used as POLY_ADDRESS in L2 CLOB request headers (i.e., the
    /// address the API key is bound to server-side). For sig_type 0/1/2 this
    /// is the EOA. For Poly1271 (sig_type=3) this is the funder (deposit
    /// wallet) — server-side validation requires the API key's address to
    /// match the order's `signer` field, which is the funder for Poly1271.
    pub auth_address: Address,
    /// The Safe / deposit-wallet / proxy that holds the USDC funds. `None`
    /// for direct EOA trading. Becomes the EIP-712 `maker` field in orders.
    pub funder: Option<Address>,
    pub wallet: LocalWallet,
    pub sig_type: SignatureType,
    pub chain_id: u64,
}

impl Executor {
    /// Buy `max_usd` of `token_id` at limit `price`.
    /// Returns `OrderResult` with status reflecting verification outcome.
    /// Reference: executor.py:138-265 (buy) + :267-322 (_verify_buy_via_balance).
    pub async fn buy(&self, token_id: &str, price: f64, max_usd: f64) -> Result<OrderResult> {
        if price > MAX_BUY_PRICE {
            return Ok(OrderResult {
                status: OrderResultStatus::Failed("price_above_max".into()),
                order_id: None,
                price,
                shares: 0,
                usd_spent: 0.0,
            });
        }
        let (shares, clean_usd) = calculate_order_size(price, max_usd);
        if shares == 0 || clean_usd < POLY_MIN_NOTIONAL {
            return Ok(OrderResult {
                status: OrderResultStatus::Failed("below_min_notional".into()),
                order_id: None,
                price,
                shares,
                usd_spent: 0.0,
            });
        }
        let balance_before = self
            .clob
            .get_balance_allowance(&self.creds, self.auth_address, self.sig_type)
            .await
            .context("balance_before")?;

        let args = OrderArgs {
            token_id: token_id.into(),
            price: PriceCents::from_dollars(price),
            size_shares: shares,
            side: Side::Buy,
            maker_address: self.signer_addr,
            funder: self.funder,
        };
        let signed = match self
            .clob
            .create_order(args, self.sig_type, self.chain_id, &self.wallet, false)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                return Ok(OrderResult {
                    status: OrderResultStatus::Failed(format!("create_order: {}", e)),
                    order_id: None,
                    price,
                    shares,
                    usd_spent: 0.0,
                });
            }
        };

        let post_result = self
            .clob
            .post_order(&self.creds, self.auth_address, &signed, OrderType::Gtc)
            .await;
        let order_id = match post_result {
            Ok(resp) if resp.success => resp.order_id,
            Ok(resp) => {
                // success=false from server. Check exception ghost path:
                // wait 3s, then re-check balance. Reference: executor.py:246-260.
                sleep(Duration::from_secs(3)).await;
                let bal_after = self
                    .clob
                    .get_balance_allowance(&self.creds, self.auth_address, self.sig_type)
                    .await
                    .unwrap_or(balance_before);
                let dropped = balance_before - bal_after;
                if dropped > GHOST_BUY_DELTA {
                    warn!(
                        "[GENGAR][EXEC] ghost fill detected (server said failure but balance dropped by ${:.2})",
                        dropped
                    );
                    return Ok(OrderResult {
                        status: OrderResultStatus::GhostFilled,
                        order_id: Some("ghost-buy".into()),
                        price,
                        shares,
                        usd_spent: dropped,
                    });
                }
                return Ok(OrderResult {
                    status: OrderResultStatus::Failed(format!("post: {:?}", resp.error_msg)),
                    order_id: None,
                    price,
                    shares,
                    usd_spent: 0.0,
                });
            }
            Err(e) => {
                // HTTP/network exception. Same ghost-fill check.
                // Reference: executor.py:246-260.
                sleep(Duration::from_secs(3)).await;
                let bal_after = self
                    .clob
                    .get_balance_allowance(&self.creds, self.auth_address, self.sig_type)
                    .await
                    .unwrap_or(balance_before);
                let dropped = balance_before - bal_after;
                if dropped > GHOST_BUY_DELTA {
                    warn!("[GENGAR][EXEC] ghost fill detected on exception path");
                    return Ok(OrderResult {
                        status: OrderResultStatus::GhostFilled,
                        order_id: Some("ghost-buy".into()),
                        price,
                        shares,
                        usd_spent: dropped,
                    });
                }
                return Ok(OrderResult {
                    status: OrderResultStatus::Failed(format!("post exception: {}", e)),
                    order_id: None,
                    price,
                    shares,
                    usd_spent: 0.0,
                });
            }
        };

        let order_id = order_id.unwrap_or_default();

        // Success path verification. Reference: executor.py:267-322.
        // 3 attempts × 3s sleep each = ~9s of polling. Polygon settlement
        // typically lands in 5-15s.
        for attempt in 1..=BUY_VERIFY_ATTEMPTS {
            sleep(Duration::from_millis(BUY_VERIFY_SLEEP_MS)).await;
            let bal_after = self
                .clob
                .get_balance_allowance(&self.creds, self.auth_address, self.sig_type)
                .await
                .unwrap_or(balance_before);
            let dropped = balance_before - bal_after;
            if dropped > GHOST_BUY_BAL_DELTA {
                let actual_shares = ((dropped / price).round() as i64).max(1);
                info!(
                    "[GENGAR][EXEC] buy verified via balance on attempt {} (-${:.2})",
                    attempt, dropped
                );
                return Ok(OrderResult {
                    status: OrderResultStatus::Filled,
                    order_id: Some(order_id.clone()),
                    price,
                    shares: actual_shares,
                    usd_spent: dropped,
                });
            }
            // Fallback: poll /data/order/{id}.
            if let Ok(Some(status)) = self
                .clob
                .get_order(&self.creds, self.auth_address, &order_id)
                .await
            {
                if let Some(sm) = status.size_matched.as_ref() {
                    if let Ok(matched) = sm.parse::<f64>() {
                        if matched > 0.0 {
                            return Ok(OrderResult {
                                status: OrderResultStatus::Filled,
                                order_id: Some(order_id),
                                price,
                                shares: matched as i64,
                                usd_spent: matched * price,
                            });
                        }
                    }
                }
            }
        }

        // CRITICAL — reference: executor.py:312-322.
        // Neither balance nor /order/{id} confirmed after ~14s.
        // DO NOT CANCEL. Polygon settlement can take 5-15s, and cancelling
        // here races the fill: we could end up with phantom shares that the
        // bot doesn't know it owns. The bot's window-boundary balance sync
        // is the recovery mechanism.
        warn!(
            "[GENGAR][EXEC] UNVERIFIED_BUY for order {}, NOT cancelling per protocol",
            order_id
        );
        Ok(OrderResult {
            status: OrderResultStatus::Failed("UNVERIFIED_BUY".into()),
            order_id: Some(order_id),
            price,
            shares,
            usd_spent: 0.0,
        })
    }

    /// Sell at limit `price` (typically claim-sell at $0.99 to detect resolution).
    /// Returns `OrderResult` where `shares` carries the remaining unfilled count
    /// (0 = fully filled). Reference: executor.py:326-401.
    pub async fn sell(
        &self,
        token_id: &str,
        price: f64,
        sell_shares: i64,
    ) -> Result<OrderResult> {
        if (sell_shares as f64) * price < POLY_MIN_NOTIONAL {
            return Ok(OrderResult {
                status: OrderResultStatus::Failed("below_min_notional".into()),
                order_id: None,
                price,
                shares: sell_shares,
                usd_spent: 0.0,
            });
        }
        let balance_before = self
            .clob
            .get_balance_allowance(&self.creds, self.auth_address, self.sig_type)
            .await
            .context("sell balance_before")?;

        let args = OrderArgs {
            token_id: token_id.into(),
            price: PriceCents::from_dollars(price),
            size_shares: sell_shares,
            side: Side::Sell,
            maker_address: self.signer_addr,
            funder: self.funder,
        };
        let signed = self
            .clob
            .create_order(args, self.sig_type, self.chain_id, &self.wallet, false)
            .await
            .context("sell create_order")?;
        let post_result = self
            .clob
            .post_order(&self.creds, self.auth_address, &signed, OrderType::Gtc)
            .await;
        let order_id = match post_result {
            Ok(r) if r.success => r.order_id.unwrap_or_default(),
            Ok(r) => {
                return Ok(OrderResult {
                    status: OrderResultStatus::Failed(format!("sell post: {:?}", r.error_msg)),
                    order_id: None,
                    price,
                    shares: sell_shares,
                    usd_spent: 0.0,
                });
            }
            Err(e) => {
                // Sell ghost-fill check. Reference: executor.py:388-397.
                sleep(Duration::from_secs(3)).await;
                let bal_after = self
                    .clob
                    .get_balance_allowance(&self.creds, self.auth_address, self.sig_type)
                    .await
                    .unwrap_or(balance_before);
                let received = bal_after - balance_before;
                if received > GHOST_SELL_BAL_DELTA {
                    return Ok(OrderResult {
                        status: OrderResultStatus::GhostFilled,
                        order_id: Some("ghost-sell".into()),
                        price,
                        shares: 0,
                        usd_spent: received,
                    });
                }
                return Ok(OrderResult {
                    status: OrderResultStatus::Failed(format!("sell exception: {}", e)),
                    order_id: None,
                    price,
                    shares: sell_shares,
                    usd_spent: 0.0,
                });
            }
        };

        sleep(Duration::from_secs(3)).await;
        let bal_after = self
            .clob
            .get_balance_allowance(&self.creds, self.auth_address, self.sig_type)
            .await
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
                status,
                order_id: Some(order_id),
                price,
                shares: shares_left.max(0),
                usd_spent: received,
            });
        }
        // The sell GTC limit is resting on the order book. Cancel it so it
        // doesn't fill asynchronously after the strategy has moved on (matches
        // gengar executor.py:433-434). Best-effort: log on failure, don't
        // propagate — the bot can still operate; phantom resolution at the
        // next window boundary will reconcile any retroactive fill.
        if let Err(e) = self
            .clob
            .cancel_order(&self.creds, self.auth_address, &order_id)
            .await
        {
            warn!("[GENGAR][EXEC] cancel UNVERIFIED_SELL order {} failed: {}", order_id, e);
        } else {
            info!("[GENGAR][EXEC] cancelled UNVERIFIED_SELL order {}", order_id);
        }
        Ok(OrderResult {
            status: OrderResultStatus::Failed("UNVERIFIED_SELL".into()),
            order_id: Some(order_id),
            price,
            shares: sell_shares,
            usd_spent: 0.0,
        })
    }
}

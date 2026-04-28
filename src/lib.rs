//! Prediction Market Arbitrage Trading System
//!
//! A high-performance, production-ready arbitrage trading system for cross-platform
//! prediction markets with real-time price monitoring and execution.

pub mod adapters;
pub mod balance;
pub mod cache;
pub mod canonical;
pub mod circuit_breaker;
pub mod config;
pub mod discovery;
pub mod execution;
pub mod fees;
pub mod kalshi;
pub mod polymarket;
pub mod polymarket_clob;
pub mod position_tracker;
pub mod types;
//! Cross-venue market discovery orchestrator.
//!
//! `DiscoveryClient` owns a `Vec<Arc<dyn EventAdapter>>`, runs each adapter's
//! `normalize()`, joins the results via `pair_batch_from`, and persists the
//! merged `MarketPair` set to a JSON cache with TTL-based incremental refresh.

use anyhow::Result;
use serde::{Serialize, Deserialize};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use crate::adapters::{EventAdapter, pair_batch_from};
use crate::types::{DiscoveryResult, MarketPair};

const DISCOVERY_CACHE_PATH: &str = ".discovery_cache.json";
const CACHE_TTL_SECS: u64 = 2 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiscoveryCache {
    timestamp_secs: u64,
    pairs: Vec<MarketPair>,
    known_kalshi_tickers: Vec<String>,
}

impl DiscoveryCache {
    fn new(pairs: Vec<MarketPair>) -> Self {
        let known_kalshi_tickers = pairs.iter()
            .map(|p| p.kalshi_market_ticker.to_string())
            .collect();
        Self {
            timestamp_secs: current_unix_secs(),
            pairs,
            known_kalshi_tickers,
        }
    }

    fn is_expired(&self) -> bool {
        current_unix_secs().saturating_sub(self.timestamp_secs) > CACHE_TTL_SECS
    }

    fn age_secs(&self) -> u64 {
        current_unix_secs().saturating_sub(self.timestamp_secs)
    }

    #[allow(dead_code)]
    fn has_ticker(&self, ticker: &str) -> bool {
        self.known_kalshi_tickers.iter().any(|t| t == ticker)
    }
}

fn current_unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub struct DiscoveryClient {
    adapters: Vec<Arc<dyn EventAdapter>>,
}

impl DiscoveryClient {
    pub fn new(adapters: Vec<Arc<dyn EventAdapter>>) -> Self {
        Self { adapters }
    }

    async fn load_cache() -> Option<DiscoveryCache> {
        let data = tokio::fs::read_to_string(DISCOVERY_CACHE_PATH).await.ok()?;
        serde_json::from_str(&data).ok()
    }

    async fn save_cache(cache: &DiscoveryCache) -> Result<()> {
        let data = serde_json::to_string_pretty(cache)?;
        tokio::fs::write(DISCOVERY_CACHE_PATH, data).await?;
        Ok(())
    }

    pub async fn discover_all(&self) -> DiscoveryResult {
        if let Some(cache) = Self::load_cache().await {
            if !cache.is_expired() {
                info!("📂 Loaded {} pairs from cache (age: {}s)", cache.pairs.len(), cache.age_secs());
                return DiscoveryResult {
                    pairs: cache.pairs,
                    kalshi_events_found: 0,
                    poly_matches: 0,
                    poly_misses: 0,
                    errors: vec![],
                };
            }
            info!("📂 Cache expired (age: {}s), refreshing via adapters...", cache.age_secs());
        } else {
            info!("📂 No cache found, running full discovery via adapters...");
        }

        let result = self.discover_full().await;
        if !result.pairs.is_empty() {
            let cache = DiscoveryCache::new(result.pairs.clone());
            if let Err(e) = Self::save_cache(&cache).await {
                warn!("Failed to save discovery cache: {}", e);
            } else {
                info!("💾 Saved {} pairs to cache", result.pairs.len());
            }
        }
        result
    }

    pub async fn discover_all_force(&self) -> DiscoveryResult {
        info!("🔄 Forced full discovery (ignoring cache)...");
        let result = self.discover_full().await;
        if !result.pairs.is_empty() {
            let cache = DiscoveryCache::new(result.pairs.clone());
            if let Err(e) = Self::save_cache(&cache).await {
                warn!("Failed to save discovery cache: {}", e);
            } else {
                info!("💾 Saved {} pairs to cache", result.pairs.len());
            }
        }
        result
    }

    async fn discover_full(&self) -> DiscoveryResult {
        let mut result = DiscoveryResult::default();
        for adapter in &self.adapters {
            info!("🔍 Adapter '{}' normalizing...", adapter.name());
            match adapter.normalize().await {
                Ok(batch) => {
                    let pairs = pair_batch_from(&**adapter, batch);
                    info!("✅ Adapter '{}' produced {} pairs", adapter.name(), pairs.len());
                    result.poly_matches += pairs.len();
                    result.pairs.extend(pairs);
                }
                Err(e) => {
                    let msg = format!("{} adapter normalize failed: {}", adapter.name(), e);
                    warn!("{}", msg);
                    result.errors.push(msg);
                }
            }
        }
        result.kalshi_events_found = result.pairs.len();
        result
    }
}

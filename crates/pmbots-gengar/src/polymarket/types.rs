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

//! FOMC rate-decision adapter — pairs Kalshi `KXFED*` markets with Polymarket
//! neg-risk outcomes via a current-rate anchor.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.5.

/// Parse a Polymarket FOMC outcome label like `"25 bps cut"` or `"No change"`
/// into a signed delta in basis points. Returns `None` for labels we don't
/// recognize so the caller can log + skip rather than silently default to 0.
///
/// Recognized shapes (case-insensitive, whitespace-tolerant):
///   - `"<N> bps? (cut|decrease|lower)"`   → −N
///   - `"<N> bps? (hike|increase|raise)"`  → +N
///   - `"no change"` | `"hold"`            →  0
pub(crate) fn parse_fomc_delta_bps(label: &str) -> Option<i32> {
    let lower = label.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    if lower == "no change" || lower == "hold" {
        return Some(0);
    }

    let tokens: Vec<&str> = lower.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }

    let n: i32 = tokens[0].parse().ok()?;
    let unit_ok = tokens[1] == "bp" || tokens[1] == "bps";
    if !unit_ok {
        return None;
    }
    let direction = tokens[2];
    let signed = match direction {
        "cut" | "decrease" | "lower" => -n,
        "hike" | "increase" | "raise" => n,
        _ => return None,
    };
    Some(signed)
}

#[cfg(test)]
mod parser_tests {
    use super::*;

    #[test]
    fn parses_25_bps_cut() {
        assert_eq!(parse_fomc_delta_bps("25 bps cut"), Some(-25));
    }

    #[test]
    fn parses_50_bps_decrease() {
        assert_eq!(parse_fomc_delta_bps("50 bps decrease"), Some(-50));
    }

    #[test]
    fn parses_25_bps_hike() {
        assert_eq!(parse_fomc_delta_bps("25 bps hike"), Some(25));
    }

    #[test]
    fn parses_no_change() {
        assert_eq!(parse_fomc_delta_bps("No change"), Some(0));
    }

    #[test]
    fn parses_hold_synonym() {
        assert_eq!(parse_fomc_delta_bps("hold"), Some(0));
    }

    #[test]
    fn parses_75_bps_increase() {
        assert_eq!(parse_fomc_delta_bps("75 bps increase"), Some(75));
    }

    #[test]
    fn parses_with_extra_whitespace() {
        assert_eq!(parse_fomc_delta_bps("  25  bps   cut  "), Some(-25));
    }

    #[test]
    fn parses_case_insensitive() {
        assert_eq!(parse_fomc_delta_bps("25 BPS HIKE"), Some(25));
        assert_eq!(parse_fomc_delta_bps("NO CHANGE"), Some(0));
    }

    #[test]
    fn parses_bp_singular() {
        assert_eq!(parse_fomc_delta_bps("25 bp cut"), Some(-25));
    }

    #[test]
    fn rejects_unknown_label() {
        assert_eq!(parse_fomc_delta_bps("rates go to the moon"), None);
    }

    #[test]
    fn rejects_label_without_direction() {
        assert_eq!(parse_fomc_delta_bps("25 bps"), None);
    }

    #[test]
    fn rejects_empty_string() {
        assert_eq!(parse_fomc_delta_bps(""), None);
    }
}

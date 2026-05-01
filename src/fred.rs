//! FRED API client for the fed-funds target lower-bound anchor used by
//! `FomcAdapter`. Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.5.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

const FRED_OBSERVATIONS_URL: &str = "https://api.stlouisfed.org/fred/series/observations";
const SERIES_ID: &str = "DFEDTARL";

#[derive(Debug, Deserialize)]
struct Observations {
    observations: Vec<Observation>,
}

#[derive(Debug, Deserialize)]
struct Observation {
    #[allow(dead_code)]
    date: String,
    value: String,
}

/// Parse a FRED observations JSON body into integer basis points.
/// Public for testing; production code goes through `fetch_fed_lower_bound_bps`.
pub(crate) fn parse_lower_bound_bps(body: &str) -> Result<i32> {
    let parsed: Observations =
        serde_json::from_str(body).context("FRED observations JSON malformed")?;
    let latest = parsed
        .observations
        .last()
        .ok_or_else(|| anyhow!("FRED observations array empty"))?;
    if latest.value == "." {
        return Err(anyhow!("FRED returned missing-value '.' for latest observation"));
    }
    let pct: f64 = latest
        .value
        .parse()
        .with_context(|| format!("FRED value not a float: {:?}", latest.value))?;
    Ok((pct * 100.0).round() as i32)
}

/// Fetch the latest fed-funds target lower-bound from FRED and return it
/// in integer basis points. The endpoint is public; the API key is optional
/// and only buys a per-key rate-limit budget.
pub async fn fetch_fed_lower_bound_bps(
    http: &reqwest::Client,
    api_key: Option<&str>,
) -> Result<i32> {
    let mut req = http.get(FRED_OBSERVATIONS_URL).query(&[
        ("series_id", SERIES_ID),
        ("file_type", "json"),
        ("sort_order", "desc"),
        ("limit", "1"),
    ]);
    if let Some(key) = api_key {
        req = req.query(&[("api_key", key)]);
    }
    let resp = req.send().await.context("FRED request failed")?;
    if !resp.status().is_success() {
        return Err(anyhow!(
            "FRED HTTP {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_default()
        ));
    }
    let body = resp.text().await.context("FRED body not UTF-8")?;
    parse_lower_bound_bps(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_recorded_observation_to_bps() {
        let body = r#"{
            "observations": [
                { "date": "2026-04-28", "value": "4.25" }
            ]
        }"#;
        let bps = parse_lower_bound_bps(body).unwrap();
        assert_eq!(bps, 425);
    }

    #[test]
    fn parses_zero_lower_bound() {
        let body = r#"{
            "observations": [
                { "date": "2020-04-01", "value": "0.00" }
            ]
        }"#;
        assert_eq!(parse_lower_bound_bps(body).unwrap(), 0);
    }

    #[test]
    fn rejects_missing_observation_with_period_value() {
        let body = r#"{ "observations": [ { "date": "2026-04-28", "value": "." } ] }"#;
        assert!(parse_lower_bound_bps(body).is_err());
    }

    #[test]
    fn rejects_empty_observations_array() {
        let body = r#"{ "observations": [] }"#;
        assert!(parse_lower_bound_bps(body).is_err());
    }

    #[test]
    fn rounds_half_to_nearest_bps() {
        let body = r#"{ "observations": [ { "date": "x", "value": "4.255" } ] }"#;
        assert_eq!(parse_lower_bound_bps(body).unwrap(), 426);
    }
}

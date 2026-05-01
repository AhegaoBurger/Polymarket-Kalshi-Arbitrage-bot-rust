//! Reads `.ai_matches.json` produced by the standalone Python sidecar and
//! emits `MarketPair` rows for AI-matched pairs.
//!
//! Spec: docs/superpowers/specs/2026-04-21-multi-category-matching-design.md §4.6.4 + §4.8.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::path::Path;
use std::sync::Arc;

use crate::fees::{MatchSource, PolyCategory};
use crate::types::{MarketPair, MarketType};

const DEFAULT_MATCHES_PATH: &str = ".ai_matches.json";

#[derive(Debug, Deserialize)]
struct AiMatchesFile {
    generated_at: DateTime<Utc>,
    model: String,
    #[allow(dead_code)]
    embedding_model: String,
    pairs: Vec<AiMatch>,
}

#[derive(Debug, Deserialize)]
struct AiMatch {
    kalshi_market_ticker: String,
    poly_condition_id: String,
    poly_yes_token: String,
    poly_no_token: String,
    category: String,
    #[allow(dead_code)]
    event_type: String,
    confidence: f32,
    description: String,
}

/// Load and validate the AI matches file. Returns `Ok(vec![])` if the file
/// is missing — that's a normal "sidecar hasn't run yet" state, not an error.
/// Returns `Err` if the file exists but is older than `max_age_secs` or malformed.
pub fn load_ai_matches(
    path: Option<&Path>,
    max_age_secs: u64,
    now: DateTime<Utc>,
) -> Result<Vec<MarketPair>> {
    let path = path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| Path::new(DEFAULT_MATCHES_PATH).to_path_buf());

    if !path.exists() {
        tracing::info!("[AI] no {} found; sidecar has not run yet", path.display());
        return Ok(vec![]);
    }

    let body = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: AiMatchesFile = serde_json::from_str(&body)
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let age_secs = now.signed_duration_since(parsed.generated_at).num_seconds();
    if age_secs < 0 {
        return Err(anyhow!(
            "{} generated_at is in the future ({})",
            path.display(),
            parsed.generated_at
        ));
    }
    if (age_secs as u64) > max_age_secs {
        return Err(anyhow!(
            "{} is stale: {}s old (max {}s)",
            path.display(),
            age_secs,
            max_age_secs
        ));
    }

    let model_arc: Arc<str> = Arc::from(parsed.model.as_str());
    let mut out = Vec::with_capacity(parsed.pairs.len());
    for p in parsed.pairs {
        let category = parse_category(&p.category);
        out.push(MarketPair {
            pair_id: Arc::from(format!("{}-{}", p.kalshi_market_ticker, p.poly_condition_id)),
            league: Arc::from("ai"),
            market_type: MarketType::Moneyline,
            description: Arc::from(p.description),
            kalshi_event_ticker: Arc::from(""),
            kalshi_market_ticker: Arc::from(p.kalshi_market_ticker),
            poly_slug: Arc::from(""),
            poly_yes_token: Arc::from(p.poly_yes_token),
            poly_no_token: Arc::from(p.poly_no_token),
            poly_condition_id: Arc::from(p.poly_condition_id),
            line_value: None,
            team_suffix: None,
            category,
            match_source: MatchSource::Ai {
                confidence: p.confidence,
                model: model_arc.clone(),
            },
        });
    }
    Ok(out)
}

fn parse_category(s: &str) -> PolyCategory {
    match s {
        "Crypto" => PolyCategory::Crypto,
        "Mentions" => PolyCategory::Mentions,
        "Economics" => PolyCategory::Economics,
        "Culture" => PolyCategory::Culture,
        "Weather" => PolyCategory::Weather,
        "Finance" => PolyCategory::Finance,
        "Politics" => PolyCategory::Politics,
        "Tech" => PolyCategory::Tech,
        "Sports" => PolyCategory::Sports,
        "Geopolitical" => PolyCategory::Geopolitical,
        _ => PolyCategory::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_fixture(body: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f
    }

    fn fresh_payload(generated_at: &str) -> String {
        format!(
            r#"{{
            "generated_at": "{generated_at}",
            "model": "claude-opus-4-7",
            "embedding_model": "sentence-transformers/all-MiniLM-L6-v2",
            "version": 1,
            "pairs": [
                {{
                    "kalshi_market_ticker": "KXPRES-USA-2028-DEM",
                    "poly_condition_id": "0xCONDA",
                    "poly_yes_token": "0xYES",
                    "poly_no_token": "0xNO",
                    "category": "Politics",
                    "event_type": "Election",
                    "confidence": 0.95,
                    "description": "2028 US presidential — Democratic candidate"
                }}
            ]
        }}"#
        )
    }

    #[test]
    fn loads_one_pair_when_file_is_fresh() {
        let now = Utc::now();
        let f = write_fixture(&fresh_payload(&now.to_rfc3339()));
        let pairs = load_ai_matches(Some(f.path()), 86_400, now).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].kalshi_market_ticker.as_ref(), "KXPRES-USA-2028-DEM");
        assert_eq!(pairs[0].category, PolyCategory::Politics);
        match &pairs[0].match_source {
            MatchSource::Ai { confidence, model } => {
                assert!((*confidence - 0.95).abs() < 1e-6);
                assert_eq!(model.as_ref(), "claude-opus-4-7");
            }
            _ => panic!("expected MatchSource::Ai"),
        }
    }

    #[test]
    fn rejects_stale_file_beyond_max_age() {
        let now = Utc::now();
        let stale = now - Duration::seconds(86_500);
        let f = write_fixture(&fresh_payload(&stale.to_rfc3339()));
        let err = load_ai_matches(Some(f.path()), 86_400, now).unwrap_err();
        assert!(err.to_string().contains("stale"));
    }

    #[test]
    fn rejects_future_dated_file() {
        let now = Utc::now();
        let future = now + Duration::seconds(60);
        let f = write_fixture(&fresh_payload(&future.to_rfc3339()));
        let err = load_ai_matches(Some(f.path()), 86_400, now).unwrap_err();
        assert!(err.to_string().contains("future"));
    }

    #[test]
    fn missing_file_returns_empty_vec_not_error() {
        let pairs = load_ai_matches(
            Some(Path::new("/nonexistent/.ai_matches.json")),
            86_400,
            Utc::now(),
        )
        .unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn unknown_category_falls_back_to_unknown() {
        let now = Utc::now();
        let body = format!(
            r#"{{
            "generated_at": "{}",
            "model": "x",
            "embedding_model": "y",
            "version": 1,
            "pairs": [{{
                "kalshi_market_ticker": "K",
                "poly_condition_id": "0xC",
                "poly_yes_token": "y",
                "poly_no_token": "n",
                "category": "Astronomy",
                "event_type": "Other",
                "confidence": 0.91,
                "description": "x"
            }}]
        }}"#,
            now.to_rfc3339()
        );
        let f = write_fixture(&body);
        let pairs = load_ai_matches(Some(f.path()), 86_400, now).unwrap();
        assert_eq!(pairs[0].category, PolyCategory::Unknown);
    }
}

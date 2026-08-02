//! Transcript usage extraction and request-level deduplication.
//!
//! Agent transcripts repeat the same usage object across adjacent records.
//! Summing lines is therefore wrong; this module counts a request once and
//! keeps pricing outside the parser so a caller can pin the table version it
//! used for a report.

use std::collections::{BTreeMap, HashSet};

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenRates {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: f64,
    pub cache_creation_per_million: f64,
}

impl TokenRates {
    fn cost(self, usage: &Usage) -> f64 {
        (usage.input_tokens as f64 * self.input_per_million
            + usage.output_tokens as f64 * self.output_per_million
            + usage.cache_read_input_tokens as f64 * self.cache_read_per_million
            + usage.cache_creation_input_tokens as f64 * self.cache_creation_per_million)
            / 1_000_000.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PricingTable {
    pub version: String,
    default: TokenRates,
    models: BTreeMap<String, TokenRates>,
}

impl PricingTable {
    pub fn new(version: impl Into<String>, default: TokenRates) -> Self {
        Self {
            version: version.into(),
            default,
            models: BTreeMap::new(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>, rates: TokenRates) -> Self {
        self.models.insert(model.into(), rates);
        self
    }

    fn rates_for(&self, model: Option<&str>) -> TokenRates {
        model
            .and_then(|name| self.models.get(name).copied())
            .unwrap_or(self.default)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageTotals {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

impl UsageTotals {
    pub fn add(&mut self, usage: Usage) {
        self.requests = self.requests.saturating_add(1);
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.cache_read_input_tokens = self
            .cache_read_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
        self.cache_creation_input_tokens = self
            .cache_creation_input_tokens
            .saturating_add(usage.cache_creation_input_tokens);
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum TranscriptError {
    #[error("invalid transcript JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq)]
struct UsageRecord {
    request_id: Option<String>,
    model: Option<String>,
    usage: Usage,
}

/// Accumulates usage records while counting each request id at most once.
#[derive(Debug, Clone)]
pub struct TranscriptAccumulator {
    pricing: PricingTable,
    seen_request_ids: HashSet<String>,
    totals: UsageTotals,
    cost_usd: f64,
}

impl TranscriptAccumulator {
    pub fn new(pricing: PricingTable) -> Self {
        Self {
            pricing,
            seen_request_ids: HashSet::new(),
            totals: UsageTotals::default(),
            cost_usd: 0.0,
        }
    }

    pub fn pricing_version(&self) -> &str {
        &self.pricing.version
    }

    pub fn totals(&self) -> UsageTotals {
        self.totals
    }

    pub fn cost_usd(&self) -> f64 {
        self.cost_usd
    }

    /// Ingest one JSONL record. Returns `true` only when it changed totals.
    pub fn ingest_line(&mut self, line: &str) -> Result<bool, TranscriptError> {
        let value: Value = serde_json::from_str(line)?;
        let Some(record) = find_usage(&value, None, None) else {
            return Ok(false);
        };
        if let Some(request_id) = &record.request_id {
            if !self.seen_request_ids.insert(request_id.clone()) {
                return Ok(false);
            }
        }
        self.totals.add(record.usage);
        self.cost_usd += self
            .pricing
            .rates_for(record.model.as_deref())
            .cost(&record.usage);
        Ok(true)
    }
}

fn find_usage(
    value: &Value,
    inherited_request_id: Option<&str>,
    inherited_model: Option<&str>,
) -> Option<UsageRecord> {
    match value {
        Value::Object(object) => {
            let request_id = string_field(object, &["requestId", "request_id"])
                .or_else(|| inherited_request_id.map(str::to_owned));
            let model =
                string_field(object, &["model"]).or_else(|| inherited_model.map(str::to_owned));
            if let Some(usage) = object.get("usage").and_then(Value::as_object) {
                return Some(UsageRecord {
                    request_id,
                    model,
                    usage: Usage {
                        input_tokens: number_field(usage, &["input_tokens", "inputTokens"]),
                        output_tokens: number_field(usage, &["output_tokens", "outputTokens"]),
                        cache_read_input_tokens: number_field(
                            usage,
                            &["cache_read_input_tokens", "cacheReadInputTokens"],
                        ),
                        cache_creation_input_tokens: number_field(
                            usage,
                            &["cache_creation_input_tokens", "cacheCreationInputTokens"],
                        ),
                    },
                });
            }
            object
                .values()
                .find_map(|child| find_usage(child, request_id.as_deref(), model.as_deref()))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|child| find_usage(child, inherited_request_id, inherited_model)),
        _ => None,
    }
}

fn string_field(object: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_str).map(str::to_owned))
}

fn number_field(object: &serde_json::Map<String, Value>, names: &[&str]) -> u64 {
    names
        .iter()
        .find_map(|name| {
            object.get(*name).and_then(|value| {
                value
                    .as_u64()
                    .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
            })
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pricing() -> PricingTable {
        PricingTable::new(
            "test-2026-08",
            TokenRates {
                input_per_million: 1.0,
                output_per_million: 2.0,
                cache_read_per_million: 0.1,
                cache_creation_per_million: 0.2,
            },
        )
    }

    #[test]
    fn repeated_request_id_counts_once_and_uses_nested_usage() {
        let line = r#"{"requestId":"r1","model":"default","message":{"usage":{"input_tokens":1000000,"output_tokens":500000,"cache_read_input_tokens":100000,"cache_creation_input_tokens":50000}}}"#;
        let mut accumulator = TranscriptAccumulator::new(pricing());
        assert!(accumulator.ingest_line(line).unwrap());
        assert!(!accumulator.ingest_line(line).unwrap());
        assert_eq!(
            accumulator.totals(),
            UsageTotals {
                requests: 1,
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                cache_read_input_tokens: 100_000,
                cache_creation_input_tokens: 50_000,
            }
        );
        assert!((accumulator.cost_usd() - 2.02).abs() < f64::EPSILON);
        assert_eq!(accumulator.pricing_version(), "test-2026-08");
    }

    #[test]
    fn records_without_request_id_are_not_silently_dropped() {
        let mut accumulator = TranscriptAccumulator::new(pricing());
        assert!(accumulator
            .ingest_line(r#"{"usage":{"input_tokens":3}}"#)
            .unwrap());
        assert!(accumulator
            .ingest_line(r#"{"usage":{"input_tokens":4}}"#)
            .unwrap());
        assert_eq!(accumulator.totals().requests, 2);
        assert_eq!(accumulator.totals().input_tokens, 7);
    }

    #[test]
    fn malformed_json_is_reported() {
        assert!(matches!(
            TranscriptAccumulator::new(pricing()).ingest_line("{"),
            Err(TranscriptError::Json(_))
        ));
    }
}

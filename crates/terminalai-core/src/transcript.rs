//! Transcript usage extraction and request-level deduplication.
//!
//! Agent transcripts repeat the same usage object across adjacent records.
//! Summing lines is therefore wrong; this module counts a request once and
//! keeps pricing outside the parser so a caller can pin the table version it
//! used for a report.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::OnceLock;

use serde_json::Value;

/// Per-million-token rates for one model.
///
/// Four fields could not express what the transcripts on this machine actually
/// carry, so the first number the fleet reported would have been confidently
/// wrong — and a spend figure that under-reports is worse than none, because
/// admission control is meant to act on it. Cache writes are billed at two
/// different rates depending on TTL, and both a geography and a speed multiplier
/// ride on the same record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenRates {
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: f64,
    /// 5-minute ephemeral cache write. 1.25x base input, per the published rates.
    pub cache_write_5m_per_million: f64,
    /// 1-hour ephemeral cache write. 2x base input.
    pub cache_write_1h_per_million: f64,
}

impl TokenRates {
    /// Build from a base input rate using the published cache-write multipliers.
    /// Used for the hardcoded fallback and for any vendored entry that does not
    /// publish an explicit 1-hour rate.
    pub fn from_base(
        input_per_million: f64,
        output_per_million: f64,
        cache_read_per_million: f64,
    ) -> Self {
        Self {
            input_per_million,
            output_per_million,
            cache_read_per_million,
            cache_write_5m_per_million: input_per_million * CACHE_WRITE_5M_MULTIPLIER,
            cache_write_1h_per_million: input_per_million * CACHE_WRITE_1H_MULTIPLIER,
        }
    }

    fn cost(self, usage: &Usage) -> f64 {
        let (write_5m, write_1h) = usage.cache_write_split();
        let base = usage.input_tokens as f64 * self.input_per_million
            + usage.output_tokens as f64 * self.output_per_million
            + usage.cache_read_input_tokens as f64 * self.cache_read_per_million
            + write_5m as f64 * self.cache_write_5m_per_million
            + write_1h as f64 * self.cache_write_1h_per_million;
        base / 1_000_000.0 * usage.multiplier()
    }
}

/// Published cache-write premiums over the base input rate.
pub const CACHE_WRITE_5M_MULTIPLIER: f64 = 1.25;
pub const CACHE_WRITE_1H_MULTIPLIER: f64 = 2.0;
/// Regional inference premium, charged when a request is pinned to a geography.
pub const GEO_MULTIPLIER: f64 = 1.1;
/// Priority-speed premium.
pub const FAST_SPEED_MULTIPLIER: f64 = 1.5;

/// How old a vendored price table may be before figures computed against it
/// stop being presented as current.
///
/// A quarter. Model prices move on announcement rather than on a schedule, so
/// no threshold is exactly right — this one is stated rather than tuned, and
/// the point is that a figure priced against a table months out of date should
/// not be reported with the same confidence as one priced against a current
/// table. Nothing is fetched to check; the age comes from the embedded date.
pub const PRICING_STALE_AFTER_DAYS: i64 = 90;

#[derive(Debug, Clone, PartialEq)]
pub struct PricingTable {
    pub version: String,
    /// The upstream commit date the snapshot was taken from, `YYYY-MM-DD`.
    ///
    /// Kept apart from `version`, which is a human string. Nothing could age
    /// the table because the only date it carried was inside prose.
    pub source_committed: Option<String>,
    default: TokenRates,
    models: BTreeMap<String, TokenRates>,
}

impl PricingTable {
    pub fn new(version: impl Into<String>, default: TokenRates) -> Self {
        Self {
            version: version.into(),
            source_committed: None,
            default,
            models: BTreeMap::new(),
        }
    }

    /// A table stamped with the upstream commit date it came from.
    pub fn committed(mut self, date: impl Into<String>) -> Self {
        self.source_committed = Some(date.into());
        self
    }

    fn rates_for(&self, model: Option<&str>) -> TokenRates {
        let Some(name) = model else {
            return self.default;
        };
        if let Some(rates) = self.models.get(name) {
            return *rates;
        }
        // Both vendors ship dated and region-prefixed aliases of the same model
        // (`claude-opus-5-20260101`, `us.anthropic.claude-opus-5`). Fall back to
        // the longest vendored name the requested model contains, so a new dated
        // build prices correctly instead of dropping to the default.
        self.models
            .iter()
            .filter(|(known, _)| name.contains(known.as_str()))
            .max_by_key(|(known, _)| known.len())
            .map(|(_, rates)| *rates)
            .unwrap_or(self.default)
    }

    /// The vendored price table.
    ///
    /// Parsed once from a snapshot embedded in the binary — never fetched at
    /// runtime, so a build is reproducible and an offline machine prices the same
    /// as a connected one. A snapshot that fails to parse degrades to
    /// [`PricingTable::fallback`] rather than to a panic or a zero.
    pub fn vendored() -> &'static PricingTable {
        static TABLE: OnceLock<PricingTable> = OnceLock::new();
        TABLE.get_or_init(|| Self::parse_snapshot(VENDORED_PRICES).unwrap_or_else(Self::fallback))
    }

    /// Rates to use when the vendored snapshot is unusable. Deliberately the
    /// most expensive model either CLI runs: under-reporting spend is the failure
    /// that matters, because admission control acts on the number.
    pub fn fallback() -> PricingTable {
        PricingTable::new(
            "fallback",
            TokenRates::from_base(5.0, 25.0, 0.5),
        )
    }

    fn parse_snapshot(snapshot: &str) -> Option<PricingTable> {
        let value: Value = serde_json::from_str(snapshot).ok()?;
        let version = value.get("source_committed")?.as_str()?;
        let retrieved = value.get("retrieved")?.as_str()?;
        let mut table = PricingTable::new(
            format!("litellm {version} (vendored {retrieved})"),
            Self::fallback().default,
        )
        .committed(version);
        for (model, entry) in value.get("models")?.as_object()? {
            let per_million = |name: &str| {
                entry
                    .get(name)
                    .and_then(Value::as_f64)
                    .map(|per_token| per_token * 1_000_000.0)
            };
            let (Some(input), Some(output)) = (
                per_million("input_cost_per_token"),
                per_million("output_cost_per_token"),
            ) else {
                continue;
            };
            let read = per_million("cache_read_input_token_cost").unwrap_or(input * 0.1);
            let mut rates = TokenRates::from_base(input, output, read);
            if let Some(write) = per_million("cache_creation_input_token_cost") {
                rates.cache_write_5m_per_million = write;
            }
            if let Some(write) = per_million("cache_creation_input_token_cost_above_1hr") {
                rates.cache_write_1h_per_million = write;
            }
            table.models.insert(model.clone(), rates);
        }
        (!table.models.is_empty()).then_some(table)
    }
}

/// A commit-pinned snapshot of LiteLLM's `model_prices_and_context_window.json`,
/// trimmed to the models either CLI can run. Neither vendor publishes a
/// machine-readable price table, and LiteLLM's is the only maintained one.
const VENDORED_PRICES: &str = include_str!("../pricing/model-prices.json");

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    /// Total cache writes. The per-TTL split below is authoritative when present;
    /// this stays the total so the token counters remain comparable.
    pub cache_creation_input_tokens: u64,
    /// `usage.cache_creation.ephemeral_5m_input_tokens`, when the record breaks
    /// the write down by TTL.
    pub cache_write_5m_tokens: Option<u64>,
    /// `usage.cache_creation.ephemeral_1h_input_tokens`.
    pub cache_write_1h_tokens: Option<u64>,
    /// `usage.inference_geo` — a real region means the regional premium applies.
    /// `"not_available"` and absence both mean it does not.
    pub geo_pinned: bool,
    /// `usage.speed == "fast"`.
    pub fast_speed: bool,
}

impl Usage {
    /// Split cache writes into the two billed tiers.
    ///
    /// When a record breaks the write down by TTL the split is used as given.
    /// When it does not, the whole write is charged at the 5-minute rate — the
    /// cheaper of the two, and the default TTL — rather than silently assuming
    /// the premium tier.
    /// Everything the provider counted as the prompt for this request: fresh
    /// input, cache reads and cache writes. A cache hit still occupies the
    /// context window — it is only billed differently.
    pub fn prompt_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_read_input_tokens)
            .saturating_add(self.cache_creation_input_tokens)
    }

    fn cache_write_split(&self) -> (u64, u64) {
        match (self.cache_write_5m_tokens, self.cache_write_1h_tokens) {
            (None, None) => (self.cache_creation_input_tokens, 0),
            (five, hour) => (five.unwrap_or(0), hour.unwrap_or(0)),
        }
    }

    fn multiplier(&self) -> f64 {
        let mut multiplier = 1.0;
        if self.geo_pinned {
            multiplier *= GEO_MULTIPLIER;
        }
        if self.fast_speed {
            multiplier *= FAST_SPEED_MULTIPLIER;
        }
        multiplier
    }
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

/// How many request ids stay deduplicable. A transcript repeats a usage object
/// across adjacent records, so the window only has to outlive that adjacency —
/// an unbounded set would grow for the life of the daemon.
pub const MAX_TRACKED_REQUEST_IDS: usize = 4096;

/// Accumulates usage records while counting each request id at most once.
#[derive(Debug, Clone)]
pub struct TranscriptAccumulator {
    pricing: PricingTable,
    seen_request_ids: HashSet<String>,
    /// Insertion order for `seen_request_ids`, so the oldest id can be evicted.
    request_id_order: VecDeque<String>,
    /// Usage summed from records that carry a request id, deduped by it.
    incremental: UsageTotals,
    incremental_cost_usd: f64,
    /// The latest cumulative figure from a source that reports session totals
    /// rather than per-request deltas. Replaced, never summed.
    cumulative: Usage,
    cumulative_requests: u64,
    cumulative_cost_usd: f64,
    /// The prompt size of the most recent request seen, which is what occupies
    /// the model's context window. Replaced on every request, never summed —
    /// see [`crate::context`] for why the totals above cannot answer this.
    latest_prompt_tokens: Option<u64>,
}

impl TranscriptAccumulator {
    pub fn new(pricing: PricingTable) -> Self {
        Self {
            pricing,
            seen_request_ids: HashSet::new(),
            request_id_order: VecDeque::new(),
            incremental: UsageTotals::default(),
            incremental_cost_usd: 0.0,
            cumulative: Usage::default(),
            cumulative_requests: 0,
            cumulative_cost_usd: 0.0,
            latest_prompt_tokens: None,
        }
    }

    /// An accumulator priced from the vendored, commit-pinned table.
    pub fn with_vendored_pricing() -> Self {
        Self::new(PricingTable::vendored().clone())
    }

    pub fn pricing_version(&self) -> &str {
        &self.pricing.version
    }

    /// Combined usage: per-request records summed, plus the latest cumulative
    /// figure. The two sources never describe the same tokens — one agent
    /// reports request ids and the other reports session totals — so adding
    /// them is correct and neither double-counts within itself.
    pub fn totals(&self) -> UsageTotals {
        let mut totals = self.incremental;
        if self.cumulative_requests > 0 {
            totals.requests = totals.requests.saturating_add(1);
            totals.input_tokens = totals.input_tokens.saturating_add(self.cumulative.input_tokens);
            totals.output_tokens = totals
                .output_tokens
                .saturating_add(self.cumulative.output_tokens);
            totals.cache_read_input_tokens = totals
                .cache_read_input_tokens
                .saturating_add(self.cumulative.cache_read_input_tokens);
            totals.cache_creation_input_tokens = totals
                .cache_creation_input_tokens
                .saturating_add(self.cumulative.cache_creation_input_tokens);
        }
        totals
    }

    pub fn cost_usd(&self) -> f64 {
        self.incremental_cost_usd + self.cumulative_cost_usd
    }

    /// Tokens occupying the model's context window as of the last request, or
    /// `None` before one has been priced.
    ///
    /// The prompt the provider counted: fresh input, cache reads and cache
    /// writes. Output is excluded deliberately — it is not part of any request
    /// yet, so including it would make this a projection of the next turn
    /// rather than a measurement of the last one. The reading therefore lags by
    /// one reply, which is the honest error direction: it under-reports
    /// pressure rather than inventing it.
    pub fn context_tokens(&self) -> Option<u64> {
        self.latest_prompt_tokens
    }

    /// Ingest one JSONL record. Returns `true` only when it changed totals.
    pub fn ingest_line(&mut self, line: &str) -> Result<bool, TranscriptError> {
        let value: Value = serde_json::from_str(line)?;
        let Some(record) = find_usage(&value, None, None) else {
            return Ok(false);
        };
        let Some(request_id) = &record.request_id else {
            // No request id means Codex, whose rollout reports the session's
            // *cumulative* usage on every turn. Adding those would multiply the
            // total by the number of turns, so a cumulative record replaces
            // rather than accumulates.
            return Ok(self.absorb_cumulative(record));
        };
        if !self.seen_request_ids.insert(request_id.clone()) {
            return Ok(false);
        }
        self.request_id_order.push_back(request_id.clone());
        while self.request_id_order.len() > MAX_TRACKED_REQUEST_IDS {
            if let Some(evicted) = self.request_id_order.pop_front() {
                self.seen_request_ids.remove(&evicted);
            }
        }
        self.incremental.add(record.usage);
        // Replaced, not accumulated. Only the request-id path sets this: the
        // cumulative path below reports session totals, and a session total is
        // never a window occupancy.
        self.latest_prompt_tokens = Some(record.usage.prompt_tokens());
        self.incremental_cost_usd += self
            .pricing
            .rates_for(record.model.as_deref())
            .cost(&record.usage);
        Ok(true)
    }

    /// Replace the running cumulative figure, if this record advances it.
    ///
    /// A cumulative counter only ever grows within one session. A record that
    /// reports *less* than the last one is either a replay of an earlier line or
    /// a different session's file, and taking it would silently walk the total
    /// backwards — so the larger figure wins.
    fn absorb_cumulative(&mut self, record: UsageRecord) -> bool {
        let total = record.usage.input_tokens.saturating_add(record.usage.output_tokens);
        let seen = self
            .cumulative
            .input_tokens
            .saturating_add(self.cumulative.output_tokens);
        if total <= seen && self.cumulative_requests > 0 {
            return false;
        }
        self.cumulative = record.usage;
        self.cumulative_requests = self.cumulative_requests.saturating_add(1);
        self.cumulative_cost_usd = self
            .pricing
            .rates_for(record.model.as_deref())
            .cost(&record.usage);
        true
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
                let cache_creation = usage.get("cache_creation").and_then(Value::as_object);
                let tier = |names: &[&str]| {
                    cache_creation.and_then(|split| {
                        names.iter().find_map(|name| {
                            split.get(*name).and_then(Value::as_u64)
                        })
                    })
                };
                let geo = usage.get("inference_geo").and_then(Value::as_str);
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
                        cache_write_5m_tokens: tier(&[
                            "ephemeral_5m_input_tokens",
                            "ephemeral5mInputTokens",
                        ]),
                        cache_write_1h_tokens: tier(&[
                            "ephemeral_1h_input_tokens",
                            "ephemeral1hInputTokens",
                        ]),
                        // "not_available" is what an unpinned request reports, and
                        // it must not be read as a region.
                        geo_pinned: geo
                            .map(|value| {
                                !value.is_empty()
                                    && !value.eq_ignore_ascii_case("not_available")
                                    && !value.eq_ignore_ascii_case("none")
                            })
                            .unwrap_or(false),
                        fast_speed: usage
                            .get("speed")
                            .and_then(Value::as_str)
                            .map(|value| value.eq_ignore_ascii_case("fast"))
                            .unwrap_or(false),
                    },
                });
            }
            // `serde_json::Map` is a `BTreeMap` here — no `preserve_order` feature
            // — so a bare `.values()` scan resolves nested objects alphabetically
            // rather than in document order, and would pick whichever `usage`
            // happened to sort first. Look inside the known carriers first.
            const CARRIERS: [&str; 6] = ["message", "response", "payload", "data", "event_msg", "info"];
            CARRIERS
                .iter()
                .filter_map(|name| object.get(*name))
                .chain(
                    object
                        .iter()
                        .filter(|(name, _)| !CARRIERS.contains(&name.as_str()))
                        .map(|(_, child)| child),
                )
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
                cache_write_5m_per_million: 0.2,
                cache_write_1h_per_million: 0.4,
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
    fn records_without_request_id_are_treated_as_cumulative_not_summed() {
        // A record with no request id is Codex-shaped, and Codex reports the
        // session's *running total* on every turn. This used to sum them, which
        // multiplied the real figure by the number of turns; the later figure
        // now replaces the earlier one. Still absorbed, never dropped: both
        // calls report that they changed something.
        let mut accumulator = TranscriptAccumulator::new(pricing());
        assert!(accumulator
            .ingest_line(r#"{"usage":{"input_tokens":3}}"#)
            .unwrap());
        assert!(accumulator
            .ingest_line(r#"{"usage":{"input_tokens":4}}"#)
            .unwrap());
        assert_eq!(accumulator.totals().input_tokens, 4, "4, not 3 + 4");
        assert_eq!(accumulator.totals().requests, 1);
    }

    #[test]
    fn a_cumulative_record_and_a_per_request_record_both_count() {
        // The two sources never describe the same tokens: one agent reports
        // request ids, the other reports session totals. A fleet running both
        // must show the sum of the two, not whichever arrived last.
        let mut accumulator = TranscriptAccumulator::new(pricing());
        assert!(accumulator
            .ingest_line(r#"{"requestId":"a","usage":{"input_tokens":10}}"#)
            .unwrap());
        assert!(accumulator
            .ingest_line(r#"{"usage":{"input_tokens":100}}"#)
            .unwrap());
        assert_eq!(accumulator.totals().input_tokens, 110);
        assert_eq!(accumulator.totals().requests, 2);
    }

    /// The exact shape of a record from `~/.claude/projects/<slug>/<uuid>.jsonl`
    /// on this machine, 2026-08-03. Verbatim so a change in the upstream contract
    /// shows up here rather than as a wrong number in the fleet header.
    const REAL_RECORD: &str = r#"{"requestId":"req_011CdgDYoDhbMZCJPANQquKm","type":"assistant","gitBranch":"main","message":{"model":"claude-opus-5","usage":{"input_tokens":2,"cache_creation_input_tokens":1000000,"cache_read_input_tokens":25695,"output_tokens":171,"server_tool_use":{"web_search_requests":0},"service_tier":"standard","cache_creation":{"ephemeral_1h_input_tokens":1000000,"ephemeral_5m_input_tokens":0},"inference_geo":"not_available","speed":"standard"}}}"#;

    #[test]
    fn a_real_record_prices_its_cache_writes_at_the_one_hour_rate() {
        let mut accumulator = TranscriptAccumulator::new(pricing());
        assert!(accumulator.ingest_line(REAL_RECORD).unwrap());
        // 1M tokens written at the 1-hour rate, not the 5-minute one. Charging
        // the whole write at 0.2 would under-report by half.
        let cost = accumulator.cost_usd();
        let expected = (2.0 * 1.0 + 171.0 * 2.0 + 25695.0 * 0.1 + 1_000_000.0 * 0.4) / 1_000_000.0;
        assert!(
            (cost - expected).abs() < 1e-12,
            "expected {expected}, got {cost}"
        );
    }

    #[test]
    fn geo_and_speed_multipliers_are_applied() {
        let base = {
            let mut accumulator = TranscriptAccumulator::new(pricing());
            accumulator.ingest_line(REAL_RECORD).unwrap();
            accumulator.cost_usd()
        };

        let pinned = REAL_RECORD.replace(r#""inference_geo":"not_available""#, r#""inference_geo":"us""#);
        let mut accumulator = TranscriptAccumulator::new(pricing());
        accumulator.ingest_line(&pinned).unwrap();
        assert!(
            (accumulator.cost_usd() - base * GEO_MULTIPLIER).abs() < 1e-12,
            "a region-pinned request must carry the geo premium"
        );

        let fast = REAL_RECORD.replace(r#""speed":"standard""#, r#""speed":"fast""#);
        let mut accumulator = TranscriptAccumulator::new(pricing());
        accumulator.ingest_line(&fast).unwrap();
        assert!(
            (accumulator.cost_usd() - base * FAST_SPEED_MULTIPLIER).abs() < 1e-12,
            "a fast request must carry the speed premium"
        );

        let both = pinned.replace(r#""speed":"standard""#, r#""speed":"fast""#);
        let mut accumulator = TranscriptAccumulator::new(pricing());
        accumulator.ingest_line(&both).unwrap();
        assert!(
            (accumulator.cost_usd() - base * GEO_MULTIPLIER * FAST_SPEED_MULTIPLIER).abs() < 1e-12,
            "the two premiums compound"
        );
    }

    #[test]
    fn a_write_with_no_ttl_breakdown_uses_the_cheaper_tier() {
        // Never assume the premium tier from missing data; the 5-minute TTL is
        // the default and the cheaper of the two.
        let mut accumulator = TranscriptAccumulator::new(pricing());
        accumulator
            .ingest_line(r#"{"usage":{"cache_creation_input_tokens":1000000}}"#)
            .unwrap();
        assert!((accumulator.cost_usd() - 0.2).abs() < 1e-12);
    }

    #[test]
    fn the_vendored_table_prices_the_models_both_clis_run() {
        let table = PricingTable::vendored();
        assert!(
            table.version.starts_with("litellm "),
            "the fallback was used instead of the vendored snapshot: {}",
            table.version
        );
        assert!(table.version.contains("vendored 2026-"));

        for model in ["claude-opus-5", "claude-sonnet-5", "gpt-5.1-codex"] {
            let rates = table.rates_for(Some(model));
            assert!(rates.input_per_million > 0.0, "{model} has no input rate");
            assert!(rates.output_per_million > rates.input_per_million, "{model}");
            assert!(
                rates.cache_write_1h_per_million >= rates.cache_write_5m_per_million,
                "{model}: the 1-hour write must not be cheaper than the 5-minute one"
            );
        }

        // Published Anthropic rates: $5/M in, $25/M out for Opus 5.
        let opus = table.rates_for(Some("claude-opus-5"));
        assert!((opus.input_per_million - 5.0).abs() < 1e-9);
        assert!((opus.output_per_million - 25.0).abs() < 1e-9);
        assert!((opus.cache_write_5m_per_million - 6.25).abs() < 1e-9);
        assert!((opus.cache_write_1h_per_million - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_dated_or_region_prefixed_alias_resolves_to_its_base_model() {
        let table = PricingTable::vendored();
        let base = table.rates_for(Some("claude-opus-5"));
        for alias in [
            "claude-opus-5-20260101",
            "us.anthropic.claude-opus-5",
            "global.anthropic.claude-opus-5-v1",
        ] {
            assert_eq!(
                table.rates_for(Some(alias)),
                base,
                "{alias} should price as claude-opus-5"
            );
        }
    }

    #[test]
    fn the_request_id_window_is_bounded() {
        let mut accumulator = TranscriptAccumulator::new(pricing());
        for index in 0..(MAX_TRACKED_REQUEST_IDS + 10) {
            accumulator
                .ingest_line(&format!(
                    r#"{{"requestId":"r{index}","usage":{{"input_tokens":1}}}}"#
                ))
                .unwrap();
        }
        assert_eq!(accumulator.seen_request_ids.len(), MAX_TRACKED_REQUEST_IDS);
        assert_eq!(accumulator.request_id_order.len(), MAX_TRACKED_REQUEST_IDS);
        // The most recent ids are still deduplicated.
        assert!(!accumulator
            .ingest_line(r#"{"requestId":"r4100","usage":{"input_tokens":1}}"#)
            .unwrap());
    }

    #[test]
    fn a_nested_usage_resolves_by_carrier_not_alphabetically() {
        // `serde_json::Map` is a BTreeMap here, so "alpha" would win a plain
        // values() scan over "message" despite being an unrelated sibling.
        let line = r#"{"alpha":{"usage":{"input_tokens":1000000}},"message":{"model":"m","usage":{"output_tokens":1000000}}}"#;
        let mut accumulator = TranscriptAccumulator::new(pricing());
        accumulator.ingest_line(line).unwrap();
        assert_eq!(accumulator.totals().output_tokens, 1_000_000);
        assert_eq!(accumulator.totals().input_tokens, 0);
    }

    #[test]
    fn context_occupancy_is_the_last_request_not_the_running_sum() {
        // The defect this exists to prevent: after ten turns the sum is ten
        // times a window the session is comfortably inside, and the row would
        // report a thousand percent.
        let mut accumulator = TranscriptAccumulator::new(pricing());
        for index in 0..10 {
            let line = format!(
                r#"{{"requestId":"req-{index}","model":"m","usage":{{"input_tokens":1000,"cache_read_input_tokens":40000,"output_tokens":500}}}}"#
            );
            accumulator.ingest_line(&line).unwrap();
        }
        assert_eq!(accumulator.totals().input_tokens, 10_000, "the sum still sums");
        assert_eq!(
            accumulator.context_tokens(),
            Some(41_000),
            "occupancy is one request's prompt: input plus cache reads"
        );
    }

    #[test]
    fn context_occupancy_counts_cache_writes_and_excludes_output() {
        let mut accumulator = TranscriptAccumulator::new(pricing());
        accumulator
            .ingest_line(
                r#"{"requestId":"req-1","model":"m","usage":{"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":30,"output_tokens":9999}}"#,
            )
            .unwrap();
        // A cache hit still occupies the window; the reply does not, because it
        // has not been sent in any request yet.
        assert_eq!(accumulator.context_tokens(), Some(60));
    }

    #[test]
    fn a_cumulative_only_transcript_reports_no_occupancy() {
        // Codex's rollout reports session totals with no request id. Reading
        // one as a window occupancy is the same error in the other vendor's
        // clothing, so this path must leave the reading absent.
        let mut accumulator = TranscriptAccumulator::new(pricing());
        accumulator
            .ingest_line(r#"{"model":"m","usage":{"input_tokens":900000,"output_tokens":1000}}"#)
            .unwrap();
        assert!(accumulator.totals().input_tokens > 0, "the total is still read");
        assert_eq!(accumulator.context_tokens(), None);
    }

    #[test]
    fn malformed_json_is_reported() {
        assert!(matches!(
            TranscriptAccumulator::new(pricing()).ingest_line("{"),
            Err(TranscriptError::Json(_))
        ));
    }
}

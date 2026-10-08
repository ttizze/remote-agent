//! Shared usage contracts.  Hosts scan provider transcripts and send only
//! aggregated buckets; native clients never receive raw transcript rows.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const CONTRACT_VERSION: u32 = 6;
pub const MERGE_COMPATIBLE_SINCE: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resolution {
    Day,
    Hour,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CostSource {
    ProviderReported,
    ModelPriced,
    Unpriced,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenTotals {
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    /// Reasoning is a subset of output and is never added to `total`.
    pub reasoning_tokens: u64,
}

impl TokenTotals {
    pub fn total(&self) -> u64 {
        self.uncached_input_tokens
            .saturating_add(self.cached_input_tokens)
            .saturating_add(self.cache_creation_tokens)
            .saturating_add(self.output_tokens)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryCost {
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bucket {
    pub day: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hour_start: Option<String>,
    pub provider: Provider,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub totals: TokenTotals,
    pub cost_usd: f64,
    pub cache_savings_usd: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_cost_usd: Option<CategoryCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultrafast_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_premium_usd: Option<f64>,
    pub cost_source: CostSource,
    pub records: u64,
    pub unpriced_records: u64,
    pub sessions: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFingerprint {
    pub host_id: String,
    pub provider: Provider,
    pub resolved_home_path: String,
    pub volume_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceStatus {
    Ok,
    Missing,
    Partial,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub fingerprint: SourceFingerprint,
    pub status: SourceStatus,
    pub scanned_files: u64,
    pub skipped_files: u64,
    pub malformed_records: u64,
    pub distinct_sessions: u64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PricingStatus {
    Fresh,
    Cached,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    pub status: PricingStatus,
    pub source: String,
    pub fetched_at: Option<String>,
    pub known_models: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PriceOverride {
    pub input_cost_per_million_tokens: f64,
    pub output_cost_per_million_tokens: f64,
    pub cache_read_cost_per_million_tokens: Option<f64>,
    pub cache_write_cost_per_million_tokens: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryInput {
    pub since_day: String,
    pub until_day: String,
    pub time_zone: String,
    #[serde(default)]
    pub resolution: Option<Resolution>,
    #[serde(default)]
    pub since_time: Option<String>,
    #[serde(default)]
    pub until_time: Option<String>,
    #[serde(default)]
    pub model_aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub price_overrides: BTreeMap<String, PriceOverride>,
}

impl SummaryInput {
    pub fn daily(since_day: impl Into<String>, until_day: impl Into<String>) -> Self {
        Self {
            since_day: since_day.into(),
            until_day: until_day.into(),
            time_zone: "UTC".into(),
            resolution: Some(Resolution::Day),
            since_time: None,
            until_time: None,
            model_aliases: BTreeMap::new(),
            price_overrides: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub contract_version: u32,
    pub read_at: String,
    pub time_zone: String,
    pub since_day: String,
    pub until_day: String,
    pub buckets: Vec<Bucket>,
    pub sources: Vec<Source>,
    pub pricing: Pricing,
    pub scan_duration_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetCredits {
    pub available_count: u32,
    pub next_expires_at: Option<i64>,
    pub next_credit_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalUsage {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowKind {
    Session,
    Weekly,
    Monthly,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_input_round_trips_binary_and_json() {
        let mut input = SummaryInput::daily("2026-10-01", "2026-10-08");
        input.time_zone = "Asia/Tokyo".into();
        input.model_aliases.insert("preview".into(), "model".into());
        let bytes = postcard::to_allocvec(&input).unwrap();
        assert_eq!(postcard::from_bytes::<SummaryInput>(&bytes).unwrap(), input);
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(serde_json::from_value::<SummaryInput>(json).unwrap(), input);
    }

    #[test]
    fn reasoning_is_a_subset_of_output() {
        let totals = TokenTotals {
            output_tokens: 7,
            reasoning_tokens: 5,
            ..Default::default()
        };
        assert_eq!(totals.total(), 7);
    }
}

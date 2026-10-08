use super::merge::MergedUsage;
use agent_protocol::usage::Provider;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageBreakdownRow {
    pub key: String,
    pub provider: String,
    pub tokens: u64,
    pub cost_usd: f64,
    pub records: u64,
}

pub fn by_model(usage: &MergedUsage) -> Vec<UsageBreakdownRow> {
    let mut rows: BTreeMap<(Provider, String), UsageBreakdownRow> = BTreeMap::new();
    for bucket in &usage.buckets {
        let key = (bucket.provider, bucket.model.clone());
        let row = rows
            .entry(key.clone())
            .or_insert_with(|| UsageBreakdownRow {
                key: key.1.clone(),
                provider: format!("{:?}", key.0),
                tokens: 0,
                cost_usd: 0.0,
                records: 0,
            });
        row.tokens = row.tokens.saturating_add(bucket.totals.total());
        row.cost_usd += bucket.cost_usd;
        row.records = row.records.saturating_add(bucket.records);
    }
    rows.into_values().collect()
}

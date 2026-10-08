use super::merge::MergedUsage;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageChartPoint {
    pub day: String,
    pub tokens: u64,
    pub cost_usd: f64,
}

pub fn daily(usage: &MergedUsage) -> Vec<UsageChartPoint> {
    let mut points: BTreeMap<String, UsageChartPoint> = BTreeMap::new();
    for bucket in &usage.buckets {
        let point = points
            .entry(bucket.day.clone())
            .or_insert_with(|| UsageChartPoint {
                day: bucket.day.clone(),
                tokens: 0,
                cost_usd: 0.0,
            });
        point.tokens = point.tokens.saturating_add(bucket.totals.total());
        point.cost_usd += bucket.cost_usd;
    }
    points.into_values().collect()
}

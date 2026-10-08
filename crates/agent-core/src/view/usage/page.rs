use super::{
    chart::daily,
    format,
    merge::{MergedUsage, merge},
};
use crate::state::Snapshot;

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageRow {
    pub day: String,
    pub provider: String,
    pub model: String,
    pub tokens: u64,
    pub cost_label: String,
    pub cost_known: bool,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsagePageView {
    pub state: String,
    pub total_tokens: u64,
    pub total_tokens_label: String,
    pub cost_usd: f64,
    pub cost_label: String,
    pub sessions: u64,
    pub pricing_status: String,
    pub error: Option<String>,
    pub rows: Vec<UsageRow>,
    pub chart: Vec<super::chart::UsageChartPoint>,
}

fn report(
    merged: MergedUsage,
    summary: &agent_protocol::usage::Summary,
    error: Option<String>,
    loading: bool,
) -> UsagePageView {
    let rows = summary
        .buckets
        .iter()
        .map(|bucket| UsageRow {
            day: bucket.day.clone(),
            provider: format!("{:?}", bucket.provider),
            model: bucket.model.clone(),
            tokens: bucket.totals.total(),
            cost_label: if bucket.cost_source == agent_protocol::usage::CostSource::Unpriced {
                "Unknown".into()
            } else {
                format::usd(bucket.cost_usd)
            },
            cost_known: bucket.cost_source != agent_protocol::usage::CostSource::Unpriced,
        })
        .collect();
    let cost_known = summary
        .buckets
        .iter()
        .any(|bucket| bucket.cost_source != agent_protocol::usage::CostSource::Unpriced);
    UsagePageView {
        state: if error.is_some() {
            "error"
        } else if loading {
            "loading"
        } else {
            "ready"
        }
        .into(),
        total_tokens: merged.total_tokens,
        total_tokens_label: format::token_count(merged.total_tokens),
        cost_usd: merged.cost_usd,
        cost_label: if cost_known || merged.total_tokens == 0 {
            format::usd(merged.cost_usd)
        } else {
            "Unknown".into()
        },
        sessions: merged.sessions,
        pricing_status: format!("{:?}", summary.pricing.status).to_ascii_lowercase(),
        error,
        rows,
        chart: daily(&merged),
    }
}

pub fn usage_page(snapshot: &Snapshot) -> UsagePageView {
    match snapshot.usage_summary.as_ref() {
        Some(summary) => report(
            merge(std::slice::from_ref(summary)),
            summary,
            snapshot.usage_error.clone(),
            snapshot.usage_loading,
        ),
        None => UsagePageView {
            state: if snapshot.usage_error.is_some() {
                "error"
            } else {
                "loading"
            }
            .into(),
            total_tokens: 0,
            total_tokens_label: "0".into(),
            cost_usd: 0.0,
            cost_label: "$0.00".into(),
            sessions: 0,
            pricing_status: "unavailable".into(),
            error: snapshot.usage_error.clone(),
            rows: vec![],
            chart: vec![],
        },
    }
}

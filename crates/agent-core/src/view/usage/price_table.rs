use agent_protocol::usage::PriceOverride;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsagePriceRow {
    pub model: String,
    pub input_per_million: f64,
    pub output_per_million: f64,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
}

pub fn rows(overrides: &BTreeMap<String, PriceOverride>) -> Vec<UsagePriceRow> {
    overrides
        .iter()
        .map(|(model, price)| UsagePriceRow {
            model: model.clone(),
            input_per_million: price.input_cost_per_million_tokens,
            output_per_million: price.output_cost_per_million_tokens,
            cache_read_per_million: price.cache_read_cost_per_million_tokens,
            cache_write_per_million: price.cache_write_cost_per_million_tokens,
        })
        .collect()
}

use agent_protocol::usage::PriceOverride;
use std::collections::HashMap;

/// Native-facing usage query.  The FFI boundary uses hash maps; the Host
/// protocol keeps deterministic ordered maps after conversion.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageSummaryInput {
    pub since_day: String,
    pub until_day: String,
    pub time_zone: String,
    pub resolution: Option<agent_protocol::usage::Resolution>,
    pub since_time: Option<String>,
    pub until_time: Option<String>,
    pub model_aliases: HashMap<String, String>,
    pub price_overrides: HashMap<String, PriceOverride>,
}

impl UsageSummaryInput {
    pub fn daily(since_day: impl Into<String>, until_day: impl Into<String>) -> Self {
        Self {
            since_day: since_day.into(),
            until_day: until_day.into(),
            time_zone: "UTC".into(),
            resolution: Some(agent_protocol::usage::Resolution::Day),
            since_time: None,
            until_time: None,
            model_aliases: HashMap::new(),
            price_overrides: HashMap::new(),
        }
    }
}

impl From<UsageSummaryInput> for agent_protocol::usage::SummaryInput {
    fn from(input: UsageSummaryInput) -> Self {
        Self {
            since_day: input.since_day,
            until_day: input.until_day,
            time_zone: input.time_zone,
            resolution: input.resolution,
            since_time: input.since_time,
            until_time: input.until_time,
            model_aliases: input.model_aliases.into_iter().collect(),
            price_overrides: input.price_overrides.into_iter().collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsagePreferences {
    pub model_aliases: HashMap<String, String>,
    pub price_overrides: HashMap<String, PriceOverride>,
}

impl UsagePreferences {
    pub fn apply(
        &self,
        mut input: agent_protocol::usage::SummaryInput,
    ) -> agent_protocol::usage::SummaryInput {
        input.model_aliases = self.model_aliases.clone().into_iter().collect();
        input.price_overrides = self.price_overrides.clone().into_iter().collect();
        input
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::usage::SummaryInput;

    #[test]
    fn applies_aliases_and_exact_price_override_keys() {
        let mut preferences = UsagePreferences::default();
        preferences
            .model_aliases
            .insert("preview".into(), "final".into());
        preferences.price_overrides.insert(
            "Custom/Model".into(),
            PriceOverride {
                input_cost_per_million_tokens: 1.,
                output_cost_per_million_tokens: 2.,
                cache_read_cost_per_million_tokens: None,
                cache_write_cost_per_million_tokens: None,
            },
        );
        let input = preferences.apply(SummaryInput::daily("2026-01-01", "2026-01-02"));
        assert_eq!(
            input.model_aliases.get("preview").map(String::as_str),
            Some("final")
        );
        assert!(input.price_overrides.contains_key("Custom/Model"));
    }
}

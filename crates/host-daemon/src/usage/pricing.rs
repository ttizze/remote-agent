//! Pure model pricing rules.  Network fetching and persistence live in the
//! usage service, keeping this module deterministic and easy to audit.
use super::transcripts::{Record, Speed};
use agent_protocol::usage::{CategoryCost, CostSource, PriceOverride, TokenTotals};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TokenRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ModelRate {
    pub standard: TokenRates,
    pub fast: Option<TokenRates>,
    pub ultrafast: Option<TokenRates>,
}

pub(crate) type RateTable = HashMap<String, ModelRate>;

fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn rates(
    value: &serde_json::Map<String, Value>,
    suffix: &str,
    standard: Option<TokenRates>,
) -> Option<TokenRates> {
    let input = number(value.get(&format!("input_cost_per_token{suffix}")))?;
    let output = number(value.get(&format!("output_cost_per_token{suffix}")))?;
    let fallback = |name: &str, default: f64| {
        number(value.get(&format!("{name}{suffix}"))).unwrap_or_else(|| {
            standard
                .filter(|standard| standard.input > 0.)
                .map_or(default, |standard| {
                    let ratio = match name {
                        "cache_read_input_token_cost" => standard.cache_read / standard.input,
                        "cache_creation_input_token_cost" => standard.cache_write / standard.input,
                        _ => 1.,
                    };
                    input * ratio
                })
        })
    };
    Some(TokenRates {
        input,
        output,
        cache_read: fallback("cache_read_input_token_cost", input),
        cache_write: fallback("cache_creation_input_token_cost", input),
    })
}

fn multiply(rates: TokenRates, multiple: f64) -> TokenRates {
    TokenRates {
        input: rates.input * multiple,
        output: rates.output * multiple,
        cache_read: rates.cache_read * multiple,
        cache_write: rates.cache_write * multiple,
    }
}

pub(crate) fn parse_rate_table(document: &Value) -> RateTable {
    let mut table = HashMap::new();
    let Some(entries) = document.as_object() else {
        return table;
    };
    for (name, raw) in entries {
        let Some(entry) = raw.as_object() else {
            continue;
        };
        let Some(standard) = rates(entry, "", None) else {
            continue;
        };
        let key = name.trim().to_lowercase();
        if key.is_empty() {
            continue;
        }
        let fast = entry
            .get("provider_specific_entry")
            .and_then(Value::as_object)
            .and_then(|specific| number(specific.get("fast")))
            .filter(|multiple| *multiple > 0.)
            .map(|multiple| multiply(standard, multiple))
            .or_else(|| rates(entry, "_priority", Some(standard)));
        let ultrafast = rates(entry, "_ultrafast", Some(standard));
        table.insert(
            key,
            ModelRate {
                standard,
                fast,
                ultrafast,
            },
        );
    }
    // A bare model is safe only when every qualified model has identical rates.
    let mut aliases: HashMap<String, Option<ModelRate>> = HashMap::new();
    for (key, rate) in &table {
        let Some((_, bare)) = key.rsplit_once('/') else {
            continue;
        };
        if table.contains_key(bare) {
            continue;
        }
        aliases
            .entry(bare.to_owned())
            .and_modify(|candidate| {
                if candidate.is_some_and(|candidate| candidate != *rate) {
                    *candidate = None;
                }
            })
            .or_insert(Some(*rate));
    }
    for (alias, rate) in aliases {
        if let Some(rate) = rate {
            table.insert(alias, rate);
        }
    }
    table
}

pub(crate) fn override_table(
    overrides: &std::collections::BTreeMap<String, PriceOverride>,
) -> RateTable {
    overrides
        .iter()
        .map(|(model, value)| {
            let input = value.input_cost_per_million_tokens / 1_000_000.;
            let output = value.output_cost_per_million_tokens / 1_000_000.;
            (
                model.trim().to_owned(),
                ModelRate {
                    standard: TokenRates {
                        input,
                        output,
                        cache_read: value
                            .cache_read_cost_per_million_tokens
                            .unwrap_or(value.input_cost_per_million_tokens)
                            / 1_000_000.,
                        cache_write: value
                            .cache_write_cost_per_million_tokens
                            .unwrap_or(value.input_cost_per_million_tokens)
                            / 1_000_000.,
                    },
                    fast: None,
                    ultrafast: None,
                },
            )
        })
        .collect()
}

fn lookup(table: &RateTable, model: &str) -> Option<ModelRate> {
    let key = model.trim().to_lowercase();
    let key = key.split('[').next().unwrap_or(&key);
    let bare = key.rsplit_once('/').map_or(key, |(_, bare)| bare);
    if matches!(
        bare,
        "synthetic" | "<synthetic>" | "opus" | "sonnet" | "haiku" | "fable"
    ) {
        return None;
    }
    table.get(key).copied()
}

fn override_key(model: &str) -> &str {
    model.trim()
}

fn at_speed(rate: ModelRate, speed: Speed) -> TokenRates {
    match speed {
        Speed::Standard => rate.standard,
        Speed::Fast => rate.fast.unwrap_or(rate.standard),
        Speed::Ultrafast => rate.ultrafast.unwrap_or(rate.standard),
    }
}

fn category(totals: &TokenTotals, rates: TokenRates) -> CategoryCost {
    CategoryCost {
        input: totals.uncached_input_tokens as f64 * rates.input,
        cache_read: totals.cached_input_tokens as f64 * rates.cache_read,
        cache_write: totals.cache_creation_tokens as f64 * rates.cache_write,
        output: totals.output_tokens as f64 * rates.output,
    }
}

fn sum(category: CategoryCost) -> f64 {
    category.input + category.cache_read + category.cache_write + category.output
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Priced {
    pub cost_usd: f64,
    pub source: CostSource,
    pub category: Option<CategoryCost>,
    pub speed_premium_usd: f64,
}

pub(crate) fn price(table: &RateTable, overrides: &RateTable, record: &Record) -> Priced {
    let reported = record
        .reported_cost_usd
        .filter(|value| value.is_finite())
        .filter(|_| !overrides.contains_key(override_key(&record.model)));
    let rate = overrides
        .get(override_key(&record.model))
        .copied()
        .or_else(|| lookup(table, &record.model));
    let Some(rate) = rate else {
        return Priced {
            cost_usd: reported.unwrap_or_default(),
            source: if reported.is_some() {
                CostSource::ProviderReported
            } else {
                CostSource::Unpriced
            },
            category: None,
            speed_premium_usd: 0.,
        };
    };
    let selected = at_speed(rate, record.speed);
    let list = category(&record.totals, selected);
    let list_total = sum(list);
    if let Some(reported) = reported {
        if list_total <= 0. {
            return Priced {
                cost_usd: reported,
                source: CostSource::ProviderReported,
                category: None,
                speed_premium_usd: 0.,
            };
        }
        let scale = reported / list_total;
        let base = sum(category(&record.totals, rate.standard));
        return Priced {
            cost_usd: reported,
            source: CostSource::ProviderReported,
            category: Some(CategoryCost {
                input: list.input * scale,
                cache_read: list.cache_read * scale,
                cache_write: list.cache_write * scale,
                output: list.output * scale,
            }),
            speed_premium_usd: (list_total - base) * scale,
        };
    }
    Priced {
        cost_usd: list_total,
        source: CostSource::ModelPriced,
        category: Some(list),
        speed_premium_usd: list_total - sum(category(&record.totals, rate.standard)),
    }
}

pub(crate) fn cache_savings(table: &RateTable, overrides: &RateTable, record: &Record) -> f64 {
    let Some(rate) = overrides
        .get(override_key(&record.model))
        .copied()
        .or_else(|| lookup(table, &record.model))
    else {
        return 0.;
    };
    let rates = at_speed(rate, record.speed);
    record.totals.cached_input_tokens as f64 * (rates.input - rates.cache_read)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aliases_only_apply_when_qualified_rates_match() {
        let same = parse_rate_table(&json!({
            "provider-a/example": {"input_cost_per_token":1e-6,"output_cost_per_token":2e-6},
            "provider-b/example": {"input_cost_per_token":1e-6,"output_cost_per_token":2e-6}
        }));
        assert!(same.contains_key("example"));
        let different = parse_rate_table(&json!({
            "provider-a/example": {"input_cost_per_token":1e-6,"output_cost_per_token":2e-6},
            "provider-b/example": {"input_cost_per_token":3e-6,"output_cost_per_token":2e-6}
        }));
        assert!(!different.contains_key("example"));
    }

    #[test]
    fn override_beats_provider_reported_cost() {
        let table = override_table(
            &[(
                "model".into(),
                PriceOverride {
                    input_cost_per_million_tokens: 2.,
                    output_cost_per_million_tokens: 8.,
                    cache_read_cost_per_million_tokens: None,
                    cache_write_cost_per_million_tokens: None,
                },
            )]
            .into_iter()
            .collect(),
        );
        let record = Record {
            provider: agent_protocol::usage::Provider::Claude,
            timestamp_ms: 0,
            model: "model".into(),
            session_id: String::new(),
            totals: TokenTotals {
                uncached_input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                ..Default::default()
            },
            reported_cost_usd: Some(99.),
            speed: Speed::Standard,
            dedupe_key: None,
        };
        assert_eq!(price(&HashMap::new(), &table, &record).cost_usd, 10.);
    }

    #[test]
    fn custom_override_keys_keep_their_entered_case() {
        let table = override_table(
            &[(
                "Custom/Model".into(),
                PriceOverride {
                    input_cost_per_million_tokens: 3.,
                    output_cost_per_million_tokens: 7.,
                    cache_read_cost_per_million_tokens: None,
                    cache_write_cost_per_million_tokens: None,
                },
            )]
            .into_iter()
            .collect(),
        );
        let record = Record {
            provider: agent_protocol::usage::Provider::Claude,
            timestamp_ms: 0,
            model: "custom/model".into(),
            session_id: String::new(),
            totals: TokenTotals {
                output_tokens: 1_000_000,
                ..Default::default()
            },
            reported_cost_usd: None,
            speed: Speed::Standard,
            dedupe_key: None,
        };
        assert_eq!(
            price(&HashMap::new(), &table, &record).source,
            CostSource::Unpriced
        );
    }
}

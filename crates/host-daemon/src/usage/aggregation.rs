//! Pure de-duplication, bucketing and pricing fold.
use super::{
    pricing,
    transcripts::{Record, Speed},
};
use agent_protocol::usage::{
    Bucket, CategoryCost, CostSource, Provider, Resolution, SummaryInput, TokenTotals,
};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
struct Mutable {
    totals: TokenTotals,
    cost_usd: f64,
    cache_savings_usd: f64,
    category: Option<CategoryCost>,
    fast_cost_usd: f64,
    ultrafast_cost_usd: f64,
    speed_premium_usd: f64,
    records: u64,
    unpriced_records: u64,
    provider_reported_records: u64,
    sessions: HashSet<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stats {
    pub duplicates_dropped: u64,
    pub out_of_window: u64,
}

fn zone_offset(time_zone: &str) -> FixedOffset {
    if time_zone.eq_ignore_ascii_case("utc") || time_zone == "Z" {
        return FixedOffset::east_opt(0).unwrap();
    }
    let normalized = time_zone.strip_prefix("UTC").unwrap_or(time_zone);
    let Ok(value) = normalized.parse::<chrono::FixedOffset>() else {
        return FixedOffset::east_opt(0).unwrap();
    };
    value
}

pub(crate) fn in_window(input: &SummaryInput, timestamp_ms: i64) -> bool {
    if matches!(input.resolution, Some(Resolution::Hour)) {
        let Some(since) = input
            .since_time
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis())
        else {
            return false;
        };
        let Some(until) = input
            .until_time
            .as_deref()
            .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.timestamp_millis())
        else {
            return false;
        };
        return timestamp_ms >= since && timestamp_ms < until;
    }
    let day = day(timestamp_ms, zone_offset(&input.time_zone));
    day >= input.since_day && day <= input.until_day
}

fn day(timestamp_ms: i64, offset: FixedOffset) -> String {
    offset
        .timestamp_millis_opt(timestamp_ms)
        .single()
        .unwrap_or_else(|| {
            Utc.timestamp_millis_opt(timestamp_ms)
                .single()
                .unwrap()
                .with_timezone(&offset)
        })
        .format("%Y-%m-%d")
        .to_string()
}

fn add_totals(target: &mut TokenTotals, source: &TokenTotals) {
    target.uncached_input_tokens = target
        .uncached_input_tokens
        .saturating_add(source.uncached_input_tokens);
    target.cached_input_tokens = target
        .cached_input_tokens
        .saturating_add(source.cached_input_tokens);
    target.cache_creation_tokens = target
        .cache_creation_tokens
        .saturating_add(source.cache_creation_tokens);
    target.output_tokens = target.output_tokens.saturating_add(source.output_tokens);
    target.reasoning_tokens = target
        .reasoning_tokens
        .saturating_add(source.reasoning_tokens);
}

fn add_category(target: &mut Option<CategoryCost>, value: CategoryCost) {
    if let Some(target) = target {
        target.input += value.input;
        target.cache_read += value.cache_read;
        target.cache_write += value.cache_write;
        target.output += value.output;
    } else {
        *target = Some(value);
    }
}

fn final_source(bucket: &Mutable) -> CostSource {
    if bucket.unpriced_records == bucket.records {
        CostSource::Unpriced
    } else if bucket.provider_reported_records == bucket.records {
        CostSource::ProviderReported
    } else {
        CostSource::ModelPriced
    }
}

fn round(value: f64) -> f64 {
    (value * 1_000_000.).round() / 1_000_000.
}

fn resolve_alias(
    aliases: &std::collections::BTreeMap<String, String>,
    model: &str,
) -> Option<String> {
    let mut current = aliases.get(model)?.clone();
    let mut seen = HashSet::from([model.to_owned()]);
    while let Some(next) = aliases.get(&current) {
        if !seen.insert(current.clone()) {
            return None;
        }
        current = next.clone();
    }
    Some(current)
}

pub(crate) fn aggregate(
    input: &SummaryInput,
    records: impl IntoIterator<Item = (Record, String)>,
    rates: &pricing::RateTable,
    overrides: &pricing::RateTable,
) -> (Vec<Bucket>, Stats) {
    let offset = zone_offset(&input.time_zone);
    let hourly = matches!(input.resolution, Some(Resolution::Hour));
    let since_time = input
        .since_time
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis());
    let mut buckets: HashMap<String, Mutable> = HashMap::new();
    let mut seen = HashSet::new();
    let mut stats = Stats::default();
    for (mut record, source_path) in records {
        if let Some(key) = record.dedupe_key.as_ref()
            && !seen.insert(key.clone())
        {
            stats.duplicates_dropped += 1;
            continue;
        }
        if !in_window(input, record.timestamp_ms) {
            stats.out_of_window += 1;
            continue;
        }
        let day = day(record.timestamp_ms, offset);
        if let Some(model) = resolve_alias(&input.model_aliases, &record.model) {
            record.model = model;
        }
        let hour_start = if hourly {
            let since = since_time.unwrap_or(record.timestamp_ms);
            let index = record.timestamp_ms.saturating_sub(since) / 3_600_000;
            Some(
                DateTime::from_timestamp_millis(since + index * 3_600_000)
                    .unwrap_or_else(|| Utc.timestamp_millis_opt(since).single().unwrap())
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            )
        } else {
            None
        };
        let key = format!(
            "{}\0{}\0{:?}\0{}\0{}",
            day,
            hour_start.as_deref().unwrap_or_default(),
            record.provider,
            record.model,
            source_path
        );
        let bucket = buckets.entry(key).or_default();
        let priced = pricing::price(rates, overrides, &record);
        add_totals(&mut bucket.totals, &record.totals);
        bucket.cost_usd += priced.cost_usd;
        bucket.cache_savings_usd += pricing::cache_savings(rates, overrides, &record);
        if let Some(category) = priced.category {
            add_category(&mut bucket.category, category);
        }
        match record.speed {
            Speed::Fast => bucket.fast_cost_usd += priced.cost_usd,
            Speed::Ultrafast => bucket.ultrafast_cost_usd += priced.cost_usd,
            Speed::Standard => {}
        }
        bucket.speed_premium_usd += priced.speed_premium_usd;
        bucket.records += 1;
        if priced.source == CostSource::Unpriced {
            bucket.unpriced_records += 1;
        }
        if priced.source == CostSource::ProviderReported {
            bucket.provider_reported_records += 1;
        }
        if !record.session_id.is_empty() {
            bucket.sessions.insert(record.session_id);
        }
    }
    let mut output = buckets
        .into_iter()
        .map(|(key, bucket)| {
            let mut parts = key.split('\0');
            let day = parts.next().unwrap_or_default().to_owned();
            let hour = parts.next().unwrap_or_default();
            let provider = match parts.next().unwrap_or_default() {
                "Claude" => Provider::Claude,
                _ => Provider::Codex,
            };
            let model = parts.next().unwrap_or_default().to_owned();
            let source = parts.next().unwrap_or_default();
            Bucket {
                day,
                hour_start: (!hour.is_empty()).then(|| hour.to_owned()),
                provider,
                model,
                source_path: (!source.is_empty()).then(|| source.to_owned()),
                totals: bucket.totals,
                cost_usd: bucket.cost_usd,
                cache_savings_usd: bucket.cache_savings_usd,
                category_cost_usd: bucket.category.map(|category| CategoryCost {
                    input: round(category.input),
                    cache_read: round(category.cache_read),
                    cache_write: round(category.cache_write),
                    output: round(category.output),
                }),
                fast_cost_usd: (bucket.fast_cost_usd != 0.).then(|| round(bucket.fast_cost_usd)),
                ultrafast_cost_usd: (bucket.ultrafast_cost_usd != 0.)
                    .then(|| round(bucket.ultrafast_cost_usd)),
                speed_premium_usd: (bucket.speed_premium_usd != 0.)
                    .then(|| round(bucket.speed_premium_usd)),
                cost_source: final_source(&bucket),
                records: bucket.records,
                unpriced_records: bucket.unpriced_records,
                sessions: bucket.sessions.len() as u64,
            }
        })
        .collect::<Vec<_>>();
    output.sort_by(|a, b| {
        a.day
            .cmp(&b.day)
            .then(a.hour_start.cmp(&b.hour_start))
            .then(a.provider.cmp(&b.provider))
            .then(a.model.cmp(&b.model))
    });
    (output, stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::usage::SummaryInput;

    fn record(model: &str, timestamp_ms: i64) -> Record {
        Record {
            provider: Provider::Claude,
            timestamp_ms,
            model: model.into(),
            session_id: "session".into(),
            totals: TokenTotals {
                output_tokens: 1,
                ..Default::default()
            },
            reported_cost_usd: None,
            speed: Speed::Standard,
            dedupe_key: Some("message".into()),
        }
    }

    #[test]
    fn aliases_and_duplicates_are_applied_before_bucket_totals() {
        let mut input = SummaryInput::daily("2026-01-01", "2026-01-01");
        input.model_aliases.insert("preview".into(), "final".into());
        let (buckets, stats) = aggregate(
            &input,
            [
                (record("preview", 1_767_225_600_000), "/claude".into()),
                (record("final", 1_767_225_600_001), "/claude".into()),
            ],
            &HashMap::new(),
            &HashMap::new(),
        );
        assert_eq!(stats.duplicates_dropped, 1);
        assert_eq!(buckets[0].model, "final");
        assert_eq!(buckets[0].records, 1);
    }
}

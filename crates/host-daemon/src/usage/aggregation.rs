//! Pure de-duplication, bucketing and pricing fold.
use super::{
    pricing,
    transcripts::{Record, Speed},
};
use agent_protocol::usage::{
    Bucket, CategoryCost, CostSource, Provider, Resolution, SummaryInput, TokenTotals,
};
use chrono::{DateTime, FixedOffset, Offset, TimeZone, Utc};
use chrono_tz::Tz;
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

#[derive(Debug, Clone)]
pub(crate) enum Zone {
    Fixed(FixedOffset),
    Iana(Tz),
}

impl Zone {
    fn offset_seconds(&self, timestamp_ms: i64) -> Option<i32> {
        let utc = Utc.timestamp_millis_opt(timestamp_ms).single()?;
        Some(match self {
            Self::Fixed(offset) => offset.local_minus_utc(),
            Self::Iana(zone) => utc.with_timezone(zone).offset().fix().local_minus_utc(),
        })
    }
}

pub(crate) fn parse_zone(time_zone: &str) -> Result<Zone, String> {
    if time_zone.eq_ignore_ascii_case("utc") || time_zone == "Z" {
        return Ok(Zone::Fixed(FixedOffset::east_opt(0).unwrap()));
    }
    let normalized = time_zone
        .get(..3)
        .filter(|prefix| prefix.eq_ignore_ascii_case("UTC") || prefix.eq_ignore_ascii_case("GMT"))
        .map(|_| &time_zone[3..])
        .unwrap_or(time_zone);
    if let Some(offset) = parse_fixed_offset(normalized) {
        return Ok(Zone::Fixed(offset));
    }
    time_zone
        .parse::<Tz>()
        .map(Zone::Iana)
        .map_err(|_| format!("タイムゾーンの定義を読み込めません: {time_zone}"))
}

fn parse_fixed_offset(value: &str) -> Option<FixedOffset> {
    if value.is_empty() {
        return FixedOffset::east_opt(0);
    }
    let sign = match value.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = &value[1..];
    if !digits
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b':')
    {
        return None;
    }
    let (hours, minutes) = if let Some((hours, minutes)) = digits.split_once(':') {
        (hours.parse::<i32>().ok()?, minutes.parse::<i32>().ok()?)
    } else if digits.len() == 4 {
        (
            digits[..2].parse::<i32>().ok()?,
            digits[2..].parse::<i32>().ok()?,
        )
    } else {
        (digits.parse::<i32>().ok()?, 0)
    };
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    let seconds = sign * (hours * 3_600 + minutes * 60);
    FixedOffset::east_opt(seconds)
}

fn day(timestamp_ms: i64, zone: &Zone) -> Option<String> {
    let utc = Utc.timestamp_millis_opt(timestamp_ms).single()?;
    Some(match zone {
        Zone::Fixed(offset) => utc.with_timezone(offset).format("%Y-%m-%d").to_string(),
        Zone::Iana(zone) => utc.with_timezone(zone).format("%Y-%m-%d").to_string(),
    })
}

pub(crate) fn in_window_with_zone(input: &SummaryInput, timestamp_ms: i64, zone: &Zone) -> bool {
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
    let Some(day) = day(timestamp_ms, zone) else {
        return false;
    };
    day >= input.since_day && day <= input.until_day
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
) -> Result<(Vec<Bucket>, Stats), String> {
    let zone = parse_zone(&input.time_zone)?;
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
        if !in_window_with_zone(input, record.timestamp_ms, &zone) {
            stats.out_of_window += 1;
            continue;
        }
        let Some(day) = day(record.timestamp_ms, &zone) else {
            stats.out_of_window += 1;
            continue;
        };
        if let Some(model) = resolve_alias(&input.model_aliases, &record.model) {
            record.model = model;
        }
        let hour_start = if hourly {
            let since = since_time.unwrap_or(record.timestamp_ms);
            let index = record.timestamp_ms.saturating_sub(since) / 3_600_000;
            let Some(start) = since.checked_add(index.saturating_mul(3_600_000)) else {
                stats.out_of_window += 1;
                continue;
            };
            let Some(start) = DateTime::from_timestamp_millis(start) else {
                stats.out_of_window += 1;
                continue;
            };
            Some(start.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
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
            let cost_source = final_source(&bucket);
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
                cost_source,
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
    Ok((output, stats))
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
        )
        .unwrap();
        assert_eq!(stats.duplicates_dropped, 1);
        assert_eq!(buckets[0].model, "final");
        assert_eq!(buckets[0].records, 1);
    }

    #[test]
    fn iana_zone_uses_dst_transitions_for_day_buckets() {
        let zone = parse_zone("America/New_York").unwrap();
        let before_spring = DateTime::parse_from_rfc3339("2026-03-08T06:30:00Z")
            .unwrap()
            .timestamp_millis();
        let after_spring = DateTime::parse_from_rfc3339("2026-03-08T07:30:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(day(before_spring, &zone).as_deref(), Some("2026-03-08"));
        assert_eq!(day(after_spring, &zone).as_deref(), Some("2026-03-08"));
        assert_eq!(zone.offset_seconds(before_spring), Some(-18_000));
        assert_eq!(zone.offset_seconds(after_spring), Some(-14_400));
    }

    #[test]
    fn iana_zone_applies_future_rules_portably() {
        let zone = parse_zone("America/New_York").unwrap();
        let future_winter = DateTime::parse_from_rfc3339("2050-01-01T05:30:00Z")
            .unwrap()
            .timestamp_millis();
        let future_summer = DateTime::parse_from_rfc3339("2050-07-01T04:30:00Z")
            .unwrap()
            .timestamp_millis();
        assert_eq!(day(future_winter, &zone).as_deref(), Some("2050-01-01"));
        assert_eq!(day(future_summer, &zone).as_deref(), Some("2050-07-01"));
        assert_eq!(zone.offset_seconds(future_winter), Some(-18_000));
        assert_eq!(zone.offset_seconds(future_summer), Some(-14_400));
    }

    #[test]
    fn invalid_iana_zone_is_rejected_instead_of_becoming_utc() {
        assert!(parse_zone("Not/AZone").is_err());
        assert!(parse_zone("../UTC").is_err());
    }

    #[test]
    fn fixed_mobile_zone_identifiers_are_supported() {
        assert_eq!(
            parse_zone("GMT+0900").unwrap().offset_seconds(0),
            Some(9 * 3_600)
        );
        assert_eq!(
            parse_zone("UTC-07:30").unwrap().offset_seconds(0),
            Some(-27_000)
        );
    }
}

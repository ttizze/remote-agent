use agent_protocol::usage::{
    Bucket, CostSource, Provider, Source, SourceFingerprint, SourceStatus, Summary, TokenTotals,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MergedUsage {
    pub buckets: Vec<Bucket>,
    pub sources: Vec<Source>,
    pub total_tokens: u64,
    pub cost_usd: f64,
    pub sessions: u64,
}

fn source_key(fingerprint: &SourceFingerprint) -> (String, Provider, String, String) {
    (
        fingerprint.host_id.clone(),
        fingerprint.provider,
        fingerprint.resolved_home_path.clone(),
        fingerprint.volume_id.clone(),
    )
}

fn source_priority(status: SourceStatus) -> u8 {
    match status {
        SourceStatus::Ok => 3,
        SourceStatus::Partial => 2,
        SourceStatus::Failed => 1,
        SourceStatus::Missing => 0,
    }
}

type SourceKey = (String, Provider, String, String);
type BucketKey = (String, Option<String>, Provider, String);

#[derive(Debug, Clone)]
struct Owner {
    summary_index: usize,
    source: Source,
}

fn bucket_key(bucket: &Bucket) -> BucketKey {
    (
        bucket.day.clone(),
        bucket.hour_start.clone(),
        bucket.provider,
        bucket.model.clone(),
    )
}

fn source_key_for_bucket(summary: &Summary, bucket: &Bucket) -> Option<SourceKey> {
    let matching = summary
        .sources
        .iter()
        .filter(|source| source.fingerprint.provider == bucket.provider);
    if let Some(path) = bucket.source_path.as_deref() {
        matching
            .filter(|source| source.fingerprint.resolved_home_path == path)
            .map(|source| source_key(&source.fingerprint))
            .next()
    } else {
        let mut matching = matching.map(|source| source_key(&source.fingerprint));
        let key = matching.next();
        (key.is_some() && matching.next().is_none())
            .then_some(key)
            .flatten()
    }
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

fn merge_bucket(current: &mut Bucket, incoming: &Bucket) {
    add_totals(&mut current.totals, &incoming.totals);
    current.cost_usd += incoming.cost_usd;
    current.cache_savings_usd += incoming.cache_savings_usd;
    match (
        current.category_cost_usd.as_mut(),
        incoming.category_cost_usd,
    ) {
        (Some(current), Some(incoming)) => {
            current.input += incoming.input;
            current.cache_read += incoming.cache_read;
            current.cache_write += incoming.cache_write;
            current.output += incoming.output;
        }
        (None, Some(incoming)) => current.category_cost_usd = Some(incoming),
        _ => {}
    }
    let fast =
        current.fast_cost_usd.unwrap_or_default() + incoming.fast_cost_usd.unwrap_or_default();
    let ultrafast = current.ultrafast_cost_usd.unwrap_or_default()
        + incoming.ultrafast_cost_usd.unwrap_or_default();
    let premium = current.speed_premium_usd.unwrap_or_default()
        + incoming.speed_premium_usd.unwrap_or_default();
    current.fast_cost_usd = (fast != 0.).then_some(fast);
    current.ultrafast_cost_usd = (ultrafast != 0.).then_some(ultrafast);
    current.speed_premium_usd = (premium != 0.).then_some(premium);
    current.records = current.records.saturating_add(incoming.records);
    current.unpriced_records = current
        .unpriced_records
        .saturating_add(incoming.unpriced_records);
    current.sessions = current.sessions.saturating_add(incoming.sessions);
    current.cost_source = if current.unpriced_records == current.records {
        CostSource::Unpriced
    } else if current.unpriced_records == 0
        && current.cost_source == CostSource::ProviderReported
        && incoming.cost_source == CostSource::ProviderReported
    {
        CostSource::ProviderReported
    } else {
        CostSource::ModelPriced
    };
}

/// Merges reports from one or more Hosts while dropping duplicate physical
/// transcript sources. A complete source wins over a partial or failed copy.
pub fn merge(summaries: &[Summary]) -> MergedUsage {
    let mut owners: BTreeMap<SourceKey, Owner> = BTreeMap::new();
    for (index, summary) in summaries.iter().enumerate() {
        for source in &summary.sources {
            if source.status == SourceStatus::Missing {
                continue;
            }
            let key = source_key(&source.fingerprint);
            let replace = owners.get(&key).is_some_and(|current| {
                source_priority(source.status) > source_priority(current.source.status)
                    || (source_priority(source.status) == source_priority(current.source.status)
                        && summary.read_at > summaries[current.summary_index].read_at)
            });
            if replace || !owners.contains_key(&key) {
                owners.insert(
                    key,
                    Owner {
                        summary_index: index,
                        source: source.clone(),
                    },
                );
            }
        }
    }

    let mut owned_paths = vec![BTreeSet::new(); summaries.len()];
    for (index, summary) in summaries.iter().enumerate() {
        for source in &summary.sources {
            let key = source_key(&source.fingerprint);
            if owners
                .get(&key)
                .is_some_and(|owner| owner.summary_index == index)
            {
                owned_paths[index].insert((
                    source.fingerprint.provider,
                    source.fingerprint.resolved_home_path.clone(),
                ));
            }
        }
    }

    let mut complete_buckets: BTreeMap<SourceKey, BTreeSet<BucketKey>> = BTreeMap::new();
    for (index, summary) in summaries.iter().enumerate() {
        for bucket in &summary.buckets {
            let Some(key) = source_key_for_bucket(summary, bucket) else {
                continue;
            };
            if owners.get(&key).is_some_and(|owner| {
                owner.summary_index == index && owner.source.status == SourceStatus::Ok
            }) {
                complete_buckets
                    .entry(key)
                    .or_default()
                    .insert(bucket_key(bucket));
            }
        }
    }

    // A newer partial scan can contain cells appended after an older complete
    // scan. Keep only cells absent from the complete copy; overlapping cells
    // cannot be reconciled without raw records and would double count.
    let mut supplemental = BTreeSet::<(usize, BucketKey)>::new();
    let mut session_counts: BTreeMap<SourceKey, u64> = owners
        .iter()
        .map(|(key, owner)| (key.clone(), owner.source.distinct_sessions))
        .collect();
    for (index, summary) in summaries.iter().enumerate() {
        for source in &summary.sources {
            if source.status != SourceStatus::Partial {
                continue;
            }
            let key = source_key(&source.fingerprint);
            let Some(owner) = owners.get(&key) else {
                continue;
            };
            if owner.source.status != SourceStatus::Ok
                || summary.read_at <= summaries[owner.summary_index].read_at
            {
                continue;
            }
            let mut added = false;
            for bucket in &summary.buckets {
                if source_key_for_bucket(summary, bucket).as_ref() != Some(&key) {
                    continue;
                }
                let cell = bucket_key(bucket);
                if complete_buckets
                    .get(&key)
                    .is_some_and(|buckets| buckets.contains(&cell))
                {
                    continue;
                }
                supplemental.insert((index, cell));
                added = true;
            }
            if added {
                session_counts
                    .entry(key)
                    .and_modify(|count| *count = (*count).max(source.distinct_sessions));
            }
        }
    }

    let mut buckets: BTreeMap<String, Bucket> = BTreeMap::new();
    for (index, summary) in summaries.iter().enumerate() {
        let has_sources = !summary.sources.is_empty();
        for bucket in &summary.buckets {
            let is_supplemental = supplemental.contains(&(index, bucket_key(bucket)));
            if has_sources {
                let include = bucket.source_path.as_ref().map_or_else(
                    || {
                        summary
                            .sources
                            .iter()
                            .filter(|source| source.fingerprint.provider == bucket.provider)
                            .count()
                            == 1
                            && owned_paths[index]
                                .iter()
                                .any(|(provider, _)| *provider == bucket.provider)
                    },
                    |path| owned_paths[index].contains(&(bucket.provider, path.clone())),
                );
                if !include && !is_supplemental {
                    continue;
                }
            }
            let key = format!(
                "{}\0{}\0{:?}\0{}\0{}",
                bucket.day,
                bucket.hour_start.as_deref().unwrap_or_default(),
                bucket.provider,
                bucket.model,
                bucket.source_path.as_deref().unwrap_or_default(),
            );
            if let Some(current) = buckets.get_mut(&key) {
                merge_bucket(current, bucket);
            } else {
                buckets.insert(key, bucket.clone());
            }
        }
    }

    let buckets: Vec<_> = buckets.into_values().collect();
    let sources: Vec<_> = owners.values().map(|owner| owner.source.clone()).collect();
    let sessions = if sources.is_empty() {
        buckets.iter().map(|bucket| bucket.sessions).sum()
    } else {
        owners
            .keys()
            .map(|key| session_counts.get(key).copied().unwrap_or_default())
            .sum()
    };
    MergedUsage {
        total_tokens: buckets.iter().map(|bucket| bucket.totals.total()).sum(),
        cost_usd: buckets.iter().map(|bucket| bucket.cost_usd).sum(),
        sessions,
        buckets,
        sources,
    }
}

pub fn provider_totals(usage: &MergedUsage) -> BTreeMap<Provider, (u64, f64)> {
    let mut totals: BTreeMap<Provider, (u64, f64)> = BTreeMap::new();
    for bucket in &usage.buckets {
        let entry = totals.entry(bucket.provider).or_insert((0, 0.0));
        entry.0 = entry.0.saturating_add(bucket.totals.total());
        entry.1 += bucket.cost_usd;
    }
    totals
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::usage::{Pricing, PricingStatus};

    fn summary(path: &str, status: SourceStatus, tokens: u64) -> Summary {
        Summary {
            contract_version: agent_protocol::usage::CONTRACT_VERSION,
            read_at: "2026-01-01T00:00:00Z".into(),
            time_zone: "UTC".into(),
            since_day: "2026-01-01".into(),
            until_day: "2026-01-01".into(),
            sources: vec![Source {
                fingerprint: SourceFingerprint {
                    host_id: "host".into(),
                    provider: Provider::Claude,
                    resolved_home_path: path.into(),
                    volume_id: "volume".into(),
                },
                status,
                scanned_files: 1,
                skipped_files: 0,
                malformed_records: 0,
                distinct_sessions: 1,
                message: None,
            }],
            buckets: vec![Bucket {
                day: "2026-01-01".into(),
                hour_start: None,
                provider: Provider::Claude,
                model: "model".into(),
                source_path: Some(path.into()),
                totals: TokenTotals {
                    output_tokens: tokens,
                    ..Default::default()
                },
                cost_usd: 1.,
                cache_savings_usd: 0.,
                category_cost_usd: None,
                fast_cost_usd: None,
                ultrafast_cost_usd: None,
                speed_premium_usd: None,
                cost_source: CostSource::ModelPriced,
                records: 1,
                unpriced_records: 0,
                sessions: 1,
            }],
            pricing: Pricing {
                status: PricingStatus::Fresh,
                source: "source".into(),
                fetched_at: None,
                known_models: 1,
            },
            scan_duration_ms: 1,
        }
    }

    #[test]
    fn duplicate_physical_sources_are_not_counted_twice() {
        let merged = merge(&[
            summary("/home/.provider/history", SourceStatus::Ok, 2),
            summary("/home/.provider/history", SourceStatus::Ok, 3),
        ]);
        assert_eq!(merged.total_tokens, 2);
        assert_eq!(merged.sources.len(), 1);
    }

    #[test]
    fn complete_scan_replaces_partial_duplicate() {
        let merged = merge(&[
            summary("/home/.provider/history", SourceStatus::Partial, 2),
            summary("/home/.provider/history", SourceStatus::Ok, 3),
        ]);
        assert_eq!(merged.total_tokens, 3);
        assert_eq!(merged.sources[0].status, SourceStatus::Ok);
    }

    #[test]
    fn newer_partial_scans_supply_cells_missing_from_a_complete_copy() {
        let complete = summary("/home/.provider/history", SourceStatus::Ok, 3);
        let mut partial = summary("/home/.provider/history", SourceStatus::Partial, 2);
        partial.read_at = "2026-01-01T00:01:00Z".into();
        partial.buckets[0].model = "new-model".into();
        let merged = merge(&[complete, partial]);
        assert_eq!(merged.total_tokens, 5);
        assert_eq!(merged.buckets.len(), 2);
    }

    #[test]
    fn totals_sessions_from_sources_when_one_session_spans_buckets() {
        let mut report = summary("/home/.provider/history", SourceStatus::Ok, 2);
        let mut second = report.buckets[0].clone();
        second.model = "second-model".into();
        report.buckets.push(second);
        assert_eq!(merge(&[report]).sessions, 1);
    }
}

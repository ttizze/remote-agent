//! Host-owned usage scan, pricing cache and summary assembly.
use super::{aggregation, pricing, reader};
use agent_protocol::provider::ProviderKind;
use agent_protocol::usage::{
    self, Pricing, PricingStatus, Provider, Source, SourceFingerprint, SourceStatus, Summary,
    SummaryInput,
};
use reqwest::Client;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

pub(crate) const RATES_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const RATES_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const REFRESH_FLOOR: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct Rates {
    fetched_at: Option<SystemTime>,
    attempted_at: Option<SystemTime>,
    table: pricing::RateTable,
    status: PricingStatus,
}

impl Default for Rates {
    fn default() -> Self {
        Self {
            fetched_at: None,
            attempted_at: None,
            table: Default::default(),
            status: PricingStatus::Unavailable,
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct RatesFile {
    fetched_at_ms: i64,
    document: Value,
}

#[derive(Clone)]
pub(crate) struct UsageService {
    cache: Arc<Mutex<reader::ScanCache>>,
    cache_loaded: Arc<tokio::sync::OnceCell<()>>,
    rates: Arc<Mutex<Rates>>,
    rates_loading: Arc<tokio::sync::Mutex<()>>,
    rates_path: PathBuf,
    scan_cache_path: PathBuf,
}

impl UsageService {
    pub(crate) fn new(state_path: &Path) -> Self {
        Self {
            cache: Arc::default(),
            cache_loaded: Arc::default(),
            rates: Arc::default(),
            rates_loading: Arc::default(),
            rates_path: state_path.with_file_name("usage-model-rates.json"),
            scan_cache_path: state_path.with_file_name("usage-scan-cache.json"),
        }
    }

    fn now() -> SystemTime {
        SystemTime::now()
    }

    fn millis(time: SystemTime) -> i64 {
        time.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(i64::MAX as u128) as i64
    }

    fn pricing(&self) -> Pricing {
        let rates = self.rates.lock().unwrap_or_else(|error| error.into_inner());
        Pricing {
            status: rates.status,
            source: RATES_URL.into(),
            fetched_at: rates.fetched_at.map(|at| {
                chrono::DateTime::<chrono::Utc>::from(at)
                    .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)
            }),
            known_models: rates.table.len() as u64,
        }
    }

    async fn load_rates(&self, force: bool) {
        let _guard = self.rates_loading.lock().await;
        let now = Self::now();
        {
            let rates = self.rates.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(attempted_at) = rates.attempted_at
                && now.duration_since(attempted_at).unwrap_or_default() < REFRESH_FLOOR
            {
                return;
            }
            if let Some(fetched_at) = rates.fetched_at {
                let max_age = if force { REFRESH_FLOOR } else { RATES_TTL };
                if now.duration_since(fetched_at).unwrap_or_default() < max_age {
                    return;
                }
            }
        }
        let should_read_disk = self
            .rates
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .fetched_at
            .is_none();
        if should_read_disk {
            if let Ok(bytes) = tokio::fs::read(&self.rates_path).await
                && let Ok(file) = serde_json::from_slice::<RatesFile>(&bytes)
            {
                let table = pricing::parse_rate_table(&file.document);
                if !table.is_empty() {
                    let fetched_at = if file.fetched_at_ms > 0 {
                        SystemTime::UNIX_EPOCH + Duration::from_millis(file.fetched_at_ms as u64)
                    } else {
                        now
                    };
                    let fresh = now.duration_since(fetched_at).unwrap_or_default()
                        < if force { REFRESH_FLOOR } else { RATES_TTL };
                    let mut rates = self.rates.lock().unwrap_or_else(|error| error.into_inner());
                    rates.fetched_at = Some(fetched_at);
                    rates.table = table;
                    rates.status = PricingStatus::Cached;
                    if fresh {
                        return;
                    }
                }
            }
        }
        let fetched = match Client::new()
            .get(RATES_URL)
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => response.json::<Value>().await.ok(),
            Err(_) => None,
        };
        self.rates
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .attempted_at = Some(now);
        let Some(document) = fetched else {
            return;
        };
        let table = pricing::parse_rate_table(&document);
        if table.is_empty() {
            return;
        }
        {
            let mut rates = self.rates.lock().unwrap_or_else(|error| error.into_inner());
            rates.fetched_at = Some(now);
            rates.table = table;
            rates.status = PricingStatus::Fresh;
        }
        let file = RatesFile {
            fetched_at_ms: Self::millis(now),
            document,
        };
        if let Ok(bytes) = serde_json::to_vec(&file) {
            let path = self.rates_path.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                std::fs::write(path, bytes)
            })
            .await;
        }
    }

    pub(crate) async fn refresh_rates(&self) -> Pricing {
        self.load_rates(true).await;
        self.pricing()
    }

    fn provider_home(provider: Provider, home: &Path) -> PathBuf {
        reader::source_root(home, provider)
    }

    fn source(
        provider: Provider,
        root: &Path,
        result: &reader::ReadFiles,
        root_exists: bool,
        distinct_sessions: u64,
    ) -> Source {
        let status = if !root_exists {
            SourceStatus::Missing
        } else if result.skipped_files > 0 || result.malformed_records > 0 {
            SourceStatus::Partial
        } else {
            SourceStatus::Ok
        };
        Source {
            fingerprint: SourceFingerprint {
                host_id: "local".into(),
                provider,
                resolved_home_path: root.to_string_lossy().into_owned(),
                volume_id: reader::volume_id(root),
            },
            status,
            scanned_files: result.scanned_files,
            skipped_files: result.skipped_files,
            malformed_records: result.malformed_records,
            distinct_sessions,
            message: (!root_exists).then(|| "履歴ディレクトリが見つかりません。".into()),
        }
    }

    pub(crate) async fn summary(
        &self,
        input: SummaryInput,
        homes: Vec<(ProviderKind, PathBuf)>,
    ) -> Result<Summary, String> {
        validate_input(&input)?;
        self.cache_loaded
            .get_or_init(|| async {
                self.load_persisted_cache().await;
            })
            .await;
        self.load_rates(false).await;
        let started = std::time::Instant::now();
        let aggregate_input = input.clone();
        let (rates, cache, scan_cache_path) = {
            let rates = self
                .rates
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .table
                .clone();
            let cache = self.cache.clone();
            (rates, cache, self.scan_cache_path.clone())
        };
        let source_result = tokio::task::spawn_blocking(move || {
            let overrides = pricing::override_table(&aggregate_input.price_overrides);
            let mut all_records = Vec::new();
            let mut sources = Vec::new();
            let mut cache_guard = cache.lock().unwrap_or_else(|error| error.into_inner());
            for (provider_kind, home) in homes {
                let provider = match provider_kind {
                    ProviderKind::Claude => Provider::Claude,
                    ProviderKind::Codex => Provider::Codex,
                };
                let root = Self::provider_home(provider, &home);
                let exists = root.is_dir();
                let files = if exists {
                    reader::read_files(&root, provider, &mut cache_guard)
                } else {
                    Ok(reader::ReadFiles::default())
                };
                let result = match files {
                    Ok(result) => result,
                    Err(error) => {
                        sources.push(Source {
                            fingerprint: SourceFingerprint {
                                host_id: "local".into(),
                                provider,
                                resolved_home_path: root.to_string_lossy().into_owned(),
                                volume_id: reader::volume_id(&root),
                            },
                            status: SourceStatus::Failed,
                            scanned_files: 0,
                            skipped_files: 0,
                            malformed_records: 0,
                            distinct_sessions: 0,
                            message: Some(error),
                        });
                        continue;
                    }
                };
                let source_path = root.to_string_lossy().into_owned();
                let distinct_sessions = result
                    .records
                    .iter()
                    .filter(|record| aggregation::in_window(&aggregate_input, record.timestamp_ms))
                    .filter_map(|record| {
                        (!record.session_id.is_empty()).then_some(&record.session_id)
                    })
                    .collect::<std::collections::BTreeSet<_>>()
                    .len() as u64;
                all_records.extend(
                    result
                        .records
                        .iter()
                        .cloned()
                        .map(|record| (record, source_path.clone())),
                );
                sources.push(Self::source(
                    provider,
                    &root,
                    &result,
                    exists,
                    distinct_sessions,
                ));
            }
            let (buckets, _stats) =
                aggregation::aggregate(&aggregate_input, all_records, &rates, &overrides);
            let _ =
                std::fs::create_dir_all(scan_cache_path.parent().unwrap_or_else(|| Path::new(".")));
            if let Ok(bytes) = serde_json::to_vec(&*cache_guard) {
                let _ = std::fs::write(scan_cache_path, bytes);
            }
            Ok::<_, String>((buckets, sources))
        })
        .await
        .map_err(|error| error.to_string())??;
        Ok(Summary {
            contract_version: usage::CONTRACT_VERSION,
            read_at: chrono::DateTime::<chrono::Utc>::from(Self::now())
                .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true),
            time_zone: input.time_zone,
            since_day: input.since_day,
            until_day: input.until_day,
            buckets: source_result.0,
            sources: source_result.1,
            pricing: self.pricing(),
            scan_duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    pub(crate) async fn load_persisted_cache(&self) {
        if let Ok(bytes) = tokio::fs::read(&self.scan_cache_path).await
            && let Ok(cache) = serde_json::from_slice::<reader::ScanCache>(&bytes)
        {
            *self.cache.lock().unwrap_or_else(|error| error.into_inner()) = cache;
        }
    }
}

fn validate_input(input: &SummaryInput) -> Result<(), String> {
    let valid_day = |day: &str| {
        day.len() == 10
            && day.as_bytes()[4] == b'-'
            && day.as_bytes()[7] == b'-'
            && day
                .bytes()
                .enumerate()
                .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    };
    if !valid_day(&input.since_day)
        || !valid_day(&input.until_day)
        || input.since_day > input.until_day
        || input.time_zone.trim().is_empty()
    {
        return Err("使用期間が不正です。".into());
    }
    if matches!(input.resolution, Some(usage::Resolution::Hour))
        && (input.since_time.is_none() || input.until_time.is_none())
    {
        return Err("時間単位の使用量には時刻範囲が必要です。".into());
    }
    if matches!(input.resolution, Some(usage::Resolution::Hour)) {
        let since = input
            .since_time
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .ok_or_else(|| "時間単位の使用量の開始時刻が不正です。".to_owned())?;
        let until = input
            .until_time
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .ok_or_else(|| "時間単位の使用量の終了時刻が不正です。".to_owned())?;
        let duration_ms = until
            .timestamp_millis()
            .saturating_sub(since.timestamp_millis());
        if !(1..=86_400_000).contains(&duration_ms) {
            return Err("時間単位の使用量の範囲は24時間以内にしてください。".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_sources_are_reported_without_fake_success() {
        let source = UsageService::source(
            Provider::Claude,
            Path::new("/missing/projects"),
            &reader::ReadFiles::default(),
            false,
            0,
        );
        assert_eq!(source.status, SourceStatus::Missing);
        assert_eq!(source.scanned_files, 0);
    }

    #[test]
    fn hourly_windows_require_valid_ordered_bounds() {
        let mut input = SummaryInput::daily("2026-01-01", "2026-01-01");
        input.resolution = Some(usage::Resolution::Hour);
        input.since_time = Some("2026-01-01T01:00:00Z".into());
        input.until_time = Some("2026-01-01T00:00:00Z".into());
        assert!(validate_input(&input).is_err());
    }
}

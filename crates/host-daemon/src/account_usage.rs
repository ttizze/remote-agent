use agent_protocol::operations::{AccountUsage, UsageWindow};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) type UsageCache = HashMap<String, Arc<UsageEntry>>;

#[derive(Default)]
pub(crate) struct UsageEntry(tokio::sync::Mutex<Option<(Instant, AccountUsage)>>);

impl UsageEntry {
    pub(crate) async fn read(
        &self,
        fetch: impl std::future::Future<Output = Result<Vec<UsageWindow>, String>>,
    ) -> AccountUsage {
        // Serialize only requests for this account's usage, never account selection.
        let mut cached = self.0.lock().await;
        if let Some((at, usage)) = cached.as_ref()
            && at.elapsed() < Duration::from_secs(60)
        {
            return usage.clone();
        }
        let result = tokio::time::timeout(Duration::from_secs(8), fetch).await;
        let (windows, error) = match result {
            Ok(Ok(windows)) if !windows.is_empty() => (windows, None),
            _ => (
                Vec::new(),
                Some(
                    "使用量を取得できませんでした。しばらくしてから再読み込みしてください。".into(),
                ),
            ),
        };
        let usage = AccountUsage {
            windows,
            fetched_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            error,
        };
        *cached = Some((Instant::now(), usage.clone()));
        usage
    }
}

pub(crate) fn codex(value: &Value) -> Vec<UsageWindow> {
    let buckets: Vec<&Value> = match value["rateLimitsByLimitId"].as_object() {
        Some(buckets) => buckets.values().collect(),
        None => vec![&value["rateLimits"]],
    };
    buckets
        .into_iter()
        .flat_map(|bucket| {
            ["primary", "secondary"].into_iter().filter_map(move |key| {
                let window = &bucket[key];
                let duration = window["windowDurationMins"].as_u64()?;
                if duration == 0 {
                    return None;
                }
                let period = match duration {
                    10080 => "週間枠".into(),
                    n if n % 60 == 0 => format!("{}時間枠", n / 60),
                    n => format!("{n}分枠"),
                };
                let name = bucket["limitName"].as_str().or(bucket["limitId"].as_str());
                let label = match name.filter(|name| *name != "codex" && !name.is_empty()) {
                    Some(name) => format!("{name} · {period}"),
                    None => period,
                };
                UsageWindow::from_used(
                    label,
                    window["usedPercent"].as_f64()?,
                    window["resetsAt"].as_i64(),
                )
            })
        })
        .collect()
}

pub(crate) fn claude(value: &Value) -> Vec<UsageWindow> {
    let limits = &value["rate_limits"];
    [
        ("five_hour", "5時間枠"),
        ("seven_day", "週間枠"),
        ("seven_day_oauth_apps", "アプリ週間枠"),
        ("seven_day_opus", "Opus 週間枠"),
        ("seven_day_sonnet", "Sonnet 週間枠"),
    ]
    .into_iter()
    .filter_map(|(key, label)| {
        let window = &limits[key];
        let resets = window["resets_at"]
            .as_str()
            .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
            .map(|date| date.timestamp());
        UsageWindow::from_used(label.into(), window["utilization"].as_f64()?, resets)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_windows_preserve_missing_values_and_normalize_remaining_usage() {
        let windows = codex(
            &json!({"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":28.2,"windowDurationMins":300,"resetsAt":1900000000},"secondary":null},"other":{"limitName":"Extra","primary":{"usedPercent":110,"windowDurationMins":10080}}}}),
        );
        assert_eq!(windows[0].remaining_percent, 71);
        assert_eq!(windows[0].label, "5時間枠");
        assert_eq!(windows[1].remaining_percent, 0);
        assert_eq!(windows[1].label, "Extra · 週間枠");
        let windows = claude(
            &json!({"rate_limits":{"five_hour":{"utilization":72,"resets_at":"2030-03-17T10:00:00Z"},"seven_day":{"utilization":null}}}),
        );
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].remaining_percent, 28);
        assert!(windows[0].resets_at.is_some());
        assert!(claude(&json!({"rate_limits":null})).is_empty());
    }

    #[tokio::test]
    async fn cache_is_scoped_to_account_and_expired_failures_replace_old_usage() {
        let mut cache = UsageCache::default();
        let a = cache.entry("a".into()).or_default().clone();
        let usage = a
            .read(async {
                Ok(vec![
                    UsageWindow::from_used("5時間枠".into(), 20., None).unwrap(),
                ])
            })
            .await;
        let cached = a
            .read(async { panic!("fresh usage must not be fetched again") })
            .await;
        assert_eq!(cached.windows, usage.windows);
        let failed = cache
            .entry("b".into())
            .or_default()
            .read(async { Err("private upstream error".into()) })
            .await;
        assert!(failed.windows.is_empty());
        assert!(!failed.error.unwrap().contains("private"));
        a.0.lock().await.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(61);
        let expired = a.read(async { Err("private upstream error".into()) }).await;
        assert!(expired.windows.is_empty());
        assert!(expired.error.is_some());
        cache.remove("a");
        assert!(!Arc::ptr_eq(&a, cache.entry("a".into()).or_default()));
    }
}

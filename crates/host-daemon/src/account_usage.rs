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
    value["rate_limits"]["limits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|window| {
            let period = match window["kind"].as_str()? {
                "session" => "5時間枠",
                "weekly_all" | "weekly_scoped" => "週間枠",
                _ => return None,
            };
            let scope = &window["scope"];
            let name = scope["model"]["display_name"]
                .as_str()
                .or_else(|| scope["surface"]["display_name"].as_str());
            let label = match name.filter(|name| !name.is_empty()) {
                Some(name) => format!("{name} · {period}"),
                None => period.into(),
            };
            let resets = window["resets_at"]
                .as_str()
                .and_then(|date| chrono::DateTime::parse_from_rfc3339(date).ok())
                .map(|date| date.timestamp());
            UsageWindow::from_used(label, window["percent"].as_f64()?, resets)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn codex_windows_preserve_missing_values_and_normalize_remaining_usage() {
        let windows = codex(
            &json!({"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":28.2,"windowDurationMins":300,"resetsAt":1900000000},"secondary":null},"other":{"limitName":"Extra","primary":{"usedPercent":110,"windowDurationMins":10080}}}}),
        );
        assert_eq!(windows[0].remaining_percent, 71);
        assert_eq!(windows[0].label, "5時間枠");
        assert_eq!(windows[1].remaining_percent, 0);
        assert_eq!(windows[1].label, "Extra · 週間枠");
    }

    #[test]
    fn claude_windows_preserve_server_order_and_model_and_surface_scopes() {
        let windows = claude(&json!({"rate_limits": {"limits": [
            {"kind":"session","percent":72,"resets_at":"2033-05-18T03:33:20Z","scope":null},
            {"kind":"weekly_all","percent":39,"resets_at":"invalid"},
            {"kind":"weekly_scoped","percent":34,"scope":{"model":{"display_name":"Fable"}}},
            {"kind":"weekly_scoped","percent":0,"scope":{"surface":{"display_name":"アプリ"}}},
            {"kind":"weekly_scoped","percent":100,"scope":{"model":{"display_name":"Sonnet"}}},
            {"kind":"weekly_all","percent":null},
            {"kind":"unknown","percent":10},
            null
        ]}}));
        assert_eq!(
            windows,
            vec![
                UsageWindow {
                    label: "5時間枠".into(),
                    remaining_percent: 28,
                    resets_at: Some(2000000000)
                },
                UsageWindow {
                    label: "週間枠".into(),
                    remaining_percent: 61,
                    resets_at: None
                },
                UsageWindow {
                    label: "Fable · 週間枠".into(),
                    remaining_percent: 66,
                    resets_at: None
                },
                UsageWindow {
                    label: "アプリ · 週間枠".into(),
                    remaining_percent: 100,
                    resets_at: None
                },
                UsageWindow {
                    label: "Sonnet · 週間枠".into(),
                    remaining_percent: 0,
                    resets_at: None
                },
            ]
        );
        assert!(claude(&json!({"rate_limits":null})).is_empty());
        assert!(claude(&json!({"rate_limits":{"limits":[]}})).is_empty());
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

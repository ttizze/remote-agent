use agent_core::client::{AccountUsage, UsageWindow};
use serde_json::Value;
use std::{
    collections::HashMap,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Default)]
pub(crate) struct UsageCache(HashMap<String, (Instant, AccountUsage)>);

impl UsageCache {
    pub(crate) fn remove(&mut self, id: &str) {
        self.0.remove(id);
    }
    pub(crate) fn get(&self, id: &str) -> Option<AccountUsage> {
        self.0
            .get(id)
            .filter(|(at, _)| at.elapsed() < Duration::from_secs(60))
            .map(|(_, usage)| usage.clone())
    }

    pub(crate) fn save(
        &mut self,
        id: String,
        result: Result<Vec<UsageWindow>, String>,
    ) -> AccountUsage {
        let (windows, error) = match result {
            Ok(windows) if !windows.is_empty() => (windows, None),
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
        self.0.insert(id, (Instant::now(), usage.clone()));
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

    #[test]
    fn cached_failures_do_not_expose_old_or_other_account_usage() {
        let mut cache = UsageCache::default();
        cache.save(
            "a".into(),
            Ok(vec![
                UsageWindow::from_used("5時間枠".into(), 20., None).unwrap(),
            ]),
        );
        assert!(cache.get("b").is_none());
        let failed = cache.save("a".into(), Err("private upstream error".into()));
        assert!(failed.windows.is_empty());
        assert!(!failed.error.unwrap().contains("private"));
    }
}

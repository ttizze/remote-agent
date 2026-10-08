use agent_protocol::operations::{AccountUsage, UsageWindow};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) type UsageCache = HashMap<String, Arc<UsageEntry>>;

#[derive(Debug, Default)]
pub(crate) struct UsageSnapshot {
    pub windows: Vec<UsageWindow>,
    pub credential_fingerprint: Option<String>,
    pub reset_credits: Option<agent_protocol::usage::ResetCredits>,
    pub external_usage: Option<agent_protocol::usage::ExternalUsage>,
}

#[derive(Default)]
pub(crate) struct UsageEntry(tokio::sync::Mutex<Option<(Instant, AccountUsage)>>);

impl UsageEntry {
    pub(crate) async fn read(
        &self,
        fetch: impl std::future::Future<Output = Result<UsageSnapshot, String>>,
    ) -> AccountUsage {
        // Serialize only requests for this account's usage, never account selection.
        let mut cached = self.0.lock().await;
        if let Some((at, usage)) = cached.as_ref()
            && at.elapsed() < Duration::from_secs(60)
        {
            return usage.clone();
        }
        let result = tokio::time::timeout(Duration::from_secs(8), fetch).await;
        let snapshot = match result {
            Ok(Ok(snapshot))
                if !snapshot.windows.is_empty() || snapshot.reset_credits.is_some() =>
            {
                snapshot
            }
            _ => {
                // A transient provider failure must not erase the last usable
                // limits. Keep its original fetched_at and cache age so a
                // later refresh can retry instead of making stale data look
                // fresh. Unsupported accounts have no usable snapshot and
                // therefore continue to receive the bounded error below.
                if let Some((_, previous)) = cached.as_ref()
                    && (!previous.windows.is_empty() || previous.reset_credits.is_some())
                {
                    return previous.clone();
                }
                let usage = AccountUsage {
                    windows: Vec::new(),
                    fetched_at: SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64,
                    error: Some(
                        "使用量を取得できませんでした。しばらくしてから再読み込みしてください。"
                            .into(),
                    ),
                    credential_fingerprint: None,
                    reset_credits: None,
                    external_usage: None,
                };
                *cached = Some((Instant::now(), usage.clone()));
                return usage;
            }
        };
        let usage = AccountUsage {
            windows: snapshot.windows,
            fetched_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64,
            error: None,
            credential_fingerprint: snapshot.credential_fingerprint,
            reset_credits: snapshot.reset_credits,
            external_usage: snapshot.external_usage,
        };
        *cached = Some((Instant::now(), usage.clone()));
        usage
    }

    pub(crate) async fn invalidate(&self) {
        *self.0.lock().await = None;
    }
}

pub(crate) async fn invalidate_after_reset<T>(
    entry: Option<Arc<UsageEntry>>,
    result: Result<T, String>,
) -> Result<T, String> {
    if let Some(entry) = entry {
        entry.invalidate().await;
    }
    result
}

pub(crate) fn codex_reset_credits(value: &Value) -> Option<agent_protocol::usage::ResetCredits> {
    let credits = value
        .get("rateLimitResetCredits")
        .or_else(|| value.get("rate_limit_reset_credits"))?;
    let available = credits
        .get("credits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|credit| {
            credit
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| status.eq_ignore_ascii_case("available"))
        })
        .collect::<Vec<_>>();
    let available_count = credits
        .get("availableCount")
        .or_else(|| credits.get("available_count"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(available.len().min(u32::MAX as usize) as u32);
    let next_expires_at = available
        .iter()
        .filter_map(|credit| {
            credit
                .get("expiresAt")
                .or_else(|| credit.get("expires_at"))
                .and_then(|value| {
                    value
                        .as_i64()
                        .or_else(|| value.as_f64().map(|value| value as i64))
                })
        })
        .min();
    let next_credit_id = available.iter().find_map(|credit| {
        credit
            .get("id")
            .or_else(|| credit.get("creditId"))
            .or_else(|| credit.get("credit_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    Some(agent_protocol::usage::ResetCredits {
        available_count,
        next_expires_at,
        next_credit_id,
    })
}

pub(crate) fn codex(value: &Value) -> Vec<UsageWindow> {
    let bucket = value["rateLimitsByLimitId"]
        .as_object()
        .and_then(|buckets| buckets.get("codex"))
        .unwrap_or(&value["rateLimits"]);
    if bucket["limitId"]
        .as_str()
        .is_some_and(|limit_id| limit_id != "codex")
    {
        return Vec::new();
    }
    let monthly_plan = matches!(bucket["planType"].as_str(), Some("free" | "go"));
    [
        ("primary", if monthly_plan { 43_200 } else { 300 }),
        ("secondary", 10_080),
    ]
    .into_iter()
    .filter_map(|(key, fallback_duration)| {
        let window = &bucket[key];
        let duration = window["windowDurationMins"]
            .as_u64()
            .filter(|duration| *duration > 0)
            .unwrap_or(fallback_duration);
        let period = match duration {
            10_080 => "週間枠".into(),
            n if n >= 43_200 => "月間枠".into(),
            n if n % 60 == 0 => format!("{}時間枠", n / 60),
            n => format!("{n}分枠"),
        };
        let name = bucket["limitName"].as_str().or(bucket["limitId"].as_str());
        let label = match name.filter(|name| *name != "codex" && !name.is_empty()) {
            Some(name) => format!("{name} · {period}"),
            None => period,
        };
        let used = window["usedPercent"].as_f64()?;
        UsageWindow::from_used(label, used, window["resetsAt"].as_i64()).map(|mut window| {
            window.id = Some(key.to_owned());
            window.kind = Some(match duration {
                n if n >= 43_200 => agent_protocol::usage::WindowKind::Monthly,
                10_080 => agent_protocol::usage::WindowKind::Weekly,
                n if n >= 60 => agent_protocol::usage::WindowKind::Session,
                _ => agent_protocol::usage::WindowKind::Other,
            });
            window.window_duration_mins = u32::try_from(duration).ok();
            window
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
        UsageWindow::from_used(label.into(), window["utilization"].as_f64()?, resets).map(
            |mut window| {
                window.id = Some(key.into());
                window.kind = Some(if key == "seven_day" || key.starts_with("seven_day_") {
                    agent_protocol::usage::WindowKind::Weekly
                } else {
                    agent_protocol::usage::WindowKind::Session
                });
                window.window_duration_mins = Some(if key == "five_hour" { 300 } else { 10080 });
                window
            },
        )
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
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, Some(28.2));
        assert_eq!(windows[0].remaining_percent, 72);
        assert_eq!(windows[0].label, "5時間枠");
        let fallback = codex(&json!({
            "rateLimits": {
                "planType": "free",
                "primary": {"usedPercent": 10},
                "secondary": {"usedPercent": 20}
            }
        }));
        assert_eq!(fallback[0].window_duration_mins, Some(43_200));
        assert_eq!(
            fallback[0].kind,
            Some(agent_protocol::usage::WindowKind::Monthly)
        );
        let windows = claude(
            &json!({"rate_limits":{"five_hour":{"utilization":72,"resets_at":"2030-03-17T10:00:00Z"},"seven_day":{"utilization":null}}}),
        );
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].remaining_percent, 28);
        assert!(windows[0].resets_at.is_some());
        assert!(claude(&json!({"rate_limits":null})).is_empty());
    }

    #[test]
    fn codex_reset_credits_count_available_grants_and_soonest_expiry() {
        let credits = codex_reset_credits(&json!({
            "rateLimitResetCredits": {
                "availableCount": 2,
                "credits": [
                    {"id": "first", "status": "available", "expiresAt": 1_784_500_000},
                    {"id": "used", "status": "redeemed", "expiresAt": 1_700_000_000},
                    {"id": "second", "status": "available", "expiresAt": 1_784_000_000}
                ]
            }
        }))
        .unwrap();
        assert_eq!(credits.available_count, 2);
        assert_eq!(credits.next_expires_at, Some(1_784_000_000));
        assert_eq!(credits.next_credit_id.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn cache_is_scoped_to_account_and_expired_failures_keep_old_usage() {
        let mut cache = UsageCache::default();
        let a = cache.entry("a".into()).or_default().clone();
        let usage = a
            .read(async {
                Ok(UsageSnapshot {
                    windows: vec![UsageWindow::from_used("5時間枠".into(), 20., None).unwrap()],
                    ..Default::default()
                })
            })
            .await;
        let fetched_at = usage.fetched_at;
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
        assert_eq!(expired.windows, usage.windows);
        assert_eq!(expired.fetched_at, fetched_at);
        assert!(expired.error.is_none());
        let retried = a
            .read(async {
                Ok(UsageSnapshot {
                    windows: vec![UsageWindow::from_used("5時間枠".into(), 32., None).unwrap()],
                    ..Default::default()
                })
            })
            .await;
        assert_eq!(retried.windows[0].used_percent, Some(32.));
        cache.remove("a");
        assert!(!Arc::ptr_eq(&a, cache.entry("a".into()).or_default()));
    }

    #[tokio::test]
    async fn empty_provider_response_is_not_reported_as_success() {
        let entry = UsageEntry::default();
        let usage = entry.read(async { Ok(UsageSnapshot::default()) }).await;
        assert!(usage.windows.is_empty());
        assert!(usage.error.is_some());
    }

    #[tokio::test]
    async fn empty_refresh_preserves_the_last_usable_snapshot() {
        let entry = UsageEntry::default();
        let first = entry
            .read(async {
                Ok(UsageSnapshot {
                    windows: vec![UsageWindow::from_used("週間枠".into(), 41.2, None).unwrap()],
                    credential_fingerprint: Some("provider-account".into()),
                    reset_credits: Some(agent_protocol::usage::ResetCredits {
                        available_count: 1,
                        next_expires_at: Some(1_900_000_000),
                        next_credit_id: Some("credit-1".into()),
                    }),
                    external_usage: Some(agent_protocol::usage::ExternalUsage {
                        label: "Provider usage".into(),
                        url: "https://example.invalid/usage".into(),
                    }),
                })
            })
            .await;
        entry.0.lock().await.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(61);

        let refreshed = entry.read(async { Ok(UsageSnapshot::default()) }).await;

        assert_eq!(refreshed, first);
    }

    #[tokio::test]
    async fn reset_outcome_invalidates_cached_usage_even_when_redemption_fails() {
        let entry = Arc::new(UsageEntry::default());
        let first = entry
            .read(async {
                Ok(UsageSnapshot {
                    windows: vec![UsageWindow::from_used("5時間枠".into(), 35.9, None).unwrap()],
                    ..Default::default()
                })
            })
            .await;
        assert_eq!(first.windows[0].used_percent, Some(35.9));

        let result = invalidate_after_reset(
            Some(entry.clone()),
            Err::<(), _>("No reset credit is available.".into()),
        )
        .await;
        assert_eq!(result.unwrap_err(), "No reset credit is available.");

        let refreshed = entry
            .read(async {
                Ok(UsageSnapshot {
                    windows: vec![UsageWindow::from_used("5時間枠".into(), 82.4, None).unwrap()],
                    ..Default::default()
                })
            })
            .await;
        assert_eq!(refreshed.windows[0].used_percent, Some(82.4));
    }
}

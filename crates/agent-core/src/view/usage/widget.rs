use super::limits::pooled_usage_limits;
use agent_protocol::{operations::Account, provider::ProviderKind};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const MAX_AGE_MS: i64 = 15 * 60_000;
const MAX_STORED_WINDOWS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetQuota {
    pub label: String,
    pub kind: String,
    pub remaining: u32,
    pub reset_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetProvider {
    pub name: String,
    pub detail: String,
    pub windows: Vec<WidgetQuota>,
    pub expires_at: i64,
    pub total_windows: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WidgetEntry {
    pub date: i64,
    pub checked_at: i64,
    pub providers: Vec<WidgetProvider>,
}

/// Display data only: no account identities, credentials, errors or remote URLs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionWidget {
    pub entries: Vec<WidgetEntry>,
}

pub fn subscription_widget(
    accounts: &[Account],
    configured: &[ProviderKind],
    now_ms: i64,
    max_windows: usize,
) -> SubscriptionWidget {
    let pools = pooled_usage_limits(accounts, now_ms);
    let mut providers = Vec::new();
    let mut checked = Vec::new();
    for (kind, name) in [
        (ProviderKind::Codex, "Codex"),
        (ProviderKind::Claude, "Claude"),
    ] {
        let kind_accounts: Vec<_> = accounts
            .iter()
            .filter(|account| account.provider == kind)
            .collect();
        if !kind_accounts.is_empty()
            && kind_accounts
                .iter()
                .all(|account| account.plan_type.as_deref() == Some("API key"))
        {
            continue;
        }
        let pool = pools
            .iter()
            .find(|pool| pool.provider == format!("{kind:?}"));
        if pool.is_none() && !configured.contains(&kind) {
            continue;
        }
        let members = pool.map_or_else(Vec::new, |pool| pool.accounts.clone());
        let mut expires = i64::MAX;
        for account in &members {
            let fetched = account.fetched_at.saturating_mul(1000);
            checked.push(fetched);
            expires = expires.min(if fetched <= 0 {
                0
            } else {
                fetched.saturating_add(MAX_AGE_MS)
            });
            for window in &account.windows {
                if let Some(reset) = window.resets_at {
                    expires = expires.min(reset.saturating_mul(1000));
                }
            }
        }
        let mut windows: Vec<_> = pool
            .map(|pool| {
                pool.windows
                    .iter()
                    .map(|window| {
                        let kind_rank = match window.kind.as_str() {
                            "session" => 0,
                            "weekly" => 1,
                            "monthly" => 2,
                            _ => 3,
                        };
                        (
                            kind_rank,
                            window.id.clone(),
                            WidgetQuota {
                                label: window.label.clone(),
                                kind: window.kind.clone(),
                                remaining: window.remaining_percent,
                                reset_at: window.resets_at.map(|value| value.saturating_mul(1000)),
                            },
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        windows.sort_by_key(|(kind, id, window)| (window.remaining, *kind, id.clone()));
        let total = windows.len() as u32;
        let mut selected = Vec::new();
        for kind in [0, 1] {
            if let Some(index) = windows.iter().position(|window| window.0 == kind) {
                selected.push(index);
            }
        }
        for index in 0..windows.len() {
            if !selected.contains(&index) {
                selected.push(index);
            }
        }
        selected.truncate(max_windows.min(MAX_STORED_WINDOWS));
        selected.sort_unstable();
        let fresh = !members.is_empty() && expires > now_ms;
        providers.push(WidgetProvider {
            name: name.into(),
            detail: if members.is_empty() {
                "No limits available".into()
            } else if !fresh {
                "Open app to refresh".into()
            } else if members.len() > 1 {
                format!("{} accounts · pooled", members.len())
            } else {
                "Subscription remaining".into()
            },
            windows: if fresh {
                selected
                    .into_iter()
                    .map(|index| windows[index].2.clone())
                    .collect()
            } else {
                Vec::new()
            },
            expires_at: if fresh { expires } else { 0 },
            total_windows: if fresh { total } else { 0 },
        });
    }
    let checked_at = checked.into_iter().min().unwrap_or(0);
    let mut dates = BTreeSet::from([now_ms]);
    dates.extend(
        providers
            .iter()
            .map(|provider| provider.expires_at)
            .filter(|date| *date > now_ms),
    );
    SubscriptionWidget {
        entries: dates
            .into_iter()
            .map(|date| WidgetEntry {
                date,
                checked_at,
                providers: providers
                    .iter()
                    .map(|provider| {
                        let mut displayed = provider.clone();
                        if !displayed.windows.is_empty() && displayed.expires_at <= date {
                            displayed.windows.clear();
                            displayed.total_windows = 0;
                            displayed.detail = "Open app to refresh".into();
                        }
                        displayed
                    })
                    .collect(),
            })
            .collect(),
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn subscription_usage_widgets_json(
    snapshots: Vec<std::sync::Arc<crate::state::Snapshot>>,
    max_windows: u32,
) -> String {
    let mut environments = crate::environment::EnvironmentRegistry::default();
    for snapshot in snapshots {
        environments.update(snapshot);
    }
    let inputs = environments.usage_snapshot();
    serde_json::to_string(&subscription_widget(
        &inputs.accounts,
        &inputs.configured_provider_kinds,
        0,
        max_windows as usize,
    ))
    .expect("widget display data serializes")
}

/// The OS supplies its layout and clock; shared rules select the displayed quotas.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn subscription_widget_entry_json(
    json: String,
    now_ms: i64,
    layout: String,
    codex_period: String,
    claude_period: String,
) -> String {
    let stored = (json.len() <= 256 * 1024)
        .then(|| serde_json::from_str::<SubscriptionWidget>(&json).ok())
        .flatten();
    let mut entry = stored
        .and_then(|view| {
            view.entries
                .into_iter()
                .filter(|entry| entry.date <= now_ms)
                .max_by_key(|entry| entry.date)
        })
        .unwrap_or(WidgetEntry {
            date: now_ms,
            checked_at: 0,
            providers: Vec::new(),
        });
    for provider in &mut entry.providers {
        let period = if provider.name == "Claude" {
            &claude_period
        } else {
            &codex_period
        };
        let all = provider.windows.clone();
        if matches!(period.as_str(), "session" | "weekly") {
            provider.windows.retain(|window| &window.kind == period);
            if provider.windows.is_empty() && !all.is_empty() {
                provider.detail = format!("No {period} limit reported");
            }
        }
        if layout == "accessoryRectangular" {
            provider.windows.truncate(1);
        } else {
            if period == "auto" {
                provider
                    .windows
                    .sort_by_key(|window| match window.kind.as_str() {
                        "session" => 0,
                        "weekly" => 1,
                        _ => 2,
                    });
            }
            let limit = match layout.as_str() {
                "systemExtraLarge" => 6,
                "systemLarge" => 4,
                _ => 2,
            };
            provider.windows.truncate(limit);
        }
    }
    serde_json::to_string(&entry).expect("widget display data serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::{
        operations::{AccountUsage, UsageWindow},
        usage::WindowKind,
    };
    const NOW: i64 = 1_900_000_000_000;
    fn account(email: &str, used: u32, fetched: i64) -> Account {
        Account {
            id: "subscription".into(),
            provider: ProviderKind::Codex,
            email: Some(email.into()),
            plan_type: None,
            usage: Some(AccountUsage {
                fetched_at: fetched / 1000,
                error: None,
                credential_fingerprint: None,
                reset_credits: None,
                external_usage: None,
                windows: vec![UsageWindow {
                    id: Some("session".into()),
                    kind: Some(WindowKind::Session),
                    label: "5 hours".into(),
                    used_percent: Some(used),
                    remaining_percent: 100 - used,
                    window_duration_mins: Some(300),
                    resets_at: Some((NOW + 10 * 60_000) / 1000),
                }],
            }),
        }
    }
    #[test]
    fn freshest_copy_is_deduplicated_and_only_display_data_reaches_os() {
        let view = subscription_widget(
            &[
                account("private@example.test", 40, NOW),
                account(" PRIVATE@example.test ", 80, NOW + 60_000),
            ],
            &[],
            NOW,
            6,
        );
        assert_eq!(view.entries[0].providers[0].windows[0].remaining, 20);
        assert_eq!(
            view.entries[0].providers[0].detail,
            "Subscription remaining"
        );
        assert_eq!(view.entries[0].checked_at, NOW + 60_000);
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("private@"));
        assert!(!json.contains("subscription\""));
    }
    #[test]
    fn native_publication_aggregates_hosts_and_removal_clears_the_os_snapshot() {
        let snapshot = |host: &str, account: Account| {
            std::sync::Arc::new(crate::state::Snapshot {
                environment: Some(agent_protocol::models::EnvironmentDescriptor {
                    environment_id: host.into(),
                    label: host.into(),
                    ..Default::default()
                }),
                accounts: Some(agent_protocol::operations::Accounts {
                    accounts: vec![account],
                    selected: Default::default(),
                    error: None,
                }),
                ..Default::default()
            })
        };
        let view: SubscriptionWidget = serde_json::from_str(&subscription_usage_widgets_json(
            vec![
                snapshot("host-a", account("one", 40, NOW)),
                snapshot("host-b", account("two", 80, NOW)),
            ],
            6,
        ))
        .unwrap();
        assert_eq!(view.entries[0].providers[0].detail, "2 accounts · pooled");
        assert_eq!(view.entries[0].providers[0].windows[0].remaining, 40);
        let view: SubscriptionWidget =
            serde_json::from_str(&subscription_usage_widgets_json(vec![], 6)).unwrap();
        assert!(view.entries[0].providers.is_empty());
    }
    #[test]
    fn reset_expires_without_inventing_refill_and_providers_expire_independently() {
        let first = account("one", 40, NOW);
        let mut second = account("two", 10, NOW);
        second.provider = ProviderKind::Claude;
        second.usage.as_mut().unwrap().windows[0].resets_at = None;
        let view = subscription_widget(&[first, second], &[], NOW, 6);
        assert_eq!(
            view.entries
                .iter()
                .map(|entry| entry.date)
                .collect::<Vec<_>>(),
            vec![NOW, NOW + 10 * 60_000, NOW + 15 * 60_000]
        );
        assert!(view.entries[1].providers[0].windows.is_empty());
        assert_eq!(view.entries[1].providers[1].windows[0].remaining, 90);
        assert!(
            view.entries[2]
                .providers
                .iter()
                .all(|provider| provider.windows.is_empty())
        );
    }
    #[test]
    fn pools_distinct_accounts_and_keeps_session_weekly_with_constrained_windows() {
        let mut first = account("one", 40, NOW);
        let second = account("two", 80, NOW);
        let usage = first.usage.as_mut().unwrap();
        for index in 0..20 {
            let mut window = usage.windows[0].clone();
            window.id = Some(format!("scope-{index}"));
            window.kind = Some(if index == 0 {
                WindowKind::Weekly
            } else {
                WindowKind::Other
            });
            window.used_percent = Some(95 - index);
            usage.windows.push(window);
        }
        let view = subscription_widget(&[first, second], &[], NOW, usize::MAX);
        let provider = &view.entries[0].providers[0];
        assert_eq!(provider.total_windows, 21);
        assert_eq!(provider.windows.len(), 6);
        assert!(provider.windows.iter().any(|window| window.remaining == 40));
        assert_eq!(provider.detail, "2 accounts · pooled");
    }
    #[test]
    fn failed_unknown_and_missing_usage_do_not_show_zero_quotas_or_errors() {
        let mut failed = account("private", 40, 0);
        failed.usage.as_mut().unwrap().error = Some("private token".into());
        let view = subscription_widget(&[failed], &[ProviderKind::Codex], NOW, 6);
        assert_eq!(view.entries[0].providers[0].detail, "No limits available");
        assert!(view.entries[0].providers[0].windows.is_empty());
        let unknown = subscription_widget(&[account("private", 40, 0)], &[], NOW, 6);
        assert!(unknown.entries[0].providers[0].windows.is_empty());
        let mut api = account("private", 0, NOW);
        api.plan_type = Some("API key".into());
        api.usage.as_mut().unwrap().windows.clear();
        assert!(
            subscription_widget(&[api], &[ProviderKind::Codex], NOW, 6).entries[0]
                .providers
                .is_empty()
        );
        assert!(
            subscription_widget(&[], &[], NOW, 6).entries[0]
                .providers
                .is_empty()
        );
    }
    #[test]
    fn stored_timeline_renders_after_expiry_and_period_filters_do_not_invent_quota() {
        let view = subscription_widget(&[account("private", 40, NOW)], &[], 0, 6);
        let json = serde_json::to_string(&view).unwrap();
        let display: WidgetEntry = serde_json::from_str(&subscription_widget_entry_json(
            json.clone(),
            NOW,
            "systemSmall".into(),
            "weekly".into(),
            "auto".into(),
        ))
        .unwrap();
        assert_eq!(display.providers[0].detail, "No weekly limit reported");
        assert!(display.providers[0].windows.is_empty());
        let expired: WidgetEntry = serde_json::from_str(&subscription_widget_entry_json(
            json,
            NOW + 10 * 60_000,
            "systemSmall".into(),
            "auto".into(),
            "auto".into(),
        ))
        .unwrap();
        assert_eq!(expired.providers[0].detail, "Open app to refresh");
        assert!(expired.providers[0].windows.is_empty());
    }
    proptest::proptest! {
        #[test]
        fn quotas_and_storage_remain_bounded(used in 0u32..=100, max_windows in 0usize..10) {
            let view = subscription_widget(&[account("private",used,NOW)],&[],NOW,max_windows);
            for entry in view.entries {
                for provider in entry.providers {
                    proptest::prop_assert!(provider.windows.len() <= max_windows);
                    proptest::prop_assert!(provider.windows.iter().all(|window|window.remaining <=100));
                }
            }
        }
    }
}

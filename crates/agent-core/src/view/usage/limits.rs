use agent_protocol::{
    operations::{Account, Accounts, UsageWindow},
    provider::ProviderKind,
    usage::WindowKind,
};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageLimitWindow {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub used_percent: u32,
    pub remaining_percent: u32,
    pub duration_minutes: Option<u32>,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageLimitAccount {
    pub id: String,
    pub provider: String,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub windows: Vec<UsageLimitWindow>,
    pub reset_credit_count: u32,
    pub next_credit_id: Option<String>,
    pub reset_credit_account_id: Option<String>,
    pub source_account_ids: Vec<String>,
    pub external_label: Option<String>,
    pub external_url: Option<String>,
    pub error: Option<String>,
    pub selected: bool,
    pub fetched_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerUsageLimits {
    pub account_id: String,
    pub label: String,
    pub windows: Vec<UsageLimitWindow>,
    pub reset_credit_count: u32,
    pub next_credit_id: Option<String>,
    pub external_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageLimitPoolMember {
    pub account_id: String,
    pub label: String,
    pub window: UsageLimitWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageLimitPoolColumn {
    pub account_id: String,
    pub label: String,
    pub window: Option<UsageLimitWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageLimitPoolReset {
    pub account_id: String,
    pub label: String,
    pub at: i64,
    pub restores_percent: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageLimitPoolWindow {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub used_percent: u32,
    pub remaining_percent: u32,
    pub duration_minutes: Option<u32>,
    pub resets_at: Option<i64>,
    pub members: Vec<UsageLimitPoolMember>,
    pub columns: Vec<UsageLimitPoolColumn>,
    pub pace: Option<String>,
    pub resets: Vec<UsageLimitPoolReset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct UsageLimitPool {
    pub provider: String,
    pub accounts: Vec<UsageLimitAccount>,
    pub windows: Vec<UsageLimitPoolWindow>,
}

#[derive(Debug, Clone)]
struct MergedAccount {
    account: Account,
    selected: bool,
    reset_credits: Option<agent_protocol::usage::ResetCredits>,
    reset_credits_fetched_at: Option<i64>,
    reset_credit_account_id: Option<String>,
    source_account_ids: Vec<String>,
}

fn normalized_email(account: &Account) -> Option<String> {
    account
        .email
        .as_deref()
        .map(str::trim)
        .filter(|email| !email.is_empty())
        .map(str::to_ascii_lowercase)
}

/// Use provider identity in the same order as the reference limits view:
/// email, an explicit non-secret provider fingerprint, then the scoped
/// account id. Scoped ids keep accounts from separate Hosts independent.
fn account_key(account: &Account) -> String {
    let provider = format!("{:?}", account.provider);
    if let Some(email) = normalized_email(account) {
        return format!("{provider}:email:{email}");
    }
    if let Some(fingerprint) = account
        .usage
        .as_ref()
        .and_then(|usage| usage.credential_fingerprint.as_deref())
        .map(str::trim)
        .filter(|fingerprint| !fingerprint.is_empty())
    {
        return format!("{provider}:credential:{fingerprint}");
    }
    format!("{provider}:account:{}", account.id)
}

fn usable(account: &Account) -> bool {
    account
        .usage
        .as_ref()
        .is_some_and(|usage| usage.error.is_none() && !usage.windows.is_empty())
}

fn fetched_at(account: &Account) -> i64 {
    account.usage.as_ref().map_or(0, |usage| usage.fetched_at)
}

fn merge_accounts(
    accounts: &[Account],
    selected: &HashMap<ProviderKind, String>,
) -> Vec<MergedAccount> {
    let mut indexes = HashMap::new();
    let mut merged = Vec::new();
    for account in accounts {
        let key = account_key(account);
        let is_selected = selected.get(&account.provider) == Some(&account.id);
        if let Some(index) = indexes.get(&key).copied() {
            let previous = &mut merged[index];
            let replace = usable(account)
                && (!usable(&previous.account)
                    || fetched_at(account) > fetched_at(&previous.account));
            if replace {
                previous.account = account.clone();
            }
            previous.selected |= is_selected;
            if !previous
                .source_account_ids
                .iter()
                .any(|source_id| source_id == &account.id)
            {
                previous.source_account_ids.push(account.id.clone());
            }
            if let Some(credits) = account
                .usage
                .as_ref()
                .filter(|usage| usage.error.is_none())
                .and_then(|usage| usage.reset_credits.clone())
                && (previous.reset_credits_fetched_at.is_none()
                    || fetched_at(account) > previous.reset_credits_fetched_at.unwrap_or_default())
            {
                previous.reset_credits = Some(credits);
                previous.reset_credits_fetched_at = Some(fetched_at(account));
                previous.reset_credit_account_id = Some(account.id.clone());
            }
        } else {
            indexes.insert(key, merged.len());
            let reset_credits = account
                .usage
                .as_ref()
                .filter(|usage| usage.error.is_none())
                .and_then(|usage| usage.reset_credits.clone());
            merged.push(MergedAccount {
                account: account.clone(),
                selected: is_selected,
                reset_credits_fetched_at: reset_credits.as_ref().map(|_| fetched_at(account)),
                reset_credit_account_id: reset_credits.as_ref().map(|_| account.id.clone()),
                source_account_ids: vec![account.id.clone()],
                reset_credits,
            });
        }
    }
    merged
}

fn kind_name(value: Option<WindowKind>) -> &'static str {
    match value {
        Some(WindowKind::Session) => "session",
        Some(WindowKind::Weekly) => "weekly",
        Some(WindowKind::Monthly) => "monthly",
        Some(WindowKind::Other) | None => "other",
    }
}

fn kind_rank(kind: &str) -> u8 {
    match kind {
        "session" => 0,
        "weekly" => 1,
        "monthly" => 2,
        _ => 3,
    }
}

fn window(value: &UsageWindow) -> UsageLimitWindow {
    let used_percent = value
        .used_percent
        .unwrap_or_else(|| 100u32.saturating_sub(value.remaining_percent))
        .min(100);
    UsageLimitWindow {
        id: value.id.clone().unwrap_or_else(|| value.label.clone()),
        label: value.label.clone(),
        kind: kind_name(value.kind).into(),
        used_percent,
        remaining_percent: value.remaining_percent.min(100),
        duration_minutes: value.window_duration_mins,
        resets_at: value.resets_at,
    }
}

fn account_view(value: &MergedAccount) -> UsageLimitAccount {
    let usage = value.account.usage.as_ref();
    let credits = value
        .reset_credits
        .as_ref()
        .or_else(|| usage.and_then(|usage| usage.reset_credits.as_ref()));
    let reset_credit_account_id = credits.map(|_| {
        value
            .reset_credit_account_id
            .clone()
            .unwrap_or_else(|| value.account.id.clone())
    });
    let external = usage.and_then(|usage| usage.external_usage.as_ref());
    UsageLimitAccount {
        id: value.account.id.clone(),
        provider: format!("{:?}", value.account.provider),
        email: value.account.email.clone(),
        plan: value.account.plan_type.clone(),
        windows: usage
            .map(|usage| usage.windows.iter().map(window).collect())
            .unwrap_or_default(),
        reset_credit_count: credits.map_or(0, |credits| credits.available_count),
        next_credit_id: credits.and_then(|credits| credits.next_credit_id.clone()),
        reset_credit_account_id,
        source_account_ids: value.source_account_ids.clone(),
        external_label: external.map(|external| external.label.clone()),
        external_url: external.map(|external| external.url.clone()),
        error: usage.and_then(|usage| usage.error.clone()),
        selected: value.selected,
        fetched_at: fetched_at(&value.account),
    }
}

/// Projects each distinct provider account once, preferring the freshest
/// usable windows while retaining the selected account and error metadata.
pub fn usage_limits(accounts: Option<&Accounts>) -> Vec<UsageLimitAccount> {
    let Some(accounts) = accounts else {
        return Vec::new();
    };
    merge_accounts(&accounts.accounts, &accounts.selected)
        .iter()
        .map(account_view)
        .collect()
}

fn reset_millis(window: &UsageLimitWindow) -> Option<i64> {
    window.resets_at.map(|value| value.saturating_mul(1_000))
}

fn elapsed_share(window: &UsageLimitWindow, now_ms: i64) -> Option<f64> {
    let reset = reset_millis(window)?;
    let duration = i64::from(window.duration_minutes?).saturating_mul(60_000);
    (duration > 0).then(|| {
        ((duration.saturating_sub(reset.saturating_sub(now_ms)) as f64) / duration as f64)
            .clamp(0.0, 1.0)
    })
}

fn pace(used_percent: f64, elapsed: f64) -> String {
    let gap = used_percent - elapsed * 100.0;
    if gap > 5.0 {
        "ahead".into()
    } else if gap < -5.0 {
        "under".into()
    } else {
        "on".into()
    }
}

fn account_label(account: &UsageLimitAccount) -> String {
    account.email.clone().unwrap_or_else(|| account.id.clone())
}

fn order_reset(account: &UsageLimitAccount, kind: &str, id: &str) -> i64 {
    account
        .windows
        .iter()
        .find(|window| window.kind == kind && window.id == id)
        .and_then(reset_millis)
        .unwrap_or(i64::MAX)
}

fn pool_windows(accounts: &[UsageLimitAccount], now_ms: i64) -> Vec<UsageLimitPoolWindow> {
    let order_window = accounts
        .iter()
        .flat_map(|account| account.windows.iter())
        .min_by_key(|window| kind_rank(&window.kind))
        .map(|window| (window.kind.clone(), window.id.clone()));
    let (order_kind, order_id) = order_window.unwrap_or_else(|| ("other".into(), String::new()));
    let mut sorted_accounts = accounts.to_vec();
    sorted_accounts.sort_by(|left, right| {
        order_reset(left, &order_kind, &order_id)
            .cmp(&order_reset(right, &order_kind, &order_id))
            .then_with(|| {
                account_label(left)
                    .to_ascii_lowercase()
                    .cmp(&account_label(right).to_ascii_lowercase())
            })
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut groups: Vec<(String, String, Vec<(usize, UsageLimitWindow)>)> = Vec::new();
    for (account_index, account) in sorted_accounts.iter().enumerate() {
        for window in &account.windows {
            let group = groups
                .iter_mut()
                .find(|(kind, id, _)| kind == &window.kind && id == &window.id);
            if let Some((_, _, members)) = group {
                members.push((account_index, window.clone()));
            } else {
                groups.push((
                    window.kind.clone(),
                    window.id.clone(),
                    vec![(account_index, window.clone())],
                ));
            }
        }
    }
    groups.sort_by_key(|(kind, _, _)| kind_rank(kind));

    groups
        .into_iter()
        .map(|(kind, id, members)| {
            let first = &members[0].1;
            let used_average = members
                .iter()
                .map(|(_, window)| f64::from(window.used_percent))
                .sum::<f64>()
                / members.len() as f64;
            let used_percent = used_average.round() as u32;
            let mut resets = members
                .iter()
                .filter_map(|(index, window)| {
                    reset_millis(window).map(|at| UsageLimitPoolReset {
                        account_id: sorted_accounts[*index].id.clone(),
                        label: account_label(&sorted_accounts[*index]),
                        at,
                        restores_percent: (f64::from(window.used_percent) / members.len() as f64)
                            .round() as u32,
                    })
                })
                .collect::<Vec<_>>();
            resets.sort_by_key(|reset| reset.at);
            let timed = members
                .iter()
                .filter_map(|(_, window)| {
                    elapsed_share(window, now_ms).map(|elapsed| (window.used_percent, elapsed))
                })
                .collect::<Vec<_>>();
            let pace = (!timed.is_empty()).then(|| {
                let used = timed.iter().map(|(used, _)| f64::from(*used)).sum::<f64>()
                    / timed.len() as f64;
                let elapsed =
                    timed.iter().map(|(_, elapsed)| elapsed).sum::<f64>() / timed.len() as f64;
                pace(used, elapsed)
            });
            let members = members
                .iter()
                .map(|(index, window)| UsageLimitPoolMember {
                    account_id: sorted_accounts[*index].id.clone(),
                    label: account_label(&sorted_accounts[*index]),
                    window: window.clone(),
                })
                .collect::<Vec<_>>();
            let columns = sorted_accounts
                .iter()
                .map(|account| UsageLimitPoolColumn {
                    account_id: account.id.clone(),
                    label: account_label(account),
                    window: members
                        .iter()
                        .find(|member| member.account_id == account.id)
                        .map(|member| member.window.clone()),
                })
                .collect();
            UsageLimitPoolWindow {
                id,
                label: first.label.clone(),
                kind,
                used_percent,
                remaining_percent: (100.0 - used_average).round().clamp(0.0, 100.0) as u32,
                duration_minutes: first.duration_minutes,
                resets_at: resets.first().map(|reset| reset.at / 1_000),
                members,
                columns,
                pace,
                resets,
            }
        })
        .collect()
}

fn pools_from_merged(merged: &[MergedAccount], now_ms: i64) -> Vec<UsageLimitPool> {
    let mut by_provider: BTreeMap<ProviderKind, Vec<UsageLimitAccount>> = BTreeMap::new();
    for merged_account in merged {
        let account = account_view(merged_account);
        if account.windows.is_empty() || account.error.is_some() {
            continue;
        }
        by_provider
            .entry(merged_account.account.provider)
            .or_default()
            .push(account);
    }
    by_provider
        .into_iter()
        .map(|(provider, accounts)| UsageLimitPool {
            provider: format!("{provider:?}"),
            windows: pool_windows(&accounts, now_ms),
            accounts,
        })
        .collect()
}

pub(crate) fn pooled_usage_limits(accounts: &[Account], now_ms: i64) -> Vec<UsageLimitPool> {
    pools_from_merged(&merge_accounts(accounts, &HashMap::new()), now_ms)
}

pub fn usage_limit_pools(accounts: Option<&Accounts>, now_ms: i64) -> Vec<UsageLimitPool> {
    let Some(accounts) = accounts else {
        return Vec::new();
    };
    pools_from_merged(
        &merge_accounts(&accounts.accounts, &accounts.selected),
        now_ms,
    )
}

/// The composer uses the selected account for this provider when available,
/// then falls back to the first account so an unset selection still renders.
pub fn composer_usage_limits(
    accounts: Option<&Accounts>,
    provider: ProviderKind,
) -> Option<ComposerUsageLimits> {
    let accounts = accounts?;
    let merged = merge_accounts(&accounts.accounts, &accounts.selected);
    let account = merged
        .iter()
        .filter(|account| account.account.provider == provider)
        .find(|account| account.selected)
        .or_else(|| {
            merged
                .iter()
                .find(|account| account.account.provider == provider)
        })?;
    let account = account_view(account);
    Some(ComposerUsageLimits {
        account_id: account.id,
        label: account
            .email
            .map(|email| format!("Usage limits · {email}"))
            .unwrap_or_else(|| "Usage limits".into()),
        windows: account.windows,
        reset_credit_count: account.reset_credit_count,
        next_credit_id: account.next_credit_id,
        external_url: account.external_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::operations::AccountUsage;

    fn usage(
        used: u32,
        fetched_at: i64,
        fingerprint: Option<&str>,
        kind: WindowKind,
        id: &str,
        reset: Option<i64>,
    ) -> AccountUsage {
        AccountUsage {
            windows: vec![UsageWindow {
                id: Some(id.into()),
                kind: Some(kind),
                label: id.into(),
                used_percent: Some(used),
                remaining_percent: 100 - used,
                window_duration_mins: Some(300),
                resets_at: reset,
            }],
            fetched_at,
            error: None,
            credential_fingerprint: fingerprint.map(str::to_owned),
            reset_credits: None,
            external_usage: None,
        }
    }

    fn account(id: &str, email: Option<&str>, usage: AccountUsage) -> Account {
        Account {
            id: id.into(),
            provider: ProviderKind::Codex,
            email: email.map(str::to_owned),
            plan_type: Some("Subscription".into()),
            usage: Some(usage),
        }
    }

    #[test]
    fn merges_by_email_then_fingerprint_and_keeps_the_freshest_usable_read() {
        let accounts = Accounts {
            accounts: vec![
                account(
                    "old",
                    Some(" SAME@example.com "),
                    usage(20, 10, None, WindowKind::Session, "primary", Some(2_000)),
                ),
                account(
                    "fresh",
                    Some("same@example.com"),
                    usage(40, 20, None, WindowKind::Session, "primary", Some(3_000)),
                ),
                account(
                    "key-old",
                    None,
                    usage(
                        10,
                        10,
                        Some("shared"),
                        WindowKind::Session,
                        "primary",
                        Some(2_000),
                    ),
                ),
                account(
                    "key-fresh",
                    None,
                    usage(
                        30,
                        20,
                        Some("shared"),
                        WindowKind::Session,
                        "primary",
                        Some(3_000),
                    ),
                ),
            ],
            selected: HashMap::new(),
            error: None,
        };
        let view = usage_limits(Some(&accounts));
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].id, "fresh");
        assert_eq!(view[0].windows[0].used_percent, 40);
        assert_eq!(view[1].id, "key-fresh");
        assert_eq!(view[1].windows[0].used_percent, 30);
    }

    #[test]
    fn carries_forward_the_freshest_reset_credit_when_windows_come_from_another_read() {
        let mut credit_read = usage(70, 10, None, WindowKind::Session, "primary", Some(2_000));
        credit_read.reset_credits = Some(agent_protocol::usage::ResetCredits {
            available_count: 2,
            next_expires_at: Some(4_000),
            next_credit_id: Some("credit-1".into()),
        });
        let accounts = Accounts {
            accounts: vec![
                account("credit", Some("same@example.test"), credit_read),
                account(
                    "windows",
                    Some(" SAME@example.test "),
                    usage(20, 20, None, WindowKind::Session, "primary", Some(3_000)),
                ),
            ],
            selected: HashMap::new(),
            error: None,
        };
        let view = usage_limits(Some(&accounts));
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].id, "windows");
        assert_eq!(view[0].windows[0].used_percent, 20);
        assert_eq!(view[0].reset_credit_count, 2);
        assert_eq!(view[0].next_credit_id.as_deref(), Some("credit-1"));
        assert_eq!(view[0].reset_credit_account_id.as_deref(), Some("credit"));
        assert_eq!(view[0].source_account_ids, vec!["credit", "windows"]);
    }

    #[test]
    fn composer_uses_the_selected_account_instead_of_the_first_provider_row() {
        let accounts = Accounts {
            accounts: vec![
                account(
                    "first",
                    Some("first@example.test"),
                    usage(80, 10, None, WindowKind::Session, "primary", None),
                ),
                account(
                    "selected",
                    Some("selected@example.test"),
                    usage(20, 10, None, WindowKind::Session, "primary", None),
                ),
            ],
            selected: HashMap::from([(ProviderKind::Codex, "selected".into())]),
            error: None,
        };
        let view = composer_usage_limits(Some(&accounts), ProviderKind::Codex).unwrap();
        assert_eq!(view.account_id, "selected");
        assert_eq!(view.windows[0].used_percent, 20);
        assert_eq!(view.label, "Usage limits · selected@example.test");
    }

    #[test]
    fn pools_windows_by_kind_and_id_with_stable_columns_and_pace() {
        let first = account(
            "a",
            Some("a@example.test"),
            usage(80, 10, None, WindowKind::Session, "primary", Some(1_000)),
        );
        let mut second = account(
            "b",
            Some("b@example.test"),
            usage(40, 10, None, WindowKind::Session, "primary", Some(1_100)),
        );
        second.usage.as_mut().unwrap().windows.push(UsageWindow {
            id: Some("weekly".into()),
            kind: Some(WindowKind::Weekly),
            label: "weekly".into(),
            used_percent: Some(10),
            remaining_percent: 90,
            window_duration_mins: Some(10_080),
            resets_at: Some(2_000),
        });
        let accounts = vec![first, second];
        let pools = pooled_usage_limits(&accounts, 820);
        let pool = &pools[0];
        assert_eq!(pool.windows[0].remaining_percent, 40);
        assert_eq!(
            pool.windows[0]
                .columns
                .iter()
                .map(|column| column.account_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(pool.windows[0].members.len(), 2);
        assert_eq!(pool.windows[0].resets[0].account_id, "a");
        assert_eq!(pool.windows[0].resets[0].restores_percent, 40);
        assert_eq!(pool.windows[0].pace.as_deref(), Some("ahead"));
        assert_eq!(pool.windows[1].kind, "weekly");
        assert!(pool.windows[1].columns[0].window.is_none());
        assert_eq!(
            pool.windows[1].columns[1]
                .window
                .as_ref()
                .unwrap()
                .used_percent,
            10
        );
    }

    proptest::proptest! {
        #[test]
        fn pooled_percentages_and_columns_stay_bounded(used in proptest::collection::vec(0u32..=100, 1..8)) {
            let accounts = used
                .into_iter()
                .enumerate()
                .map(|(index, used)| {
                    let id = format!("account-{index}");
                    let email = format!("account-{index}@example.test");
                    account(
                        &id,
                        Some(&email),
                        usage(used, index as i64, None, WindowKind::Session, "primary", None),
                    )
                })
                .collect::<Vec<_>>();
            let pools = pooled_usage_limits(&accounts, 0);
            let pool = &pools[0];
            for window in &pool.windows {
                proptest::prop_assert!(window.used_percent <= 100);
                proptest::prop_assert!(window.remaining_percent <= 100);
                proptest::prop_assert_eq!(window.columns.len(), pool.accounts.len());
            }
        }
    }
}

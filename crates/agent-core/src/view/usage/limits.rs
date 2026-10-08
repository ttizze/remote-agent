use agent_protocol::operations::{Account, Accounts, UsageWindow};
use agent_protocol::provider::ProviderKind;

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
    pub external_label: Option<String>,
    pub external_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerUsageLimits {
    pub label: String,
    pub windows: Vec<UsageLimitWindow>,
    pub reset_credit_count: u32,
    pub next_credit_id: Option<String>,
    pub external_url: Option<String>,
}

fn window(value: &UsageWindow) -> UsageLimitWindow {
    UsageLimitWindow {
        id: value.id.clone().unwrap_or_else(|| value.label.clone()),
        label: value.label.clone(),
        kind: value
            .kind
            .as_ref()
            .map(|kind| format!("{kind:?}").to_ascii_lowercase())
            .unwrap_or_else(|| "other".into()),
        used_percent: value
            .used_percent
            .unwrap_or_else(|| 100u32.saturating_sub(value.remaining_percent)),
        remaining_percent: value.remaining_percent,
        duration_minutes: value.window_duration_mins,
        resets_at: value.resets_at,
    }
}

fn account(value: &Account) -> UsageLimitAccount {
    let usage = value.usage.as_ref();
    let credits = usage.and_then(|usage| usage.reset_credits.as_ref());
    let external = usage.and_then(|usage| usage.external_usage.as_ref());
    UsageLimitAccount {
        id: value.id.clone(),
        provider: format!("{:?}", value.provider),
        email: value.email.clone(),
        plan: value.plan_type.clone(),
        windows: usage
            .map(|usage| usage.windows.iter().map(window).collect())
            .unwrap_or_default(),
        reset_credit_count: credits.map_or(0, |credits| credits.available_count),
        next_credit_id: credits.and_then(|credits| credits.next_credit_id.clone()),
        external_label: external.map(|external| external.label.clone()),
        external_url: external.map(|external| external.url.clone()),
        error: usage.and_then(|usage| usage.error.clone()),
    }
}

/// Projects account usage once so each native client renders the same limits.
pub fn usage_limits(accounts: Option<&Accounts>) -> Vec<UsageLimitAccount> {
    accounts
        .map(|accounts| accounts.accounts.iter().map(account).collect())
        .unwrap_or_default()
}

/// The compact provider-specific row used above the composer on mobile.
pub fn composer_usage_limits(
    accounts: Option<&Accounts>,
    provider: ProviderKind,
) -> Option<ComposerUsageLimits> {
    usage_limits(accounts)
        .into_iter()
        .find(|account| {
            account
                .provider
                .eq_ignore_ascii_case(&format!("{provider:?}"))
        })
        .map(|account| ComposerUsageLimits {
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
    use agent_protocol::{operations::AccountUsage, provider::ProviderKind};

    #[test]
    fn uses_explicit_used_percent_and_preserves_credit_link() {
        let accounts = Accounts {
            accounts: vec![Account {
                id: "account".into(),
                provider: ProviderKind::Codex,
                email: None,
                plan_type: None,
                usage: Some(AccountUsage {
                    windows: vec![UsageWindow {
                        id: Some("primary".into()),
                        kind: Some(agent_protocol::usage::WindowKind::Session),
                        label: "Session".into(),
                        used_percent: Some(42),
                        remaining_percent: 58,
                        window_duration_mins: Some(300),
                        resets_at: None,
                    }],
                    fetched_at: 0,
                    error: None,
                    reset_credits: Some(agent_protocol::usage::ResetCredits {
                        available_count: 1,
                        next_expires_at: None,
                        next_credit_id: Some("credit".into()),
                    }),
                    external_usage: None,
                }),
            }],
            selected: Default::default(),
            error: None,
        };
        let view = usage_limits(Some(&accounts));
        assert_eq!(view[0].windows[0].used_percent, 42);
        assert_eq!(view[0].next_credit_id.as_deref(), Some("credit"));
    }
}

//! Account listing, selection, and login records.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub provider: crate::session::ProviderKind,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub usage: Option<AccountUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub windows: Vec<UsageWindow>,
    pub fetched_at: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub label: String,
    pub remaining_percent: u32,
    pub resets_at: Option<i64>,
}

impl UsageWindow {
    pub fn from_used(label: String, used: f64, resets_at: Option<i64>) -> Option<Self> {
        used.is_finite().then(|| Self {
            label,
            remaining_percent: (100.0 - used.clamp(0.0, 100.0)).floor() as u32,
            resets_at,
        })
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Accounts {
    pub accounts: Vec<Account>,
    pub selected_id: Option<String>,
    pub selected_claude_id: Option<String>,
    pub error: Option<String>,
}

impl Accounts {
    pub fn is_selected(&self, account: &Account) -> bool {
        (match account.provider {
            crate::session::ProviderKind::Codex => self.selected_id.as_ref(),
            crate::session::ProviderKind::Claude => self.selected_claude_id.as_ref(),
        }) == Some(&account.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartAccountLogin {
    pub provider: crate::session::ProviderKind,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SubmitAccountLogin {
    pub id: String,
    pub code: String,
}
impl std::fmt::Debug for SubmitAccountLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubmitAccountLogin")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelection {
    pub provider: crate::session::ProviderKind,
    pub selected_id: String,
    pub persistence_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLogin {
    pub login_id: String,
    pub requires_code_submission: bool,
    pub user_code: String,
    pub verification_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginStatus {
    pub completed: bool,
    pub account_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectAccount {
    #[serde(rename = "accountId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogoutAccount {
    #[serde(rename = "accountId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountUsage {
    #[serde(rename = "accountId")]
    pub id: String,
}

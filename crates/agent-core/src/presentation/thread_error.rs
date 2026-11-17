//! T3's ThreadErrorBanner message and usage-limit decisions, shared by clients.
use agent_protocol::execution::{ErrorCategory, ExecutionError};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadErrorAction {
    ManageUsage,
    CopyAdminRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadErrorBanner {
    pub text: String,
    pub collapsed_text: String,
    pub warning: bool,
    pub action: Option<ThreadErrorAction>,
}

impl ThreadErrorBanner {
    pub(super) fn from_failure(error: &ExecutionError) -> Self {
        let action = match error.provider_code.as_deref() {
            Some("subscription_sharing_usage_limit_exceeded") => {
                Some(ThreadErrorAction::ManageUsage)
            }
            Some("subscription_sharing_workplace_individual_usage_limit_exceeded") => {
                Some(ThreadErrorAction::CopyAdminRequest)
            }
            _ => None,
        };
        let payload = serde_json::from_str::<serde_json::Value>(&error.message).ok();
        let collapsed_text = payload
            .as_ref()
            .and_then(|payload| payload.get("error"))
            .and_then(|error| error.get("message"))
            .and_then(|message| message.as_str())
            .filter(|message| !message.trim().is_empty())
            .unwrap_or(&error.message)
            .to_owned();
        Self {
            text: error.message.clone(),
            collapsed_text,
            warning: error.category == ErrorCategory::UsageLimit || action.is_some(),
            action,
        }
    }
}

#[cfg(test)]
mod tests;

use crate::{provider::ProviderKind, state::Snapshot};
pub fn validate_ssh_destination(destination: &str) -> Result<&str, &'static str> {
    let destination = destination.trim();
    if destination.is_empty()
        || destination.starts_with('-')
        || !destination.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-' | '[' | ']' | ':')
        })
    {
        return Err("SSH の接続先は user@hostname の形式で入力してください。");
    }
    Ok(destination)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AgentAvailability {
    Checking,
    Ready,
    LoginRequired,
    Unavailable,
}
#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConnectionAgent {
    pub provider: ProviderKind,
    pub name: String,
    pub availability: AgentAvailability,
    pub label: String,
}
#[derive(Debug, Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConnectionSetup {
    pub agents: Vec<ConnectionAgent>,
    pub can_start: bool,
}
pub fn connection_setup(snapshot: &Snapshot) -> ConnectionSetup {
    let agents = [
        (ProviderKind::Codex, "codex", "Codex"),
        (ProviderKind::Claude, "claude", "Claude Code"),
    ]
    .into_iter()
    .map(|(provider, key, name)| {
        let availability = if !snapshot.connected {
            AgentAvailability::Checking
        } else if snapshot.model_errors.contains_key(key) {
            AgentAvailability::Unavailable
        } else if snapshot.accounts.is_none() {
            AgentAvailability::Checking
        } else if !snapshot.accounts.as_ref().is_some_and(|accounts| {
            accounts
                .accounts
                .iter()
                .any(|a| a.provider == provider && accounts.is_selected(a))
        }) {
            AgentAvailability::LoginRequired
        } else if !snapshot.models.iter().any(|m| m.model.provider == provider) {
            AgentAvailability::Unavailable
        } else {
            AgentAvailability::Ready
        };
        ConnectionAgent {
            provider,
            name: name.into(),
            availability,
            label: match availability {
                AgentAvailability::Checking => "Checking…",
                AgentAvailability::Ready => "Ready",
                AgentAvailability::LoginRequired => "Sign in",
                AgentAvailability::Unavailable => "Unavailable",
            }
            .into(),
        }
    })
    .collect::<Vec<_>>();
    ConnectionSetup {
        can_start: agents
            .iter()
            .any(|a| a.availability == AgentAvailability::Ready),
        agents,
    }
}

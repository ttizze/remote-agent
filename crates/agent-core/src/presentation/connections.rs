//! Availability is derived from the connected Host's catalog and authentication.
use crate::{models::Model, session::ProviderKind, state::Snapshot};
use agent_protocol::operations::Accounts;
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum AgentAvailability {
    Checking,
    Ready,
    LoginRequired,
    Unavailable,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConnectionAgent {
    pub provider: ProviderKind,
    pub name: String,
    pub availability: AgentAvailability,
    pub label: String,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConnectionSetup {
    pub agents: Vec<ConnectionAgent>,
    pub can_start: bool,
}

fn availability(
    connected: bool,
    provider: ProviderKind,
    models: &[Model],
    accounts: Option<&Accounts>,
    errors: &Map<String, Value>,
) -> AgentAvailability {
    if !connected {
        return AgentAvailability::Checking;
    }
    let key = match provider {
        ProviderKind::Codex => "codex",
        ProviderKind::Claude => "claude",
    };
    if errors.contains_key(key) {
        return AgentAvailability::Unavailable;
    }
    let Some(accounts) = accounts else {
        return AgentAvailability::Checking;
    };
    let has_model = models.iter().any(|model| model.model.provider == provider);
    let authenticated = accounts
        .accounts
        .iter()
        .any(|account| account.provider == provider && accounts.is_selected(account));
    match (has_model, authenticated) {
        (true, true) => AgentAvailability::Ready,
        (true, false) => AgentAvailability::LoginRequired,
        (false, true) => AgentAvailability::Checking,
        (false, false) => AgentAvailability::Unavailable,
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn connection_setup(&self) -> ConnectionSetup {
        let agents: Vec<_> = [
            (ProviderKind::Codex, "Codex"),
            (ProviderKind::Claude, "Claude Code"),
        ]
        .into_iter()
        .map(|(provider, name)| {
            let availability = availability(
                self.connected,
                provider,
                &self.models,
                self.account.accounts.as_deref(),
                &self.model_errors,
            );
            ConnectionAgent {
                provider,
                name: name.into(),
                availability,
                label: match availability {
                    AgentAvailability::Checking => "確認中…",
                    AgentAvailability::Ready => "利用可能",
                    AgentAvailability::LoginRequired => "未ログイン",
                    AgentAvailability::Unavailable => "設定が必要",
                }
                .into(),
            }
        })
        .collect();
        ConnectionSetup {
            can_start: agents
                .iter()
                .any(|agent| agent.availability == AgentAvailability::Ready),
            agents,
        }
    }
}

/// SSH receives a destination argument, never shell code or additional options.
pub fn validate_ssh_destination(destination: &str) -> Result<&str, &'static str> {
    let destination = destination.trim();
    if destination.is_empty()
        || destination.starts_with('-')
        || !destination
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-@:[]".contains(&byte))
    {
        return Err("SSH接続先に user@server またはSSHのホスト名を入力してください。");
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    proptest::proptest! {
        #[test]
        fn cached_catalog_and_accounts_never_prove_a_disconnected_agent_ready(
            connected in proptest::bool::ANY,
            authenticated in proptest::bool::ANY,
            catalog in proptest::bool::ANY,
            failed in proptest::bool::ANY,
            provider in proptest::sample::select(vec![ProviderKind::Codex, ProviderKind::Claude]),
        ) {
            let key = if provider == ProviderKind::Codex { "codex" } else { "claude" };
            let models: Vec<Model> = if catalog {
                serde_json::from_value(serde_json::json!([{
                    "id":"same-native-id", "model":{"provider":key,"id":"same-native-id"},
                    "displayName":"Agent", "defaultReasoningEffort":"", "supportedReasoningEfforts":[]
                }])).unwrap()
            } else { Vec::new() };
            let accounts: Accounts = serde_json::from_value(serde_json::json!({
                "accounts": if authenticated { vec![serde_json::json!({"id":"selected","provider":key})] } else { Vec::new() },
                "selectedId": if provider == ProviderKind::Codex { Some("selected") } else { None },
                "selectedClaudeId": if provider == ProviderKind::Claude { Some("selected") } else { None },
                "error":null
            })).unwrap();
            let errors = if failed { Map::from_iter([(key.into(), Value::Null)]) } else { Map::new() };
            let actual = availability(connected, provider, &models, Some(&accounts), &errors);
            proptest::prop_assert_eq!(actual == AgentAvailability::Ready, connected && authenticated && catalog && !failed);
        }

        #[test]
        fn ssh_targets_cannot_inject_options_or_shell_syntax(value in ".{0,120}") {
            if let Ok(target) = validate_ssh_destination(&value) {
                proptest::prop_assert!(!target.is_empty());
                proptest::prop_assert!(!target.starts_with('-'));
                proptest::prop_assert!(!target.chars().any(|c| c.is_whitespace() || ";|&$`\\\"'".contains(c)));
            }
        }
    }

    #[test]
    fn one_ready_provider_can_start_without_configuring_the_other() {
        let mut snapshot = Snapshot {
            connected: true,
            models: std::sync::Arc::new(serde_json::from_value(serde_json::json!([
                {"id":"gpt", "model":{"provider":"codex","id":"gpt"}, "displayName":"GPT", "defaultReasoningEffort":"", "supportedReasoningEfforts":[]},
                {"id":"sonnet", "model":{"provider":"claude","id":"sonnet"}, "displayName":"Sonnet", "defaultReasoningEffort":"", "supportedReasoningEfforts":[]}
            ])).unwrap()),
            ..Default::default()
        };
        std::sync::Arc::make_mut(&mut snapshot.account).accounts = Some(std::sync::Arc::new(
            serde_json::from_value(serde_json::json!({
                "accounts":[{"id":"native", "provider":"codex"}, {"id":"native", "provider":"claude"}],
                "selectedId":"native", "selectedClaudeId":null
            })).unwrap()
        ));
        let setup = snapshot.connection_setup();
        assert!(setup.can_start);
        assert_eq!(setup.agents[0].availability, AgentAvailability::Ready);
        assert_eq!(
            setup.agents[1].availability,
            AgentAvailability::LoginRequired
        );
        assert_eq!(setup.agents[0].label, "利用可能");
        assert_eq!(setup.agents[1].label, "未ログイン");
        snapshot.connected = false;
        let setup = snapshot.connection_setup();
        assert!(!setup.can_start);
        assert!(
            setup
                .agents
                .iter()
                .all(|agent| agent.availability == AgentAvailability::Checking)
        );
    }

    #[test]
    fn incomplete_checks_and_authentication_remain_distinct() {
        let model: Model = serde_json::from_value(serde_json::json!({
            "id":"gpt", "model":{"provider":"codex","id":"gpt"},
            "displayName":"GPT", "defaultReasoningEffort":"", "supportedReasoningEfforts":[]
        }))
        .unwrap();
        let accounts = Accounts {
            accounts: Vec::new(),
            selected_id: None,
            selected_claude_id: None,
            error: None,
        };
        assert_eq!(
            availability(
                true,
                ProviderKind::Codex,
                std::slice::from_ref(&model),
                None,
                &Map::new()
            ),
            AgentAvailability::Checking
        );
        assert_eq!(
            availability(
                true,
                ProviderKind::Codex,
                &[model],
                Some(&accounts),
                &Map::new()
            ),
            AgentAvailability::LoginRequired
        );
        assert_eq!(
            availability(true, ProviderKind::Codex, &[], Some(&accounts), &Map::new()),
            AgentAvailability::Unavailable
        );
        let authenticated = serde_json::from_value(serde_json::json!({
            "accounts":[{"id":"native", "provider":"codex"}], "selectedId":"native"
        }))
        .unwrap();
        assert_eq!(
            availability(
                true,
                ProviderKind::Codex,
                &[],
                Some(&authenticated),
                &Map::new()
            ),
            AgentAvailability::Checking
        );
    }

    #[test]
    fn ssh_accepts_destinations_and_rejects_options_and_commands() {
        assert_eq!(
            validate_ssh_destination(" user@server \n"),
            Ok("user@server")
        );
        for target in ["user@server", "dev-server", "user@[::1]"] {
            assert_eq!(validate_ssh_destination(target), Ok(target));
        }
        for target in [
            "",
            "-oProxyCommand=anything",
            "user@server; command",
            "user@$(command)",
        ] {
            assert!(validate_ssh_destination(target).is_err());
        }
    }
}

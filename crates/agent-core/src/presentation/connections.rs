//! Availability is derived from the connected Host's catalog and authentication.
use crate::{models::Model, session::ProviderInstanceId, state::Snapshot};
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
    pub instance_id: ProviderInstanceId,
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
    instance_id: &ProviderInstanceId,
    configured: &agent_protocol::providers::ProviderAvailability,
    requires_account: bool,
    models: &[Model],
    accounts: Option<&Accounts>,
    errors: &Map<String, Value>,
) -> AgentAvailability {
    if !connected || *configured == agent_protocol::providers::ProviderAvailability::Starting {
        return AgentAvailability::Checking;
    }
    if *configured != agent_protocol::providers::ProviderAvailability::Ready
        || errors.contains_key(instance_id.as_str())
    {
        return AgentAvailability::Unavailable;
    }
    if requires_account && accounts.is_none() {
        return AgentAvailability::Checking;
    }
    let has_model = models
        .iter()
        .any(|model| &model.model.instance_id == instance_id);
    let authenticated = !requires_account
        || accounts.is_some_and(|accounts| {
            accounts
                .accounts
                .iter()
                .any(|account| &account.instance_id == instance_id && accounts.is_selected(account))
        });
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
        let agents: Vec<_> = self
            .provider_instances
            .iter()
            .map(|instance| {
                let availability = availability(
                    self.connected,
                    &instance.reference.instance_id,
                    &instance.availability,
                    instance.requires_account,
                    &self.models,
                    self.account.accounts.as_deref(),
                    &self.model_errors,
                );
                ConnectionAgent {
                    instance_id: instance.reference.instance_id.clone(),
                    name: instance.display_name.clone(),
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
            provider in proptest::sample::select(vec!["codex".parse::<crate::session::ProviderInstanceId>().unwrap(), "claude".parse::<crate::session::ProviderInstanceId>().unwrap()]),
        ) {
            let key = if provider == "codex".parse::<crate::session::ProviderInstanceId>().unwrap() { "codex" } else { "claude" };
            let models: Vec<Model> = if catalog {
                serde_json::from_value(serde_json::json!([{
                    "id":"same-native-id", "model":{"instanceId":key,"id":"same-native-id"},
                    "displayName":"Agent", "defaultReasoningEffort":"", "supportedReasoningEfforts":[]
                }])).unwrap()
            } else { Vec::new() };
            let accounts: Accounts = serde_json::from_value(serde_json::json!({
                "accounts": if authenticated { vec![serde_json::json!({"id":"selected","instanceId":key})] } else { Vec::new() },
                "selected":{(key):"selected"},
                "error":null
            })).unwrap();
            let errors = if failed { Map::from_iter([(key.into(), Value::Null)]) } else { Map::new() };
            let actual = availability(connected, &provider, &agent_protocol::providers::ProviderAvailability::Ready, true, &models, Some(&accounts), &errors);
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
            provider_instances: crate::test_support::instances(),
            models: std::sync::Arc::new(serde_json::from_value(serde_json::json!([
                {"id":"gpt", "model":{"instanceId":"codex","id":"gpt"}, "displayName":"GPT", "defaultReasoningEffort":"", "supportedReasoningEfforts":[]},
                {"id":"sonnet", "model":{"instanceId":"claude","id":"sonnet"}, "displayName":"Sonnet", "defaultReasoningEffort":"", "supportedReasoningEfforts":[]}
            ])).unwrap()),
            ..Default::default()
        };
        std::sync::Arc::make_mut(&mut snapshot.account).accounts = Some(std::sync::Arc::new(
            serde_json::from_value(serde_json::json!({
                "accounts":[{"id":"native", "instanceId":"codex"}, {"id":"native", "instanceId":"claude"}],
                "selected":{"codex":"native"}
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
            "id":"gpt", "model":{"instanceId":"codex","id":"gpt"},
            "displayName":"GPT", "defaultReasoningEffort":"", "supportedReasoningEfforts":[]
        }))
        .unwrap();
        let accounts = Accounts {
            accounts: Vec::new(),
            selected: Default::default(),
            error: None,
        };
        assert_eq!(
            availability(
                true,
                &"codex"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                &agent_protocol::providers::ProviderAvailability::Ready,
                true,
                std::slice::from_ref(&model),
                None,
                &Map::new()
            ),
            AgentAvailability::Checking
        );
        assert_eq!(
            availability(
                true,
                &"codex"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                &agent_protocol::providers::ProviderAvailability::Ready,
                true,
                &[model],
                Some(&accounts),
                &Map::new()
            ),
            AgentAvailability::LoginRequired
        );
        assert_eq!(
            availability(
                true,
                &"codex"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                &agent_protocol::providers::ProviderAvailability::Ready,
                true,
                &[],
                Some(&accounts),
                &Map::new()
            ),
            AgentAvailability::Unavailable
        );
        let authenticated = serde_json::from_value(serde_json::json!({
            "accounts":[{"id":"native", "instanceId":"codex"}], "selected":{"codex":"native"}
        }))
        .unwrap();
        assert_eq!(
            availability(
                true,
                &"codex"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                &agent_protocol::providers::ProviderAvailability::Ready,
                true,
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

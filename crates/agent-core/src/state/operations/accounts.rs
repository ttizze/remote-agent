use super::*;
use crate::client::ClientExt;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAccounts {}
rpc::rpc_method!(ListAccounts, ListAccounts, |self| crate::models::Empty {});

impl Operation for ListAccounts {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Accounts)
    }
    rpc_operation!();
    const STALE_POLICY: StalePolicy = StalePolicy::Retry;
    fn apply(self, snapshot: &mut Snapshot, mut output: Self::Output) -> Vec<Effect> {
        let effects = output
            .accounts
            .iter_mut()
            .map(|account| {
                account.usage = snapshot
                    .account
                    .accounts
                    .as_ref()
                    .and_then(|previous| {
                        previous.accounts.iter().find(|old| {
                            old.instance_id == account.instance_id
                                && old.id == account.id
                                && old.email == account.email
                        })
                    })
                    .and_then(|old| old.usage.clone());
                Effect::execute(ReadAccountUsage {
                    instance_id: account.instance_id.clone(),
                    id: account.id.clone(),
                    email: account.email.clone(),
                })
            })
            .collect();
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(output));
        effects
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountUsage {
    pub instance_id: crate::session::ProviderInstanceId,
    #[serde(rename = "accountId")]
    pub id: String,
    // The native profile can change identity while a usage read is in flight.
    #[serde(skip)]
    email: Option<String>,
}

rpc::rpc_method!(ReadAccountUsage, ReadAccountUsage, |self| {
    agent_protocol::operations::ReadAccountUsage {
        instance_id: self.instance_id.clone(),
        id: self.id.clone(),
    }
});

impl Operation for ReadAccountUsage {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if let Some(accounts) = &mut Arc::make_mut(&mut snapshot.account).accounts
            && let Some(account) = Arc::make_mut(accounts).accounts.iter_mut().find(|account| {
                account.instance_id == self.instance_id
                    && account.id == self.id
                    && account.email == self.email
            })
        {
            account.usage = Some(output);
        }
        Vec::new()
    }
}

pub use agent_protocol::operations::SelectAccount;

impl Operation for SelectAccount {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Accounts)
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        apply_selection(snapshot, output);
        vec![
            Effect::execute(ListAccounts {}),
            Effect::execute(LoadModels {}),
        ]
    }
}

fn apply_selection(snapshot: &mut Snapshot, output: rpc::AccountSelection) {
    snapshot.composer_catalog = None;
    let rpc::AccountSelection {
        selected_id,
        instance_id: provider,
        persistence_error,
    } = output;
    if let Some(accounts) = &mut Arc::make_mut(&mut snapshot.account).accounts {
        let accounts = Arc::make_mut(accounts);
        accounts.selected.insert(provider, selected_id);
    }
    snapshot.error = persistence_error;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SelectAccountForDraft {
    pub instance_id: crate::session::ProviderInstanceId,
    pub id: String,
    pub thread_id: DraftKey,
}

impl Operation for SelectAccountForDraft {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Accounts)
    }
    no_input!();
    type Output = (rpc::AccountSelection, Result<rpc::ModelPage, PeerError>);

    async fn run(
        &self,
        _: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let selection = context
            .call(&SelectAccount {
                instance_id: self.instance_id.clone(),
                id: self.id.clone(),
            })
            .await?;
        Ok((selection, context.client.models().await))
    }

    fn apply(self, snapshot: &mut Snapshot, (selection, catalog): Self::Output) -> Vec<Effect> {
        let previous_provider = snapshot.model_instance_for_draft(self.thread_id.clone());
        let mut draft = snapshot
            .drafts
            .get(&self.thread_id)
            .map(|draft| (**draft).clone())
            .unwrap_or_default();
        let provider = selection.instance_id.clone();
        apply_selection(snapshot, selection);
        match catalog {
            Ok(catalog) => {
                LoadModels {}.apply(snapshot, catalog);
                if snapshot
                    .models
                    .iter()
                    .any(|model| model.model.instance_id == provider)
                {
                    if previous_provider.as_ref() != Some(&provider) {
                        draft.model = None;
                        draft.effort = None;
                        draft.service_tier = None;
                    }
                    let (model, effort, tier) = supported_settings(
                        draft.model.as_ref(),
                        draft.effort.as_deref(),
                        draft.service_tier.as_deref(),
                        Some(&provider),
                        &snapshot.models,
                        false,
                    );
                    let settings = (
                        model.cloned(),
                        effort.map(str::to_owned),
                        tier.map(str::to_owned),
                    );
                    (draft.model, draft.effort, draft.service_tier) = settings;
                    Arc::make_mut(&mut snapshot.drafts).insert(self.thread_id, Arc::new(draft));
                }
            }
            Err(error) => snapshot.error = Some(error.to_string()),
        }
        vec![Effect::execute(ListAccounts {})]
    }
}

pub use agent_protocol::operations::{StartAccountLogin, SubmitAccountLogin};

impl Operation for StartAccountLogin {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::AccountLogin {
            instance_id: self.instance_id.clone(),
        })
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, login: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = Some(Arc::new(login));
        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ReadAccountLogin {
    pub instance_id: crate::session::ProviderInstanceId,
    pub id: String,
    pub thread_id: Option<DraftKey>,
}
rpc::rpc_method!(ReadAccountLogin, ReadAccountLogin, |self| {
    agent_protocol::operations::ReadAccountLogin {
        instance_id: self.instance_id.clone(),
        id: self.id.clone(),
    }
});

impl Operation for ReadAccountLogin {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::AccountLogin {
            instance_id: self.instance_id.clone(),
        })
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, status: Self::Output) -> Vec<Effect> {
        if status.completed {
            Arc::make_mut(&mut snapshot.account).login = None;
            if let Some(id) = status.account_id {
                let intent = match self.thread_id {
                    Some(thread_id) => Intent::SelectAccountForDraft(SelectAccountForDraft {
                        instance_id: self.instance_id.clone(),
                        id,
                        thread_id,
                    }),
                    None => Intent::SelectAccount(SelectAccount {
                        instance_id: self.instance_id.clone(),
                        id,
                    }),
                };
                let (updated, effects) = reduce(snapshot, Event::Intent(intent));
                *snapshot = updated;
                return effects;
            }
            return vec![
                Effect::execute(ListAccounts {}),
                Effect::execute(LoadModels {}),
            ];
        }
        Vec::new()
    }
}

pub use agent_protocol::operations::CancelAccountLogin;

impl Operation for CancelAccountLogin {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::AccountLogin {
            instance_id: self.instance_id.clone(),
        })
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = None;
        Vec::new()
    }
}

pub use agent_protocol::operations::LogoutAccount;

impl Operation for LogoutAccount {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Accounts)
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.account).accounts = None;
        vec![
            Effect::execute(ListAccounts {}),
            Effect::execute(LoadModels {}),
        ]
    }
}

impl Operation for SubmitAccountLogin {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::AccountLogin {
            instance_id: self.instance_id.clone(),
        })
    }
    rpc_operation!();
}

#[cfg(test)]
mod account_model_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_updates_only_its_account_and_survives_list_refresh() {
        let mut snapshot = Snapshot::default();
        let accounts: rpc::Accounts = serde_json::from_value(json!({
            "accounts": [{"id":"a","instanceId":"codex","email":"a@example.invalid"}, {"id":"b","instanceId":"codex"}, {"id":"a","instanceId":"claude","email":"a@example.invalid"}],
            "selected":{"codex":"b","claude":"a"}
        })).unwrap();
        let effects = ListAccounts {}.apply(&mut snapshot, accounts.clone());
        assert_eq!(effects.len(), 3);
        let usage = rpc::AccountUsage {
            windows: vec![],
            fetched_at: 1,
            error: Some("unavailable".into()),
        };
        ReadAccountUsage {
            instance_id: "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            id: "a".into(),
            email: Some("a@example.invalid".into()),
        }
        .apply(&mut snapshot, usage.clone());
        ListAccounts {}.apply(&mut snapshot, accounts);
        let listed = snapshot.account.accounts.as_ref().unwrap();
        assert_eq!(
            listed
                .selected
                .get(
                    &"codex"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap()
                )
                .map(String::as_str),
            Some("b")
        );
        assert_eq!(listed.accounts[0].usage.as_ref(), Some(&usage));
        assert!(listed.accounts[1].usage.is_none());
        assert!(listed.accounts[2].usage.is_none());
        assert!(!listed.is_selected(&listed.accounts[0]));
        assert!(listed.is_selected(&listed.accounts[1]));
        assert!(listed.is_selected(&listed.accounts[2]));
        let mut changed = (**listed).clone();
        changed.accounts[0].email = Some("different@example.invalid".into());
        ListAccounts {}.apply(&mut snapshot, changed);
        ReadAccountUsage {
            instance_id: "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            id: "a".into(),
            email: Some("a@example.invalid".into()),
        }
        .apply(&mut snapshot, usage.clone());
        assert!(
            snapshot.account.accounts.as_ref().unwrap().accounts[0]
                .usage
                .is_none()
        );
        ListAccounts {}.apply(
            &mut snapshot,
            serde_json::from_value(json!({"accounts":[],"selected":{}})).unwrap(),
        );
        ReadAccountUsage {
            instance_id: "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            id: "a".into(),
            email: Some("a@example.invalid".into()),
        }
        .apply(&mut snapshot, usage);
        assert!(
            snapshot
                .account
                .accounts
                .as_ref()
                .unwrap()
                .accounts
                .is_empty()
        );
    }

    #[test]
    fn account_selection_preserves_supported_settings_and_normalizes_new_catalog() {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(serde_json::from_value(json!({
            "accounts":[{"id":"a","instanceId":"codex"},{"id":"b","instanceId":"codex"},{"id":"claude:c","instanceId":"claude"}],
            "selected":{"codex":"a","claude":"claude:c"}
        })).unwrap()));
        let codex: Model = serde_json::from_value(json!({"id":"gpt", "model":{"instanceId": "codex", "id": "gpt"}, "displayName":"GPT", "capabilities":{"optionDescriptors":[{"id":"reasoningEffort","label":"Reasoning","type":"select","options":[{"id":"medium","label":"medium","isDefault":true},{"id":"high","label":"high","isDefault":false}],"currentValue":"medium"},{"id":"serviceTier","label":"Service Tier","type":"select","options":[{"id":"default","label":"Standard","isDefault":true},{"id":"fast","label":"fast","isDefault":false}],"currentValue":"default"}]}})).unwrap();
        let claude: Model = serde_json::from_value(json!({"id":"claude:sonnet", "model":{"instanceId": "claude", "id": "sonnet"}, "displayName":"Sonnet", "capabilities":{"optionDescriptors":[{"id":"reasoningEffort","label":"Reasoning","type":"select","options":[{"id":"high","label":"high","isDefault":true}],"currentValue":"high"}]}})).unwrap();
        snapshot.models = Arc::new(vec![codex.clone(), claude.clone()]);
        Arc::make_mut(&mut snapshot.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    instance_id: "codex"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "gpt".into(),
                }),
                effort: Some("medium".into()),
                service_tier: Some("fast".into()),
                ..Default::default()
            }),
        );
        let select = |snapshot: &mut Snapshot,
                      id: &str,
                      provider: crate::session::ProviderInstanceId,
                      data| {
            SelectAccountForDraft {
                instance_id: provider.clone(),
                id: id.into(),
                thread_id: "draft".into(),
            }
            .apply(
                snapshot,
                (
                    rpc::AccountSelection {
                        selected_id: id.into(),
                        instance_id: provider.clone(),
                        persistence_error: None,
                    },
                    Ok(rpc::ModelPage {
                        instances: Vec::new(),
                        data,
                        next_cursor: None,
                        provider_errors: None,
                    }),
                ),
            );
        };
        select(
            &mut snapshot,
            "b",
            "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            vec![codex.clone(), claude.clone()],
        );
        assert_eq!(
            snapshot.drafts[&crate::state::DraftKey::from("draft")]
                .model
                .as_ref(),
            Some(&agent_protocol::models::ModelRef {
                instance_id: "codex"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                id: "gpt".into()
            })
        );
        assert_eq!(
            snapshot.drafts[&crate::state::DraftKey::from("draft")]
                .effort
                .as_deref(),
            Some("medium")
        );
        assert_eq!(
            snapshot.drafts[&crate::state::DraftKey::from("draft")]
                .service_tier
                .as_deref(),
            Some("fast")
        );
        select(
            &mut snapshot,
            "claude:c",
            "claude"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            vec![codex.clone(), claude.clone()],
        );
        let draft = &snapshot.drafts[&crate::state::DraftKey::from("draft")];
        assert_eq!(
            draft.model.as_ref(),
            Some(&agent_protocol::models::ModelRef {
                instance_id: "claude"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                id: "sonnet".into()
            })
        );
        assert_eq!(draft.effort.as_deref(), Some("high"));
        assert_eq!(draft.service_tier.as_deref(), Some("default"));
        assert_eq!(
            snapshot
                .account
                .accounts
                .as_ref()
                .unwrap()
                .selected
                .get(
                    &"claude"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap()
                )
                .map(String::as_str),
            Some("claude:c")
        );
        LoadModels {}.apply(
            &mut snapshot,
            rpc::ModelPage {
                instances: Vec::new(),
                data: vec![codex.clone()],
                next_cursor: None,
                provider_errors: None,
            },
        );
        assert_eq!(
            snapshot.drafts[&crate::state::DraftKey::from("draft")]
                .model
                .as_ref(),
            Some(&agent_protocol::models::ModelRef {
                instance_id: "claude"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap(),
                id: "sonnet".into()
            })
        );
        let mut restricted = codex;
        restricted
            .capabilities
            .option_descriptors
            .retain(|descriptor| descriptor.id != "serviceTier");
        if let agent_protocol::models::ModelOptionKind::Select { options, .. } =
            &mut restricted.capabilities.option_descriptors[0].kind
        {
            options.truncate(1);
        }
        select(
            &mut snapshot,
            "b",
            "codex"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            vec![restricted.clone(), claude],
        );
        assert_eq!(
            snapshot
                .account
                .accounts
                .as_ref()
                .unwrap()
                .selected
                .get(
                    &"codex"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap()
                )
                .map(String::as_str),
            Some("b")
        );
        assert_eq!(
            snapshot.models_matching(
                Some(
                    "codex"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap()
                ),
                String::new()
            ),
            vec![restricted]
        );
        assert_eq!(
            snapshot.drafts[&crate::state::DraftKey::from("draft")]
                .effort
                .as_deref(),
            Some("medium")
        );
        assert_eq!(
            snapshot.drafts[&crate::state::DraftKey::from("draft")]
                .service_tier
                .as_deref(),
            Some("default")
        );
    }
}

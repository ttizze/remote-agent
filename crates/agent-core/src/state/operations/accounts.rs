use super::*;
use crate::client::ClientExt;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAccounts {}
rpc::rpc_method!(ListAccounts, ListAccounts, |self| crate::models::Empty {});

impl Operation for ListAccounts {
    rpc_operation!();
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
                        previous
                            .accounts
                            .iter()
                            .find(|old| old.id == account.id && old.email == account.email)
                    })
                    .and_then(|old| old.usage.clone());
                Effect::execute(ReadAccountUsage {
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
    #[serde(rename = "accountId")]
    pub id: String,
    // The native profile can change identity while a usage read is in flight.
    #[serde(skip)]
    email: Option<String>,
}

rpc::rpc_method!(ReadAccountUsage, ReadAccountUsage, |self| {
    agent_protocol::operations::ReadAccountUsage {
        id: self.id.clone(),
    }
});

impl Operation for ReadAccountUsage {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, output: Self::Output) -> Vec<Effect> {
        if let Some(accounts) = &mut Arc::make_mut(&mut snapshot.account).accounts
            && let Some(account) = Arc::make_mut(accounts)
                .accounts
                .iter_mut()
                .find(|account| account.id == self.id && account.email == self.email)
        {
            account.usage = Some(output);
        }
        Vec::new()
    }
}

pub use agent_protocol::operations::SelectAccount;

impl Operation for SelectAccount {
    rpc_operation!();
    const INVALIDATES: bool = true;
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
        provider,
        persistence_error,
    } = output;
    if let Some(accounts) = &mut Arc::make_mut(&mut snapshot.account).accounts {
        let accounts = Arc::make_mut(accounts);
        match provider {
            crate::session::ProviderKind::Codex => accounts.selected_id = Some(selected_id),
            crate::session::ProviderKind::Claude => accounts.selected_claude_id = Some(selected_id),
        }
    }
    snapshot.error = persistence_error;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SelectAccountForDraft {
    pub id: String,
    pub thread_id: String,
}

impl Operation for SelectAccountForDraft {
    type Output = (rpc::AccountSelection, Result<rpc::ModelPage, PeerError>);
    const INVALIDATES: bool = true;

    async fn run(&self, context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        let selection = context
            .call(&SelectAccount {
                id: self.id.clone(),
            })
            .await?;
        Ok((selection, context.client.models().await))
    }

    fn apply(self, snapshot: &mut Snapshot, (selection, catalog): Self::Output) -> Vec<Effect> {
        let was_active =
            snapshot.account_is_active_for_draft(self.id.clone(), self.thread_id.clone());
        let mut draft = snapshot
            .drafts
            .get(&self.thread_id)
            .map(|draft| (**draft).clone())
            .unwrap_or_default();
        let provider = selection.provider;
        apply_selection(snapshot, selection);
        match catalog {
            Ok(catalog) => {
                LoadModels {}.apply(snapshot, catalog);
                let models = crate::models::provider_models(&snapshot.models, provider);
                if !models.is_empty() {
                    if !was_active {
                        draft.model = None;
                        draft.effort = None;
                        draft.service_tier = None;
                    }
                    let (model, effort, tier) = supported_settings(&draft, &models, &Map::new());
                    let settings = (
                        model.map(str::to_owned),
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
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, login: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = Some(Arc::new(login));
        Vec::new()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ReadAccountLogin {
    pub id: String,
    pub thread_id: Option<String>,
}
rpc::rpc_method!(ReadAccountLogin, ReadAccountLogin, |self| {
    agent_protocol::operations::ReadAccountLogin {
        id: self.id.clone(),
    }
});

impl Operation for ReadAccountLogin {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, status: Self::Output) -> Vec<Effect> {
        if status.completed {
            Arc::make_mut(&mut snapshot.account).login = None;
            if let Some(id) = status.account_id {
                let intent = match self.thread_id {
                    Some(thread_id) => {
                        Intent::SelectAccountForDraft(SelectAccountForDraft { id, thread_id })
                    }
                    None => Intent::SelectAccount(SelectAccount { id }),
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
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = None;
        Vec::new()
    }
}

pub use agent_protocol::operations::LogoutAccount;

impl Operation for LogoutAccount {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        Arc::make_mut(&mut snapshot.account).accounts = None;
        vec![
            Effect::execute(ListAccounts {}),
            Effect::execute(LoadModels {}),
        ]
    }
}

impl Operation for SubmitAccountLogin {
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
            "accounts": [{"id":"a","provider":"codex","email":"a@example.invalid"}, {"id":"b","provider":"codex"}],
            "selectedId":"b"
        })).unwrap();
        let effects = ListAccounts {}.apply(&mut snapshot, accounts.clone());
        assert_eq!(effects.len(), 2);
        let usage = rpc::AccountUsage {
            windows: vec![],
            fetched_at: 1,
            error: Some("unavailable".into()),
        };
        ReadAccountUsage {
            id: "a".into(),
            email: Some("a@example.invalid".into()),
        }
        .apply(&mut snapshot, usage.clone());
        ListAccounts {}.apply(&mut snapshot, accounts);
        let listed = snapshot.account.accounts.as_ref().unwrap();
        assert_eq!(listed.selected_id.as_deref(), Some("b"));
        assert_eq!(listed.accounts[0].usage.as_ref(), Some(&usage));
        assert!(listed.accounts[1].usage.is_none());
        let mut changed = (**listed).clone();
        changed.accounts[0].email = Some("different@example.invalid".into());
        ListAccounts {}.apply(&mut snapshot, changed);
        ReadAccountUsage {
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
            serde_json::from_value(json!({"accounts":[]})).unwrap(),
        );
        ReadAccountUsage {
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
    fn account_selection_owns_catalog_model_effort_and_speed() {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(serde_json::from_value(json!({
            "accounts":[{"id":"a","provider":"codex"},{"id":"b","provider":"codex"},{"id":"claude:c","provider":"claude"}],
            "selectedId":"a","selectedClaudeId":"claude:c"
        })).unwrap()));
        let codex: Model = serde_json::from_value(json!({"id":"gpt","model":"gpt","displayName":"GPT","defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],"serviceTiers":[{"id":"fast"}]})).unwrap();
        let claude: Model = serde_json::from_value(json!({"id":"claude:sonnet","model":"claude:sonnet","displayName":"Sonnet","defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}]})).unwrap();
        snapshot.models = Arc::new(vec![codex.clone(), claude.clone()]);
        Arc::make_mut(&mut snapshot.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some("gpt".into()),
                effort: Some("medium".into()),
                service_tier: Some("fast".into()),
                ..Default::default()
            }),
        );
        assert_eq!(snapshot.account_models("a".into()), vec![codex.clone()]);
        assert!(snapshot.account_models("b".into()).is_empty());
        let select = |snapshot: &mut Snapshot, id: &str, provider, data| {
            SelectAccountForDraft {
                id: id.into(),
                thread_id: "draft".into(),
            }
            .apply(
                snapshot,
                (
                    rpc::AccountSelection {
                        selected_id: id.into(),
                        provider,
                        persistence_error: None,
                    },
                    Ok(rpc::ModelPage {
                        data,
                        next_cursor: None,
                        provider_errors: None,
                    }),
                ),
            );
        };
        select(
            &mut snapshot,
            "claude:c",
            crate::session::ProviderKind::Claude,
            vec![codex.clone(), claude.clone()],
        );
        let draft = &snapshot.drafts["draft"];
        assert_eq!(draft.model.as_deref(), Some("claude:sonnet"));
        assert_eq!(draft.effort.as_deref(), Some("high"));
        assert_eq!(draft.service_tier.as_deref(), Some("default"));
        assert!(snapshot.account_is_active_for_draft("claude:c".into(), "draft".into()));
        assert!(!snapshot.account_is_active_for_draft("a".into(), "draft".into()));
        LoadModels {}.apply(
            &mut snapshot,
            rpc::ModelPage {
                data: vec![codex.clone()],
                next_cursor: None,
                provider_errors: None,
            },
        );
        assert_eq!(
            snapshot.drafts["draft"].model.as_deref(),
            Some("claude:sonnet")
        );
        assert!(!snapshot.account_is_active_for_draft("a".into(), "draft".into()));
        let mut restricted = codex;
        restricted.supported_reasoning_efforts.truncate(1);
        restricted.service_tiers = None;
        select(
            &mut snapshot,
            "b",
            crate::session::ProviderKind::Codex,
            vec![restricted.clone(), claude],
        );
        assert!(snapshot.account_models("a".into()).is_empty());
        assert_eq!(snapshot.account_models("b".into()), vec![restricted]);
        assert_eq!(snapshot.drafts["draft"].effort.as_deref(), Some("medium"));
        assert_eq!(
            snapshot.drafts["draft"].service_tier.as_deref(),
            Some("default")
        );
    }
}

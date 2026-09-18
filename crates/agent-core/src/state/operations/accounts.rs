use super::*;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListAccounts {}
rpc::rpc_method!(ListAccounts, ListAccounts, |self| crate::models::Empty {});

impl Operation for ListAccounts {
    rpc_operation!(account.accounts);
}

pub use crate::client::SelectAccount;

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
        apply_selection(snapshot, selection);
        match catalog {
            Ok(catalog) => {
                LoadModels {}.apply(snapshot, catalog);
                let models = snapshot.account_models(self.id);
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

pub use crate::client::{StartAccountLogin, SubmitAccountLogin};

impl Operation for StartAccountLogin {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, login: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = Some(Arc::new(login));
        Vec::new()
    }
}

pub use crate::client::ReadAccountLogin;

impl Operation for ReadAccountLogin {
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, status: Self::Output) -> Vec<Effect> {
        if status.completed {
            Arc::make_mut(&mut snapshot.account).login = None;
            if let Some(id) = status.account_id {
                let (updated, effects) = reduce(
                    snapshot,
                    Event::Intent(Intent::SelectAccount(SelectAccount { id })),
                );
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

pub use crate::client::CancelAccountLogin;

impl Operation for CancelAccountLogin {
    rpc_operation!();
    const INVALIDATES: bool = true;
    fn apply(self, snapshot: &mut Snapshot, _output: Self::Output) -> Vec<Effect> {
        let account = Arc::make_mut(&mut snapshot.account);
        account.login = None;
        Vec::new()
    }
}

pub use crate::client::LogoutAccount;

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

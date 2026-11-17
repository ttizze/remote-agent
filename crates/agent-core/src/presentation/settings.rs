//! Settings retain catalog facts and controls, never transcripts or draft text.
use super::{
    connections::ConnectionSetup,
    error::{ErrorNotice, ErrorScope, ErrorText},
    model_settings::{CredentialControl, ModelQuickControls, OpenRouterPicks},
    view::Presentation,
};
use crate::{
    models::{Model, ModelRef},
    session::ProviderKind,
    state::{DraftKey, Same, Snapshot, operations::OperationKey},
};
use agent_protocol::{
    operations::{AccountLogin, Accounts, CodexCliStatus, HostSoftware, UsageWindow},
    providers::ProviderSettings,
};
use std::{collections::BTreeMap, sync::Arc};

#[derive(PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct SettingsPresentation {
    language: crate::localization::UiLanguage,
    key: DraftKey,
    provider: Option<ProviderKind>,
    model: Option<ModelRef>,
    connected: bool,
    connection_enabled: bool,
    pub connection_epoch: u64,
    pub permission: super::permissions::PermissionControl,
    cwd: String,
    host: Option<String>,
    software: Option<Arc<HostSoftware>>,
    models: Arc<Vec<Model>>,
    providers: Option<Arc<ProviderSettings>>,
    pub accounts: Option<Arc<Accounts>>,
    pub login: Option<Arc<AccountLogin>>,
    pub cli: Option<Arc<CodexCliStatus>>,
    pub worktree_settings: Option<Arc<agent_protocol::models::WorktreeSettings>>,
    pub worktrees: Option<Arc<Vec<agent_protocol::models::Worktree>>>,
    custom_models: bool,
    setup: ConnectionSetup,
    credentials: Vec<CredentialControl>,
    quick: ModelQuickControls,
    choices: BTreeMap<ProviderKind, Option<ModelRef>>,
    loading: Vec<ProviderKind>,
    errors: Arc<serde_json::Map<String, serde_json::Value>>,
    credential_busy: bool,
    credential_error: Option<ErrorText>,
    login_errors: BTreeMap<ProviderKind, ErrorText>,
    provider_settings_busy: bool,
    codex_cli_busy: bool,
    notices: Vec<ErrorNotice>,
}

const PROVIDERS: [ProviderKind; 3] = [
    ProviderKind::Codex,
    ProviderKind::Claude,
    ProviderKind::DeepSeek,
];

impl SettingsPresentation {
    pub fn equivalent(&self, other: &Self) -> bool {
        std::ptr::eq(self, other) || self == other
    }
    /// The models of the providers the Host's settings leave enabled.
    fn enabled_models(&self) -> impl Iterator<Item = &Model> + Clone {
        self.models.iter().filter(|model| {
            self.providers
                .as_ref()
                .is_none_or(|settings| settings.enabled(model.model.provider))
        })
    }
    pub(super) fn project(
        snapshot: &Snapshot,
        pane: &crate::state::PaneState,
        provider: Option<ProviderKind>,
        previous: Option<&Presentation>,
    ) -> Arc<Self> {
        let input = snapshot.composer_key(
            pane.navigation.thread_id.as_ref(),
            &pane.navigation.draft_key,
        );
        let key = &input;
        if let Some(previous) = previous {
            let before = &previous.snapshot;
            let draft = |state: &Snapshot| {
                state.input.drafts.get(key).map(|draft| {
                    (
                        draft.model.clone(),
                        draft.effort.clone(),
                        draft.service_tier.clone(),
                    )
                })
            };
            if key == &previous.settings.key
                && provider == previous.settings.provider
                && snapshot.catalog.same(&before.catalog)
                && snapshot.operations.same(&before.operations)
                && snapshot.connection.connected == before.connection.connected
                && snapshot.connection.connection_disabled == before.connection.connection_disabled
                && snapshot.connection.host_name == before.connection.host_name
                && snapshot.connection.epoch == before.connection.epoch
                && pane.navigation.cwd == previous.pane().navigation.cwd
                && snapshot
                    .files
                    .workspace
                    .settings
                    .same(&before.files.workspace.settings)
                && snapshot
                    .files
                    .workspace
                    .worktrees
                    .same(&before.files.workspace.worktrees)
                && draft(snapshot) == draft(before)
            {
                return previous.settings.clone();
            }
        }
        let next = Self {
            language: snapshot.catalog.ui_language,
            key: key.clone(),
            provider,
            model: snapshot
                .input
                .drafts
                .get(key)
                .and_then(|draft| draft.model.clone()),
            connected: snapshot.connection.connected,
            connection_enabled: !snapshot.connection.connection_disabled,
            connection_epoch: snapshot.connection.epoch,
            permission: snapshot.permission_control(key),
            cwd: pane.navigation.cwd.clone(),
            host: snapshot.connection.host_name.clone(),
            software: snapshot.catalog.host_software.clone(),
            models: snapshot.catalog.models.clone(),
            providers: snapshot.catalog.provider_settings.clone(),
            accounts: snapshot.catalog.account.accounts.clone(),
            login: snapshot.catalog.account.login.clone(),
            cli: snapshot.catalog.account.codex_cli.clone(),
            worktree_settings: snapshot.files.workspace.settings.clone(),
            worktrees: snapshot.files.workspace.worktrees.clone(),
            custom_models: snapshot.allows_custom_codex_models(),
            setup: snapshot.connection_setup(),
            credentials: snapshot.credential_controls(),
            quick: snapshot.model_quick_controls(key.clone()),
            choices: PROVIDERS
                .into_iter()
                .map(|provider| (provider, snapshot.model_for_provider(key.clone(), provider)))
                .collect(),
            loading: PROVIDERS
                .into_iter()
                .filter(|provider| snapshot.models_loading(Some(*provider)))
                .collect(),
            errors: snapshot.catalog.model_errors.clone(),
            credential_busy: snapshot.operation_running(OperationKey::DshCredentials),
            credential_error: snapshot.operation_error(OperationKey::DshCredentials),
            login_errors: PROVIDERS
                .into_iter()
                .filter_map(|provider| {
                    snapshot
                        .operation_error(OperationKey::AccountLogin { provider })
                        .map(|error| (provider, error))
                })
                .collect(),
            provider_settings_busy: snapshot.operation_running(OperationKey::ProviderSettings),
            codex_cli_busy: snapshot.operation_running(OperationKey::CodexCli),
            notices: snapshot.error_notices("", ErrorScope::Settings),
        };
        if let Some(previous) = previous
            && next == *previous.settings
        {
            return previous.settings.clone();
        }
        Arc::new(next)
    }

    pub fn worktree_is_current(&self, path: &str) -> bool {
        std::path::Path::new(&self.cwd).starts_with(path)
    }

    pub fn custom_model_choice(
        &self,
        provider: Option<ProviderKind>,
        query: &str,
    ) -> Option<ModelRef> {
        let id = query.trim();
        (self.custom_models
            && provider == Some(ProviderKind::Codex)
            && !id.is_empty()
            && !id.chars().any(char::is_whitespace)
            && !self
                .models
                .iter()
                .any(|model| model.model.provider == ProviderKind::Codex && model.model.id == id))
        .then(|| ModelRef {
            provider: ProviderKind::Codex,
            id: id.into(),
        })
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl SettingsPresentation {
    pub fn draft_key(&self) -> DraftKey {
        self.key.clone()
    }
    pub fn provider(&self) -> Option<ProviderKind> {
        self.provider
    }
    pub fn selected_model(&self) -> Option<ModelRef> {
        self.model.clone()
    }
    pub fn current_model(&self) -> Option<Model> {
        super::model_settings::current_model(&self.models, self.model.as_ref()).cloned()
    }
    pub fn selected_model_name(&self) -> Option<String> {
        self.model.as_ref().map(|selected| {
            super::model_settings::current_model(&self.models, Some(selected))
                .map_or_else(|| selected.id.clone(), |model| model.display_name.clone())
        })
    }
    pub fn connected(&self) -> bool {
        self.connected
    }
    pub fn connection_enabled(&self) -> bool {
        self.connection_enabled
    }
    pub fn connection_status_label(&self) -> String {
        super::connections::connection_status_label(
            self.language,
            self.connected,
            self.connection_enabled,
        )
    }
    pub fn account_login_error(&self, provider: ProviderKind) -> Option<ErrorText> {
        self.login_errors.get(&provider).cloned()
    }
    pub fn host_name(&self) -> Option<String> {
        self.host.clone()
    }
    pub fn host_software(&self) -> Option<HostSoftware> {
        self.software.as_deref().cloned()
    }
    pub fn accounts(&self) -> Option<Accounts> {
        self.accounts.as_deref().cloned()
    }
    pub fn account_login(&self) -> Option<AccountLogin> {
        self.login.as_deref().cloned()
    }
    pub fn codex_cli_status(&self) -> Option<CodexCliStatus> {
        self.cli.as_deref().cloned()
    }
    pub fn connection_setup(&self) -> ConnectionSetup {
        self.setup.clone()
    }
    pub fn credential_controls(&self) -> Vec<CredentialControl> {
        self.credentials.clone()
    }
    pub fn quick_controls(&self) -> ModelQuickControls {
        self.quick.clone()
    }
    pub fn notices(&self) -> Vec<ErrorNotice> {
        self.notices.clone()
    }
    pub fn provider_settings_loaded(&self) -> bool {
        self.providers.is_some()
    }
    pub fn credential_busy(&self) -> bool {
        self.credential_busy
    }
    pub fn credential_error(&self) -> Option<ErrorText> {
        self.credential_error.clone()
    }
    pub fn provider_settings_busy(&self) -> bool {
        self.provider_settings_busy
    }
    pub fn codex_cli_busy(&self) -> bool {
        self.codex_cli_busy
    }
    pub fn models_loading(&self, provider: Option<ProviderKind>) -> bool {
        provider.map_or(!self.loading.is_empty(), |provider| {
            self.loading.contains(&provider)
        })
    }
    pub fn models_matching(&self, provider: Option<ProviderKind>, query: String) -> Vec<Model> {
        super::model_settings::matching_models(self.enabled_models(), provider, &query)
    }
    pub fn openrouter_picks(&self, provider: Option<ProviderKind>) -> Option<OpenRouterPicks> {
        super::model_settings::openrouter_picks(
            provider,
            self.enabled_models(),
            self.model.as_ref(),
        )
    }
    pub fn model_for_provider(&self, provider: ProviderKind) -> Option<ModelRef> {
        self.choices.get(&provider).cloned().flatten()
    }
    pub fn model_error_messages(&self, provider: Option<ProviderKind>) -> Vec<String> {
        super::model_settings::error_messages(self.language, &self.errors, provider)
    }
    pub fn account_is_selected(&self, provider: ProviderKind, id: String) -> bool {
        self.accounts.as_ref().is_some_and(|accounts| {
            accounts.accounts.iter().any(|account| {
                account.provider == provider && account.id == id && accounts.is_selected(account)
            })
        })
    }
    pub fn account_weekly_usage(&self, provider: ProviderKind, id: String) -> Vec<UsageWindow> {
        super::model_settings::weekly_usage(self.accounts.as_deref(), provider, &id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{
        Draft,
        operations::{OperationPhase, OperationState},
    };

    fn snapshot() -> Snapshot {
        let mut state = Snapshot::default();
        state.connection.connected = true;
        state.catalog.models = Arc::new(
            serde_json::from_value(serde_json::json!([
                {"id":"a", "model":{"provider":"codex","id":"a"}, "displayName":"Alpha"},
                {"id":"b", "model":{"provider":"codex","id":"b"}, "displayName":"Beta"}
            ]))
            .unwrap(),
        );
        state
    }

    #[test]
    fn workspace_and_connection_inputs_invalidate_settings_independently() {
        let changes = [
            (
                "settings",
                (|state: &mut Snapshot| {
                    Arc::make_mut(&mut state.files.workspace).settings = Some(Arc::default());
                }) as fn(&mut Snapshot),
            ),
            ("worktrees", |state| {
                Arc::make_mut(&mut state.files.workspace).worktrees = Some(Arc::new(Vec::new()));
            }),
            ("directory", |state| {
                Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).cwd =
                    "/repo/task/src".into();
            }),
            ("reconnection", |state| state.connection.epoch += 1),
            ("permissions", |state| {
                Arc::make_mut(&mut state.catalog.permission_settings).insert(
                    ProviderKind::Codex,
                    Arc::new(crate::state::operations::PermissionSettingsState { result: None }),
                );
            }),
        ];
        for (name, change) in changes {
            let mut state = snapshot();
            Arc::make_mut(&mut state.input.drafts).insert(
                state.panes.primary().navigation.draft_key.clone(),
                Arc::new(Draft {
                    model: Some(state.catalog.models[0].model.clone()),
                    ..Default::default()
                }),
            );
            let initial = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
            change(&mut state);
            let changed = Presentation::current(Arc::new(state.clone()), &initial).unwrap();
            assert!(!Arc::ptr_eq(&initial.settings, &changed.settings), "{name}");
            assert!(changed.changes(initial).settings, "{name}");
            assert_eq!(
                changed.settings.worktree_settings,
                state.files.workspace.settings
            );
            assert_eq!(changed.settings.worktrees, state.files.workspace.worktrees);
            assert_eq!(changed.settings.connection_epoch, state.connection.epoch);
            assert_eq!(changed.settings.permission.loading, name == "permissions");
            assert_eq!(
                changed.settings.permission.load_provider,
                (name != "permissions").then_some(ProviderKind::Codex),
            );
            assert_eq!(
                changed.settings.permission.provider,
                Some(ProviderKind::Codex)
            );
            assert_eq!(
                changed.settings.worktree_is_current("/repo/task"),
                name == "directory"
            );
            assert!(!changed.settings.worktree_is_current("/repo/tasks"));
            let stable = Presentation::project(Arc::new(state), "", Some(&changed)).unwrap();
            assert!(Arc::ptr_eq(&stable.settings, &changed.settings), "{name}");
        }
    }

    #[test]
    fn custom_model_choices_follow_the_presented_account() {
        let mut state = snapshot();
        let initial = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        assert_eq!(
            initial
                .settings
                .custom_model_choice(Some(ProviderKind::Codex), " custom ")
                .unwrap()
                .id,
            "custom"
        );
        for query in ["a", "", "two words"] {
            assert!(
                initial
                    .settings
                    .custom_model_choice(Some(ProviderKind::Codex), query)
                    .is_none()
            );
        }
        assert!(
            initial
                .settings
                .custom_model_choice(Some(ProviderKind::Claude), "custom")
                .is_none()
        );
        Arc::make_mut(&mut state.catalog.account).accounts = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
                "accounts":[{"provider":"codex", "id":"subscription", "planType":"Pro"}],
                "selected":{"codex":"subscription"}
            }))
            .unwrap(),
        ));
        let selected = Presentation::current(Arc::new(state), &initial).unwrap();
        assert!(
            selected
                .settings
                .custom_model_choice(Some(ProviderKind::Codex), "custom")
                .is_none()
        );
    }

    #[test]
    fn selected_model_names_keep_custom_ids_when_the_catalog_has_no_display_name() {
        let mut state = snapshot();
        let name = |state: &Snapshot| {
            Presentation::project(Arc::new(state.clone()), "", None)
                .unwrap()
                .settings
                .selected_model_name()
        };
        assert!(name(&state).is_none());
        for (id, expected) in [("a", "Alpha"), ("custom-model", "custom-model")] {
            Arc::make_mut(&mut state.input.drafts).insert(
                state.panes.primary().navigation.draft_key.clone(),
                Arc::new(Draft {
                    model: Some(ModelRef {
                        provider: ProviderKind::Codex,
                        id: id.into(),
                    }),
                    ..Default::default()
                }),
            );
            assert_eq!(name(&state).as_deref(), Some(expected));
        }
    }

    #[test]
    fn typing_streaming_and_unrelated_operations_keep_settings_identity() {
        let mut state = snapshot();
        let initial = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        let independent = Presentation::project(initial.snapshot.clone(), "", None).unwrap();
        assert!(!Arc::ptr_eq(&independent.settings, &initial.settings));
        assert!(!independent.changes(initial.clone()).settings);
        Arc::make_mut(&mut state.input.drafts).insert(
            state.panes.primary().navigation.draft_key.clone(),
            Arc::new(Draft {
                text: "typed".into(),
                ..Default::default()
            }),
        );
        Arc::make_mut(&mut state.sessions.conversations).insert(
            crate::session::SessionRef::new(ProviderKind::Codex, "stream".into()).unwrap(),
            Arc::new(crate::models::Thread {
                preview: Some("streaming text".into()),
                ..Default::default()
            }),
        );
        let typed = Presentation::project(Arc::new(state.clone()), "", Some(&initial)).unwrap();
        assert!(Arc::ptr_eq(&initial.settings, &typed.settings));
        Arc::make_mut(&mut state.operations).insert(
            OperationKey::WorkspaceReview {
                pane: ("").to_owned(),
            },
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        let reviewing = Presentation::current(Arc::new(state.clone()), &typed).unwrap();
        assert!(Arc::ptr_eq(&typed.settings, &reviewing.settings));
        Arc::make_mut(&mut state.operations)
            .get_mut(&OperationKey::WorkspaceReview {
                pane: ("").to_owned(),
            })
            .unwrap()
            .phase = OperationPhase::Failed {
            message: "review failed".into(),
        };
        let failed = Presentation::current(Arc::new(state), &reviewing).unwrap();
        assert!(Arc::ptr_eq(&reviewing.settings, &failed.settings));
        assert!(
            failed
                .context
                .workspace_error()
                .unwrap()
                .to_string()
                .contains("review failed")
        );
    }

    #[test]
    fn model_and_account_choices_acknowledge_before_transcript_projection() {
        let mut state = snapshot();
        let first = state.panes.primary().navigation.draft_key.clone();
        Arc::make_mut(&mut state.input.drafts).insert(
            first.clone(),
            Arc::new(Draft {
                model: Some(ModelRef {
                    provider: ProviderKind::Codex,
                    id: "a".into(),
                }),
                ..Default::default()
            }),
        );
        let initial = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        let model = ModelRef {
            provider: ProviderKind::Codex,
            id: "b".into(),
        };
        Arc::make_mut(&mut state.input.drafts).insert(
            first.clone(),
            Arc::new(Draft {
                model: Some(model.clone()),
                ..Default::default()
            }),
        );
        let key = DraftKey::from("second");
        let draft = state.input.drafts[&first].clone();
        Arc::make_mut(&mut state.input.drafts).insert(key.clone(), draft);
        let selected = Presentation::current(Arc::new(state.clone()), &initial).unwrap();
        assert!(selected.changes(initial).settings);
        assert_eq!(selected.settings.selected_model(), Some(model.clone()));
        assert_eq!(
            selected.settings.current_model().unwrap().display_name,
            "Beta"
        );
        Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation).draft_key =
            key.clone();
        let navigated = Presentation::current(Arc::new(state.clone()), &selected).unwrap();
        assert!(navigated.changes(selected).settings);
        assert_eq!(navigated.settings.draft_key(), key);
        Arc::make_mut(&mut state.catalog.account).accounts = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
                "accounts":[{"provider":"codex", "id":"chosen", "email":"fixture@example.invalid"}],
                "selected":{"codex":"chosen"}
            }))
            .unwrap(),
        ));
        let current = Presentation::current(Arc::new(state.clone()), &navigated).unwrap();
        assert!(current.changes(navigated).settings);
        assert_eq!(current.settings.draft_key(), key);
        assert_eq!(current.settings.provider(), Some(ProviderKind::Codex));
        assert_eq!(
            current.settings.model_for_provider(ProviderKind::Codex),
            Some(model)
        );
        assert_eq!(
            current.settings.models_matching(None, " BETA ".into())[0].display_name,
            "Beta"
        );
        assert!(
            current
                .settings
                .models_matching(Some(ProviderKind::Claude), String::new())
                .is_empty()
        );
        assert!(
            current
                .settings
                .account_is_selected(ProviderKind::Codex, "chosen".into())
        );
        assert!(
            !current
                .settings
                .account_is_selected(ProviderKind::Codex, "missing".into())
        );
        Arc::make_mut(&mut state.catalog.model_errors)
            .insert("codex".into(), serde_json::json!({"message":"unavailable"}));
        let failed = Presentation::current(Arc::new(state), &current).unwrap();
        assert_eq!(
            failed
                .settings
                .model_error_messages(Some(ProviderKind::Codex)),
            ["エラーが発生しました。\ncodex: unavailable"]
        );
    }

    #[test]
    fn active_provider_and_host_facts_update_without_a_draft_or_catalog_change() {
        let mut state = snapshot();
        let id = crate::session::SessionRef::new(ProviderKind::Codex, "active".into()).unwrap();
        let nav = Arc::make_mut(&mut Arc::make_mut(state.panes.primary_mut()).navigation);
        nav.thread_id = Some(id.clone());
        nav.draft_key = id.clone().into();
        Arc::make_mut(&mut state.sessions.conversations).insert(
            id.clone(),
            Arc::new(crate::models::Thread {
                id: Some(id.clone()),
                active_provider: Some(ProviderKind::Codex),
                ..Default::default()
            }),
        );
        let initial = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        assert_eq!(initial.settings.provider(), Some(ProviderKind::Codex));
        Arc::make_mut(
            Arc::make_mut(&mut state.sessions.conversations)
                .get_mut(&id)
                .unwrap(),
        )
        .active_provider = Some(ProviderKind::Claude);
        let switched = Presentation::current(Arc::new(state.clone()), &initial).unwrap();
        assert_eq!(switched.settings.provider(), Some(ProviderKind::Claude));
        state.connection.host_name = Some("Another PC".into());
        let named = Presentation::current(Arc::new(state.clone()), &switched).unwrap();
        assert_eq!(named.settings.host_name().as_deref(), Some("Another PC"));
        state.connection.connection_disabled = true;
        let disabled = Presentation::current(Arc::new(state.clone()), &named).unwrap();
        assert!(!disabled.settings.connection_enabled());
        state.connection.connected = false;
        let disconnected = Presentation::current(Arc::new(state), &disabled).unwrap();
        assert!(!disconnected.settings.connected());
    }

    #[test]
    fn settings_controls_publish_loading_failure_and_retry_but_ignore_generations() {
        let mut state = snapshot();
        let mut previous = Presentation::project(Arc::new(state.clone()), "", None).unwrap();
        for phase in [
            OperationPhase::Running,
            OperationPhase::Failed {
                message: "credentials failed".into(),
            },
            OperationPhase::Running,
        ] {
            Arc::make_mut(&mut state.operations).insert(
                OperationKey::DshCredentials,
                OperationState {
                    generation: 1,
                    phase: phase.clone(),
                },
            );
            let next = Presentation::current(Arc::new(state.clone()), &previous).unwrap();
            assert!(next.changes(previous).settings);
            assert_eq!(
                next.settings.credential_busy(),
                phase == OperationPhase::Running
            );
            assert_eq!(
                next.settings.credential_error().is_some(),
                matches!(phase, OperationPhase::Failed { .. })
            );
            previous = next;
        }
        Arc::make_mut(&mut state.operations)
            .get_mut(&OperationKey::DshCredentials)
            .unwrap()
            .generation += 1;
        let next = Presentation::current(Arc::new(state.clone()), &previous).unwrap();
        assert!(Arc::ptr_eq(&next.settings, &previous.settings));
        Arc::make_mut(&mut state.operations).insert(
            OperationKey::Models {
                provider: ProviderKind::Codex,
            },
            OperationState {
                generation: 2,
                phase: OperationPhase::Running,
            },
        );
        let loading = Presentation::current(Arc::new(state), &next).unwrap();
        assert!(loading.settings.models_loading(Some(ProviderKind::Codex)));
        assert!(!loading.settings.models_loading(Some(ProviderKind::Claude)));
    }
}

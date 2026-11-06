//! Model-picker and quick-control decisions shared by native clients.
use crate::{
    models::{Model, ModelRef},
    session::ProviderKind,
    state::Snapshot,
};
use agent_protocol::operations::UsageWindow;

pub(crate) fn current_model<'a>(
    models: &'a [Model],
    selected: Option<&ModelRef>,
) -> Option<&'a Model> {
    models.iter().find(|model| Some(&model.model) == selected)
}

pub(crate) fn error_messages(
    errors: &serde_json::Map<String, serde_json::Value>,
    provider: Option<ProviderKind>,
) -> Vec<String> {
    let provider = provider.map(ProviderKind::key);
    errors
        .iter()
        .filter(|(key, _)| provider.is_none_or(|provider| provider == key.as_str()))
        .map(|(provider, error)| {
            let message = error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            format!("{provider}: {message}")
        })
        .collect()
}

pub(crate) fn matching_models<'a>(
    models: impl Iterator<Item = &'a Model>,
    provider: Option<ProviderKind>,
    query: &str,
) -> Vec<Model> {
    let query = query.trim().to_lowercase();
    models
        .filter(|model| provider.is_none_or(|provider| model.model.provider == provider))
        .filter(|model| {
            query.is_empty()
                || model.display_name.to_lowercase().contains(&query)
                || model.model.id.to_lowercase().contains(&query)
        })
        .cloned()
        .collect()
}

pub(crate) fn weekly_usage(
    accounts: Option<&agent_protocol::operations::Accounts>,
    provider: ProviderKind,
    id: &str,
) -> Vec<UsageWindow> {
    accounts
        .and_then(|accounts| {
            accounts
                .accounts
                .iter()
                .find(|account| account.provider == provider && account.id == id)
        })
        .and_then(|account| account.usage.as_ref())
        .filter(|usage| usage.error.is_none())
        .map(|usage| {
            usage
                .windows
                .iter()
                .filter(|window| window.label.contains("週間枠"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

impl Snapshot {
    pub(crate) fn draft_provider(
        &self,
        key: &crate::state::DraftKey,
        model: Option<&ModelRef>,
    ) -> Option<ProviderKind> {
        model.map(|model| model.provider).or_else(|| match key {
            crate::state::DraftKey::Session { session } => Some(
                self.thread_metadata(session)
                    .and_then(|thread| thread.active_provider)
                    .unwrap_or(session.provider),
            ),
            crate::state::DraftKey::Local { .. } => None,
        })
    }
}

pub(crate) fn automatic_provider(
    models: &[Model],
    selected_accounts: Option<&std::collections::HashMap<ProviderKind, String>>,
    errors: &serde_json::Map<String, serde_json::Value>,
) -> Option<ProviderKind> {
    let selected_accounts = selected_accounts?;
    let mut available = models.iter().filter(|model| {
        let provider = model.model.provider;
        !errors.contains_key(provider.key()) && selected_accounts.contains_key(&provider)
    });
    available
        .clone()
        .find(|model| model.is_default == Some(true))
        .or_else(|| available.next())
        .map(|model| model.model.provider)
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelQuickControls {
    pub efforts: Vec<String>,
    pub effort: String,
    pub effort_level: u32,
    pub fast: bool,
    pub toggle_fast_to: Option<String>,
    pub speed_choices: Vec<ModelSpeedChoice>,
}
#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelSpeedChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub selected: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn models_loading(&self, provider: Option<ProviderKind>) -> bool {
        use crate::state::operations::{OperationKey, OperationPhase};
        self.operations.iter().any(|(key, state)| {
            matches!(key, OperationKey::Models { provider: owner } if provider.is_none_or(|provider| provider == *owner))
                && state.phase == OperationPhase::Running
        })
    }

    pub fn model_provider_for_draft(
        &self,
        thread_id: crate::state::DraftKey,
    ) -> Option<ProviderKind> {
        self.draft_provider(
            &thread_id,
            self.input
                .drafts
                .get(&thread_id)
                .and_then(|draft| draft.model.as_ref()),
        )
        .or_else(|| {
            automatic_provider(
                &self.catalog.models,
                self.catalog
                    .account
                    .accounts
                    .as_ref()
                    .map(|accounts| &accounts.selected),
                &self.catalog.model_errors,
            )
        })
    }

    pub fn models_matching(&self, provider: Option<ProviderKind>, query: String) -> Vec<Model> {
        matching_models(
            self.catalog
                .models
                .iter()
                .filter(|model| self.provider_enabled(model.model.provider)),
            provider,
            &query,
        )
    }

    pub(crate) fn allows_custom_codex_models(&self) -> bool {
        self.provider_enabled(ProviderKind::Codex)
            && crate::state::allows_custom_codex_models(self.catalog.account.accounts.as_deref())
    }

    /// A recommended picker catalog is not an exhaustive allowlist of Codex IDs.
    pub fn custom_model_choice(
        &self,
        provider: Option<ProviderKind>,
        query: &str,
    ) -> Option<ModelRef> {
        let id = query.trim();
        (self.allows_custom_codex_models()
            && provider == Some(ProviderKind::Codex)
            && !id.is_empty()
            && !id.chars().any(char::is_whitespace)
            && !self
                .catalog
                .models
                .iter()
                .any(|model| model.model.provider == ProviderKind::Codex && model.model.id == id))
        .then(|| ModelRef {
            provider: ProviderKind::Codex,
            id: id.into(),
        })
    }

    pub fn model_for_provider(
        &self,
        thread_id: crate::state::DraftKey,
        provider: ProviderKind,
    ) -> Option<ModelRef> {
        let saved = self
            .input
            .drafts
            .get(&thread_id)
            .and_then(|draft| draft.model.as_ref());
        let remembered = self.catalog.model_preferences.selections.get(&provider);
        let preferred = saved
            .filter(|model| model.provider == provider)
            .or_else(|| remembered.map(|settings| &settings.model));
        let (model, _, _) = crate::state::supported_settings(
            preferred,
            None,
            None,
            Some(provider),
            &self.catalog.models,
            &self.catalog.model_errors,
            self.allows_custom_codex_models(),
        );
        model.cloned()
    }

    /// Keep every weekly bucket (including model-specific limits); never turn
    /// an unavailable quota into a full or empty bar. Labels are Host-normalized.
    pub fn account_weekly_usage(&self, provider: ProviderKind, id: String) -> Vec<UsageWindow> {
        weekly_usage(self.catalog.account.accounts.as_deref(), provider, &id)
    }

    pub fn model_quick_controls(&self, thread_id: crate::state::DraftKey) -> ModelQuickControls {
        let Some(draft) = self.input.drafts.get(&thread_id) else {
            return ModelQuickControls::default();
        };
        quick_controls(
            self.catalog
                .models
                .iter()
                .find(|model| Some(&model.model) == draft.model.as_ref()),
            draft.effort.as_deref(),
            draft.service_tier.as_deref(),
        )
    }
}

fn quick_controls(
    model: Option<&Model>,
    effort: Option<&str>,
    service_tier: Option<&str>,
) -> ModelQuickControls {
    let Some(model) = model.and_then(|model| model.settings.as_ref()) else {
        return ModelQuickControls::default();
    };
    let efforts: Vec<_> = model
        .supported_reasoning_efforts
        .iter()
        .map(|choice| choice.reasoning_effort.clone())
        .collect();
    let (effort, tier) = crate::state::supported_model_options(model, effort, service_tier);
    let effort = effort.unwrap_or_default().to_owned();
    let effort_level = efforts
        .iter()
        .position(|value| *value == effort)
        .map_or(0, |index| index as u32 + 1);
    let fast_tier = model
        .service_tiers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|tier| tier.fast);
    let fast = model
        .service_tiers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|speed| speed.fast && speed.id == tier);
    let mut speed_choices = vec![ModelSpeedChoice {
        id: "default".into(),
        label: "通常".into(),
        description: None,
        selected: tier == "default",
    }];
    speed_choices.extend(
        model
            .service_tiers
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|speed| speed.id != "default")
            .map(|speed| ModelSpeedChoice {
                id: speed.id.clone(),
                label: speed.name.clone().unwrap_or_else(|| speed.id.clone()),
                description: speed.description.clone(),
                selected: speed.id == tier,
            }),
    );
    ModelQuickControls {
        efforts,
        effort,
        effort_level,
        fast,
        speed_choices,
        toggle_fast_to: fast_tier.map(|tier| {
            if fast {
                "default".into()
            } else {
                tier.id.clone()
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Draft;
    use std::sync::Arc;

    #[rstest::rstest]
    #[case(ProviderKind::Codex, "shared", vec![("週間枠", 86)])]
    #[case(ProviderKind::Claude, "shared", vec![("Fable · 週間枠", 66), ("Sonnet · 週間枠", 42)])]
    #[case(ProviderKind::Codex, "missing", vec![])]
    fn weekly_limits_keep_the_account_provider_and_model_scope(
        #[case] provider: ProviderKind,
        #[case] id: &str,
        #[case] expected: Vec<(&str, u32)>,
    ) {
        let mut accounts: agent_protocol::operations::Accounts =
            serde_json::from_value(serde_json::json!({
                "accounts":[
                    {"id":"shared","provider":"codex","usage":{"fetchedAt":1,"windows":[
                        {"label":"5時間枠","remainingPercent":72},
                        {"label":"週間枠","remainingPercent":86}
                    ]}},
                    {"id":"shared","provider":"claude","usage":{"fetchedAt":1,"windows":[
                        {"label":"Fable · 5時間枠","remainingPercent":50},
                        {"label":"Fable · 週間枠","remainingPercent":66},
                        {"label":"Sonnet · 週間枠","remainingPercent":42}
                    ]}}
                ],"selected":{}
            }))
            .unwrap();
        let windows = weekly_usage(Some(&accounts), provider, id);
        assert_eq!(
            windows
                .iter()
                .map(|window| (window.label.as_str(), window.remaining_percent))
                .collect::<Vec<_>>(),
            expected
        );
        for account in &mut accounts.accounts {
            account.usage.as_mut().unwrap().error = Some("unavailable".into());
        }
        assert!(weekly_usage(Some(&accounts), provider, id).is_empty());
        assert!(weekly_usage(None, provider, id).is_empty());
    }

    #[test]
    fn speed_choices_preserve_native_multipliers_and_select_each_tier() {
        let model: Model = serde_json::from_value(serde_json::json!({
            "id":"model","model":{"provider":"codex","id":"model"},"displayName":"Model", "settings": {
            "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}],
            "serviceTiers":[{"id":"priority","name":"Fast","description":"2x speed, increased usage","fast":true},
                {"id":"ultrafast","name":"Ultrafast","description":"Fastest processing","fast":true}]
        }})).unwrap();
        for id in ["default", "priority", "ultrafast"] {
            let controls = quick_controls(Some(&model), Some("low"), Some(id));
            assert_eq!(
                controls
                    .speed_choices
                    .iter()
                    .filter(|choice| choice.selected)
                    .count(),
                1
            );
            assert_eq!(
                controls
                    .speed_choices
                    .iter()
                    .find(|choice| choice.selected)
                    .unwrap()
                    .id,
                id
            );
            assert_eq!(controls.fast, id != "default");
            assert_eq!(
                controls.speed_choices[1].description.as_deref(),
                Some("2x speed, increased usage")
            );
        }
    }

    #[test]
    fn automatic_provider_uses_catalog_defaults_and_speed_uses_metadata() {
        use serde_json::json;
        let mut models: Vec<Model> = serde_json::from_value(json!([
            {"id":"first","model":{"provider":"codex","id":"first"},"displayName":"First","isDefault":false, "settings": {
             "defaultReasoningEffort":"","supportedReasoningEfforts":[]}},
            {"id":"preferred","model":{"provider":"claude","id":"preferred"},"displayName":"Preferred","isDefault":true, "settings": {
             "defaultReasoningEffort":"","supportedReasoningEfforts":[],
             "serviceTiers":[{"id":"express","fast":true},{"id":"priority","fast":false}]}}
        ])).unwrap();
        let errors = serde_json::Map::new();
        let selected = [
            (ProviderKind::Codex, "codex-account".into()),
            (ProviderKind::Claude, "claude-account".into()),
        ]
        .into();
        assert_eq!(automatic_provider(&models, None, &errors), None);
        assert_eq!(
            automatic_provider(&models, Some(&selected), &errors),
            Some(ProviderKind::Claude)
        );
        let controls = quick_controls(Some(&models[1]), None, Some("express"));
        assert!(controls.fast);
        assert_eq!(controls.toggle_fast_to.as_deref(), Some("default"));
        let controls = quick_controls(Some(&models[1]), None, Some("priority"));
        assert!(!controls.fast);
        assert_eq!(controls.toggle_fast_to.as_deref(), Some("express"));
        models[1].is_default = Some(false);
        assert_eq!(
            automatic_provider(&models, Some(&selected), &errors),
            Some(ProviderKind::Codex)
        );
        assert_eq!(
            automatic_provider(&models, Some(&std::collections::HashMap::new()), &errors),
            None
        );
    }

    proptest::proptest! {
        #[test]
        fn automatic_new_chats_use_the_authenticated_available_provider(
            provider in proptest::sample::select(vec![ProviderKind::Codex, ProviderKind::Claude]),
            failed in proptest::bool::ANY,
            reversed in proptest::bool::ANY,
        ) {
            use crate::state::{Event, Intent, reduce};
            use serde_json::json;
            let other = if provider == ProviderKind::Codex { ProviderKind::Claude } else { ProviderKind::Codex };
            let mut models: Vec<Model> = serde_json::from_value(json!([
                {"id":"other", "model":{"provider":other,"id":"other"},"isDefault":true,"displayName":"Other", "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}},
                {"id":"available", "model":{"provider":provider,"id":"available"},"isDefault":true,"displayName":"Available", "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}}
            ])).unwrap();
            if reversed { models.reverse(); }
            let selected = [(provider, "account".into())].into();
            let errors = if failed { serde_json::Map::from_iter([(other.key().into(), json!({"message":"offline"}))]) } else { serde_json::Map::new() };
            proptest::prop_assert_eq!(automatic_provider(&models, Some(&selected), &errors), Some(provider));
            let snapshot = Snapshot {
                catalog: crate::state::Catalog {
                    models: Arc::new(models),
                    model_errors: Arc::new(errors),
                    account: Arc::new(crate::state::AccountState {
                        accounts: Some(Arc::new(agent_protocol::operations::Accounts {
                            accounts: Vec::new(), selected, errors: Default::default(),
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let (next, _) = reduce(&snapshot, Event::Intent(Intent::NewChat { cwd: "/project".into() }));
            let key = crate::state::DraftKey::from("new:/project");
            proptest::prop_assert_eq!(next.model_provider_for_draft(key.clone()), Some(provider));
            proptest::prop_assert_eq!(next.input.drafts[&key].model.as_ref().unwrap().provider, provider);
            proptest::prop_assert!(snapshot.input.drafts.is_empty());
        }
    }

    #[test]
    fn absent_or_failed_providers_preserve_unsent_input() {
        use crate::state::{Event, Intent, reduce};
        use serde_json::json;
        for catalog in [
            json!([]),
            json!([{"id":"failed","model":{"provider":"claude","id":"failed"},"displayName":"Failed", "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}}]),
        ] {
            let snapshot = Snapshot {
                catalog: crate::state::Catalog {
                    models: Arc::new(serde_json::from_value(catalog).unwrap()),
                    model_errors: Arc::new(serde_json::Map::from_iter([(
                        "claude".into(),
                        json!({"message":"offline"}),
                    )])),
                    ..Default::default()
                },
                ..Default::default()
            };
            let (snapshot, _) = reduce(
                &snapshot,
                Event::Intent(Intent::NewChat {
                    cwd: "/project".into(),
                }),
            );
            let key = snapshot.pane.navigation.draft_key.clone();
            let (snapshot, _) = reduce(
                &snapshot,
                Event::Intent(Intent::SetDraftText {
                    thread_id: key.clone(),
                    text: "keep this input".into(),
                }),
            );
            assert_eq!(snapshot.model_provider_for_draft(key.clone()), None);
            assert!(snapshot.permission_control(&key).load_request.is_none());
            let (next, effects) = reduce(
                &snapshot,
                Event::Intent(Intent::Submit {
                    thread_id: None,
                    client_user_message_id: "input".into(),
                }),
            );
            assert!(effects.is_empty());
            assert!(next.sessions.pending_submissions.is_empty());
            assert_eq!(next.input.drafts[&key].text, "keep this input");
            assert!(next.pane.error.is_none());
            assert!(
                next.operation_error(crate::state::operations::OperationKey::Submission {
                    draft_key: key
                })
                .is_some()
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn identical_native_model_ids_keep_provider_choices_and_controls_separate(suffix in "[a-zA-Z0-9:_-]{1,40}") {
            let id = format!("claude:{suffix}");
            let mut snapshot = Snapshot {
                catalog: crate::state::Catalog {
                    models: Arc::new(serde_json::from_value(serde_json::json!([
                        {"id":id,"model":{"provider":"codex","id":id},"displayName":"Codex","isDefault":true, "settings": {
                         "defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}]}},
                        {"id":id,"model":{"provider":"claude","id":id},"displayName":"Claude","isDefault":true, "settings": {
                         "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]}}
                    ])).unwrap()),
                    ..Default::default()
                },
                ..Default::default()
            };
            for (provider, effort) in [(ProviderKind::Codex, "high"), (ProviderKind::Claude, "low")] {
                let selected = ModelRef { provider, id: id.clone() };
                for key in [crate::state::DraftKey::from("local"), crate::session::SessionRef { provider, id: "session".into() }.into()] {
                    Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), Arc::new(Draft {model:Some(selected.clone()),..Default::default()}));
                    proptest::prop_assert_eq!(snapshot.model_provider_for_draft(key.clone()), Some(provider));
                    proptest::prop_assert_eq!(snapshot.model_for_provider(key.clone(), provider), Some(selected.clone()));
                    proptest::prop_assert_eq!(snapshot.model_quick_controls(key).effort, effort);
                }
                let choices = snapshot.models_matching(Some(provider), id.clone());
                proptest::prop_assert_eq!(choices.len(), 1);
                proptest::prop_assert_eq!(&choices[0].model, &selected);
            }
            proptest::prop_assert_eq!(snapshot.models_matching(None, id).len(), 2);
        }
    }

    #[test]
    fn model_search_matches_display_names_ids_and_empty_queries_in_catalog_order() {
        let snapshot = Snapshot {
            catalog: crate::state::Catalog {
                models: Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":"codex-first","model":{"provider":"codex","id":"MX-ID"},"displayName":"Visible 🦀", "settings":{"defaultReasoningEffort":"","supportedReasoningEfforts":[]}},
                    {"id":"claude","model":{"provider":"claude","id":"MX-ID"},"displayName":"İSTANBUL", "settings":{"defaultReasoningEffort":"","supportedReasoningEfforts":[]}},
                    {"id":"codex-last","model":{"provider":"codex","id":"other"},"displayName":"Visible Other", "settings":{"defaultReasoningEffort":"","supportedReasoningEfforts":[]}}
                ])).unwrap()),
                ..Default::default()
            },
            ..Default::default()
        };
        for (provider, query, indices) in [
            (None, "", vec![0usize, 1, 2]),
            (None, " \t\u{2003}", vec![0, 1, 2]),
            (Some(ProviderKind::Codex), " \t\u{2003}", vec![0, 2]),
            (None, " ViSiBlE ", vec![0, 2]),
            (None, " MX-id ", vec![0, 1]),
            (Some(ProviderKind::Codex), " MX-id ", vec![0]),
            (None, " İSTANBUL ", vec![1]),
            (None, "i\u{307}stanBUL", vec![1]),
            (Some(ProviderKind::Codex), "İSTANBUL", vec![]),
            (None, "missing", vec![]),
        ] {
            let expected: Vec<_> = indices
                .into_iter()
                .map(|index| snapshot.catalog.models[index].clone())
                .collect();
            assert_eq!(
                snapshot.models_matching(provider, query.into()),
                expected,
                "query: {query:?}"
            );
        }
    }

    #[test]
    fn model_failures_are_shown_only_for_the_requested_provider() {
        let snapshot = Snapshot {
            catalog: crate::state::Catalog {
                model_errors: Arc::new(
                    serde_json::from_value(serde_json::json!({
                        "codex": {"message": "Codex catalog failed"},
                        "claude": {"message": "Claude catalog failed"}
                    }))
                    .unwrap(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            snapshot.model_error_messages(Some(ProviderKind::Codex)),
            ["codex: Codex catalog failed"]
        );
        assert_eq!(
            snapshot.model_error_messages(Some(ProviderKind::Claude)),
            ["claude: Claude catalog failed"]
        );
        assert_eq!(snapshot.model_error_messages(None).len(), 2);
    }

    #[test]
    fn quick_controls_use_capabilities_and_saved_values() {
        let mut snapshot = Snapshot {
            catalog: crate::state::Catalog {
                models: Arc::new(serde_json::from_value(serde_json::json!([
                {"id":"gpt","model":{"provider": "codex", "id": "gpt"},"displayName":"GPT", "settings": {"defaultReasoningEffort":"medium",
                 "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
                 "serviceTiers":[{"id":"priority","fast":true}]}},
                {"id":"claude:haiku","model":{"provider": "claude", "id": "haiku"},"displayName":"Haiku", "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}}
            ])).unwrap()),
                ..Default::default()
            },
            ..Default::default()
        };
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "gpt".into(),
                }),
                ..Default::default()
            }),
        );
        let controls = snapshot.model_quick_controls("draft".into());
        assert_eq!(controls.effort_level, 2);
        assert_eq!(controls.toggle_fast_to.as_deref(), Some("priority"));
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.input.drafts)
                .get_mut(&crate::state::DraftKey::from("draft"))
                .unwrap(),
        )
        .service_tier = Some("priority".into());
        assert_eq!(
            snapshot
                .model_quick_controls("draft".into())
                .toggle_fast_to
                .as_deref(),
            Some("default")
        );
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.input.drafts)
                .get_mut(&crate::state::DraftKey::from("draft"))
                .unwrap(),
        )
        .model = Some(agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Claude,
            id: "haiku".into(),
        });
        let controls = snapshot.model_quick_controls("draft".into());
        assert!(controls.efforts.is_empty());
        assert!(controls.toggle_fast_to.is_none());
        assert!(
            snapshot
                .models_matching(Some(ProviderKind::Codex), "haiku".into())
                .is_empty()
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CredentialControl {
    pub provider: agent_protocol::dsh::KeyProvider,
    pub label: String,
    pub key_url: String,
    pub status: String,
    pub can_remove: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn credential_controls(&self) -> Vec<CredentialControl> {
        use agent_protocol::dsh::{CredentialSource, KeyProvider};
        KeyProvider::ALL
            .into_iter()
            .map(|provider| {
                let route = self
                    .catalog
                    .account
                    .dsh_credentials
                    .as_ref()
                    .and_then(|credentials| {
                        credentials
                            .routes
                            .iter()
                            .find(|route| route.provider == provider)
                    });
                let status = match route {
                    Some(route) if route.needs_attention => "キーの接続先を確認してください",
                    Some(route) => match route.source {
                        CredentialSource::Managed => "保存済み（キーは非表示）",
                        CredentialSource::External => "ホストの外部設定を使用",
                        CredentialSource::Missing => "未設定",
                    },
                    None => "設定状態を取得してください",
                };
                CredentialControl {
                    provider,
                    label: provider.label().into(),
                    key_url: provider.key_url().into(),
                    status: status.into(),
                    can_remove: route
                        .is_some_and(|route| route.source == CredentialSource::Managed),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod credential_control_tests {
    use super::*;
    use agent_protocol::dsh::{CredentialRoute, CredentialSource, Credentials, KeyProvider};
    #[test]
    fn credential_controls_only_offer_removal_for_managed_keys() {
        let mut snapshot = Snapshot::default();
        std::sync::Arc::make_mut(&mut snapshot.catalog.account).dsh_credentials =
            Some(std::sync::Arc::new(Credentials {
                routes: vec![
                    CredentialRoute {
                        provider: KeyProvider::DeepSeek,
                        source: CredentialSource::External,
                        needs_attention: false,
                    },
                    CredentialRoute {
                        provider: KeyProvider::OpenRouter,
                        source: CredentialSource::Managed,
                        needs_attention: false,
                    },
                    CredentialRoute {
                        provider: KeyProvider::OpenAI,
                        source: CredentialSource::Managed,
                        needs_attention: true,
                    },
                ],
            }));
        let controls = snapshot.credential_controls();
        assert_eq!(controls.len(), 9);
        assert!(
            !controls
                .iter()
                .find(|route| route.provider == KeyProvider::DeepSeek)
                .unwrap()
                .can_remove
        );
        assert!(
            controls
                .iter()
                .find(|route| route.provider == KeyProvider::OpenRouter)
                .unwrap()
                .can_remove
        );
        let misplaced = controls
            .iter()
            .find(|route| route.provider == KeyProvider::OpenAI)
            .unwrap();
        assert!(misplaced.can_remove);
        assert!(misplaced.status.contains("確認"));
        assert!(
            !controls
                .iter()
                .find(|route| route.provider == KeyProvider::Gemini)
                .unwrap()
                .can_remove
        );
    }
}

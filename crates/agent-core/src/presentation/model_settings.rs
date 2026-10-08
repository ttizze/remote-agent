//! Model-picker and quick-control decisions shared by native clients.
use crate::{
    models::{Model, ModelRef},
    session::ProviderKind,
    state::{ModelDefaults, ModelDefaultsScope, ProviderModelDefaults, Snapshot},
};
use agent_protocol::operations::UsageWindow;

pub(crate) fn draft_provider(
    key: &crate::state::DraftKey,
    model: Option<&ModelRef>,
) -> Option<ProviderKind> {
    match key {
        crate::state::DraftKey::Session { session } => Some(session.provider),
        crate::state::DraftKey::Local { .. } => model.map(|model| model.provider),
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

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelQuickControls {
    pub efforts: Vec<String>,
    pub effort: String,
    pub effort_level: u32,
    pub fast: bool,
    pub toggle_fast_to: Option<String>,
    pub fast_service_tier: Option<String>,
}
#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelScopeChoice {
    pub id: String,
    pub label: String,
    pub scope: ModelDefaultsScope,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn model_defaults(&self, scope: ModelDefaultsScope) -> ModelDefaults {
        if scope == ModelDefaultsScope::Global {
            return self.model_defaults.clone();
        }
        let inherited = match &scope {
            ModelDefaultsScope::Project { environment, .. } => {
                self.scoped_model_defaults
                    .get(&ModelDefaultsScope::Environment {
                        id: environment.clone(),
                    })
            }
            _ => None,
        };
        self.scoped_model_defaults
            .get(&scope)
            .or(inherited)
            .unwrap_or(&self.model_defaults)
            .clone()
    }

    pub fn has_model_defaults_override(&self, scope: ModelDefaultsScope) -> bool {
        self.scoped_model_defaults.contains_key(&scope)
    }

    pub fn model_project_scope_choices(&self, scope: ModelDefaultsScope) -> Vec<ModelScopeChoice> {
        let all = match scope {
            ModelDefaultsScope::Global => ModelDefaultsScope::Global,
            _ if !self.storage_scope.is_empty() => ModelDefaultsScope::Environment {
                id: self.model_environment_id().to_owned(),
            },
            _ => ModelDefaultsScope::Global,
        };
        let mut choices = vec![ModelScopeChoice {
            id: "all".into(),
            label: "すべてのプロジェクト".into(),
            scope: all,
        }];
        if !self.storage_scope.is_empty() {
            choices.extend(
                self.threads
                    .iter()
                    .flat_map(|list| &list.projects)
                    .map(|project| ModelScopeChoice {
                        id: project.id.clone(),
                        label: project.name.clone(),
                        scope: ModelDefaultsScope::Project {
                            environment: self.model_environment_id().to_owned(),
                            project: project.id.clone(),
                        },
                    }),
            );
        }
        choices
    }

    pub fn model_environment_scope_choices(
        &self,
        scope: ModelDefaultsScope,
    ) -> Vec<ModelScopeChoice> {
        let mut choices = Vec::new();
        if !self.storage_scope.is_empty() {
            let current = match scope {
                ModelDefaultsScope::Global => ModelDefaultsScope::Environment {
                    id: self.model_environment_id().to_owned(),
                },
                other => other,
            };
            choices.push(ModelScopeChoice {
                id: "current".into(),
                label: self.host_name.clone().unwrap_or("この環境".into()),
                scope: current,
            });
        }
        choices.push(ModelScopeChoice {
            id: "all".into(),
            label: "すべての環境".into(),
            scope: ModelDefaultsScope::Global,
        });
        choices
    }

    pub fn provider_model_defaults(
        &self,
        scope: ModelDefaultsScope,
        provider: ProviderKind,
    ) -> ProviderModelDefaults {
        self.model_defaults(scope)
            .providers
            .get(&provider)
            .cloned()
            .unwrap_or_default()
    }

    pub fn default_model_controls(
        &self,
        scope: ModelDefaultsScope,
        provider: ProviderKind,
    ) -> ModelQuickControls {
        let defaults = self.provider_model_defaults(scope, provider);
        let (model, effort, tier) = crate::state::supported_settings(
            defaults.model.as_ref(),
            defaults.effort.as_deref(),
            defaults.service_tier.as_deref(),
            Some(provider),
            &self.models,
            !self.model_errors.is_empty(),
        );
        quick_controls(
            self.models
                .iter()
                .find(|choice| Some(&choice.model) == model),
            effort,
            tier,
        )
    }

    pub fn model_provider_for_draft(
        &self,
        thread_id: crate::state::DraftKey,
    ) -> Option<ProviderKind> {
        draft_provider(
            &thread_id,
            self.drafts
                .get(&thread_id)
                .and_then(|draft| draft.model.as_ref()),
        )
        .or_else(|| {
            automatic_provider(
                &self.models,
                self.account
                    .accounts
                    .as_ref()
                    .map(|accounts| &accounts.selected),
                &self.model_errors,
            )
        })
    }

    pub fn models_matching(&self, provider: Option<ProviderKind>, query: String) -> Vec<Model> {
        let query = query.trim().to_lowercase();
        self.models
            .iter()
            .filter(|model| provider.is_none_or(|provider| model.model.provider == provider))
            .filter(|model| {
                model.display_name.to_lowercase().contains(&query)
                    || model.model.id.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    pub fn model_for_provider(
        &self,
        thread_id: crate::state::DraftKey,
        provider: ProviderKind,
    ) -> Option<ModelRef> {
        let saved = self
            .drafts
            .get(&thread_id)
            .and_then(|draft| draft.model.as_ref());
        let defaults = self.model_defaults_for_cwd(&self.navigation.cwd);
        let preferred = saved
            .filter(|model| model.provider == provider)
            .or_else(|| {
                defaults
                    .providers
                    .get(&provider)
                    .and_then(|settings| settings.model.as_ref())
            });
        let (model, _, _) = crate::state::supported_settings(
            preferred,
            None,
            None,
            Some(provider),
            &self.models,
            !self.model_errors.is_empty(),
        );
        model.cloned()
    }

    /// Keep every weekly bucket (including model-specific limits); never turn
    /// an unavailable quota into a full or empty bar. Labels are Host-normalized.
    pub fn account_weekly_usage(&self, provider: ProviderKind, id: String) -> Vec<UsageWindow> {
        self.account
            .accounts
            .as_ref()
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

    pub fn model_quick_controls(&self, thread_id: crate::state::DraftKey) -> ModelQuickControls {
        let Some(draft) = self.drafts.get(&thread_id) else {
            return ModelQuickControls::default();
        };
        quick_controls(
            self.models
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
    let Some(model) = model else {
        return ModelQuickControls::default();
    };
    let efforts: Vec<_> = model
        .supported_reasoning_efforts
        .iter()
        .map(|choice| choice.reasoning_effort.clone())
        .collect();
    let effort = effort
        .filter(|value| !value.is_empty())
        .unwrap_or(&model.default_reasoning_effort)
        .to_owned();
    let effort_level = efforts
        .iter()
        .position(|value| *value == effort)
        .map_or(0, |index| index as u32 + 1);
    let tier = service_tier
        .filter(|value| !value.is_empty())
        .or(model.default_service_tier.as_deref())
        .unwrap_or("default");
    let fast_tier = model
        .service_tiers
        .as_deref()
        .unwrap_or_default()
        .iter()
        .find(|tier| tier.fast);
    let fast = fast_tier.is_some_and(|fast| fast.id == tier);
    ModelQuickControls {
        efforts,
        effort,
        effort_level,
        fast,
        fast_service_tier: fast_tier.map(|tier| tier.id.clone()),
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

    #[test]
    fn automatic_provider_uses_catalog_defaults_and_speed_uses_metadata() {
        use serde_json::json;
        let mut models: Vec<Model> = serde_json::from_value(json!([
            {"id":"first","model":{"provider":"codex","id":"first"},"displayName":"First",
             "defaultReasoningEffort":"","supportedReasoningEfforts":[],"isDefault":false},
            {"id":"preferred","model":{"provider":"claude","id":"preferred"},"displayName":"Preferred",
             "defaultReasoningEffort":"","supportedReasoningEfforts":[],"isDefault":true,
             "serviceTiers":[{"id":"express","fast":true},{"id":"priority","fast":false}]}
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
                {"id":"other", "model":{"provider":other,"id":"other"},"isDefault":true,"displayName":"Other","defaultReasoningEffort":"","supportedReasoningEfforts":[]},
                {"id":"available", "model":{"provider":provider,"id":"available"},"isDefault":true,"displayName":"Available","defaultReasoningEffort":"","supportedReasoningEfforts":[]}
            ])).unwrap();
            if reversed { models.reverse(); }
            let selected = [(provider, "account".into())].into();
            let errors = if failed { serde_json::Map::from_iter([(other.key().into(), json!({"message":"offline"}))]) } else { serde_json::Map::new() };
            proptest::prop_assert_eq!(automatic_provider(&models, Some(&selected), &errors), Some(provider));
            let snapshot = Snapshot {
                models: Arc::new(models),
                model_errors: Arc::new(errors),
                account: Arc::new(crate::state::AccountState {
                    accounts: Some(Arc::new(agent_protocol::operations::Accounts {
                        accounts: Vec::new(), selected, error: None,
                    })),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let (next, _) = reduce(&snapshot, Event::Intent(Intent::NewChat { cwd: "/project".into() }));
            let key = crate::state::DraftKey::from("new:/project");
            proptest::prop_assert_eq!(next.model_provider_for_draft(key.clone()), Some(provider));
            proptest::prop_assert_eq!(next.drafts[&key].model.as_ref().unwrap().provider, provider);
            proptest::prop_assert!(snapshot.drafts.is_empty());
        }
    }

    #[test]
    fn absent_or_failed_providers_preserve_unsent_input() {
        use crate::state::{Event, Intent, reduce};
        use serde_json::json;
        for catalog in [
            json!([]),
            json!([{"id":"failed","model":{"provider":"claude","id":"failed"},"displayName":"Failed","defaultReasoningEffort":"","supportedReasoningEfforts":[]}]),
        ] {
            let snapshot = Snapshot {
                models: Arc::new(serde_json::from_value(catalog).unwrap()),
                model_errors: Arc::new(serde_json::Map::from_iter([(
                    "claude".into(),
                    json!({"message":"offline"}),
                )])),
                ..Default::default()
            };
            let (snapshot, _) = reduce(
                &snapshot,
                Event::Intent(Intent::NewChat {
                    cwd: "/project".into(),
                }),
            );
            let key = snapshot.navigation.draft_key.clone();
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
            assert!(next.pending_submissions.is_empty());
            assert_eq!(next.drafts[&key].text, "keep this input");
            assert!(next.error.is_some());
        }
    }

    proptest::proptest! {
        #[test]
        fn identical_native_model_ids_keep_provider_choices_and_controls_separate(suffix in "[a-zA-Z0-9:_-]{1,40}") {
            let id = format!("claude:{suffix}");
            let mut snapshot = Snapshot {
                models: Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":id,"model":{"provider":"codex","id":id},"displayName":"Codex","isDefault":true,
                     "defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}]},
                    {"id":id,"model":{"provider":"claude","id":id},"displayName":"Claude","isDefault":true,
                     "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]}
                ])).unwrap()),
                ..Default::default()
            };
            for (provider, effort) in [(ProviderKind::Codex, "high"), (ProviderKind::Claude, "low")] {
                let selected = ModelRef { provider, id: id.clone() };
                for key in [crate::state::DraftKey::from("local"), crate::session::SessionRef { provider, id: "session".into() }.into()] {
                    Arc::make_mut(&mut snapshot.drafts).insert(key.clone(), Arc::new(Draft {model:Some(selected.clone()),..Default::default()}));
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
    fn model_failures_are_shown_only_for_the_requested_provider() {
        let snapshot = Snapshot {
            model_errors: Arc::new(
                serde_json::from_value(serde_json::json!({
                    "codex": {"message": "Codex catalog failed"},
                    "claude": {"message": "Claude catalog failed"}
                }))
                .unwrap(),
            ),
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
    fn quick_controls_use_capabilities_and_saved_values_without_inventing_quotas() {
        let mut snapshot = Snapshot { models: Arc::new(serde_json::from_value(serde_json::json!([
            {"id":"gpt","model":{"provider": "codex", "id": "gpt"},"displayName":"GPT","defaultReasoningEffort":"medium",
             "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
             "serviceTiers":[{"id":"priority","fast":true}]},
            {"id":"claude:haiku","model":{"provider": "claude", "id": "haiku"},"displayName":"Haiku","defaultReasoningEffort":"","supportedReasoningEfforts":[]}
        ])).unwrap()), ..Default::default() };
        Arc::make_mut(&mut snapshot.drafts).insert(
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
            Arc::make_mut(&mut snapshot.drafts)
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
            Arc::make_mut(&mut snapshot.drafts)
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
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(
            serde_json::from_value(serde_json::json!({"accounts":[
            {"id":"a","provider":"codex","usage":{"fetchedAt":1,"windows":[
                {"label":"5時間枠","remainingPercent":72},{"label":"週間枠","remainingPercent":42},
                {"label":"Opus 週間枠","remainingPercent":12}]}}
        ],"selected":{}}))
            .unwrap(),
        ));
        assert_eq!(
            snapshot
                .account_weekly_usage(ProviderKind::Codex, "a".into())
                .len(),
            2
        );
        let accounts = Arc::make_mut(
            Arc::make_mut(&mut snapshot.account)
                .accounts
                .as_mut()
                .unwrap(),
        );
        accounts.accounts[0].usage.as_mut().unwrap().error = Some("unavailable".into());
        assert!(
            snapshot
                .account_weekly_usage(ProviderKind::Codex, "a".into())
                .is_empty()
        );
    }
}

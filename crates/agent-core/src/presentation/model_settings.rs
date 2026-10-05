//! Model-picker and quick-control decisions shared by native clients.
use crate::{
    models::{
        Model, ModelCapabilities, ModelOptionKind, ModelOptionSelection, ModelOptionValue,
        ModelRef, model_option_value, provider_models,
    },
    session::ProviderInstanceId,
    state::{DraftKey, ModelDefaults, ModelDefaultsScope, Snapshot},
};
use agent_protocol::operations::UsageWindow;

pub(crate) fn draft_instance(
    instance: Option<&ProviderInstanceId>,
    model: Option<&ModelRef>,
) -> Option<ProviderInstanceId> {
    instance
        .or_else(|| model.map(|model| &model.instance_id))
        .cloned()
}

impl Snapshot {
    pub(crate) fn session_provider(
        &self,
        session: &crate::session::SessionRef,
    ) -> Option<ProviderInstanceId> {
        self.conversations
            .get(session)
            .and_then(|thread| {
                thread
                    .provider
                    .as_ref()
                    .map(|provider| provider.instance_id.clone())
            })
            .or_else(|| {
                self.threads
                    .as_ref()?
                    .data
                    .iter()
                    .find(|thread| thread.id.as_ref() == Some(session))?
                    .provider
                    .as_ref()
                    .map(|provider| provider.instance_id.clone())
            })
    }
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelQuickControls {
    pub effort: Option<ModelOptionControl>,
    pub effort_level: u32,
    pub fast: bool,
    pub fast_option_id: Option<String>,
    pub toggle_fast_to: Option<ModelOptionValue>,
}
#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelOptionControl {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub choices: Vec<ModelOptionControlChoice>,
    pub value: Option<ModelOptionValue>,
    pub value_label: Option<String>,
    pub is_explicit: bool,
    pub disabled_reason: Option<String>,
}
#[derive(Clone, Debug)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelOptionControlChoice {
    pub label: String,
    pub value: ModelOptionValue,
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

    pub fn default_model(&self, scope: ModelDefaultsScope) -> Option<Model> {
        let defaults = self.model_defaults(scope);
        let (model, _) = crate::state::supported_settings(
            defaults.model.as_ref(),
            &defaults.options,
            None,
            &self.models,
            !self.model_errors.is_empty(),
        );
        self.models
            .iter()
            .find(|choice| Some(&choice.model) == model)
            .cloned()
    }

    pub fn default_model_option_controls(
        &self,
        scope: ModelDefaultsScope,
    ) -> Vec<ModelOptionControl> {
        let defaults = self.model_defaults(scope.clone());
        let model = self.default_model(scope);
        let options = model
            .as_ref()
            .map(|model| {
                model.capabilities.normalize_options(
                    if defaults
                        .model
                        .as_ref()
                        .is_some_and(|saved| saved != &model.model)
                    {
                        &[]
                    } else {
                        &defaults.options
                    },
                    false,
                )
            })
            .unwrap_or(defaults.options);
        option_controls(
            model.as_ref().map(|model| &model.capabilities),
            &options,
            None,
        )
    }

    pub fn model_option_controls(&self, key: DraftKey) -> Vec<ModelOptionControl> {
        let Some(draft) = self.drafts.get(&key) else {
            return Vec::new();
        };
        option_controls(
            self.models
                .iter()
                .find(|model| Some(&model.model) == draft.model.as_ref())
                .map(|model| &model.capabilities),
            &draft.options,
            Some(&draft.text),
        )
    }

    /// Prefer a configured instance while retaining an existing conversation's owner.
    pub fn provider_selection(
        &self,
        key: DraftKey,
        preferred: Option<ProviderInstanceId>,
    ) -> Option<ProviderInstanceId> {
        preferred
            .filter(|id| {
                self.provider_instances
                    .iter()
                    .any(|entry| &entry.reference.instance_id == id)
            })
            .or_else(|| self.model_instance_for_draft(key))
            .or_else(|| {
                self.provider_instances
                    .first()
                    .map(|entry| entry.reference.instance_id.clone())
            })
    }
    pub fn instance_name(&self, instance_id: ProviderInstanceId) -> String {
        self.provider_instances
            .iter()
            .find(|entry| entry.reference.instance_id == instance_id)
            .map(|entry| entry.display_name.clone())
            .unwrap_or_else(|| instance_id.to_string())
    }
    pub fn instance_driver(
        &self,
        instance_id: ProviderInstanceId,
    ) -> Option<agent_protocol::providers::ProviderDriver> {
        self.provider_instances
            .iter()
            .find(|entry| entry.reference.instance_id == instance_id)
            .map(|entry| entry.reference.driver.clone())
    }
    pub fn model_instance_for_draft(
        &self,
        thread_id: crate::state::DraftKey,
    ) -> Option<ProviderInstanceId> {
        let instance = match &thread_id {
            crate::state::DraftKey::Session { session }
            | crate::state::DraftKey::Queued { session, .. } => self.session_provider(session),
            crate::state::DraftKey::Local { .. } => None,
        };
        draft_instance(
            instance.as_ref(),
            self.drafts
                .get(&thread_id)
                .and_then(|draft| draft.model.as_ref()),
        )
        .or_else(|| {
            self.models
                .first()
                .map(|model| model.model.instance_id.clone())
        })
    }

    pub fn models_matching(
        &self,
        provider: Option<ProviderInstanceId>,
        query: String,
    ) -> Vec<Model> {
        let query = query.trim().to_lowercase();
        self.models
            .iter()
            .filter(|model| {
                provider
                    .as_ref()
                    .is_none_or(|provider| &model.model.instance_id == provider)
            })
            .filter(|model| {
                model.display_name.to_lowercase().contains(&query)
                    || model.model.id.to_lowercase().contains(&query)
            })
            .cloned()
            .collect()
    }

    pub fn model_for_instance(
        &self,
        thread_id: crate::state::DraftKey,
        provider: ProviderInstanceId,
    ) -> Option<ModelRef> {
        let models = provider_models(&self.models, &provider);
        let saved = self
            .drafts
            .get(&thread_id)
            .and_then(|draft| draft.model.as_ref());
        models
            .iter()
            .find(|model| Some(&model.model) == saved)
            .or_else(|| models.iter().find(|model| model.is_default == Some(true)))
            .or(models.first())
            .map(|model| model.model.clone())
    }

    /// Keep every weekly bucket (including model-specific limits); never turn
    /// an unavailable quota into a full or empty bar. Labels are Host-normalized.
    pub fn account_weekly_usage(
        &self,
        provider: ProviderInstanceId,
        id: String,
    ) -> Vec<UsageWindow> {
        self.account
            .accounts
            .as_ref()
            .and_then(|accounts| {
                accounts
                    .accounts
                    .iter()
                    .find(|account| account.instance_id == provider && account.id == id)
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
                .find(|model| Some(&model.model) == draft.model.as_ref())
                .map(|model| &model.capabilities),
            &draft.options,
            Some(&draft.text),
        )
    }
}

fn quick_controls(
    capabilities: Option<&ModelCapabilities>,
    options: &[ModelOptionSelection],
    prompt: Option<&str>,
) -> ModelQuickControls {
    let Some(capabilities) = capabilities else {
        return ModelQuickControls::default();
    };
    let primary = capabilities.primary_select();
    let effort = option_controls(Some(capabilities), options, prompt)
        .into_iter()
        .find(|control| primary.is_some_and(|descriptor| descriptor.id == control.id))
        .filter(|control| !control.choices.is_empty());
    let effort_level = effort
        .as_ref()
        .and_then(|control| {
            control
                .choices
                .iter()
                .position(|choice| Some(&choice.value) == control.value.as_ref())
        })
        .map_or(0, |index| index as u32 + 1);
    let service = capabilities.select(&["serviceTier"]);
    let tier = service.and_then(|descriptor| descriptor.value(options));
    let mut fast_tiers = service
        .into_iter()
        .flat_map(|descriptor| descriptor.choices())
        .filter(|tier| {
            matches!(tier.id.as_str(), "priority" | "fast" | "ultrafast")
                || matches!(tier.label.as_str(), "Fast" | "Ultrafast")
        });
    let fast_tier = fast_tiers.clone().next();
    let boolean_fast = capabilities.option_descriptors.iter().find(|descriptor| {
        descriptor.id == "fastMode" && matches!(descriptor.kind, ModelOptionKind::Boolean { .. })
    });
    let fast = if fast_tier.is_some() {
        fast_tiers.any(|fast| tier.as_ref() == Some(&ModelOptionValue::String(fast.id.clone())))
    } else {
        boolean_fast.and_then(|descriptor| descriptor.value(options))
            == Some(ModelOptionValue::Boolean(true))
    };
    let (fast_option_id, toggle_fast_to) = if let Some(tier) = fast_tier {
        (
            service.map(|descriptor| descriptor.id.clone()),
            if fast {
                service
                    .and_then(|descriptor| {
                        descriptor
                            .choices()
                            .iter()
                            .find(|choice| choice.id == "default")
                    })
                    .map(|choice| ModelOptionValue::String(choice.id.clone()))
            } else {
                Some(ModelOptionValue::String(tier.id.clone()))
            },
        )
    } else if let Some(descriptor) = boolean_fast {
        (
            Some(descriptor.id.clone()),
            Some(ModelOptionValue::Boolean(!fast)),
        )
    } else {
        (None, None)
    };
    ModelQuickControls {
        effort,
        effort_level,
        fast,
        fast_option_id,
        toggle_fast_to,
    }
}

fn option_controls(
    capabilities: Option<&ModelCapabilities>,
    selections: &[ModelOptionSelection],
    prompt: Option<&str>,
) -> Vec<ModelOptionControl> {
    let prompt_primary = capabilities.zip(prompt).and_then(|(capabilities, text)| {
        crate::state::model_options::prompt_controlled_primary(capabilities, text)
    });
    capabilities
        .into_iter()
        .flat_map(|capabilities| &capabilities.option_descriptors)
        .map(|descriptor| {
            let is_explicit = model_option_value(selections, &descriptor.id).is_some();
            let prompt_controlled =
                prompt_primary.is_some_and(|primary| primary.id == descriptor.id);
            let value = if prompt_controlled {
                Some(ModelOptionValue::String("ultrathink".into()))
            } else if descriptor.id == "variant" && !is_explicit {
                None
            } else {
                descriptor.value(selections)
            };
            let value_label = value.as_ref().map(|value| match value {
                ModelOptionValue::String(value) => descriptor
                    .choices()
                    .iter()
                    .find(|choice| &choice.id == value)
                    .map(|choice| choice.label.clone())
                    .unwrap_or_else(|| value.clone()),
                ModelOptionValue::Boolean(value) => {
                    if *value {
                        "On".into()
                    } else {
                        "Off".into()
                    }
                }
            });
            ModelOptionControl {
                id: descriptor.id.clone(),
                label: descriptor.label.clone(),
                description: descriptor.description.clone(),
                choices: match &descriptor.kind {
                    ModelOptionKind::Select { options, .. } => options
                        .iter()
                        .map(|choice| ModelOptionControlChoice {
                            label: choice.label.clone(),
                            value: ModelOptionValue::String(choice.id.clone()),
                        })
                        .collect(),
                    ModelOptionKind::Boolean { .. } => [false, true]
                        .into_iter()
                        .map(|value| ModelOptionControlChoice {
                            label: if value { "On" } else { "Off" }.into(),
                            value: ModelOptionValue::Boolean(value),
                        })
                        .collect(),
                },
                value,
                value_label,
                is_explicit,
                disabled_reason: prompt
                    .filter(|text| {
                        prompt_controlled
                            && agent_protocol::model_prompt::contains_ultrathink(
                                agent_protocol::model_prompt::strip_ultrathink_prefix(text),
                            )
                    })
                    .map(|_| "本文中の ultrathink を削除すると、この設定を変更できます。".into()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Draft;

    #[test]
    fn generic_controls_keep_false_and_explicitness_and_drop_overrides_for_a_removed_model() {
        let model: Model = serde_json::from_value(serde_json::json!({
            "id":"custom", "model":{"instanceId":"claude","id":"custom"}, "displayName":"Custom",
            "capabilities":{"optionDescriptors":[
                {"id":"fastMode","label":"Fast","type":"boolean","currentValue":true},
                {"id":"variant","label":"Variant","type":"select","options":[{"id":"standard","label":"Standard","isDefault":true}]}
            ]}
        })).unwrap();
        let mut snapshot = Snapshot {
            models: Arc::new(vec![model.clone()]),
            model_defaults: ModelDefaults {
                model: Some(model.model.clone()),
                options: vec![ModelOptionSelection {
                    id: "fastMode".into(),
                    value: ModelOptionValue::Boolean(false),
                }],
            },
            ..Default::default()
        };
        let controls = snapshot.default_model_option_controls(ModelDefaultsScope::Global);
        assert_eq!(controls[0].value, Some(ModelOptionValue::Boolean(false)));
        assert_eq!(controls[0].value_label.as_deref(), Some("Off"));
        assert_eq!(
            controls[0]
                .choices
                .iter()
                .map(|choice| &choice.value)
                .collect::<Vec<_>>(),
            [
                &ModelOptionValue::Boolean(false),
                &ModelOptionValue::Boolean(true)
            ]
        );
        assert!(controls[0].is_explicit);
        assert_eq!(controls[1].value, None);
        assert!(!controls[1].is_explicit);
        assert_eq!(controls[1].choices[0].label, "Standard");
        assert_eq!(
            controls[1].choices[0].value,
            ModelOptionValue::String("standard".into())
        );
        let quick = quick_controls(
            Some(&model.capabilities),
            &snapshot.model_defaults.options,
            None,
        );
        assert!(!quick.fast);
        assert_eq!(quick.fast_option_id.as_deref(), Some("fastMode"));
        assert_eq!(quick.toggle_fast_to, Some(ModelOptionValue::Boolean(true)));
        snapshot.model_defaults.model.as_mut().unwrap().id = "removed".into();
        let controls = snapshot.default_model_option_controls(ModelDefaultsScope::Global);
        assert_eq!(controls[0].value, Some(ModelOptionValue::Boolean(true)));
        assert_eq!(controls[0].value_label.as_deref(), Some("On"));
        assert!(!controls[0].is_explicit);
        assert_eq!(
            snapshot.model_defaults.options[0].value,
            ModelOptionValue::Boolean(false)
        );
        Arc::make_mut(&mut snapshot.drafts).insert(
            "active".into(),
            Arc::new(Draft {
                model: Some(model.model),
                options: crate::models::with_model_option(
                    &snapshot.model_defaults.options,
                    "variant",
                    Some(ModelOptionValue::String("standard".into())),
                ),
                ..Default::default()
            }),
        );
        let controls = snapshot.model_option_controls("active".into());
        assert_eq!(controls[0].value, Some(ModelOptionValue::Boolean(false)));
        assert!(controls[0].is_explicit);
        assert_eq!(
            controls[1].value,
            Some(ModelOptionValue::String("standard".into()))
        );
        assert_eq!(controls[1].value_label.as_deref(), Some("Standard"));
        assert!(controls[1].is_explicit);
        assert!(snapshot.model_option_controls("missing".into()).is_empty());
    }
    use std::sync::Arc;

    proptest::proptest! {
        #[test]
        fn identical_native_model_ids_keep_provider_choices_and_controls_separate(suffix in "[a-zA-Z0-9:_-]{1,40}") {
            let id = format!("claude:{suffix}");
            let mut snapshot = Snapshot {
                models: Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":id, "model":{"instanceId":"codex","id":id}, "displayName":"Codex", "isDefault":true, "capabilities":{"optionDescriptors":[{"id":"reasoningEffort","label":"Reasoning","type":"select","options":[{"id":"high","label":"high","isDefault":true}],"currentValue":"high"}]}},
                    {"id":id, "model":{"instanceId":"claude","id":id}, "displayName":"Claude", "isDefault":true, "capabilities":{"optionDescriptors":[{"id":"reasoningEffort","label":"Reasoning","type":"select","options":[{"id":"low","label":"low","isDefault":true}],"currentValue":"low"}]}}
                ])).unwrap()),
                ..Default::default()
            };
            for (provider, effort) in [("codex".parse::<crate::session::ProviderInstanceId>().unwrap(), "high"), ("claude".parse::<crate::session::ProviderInstanceId>().unwrap(), "low")] {
                let selected = ModelRef { instance_id: provider.clone(), id: id.clone() };
                for key in [crate::state::DraftKey::from("local"), crate::session::SessionRef { id: "session".into() }.into()] {
                    Arc::make_mut(&mut snapshot.drafts).insert(key.clone(), Arc::new(Draft {model:Some(selected.clone()),..Default::default()}));
                    proptest::prop_assert_eq!(snapshot.model_instance_for_draft(key.clone()), Some(provider.clone()));
                    proptest::prop_assert_eq!(snapshot.model_for_instance(key.clone(), provider.clone()), Some(selected.clone()));
                    proptest::prop_assert_eq!(snapshot.model_quick_controls(key).effort.unwrap().value, Some(ModelOptionValue::String(effort.into())));
                }
                let choices = snapshot.models_matching(Some(provider.clone()), id.clone());
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
            snapshot.model_error_messages(Some(
                "codex"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap()
            )),
            ["codex: Codex catalog failed"]
        );
        assert_eq!(
            snapshot.model_error_messages(Some(
                "claude"
                    .parse::<crate::session::ProviderInstanceId>()
                    .unwrap()
            )),
            ["claude: Claude catalog failed"]
        );
        assert_eq!(snapshot.model_error_messages(None).len(), 2);
    }

    #[test]
    fn quick_controls_use_capabilities_and_saved_values_without_inventing_quotas() {
        let mut snapshot = Snapshot { models: Arc::new(serde_json::from_value(serde_json::json!([
            {"id":"gpt", "model":{"instanceId": "codex", "id": "gpt"}, "displayName":"GPT", "capabilities":{"optionDescriptors":[{"id":"reasoningEffort","label":"Reasoning","type":"select","options":[{"id":"low","label":"low","isDefault":false},{"id":"medium","label":"medium","isDefault":true},{"id":"high","label":"high","isDefault":false}],"currentValue":"medium"},{"id":"serviceTier","label":"Service Tier","type":"select","options":[{"id":"default","label":"Standard","isDefault":true},{"id":"priority","label":"priority","isDefault":false}],"currentValue":"default"}]}},
            {"id":"claude:haiku", "model":{"instanceId": "claude", "id": "haiku"}, "displayName":"Haiku", "capabilities":{"optionDescriptors":[{"id":"thinking","label":"Thinking","type":"boolean","currentValue":true}]}}
        ])).unwrap()), ..Default::default() };
        Arc::make_mut(&mut snapshot.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some(agent_protocol::models::ModelRef {
                    instance_id: "codex"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap(),
                    id: "gpt".into(),
                }),
                ..Default::default()
            }),
        );
        let controls = snapshot.model_quick_controls("draft".into());
        assert_eq!(controls.effort_level, 2);
        assert_eq!(
            controls.toggle_fast_to,
            Some(ModelOptionValue::String("priority".into()))
        );
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.drafts)
                .get_mut(&crate::state::DraftKey::from("draft"))
                .unwrap(),
        )
        .options = vec![ModelOptionSelection {
            id: "serviceTier".into(),
            value: ModelOptionValue::String("priority".into()),
        }];
        assert_eq!(
            snapshot.model_quick_controls("draft".into()).toggle_fast_to,
            Some(ModelOptionValue::String("default".into()))
        );
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.drafts)
                .get_mut(&crate::state::DraftKey::from("draft"))
                .unwrap(),
        )
        .model = Some(agent_protocol::models::ModelRef {
            instance_id: "claude"
                .parse::<crate::session::ProviderInstanceId>()
                .unwrap(),
            id: "haiku".into(),
        });
        let controls = snapshot.model_quick_controls("draft".into());
        assert!(controls.effort.is_none());
        assert!(controls.toggle_fast_to.is_none());
        assert!(
            snapshot
                .models_matching(
                    Some(
                        "codex"
                            .parse::<crate::session::ProviderInstanceId>()
                            .unwrap()
                    ),
                    "haiku".into()
                )
                .is_empty()
        );
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(
            serde_json::from_value(serde_json::json!({"accounts":[
            {"id":"a","instanceId":"codex","usage":{"fetchedAt":1,"windows":[
                {"label":"5時間枠","remainingPercent":72},{"label":"週間枠","remainingPercent":42},
                {"label":"Opus 週間枠","remainingPercent":12}]}}
        ],"selected":{}}))
            .unwrap(),
        ));
        assert_eq!(
            snapshot
                .account_weekly_usage(
                    "codex"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap(),
                    "a".into()
                )
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
                .account_weekly_usage(
                    "codex"
                        .parse::<crate::session::ProviderInstanceId>()
                        .unwrap(),
                    "a".into()
                )
                .is_empty()
        );
    }
}

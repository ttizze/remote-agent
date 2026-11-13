//! Model catalogs and per-model settings.
use super::*;
use crate::client::ClientExt;

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadModels {
    pub provider: crate::session::ProviderKind,
}

pub(crate) fn refresh_models() -> Vec<Effect> {
    use crate::session::ProviderKind;
    [
        ProviderKind::Codex,
        ProviderKind::Claude,
        ProviderKind::DeepSeek,
    ]
    .into_iter()
    .map(|provider| Effect::execute(LoadModels { provider }))
    .collect()
}

impl Operation for LoadModels {
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::Models {
            provider: self.provider,
        })
    }
    fn scheduling(&self) -> Scheduling {
        Scheduling::LatestHostState(OperationKey::Models {
            provider: self.provider,
        })
    }
    no_input!();
    type Output = rpc::ModelPage;
    async fn run(&self, _: (), context: &mut Execution<'_>) -> Result<Self::Output, PeerError> {
        context.client.models(self.provider).await
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, catalog: Self::Output) -> Vec<Effect> {
        let models = Arc::make_mut(&mut snapshot.catalog.models);
        models.retain(|model| model.model.provider != self.provider);
        if snapshot
            .catalog
            .provider_settings
            .as_ref()
            .is_none_or(|settings| settings.enabled(self.provider))
        {
            models.extend(catalog.data);
        }
        models.sort_by_key(|model| model.model.provider);
        let errors = Arc::make_mut(&mut snapshot.catalog.model_errors);
        errors.remove(self.provider.key());
        errors.extend(catalog.provider_errors.unwrap_or_default());
        snapshot.normalize_model_drafts();
        Arc::make_mut(&mut snapshot.operations).retain(|key, state| {
            !matches!(key, OperationKey::ModelSettings { model } if model.provider == self.provider)
                || state.phase == OperationPhase::Running
        });
        Vec::new()
    }
}

pub(crate) fn prefetch_model_settings<'a>(
    connected: bool,
    selected: Option<&crate::models::ModelRef>,
    models: &[Model],
    mut pending: impl Iterator<Item = &'a OperationKey>,
) -> Option<Effect> {
    if !connected {
        return None;
    }
    let selected = selected?;
    let model = models.iter().find(|model| {
        &model.model == selected && model.settings.is_none() && model.settings_discovery
    })?;
    if pending.any(|key| matches!(key, OperationKey::ModelSettings { model } if model == selected))
    {
        return None;
    }
    Some(Effect::execute(ReadModelSettings {
        model: model.model.clone(),
    }))
}

pub use agent_protocol::operations::ReadModelSettings;
impl Operation for ReadModelSettings {
    const BACKGROUND: bool = true;
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ModelSettings {
            model: self.model.clone(),
        })
    }
    rpc_operation!();
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, settings: Self::Output) -> Vec<Effect> {
        if let Some(model) = Arc::make_mut(&mut snapshot.catalog.models)
            .iter_mut()
            .find(|model| model.model == self.model)
        {
            model.settings = Some(settings);
            snapshot.normalize_model_drafts();
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ProviderKind;

    #[test]
    fn refreshing_one_provider_preserves_other_catalogs_errors_and_settings() {
        let catalog = |provider, id| {
            serde_json::from_value(serde_json::json!({
                "data":[{"id":id,"model":{"provider":provider,"id":id},"displayName":id}]
            }))
            .unwrap()
        };
        let mut snapshot = Snapshot::default();
        for (provider, id) in [
            (ProviderKind::Codex, "old"),
            (ProviderKind::Claude, "retained"),
        ] {
            LoadModels { provider }.apply(&mut snapshot, "", catalog(provider, id));
        }
        let retained = snapshot.catalog.models[1].clone();
        Arc::make_mut(&mut snapshot.catalog.model_errors).insert(
            "claude".into(),
            serde_json::json!({"message":"retained error"}),
        );
        let key = OperationKey::ModelSettings {
            model: retained.model.clone(),
        };
        Arc::make_mut(&mut snapshot.operations).insert(
            key.clone(),
            OperationState {
                generation: 1,
                phase: OperationPhase::Failed {
                    message: "retained settings failure".into(),
                },
            },
        );
        let failed = OperationKey::ModelSettings {
            model: snapshot.catalog.models[0].model.clone(),
        };
        let running = OperationKey::ModelSettings {
            model: crate::models::ModelRef {
                provider: ProviderKind::Codex,
                id: "pending".into(),
            },
        };
        for (key, phase) in [
            (
                failed.clone(),
                OperationPhase::Failed {
                    message: "retry after refresh".into(),
                },
            ),
            (running.clone(), OperationPhase::Running),
        ] {
            Arc::make_mut(&mut snapshot.operations).insert(
                key,
                OperationState {
                    generation: 1,
                    phase,
                },
            );
        }
        LoadModels {
            provider: ProviderKind::Codex,
        }
        .apply(&mut snapshot, "", catalog(ProviderKind::Codex, "fresh"));
        assert_eq!(snapshot.catalog.models[0].id, "fresh");
        assert_eq!(snapshot.catalog.models[1], retained);
        assert_eq!(
            snapshot.catalog.model_errors["claude"]["message"],
            "retained error"
        );
        assert!(snapshot.operations.contains_key(&key));
        assert!(!snapshot.operations.contains_key(&failed));
        assert_eq!(
            snapshot.operations.get(&running).unwrap().phase,
            OperationPhase::Running
        );
        assert_ne!(
            LoadModels {
                provider: ProviderKind::Codex
            }
            .key(),
            LoadModels {
                provider: ProviderKind::Claude
            }
            .key()
        );
    }

    #[test]
    fn model_settings_resolution_preserves_unresolved_drafts_and_applies_by_model_identity() {
        let model = crate::models::ModelRef {
            provider: ProviderKind::DeepSeek,
            id: "router-deepseek".into(),
        };
        let other = crate::models::ModelRef {
            provider: ProviderKind::DeepSeek,
            id: "other-route".into(),
        };
        let mut snapshot = Snapshot::default();
        snapshot = reduce_intent(
            &snapshot,
            "",
            Intent::SetDraft {
                thread_id: DraftKey::default(),
                draft: Draft::default(),
            },
        )
        .0;
        let key = snapshot.panes.primary().navigation.draft_key.clone();
        let other_key = DraftKey::from("other");
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                model: Some(model.clone()),
                effort: Some("high".into()),
                ..Default::default()
            }),
        );
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            other_key.clone(),
            Arc::new(Draft {
                model: Some(other.clone()),
                effort: Some("max".into()),
                ..Default::default()
            }),
        );
        let page = serde_json::from_value(serde_json::json!({"data":[
            {"id":"selected","model":model,"displayName":"Selected","settings":null,"settingsDiscovery":true},
            {"id":"other","model":other,"displayName":"Other","settings":null,"settingsDiscovery":true}
        ],"nextCursor":null}))
        .unwrap();
        snapshot.connection.connected = true;
        LoadModels {
            provider: ProviderKind::DeepSeek,
        }
        .apply(&mut snapshot, "", page);
        assert!(
            prefetch_model_settings(
                snapshot.connection.connected,
                snapshot
                    .input
                    .drafts
                    .get(&key)
                    .and_then(|draft| draft.model.as_ref()),
                &snapshot.catalog.models,
                snapshot.operations.keys(),
            )
            .is_some()
        );
        assert_eq!(snapshot.input.drafts[&key].effort.as_deref(), Some("high"));
        assert_eq!(
            snapshot.input.drafts[&other_key].effort.as_deref(),
            Some("max")
        );
        let (mut switched, _) = reduce_intent(
            &snapshot,
            "",
            Intent::SelectModel {
                thread_id: key.clone(),
                model: other.clone(),
            },
        );
        let settings = serde_json::from_value(serde_json::json!({"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]})).unwrap();
        ReadModelSettings {
            model: model.clone(),
        }
        .apply(&mut switched, "", settings);
        assert_eq!(switched.input.drafts[&key].model.as_ref(), Some(&other));
        assert_eq!(
            switched.input.drafts[&other_key].effort.as_deref(),
            Some("max")
        );
        assert!(
            switched
                .catalog
                .models
                .iter()
                .find(|entry| entry.model == model)
                .unwrap()
                .settings
                .is_some()
        );
        assert!(
            switched
                .catalog
                .models
                .iter()
                .find(|entry| entry.model == other)
                .unwrap()
                .settings
                .is_none()
        );
    }
}

#[cfg(test)]
mod model_settings_properties {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn unresolved_settings_never_invalidate_a_saved_choice(effort in ".{0,40}", tier in ".{0,40}") {
            let model = crate::models::Model { id: "model".into(), model: crate::models::ModelRef { provider: crate::session::ProviderKind::DeepSeek, id: "route".into() }, display_name: "Model".into(), credential_route: None, settings: None, settings_discovery: true, is_default: None };
            let models = vec![model];
            let result = supported_settings(Some(&models[0].model), Some(&effort), Some(&tier), None, &models, &Default::default(), false);
            prop_assert_eq!(result, (Some(&models[0].model), Some(effort.as_str()), Some(tier.as_str())));
            prop_assert_eq!(supported_settings(Some(&models[0].model), Some(&effort), Some(&tier), None, &[], &Default::default(), false), (Some(&models[0].model), Some(effort.as_str()), Some(tier.as_str())));
            let mut known = models.clone();
            known[0].settings = Some(crate::models::ModelSettings { default_reasoning_effort: String::new(), supported_reasoning_efforts: vec![], service_tiers: None, default_service_tier: None });
            prop_assert!(supported_settings(Some(&known[0].model), Some(&effort), Some(&tier), None, &known, &Default::default(), false).1.is_none());
        }
    }
}

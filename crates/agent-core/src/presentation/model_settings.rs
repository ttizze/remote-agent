//! Model-picker and quick-control decisions shared by native clients.
use crate::{
    models::{Model, ModelRef, provider_models},
    session::ProviderKind,
    state::Snapshot,
};
use agent_protocol::operations::UsageWindow;

pub(crate) fn draft_provider(
    key: &crate::state::DraftKey,
    model: Option<&ModelRef>,
) -> ProviderKind {
    match key {
        crate::state::DraftKey::Session { session } => session.provider,
        crate::state::DraftKey::Local { .. } => model
            .map(|model| model.provider)
            .unwrap_or(ProviderKind::Codex),
    }
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

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn model_defaults(&self) -> crate::state::ModelDefaults {
        self.model_defaults.clone()
    }

    pub fn default_model(&self) -> Option<Model> {
        let (model, _, _) = crate::state::supported_settings(
            self.model_defaults.model.as_ref(),
            self.model_defaults.effort.as_deref(),
            self.model_defaults.service_tier.as_deref(),
            None,
            &self.models,
            !self.model_errors.is_empty(),
        );
        self.models
            .iter()
            .find(|choice| Some(&choice.model) == model)
            .cloned()
    }

    pub fn default_model_controls(&self) -> ModelQuickControls {
        let (model, effort, tier) = crate::state::supported_settings(
            self.model_defaults.model.as_ref(),
            self.model_defaults.effort.as_deref(),
            self.model_defaults.service_tier.as_deref(),
            None,
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

    pub fn model_provider_for_draft(&self, thread_id: crate::state::DraftKey) -> ProviderKind {
        draft_provider(
            &thread_id,
            self.drafts
                .get(&thread_id)
                .and_then(|draft| draft.model.as_ref()),
        )
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
        let models = provider_models(&self.models, provider);
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
        .find(|tier| matches!(tier.id.as_str(), "priority" | "fast"));
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
                    proptest::prop_assert_eq!(snapshot.model_provider_for_draft(key.clone()), provider);
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
             "serviceTiers":[{"id":"priority"}]},
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

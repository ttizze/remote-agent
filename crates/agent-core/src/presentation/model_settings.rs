//! Model-picker and quick-control decisions shared by native clients.
use crate::{
    models::{Model, model_provider, provider_models},
    session::ProviderKind,
    state::Snapshot,
};
use agent_protocol::operations::UsageWindow;

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelQuickControls {
    pub efforts: Vec<String>,
    pub effort: String,
    pub effort_level: u32,
    pub fast: bool,
    pub toggle_fast_to: Option<String>,
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl Snapshot {
    pub fn model_provider_for_draft(&self, thread_id: String) -> ProviderKind {
        self.drafts
            .get(&thread_id)
            .and_then(|draft| draft.model.as_deref())
            .map(model_provider)
            .unwrap_or(ProviderKind::Codex)
    }

    pub fn provider_models_matching(&self, provider: ProviderKind, query: String) -> Vec<Model> {
        let query = query.trim().to_lowercase();
        provider_models(&self.models, provider)
            .into_iter()
            .filter(|model| {
                model.display_name.to_lowercase().contains(&query)
                    || model.model.to_lowercase().contains(&query)
            })
            .collect()
    }

    pub fn model_for_provider(&self, thread_id: String, provider: ProviderKind) -> Option<String> {
        let models = provider_models(&self.models, provider);
        let saved = self
            .drafts
            .get(&thread_id)
            .and_then(|draft| draft.model.as_deref());
        models
            .iter()
            .find(|model| Some(model.model.as_str()) == saved)
            .or_else(|| models.iter().find(|model| model.is_default == Some(true)))
            .or(models.first())
            .map(|model| model.model.clone())
    }

    /// Keep every weekly bucket (including model-specific limits); never turn
    /// an unavailable quota into a full or empty bar. Labels are Host-normalized.
    pub fn account_weekly_usage(&self, id: String) -> Vec<UsageWindow> {
        self.account
            .accounts
            .as_ref()
            .and_then(|accounts| accounts.accounts.iter().find(|account| account.id == id))
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

    pub fn model_quick_controls(&self, thread_id: String) -> ModelQuickControls {
        let Some(draft) = self.drafts.get(&thread_id) else {
            return ModelQuickControls::default();
        };
        let Some(model) = self
            .models
            .iter()
            .find(|model| Some(&model.model) == draft.model.as_ref())
        else {
            return ModelQuickControls::default();
        };
        let efforts: Vec<_> = model
            .supported_reasoning_efforts
            .iter()
            .map(|choice| choice.reasoning_effort.clone())
            .collect();
        let effort = draft
            .effort
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(&model.default_reasoning_effort)
            .to_owned();
        let effort_level = efforts
            .iter()
            .position(|value| *value == effort)
            .map_or(0, |index| index as u32 + 1);
        let tier = draft
            .service_tier
            .as_deref()
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
            toggle_fast_to: fast_tier.map(|tier| {
                if fast {
                    "default".into()
                } else {
                    tier.id.clone()
                }
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Draft;
    use std::sync::Arc;

    #[test]
    fn quick_controls_use_capabilities_and_saved_values_without_inventing_quotas() {
        let mut snapshot = Snapshot { models: Arc::new(serde_json::from_value(serde_json::json!([
            {"id":"gpt","model":"gpt","displayName":"GPT","defaultReasoningEffort":"medium",
             "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
             "serviceTiers":[{"id":"priority"}]},
            {"id":"claude:haiku","model":"claude:haiku","displayName":"Haiku","defaultReasoningEffort":"","supportedReasoningEfforts":[]}
        ])).unwrap()), ..Default::default() };
        Arc::make_mut(&mut snapshot.drafts).insert(
            "draft".into(),
            Arc::new(Draft {
                model: Some("gpt".into()),
                ..Default::default()
            }),
        );
        let controls = snapshot.model_quick_controls("draft".into());
        assert_eq!(controls.effort_level, 2);
        assert_eq!(controls.toggle_fast_to.as_deref(), Some("priority"));
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.drafts)
                .get_mut("draft")
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
                .get_mut("draft")
                .unwrap(),
        )
        .model = Some("claude:haiku".into());
        let controls = snapshot.model_quick_controls("draft".into());
        assert!(controls.efforts.is_empty());
        assert!(controls.toggle_fast_to.is_none());
        assert!(
            snapshot
                .provider_models_matching(ProviderKind::Codex, "haiku".into())
                .is_empty()
        );
        Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(serde_json::from_value(serde_json::json!({"accounts":[
            {"id":"a","provider":"codex","usage":{"fetchedAt":1,"windows":[
                {"label":"5時間枠","remainingPercent":72},{"label":"週間枠","remainingPercent":42},
                {"label":"Opus 週間枠","remainingPercent":12}]}}
        ]})).unwrap()));
        assert_eq!(snapshot.account_weekly_usage("a".into()).len(), 2);
        let accounts = Arc::make_mut(
            Arc::make_mut(&mut snapshot.account)
                .accounts
                .as_mut()
                .unwrap(),
        );
        accounts.accounts[0].usage.as_mut().unwrap().error = Some("unavailable".into());
        assert!(snapshot.account_weekly_usage("a".into()).is_empty());
    }
}

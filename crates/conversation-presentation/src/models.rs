use crate::{array, text};
use serde_json::Value;

pub fn supported_model_settings<'a>(
    model: Option<&'a Value>,
    effort: &str,
    tier: &str,
) -> (&'a str, &'a str, usize) {
    let Some(model) = model else {
        return ("", "default", 0);
    };
    let efforts = array(&model["supportedReasoningEfforts"]);
    let index = efforts
        .iter()
        .position(|e| e["reasoningEffort"] == effort)
        .or_else(|| {
            efforts
                .iter()
                .position(|e| e["reasoningEffort"] == model["defaultReasoningEffort"])
        })
        .unwrap_or(0);
    let tiers = array(&model["serviceTiers"]);
    let supported_tier = |id: &str| {
        if id == "default" {
            Some("default")
        } else {
            tiers.iter().find(|t| t["id"] == id).map(|t| text(t, "id"))
        }
    };
    (
        efforts
            .get(index)
            .map(|e| text(e, "reasoningEffort"))
            .unwrap_or_default(),
        supported_tier(tier)
            .or_else(|| supported_tier(text(model, "defaultServiceTier")))
            .unwrap_or("default"),
        index,
    )
}

#[cfg(test)]
mod model_settings_tests {
    use super::supported_model_settings;
    use serde_json::json;

    #[test]
    fn refreshed_catalog_preserves_supported_choices_and_tracks_reordered_efforts() {
        let mut model = json!({
            "defaultReasoningEffort":"medium", "defaultServiceTier":"priority",
            "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"max"}],
            "serviceTiers":[{"id":"priority"}]
        });
        assert_eq!(
            supported_model_settings(Some(&model), "max", "default"),
            ("max", "default", 1)
        );
        model["supportedReasoningEfforts"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert_eq!(
            supported_model_settings(Some(&model), "max", "priority"),
            ("max", "priority", 0)
        );
    }

    #[test]
    fn changed_model_or_removed_options_use_supported_defaults() {
        let mut model = json!({
            "defaultReasoningEffort":"low", "defaultServiceTier":"priority",
            "supportedReasoningEfforts":[{"reasoningEffort":"low"}],
            "serviceTiers":[{"id":"priority"}]
        });
        assert_eq!(
            supported_model_settings(Some(&model), "", ""),
            ("low", "priority", 0)
        );
        model["serviceTiers"] = json!([]);
        assert_eq!(
            supported_model_settings(Some(&model), "max", "priority"),
            ("low", "default", 0)
        );
        model["defaultReasoningEffort"] = json!("max");
        assert_eq!(
            supported_model_settings(Some(&model), "", ""),
            ("low", "default", 0)
        );
    }

    #[test]
    fn no_model_or_no_options_clears_effort_and_fast() {
        assert_eq!(
            supported_model_settings(None, "max", "priority"),
            ("", "default", 0)
        );
        assert_eq!(
            supported_model_settings(Some(&json!({})), "max", "priority"),
            ("", "default", 0)
        );
    }
}

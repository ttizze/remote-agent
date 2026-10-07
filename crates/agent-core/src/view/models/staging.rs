//! The mobile thread settings sheet's staged model: a model picked but not
//! saved yet, with the options edited for it, and the options each model was
//! last given, which a newly picked model takes again.
use super::{
    ModelCatalog,
    ordering::provider_model_key,
    traits::{TraitsView, build_traits, select_trait, toggle_trait},
};
use crate::state::{Draft, ModelOption};
use agent_domain::Driver;
use std::collections::BTreeMap;

/// A model the sheet holds until Save; the option rows edit its options.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct StagedModel {
    pub instance_id: String,
    pub driver: Driver,
    pub model: String,
    pub options: Vec<ModelOption>,
}
impl StagedModel {
    /// `instance:slug`, as `CatalogSheetOptions.staged_key` names it.
    pub fn key(&self) -> String {
        provider_model_key(&self.instance_id, &self.model)
    }
    fn draft(&self) -> Draft {
        Draft {
            instance_id: self.instance_id.clone(),
            driver: self.driver,
            model: self.model.clone(),
            options: self.options.clone(),
            ..Draft::default()
        }
    }
}

/// Pressing a catalogue model: the applied model ends staging, the staged
/// model keeps its edited options, another model is staged with its own.
pub fn staged_model_after_press(
    current: Option<StagedModel>,
    pressed: StagedModel,
    pressed_is_applied: bool,
) -> Option<StagedModel> {
    if pressed_is_applied {
        return None;
    }
    match current {
        Some(current) if current.key() == pressed.key() => Some(current),
        _ => Some(pressed),
    }
}

/// The staged model's option rows.
pub fn staged_model_traits(catalog: &ModelCatalog, staged: &StagedModel) -> TraitsView {
    build_traits(catalog, &staged.draft(), false)
}

/// The staged model after choosing a select option.
pub fn select_staged_trait(
    catalog: &ModelCatalog,
    staged: StagedModel,
    descriptor_id: &str,
    choice: &str,
) -> StagedModel {
    let options = select_trait(catalog, &staged.draft(), false, descriptor_id, choice)
        .and_then(|change| change.options);
    with_options(staged, options)
}

/// The staged model after switching a toggle option.
pub fn toggle_staged_trait(
    catalog: &ModelCatalog,
    staged: StagedModel,
    descriptor_id: &str,
    on: bool,
) -> StagedModel {
    let options =
        toggle_trait(catalog, &staged.draft(), descriptor_id, on).and_then(|change| change.options);
    with_options(staged, options)
}

fn with_options(staged: StagedModel, options: Option<Vec<ModelOption>>) -> StagedModel {
    match options {
        Some(options) => StagedModel { options, ..staged },
        None => staged,
    }
}

/// A staged model can be saved while its provider still offers it.
pub fn can_save_staged_model(catalog: &ModelCatalog, staged: &StagedModel) -> bool {
    catalog
        .instance(&staged.instance_id)
        .is_some_and(|instance| instance.enabled && instance.installed && instance.available)
        && catalog
            .models_of(&staged.instance_id)
            .any(|model| model.slug == staged.model)
}

/// Each model's last explicitly chosen options, by instance and model.
pub type ModelOptionMemory = BTreeMap<String, BTreeMap<String, Vec<ModelOption>>>;

/// Records an explicitly chosen option set; an empty one changes nothing.
pub fn remember_model_options(
    memory: &mut ModelOptionMemory,
    instance_id: &str,
    model: &str,
    options: &[ModelOption],
) {
    if options.is_empty() {
        return;
    }
    memory
        .entry(instance_id.into())
        .or_default()
        .insert(model.into(), options.to_vec());
}

pub fn remembered_model_options<'a>(
    memory: &'a ModelOptionMemory,
    instance_id: &str,
    model: &str,
) -> Option<&'a Vec<ModelOption>> {
    memory.get(instance_id)?.get(model)
}

/// The options a freshly picked model starts with: the remembered ones, else
/// the incoming ones.
pub fn with_remembered_model_options(
    memory: &ModelOptionMemory,
    instance_id: &str,
    model: &str,
    options: Vec<ModelOption>,
) -> Vec<ModelOption> {
    remembered_model_options(memory, instance_id, model)
        .cloned()
        .unwrap_or(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::models::fixtures::{instance, model};

    fn option(key: &str, value: &str) -> ModelOption {
        ModelOption {
            key: key.into(),
            value: value.into(),
        }
    }

    fn staged(model: &str, options: Vec<ModelOption>) -> StagedModel {
        StagedModel {
            instance_id: "codex".into(),
            driver: Driver::Codex,
            model: model.into(),
            options,
        }
    }

    #[test]
    fn clears_staging_when_the_applied_model_is_pressed() {
        assert_eq!(
            staged_model_after_press(
                Some(staged("gpt-next", vec![])),
                staged("gpt-current", vec![]),
                true
            ),
            None
        );
    }

    #[test]
    fn preserves_staged_options_when_the_highlighted_model_is_pressed_again() {
        let pending = staged("gpt-next", vec![option("effort", "high")]);
        assert_eq!(
            staged_model_after_press(Some(pending.clone()), staged("gpt-next", vec![]), false),
            Some(pending)
        );
    }

    #[test]
    fn stages_a_different_model() {
        let pressed = staged("gpt-other", vec![]);
        assert_eq!(
            staged_model_after_press(Some(staged("gpt-next", vec![])), pressed.clone(), false),
            Some(pressed)
        );
    }

    #[test]
    fn cannot_save_a_staged_model_after_sign_out_removes_it_from_the_catalog() {
        let pending = staged("gemini-native", vec![]);
        let mut catalog = ModelCatalog {
            instances: vec![instance("codex", Driver::Codex)],
            models: vec![model("codex", "gemini-native", "Gemini")],
        };
        assert!(can_save_staged_model(&catalog, &pending));
        catalog.models.clear();
        assert!(!can_save_staged_model(&catalog, &pending));
        catalog.models = vec![model("codex", "gemini-native", "Gemini")];
        catalog.instances[0].available = false;
        assert!(!can_save_staged_model(&catalog, &pending));
    }

    #[test]
    fn records_and_looks_up_options_per_instance_and_model() {
        let mut memory = ModelOptionMemory::new();
        let xhigh = vec![option("thinking", "xhigh")];
        let high = vec![option("thinking", "high")];
        remember_model_options(&mut memory, "codex", "gpt-5.3-codex", &xhigh);
        remember_model_options(&mut memory, "codex", "gpt-5.4", &high);
        assert_eq!(
            remembered_model_options(&memory, "codex", "gpt-5.3-codex"),
            Some(&xhigh)
        );
        assert_eq!(
            remembered_model_options(&memory, "codex", "gpt-5.4"),
            Some(&high)
        );
        assert_eq!(
            remembered_model_options(&memory, "pi", "gpt-5.3-codex"),
            None
        );
    }

    #[test]
    fn ignores_empty_option_sets_when_recording() {
        let mut memory = ModelOptionMemory::new();
        remember_model_options(&mut memory, "codex", "gpt-5.4", &[]);
        assert_eq!(remembered_model_options(&memory, "codex", "gpt-5.4"), None);
    }

    #[test]
    fn restores_the_remembered_options_over_descriptor_defaults() {
        let mut memory = ModelOptionMemory::new();
        let xhigh = vec![option("thinking", "xhigh")];
        remember_model_options(&mut memory, "codex", "gpt-5.3-codex", &xhigh);
        assert_eq!(
            with_remembered_model_options(
                &memory,
                "codex",
                "gpt-5.3-codex",
                vec![option("reasoningEffort", "low")]
            ),
            xhigh
        );
    }

    #[test]
    fn keeps_incoming_selections_when_nothing_is_remembered() {
        let incoming = vec![option("thinking", "high")];
        assert_eq!(
            with_remembered_model_options(
                &ModelOptionMemory::new(),
                "pi",
                "openai-codex/gpt-5.6-sol",
                incoming.clone()
            ),
            incoming
        );
    }
}

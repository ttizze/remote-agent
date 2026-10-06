//! The composer's draft controls: model and traits, runtime mode and the
//! Build/Plan toggle.
use crate::state::{Draft, ModelOption};
use agent_domain::{Driver, InteractionMode, RuntimeMode};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RuntimeModeChoice {
    pub mode: RuntimeMode,
    pub label: String,
    pub description: String,
}

const RUNTIME_MODES: [(RuntimeMode, &str, &str); 4] = [
    (
        RuntimeMode::ApprovalRequired,
        "Supervised",
        "Ask before commands and file changes.",
    ),
    (
        RuntimeMode::AutoAcceptEdits,
        "Auto-accept edits",
        "Auto-approve edits, ask before other actions.",
    ),
    (
        RuntimeMode::Auto,
        "Auto",
        "Supported providers approve routine actions; others still ask.",
    ),
    (
        RuntimeMode::FullAccess,
        "Full access",
        "Allow commands and edits without prompts.",
    ),
];

pub fn runtime_mode_choice(mode: RuntimeMode) -> RuntimeModeChoice {
    let (mode, label, description) = RUNTIME_MODES
        .into_iter()
        .find(|(candidate, ..)| *candidate == mode)
        .unwrap_or(RUNTIME_MODES[0]);
    RuntimeModeChoice {
        mode,
        label: label.into(),
        description: description.into(),
    }
}

/// Every mode when the provider advertises none, which keeps the control
/// usable when decoding dropped unknown modes.
pub fn runtime_mode_choices(supported: &[RuntimeMode]) -> Vec<RuntimeModeChoice> {
    RUNTIME_MODES
        .into_iter()
        .filter(|(mode, ..)| supported.is_empty() || supported.contains(mode))
        .map(|(mode, ..)| runtime_mode_choice(mode))
        .collect()
}

/// A mode the provider no longer offers shows as its first offered mode until
/// the user chooses; the stored mode is not changed.
pub fn compatible_runtime_mode(mode: RuntimeMode, choices: &[RuntimeModeChoice]) -> RuntimeMode {
    if choices.iter().any(|choice| choice.mode == mode) {
        mode
    } else {
        choices.first().map_or(mode, |choice| choice.mode)
    }
}

/// Plan mode needs the setting and a known provider that does not opt out; a
/// restored Plan draft otherwise reads as the default mode.
pub fn resolve_composer_interaction_mode(
    plan_mode_enabled: bool,
    provider_known: bool,
    provider_shows_toggle: Option<bool>,
    mode: InteractionMode,
) -> (bool, InteractionMode) {
    let enabled = plan_mode_enabled && provider_known && provider_shows_toggle != Some(false);
    (
        enabled,
        if enabled {
            mode
        } else {
            InteractionMode::Default
        },
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InteractionModeToggle {
    pub mode: InteractionMode,
    pub label: String,
    pub tooltip: String,
    /// The mode a tap switches to.
    pub toggled: InteractionMode,
}

pub fn interaction_mode_toggle(mode: InteractionMode) -> InteractionModeToggle {
    match mode {
        InteractionMode::Plan => InteractionModeToggle {
            mode,
            label: "Plan".into(),
            tooltip: "Plan mode — click to return to normal build mode".into(),
            toggled: InteractionMode::Default,
        },
        InteractionMode::Default => InteractionModeToggle {
            mode,
            label: "Build".into(),
            tooltip: "Default mode — click to enter plan mode".into(),
            toggled: InteractionMode::Plan,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelControl {
    pub instance_id: String,
    pub driver: Driver,
    pub model: String,
    /// The catalog's name for the model, else its slug.
    pub label: String,
    pub options: Vec<ModelOption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerControls {
    pub model: Option<ModelControl>,
    pub runtime_mode: RuntimeModeChoice,
    pub runtime_mode_choices: Vec<RuntimeModeChoice>,
    pub interaction_mode: InteractionMode,
    /// `None` hides the Build/Plan toggle.
    pub interaction_toggle: Option<InteractionModeToggle>,
}

/// What the selected provider instance says about the draft's controls.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderControls {
    pub known: bool,
    pub supported_runtime_modes: Vec<RuntimeMode>,
    pub shows_interaction_toggle: Option<bool>,
    pub model_label: Option<String>,
}

pub fn composer_controls(
    draft: &Draft,
    provider: &ProviderControls,
    plan_mode_enabled: bool,
) -> ComposerControls {
    let choices = runtime_mode_choices(&provider.supported_runtime_modes);
    let (toggle_enabled, interaction_mode) = resolve_composer_interaction_mode(
        plan_mode_enabled,
        provider.known,
        provider.shows_interaction_toggle,
        draft.interaction_mode,
    );
    ComposerControls {
        model: (!draft.instance_id.is_empty() && !draft.model.is_empty()).then(|| ModelControl {
            instance_id: draft.instance_id.clone(),
            driver: draft.driver,
            model: draft.model.clone(),
            label: provider
                .model_label
                .clone()
                .unwrap_or_else(|| draft.model.clone()),
            options: draft.options.clone(),
        }),
        runtime_mode: runtime_mode_choice(compatible_runtime_mode(draft.runtime_mode, &choices)),
        runtime_mode_choices: choices,
        interaction_mode,
        interaction_toggle: toggle_enabled.then(|| interaction_mode_toggle(interaction_mode)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_controls_usable_when_forward_compatible_decoding_removes_every_advertised_mode() {
        assert_eq!(runtime_mode_choices(&[]).len(), 4);
    }

    #[test]
    fn offers_the_supported_modes_in_order_and_shows_an_unsupported_mode_as_the_first() {
        let choices =
            runtime_mode_choices(&[RuntimeMode::FullAccess, RuntimeMode::AutoAcceptEdits]);
        let labels: Vec<_> = choices.iter().map(|choice| choice.label.as_str()).collect();
        assert_eq!(labels, ["Auto-accept edits", "Full access"]);
        assert_eq!(
            compatible_runtime_mode(RuntimeMode::Auto, &choices),
            RuntimeMode::AutoAcceptEdits
        );
        assert_eq!(
            compatible_runtime_mode(RuntimeMode::FullAccess, &choices),
            RuntimeMode::FullAccess
        );
        assert_eq!(
            runtime_mode_choice(RuntimeMode::ApprovalRequired),
            RuntimeModeChoice {
                mode: RuntimeMode::ApprovalRequired,
                label: "Supervised".into(),
                description: "Ask before commands and file changes.".into(),
            }
        );
    }

    #[test]
    fn resets_a_restored_plan_draft_when_the_selected_instance_does_not_support_plan_mode() {
        assert_eq!(
            resolve_composer_interaction_mode(true, true, Some(false), InteractionMode::Plan),
            (false, InteractionMode::Default)
        );
    }

    #[test]
    fn keeps_legacy_plan_behavior_for_providers_that_omit_the_capability() {
        assert_eq!(
            resolve_composer_interaction_mode(true, true, None, InteractionMode::Plan),
            (true, InteractionMode::Plan)
        );
    }

    #[test]
    fn resets_a_restored_plan_draft_when_the_beta_setting_is_off() {
        assert_eq!(
            resolve_composer_interaction_mode(false, true, Some(true), InteractionMode::Plan),
            (false, InteractionMode::Default)
        );
    }

    #[test]
    fn disables_plan_mode_until_the_selected_provider_is_available() {
        assert_eq!(
            resolve_composer_interaction_mode(true, false, None, InteractionMode::Plan),
            (false, InteractionMode::Default)
        );
    }

    #[test]
    fn the_controls_show_the_draft_model_mode_and_toggle() {
        let draft = Draft {
            instance_id: "codex".into(),
            model: "gpt-5.6-sol".into(),
            options: vec![ModelOption {
                key: "reasoningEffort".into(),
                value: "high".into(),
            }],
            runtime_mode: RuntimeMode::Auto,
            interaction_mode: InteractionMode::Plan,
            ..Draft::default()
        };
        let provider = ProviderControls {
            known: true,
            supported_runtime_modes: vec![RuntimeMode::ApprovalRequired, RuntimeMode::FullAccess],
            shows_interaction_toggle: Some(true),
            model_label: Some("5.6 Sol".into()),
        };
        let controls = composer_controls(&draft, &provider, true);
        let model = controls.model.unwrap();
        assert_eq!((model.label.as_str(), model.options.len()), ("5.6 Sol", 1));
        assert_eq!(controls.runtime_mode.label, "Supervised");
        assert_eq!(controls.runtime_mode_choices.len(), 2);
        assert_eq!(
            controls.interaction_toggle,
            Some(InteractionModeToggle {
                mode: InteractionMode::Plan,
                label: "Plan".into(),
                tooltip: "Plan mode — click to return to normal build mode".into(),
                toggled: InteractionMode::Default,
            })
        );
        let hidden = composer_controls(&draft, &ProviderControls::default(), true);
        assert_eq!(hidden.interaction_mode, InteractionMode::Default);
        assert_eq!(hidden.interaction_toggle, None);
        assert_eq!(hidden.model.unwrap().label, "gpt-5.6-sol");
        assert_eq!(
            composer_controls(&Draft::default(), &provider, true).model,
            None
        );
        assert_eq!(
            interaction_mode_toggle(InteractionMode::Default).label,
            "Build"
        );
    }
}

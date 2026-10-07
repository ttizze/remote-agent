//! Model option descriptors (reasoning effort, service tier, fast mode, …):
//! what a model offers, its current values and the options a draft stores.
//!
//! Draft options are `key`/`value` strings; a toggle stores `"true"` or
//! `"false"`.
use super::CatalogModel;
use crate::{models::Model, provider::ProviderKind, state::ModelOption};
use agent_domain::Driver;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct OptionChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum OptionDescriptor {
    Select {
        id: String,
        label: String,
        choices: Vec<OptionChoice>,
        current: Option<String>,
        /// Choices requested in the prompt text rather than stored.
        prompt_injected: Vec<String>,
    },
    Toggle {
        id: String,
        label: String,
        current: Option<bool>,
    },
}
impl OptionDescriptor {
    pub fn id(&self) -> &str {
        match self {
            Self::Select { id, .. } | Self::Toggle { id, .. } => id,
        }
    }
    pub fn label(&self) -> &str {
        match self {
            Self::Select { label, .. } | Self::Toggle { label, .. } => label,
        }
    }
    fn default_choice(&self) -> Option<&str> {
        match self {
            Self::Select { choices, .. } => choices
                .iter()
                .find(|choice| choice.is_default)
                .map(|choice| choice.id.as_str()),
            Self::Toggle { .. } => None,
        }
    }
}

/// A descriptor's value: a select's choice or a toggle's state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum OptionValue {
    Choice { id: String },
    Flag { on: bool },
}
impl OptionValue {
    pub fn wire(&self) -> String {
        match self {
            Self::Choice { id } => id.clone(),
            Self::Flag { on } => on.to_string(),
        }
    }
}

fn raw_value<'a>(selections: &'a [ModelOption], id: &str) -> Option<&'a str> {
    selections
        .iter()
        .find(|selection| selection.key == id)
        .map(|selection| selection.value.as_str())
}

fn trimmed(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|value| !value.is_empty())
}

fn resolve_choice(descriptor: &OptionDescriptor, raw: Option<&str>) -> Option<String> {
    let OptionDescriptor::Select {
        choices,
        current,
        prompt_injected,
        ..
    } = descriptor
    else {
        return None;
    };
    let fallback = || {
        current
            .clone()
            .or_else(|| descriptor.default_choice().map(Into::into))
    };
    let Some(value) = trimmed(raw) else {
        return fallback();
    };
    if choices.is_empty() {
        return Some(value.into());
    }
    let offered = choices.iter().any(|choice| choice.id == value);
    if offered && prompt_injected.iter().any(|injected| injected == value) {
        return descriptor.default_choice().map(Into::into);
    }
    if offered {
        return Some(value.into());
    }
    fallback()
}

/// The model's descriptors with the draft's stored values applied; values a
/// model does not offer fall back to its current or default choice.
pub fn option_descriptors(
    capabilities: &[OptionDescriptor],
    selections: &[ModelOption],
) -> Vec<OptionDescriptor> {
    capabilities
        .iter()
        .map(|descriptor| {
            let raw = raw_value(selections, descriptor.id());
            match descriptor {
                OptionDescriptor::Toggle { id, label, current } => OptionDescriptor::Toggle {
                    id: id.clone(),
                    label: label.clone(),
                    current: match raw {
                        Some("true") => Some(true),
                        Some("false") => Some(false),
                        _ => *current,
                    },
                },
                OptionDescriptor::Select {
                    id,
                    label,
                    choices,
                    current,
                    prompt_injected,
                } => OptionDescriptor::Select {
                    id: id.clone(),
                    label: label.clone(),
                    choices: choices.clone(),
                    current: resolve_choice(descriptor, raw.or(current.as_deref())),
                    prompt_injected: prompt_injected.clone(),
                },
            }
        })
        .collect()
}

pub fn option_current_value(descriptor: &OptionDescriptor) -> Option<OptionValue> {
    match descriptor {
        OptionDescriptor::Toggle { current, .. } => current.map(|on| OptionValue::Flag { on }),
        OptionDescriptor::Select { current, .. } => current
            .as_deref()
            .filter(|current| !current.is_empty())
            .or_else(|| descriptor.default_choice())
            .map(|id| OptionValue::Choice { id: id.into() }),
    }
}

/// "On"/"Off" for a toggle, the current choice's label for a select.
pub fn option_current_label(descriptor: &OptionDescriptor) -> Option<String> {
    match (descriptor, option_current_value(descriptor)) {
        (OptionDescriptor::Toggle { .. }, Some(OptionValue::Flag { on })) => {
            Some(if on { "On" } else { "Off" }.into())
        }
        (OptionDescriptor::Select { choices, .. }, Some(OptionValue::Choice { id })) => choices
            .iter()
            .find(|choice| choice.id == id)
            .map(|choice| choice.label.clone()),
        _ => None,
    }
}

/// Every descriptor's current value, as the draft stores them.
pub fn selections_from_descriptors(descriptors: &[OptionDescriptor]) -> Vec<ModelOption> {
    descriptors
        .iter()
        .filter_map(|descriptor| {
            option_current_value(descriptor).map(|value| ModelOption {
                key: descriptor.id().into(),
                value: value.wire(),
            })
        })
        .collect()
}

/// The normalized values of only the options the user chose, for dispatch.
pub fn explicit_selections_from_descriptors(
    descriptors: &[OptionDescriptor],
    selections: &[ModelOption],
) -> Vec<ModelOption> {
    selections_from_descriptors(descriptors)
        .into_iter()
        .filter(|normalized| {
            selections
                .iter()
                .any(|selection| selection.key == normalized.key)
        })
        .collect()
}

/// Sets one descriptor's value when its kind matches.
pub fn replace_descriptor_current_value(
    descriptors: &[OptionDescriptor],
    descriptor_id: &str,
    value: &OptionValue,
) -> Vec<OptionDescriptor> {
    descriptors
        .iter()
        .map(|descriptor| match (descriptor, value) {
            (OptionDescriptor::Toggle { id, label, .. }, OptionValue::Flag { on })
                if id == descriptor_id =>
            {
                OptionDescriptor::Toggle {
                    id: id.clone(),
                    label: label.clone(),
                    current: Some(*on),
                }
            }
            (
                OptionDescriptor::Select {
                    id,
                    label,
                    choices,
                    prompt_injected,
                    ..
                },
                OptionValue::Choice { id: choice },
            ) if id == descriptor_id => OptionDescriptor::Select {
                id: id.clone(),
                label: label.clone(),
                choices: choices.clone(),
                current: Some(choice.clone()),
                prompt_injected: prompt_injected.clone(),
            },
            _ => descriptor.clone(),
        })
        .collect()
}

/// Applies one change and returns every option to store, or `None` when the
/// model does not offer that option or choice.
pub fn apply_option_selection(
    descriptors: &[OptionDescriptor],
    descriptor_id: &str,
    value: &OptionValue,
) -> Option<Vec<ModelOption>> {
    let descriptor = descriptors.iter().find(|d| d.id() == descriptor_id)?;
    let valid = match (descriptor, value) {
        (OptionDescriptor::Toggle { .. }, OptionValue::Flag { .. }) => true,
        (OptionDescriptor::Select { choices, .. }, OptionValue::Choice { id }) => {
            choices.iter().any(|choice| &choice.id == id)
        }
        _ => false,
    };
    valid.then(|| {
        selections_from_descriptors(&replace_descriptor_current_value(
            descriptors,
            descriptor_id,
            value,
        ))
    })
}

fn effort_label(effort: &str) -> String {
    match effort {
        "none" => "None",
        "minimal" => "Minimal",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        "max" => "Max",
        "ultra" => "Ultra",
        "ultracode" => "Ultracode",
        "ultrathink" => "Ultrathink",
        other => other,
    }
    .into()
}

const DEFAULT_SERVICE_TIER: &str = "default";

/// Compares Codex model families without changing the routing slug.
pub fn codex_model_family(slug: &str) -> &str {
    if slug.starts_with("openai.gpt-") {
        &slug["openai.".len()..]
    } else {
        slug
    }
}

/// "gpt-5.3-codex-spark" reads "GPT-5.3-Codex-Spark".
pub fn format_codex_model_name(name: &str) -> String {
    let name = match name.get(..3) {
        Some(head) if head.eq_ignore_ascii_case("gpt") => format!("GPT{}", &name[3..]),
        _ => name.into(),
    };
    let mut out = String::with_capacity(name.len());
    let mut after_dash = false;
    for character in name.chars() {
        if after_dash && character.is_ascii_lowercase() {
            out.push(character.to_ascii_uppercase());
        } else {
            out.push(character);
        }
        after_dash = character == '-';
    }
    out
}

/// Reasoning and service-tier descriptors from a Codex catalogue entry.
pub fn codex_model_descriptors(model: &Model) -> Vec<OptionDescriptor> {
    let default_effort = if codex_model_family(&model.model.id) == "gpt-6-astra" {
        "medium"
    } else {
        model.default_reasoning_effort.as_str()
    };
    let efforts: Vec<OptionChoice> = model
        .supported_reasoning_efforts
        .iter()
        .map(|effort| OptionChoice {
            id: effort.reasoning_effort.clone(),
            label: effort_label(&effort.reasoning_effort),
            description: None,
            is_default: effort.reasoning_effort == default_effort,
        })
        .collect();
    let mut descriptors = vec![];
    if !efforts.is_empty() {
        descriptors.push(OptionDescriptor::Select {
            id: "reasoningEffort".into(),
            label: "Reasoning".into(),
            current: efforts
                .iter()
                .find(|choice| choice.is_default)
                .map(|choice| choice.id.clone()),
            choices: efforts,
            prompt_injected: vec![],
        });
    }
    let tiers = model.service_tiers.as_deref().unwrap_or_default();
    if !tiers.is_empty() {
        let default_tier = model
            .default_service_tier
            .as_deref()
            .filter(|tier| tiers.iter().any(|candidate| candidate.id == *tier))
            .unwrap_or(DEFAULT_SERVICE_TIER);
        let choices = std::iter::once(OptionChoice {
            id: DEFAULT_SERVICE_TIER.into(),
            label: "Standard".into(),
            description: None,
            is_default: default_tier == DEFAULT_SERVICE_TIER,
        })
        .chain(tiers.iter().map(|tier| OptionChoice {
            id: tier.id.clone(),
            label: tier.name.clone().unwrap_or_else(|| tier.id.clone()),
            description: (tier.id == "ultrafast").then(|| "Even faster, more expensive".into()),
            is_default: tier.id == default_tier,
        }))
        .collect();
        descriptors.push(OptionDescriptor::Select {
            id: "serviceTier".into(),
            label: "Service Tier".into(),
            choices,
            current: Some(default_tier.into()),
            prompt_injected: vec![],
        });
    }
    descriptors
}

/// The effort descriptor from a Claude catalogue entry. `ultrathink` is
/// requested in the prompt.
pub fn claude_model_descriptors(model: &Model) -> Vec<OptionDescriptor> {
    let choices: Vec<OptionChoice> = model
        .supported_reasoning_efforts
        .iter()
        .map(|effort| OptionChoice {
            id: effort.reasoning_effort.clone(),
            label: effort_label(&effort.reasoning_effort),
            description: (effort.reasoning_effort == "ultracode")
                .then(|| "xhigh effort plus multi-agent workflow orchestration".into()),
            is_default: effort.reasoning_effort == model.default_reasoning_effort,
        })
        .collect();
    if choices.is_empty() {
        return vec![];
    }
    let prompt_injected = choices
        .iter()
        .filter(|choice| choice.id == "ultrathink")
        .map(|choice| choice.id.clone())
        .collect();
    vec![OptionDescriptor::Select {
        id: "effort".into(),
        label: "Reasoning".into(),
        choices,
        current: None,
        prompt_injected,
    }]
}

pub fn model_descriptors(model: &Model) -> Vec<OptionDescriptor> {
    match model.model.provider {
        ProviderKind::Codex => codex_model_descriptors(model),
        ProviderKind::Claude => claude_model_descriptors(model),
    }
}

fn normalize_model_slug(model: &str, driver: Driver) -> &str {
    match driver {
        Driver::Codex => match model {
            "gpt-5-codex" | "5.4" => "gpt-5.4",
            "5.3" | "gpt-5.3" => "gpt-5.3-codex",
            "5.3-spark" | "gpt-5.3-spark" => "gpt-5.3-codex-spark",
            other => other,
        },
        Driver::Claude => model,
    }
}

/// The catalogue slug a typed or stored model names: its slug, its name, or
/// a known alias.
pub fn resolve_selectable_model<'a>(
    driver: Driver,
    value: &str,
    models: impl IntoIterator<Item = &'a CatalogModel> + Clone,
) -> Option<&'a CatalogModel> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let mut candidates = models.clone().into_iter();
    candidates
        .find(|model| model.slug == value)
        .or_else(|| {
            models
                .clone()
                .into_iter()
                .find(|model| model.name.to_lowercase() == value.to_lowercase())
        })
        .or_else(|| {
            let normalized = normalize_model_slug(value, driver);
            models.into_iter().find(|model| model.slug == normalized)
        })
}

/// The descriptors a model offers; none for a model the catalogue lacks.
pub fn model_capabilities<'a>(
    driver: Driver,
    models: impl IntoIterator<Item = &'a CatalogModel> + Clone,
    model: &str,
) -> Vec<OptionDescriptor> {
    resolve_selectable_model(driver, model, models)
        .map(|model| model.descriptors.clone())
        .unwrap_or_default()
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The prompt asks for ultrathink anywhere as a word.
pub fn is_ultrathink_prompt(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    lower.match_indices("ultrathink").any(|(start, word)| {
        let end = start + word.len();
        (start == 0 || !is_word(bytes[start - 1])) && (end == bytes.len() || !is_word(bytes[end]))
    })
}

/// `Ultrathink:` prefixes a prompt requesting that effort, except a slash
/// command, which the prefix would turn into prose.
pub fn apply_prompt_effort_prefix(text: &str, effort: Option<&str>) -> String {
    let trimmed = text.trim();
    let command = trimmed.strip_prefix('/').is_some_and(|rest| {
        let name = rest.split(char::is_whitespace).next().unwrap_or_default();
        !name.is_empty() && !name.contains('/')
    });
    if trimmed.is_empty()
        || effort != Some("ultrathink")
        || command
        || trimmed.starts_with("Ultrathink:")
    {
        trimmed.into()
    } else {
        format!("Ultrathink:\n{trimmed}")
    }
}

/// Removes a leading `Ultrathink:` and the whitespace after it.
pub fn strip_ultrathink_prefix(text: &str) -> String {
    const PREFIX: &str = "ultrathink:";
    match text.get(..PREFIX.len()) {
        Some(head) if head.eq_ignore_ascii_case(PREFIX) => text[PREFIX.len()..].trim_start().into(),
        _ => text.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ModelRef, ReasoningEffort, ServiceTier};

    pub fn choice(id: &str, label: &str, is_default: bool) -> OptionChoice {
        OptionChoice {
            id: id.into(),
            label: label.into(),
            description: None,
            is_default,
        }
    }
    fn option(key: &str, value: &str) -> ModelOption {
        ModelOption {
            key: key.into(),
            value: value.into(),
        }
    }
    fn codex_caps() -> Vec<OptionDescriptor> {
        vec![
            OptionDescriptor::Select {
                id: "reasoningEffort".into(),
                label: "Reasoning".into(),
                choices: vec![
                    choice("xhigh", "Extra High", false),
                    choice("high", "High", true),
                ],
                current: Some("high".into()),
                prompt_injected: vec![],
            },
            OptionDescriptor::Toggle {
                id: "fastMode".into(),
                label: "Fast Mode".into(),
                current: None,
            },
        ]
    }
    fn claude_effort(current: Option<&str>) -> OptionDescriptor {
        OptionDescriptor::Select {
            id: "effort".into(),
            label: "Reasoning".into(),
            choices: vec![
                choice("medium", "Medium", false),
                choice("high", "High", true),
                choice("ultrathink", "Ultrathink", false),
            ],
            current: current.map(Into::into),
            prompt_injected: vec!["ultrathink".into()],
        }
    }
    fn context_window(current: Option<&str>) -> OptionDescriptor {
        OptionDescriptor::Select {
            id: "contextWindow".into(),
            label: "Context Window".into(),
            choices: vec![choice("200k", "200k", false), choice("1m", "1M", true)],
            current: current.map(Into::into),
            prompt_injected: vec![],
        }
    }
    fn codex_model(tiers: Vec<ServiceTier>, default_tier: Option<&str>) -> Model {
        Model {
            id: "gpt-test".into(),
            model: ModelRef {
                provider: ProviderKind::Codex,
                id: "gpt-test".into(),
            },
            display_name: "GPT Test".into(),
            default_reasoning_effort: "super-high".into(),
            supported_reasoning_efforts: vec![ReasoningEffort {
                reasoning_effort: "super-high".into(),
            }],
            service_tiers: Some(tiers),
            default_service_tier: default_tier.map(Into::into),
            is_default: Some(true),
        }
    }
    fn tier(id: &str, name: &str) -> ServiceTier {
        ServiceTier {
            id: id.into(),
            name: Some(name.into()),
        }
    }

    #[test]
    fn keeps_the_codex_catalog_display_formatting() {
        assert_eq!(
            format_codex_model_name("gpt-5.3-codex-spark"),
            "GPT-5.3-Codex-Spark"
        );
        assert_eq!(format_codex_model_name("GPT Test"), "GPT Test");
    }

    #[test]
    fn applies_selection_values_to_capability_descriptors() {
        assert_eq!(
            option_descriptors(
                &[claude_effort(Some("high")), context_window(Some("1m"))],
                &[option("effort", "medium"), option("contextWindow", "200k")],
            ),
            vec![claude_effort(Some("medium")), context_window(Some("200k"))]
        );
    }

    #[test]
    fn a_stored_prompt_injected_value_falls_back_to_the_default_choice() {
        assert_eq!(
            option_descriptors(&[claude_effort(None)], &[option("effort", "ultrathink")]),
            vec![claude_effort(Some("high"))]
        );
        assert_eq!(
            option_descriptors(&[claude_effort(None)], &[option("effort", "unknown")]),
            vec![claude_effort(Some("high"))]
        );
    }

    #[test]
    fn builds_wire_format_option_selections_from_descriptors() {
        let descriptors = option_descriptors(
            &codex_caps(),
            &[
                option("reasoningEffort", "high"),
                option("fastMode", "true"),
            ],
        );
        assert_eq!(
            selections_from_descriptors(&descriptors),
            vec![
                option("reasoningEffort", "high"),
                option("fastMode", "true")
            ]
        );
    }

    #[test]
    fn builds_dispatch_options_only_from_explicit_selections() {
        let descriptors = option_descriptors(&codex_caps(), &[option("fastMode", "true")]);
        assert_eq!(explicit_selections_from_descriptors(&descriptors, &[]), []);
        assert_eq!(
            explicit_selections_from_descriptors(&descriptors, &[option("fastMode", "true")]),
            vec![option("fastMode", "true")]
        );
    }

    #[test]
    fn keeps_slash_commands_intact_when_ultrathink_is_selected() {
        let ultrathink = Some("ultrathink");
        assert_eq!(
            apply_prompt_effort_prefix("/compact", ultrathink),
            "/compact"
        );
        assert_eq!(
            apply_prompt_effort_prefix(" /compact keep recent errors ", ultrathink),
            "/compact keep recent errors"
        );
        assert_eq!(
            apply_prompt_effort_prefix(" /review src/model.ts ", ultrathink),
            "/review src/model.ts"
        );
        assert_eq!(
            apply_prompt_effort_prefix("/security-review", ultrathink),
            "/security-review"
        );
        assert_eq!(
            apply_prompt_effort_prefix("/plugin:skill run", ultrathink),
            "/plugin:skill run"
        );
        assert_eq!(
            apply_prompt_effort_prefix("/deploy.prod to staging", ultrathink),
            "/deploy.prod to staging"
        );
    }

    #[test]
    fn still_adds_the_ultrathink_prefix_to_ordinary_prompts() {
        let ultrathink = Some("ultrathink");
        assert_eq!(
            apply_prompt_effort_prefix("Investigate this failure", ultrathink),
            "Ultrathink:\nInvestigate this failure"
        );
        assert_eq!(
            apply_prompt_effort_prefix("/home/theo/app.ts crashed on load", ultrathink),
            "Ultrathink:\n/home/theo/app.ts crashed on load"
        );
    }

    #[test]
    fn ultrathink_is_detected_as_a_whole_word_in_any_case() {
        assert!(is_ultrathink_prompt("please ULTRATHINK about it"));
        assert!(is_ultrathink_prompt("Ultrathink:\nfix"));
        assert!(!is_ultrathink_prompt("ultrathinking"));
        assert!(!is_ultrathink_prompt("my_ultrathink"));
        assert_eq!(strip_ultrathink_prefix("ultrathink:  fix"), "fix");
        assert_eq!(strip_ultrathink_prefix("fix ultrathink"), "fix ultrathink");
    }

    #[test]
    fn maps_current_codex_model_capability_fields() {
        let model = codex_model(
            vec![tier("priority", "Fast"), tier("flex", "Flex")],
            Some("flex"),
        );
        assert_eq!(
            codex_model_descriptors(&model),
            vec![
                OptionDescriptor::Select {
                    id: "reasoningEffort".into(),
                    label: "Reasoning".into(),
                    choices: vec![choice("super-high", "super-high", true)],
                    current: Some("super-high".into()),
                    prompt_injected: vec![],
                },
                OptionDescriptor::Select {
                    id: "serviceTier".into(),
                    label: "Service Tier".into(),
                    choices: vec![
                        choice("default", "Standard", false),
                        choice("priority", "Fast", false),
                        choice("flex", "Flex", true),
                    ],
                    current: Some("flex".into()),
                    prompt_injected: vec![],
                },
            ]
        );
    }

    #[test]
    fn uses_standard_routing_when_the_catalog_has_no_default_service_tier() {
        let mut model = codex_model(
            vec![tier("priority", "Fast"), tier("ultrafast", "Ultrafast")],
            None,
        );
        model.default_reasoning_effort = "medium".into();
        model.supported_reasoning_efforts = vec![];
        assert_eq!(
            codex_model_descriptors(&model),
            vec![OptionDescriptor::Select {
                id: "serviceTier".into(),
                label: "Service Tier".into(),
                choices: vec![
                    choice("default", "Standard", true),
                    choice("priority", "Fast", false),
                    OptionChoice {
                        description: Some("Even faster, more expensive".into()),
                        ..choice("ultrafast", "Ultrafast", false)
                    },
                ],
                current: Some("default".into()),
                prompt_injected: vec![],
            }]
        );
    }

    #[test]
    fn the_flagship_codex_family_defaults_to_medium_reasoning() {
        let mut model = codex_model(vec![], None);
        model.model.id = "openai.gpt-6-astra".into();
        model.default_reasoning_effort = "high".into();
        model.supported_reasoning_efforts = ["medium", "high"]
            .map(|effort| ReasoningEffort {
                reasoning_effort: effort.into(),
            })
            .to_vec();
        let descriptors = codex_model_descriptors(&model);
        assert_eq!(descriptors.len(), 1);
        assert_eq!(
            option_current_value(&descriptors[0]),
            Some(OptionValue::Choice {
                id: "medium".into()
            })
        );
    }

    #[test]
    fn claude_efforts_are_labelled_and_ultrathink_goes_in_the_prompt() {
        let model = Model {
            model: ModelRef {
                provider: ProviderKind::Claude,
                id: "claude-opus-5".into(),
            },
            supported_reasoning_efforts: ["high", "xhigh", "ultracode", "ultrathink"]
                .map(|effort| ReasoningEffort {
                    reasoning_effort: effort.into(),
                })
                .to_vec(),
            default_reasoning_effort: "high".into(),
            service_tiers: Some(vec![]),
            ..codex_model(vec![], None)
        };
        let descriptors = model_descriptors(&model);
        let [
            OptionDescriptor::Select {
                id,
                choices,
                prompt_injected,
                current: None,
                ..
            },
        ] = descriptors.as_slice()
        else {
            panic!("one effort select: {descriptors:?}");
        };
        assert_eq!(id, "effort");
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            ["High", "Extra High", "Ultracode", "Ultrathink"]
        );
        assert!(choices[0].is_default);
        assert!(choices[2].description.is_some());
        assert_eq!(prompt_injected, &["ultrathink"]);
    }

    #[test]
    fn updates_generic_select_options_without_knowing_provider_specific_ids() {
        let descriptors = option_descriptors(
            &[
                OptionDescriptor::Select {
                    id: "reasoningEffort".into(),
                    label: "Reasoning".into(),
                    choices: vec![
                        choice("medium", "Medium", true),
                        choice("high", "High", false),
                    ],
                    current: Some("medium".into()),
                    prompt_injected: vec![],
                },
                OptionDescriptor::Select {
                    id: "serviceTier".into(),
                    label: "Service Tier".into(),
                    choices: vec![
                        choice("default", "Standard", true),
                        choice("priority", "Fast", false),
                    ],
                    current: Some("default".into()),
                    prompt_injected: vec![],
                },
            ],
            &[],
        );
        let choose = |id: &str, value: &str| {
            apply_option_selection(&descriptors, id, &OptionValue::Choice { id: value.into() })
        };
        assert_eq!(
            choose("serviceTier", "priority"),
            Some(vec![
                option("reasoningEffort", "medium"),
                option("serviceTier", "priority")
            ])
        );
        assert_eq!(choose("serviceTier", "turbo"), None);
        assert_eq!(choose("unknown", "high"), None);
    }

    #[test]
    fn updates_generic_boolean_options() {
        let descriptors = option_descriptors(
            &[OptionDescriptor::Toggle {
                id: "fastMode".into(),
                label: "Fast Mode".into(),
                current: None,
            }],
            &[],
        );
        assert_eq!(
            apply_option_selection(&descriptors, "fastMode", &OptionValue::Flag { on: true }),
            Some(vec![option("fastMode", "true")])
        );
    }

    #[test]
    fn prefers_an_exact_slug_and_returns_no_capabilities_for_an_unknown_one() {
        let toggle = |id: &str| {
            vec![OptionDescriptor::Toggle {
                id: id.into(),
                label: id.into(),
                current: None,
            }]
        };
        let models = [
            CatalogModel {
                descriptors: toggle("built-in-option"),
                ..super::super::fixtures::model("claude", "synthetic-model", "custom-model")
            },
            CatalogModel {
                descriptors: toggle("custom-option"),
                ..super::super::fixtures::model("claude", "custom-model", "Custom")
            },
        ];
        assert_eq!(
            model_capabilities(Driver::Claude, &models, " custom-model "),
            toggle("custom-option")
        );
        assert_eq!(
            model_capabilities(Driver::Claude, &models, "unknown-model"),
            []
        );
        assert_eq!(
            resolve_selectable_model(
                Driver::Codex,
                "5.4",
                &[super::super::fixtures::model("codex", "gpt-5.4", "GPT-5.4")]
            )
            .map(|model| model.slug.as_str()),
            Some("gpt-5.4")
        );
    }
}

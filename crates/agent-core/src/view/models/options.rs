//! Model option descriptors (reasoning effort, service tier, fast mode, …):
//! what a model offers, its current values and the options a draft stores.
//!
//! Draft options are `key`/`value` strings; a toggle stores `"true"` or
//! `"false"`.
use super::CatalogModel;
use crate::state::ModelOption;
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

impl From<&agent_domain::OptionDescriptor> for OptionDescriptor {
    fn from(descriptor: &agent_domain::OptionDescriptor) -> Self {
        match descriptor {
            agent_domain::OptionDescriptor::Select(select) => Self::Select {
                id: select.id.clone(),
                label: select.label.clone(),
                choices: select
                    .options
                    .iter()
                    .map(|choice| OptionChoice {
                        id: choice.id.clone(),
                        label: choice.label.clone(),
                        description: choice.description.clone(),
                        is_default: choice.is_default,
                    })
                    .collect(),
                current: select.current_value.clone(),
                prompt_injected: select.prompt_injected_values.clone(),
            },
            agent_domain::OptionDescriptor::Boolean(boolean) => Self::Toggle {
                id: boolean.id.clone(),
                label: boolean.label.clone(),
                current: boolean.current_value,
            },
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

/// Normalizes selections against the capabilities of the model that will
/// receive them. Unknown keys are dropped; known choices and flags use the
/// descriptor's validation and fallback rules. This keeps a source draft's
/// choices only when the selected target model actually offers them.
pub fn normalize_model_options(
    capabilities: &[agent_domain::OptionDescriptor],
    selections: &[ModelOption],
) -> Vec<ModelOption> {
    let descriptors: Vec<_> = capabilities.iter().map(OptionDescriptor::from).collect();
    explicit_selections_from_descriptors(&option_descriptors(&descriptors, selections), selections)
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

/// The catalogue slug a known alias stands for.
pub(crate) fn normalize_model_slug(model: &str, driver: Driver) -> &str {
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

/// The catalogue slug a typed or stored model names: its slug, its name, one
/// of its aliases, or a known alias.
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
            models.clone().into_iter().find(|model| {
                model
                    .aliases
                    .iter()
                    .any(|alias| alias.to_lowercase() == value.to_lowercase())
            })
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

    #[test]
    fn normalizes_selections_for_a_fallback_model() {
        let target = vec![
            agent_domain::OptionDescriptor::Select(agent_domain::SelectOption {
                id: "effort".into(),
                label: "Effort".into(),
                description: None,
                options: vec![
                    agent_domain::OptionChoice {
                        id: "high".into(),
                        label: "High".into(),
                        description: None,
                        is_default: true,
                    },
                    agent_domain::OptionChoice {
                        id: "low".into(),
                        label: "Low".into(),
                        description: None,
                        is_default: false,
                    },
                ],
                current_value: Some("high".into()),
                prompt_injected_values: vec![],
            }),
            agent_domain::OptionDescriptor::Boolean(agent_domain::BooleanOption {
                id: "fast".into(),
                label: "Fast".into(),
                description: None,
                current_value: Some(false),
            }),
        ];
        assert_eq!(
            normalize_model_options(
                &target,
                &[option("effort", "invalid"), option("old", "value")],
            ),
            vec![option("effort", "high")],
        );
        assert_eq!(
            normalize_model_options(&target, &[option("fast", "true")]),
            vec![option("fast", "true")],
        );
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
        let aliased = CatalogModel {
            aliases: vec!["opus".into()],
            ..super::super::fixtures::model("claude", "claude-opus-5", "Claude Opus 5")
        };
        assert_eq!(
            resolve_selectable_model(Driver::Claude, "Opus", [&aliased])
                .map(|model| model.slug.as_str()),
            Some("claude-opus-5")
        );
    }

    #[test]
    fn host_descriptors_keep_their_choices_current_values_and_prompt_values() {
        let host = agent_domain::OptionDescriptor::Select(agent_domain::SelectOption {
            id: "effort".into(),
            label: "Reasoning".into(),
            description: None,
            options: vec![
                agent_domain::OptionChoice {
                    id: "high".into(),
                    label: "High".into(),
                    description: None,
                    is_default: true,
                },
                agent_domain::OptionChoice {
                    id: "ultrathink".into(),
                    label: "Ultrathink".into(),
                    description: None,
                    is_default: false,
                },
            ],
            current_value: None,
            prompt_injected_values: vec!["ultrathink".into()],
        });
        assert_eq!(
            OptionDescriptor::from(&host),
            OptionDescriptor::Select {
                id: "effort".into(),
                label: "Reasoning".into(),
                choices: vec![
                    choice("high", "High", true),
                    choice("ultrathink", "Ultrathink", false)
                ],
                current: None,
                prompt_injected: vec!["ultrathink".into()],
            }
        );
        let toggle = agent_domain::OptionDescriptor::Boolean(agent_domain::BooleanOption {
            id: "fastMode".into(),
            label: "Fast Mode".into(),
            description: None,
            current_value: Some(false),
        });
        assert_eq!(
            OptionDescriptor::from(&toggle),
            OptionDescriptor::Toggle {
                id: "fastMode".into(),
                label: "Fast Mode".into(),
                current: Some(false),
            }
        );
    }
}

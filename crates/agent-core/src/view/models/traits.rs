//! The traits control beside the model picker: effort, service tier, fast
//! mode, context window and thinking, with the label its trigger shows.
use super::{
    ModelCatalog, catalog,
    options::{
        OptionChoice, OptionDescriptor, OptionValue, apply_prompt_effort_prefix,
        is_ultrathink_prompt, model_capabilities, option_current_label, option_current_value,
        option_descriptors, replace_descriptor_current_value, selections_from_descriptors,
        strip_ultrathink_prefix,
    },
};
use crate::state::{Draft, ModelOption, Snapshot};
use agent_domain::Driver;

/// One bolt for fast mode, two for Codex Ultrafast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SpeedIcon {
    Fast,
    Ultrafast,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TraitsTrigger {
    pub label: String,
    pub speed_icon: Option<SpeedIcon>,
}

/// The trigger label joins every trait's current label with " · ". Fast mode
/// and Codex speed tiers show as a bolt instead, or as text when speed is the
/// only trait.
pub fn traits_trigger_display(
    driver: Driver,
    descriptors: &[OptionDescriptor],
    primary_select_id: Option<&str>,
    ultrathink_prompt_controlled: bool,
) -> TraitsTrigger {
    let mut fallback: Option<String> = None;
    let mut speed_icon = None;
    let mut labels: Vec<String> = vec![];
    for descriptor in descriptors {
        if let OptionDescriptor::Toggle { id, current, .. } = descriptor
            && id == "fastMode"
        {
            speed_icon = (*current == Some(true)).then_some(SpeedIcon::Fast);
            fallback = Some(
                if speed_icon.is_some() {
                    "Fast"
                } else {
                    "Normal"
                }
                .into(),
            );
            continue;
        }
        if driver == Driver::Codex
            && let OptionDescriptor::Select { id, choices, .. } = descriptor
            && id == "serviceTier"
        {
            let current = match option_current_value(descriptor) {
                Some(OptionValue::Choice { id }) => Some(id),
                _ => None,
            };
            let tier = |label: &str| choices.iter().find(|choice| choice.label == label);
            let is = |choice: Option<&OptionChoice>| {
                choice.is_some_and(|choice| current.as_deref() == Some(choice.id.as_str()))
            };
            let (fast, ultrafast) = (tier("Fast"), tier("Ultrafast"));
            if ((fast.is_some() || ultrafast.is_some()) && current.as_deref() == Some("default"))
                || is(fast)
                || is(ultrafast)
            {
                speed_icon = if is(ultrafast) {
                    Some(SpeedIcon::Ultrafast)
                } else if is(fast) {
                    Some(SpeedIcon::Fast)
                } else {
                    None
                };
                fallback = Some(
                    choices
                        .iter()
                        .find(|choice| Some(choice.id.as_str()) == current.as_deref())
                        .map_or_else(|| "Normal".into(), |choice| choice.label.clone()),
                );
                continue;
            }
        }
        let label = if ultrathink_prompt_controlled && Some(descriptor.id()) == primary_select_id {
            Some("Ultrathink".into())
        } else if let OptionDescriptor::Toggle { label, current, .. } = descriptor {
            Some(format!(
                "{label} {}",
                if *current == Some(true) { "On" } else { "Off" }
            ))
        } else {
            option_current_label(descriptor)
        };
        if let Some(label) = label.filter(|label| !label.is_empty()) {
            labels.push(label);
        }
    }
    match fallback {
        Some(fallback) if labels.is_empty() => TraitsTrigger {
            label: fallback,
            speed_icon: None,
        },
        _ => TraitsTrigger {
            label: labels.join(" · "),
            speed_icon,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TraitControl {
    Select {
        id: String,
        label: String,
        choices: Vec<OptionChoice>,
        /// Empty when nothing resolves.
        selected: String,
        /// Explains why the choice is locked.
        note: Option<String>,
        disabled: bool,
    },
    Toggle {
        id: String,
        label: String,
        on: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TraitsView {
    /// The model offers a trait to change.
    pub visible: bool,
    pub trigger: TraitsTrigger,
    pub accessible_label: String,
    pub controls: Vec<TraitControl>,
}

struct Selected {
    descriptors: Vec<OptionDescriptor>,
    primary: Option<String>,
    prompt_controlled: bool,
    in_body: bool,
}

fn selected_traits(
    catalog: &ModelCatalog,
    draft: &Draft,
    allow_prompt_injected_effort: bool,
) -> (Driver, Selected) {
    let driver = catalog
        .instance(&draft.instance_id)
        .map_or(draft.driver, |instance| instance.driver);
    let capabilities =
        model_capabilities(driver, catalog.models_of(&draft.instance_id), &draft.model);
    let descriptors = option_descriptors(&capabilities, &draft.options);
    let primary = descriptors
        .iter()
        .find(|descriptor| matches!(descriptor, OptionDescriptor::Select { .. }));
    let prompt_controlled = allow_prompt_injected_effort
        && matches!(primary, Some(OptionDescriptor::Select { prompt_injected, .. }) if !prompt_injected.is_empty())
        && is_ultrathink_prompt(&draft.text);
    let in_body = prompt_controlled && is_ultrathink_prompt(&strip_ultrathink_prefix(&draft.text));
    let primary = primary.map(|descriptor| descriptor.id().to_owned());
    (
        driver,
        Selected {
            descriptors,
            primary,
            prompt_controlled,
            in_body,
        },
    )
}

/// The traits control for a draft. `allow_prompt_injected_effort` is false
/// where there is no prompt, such as a default-model setting.
pub fn build_traits(
    catalog: &ModelCatalog,
    draft: &Draft,
    allow_prompt_injected_effort: bool,
) -> TraitsView {
    let (driver, selected) = selected_traits(catalog, draft, allow_prompt_injected_effort);
    let primary = selected.primary.as_deref();
    let visible = primary.is_some()
        || selected.descriptors.iter().any(|descriptor| {
            matches!(descriptor, OptionDescriptor::Toggle { id, .. } if id == "fastMode" || id == "thinking")
        });
    let trigger = traits_trigger_display(
        driver,
        &selected.descriptors,
        primary,
        selected.prompt_controlled,
    );
    let accessible_label = match trigger.speed_icon {
        Some(SpeedIcon::Ultrafast) => format!("{}, Ultrafast mode on", trigger.label),
        Some(SpeedIcon::Fast) => format!("{}, Fast mode on", trigger.label),
        None => trigger.label.clone(),
    };
    let selects = selected.descriptors.iter().filter_map(|descriptor| match descriptor {
        OptionDescriptor::Select { id, label, choices, .. } => {
            let is_primary = Some(id.as_str()) == primary;
            let locked = selected.in_body && is_primary;
            Some(TraitControl::Select {
                id: id.clone(),
                label: label.clone(),
                choices: choices.clone(),
                selected: if selected.prompt_controlled && is_primary {
                    "ultrathink".into()
                } else {
                    match option_current_value(descriptor) {
                        Some(OptionValue::Choice { id }) => id,
                        _ => String::new(),
                    }
                },
                note: locked.then(|| {
                    "Your prompt contains \"ultrathink\" in the text. Remove it to change this option."
                        .into()
                }),
                disabled: locked,
            })
        }
        OptionDescriptor::Toggle { .. } => None,
    });
    let toggles = selected
        .descriptors
        .iter()
        .filter_map(|descriptor| match descriptor {
            OptionDescriptor::Toggle { id, label, current } => Some(TraitControl::Toggle {
                id: id.clone(),
                label: label.clone(),
                on: *current == Some(true),
            }),
            OptionDescriptor::Select { .. } => None,
        });
    TraitsView {
        visible,
        trigger,
        accessible_label,
        controls: selects.chain(toggles).collect(),
    }
}

pub fn traits(
    snapshot: &Snapshot,
    draft: &Draft,
    allow_prompt_injected_effort: bool,
) -> TraitsView {
    build_traits(&catalog(snapshot), draft, allow_prompt_injected_effort)
}

/// What picking a trait changes: the draft's options, its text, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TraitChange {
    pub options: Option<Vec<ModelOption>>,
    pub text: Option<String>,
}

/// Picks a select trait's choice. A prompt-injected choice (ultrathink)
/// prefixes the prompt instead; another effort removes that prefix. `None`
/// when the choice cannot change.
pub fn select_trait(
    catalog: &ModelCatalog,
    draft: &Draft,
    allow_prompt_injected_effort: bool,
    descriptor_id: &str,
    choice: &str,
) -> Option<TraitChange> {
    if choice.is_empty() {
        return None;
    }
    let (_, selected) = selected_traits(catalog, draft, allow_prompt_injected_effort);
    let OptionDescriptor::Select {
        prompt_injected, ..
    } = selected
        .descriptors
        .iter()
        .find(|descriptor| descriptor.id() == descriptor_id)?
    else {
        return None;
    };
    if prompt_injected.iter().any(|value| value == choice) {
        let text = if draft.text.trim().is_empty() {
            "Ultrathink:\n".into()
        } else {
            apply_prompt_effort_prefix(&draft.text, Some("ultrathink"))
        };
        return Some(TraitChange {
            options: None,
            text: Some(text),
        });
    }
    let is_primary = selected.primary.as_deref() == Some(descriptor_id);
    if selected.in_body && is_primary {
        return None;
    }
    Some(TraitChange {
        options: Some(selections_from_descriptors(
            &replace_descriptor_current_value(
                &selected.descriptors,
                descriptor_id,
                &OptionValue::Choice { id: choice.into() },
            ),
        )),
        text: (selected.prompt_controlled && is_primary)
            .then(|| strip_ultrathink_prefix(&draft.text)),
    })
}

/// Turns a toggle trait on or off.
pub fn toggle_trait(
    catalog: &ModelCatalog,
    draft: &Draft,
    descriptor_id: &str,
    on: bool,
) -> Option<TraitChange> {
    let (_, selected) = selected_traits(catalog, draft, false);
    selected
        .descriptors
        .iter()
        .any(|descriptor| matches!(descriptor, OptionDescriptor::Toggle { id, .. } if id == descriptor_id))
        .then(|| TraitChange {
            options: Some(selections_from_descriptors(&replace_descriptor_current_value(
                &selected.descriptors,
                descriptor_id,
                &OptionValue::Flag { on },
            ))),
            text: None,
        })
}

#[cfg(test)]
mod tests {
    use super::super::{CatalogModel, fixtures::instance};
    use super::*;

    fn choice(id: &str, label: &str, is_default: bool) -> OptionChoice {
        OptionChoice {
            id: id.into(),
            label: label.into(),
            description: None,
            is_default,
        }
    }
    fn select(id: &str, choices: Vec<OptionChoice>, current: Option<&str>) -> OptionDescriptor {
        OptionDescriptor::Select {
            id: id.into(),
            label: id.into(),
            choices,
            current: current.map(Into::into),
            prompt_injected: vec![],
        }
    }
    fn fast_mode(on: bool) -> OptionDescriptor {
        OptionDescriptor::Toggle {
            id: "fastMode".into(),
            label: "Fast Mode".into(),
            current: Some(on),
        }
    }
    fn service_tier(current: &str) -> OptionDescriptor {
        OptionDescriptor::Select {
            id: "serviceTier".into(),
            label: "Service Tier".into(),
            choices: vec![
                choice("default", "Standard", true),
                choice("priority", "Fast", false),
                choice("ultrafast", "Ultrafast", false),
                choice("flex", "Flex", false),
            ],
            current: Some(current.into()),
            prompt_injected: vec![],
        }
    }
    fn without(descriptor: OptionDescriptor, keep: &[&str]) -> OptionDescriptor {
        match descriptor {
            OptionDescriptor::Select {
                id,
                label,
                choices,
                current,
                prompt_injected,
            } => OptionDescriptor::Select {
                id,
                label,
                choices: choices
                    .into_iter()
                    .filter(|choice| keep.contains(&choice.id.as_str()))
                    .collect(),
                current,
                prompt_injected,
            },
            toggle => toggle,
        }
    }
    fn effort() -> OptionDescriptor {
        select(
            "reasoningEffort",
            vec![choice("high", "High", false), choice("max", "Max", false)],
            Some("high"),
        )
    }
    fn context_window() -> OptionDescriptor {
        select(
            "contextWindow",
            vec![choice("200k", "200k", false), choice("1m", "1M", false)],
            Some("1m"),
        )
    }
    fn display(descriptors: &[OptionDescriptor]) -> (String, Option<SpeedIcon>) {
        let trigger =
            traits_trigger_display(Driver::Codex, descriptors, Some("reasoningEffort"), false);
        (trigger.label, trigger.speed_icon)
    }
    fn text(label: &str, icon: Option<SpeedIcon>) -> (String, Option<SpeedIcon>) {
        (label.into(), icon)
    }

    #[test]
    fn omits_fast_mode_from_the_label_entirely_when_it_is_off() {
        assert_eq!(
            display(&[effort(), fast_mode(false), context_window()]),
            text("High · 1M", None)
        );
    }

    #[test]
    fn shows_the_bolt_instead_of_a_text_label_when_fast_mode_is_on() {
        assert_eq!(
            display(&[effort(), fast_mode(true), context_window()]),
            text("High · 1M", Some(SpeedIcon::Fast))
        );
    }

    #[test]
    fn treats_codex_standard_and_fast_service_tiers_as_fast_mode_states() {
        assert_eq!(
            display(&[effort(), service_tier("default")]),
            text("High", None)
        );
        assert_eq!(
            display(&[effort(), service_tier("priority")]),
            text("High", Some(SpeedIcon::Fast))
        );
    }

    #[test]
    fn uses_a_distinct_double_bolt_for_codex_ultrafast() {
        assert_eq!(
            display(&[effort(), service_tier("ultrafast")]),
            text("High", Some(SpeedIcon::Ultrafast))
        );
    }

    #[test]
    fn uses_ultrafast_without_requiring_a_fast_tier() {
        let descriptor = without(service_tier("ultrafast"), &["default", "ultrafast", "flex"]);
        assert_eq!(
            display(&[effort(), descriptor]),
            text("High", Some(SpeedIcon::Ultrafast))
        );
    }

    #[test]
    fn keeps_other_codex_service_tiers_in_the_label() {
        assert_eq!(
            display(&[effort(), service_tier("flex")]),
            text("High · Flex", None)
        );
    }

    #[test]
    fn keeps_standard_as_text_for_models_without_speed_tiers() {
        let descriptor = without(service_tier("default"), &["default", "flex"]);
        assert_eq!(
            display(&[effort(), descriptor.clone()]),
            text("High · Standard", None)
        );
        assert_eq!(display(&[descriptor]), text("Standard", None));
    }

    #[test]
    fn keeps_the_codex_service_tier_readable_when_it_is_the_only_trait() {
        assert_eq!(display(&[service_tier("default")]), text("Standard", None));
        assert_eq!(display(&[service_tier("priority")]), text("Fast", None));
    }

    #[test]
    fn keeps_ultrafast_readable_when_it_is_the_only_trait() {
        assert_eq!(
            display(&[service_tier("ultrafast")]),
            text("Ultrafast", None)
        );
    }

    #[test]
    fn keeps_non_fast_mode_booleans_as_text_labels() {
        let thinking = OptionDescriptor::Toggle {
            id: "thinking".into(),
            label: "Thinking".into(),
            current: Some(true),
        };
        assert_eq!(
            display(&[effort(), thinking]),
            text("High · Thinking On", None)
        );
    }

    #[test]
    fn falls_back_to_a_text_label_when_fast_mode_is_the_only_trait() {
        assert_eq!(display(&[fast_mode(true)]), text("Fast", None));
        assert_eq!(display(&[fast_mode(false)]), text("Normal", None));
    }

    #[test]
    fn stays_blank_when_descriptors_resolve_to_no_label_and_there_is_no_fast_mode() {
        let unresolved = select(
            "effort",
            vec![choice("low", "Low", false), choice("high", "High", false)],
            None,
        );
        assert_eq!(display(&[unresolved]), text("", None));
    }

    #[test]
    fn still_renders_the_prompt_controlled_ultrathink_label_alongside_the_bolt() {
        let trigger = traits_trigger_display(
            Driver::Codex,
            &[effort(), fast_mode(true)],
            Some("reasoningEffort"),
            true,
        );
        assert_eq!(
            (trigger.label.as_str(), trigger.speed_icon),
            ("Ultrathink", Some(SpeedIcon::Fast))
        );
    }

    fn claude_catalog() -> ModelCatalog {
        ModelCatalog {
            instances: vec![instance("claude", Driver::Claude)],
            models: vec![CatalogModel {
                instance_id: "claude".into(),
                slug: "claude-opus-5".into(),
                name: "Claude Opus 5".into(),
                is_default: true,
                descriptors: vec![
                    OptionDescriptor::Select {
                        id: "effort".into(),
                        label: "Reasoning".into(),
                        choices: vec![
                            choice("medium", "Medium", false),
                            choice("high", "High", true),
                            choice("ultrathink", "Ultrathink", false),
                        ],
                        current: None,
                        prompt_injected: vec!["ultrathink".into()],
                    },
                    OptionDescriptor::Toggle {
                        id: "fastMode".into(),
                        label: "Fast Mode".into(),
                        current: None,
                    },
                ],
            }],
        }
    }
    fn claude_draft(text: &str) -> Draft {
        Draft {
            instance_id: "claude".into(),
            driver: Driver::Claude,
            model: "claude-opus-5".into(),
            text: text.into(),
            ..Draft::default()
        }
    }

    #[test]
    fn the_traits_view_lists_selects_then_toggles_with_current_values() {
        let view = build_traits(&claude_catalog(), &claude_draft(""), true);
        assert!(view.visible);
        assert_eq!(view.trigger.label, "High");
        assert_eq!(
            view.controls,
            vec![
                TraitControl::Select {
                    id: "effort".into(),
                    label: "Reasoning".into(),
                    choices: vec![
                        choice("medium", "Medium", false),
                        choice("high", "High", true),
                        choice("ultrathink", "Ultrathink", false),
                    ],
                    selected: "high".into(),
                    note: None,
                    disabled: false,
                },
                TraitControl::Toggle {
                    id: "fastMode".into(),
                    label: "Fast Mode".into(),
                    on: false,
                },
            ]
        );
        let unknown = Draft {
            model: "unknown".into(),
            ..claude_draft("")
        };
        assert!(!build_traits(&claude_catalog(), &unknown, true).visible);
    }

    #[test]
    fn an_ultrathink_prompt_controls_the_effort_until_it_is_only_in_the_prefix() {
        let catalog = claude_catalog();
        let prefixed = build_traits(&catalog, &claude_draft("Ultrathink:\nfix it"), true);
        assert_eq!(prefixed.trigger.label, "Ultrathink");
        assert!(
            matches!(&prefixed.controls[0], TraitControl::Select { selected, disabled: false, .. } if selected == "ultrathink")
        );
        let in_body = build_traits(&catalog, &claude_draft("please ultrathink"), true);
        assert!(matches!(
            &in_body.controls[0],
            TraitControl::Select {
                disabled: true,
                note: Some(_),
                ..
            }
        ));
        assert_eq!(
            select_trait(
                &catalog,
                &claude_draft("please ultrathink"),
                true,
                "effort",
                "medium"
            ),
            None
        );
        let settings = build_traits(&catalog, &claude_draft("please ultrathink"), false);
        assert_eq!(settings.trigger.label, "High");
    }

    #[test]
    fn picking_ultrathink_prefixes_the_prompt_and_another_effort_removes_it() {
        let catalog = claude_catalog();
        assert_eq!(
            select_trait(&catalog, &claude_draft(""), true, "effort", "ultrathink"),
            Some(TraitChange {
                options: None,
                text: Some("Ultrathink:\n".into())
            })
        );
        assert_eq!(
            select_trait(
                &catalog,
                &claude_draft("fix it"),
                true,
                "effort",
                "ultrathink"
            )
            .and_then(|change| change.text),
            Some("Ultrathink:\nfix it".into())
        );
        let change = select_trait(
            &catalog,
            &claude_draft("Ultrathink:\nfix it"),
            true,
            "effort",
            "medium",
        )
        .unwrap();
        assert_eq!(change.text.as_deref(), Some("fix it"));
        assert_eq!(
            change.options,
            Some(vec![ModelOption {
                key: "effort".into(),
                value: "medium".into()
            },])
        );
    }

    #[test]
    fn toggling_fast_mode_stores_every_current_option() {
        let change = toggle_trait(&claude_catalog(), &claude_draft(""), "fastMode", true).unwrap();
        assert_eq!(
            change.options,
            Some(vec![
                ModelOption {
                    key: "effort".into(),
                    value: "high".into()
                },
                ModelOption {
                    key: "fastMode".into(),
                    value: "true".into()
                },
            ])
        );
        assert_eq!(
            toggle_trait(&claude_catalog(), &claude_draft(""), "effort", true),
            None
        );
        let view = build_traits(
            &claude_catalog(),
            &Draft {
                options: change.options.unwrap(),
                ..claude_draft("")
            },
            true,
        );
        assert_eq!(view.trigger.speed_icon, Some(SpeedIcon::Fast));
        assert_eq!(view.accessible_label, "High, Fast mode on");
    }
}

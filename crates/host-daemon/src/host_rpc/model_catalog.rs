//! Native catalogs become instance-scoped models before reaching clients.
// T3 mapping and custom-model behavior: MIT, Copyright (c) 2026 T3 Tools Inc.
// (see third-party/T3-Code-LICENSE), reference 4ee6bfd50ef4a089440d5c3662db2298da9cc50e.
use super::service::Failure;
use agent_protocol::{models::*, operations::ModelPage, providers::ProviderInstanceId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexModel {
    id: String,
    model: String,
    display_name: String,
    default_reasoning_effort: String,
    supported_reasoning_efforts: Vec<CodexEffort>,
    service_tiers: Option<Vec<CodexTier>>,
    default_service_tier: Option<String>,
    #[serde(default)]
    additional_speed_tiers: Vec<String>,
    is_default: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CodexEffort {
    reasoning_effort: String,
}
#[derive(Deserialize)]
struct CodexTier {
    id: String,
    name: Option<String>,
    description: Option<String>,
}

fn choice(
    id: String,
    label: String,
    description: Option<String>,
    is_default: bool,
) -> ModelOptionChoice {
    ModelOptionChoice {
        id,
        label,
        description,
        is_default,
    }
}

fn select(
    id: &str,
    label: &str,
    options: Vec<ModelOptionChoice>,
    current_value: Option<String>,
) -> ModelOptionDescriptor {
    ModelOptionDescriptor {
        id: id.into(),
        label: label.into(),
        description: None,
        kind: ModelOptionKind::Select {
            options,
            current_value,
            prompt_injected_values: Vec::new(),
        },
    }
}

pub(super) fn codex_page(
    value: Value,
    instance_id: &ProviderInstanceId,
) -> Result<ModelPage, Failure> {
    let native: super::codex::Page<CodexModel> = serde_json::from_value(value)?;
    let data = native
        .data
        .into_iter()
        .map(|native| {
            let mut descriptors = Vec::new();
            let family = native
                .model
                .strip_prefix("openai.")
                .unwrap_or(&native.model);
            let default_effort = if family == "gpt-6-astra" {
                "medium"
            } else {
                &native.default_reasoning_effort
            };
            let efforts: Vec<_> = native
                .supported_reasoning_efforts
                .into_iter()
                .map(|effort| {
                    let label = match effort.reasoning_effort.as_str() {
                        "none" => "None",
                        "minimal" => "Minimal",
                        "low" => "Low",
                        "medium" => "Medium",
                        "high" => "High",
                        "xhigh" => "Extra High",
                        value => value,
                    }
                    .to_owned();
                    let is_default = effort.reasoning_effort == default_effort;
                    choice(effort.reasoning_effort, label, None, is_default)
                })
                .collect();
            if !efforts.is_empty() {
                let current = efforts
                    .iter()
                    .find(|option| option.is_default)
                    .map(|option| option.id.clone());
                descriptors.push(select("reasoningEffort", "Reasoning", efforts, current));
            }
            let tiers = native
                .service_tiers
                .filter(|tiers| !tiers.is_empty())
                .unwrap_or_else(|| {
                    native
                        .additional_speed_tiers
                        .into_iter()
                        .map(|id| CodexTier {
                            name: Some(if id == "fast" {
                                "Fast".into()
                            } else {
                                id.clone()
                            }),
                            id,
                            description: None,
                        })
                        .collect()
                });
            if !tiers.is_empty() {
                let default = native
                    .default_service_tier
                    .filter(|id| tiers.iter().any(|tier| &tier.id == id))
                    .unwrap_or_else(|| "default".into());
                let mut options = vec![choice(
                    "default".into(),
                    "Standard".into(),
                    None,
                    default == "default",
                )];
                options.extend(tiers.into_iter().map(|tier| {
                    let label = tier.name.unwrap_or_else(|| tier.id.clone());
                    let description = if tier.id == "ultrafast" {
                        Some("Even faster, more expensive".into())
                    } else {
                        tier.description
                    };
                    let is_default = tier.id == default;
                    choice(tier.id, label, description, is_default)
                }));
                descriptors.push(select(
                    "serviceTier",
                    "Service Tier",
                    options,
                    Some(default),
                ));
            }
            Model {
                id: native.id,
                model: ModelRef {
                    instance_id: instance_id.clone(),
                    id: native.model,
                },
                display_name: native.display_name,
                capabilities: ModelCapabilities {
                    option_descriptors: descriptors,
                },
                is_custom: false,
                is_default: native.is_default,
            }
        })
        .collect();
    Ok(ModelPage {
        data,
        instances: Vec::new(),
        next_cursor: native.next_cursor,
        provider_errors: None,
    })
}

pub(crate) fn claude_capabilities(values: Option<&Value>) -> Result<ModelCapabilities, String> {
    let Some(values) = values else {
        return Ok(ModelCapabilities::default());
    };
    let efforts = values.as_array().ok_or("invalid Claude effort levels")?;
    let mut options = Vec::new();
    for value in efforts {
        let id = value
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or("invalid Claude effort level")?;
        options.push(choice(id.into(), id.into(), None, id == "high"));
    }
    let current = options
        .iter()
        .find(|option| option.is_default)
        .or(options.first())
        .map(|option| option.id.clone());
    Ok(ModelCapabilities {
        option_descriptors: if options.is_empty() {
            Vec::new()
        } else {
            vec![select("effort", "Effort", options, current)]
        },
    })
}

/// The native cursor alone cannot distinguish custom slugs from earlier pages.
/// This state travels with the generation-fenced Host catalog cursor.
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ModelProgress {
    pub(super) cursor: Option<String>,
    seen: BTreeSet<String>,
    fallback: Option<ModelCapabilities>,
}

pub(super) fn custom_models_for_page(
    driver: &str,
    instance_id: &ProviderInstanceId,
    custom_models: Option<&Value>,
    progress: &ModelProgress,
    models: &[Model],
    next_cursor: Option<&str>,
) -> (ModelProgress, Vec<Model>) {
    let entries = agent_protocol::models::read_custom_models(custom_models);
    if entries.is_empty() {
        return (
            ModelProgress {
                cursor: next_cursor.map(str::to_owned),
                ..Default::default()
            },
            Vec::new(),
        );
    }
    let mut seen = progress.seen.clone();
    let fallback = if driver == "codex" && entries.iter().any(|entry| entry.capabilities.is_none())
    {
        progress
            .fallback
            .clone()
            .or_else(|| models.first().map(|model| model.capabilities.clone()))
    } else {
        None
    };
    for model in models {
        seen.insert(model.model.id.clone());
    }
    let mut custom = Vec::new();
    if next_cursor.is_none() {
        for entry in entries {
            if !seen.insert(entry.slug.clone()) {
                continue;
            }
            let capabilities = entry.capabilities.unwrap_or_else(|| {
                if driver == "codex" {
                    fallback.clone().unwrap_or_default()
                } else {
                    ModelCapabilities::default()
                }
            });
            custom.push(Model {
                id: entry.slug.clone(),
                model: ModelRef {
                    instance_id: instance_id.clone(),
                    id: entry.slug,
                },
                display_name: entry.name,
                capabilities,
                is_custom: true,
                is_default: Some(false),
            });
        }
    }
    (
        ModelProgress {
            cursor: next_cursor.map(str::to_owned),
            seen,
            fallback,
        },
        custom,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn native(id: &str, current: &str) -> Model {
        serde_json::from_value(json!({
            "id":id,"model":{"instanceId":"work","id":id},"displayName":id,
            "capabilities":{"optionDescriptors":[{"id":"reasoningEffort","label":"Reasoning","type":"select",
                "options":[{"id":current,"label":current}],"currentValue":current}]}
        })).unwrap()
    }

    #[test]
    fn native_codex_capabilities_use_astra_default_and_live_speed_tiers() {
        let page = codex_page(json!({"data":[
            {"id":"native-id","model":"openai.gpt-6-astra","displayName":"GPT-6 Astra",
             "defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
             "serviceTiers":[{"id":"priority","name":"Priority","description":"Fast"},{"id":"ultrafast","name":"Ultra","description":"Verbose"}],
             "defaultServiceTier":"priority","isDefault":true},
            {"id":"other","model":"other","displayName":"Other","defaultReasoningEffort":"high","supportedReasoningEfforts":[],
             "serviceTiers":[],"additionalSpeedTiers":["fast"],"defaultServiceTier":"missing"}
        ],"nextCursor":"native-next"}), &"work".parse().unwrap()).unwrap();
        let first = &page.data[0];
        assert_eq!(first.id, "native-id");
        assert_eq!(first.model.id, "openai.gpt-6-astra");
        assert_eq!(first.model.instance_id.as_str(), "work");
        assert!(!first.is_custom);
        assert_eq!(
            first
                .capabilities
                .select(&["reasoningEffort"])
                .unwrap()
                .selected(None),
            Some("medium")
        );
        let speed = first.capabilities.select(&["serviceTier"]).unwrap();
        assert_eq!(speed.selected(None), Some("priority"));
        assert_eq!(
            speed
                .choices()
                .iter()
                .map(|choice| choice.is_default)
                .collect::<Vec<_>>(),
            [false, true, false]
        );
        assert_eq!(
            speed
                .choices()
                .iter()
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            ["default", "priority", "ultrafast"]
        );
        assert_eq!(
            speed.choices()[2].description.as_deref(),
            Some("Even faster, more expensive")
        );
        assert!(
            page.data[1]
                .capabilities
                .select(&["reasoningEffort"])
                .is_none()
        );
        let fallback = page.data[1].capabilities.select(&["serviceTier"]).unwrap();
        assert_eq!(fallback.selected(None), Some("default"));
        assert_eq!(fallback.choices()[1].label, "Fast");
        assert_eq!(page.next_cursor.as_deref(), Some("native-next"));
        let bytes = agent_protocol::protocol::response_frame(
            agent_protocol::protocol::Response::from_result::<_, Failure>(Ok(page)),
        )
        .unwrap();
        let agent_protocol::protocol::Response::Success { result: wire } =
            agent_protocol::protocol::decode::<agent_protocol::protocol::Response<ModelPage>>(
                &bytes,
            )
            .unwrap()
        else {
            panic!("expected model page")
        };
        assert_eq!(wire.data[0].model.instance_id.as_str(), "work");
        assert_eq!(
            wire.data[0]
                .capabilities
                .select(&["reasoningEffort"])
                .unwrap()
                .selected(None),
            Some("medium")
        );
        assert_eq!(
            wire.data[1]
                .capabilities
                .select(&["serviceTier"])
                .unwrap()
                .choices()[1]
                .id,
            "fast"
        );
        assert!(
            codex_page(
                json!({"data":[{"model":42}],"nextCursor":null}),
                &"work".parse().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn custom_catalog_waits_for_all_pages_and_uses_first_native_capabilities() {
        let id = "work".parse().unwrap();
        let settings = json!(["first","later","bare",{"slug":"named","name":"My Model","capabilities":{"optionDescriptors":[]}},"bare",{"slug":"bad-caps","capabilities":{"optionDescriptors":42}}]);
        let initial = ModelProgress::default();
        let (progress, custom) = custom_models_for_page(
            "codex",
            &id,
            Some(&settings),
            &initial,
            &[native("first", "low")],
            Some("next"),
        );
        assert!(custom.is_empty());
        assert!(initial.seen.is_empty());
        assert_eq!(progress.cursor.as_deref(), Some("next"));
        let (finished, custom) = custom_models_for_page(
            "codex",
            &id,
            Some(&settings),
            &progress,
            &[native("later", "high")],
            None,
        );
        assert!(finished.cursor.is_none());
        assert_eq!(
            custom
                .iter()
                .map(|model| model.model.id.as_str())
                .collect::<Vec<_>>(),
            ["bare", "named", "bad-caps"]
        );
        assert!(custom.iter().all(|model| model.is_custom
            && model.model.instance_id == id
            && model.is_default == Some(false)));
        assert_eq!(
            custom[0]
                .capabilities
                .select(&["reasoningEffort"])
                .unwrap()
                .selected(None),
            Some("low")
        );
        assert_eq!(custom[2].capabilities, custom[0].capabilities);
        assert_eq!(custom[1].display_name, "My Model");
        assert!(custom[1].capabilities.option_descriptors.is_empty());
        // Native cursors must keep advancing even without custom models. They
        // do not carry an unused catalog snapshot for explicit caps or Claude.
        for settings in [None, Some(json!([]))] {
            let (progress, custom) = custom_models_for_page(
                "codex",
                &id,
                settings.as_ref(),
                &initial,
                &[native("first", "low")],
                Some("next"),
            );
            let wire = serde_json::to_value(progress).unwrap();
            assert_eq!(wire["cursor"], "next");
            assert_eq!(wire["seen"], json!([]));
            assert_eq!(wire["fallback"], Value::Null);
            assert!(custom.is_empty());
        }
        for (driver, settings) in [
            (
                "codex",
                json!([{"slug":"named","capabilities":{"optionDescriptors":[]}}]),
            ),
            ("claudeAgent", json!(["bare"])),
        ] {
            let (progress, _) = custom_models_for_page(
                driver,
                &id,
                Some(&settings),
                &initial,
                &[native("first", "low")],
                Some("next"),
            );
            let wire = serde_json::to_value(progress).unwrap();
            assert_eq!(wire["cursor"], "next");
            assert_eq!(wire["fallback"], Value::Null);
        }
        let (_, claude) =
            custom_models_for_page("claudeAgent", &id, Some(&settings), &initial, &[], None);
        assert!(
            claude
                .iter()
                .all(|model| model.capabilities.option_descriptors.is_empty())
        );
    }

    #[test]
    fn claude_effort_levels_are_native_and_absence_is_not_an_invented_capability() {
        let caps = claude_capabilities(Some(&json!(["low", "high", "max"]))).unwrap();
        let effort = caps.select(&["effort"]).unwrap();
        assert_eq!(effort.selected(None), Some("high"));
        assert_eq!(
            effort
                .choices()
                .iter()
                .map(|choice| choice.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "high", "max"]
        );
        assert!(
            claude_capabilities(None)
                .unwrap()
                .option_descriptors
                .is_empty()
        );
        assert!(
            claude_capabilities(Some(&json!([])))
                .unwrap()
                .option_descriptors
                .is_empty()
        );
        assert!(claude_capabilities(Some(&json!([42]))).is_err());
        assert!(claude_capabilities(Some(&json!("high"))).is_err());
        assert_eq!(
            claude_capabilities(Some(&json!(["low"])))
                .unwrap()
                .select(&["effort"])
                .unwrap()
                .selected(None),
            Some("low")
        );
    }

    proptest::proptest! {
        #[test]
        fn paging_never_promotes_a_native_slug_to_custom(split in 0usize..9) {
            let all: Vec<_> = (0..8).map(|index|native(&format!("native-{index}"),"high")).collect();
            let settings = json!(["native-0","extra","native-7","extra", "last"]);
            let id = "work".parse().unwrap();
            let (progress, first) = custom_models_for_page("codex", &id, Some(&settings), &ModelProgress::default(), &all[..split], Some("next"));
            proptest::prop_assert!(first.is_empty());
            let (_, last) = custom_models_for_page("codex", &id, Some(&settings), &progress, &all[split..], None);
            proptest::prop_assert_eq!(last.iter().map(|model|model.model.id.as_str()).collect::<Vec<_>>(),["extra","last"]);
            proptest::prop_assert_eq!(&last[0].capabilities,&all[0].capabilities);
        }
    }
}

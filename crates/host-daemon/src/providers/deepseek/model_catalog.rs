//! Pure ACP model and option normalization; no provider process or client state.
use super::{Failure, PROVIDER, failure};
use agent_protocol::{
    dsh::{CredentialRoute, CredentialSource, KeyProvider},
    models::{Model, ModelRef, ReasoningEffort},
};
use serde_json::{Value, json};

pub(super) fn credential_route(id: &str) -> Option<KeyProvider> {
    model_route(id).map(|(provider, _)| provider)
}

fn model_route(id: &str) -> Option<(KeyProvider, &'static str)> {
    let [route, model]: [String; 2] = serde_json::from_str(id).ok()?;
    if model.is_empty() {
        return None;
    }
    KeyProvider::ALL.into_iter().find_map(|provider| {
        provider
            .routes()
            .iter()
            .find(|candidate| **candidate == route)
            .map(|route| (provider, *route))
    })
}

/// Names the service behind a route; one key may serve several routes.
fn route_label(provider: KeyProvider, route: &str) -> &'static str {
    match route {
        "opencode-go" => "OpenCode Go",
        "opencode" => "OpenCode Zen",
        _ => provider.model_label(),
    }
}

pub(super) fn provider_profile(provider: KeyProvider) -> Value {
    let mut profile = json!({"apiKeyEnv":provider.reference()});
    if provider == KeyProvider::Venice {
        profile["displayName"] = json!("Venice AI");
        profile["api"] = json!("openai-completions");
        profile["baseURL"] = json!("https://api.venice.ai/api/v1");
        profile["models"] = json!([
            {
                "id":"deepseek-v4-1-flash",
                "name":"DeepSeek V4.1 Flash",
                "contextWindow":1000000,
                "maxTokens":32768,
                "input":["text","image"],
                "reasoningEfforts":{"off":"none","low":"low","high":"high","max":"max"}
            },
            {
                "id":"qwen3-coder-480b-a35b-instruct-turbo",
                "name":"Qwen 3 Coder 480B Turbo",
                "contextWindow":256000,
                "maxTokens":65536,
                "input":["text"]
            },
            {
                "id":"olafangensan-glm-4.7-flash-heretic",
                "name":"GLM 4.7 Flash Heretic",
                "contextWindow":200000,
                "maxTokens":24000,
                "input":["text"],
                "reasoningEfforts":{"off":"none","low":"low","medium":"medium","high":"high"}
            },
            {
                "id":"venice-uncensored-1-2",
                "name":"Venice Uncensored 1.2",
                "contextWindow":128000,
                "maxTokens":8192,
                "input":["text"]
            }
        ]);
    }
    if provider == KeyProvider::NanoGpt {
        // NanoGPT is a custom OpenAI-compatible route outside the installed catalog.
        // Model capabilities: https://nano-gpt.com/api/v1/models?detailed=true
        profile["displayName"] = json!("NanoGPT");
        profile["api"] = json!("openai-completions");
        profile["baseURL"] = json!("https://nano-gpt.com/api/v1");
        profile["compat"] = json!({
            "supportsReasoningEffort":true,
            "requiresReasoningContentOnAssistantMessages":true
        });
        profile["models"] = json!([
            {
                "id":"deepseek/deepseek-v4.1-flash",
                "name":"DeepSeek V4.1 Flash",
                "contextWindow":1000000,
                "maxTokens":32768,
                "input":["text","image"],
                "reasoningEfforts":{"off":"none","low":"low","high":"high","max":"max"}
            },
            {
                "id":"deepseek/deepseek-v4.1-flash:thinking",
                "name":"DeepSeek V4.1 Flash (Thinking)",
                "contextWindow":1000000,
                "maxTokens":32768,
                "input":["text","image"],
                "reasoningEfforts":{"low":"low","high":"high","max":"max"}
            },
            {
                "id":"z-ai/glm-5.3-flash-uncensored",
                "name":"GLM 5.3 Flash Uncensored"
            }
        ]);
    }
    profile
}

pub(super) fn catalog(created: &Value, credentials: &[CredentialRoute]) -> Vec<Model> {
    let mut models: Vec<_> = models(created)
        .into_iter()
        .filter(|model| {
            credentials.iter().any(|route| {
                model.credential_route == Some(route.provider)
                    && route.source != CredentialSource::Missing
                    && !route.needs_attention
            })
        })
        .collect();
    // ACP's current model can belong to an unconfigured built-in route.
    if !models.iter().any(|model| model.is_default == Some(true))
        && let Some(first) = models.first_mut()
    {
        first.is_default = Some(true);
    }
    models
}

fn models(created: &Value) -> Vec<Model> {
    let options = created["configOptions"].as_array();
    let model = options.into_iter().flatten().find(|o| o["id"] == "model");
    let reasoning = options
        .into_iter()
        .flatten()
        .find(|o| o["id"] == "reasoning_effort");
    model
        .into_iter()
        .flat_map(|m| m["options"].as_array().into_iter().flatten())
        .flat_map(|option| {
            option["options"]
                .as_array()
                .map(|v| v.iter().collect::<Vec<_>>())
                .unwrap_or_else(|| vec![option])
        })
        .filter_map(|option| {
            let id = option["value"].as_str()?;
            let current = model.is_some_and(|m| m["currentValue"] == id);
            let name = option["name"].as_str().unwrap_or(id);
            let route = model_route(id);
            Some(Model {
                id: format!("deepseek:{id}"),
                model: ModelRef {
                    provider: PROVIDER,
                    id: id.into(),
                },
                credential_route: route.map(|(provider, _)| provider),
                display_name: route.map_or_else(
                    || name.to_owned(),
                    |(provider, route)| format!("{} · {name}", route_label(provider, route)),
                ),
                settings_discovery: true,
                settings: current.then(|| agent_protocol::models::ModelSettings {
                    default_reasoning_effort: reasoning
                        .and_then(|option| option["currentValue"].as_str())
                        .unwrap_or_default()
                        .into(),
                    supported_reasoning_efforts: reasoning
                        .into_iter()
                        .flat_map(|option| option["options"].as_array().into_iter().flatten())
                        .filter_map(|option| {
                            option["value"].as_str().map(|value| ReasoningEffort {
                                reasoning_effort: value.into(),
                            })
                        })
                        .collect(),
                    service_tiers: None,
                    default_service_tier: None,
                }),
                is_default: Some(current),
            })
        })
        .collect()
}

pub(super) fn resolved_model_settings(
    model_id: &str,
    config: &Value,
) -> Result<agent_protocol::models::ModelSettings, Failure> {
    models(config)
        .into_iter()
        .find(|model| model.model.id == model_id)
        .and_then(|model| model.settings)
        .ok_or_else(|| failure("The provider did not resolve settings for the requested model"))
}

#[cfg(test)]
mod tests {
    use super::super::DEFAULT_MODEL;
    use super::*;

    #[test]
    fn only_configured_routes_are_listed_and_an_available_model_is_the_default() {
        let official = DEFAULT_MODEL;
        let go = r#"["opencode-go","deepseek-v4.1-flash"]"#;
        let zen = r#"["opencode","deepseek-v4.1-flash"]"#;
        let mut value = json!({"configOptions":[
            {"id":"model","currentValue":official,"options":[
                {"value":official}, {"value":go}, {"value":zen},
                {"value":r#"["openrouter","deepseek/flash"]"#},
                {"value":"unknown-route"}
            ]},
            {"id":"reasoning_effort","currentValue":"high","options":[{"value":"high"}]}
        ]});
        let mut credentials = vec![CredentialRoute {
            provider: KeyProvider::OpenCode,
            source: CredentialSource::Managed,
            needs_attention: false,
        }];
        let models = catalog(&value, &credentials);
        assert_eq!(
            models
                .iter()
                .map(|m| m.model.id.as_str())
                .collect::<Vec<_>>(),
            [go, zen]
        );
        assert_eq!(models[0].is_default, Some(true));
        assert_eq!(models[1].is_default, Some(false));
        assert!(
            models.iter().all(|model| model.settings.is_none()),
            "a new default must discover its own settings"
        );

        credentials.push(CredentialRoute {
            provider: KeyProvider::DeepSeek,
            source: CredentialSource::External,
            needs_attention: false,
        });
        let models = catalog(&value, &credentials);
        assert_eq!(
            models
                .iter()
                .map(|m| m.model.id.as_str())
                .collect::<Vec<_>>(),
            [official, go, zen]
        );
        assert_eq!(
            models
                .iter()
                .filter(|m| m.is_default == Some(true))
                .map(|m| m.model.id.as_str())
                .collect::<Vec<_>>(),
            [official]
        );
        assert_eq!(
            models[0]
                .settings
                .as_ref()
                .unwrap()
                .default_reasoning_effort,
            "high"
        );
        value["configOptions"][0]["currentValue"] = json!(go);
        let models = catalog(&value, &credentials);
        assert_eq!(
            models
                .iter()
                .filter(|m| m.is_default == Some(true))
                .map(|m| m.model.id.as_str())
                .collect::<Vec<_>>(),
            [go],
            "an available native default must remain selected even when it is not first"
        );
    }

    #[test]
    fn missing_and_misplaced_credentials_never_enable_a_model() {
        let value = json!({"configOptions":[{"id":"model","currentValue":DEFAULT_MODEL,"options":[{"value":DEFAULT_MODEL}]}]});
        for (source, needs_attention, count) in [
            (CredentialSource::Missing, false, 0),
            (CredentialSource::Managed, true, 0),
            (CredentialSource::External, true, 0),
            (CredentialSource::Managed, false, 1),
            (CredentialSource::External, false, 1),
        ] {
            let credentials = [CredentialRoute {
                provider: KeyProvider::DeepSeek,
                source,
                needs_attention,
            }];
            assert_eq!(
                catalog(&value, &credentials).len(),
                count,
                "{source:?}, attention={needs_attention}"
            );
        }
        assert!(catalog(&value, &[]).is_empty());
    }

    #[test]
    fn model_routes_preserve_provider_boundaries() {
        for provider in KeyProvider::ALL {
            for route in provider.routes() {
                let id = serde_json::to_string(&[route, "test/model"]).unwrap();
                assert_eq!(credential_route(&id), Some(provider));
            }
        }
        for id in [
            "deepseek-flash",
            "[]",
            r#"["openrouter",""]"#,
            r#"["unknown","model"]"#,
        ] {
            assert_eq!(credential_route(id), None);
        }
    }

    #[test]
    fn venice_sessions_keep_their_native_route_and_managed_credential() {
        let id = r#"["venice","deepseek-v4-1-flash"]"#;
        let profile = provider_profile(KeyProvider::Venice);
        assert_eq!(profile["apiKeyEnv"], "VENICE_API_KEY");
        assert_eq!(profile["baseURL"], "https://api.venice.ai/api/v1");
        assert_eq!(profile["models"][0]["id"], "deepseek-v4-1-flash");
        let created = json!({"configOptions":[{"id":"model","currentValue":id,"options":[
            {"value":id,"name":"DeepSeek V4.1 Flash"}
        ]}]});
        let credentials = [CredentialRoute {
            provider: KeyProvider::Venice,
            source: CredentialSource::Managed,
            needs_attention: false,
        }];
        let models = catalog(&created, &credentials);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model.id, id);
        assert_eq!(models[0].credential_route, Some(KeyProvider::Venice));
        assert_eq!(models[0].display_name, "Venice AI · DeepSeek V4.1 Flash");
    }
    #[test]
    fn nanogpt_uses_its_own_endpoint_credential_and_reasoning_wire_values() {
        let profile = provider_profile(KeyProvider::NanoGpt);
        assert_eq!(profile["apiKeyEnv"], "NANOGPT_API_KEY");
        assert_eq!(profile["baseURL"], "https://nano-gpt.com/api/v1");
        assert_eq!(profile["api"], "openai-completions");
        let models = profile["models"].as_array().unwrap();
        assert_eq!(models[0]["id"], "deepseek/deepseek-v4.1-flash");
        assert_eq!(models[0]["reasoningEfforts"]["off"], "none");
        assert!(models[1]["reasoningEfforts"].get("off").is_none());
        assert_eq!(
            provider_profile(KeyProvider::OpenRouter),
            json!({"apiKeyEnv":"OPENROUTER_API_KEY"})
        );
    }

    #[test]
    fn reasoning_discovery_is_specific_to_the_selected_route() {
        let initial = json!({"configOptions":[
            {"id":"model","currentValue":"official","options":[{"value":"official"},{"value":"router"},{"value":"plain"}]},
            {"id":"reasoning_effort","currentValue":"high","options":[{"value":"off"},{"value":"high"},{"value":"max"}]}
        ]});
        let mut models = models(&initial);
        let router = json!({"configOptions":[
            {"id":"model","currentValue":"router","options":[{"value":"router"}]},
            {"id":"reasoning_effort","currentValue":"","options":[{"value":""},{"value":"low"},{"value":"high"}]}
        ]});
        models[1].settings = Some(resolved_model_settings("router", &router).unwrap());
        assert_eq!(
            models[1]
                .settings
                .as_ref()
                .unwrap()
                .supported_reasoning_efforts
                .iter()
                .map(|e| e.reasoning_effort.as_str())
                .collect::<Vec<_>>(),
            vec!["", "low", "high"]
        );
        assert_eq!(
            models[1]
                .settings
                .as_ref()
                .unwrap()
                .default_reasoning_effort,
            ""
        );
        assert_eq!(models[1].is_default, Some(false));
        assert!(resolved_model_settings("plain", &router).is_err());
        assert!(models[2].settings.is_none());
        assert_eq!(
            models[0]
                .settings
                .as_ref()
                .unwrap()
                .supported_reasoning_efforts
                .len(),
            3
        );
    }

    #[test]
    fn opencode_routes_share_one_key_but_name_their_service() {
        let value = json!({"configOptions":[{"id":"model","currentValue":"","options":[
            {"value":r#"["opencode-go","glm-5.3"]"#,"name":"GLM-5.3"},
            {"value":r#"["opencode","glm-5.3"]"#,"name":"GLM-5.3"}
        ]}]});
        let models = models(&value);
        assert_eq!(models[0].display_name, "OpenCode Go · GLM-5.3");
        assert_eq!(models[1].display_name, "OpenCode Zen · GLM-5.3");
        assert!(
            models
                .iter()
                .all(|model| model.credential_route == Some(KeyProvider::OpenCode))
        );
        assert_eq!(
            provider_profile(KeyProvider::OpenCode),
            json!({"apiKeyEnv":"OPENCODE_API_KEY"})
        );
    }

    #[test]
    fn catalog_preserves_opaque_model_routes_and_native_reasoning() {
        let value = json!({"configOptions":[{"id":"model","currentValue":DEFAULT_MODEL,"options":[{"name":"DeepSeek","options":[{"value":DEFAULT_MODEL,"name":"deepseek-flash"},{"value":"other-route","name":"Other"}]}]},{"id":"reasoning_effort","currentValue":"high","options":[{"value":"off"},{"value":"high"},{"value":"max"}]}]});
        let models = models(&value);
        assert_eq!(models[0].model.id, DEFAULT_MODEL);
        assert_eq!(models[0].display_name, "DeepSeek公式 · deepseek-flash");
        assert_eq!(
            models[0]
                .settings
                .as_ref()
                .unwrap()
                .default_reasoning_effort,
            "high"
        );
        assert_eq!(
            models[0]
                .settings
                .as_ref()
                .unwrap()
                .supported_reasoning_efforts
                .len(),
            3
        );
        assert!(models[1].settings.is_none());
        assert_eq!(models[1].model.id, "other-route");
    }
}

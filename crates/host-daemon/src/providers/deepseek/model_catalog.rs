//! Pure ACP model and option normalization; no provider process or client state.
use super::{Failure, PROVIDER, failure};
use agent_protocol::{
    dsh::KeyProvider,
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
            }
        ]);
    }
    profile
}

pub(super) fn catalog(created: &Value) -> Vec<Model> {
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
    catalog(config)
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
        let mut models = catalog(&initial);
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
        let models = catalog(&value);
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
        let models = catalog(&value);
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

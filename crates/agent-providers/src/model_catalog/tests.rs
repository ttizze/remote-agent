use super::*;
use serde_json::json;

fn choice(id: &str, label: &str, description: Option<&str>, default: bool) -> OptionChoice {
    OptionChoice {
        id: id.into(),
        label: label.into(),
        description: description.map(str::to_owned),
        is_default: default,
    }
}

fn select(
    id: &str,
    label: &str,
    options: Vec<OptionChoice>,
    current: Option<&str>,
) -> OptionDescriptor {
    OptionDescriptor::Select(SelectOption {
        id: id.into(),
        label: label.into(),
        description: None,
        options,
        current_value: current.map(str::to_owned),
        prompt_injected_values: vec![],
    })
}

// CodexProvider.test.ts "maps current Codex model capability fields".
#[test]
fn codex_models_map_effort_and_service_tier_descriptors() {
    let models = codex_catalog(
        &[json!({
            "additionalSpeedTiers": [],
            "defaultReasoningEffort": "super-high",
            "description": "Test model",
            "displayName": "GPT Test",
            "hidden": false,
            "id": "gpt-test",
            "isDefault": true,
            "model": "gpt-test",
            "defaultServiceTier": "flex",
            "serviceTiers": [
                {"id": "priority", "name": "Fast", "description": "Lower latency responses."},
                {"id": "flex", "name": "Flex", "description": "Lower-cost asynchronous routing."}
            ],
            "supportedReasoningEfforts": [
                {"description": "Maximum reasoning", "reasoningEffort": "super-high"}
            ]
        })],
        false,
    )
    .unwrap();
    assert_eq!(
        models[0].descriptors,
        [
            select(
                "reasoningEffort",
                "Reasoning",
                vec![choice("super-high", "super-high", None, true)],
                Some("super-high"),
            ),
            select(
                "serviceTier",
                "Service Tier",
                vec![
                    choice("default", "Standard", None, false),
                    choice("priority", "Fast", Some("Lower latency responses."), false),
                    choice(
                        "flex",
                        "Flex",
                        Some("Lower-cost asynchronous routing."),
                        true
                    ),
                ],
                Some("flex"),
            ),
        ]
    );
    assert!(models[0].is_default);
    assert_eq!(models[0].name, "GPT Test");
    // A shared ChatGPT login offers no service tier.
    let shared = codex_catalog(&[json!({"model": "gpt-test", "displayName": "gpt-test", "serviceTiers": [{"id": "flex", "name": "Flex"}]})], true).unwrap();
    assert!(shared[0].descriptors.is_empty());
}

// "uses standard routing when the catalog has no default service tier".
#[test]
fn codex_models_default_to_standard_routing() {
    let models = codex_catalog(
        &[json!({
            "additionalSpeedTiers": ["fast"],
            "defaultReasoningEffort": "medium",
            "defaultServiceTier": null,
            "displayName": "GPT Test",
            "isDefault": true,
            "model": "gpt-test",
            "serviceTiers": [
                {"id": "priority", "name": "Fast", "description": "1.5x speed, increased usage"},
                {"id": "ultrafast", "name": "Ultrafast", "description": "The fastest available responses for latency-sensitive work."}
            ],
            "supportedReasoningEfforts": []
        })],
        false,
    )
    .unwrap();
    assert_eq!(
        models[0].descriptors,
        [select(
            "serviceTier",
            "Service Tier",
            vec![
                choice("default", "Standard", None, true),
                choice(
                    "priority",
                    "Fast",
                    Some("1.5x speed, increased usage"),
                    false
                ),
                choice(
                    "ultrafast",
                    "Ultrafast",
                    Some("Even faster, more expensive"),
                    false
                ),
            ],
            Some("default"),
        )]
    );
    let speed = codex_catalog(
        &[json!({"model": "gpt-test", "displayName": "x", "additionalSpeedTiers": ["fast"]})],
        false,
    )
    .unwrap();
    let OptionDescriptor::Select(tiers) = &speed[0].descriptors[0] else {
        panic!("a select");
    };
    assert_eq!(tiers.options[1], choice("fast", "Fast", None, false));
}

fn defaults(slugs: &[(&str, bool)]) -> Vec<(String, bool)> {
    let native: Vec<Value> = slugs
        .iter()
        .map(|(slug, default)| json!({"model": slug, "displayName": slug, "isDefault": default}))
        .collect();
    codex_catalog(&native, false)
        .unwrap()
        .into_iter()
        .map(|model| (model.slug, model.is_default))
        .collect()
}

// CodexProvider.test.ts applyPreferredCodexDefaultModel cases.
#[test]
fn the_most_preferred_available_codex_model_is_the_default() {
    assert_eq!(
        defaults(&[("gpt-5.6-terra", false), ("gpt-5.4", true)]),
        [("gpt-5.6-terra".into(), true), ("gpt-5.4".into(), false)]
    );
    assert_eq!(
        defaults(&[("gpt-5.6-terra", false), ("gpt-5.6-sol", false)]),
        [
            ("gpt-5.6-terra".into(), false),
            ("gpt-5.6-sol".into(), true)
        ]
    );
    assert_eq!(
        defaults(&[("openai.gpt-5.6-luna", true), ("openai.gpt-5.6-sol", false)]),
        [
            ("openai.gpt-5.6-luna".into(), false),
            ("openai.gpt-5.6-sol".into(), true)
        ]
    );
    assert_eq!(
        defaults(&[("gpt-5.5", false), ("gpt-5.4", true)]),
        [("gpt-5.5".into(), false), ("gpt-5.4".into(), true)]
    );
}

// ModelManifest.test.ts "classifies qualified Codex families without changing
// their wire ids".
#[test]
fn codex_models_missing_from_the_manifest_are_legacy() {
    let models = codex_catalog(
        &[
            json!({"model": "openai.gpt-6-astra", "displayName": "a"}),
            json!({"model": "openai.gpt-old", "displayName": "b"}),
        ],
        false,
    )
    .unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| (model.slug.as_str(), model.is_legacy))
            .collect::<Vec<_>>(),
        [("openai.gpt-6-astra", false), ("openai.gpt-old", true)]
    );
    assert_eq!(codex_model_name("gpt-6-astra"), "GPT-6-Astra");
}

// ModelManifest.test.ts resolveProviderCatalog and ClaudeModelCatalog.test.ts
// version filtering, against the bundled manifest.
#[test]
fn claude_models_come_from_the_manifest_for_the_installed_version() {
    let all = claude_catalog(Some("2.1.284"));
    let fable = all
        .iter()
        .find(|model| model.slug == "claude-fable-5-1")
        .unwrap();
    assert!(fable.is_default && !fable.is_legacy);
    let opus = all
        .iter()
        .find(|model| model.slug == "claude-opus-5-5")
        .unwrap();
    assert_eq!(opus.badge.as_deref(), Some("new"));
    assert_eq!(
        opus.descriptors
            .iter()
            .map(OptionDescriptor::id)
            .collect::<Vec<_>>(),
        ["effort", "fastMode", "contextWindow"]
    );
    let haiku = all
        .iter()
        .find(|model| model.slug == "claude-haiku-4-5")
        .unwrap();
    assert!(haiku.is_legacy);
    assert_eq!(
        haiku
            .descriptors
            .iter()
            .map(OptionDescriptor::id)
            .collect::<Vec<_>>(),
        ["thinking"]
    );
    let older: Vec<_> = claude_catalog(Some("2.1.200"))
        .into_iter()
        .map(|model| model.slug)
        .collect();
    assert!(older.contains(&"claude-opus-4-8".to_owned()));
    assert!(!older.contains(&"claude-opus-5".to_owned()));
    assert!(
        claude_catalog(None)
            .iter()
            .all(|model| model.slug != "claude-opus-4-7")
    );
    assert_eq!(
        claude_upgrade_message(Some("2.1.200")).as_deref(),
        Some(
            "Claude Code v2.1.200 is too old for Claude Opus 5. Upgrade to v2.1.219 or newer to access it."
        )
    );
    assert_eq!(claude_upgrade_message(Some("2.1.284")), None);
    assert_eq!(
        cli_version("2.1.284 (Claude Code)").as_deref(),
        Some("2.1.284")
    );
    assert_eq!(claude_entry("Opus").unwrap().model.slug, "claude-opus-5");
}

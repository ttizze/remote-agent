//! Claude manifest and option compilation at the native provider boundary.
//! Adapted from T3 Tools Inc.'s MIT implementation; see third-party/T3-Code-LICENSE.
use std::collections::{BTreeMap, BTreeSet};

use agent_protocol::{models::*, providers::ProviderInstanceId};
use semver::Version;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Query {
    pub model: String,
    pub effort: Option<String>,
    pub settings: BTreeMap<String, bool>,
}

pub(super) struct Selection {
    pub query: Query,
    pub prompt_effort: Option<String>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Runtime {
    #[serde(default)]
    effort_map: BTreeMap<String, Option<String>>,
    #[serde(default)]
    model_suffixes: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Compatibility {
    min_version: Option<Version>,
    max_version_exclusive: Option<Version>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Adapter<T> {
    #[serde(default)]
    claude_code: T,
}

#[derive(Deserialize)]
struct Profile {
    #[serde(default)]
    capabilities: ModelCapabilities,
    #[serde(default)]
    adapter: Adapter<Runtime>,
}

#[derive(Deserialize)]
struct ManifestModel {
    slug: String,
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    profile: Option<String>,
    #[serde(default)]
    adapter: Adapter<Compatibility>,
}

#[derive(Default, Deserialize)]
struct Defaults {
    chat: Option<String>,
}

#[derive(Deserialize)]
struct ProviderCatalog {
    #[serde(default)]
    defaults: Defaults,
    profiles: BTreeMap<String, Profile>,
    models: Vec<ManifestModel>,
}

struct Entry {
    slug: String,
    name: String,
    aliases: Vec<String>,
    capabilities: ModelCapabilities,
    runtime: Runtime,
    compatibility: Compatibility,
}

pub(super) struct Catalog {
    entries: Vec<Entry>,
    custom_capabilities: BTreeMap<String, ModelCapabilities>,
    default: Option<String>,
}

impl Catalog {
    pub fn bundled(custom_models: Vec<CustomModel>) -> Result<Self, String> {
        Self::decode(
            include_str!("../../../../third-party/T3-Model-Manifest.json"),
            custom_models,
        )
    }

    fn decode(raw: &str, custom_models: Vec<CustomModel>) -> Result<Self, String> {
        let manifest: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        if manifest["version"] != 1 {
            return Err("unsupported model manifest version".into());
        }
        let provider: ProviderCatalog =
            serde_json::from_value(manifest["providers"]["claudeAgent"].clone())
                .map_err(|error| error.to_string())?;
        let mut seen = BTreeSet::new();
        let mut entries = Vec::new();
        for model in provider.models {
            if model.slug.trim().is_empty() || !seen.insert(model.slug.clone()) {
                return Err("invalid or duplicate Claude model slug".into());
            }
            let profile = model
                .profile
                .as_ref()
                .map(|id| {
                    provider
                        .profiles
                        .get(id)
                        .ok_or_else(|| format!("missing Claude model profile: {id}"))
                })
                .transpose()?;
            let (capabilities, runtime) = match profile {
                Some(profile) => (
                    profile.capabilities.clone(),
                    profile.adapter.claude_code.clone(),
                ),
                None => (ModelCapabilities::default(), Runtime::default()),
            };
            entries.push(Entry {
                slug: model.slug,
                name: model.name,
                aliases: model.aliases,
                capabilities,
                runtime,
                compatibility: model.adapter.claude_code,
            });
        }
        if provider
            .defaults
            .chat
            .as_ref()
            .is_some_and(|id| !seen.contains(id))
        {
            return Err("missing Claude default model".into());
        }
        for entry in &mut entries {
            entry.aliases.retain(|alias| {
                !custom_models
                    .iter()
                    .any(|custom| custom.slug.eq_ignore_ascii_case(alias))
            });
        }
        let mut custom_capabilities = BTreeMap::new();
        for custom in custom_models {
            if !seen.contains(&custom.slug)
                && let Some(capabilities) = custom.capabilities
            {
                custom_capabilities
                    .entry(custom.slug)
                    .or_insert(capabilities);
            }
        }
        Ok(Self {
            entries,
            custom_capabilities,
            default: provider.defaults.chat,
        })
    }

    pub fn default_model(&self) -> &str {
        self.default
            .as_deref()
            .or_else(|| self.entries.first().map(|entry| entry.slug.as_str()))
            .unwrap_or("default")
    }

    fn entry(&self, model: &str) -> Option<&Entry> {
        let model = model.trim();
        self.entries
            .iter()
            .find(|entry| entry.slug == model)
            .or_else(|| {
                self.entries.iter().find(|entry| {
                    entry
                        .aliases
                        .iter()
                        .any(|alias| alias.eq_ignore_ascii_case(model))
                })
            })
    }

    pub fn models(
        &self,
        version: Option<&Version>,
        instance_id: &ProviderInstanceId,
    ) -> Vec<Model> {
        self.entries
            .iter()
            .filter(|entry| supported(&entry.compatibility, version))
            .map(|entry| Model {
                id: entry.slug.clone(),
                model: ModelRef {
                    instance_id: instance_id.clone(),
                    id: entry.slug.clone(),
                },
                display_name: entry.name.clone(),
                capabilities: entry.capabilities.clone(),
                is_custom: false,
                is_default: Some(self.default.as_deref() == Some(entry.slug.as_str())),
            })
            .collect()
    }

    fn capabilities(&self, model: &str) -> Option<&ModelCapabilities> {
        self.entry(model)
            .map(|entry| &entry.capabilities)
            .or_else(|| self.custom_capabilities.get(model.trim()))
    }

    pub fn prompt_effort<'a>(
        &self,
        model: &str,
        options: &'a [ModelOptionSelection],
    ) -> Option<&'a str> {
        model_option_string(options, "effort").filter(|raw| {
            self.capabilities(model).is_some_and(|capabilities| capabilities.option_descriptors.iter().any(|descriptor| {
                matches!(&descriptor.kind, ModelOptionKind::Select { prompt_injected_values, .. } if prompt_injected_values.iter().any(|value| value == raw))
            }))
        })
    }

    pub fn compile(&self, model: &str, options: &[ModelOptionSelection]) -> Selection {
        let entry = self.entry(model);
        let capabilities = self.capabilities(model);
        let descriptor = |id: &str| {
            capabilities.and_then(|capabilities| {
                capabilities
                    .option_descriptors
                    .iter()
                    .find(|descriptor| descriptor.id == id)
            })
        };
        let value = |id: &str| descriptor(id).and_then(|descriptor| descriptor.value(options));
        let mut api_model = entry.map_or_else(
            || {
                self.custom_capabilities
                    .get_key_value(model.trim())
                    .map_or_else(|| model.to_owned(), |(slug, _)| slug.clone())
            },
            |entry| entry.slug.clone(),
        );
        if let Some(entry) = entry {
            for (id, suffixes) in &entry.runtime.model_suffixes {
                if let Some(ModelOptionValue::String(value)) = value(id)
                    && let Some(suffix) = suffixes.get(&value).filter(|suffix| !suffix.is_empty())
                {
                    api_model.push_str(suffix);
                    break;
                }
            }
        }
        let resolved_effort = match value("effort") {
            Some(ModelOptionValue::String(value)) => Some(value),
            _ => None,
        };
        let effort = resolved_effort.as_ref().and_then(|effort| {
            entry
                .and_then(|entry| entry.runtime.effort_map.get(effort))
                .cloned()
                .unwrap_or_else(|| Some(effort.clone()))
        });
        let mut settings = BTreeMap::new();
        for (id, key) in [
            ("thinking", "alwaysThinkingEnabled"),
            ("fastMode", "fastMode"),
        ] {
            if descriptor(id).is_some_and(|descriptor| {
                matches!(descriptor.kind, ModelOptionKind::Boolean { .. })
            }) && let Some(value) = model_option_boolean(options, id)
            {
                settings.insert(key.into(), value);
            }
        }
        if resolved_effort.as_deref() == Some("ultracode") {
            settings.insert("ultracode".into(), true);
        }
        let prompt_effort = self.prompt_effort(model, options).map(str::to_owned);
        Selection {
            query: Query {
                model: api_model,
                effort,
                settings,
            },
            prompt_effort,
        }
    }
}

fn supported(compatibility: &Compatibility, version: Option<&Version>) -> bool {
    let Some(version) = version else {
        return compatibility.min_version.is_none()
            && compatibility.max_version_exclusive.is_none();
    };
    compatibility
        .min_version
        .as_ref()
        .is_none_or(|min| !version.cmp_precedence(min).is_lt())
        && compatibility
            .max_version_exclusive
            .as_ref()
            .is_none_or(|max| version.cmp_precedence(max).is_lt())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn options(value: Value) -> Vec<ModelOptionSelection> {
        serde_json::from_value(value).unwrap()
    }
    fn catalog() -> Catalog {
        Catalog::bundled(Vec::new()).unwrap()
    }

    #[test]
    fn canonical_models_follow_version_gates_and_manifest_default() {
        let catalog = catalog();
        assert_eq!(catalog.default_model(), "claude-fable-5-1");
        let instance = "claude".parse().unwrap();
        let models = catalog.models(Some(&Version::parse("2.1.266").unwrap()), &instance);
        assert_eq!(models.len(), 10);
        assert!(
            !models
                .iter()
                .any(|model| model.model.id == "claude-opus-5-5")
        );
        assert_eq!(
            models
                .iter()
                .find(|model| model.is_default == Some(true))
                .unwrap()
                .model
                .id,
            "claude-fable-5-1"
        );
        let newer = catalog.models(Some(&Version::parse("2.1.284").unwrap()), &instance);
        assert_eq!(newer.len(), 12);
        assert_eq!(newer[0].model.id, "claude-opus-5-5");
        assert_eq!(newer[1].model.id, "claude-sonnet-5-5");
        assert_eq!(catalog.models(None, &instance).len(), 5);
    }

    #[test]
    fn native_query_compiles_alias_context_effort_and_explicit_false() {
        let selection = catalog().compile(
            "OPUS",
            &options(json!([
                {"id":"effort","value":"ultracode"}, {"id":"contextWindow","value":"1m"},
                {"id":"fastMode","value":false}, {"id":"thinking","value":true}
            ])),
        );
        assert_eq!(selection.query.model, "claude-opus-5[1m]");
        assert_eq!(selection.query.effort.as_deref(), Some("xhigh"));
        assert_eq!(
            selection.query.settings,
            BTreeMap::from([("fastMode".into(), false), ("ultracode".into(), true)])
        );
        assert_eq!(selection.prompt_effort, None);
        let haiku = catalog().compile(
            "haiku",
            &options(json!([
                {"id":"thinking","value":false}, {"id":"fastMode","value":true}
            ])),
        );
        assert_eq!(haiku.query.model, "claude-haiku-4-5");
        assert_eq!(
            haiku.query.settings,
            BTreeMap::from([("alwaysThinkingEnabled".into(), false)])
        );
        assert_eq!(haiku.query.effort, None);
    }

    #[test]
    fn prompt_effort_changes_content_without_changing_native_query_identity() {
        let catalog = catalog();
        let ordinary = catalog.compile("fable", &[]);
        let prompt = catalog.compile(
            "fable",
            &options(json!([{"id":"effort","value":"ultrathink"}])),
        );
        assert_eq!(ordinary.query, prompt.query);
        assert_eq!(ordinary.query.model, "claude-fable-5-1[1m]");
        assert_eq!(ordinary.query.effort.as_deref(), Some("medium"));
        assert_eq!(prompt.prompt_effort.as_deref(), Some("ultrathink"));
        assert_eq!(
            agent_protocol::model_prompt::apply_prompt_effort(
                "  investigate  ",
                prompt.prompt_effort.as_deref()
            ),
            "Ultrathink:\ninvestigate"
        );
    }

    #[test]
    fn custom_aliases_stay_opaque_and_only_declared_capabilities_compile() {
        let custom = read_custom_models(Some(&json!([
            "Opus", {"slug":"Custom","name":"Private model","capabilities":{"optionDescriptors":[
                {"id":"effort","label":"Effort","type":"select","options":[]},
                {"id":"fastMode","label":"Fast","type":"boolean"}
            ]}}
        ])));
        let catalog = Catalog::bundled(custom).unwrap();
        let unknown = catalog.compile("Opus", &options(json!([{"id":"effort","value":"max"}])));
        assert_eq!(unknown.query.model, "Opus");
        assert_eq!(unknown.query.effort, None);
        let custom = catalog.compile(
            "Custom",
            &options(json!([
                {"id":"effort","value":"bespoke"}, {"id":"fastMode","value":false}
            ])),
        );
        assert_eq!(custom.query.model, "Custom");
        assert_eq!(custom.query.effort.as_deref(), Some("bespoke"));
        assert_eq!(
            custom.query.settings,
            BTreeMap::from([("fastMode".into(), false)])
        );
        assert_eq!(
            catalog.compile("claude-opus-5", &[]).query.model,
            "claude-opus-5[1m]"
        );
        assert_eq!(catalog.compile("custom", &[]).query.model, "custom");
    }

    proptest::proptest! {
        #[test]
        fn compatibility_has_inclusive_minimum_and_exclusive_maximum(patch in 0u64..400) {
            let compatibility = Compatibility {
                min_version: Some(Version::new(2,1,111)), max_version_exclusive: Some(Version::new(2,1,280)),
            };
            proptest::prop_assert_eq!(supported(&compatibility, Some(&Version::new(2,1,patch))), (111..280).contains(&patch));
            proptest::prop_assert!(!supported(&compatibility, None));
        }

    }
}

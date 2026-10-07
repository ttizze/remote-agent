//! The model catalog clients pick from: Claude models from the bundled model
//! manifest, Codex models from the app-server's `model/list`, each with the
//! option descriptors the composer offers.
use agent_domain::{BooleanOption, OptionChoice, OptionDescriptor, SelectOption};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// One model as clients list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub slug: String,
    pub name: String,
    pub aliases: Vec<String>,
    /// `new` for a recently added model.
    pub badge: Option<String>,
    pub is_default: bool,
    pub is_legacy: bool,
    pub descriptors: Vec<OptionDescriptor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    current_models: BTreeMap<String, Vec<String>>,
    claude: ClaudeManifest,
}
#[derive(Deserialize)]
struct ClaudeManifest {
    defaults: Defaults,
    profiles: BTreeMap<String, Profile>,
    models: Vec<ManifestModel>,
}
#[derive(Deserialize)]
struct Defaults {
    chat: String,
}
#[derive(Deserialize)]
struct Profile {
    capabilities: Capabilities,
    adapter: ProfileAdapter,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capabilities {
    option_descriptors: Vec<ManifestDescriptor>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ManifestDescriptor {
    #[serde(rename_all = "camelCase")]
    Select {
        id: String,
        label: String,
        description: Option<String>,
        options: Vec<ManifestChoice>,
        current_value: Option<String>,
        #[serde(default)]
        prompt_injected_values: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Boolean {
        id: String,
        label: String,
        description: Option<String>,
        current_value: Option<bool>,
    },
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestChoice {
    id: String,
    label: String,
    description: Option<String>,
    #[serde(default)]
    is_default: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProfileAdapter {
    claude_code: ClaudeProfile,
}
/// How a profile's options reach Claude Code.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClaudeProfile {
    /// Effort values sent under another native name; `null` sends no effort.
    #[serde(default)]
    pub(crate) effort_map: BTreeMap<String, Option<String>>,
    /// Model id suffixes per option value, e.g. `[1m]` for a 1M window.
    #[serde(default)]
    pub(crate) model_suffixes: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub(crate) context_window_tokens: BTreeMap<String, u64>,
    pub(crate) fixed_context_window_tokens: Option<u64>,
}
#[derive(Deserialize)]
struct ManifestModel {
    slug: String,
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    status: String,
    badge: Option<String>,
    profile: String,
    adapter: Option<ModelAdapter>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelAdapter {
    claude_code: Option<Compatibility>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Compatibility {
    min_version: Option<String>,
}

static MANIFEST: LazyLock<Manifest> = LazyLock::new(|| {
    serde_json::from_str(include_str!("model_manifest.json")).expect("bundled manifest is valid")
});

impl From<&ManifestDescriptor> for OptionDescriptor {
    fn from(descriptor: &ManifestDescriptor) -> Self {
        match descriptor {
            ManifestDescriptor::Select {
                id,
                label,
                description,
                options,
                current_value,
                prompt_injected_values,
            } => Self::Select(SelectOption {
                id: id.clone(),
                label: label.clone(),
                description: description.clone(),
                options: options
                    .iter()
                    .map(|choice| OptionChoice {
                        id: choice.id.clone(),
                        label: choice.label.clone(),
                        description: choice.description.clone(),
                        is_default: choice.is_default,
                    })
                    .collect(),
                current_value: current_value.clone(),
                prompt_injected_values: prompt_injected_values.clone(),
            }),
            ManifestDescriptor::Boolean {
                id,
                label,
                description,
                current_value,
            } => Self::Boolean(BooleanOption {
                id: id.clone(),
                label: label.clone(),
                description: description.clone(),
                current_value: *current_value,
            }),
        }
    }
}

/// A Claude catalog entry with its launch profile.
pub(crate) struct ClaudeEntry {
    pub(crate) model: CatalogModel,
    pub(crate) profile: &'static ClaudeProfile,
    min_version: Option<&'static str>,
}

static CLAUDE: LazyLock<Vec<ClaudeEntry>> = LazyLock::new(|| {
    let manifest = &*MANIFEST;
    manifest
        .claude
        .models
        .iter()
        .map(|entry| {
            let profile = &manifest.claude.profiles[&entry.profile];
            ClaudeEntry {
                model: CatalogModel {
                    slug: entry.slug.clone(),
                    name: entry.name.clone(),
                    aliases: entry.aliases.clone(),
                    badge: entry.badge.clone(),
                    is_default: entry.slug == manifest.claude.defaults.chat,
                    is_legacy: is_legacy("claude", &entry.slug),
                    descriptors: profile
                        .capabilities
                        .option_descriptors
                        .iter()
                        .map(OptionDescriptor::from)
                        .collect(),
                },
                profile: &profile.adapter.claude_code,
                min_version: entry
                    .adapter
                    .as_ref()
                    .and_then(|adapter| adapter.claude_code.as_ref())
                    .and_then(|compatibility| compatibility.min_version.as_deref()),
            }
        })
        .collect()
});

/// Legacy unless the manifest lists the model as current (by family for Codex).
fn is_legacy(driver: &str, slug: &str) -> bool {
    let manifest = &*MANIFEST;
    let family = if driver == "codex" {
        codex_model_family(slug)
    } else {
        slug
    };
    if driver == "claude"
        && let Some(model) = manifest
            .claude
            .models
            .iter()
            .find(|model| model.slug == slug || model.slug == family)
    {
        return model.status == "legacy";
    }
    manifest
        .current_models
        .get(driver)
        .is_some_and(|current| !current.iter().any(|model| model == slug || model == family))
}

/// The catalog entry for a slug or a case-insensitive alias.
pub(crate) fn claude_entry(slug_or_alias: &str) -> Option<&'static ClaudeEntry> {
    let value = slug_or_alias.trim();
    if value.is_empty() {
        return None;
    }
    CLAUDE
        .iter()
        .find(|entry| entry.model.slug == value)
        .or_else(|| {
            CLAUDE.iter().find(|entry| {
                entry
                    .model
                    .aliases
                    .iter()
                    .any(|alias| alias.eq_ignore_ascii_case(value))
            })
        })
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .split(['.', '-', '+'])
        .take(3)
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

fn older(version: &str, minimum: &str) -> bool {
    version_parts(version) < version_parts(minimum)
}

/// The first `x.y.z` in a CLI's `--version` output.
pub fn cli_version(output: &str) -> Option<String> {
    output
        .split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .find_map(|token| {
            let parts: Vec<&str> = token.split('.').collect();
            (parts.len() >= 3 && parts[..3].iter().all(|part| !part.is_empty()))
                .then(|| parts[..3].join("."))
        })
}

/// The Claude models the installed Claude Code supports; a model with a
/// minimum version needs a known version.
pub fn claude_catalog(version: Option<&str>) -> Vec<CatalogModel> {
    CLAUDE
        .iter()
        .filter(|entry| match (entry.min_version, version) {
            (None, _) => true,
            (Some(minimum), Some(version)) => !older(version, minimum),
            (Some(_), None) => false,
        })
        .map(|entry| entry.model.clone())
        .collect()
}

/// Names the oldest model the installed Claude Code is too old for.
pub fn claude_upgrade_message(version: Option<&str>) -> Option<String> {
    let mut unavailable: Vec<(&str, &str)> = CLAUDE
        .iter()
        .filter_map(|entry| {
            let minimum = entry.min_version?;
            version
                .is_none_or(|version| older(version, minimum))
                .then_some((minimum, entry.model.name.as_str()))
        })
        .collect();
    unavailable.sort_by(|a, b| version_parts(a.0).cmp(&version_parts(b.0)));
    let (minimum, name) = unavailable.first()?;
    let installed = version.map_or_else(|| "the installed version".to_owned(), |v| format!("v{v}"));
    Some(format!(
        "Claude Code {installed} is too old for {name}. Upgrade to v{minimum} or newer to access it."
    ))
}

/// Codex's preferred default models, most preferred first.
const PREFERRED_CODEX_DEFAULTS: [&str; 3] = ["gpt-6-astra", "gpt-5.6-sol", "gpt-5.6-terra"];
const DEFAULT_SERVICE_TIER: &str = "default";

fn codex_model_family(slug: &str) -> &str {
    if slug.starts_with("openai.gpt-") {
        &slug["openai.".len()..]
    } else {
        slug
    }
}

/// `gpt-6-astra` reads `GPT-6-Astra`.
pub fn codex_model_name(name: &str) -> String {
    let name = match name.get(..3) {
        Some(prefix) if prefix.eq_ignore_ascii_case("gpt") => format!("GPT{}", &name[3..]),
        _ => name.to_owned(),
    };
    let mut result = String::with_capacity(name.len());
    let mut after_dash = false;
    for c in name.chars() {
        if after_dash && c.is_ascii_lowercase() {
            result.push(c.to_ascii_uppercase());
        } else {
            result.push(c);
        }
        after_dash = c == '-';
    }
    result
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
        other => other,
    }
    .to_owned()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeCodexModel {
    model: String,
    display_name: String,
    #[serde(default)]
    supported_reasoning_efforts: Vec<NativeEffort>,
    #[serde(default)]
    default_reasoning_effort: Option<String>,
    #[serde(default)]
    service_tiers: Option<Vec<NativeTier>>,
    #[serde(default)]
    additional_speed_tiers: Option<Vec<String>>,
    #[serde(default)]
    default_service_tier: Option<String>,
    #[serde(default)]
    is_default: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeEffort {
    reasoning_effort: String,
}
#[derive(Deserialize)]
struct NativeTier {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
}

fn codex_descriptors(model: &NativeCodexModel) -> Vec<OptionDescriptor> {
    let default_effort = if codex_model_family(&model.model) == "gpt-6-astra" {
        Some("medium")
    } else {
        model.default_reasoning_effort.as_deref()
    };
    let efforts: Vec<OptionChoice> = model
        .supported_reasoning_efforts
        .iter()
        .map(|effort| OptionChoice {
            id: effort.reasoning_effort.clone(),
            label: effort_label(&effort.reasoning_effort),
            description: None,
            is_default: Some(effort.reasoning_effort.as_str()) == default_effort,
        })
        .collect();
    let tiers: Vec<NativeTier> = match &model.service_tiers {
        Some(tiers) if !tiers.is_empty() => tiers
            .iter()
            .map(|tier| NativeTier {
                id: tier.id.clone(),
                name: tier.name.clone(),
                description: tier.description.clone(),
            })
            .collect(),
        _ => model
            .additional_speed_tiers
            .iter()
            .flatten()
            .map(|id| NativeTier {
                name: if id == "fast" {
                    "Fast".into()
                } else {
                    id.clone()
                },
                id: id.clone(),
                description: None,
            })
            .collect(),
    };
    let default_tier = model
        .default_service_tier
        .as_deref()
        .filter(|tier| tiers.iter().any(|candidate| candidate.id == *tier))
        .unwrap_or(DEFAULT_SERVICE_TIER)
        .to_owned();
    let mut descriptors = vec![];
    if !efforts.is_empty() {
        descriptors.push(OptionDescriptor::Select(SelectOption {
            id: "reasoningEffort".into(),
            label: "Reasoning".into(),
            description: None,
            current_value: efforts
                .iter()
                .find(|choice| choice.is_default)
                .map(|choice| choice.id.clone()),
            options: efforts,
            prompt_injected_values: vec![],
        }));
    }
    if !tiers.is_empty() {
        let mut options = vec![OptionChoice {
            id: DEFAULT_SERVICE_TIER.into(),
            label: "Standard".into(),
            description: None,
            is_default: default_tier == DEFAULT_SERVICE_TIER,
        }];
        options.extend(tiers.into_iter().map(|tier| {
            let description = match tier.id.as_str() {
                "ultrafast" => Some("Even faster, more expensive".to_owned()),
                _ => tier
                    .description
                    .filter(|description| !description.is_empty()),
            };
            OptionChoice {
                is_default: tier.id == default_tier,
                id: tier.id,
                label: tier.name,
                description,
            }
        }));
        descriptors.push(OptionDescriptor::Select(SelectOption {
            id: "serviceTier".into(),
            label: "Service Tier".into(),
            description: None,
            options,
            current_value: Some(default_tier),
            prompt_injected_values: vec![],
        }));
    }
    descriptors
}

/// Codex's `model/list` entries as catalog models: the preferred default
/// model wins over Codex's flag, and a ChatGPT login that shares tokens offers
/// no service tier.
pub fn codex_catalog(native: &[Value], shares_tokens: bool) -> Result<Vec<CatalogModel>, String> {
    let models: Vec<NativeCodexModel> = native
        .iter()
        .map(|model| serde_json::from_value(model.clone()).map_err(|error| error.to_string()))
        .collect::<Result<_, _>>()?;
    let preferred = PREFERRED_CODEX_DEFAULTS.iter().find_map(|preferred| {
        models
            .iter()
            .find(|model| codex_model_family(&model.model) == *preferred)
            .map(|model| model.model.clone())
    });
    Ok(models
        .iter()
        .map(|model| CatalogModel {
            slug: model.model.clone(),
            name: codex_model_name(&model.display_name),
            aliases: vec![],
            badge: None,
            is_default: preferred
                .as_ref()
                .map_or(model.is_default, |preferred| *preferred == model.model),
            is_legacy: is_legacy("codex", &model.model),
            descriptors: codex_descriptors(model)
                .into_iter()
                .filter(|descriptor| !(shares_tokens && descriptor.id() == "serviceTier"))
                .collect(),
        })
        .collect())
}

#[cfg(test)]
mod tests;

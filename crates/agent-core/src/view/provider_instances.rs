//! Pure editing rules for Host-owned provider instances.
//!
//! The desktop settings form keeps only input widgets and delegates parsing,
//! normalization, and map validation here.  The resulting map is sent as one
//! Host settings patch so an edit or removal cannot leave a partially updated
//! launch configuration behind.

use crate::models::{HostSettings, HostSettingsPatch, ProviderCustomModel, ProviderInstanceConfig};
use agent_protocol::models::MAX_PROVIDER_INSTANCE_ID_LENGTH;
use std::collections::{BTreeMap, BTreeSet};

/// The maximum id length enforced by the protocol's provider instance rule.
pub const MAX_INSTANCE_ID_LENGTH: usize = MAX_PROVIDER_INSTANCE_ID_LENGTH;
/// Trims optional text fields while keeping empty values absent from the
/// persisted settings document.
pub fn normalize_optional(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_owned();
        (!value.is_empty()).then_some(value)
    })
}

/// Trims and de-duplicates a list of model aliases in display order.
pub fn normalize_aliases(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter_map(|value| {
            let value = value.trim().to_owned();
            (!value.is_empty() && seen.insert(value.clone())).then_some(value)
        })
        .collect()
}

/// Parses one environment variable per line (`NAME=value`). Empty lines are
/// ignored; the protocol validates names and the returned map preserves the
/// value, including any `=` characters after the first one.
pub fn parse_environment_lines(input: &str) -> Result<BTreeMap<String, String>, String> {
    let mut environment = BTreeMap::new();
    for (line_number, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!(
                "Environment line {} must use NAME=value.",
                line_number + 1
            ));
        };
        let name = name.trim().to_owned();
        if name.is_empty() {
            return Err(format!(
                "Environment line {} has no variable name.",
                line_number + 1
            ));
        }
        if environment.insert(name.clone(), value.to_owned()).is_some() {
            return Err(format!("Environment variable {name} is repeated."));
        }
    }
    Ok(environment)
}

/// Formats an environment map for the multiline desktop editor.
pub fn format_environment_lines(environment: &BTreeMap<String, String>) -> String {
    environment
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Parses one provider launch argument per line. Empty lines are omitted and
/// surrounding whitespace is removed; arguments themselves are not shell
/// parsed because the Host appends this list to its argument vector directly.
pub fn parse_launch_args(input: &str) -> Vec<String> {
    input
        .lines()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Formats launch arguments for the multiline editor.
pub fn format_launch_args(arguments: &[String]) -> String {
    arguments.join("\n")
}

/// Normalizes the values a provider editor can change and validates the
/// complete instance config using the protocol's shared rules.
pub fn normalize_config(
    instance_id: &str,
    mut config: ProviderInstanceConfig,
) -> Result<ProviderInstanceConfig, String> {
    config.display_name = config.display_name.trim().to_owned();
    config.accent_color = normalize_optional(config.accent_color);
    config.binary_path = normalize_optional(config.binary_path);
    config.home_path = normalize_optional(config.home_path);
    config.environment = config
        .environment
        .into_iter()
        .map(|(name, value)| (name.trim().to_owned(), value))
        .collect();
    config.launch_args = config
        .launch_args
        .into_iter()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .collect();
    config.custom_models = normalize_custom_models(config.custom_models);
    config.validate(instance_id)?;
    Ok(config)
}

/// Normalizes every custom model before the protocol's canonical validator
/// checks required fields, slug length, and uniqueness.
pub fn normalize_custom_models(models: Vec<ProviderCustomModel>) -> Vec<ProviderCustomModel> {
    let mut normalized = Vec::with_capacity(models.len());
    for mut model in models {
        model.slug = model.slug.trim().to_owned();
        model.name = model.name.trim().to_owned();
        if model.name.is_empty() {
            model.name = model.slug.clone();
        }
        model.aliases = normalize_aliases(std::mem::take(&mut model.aliases));
        model.badge = normalize_optional(model.badge.take());
        normalized.push(model);
    }
    normalized
}

/// Validates an instance id before an editor creates or replaces it.
pub fn validate_instance_id(
    instance_id: &str,
    current: &BTreeMap<String, ProviderInstanceConfig>,
    replacing: bool,
) -> Result<(), String> {
    if instance_id.is_empty() {
        return Err("Provider instance id is required.".into());
    }
    if instance_id.len() > MAX_INSTANCE_ID_LENGTH {
        return Err(format!(
            "Provider instance ids must be {MAX_INSTANCE_ID_LENGTH} characters or fewer."
        ));
    }
    if current.contains_key(instance_id) && !replacing {
        return Err(format!("Provider instance {instance_id} already exists."));
    }
    Ok(())
}

/// Validates and replaces one entry in the Host's provider instance map.
pub fn upsert(
    current: &BTreeMap<String, ProviderInstanceConfig>,
    instance_id: String,
    config: ProviderInstanceConfig,
) -> Result<BTreeMap<String, ProviderInstanceConfig>, String> {
    let replacing = current.contains_key(&instance_id);
    validate_instance_id(&instance_id, current, replacing)?;
    let config = normalize_config(&instance_id, config)?;
    let mut next = current.clone();
    next.insert(instance_id, config);
    validate_map(&next)?;
    Ok(next)
}

/// Removes a custom instance. Built-in ids remain available as resettable
/// slots and cannot be removed from the Host settings map through this API.
pub fn remove(
    current: &BTreeMap<String, ProviderInstanceConfig>,
    instance_id: &str,
) -> Result<BTreeMap<String, ProviderInstanceConfig>, String> {
    if matches!(instance_id, "codex" | "claude") {
        return Err("Built-in provider instances can be reset but not removed.".into());
    }
    let mut next = current.clone();
    next.remove(instance_id);
    validate_map(&next)?;
    Ok(next)
}

/// Validates a complete provider map before it crosses the core/Host
/// boundary. This mirrors `HostSettings::validate` without constructing a
/// patch in the caller.
pub fn validate_map(
    provider_instances: &BTreeMap<String, ProviderInstanceConfig>,
) -> Result<(), String> {
    let settings = HostSettings::default().patched(&HostSettingsPatch {
        provider_instances: Some(provider_instances.clone()),
        ..Default::default()
    });
    settings.validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::Driver;
    use proptest::prelude::*;

    fn config() -> ProviderInstanceConfig {
        ProviderInstanceConfig {
            driver: Driver::Codex,
            display_name: " Build ".into(),
            environment: BTreeMap::from([("MODE".into(), "work".into())]),
            launch_args: vec![" --verbose ".into(), "".into()],
            custom_models: vec![ProviderCustomModel {
                slug: " model ".into(),
                name: " ".into(),
                aliases: vec![" alias ".into(), "alias".into()],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn normalizes_editor_values_before_validation() {
        let normalized = normalize_config("build", config()).unwrap();
        assert_eq!(normalized.display_name, "Build");
        assert_eq!(normalized.launch_args, ["--verbose"]);
        assert_eq!(normalized.custom_models[0].slug, "model");
        assert_eq!(normalized.custom_models[0].name, "model");
        assert_eq!(normalized.custom_models[0].aliases, ["alias"]);
    }

    #[test]
    fn environment_lines_preserve_values_after_first_equals() {
        let parsed = parse_environment_lines("TOKEN=a=b\n MODE = work ").unwrap();
        assert_eq!(parsed["TOKEN"], "a=b");
        assert_eq!(parsed["MODE"], " work ");
        assert_eq!(
            parse_environment_lines("bad").unwrap_err(),
            "Environment line 1 must use NAME=value."
        );
    }

    #[test]
    fn upsert_and_remove_keep_the_map_atomic() {
        let current = BTreeMap::new();
        let next = upsert(&current, "build".into(), config()).unwrap();
        assert!(current.is_empty());
        assert!(next.contains_key("build"));
        assert!(remove(&next, "build").unwrap().is_empty());
        assert!(remove(&next, "codex").is_err());
    }

    #[test]
    fn duplicate_custom_models_are_rejected_without_mutating_input() {
        let models = vec![
            ProviderCustomModel {
                slug: "model".into(),
                name: "First".into(),
                ..Default::default()
            },
            ProviderCustomModel {
                slug: " model ".into(),
                name: "Second".into(),
                ..Default::default()
            },
        ];
        assert!(
            normalize_config(
                "build",
                ProviderInstanceConfig {
                    custom_models: models.clone(),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert_eq!(models[0].name, "First");
    }

    proptest! {
        #[test]
        fn launch_argument_formatting_is_stable(arguments in prop::collection::vec("[a-zA-Z0-9_./:-]{0,16}", 0..12)) {
            let formatted = format_launch_args(&arguments);
            let expected = arguments
                .iter()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            prop_assert_eq!(parse_launch_args(&formatted), expected);
        }
    }
}

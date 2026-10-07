//! Claude model options compiled into native launch values, from the bundled
//! model manifest.
use crate::model_catalog::claude_entry;
use agent_domain::{ModelSelection, OptionDescriptor, option_value, prompt_injected_value};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeModelOptions {
    /// The model sent to the CLI, with its context-window suffix.
    pub model: String,
    pub effort: Option<String>,
    /// `ultrathink` is requested in the prompt rather than as an effort.
    pub prompt_effort: Option<String>,
    pub settings: Value,
    /// The window reported with turn usage.
    pub context_window: u64,
    /// The catalog window for handoff budgets, when the catalog knows it.
    pub model_window: Option<u64>,
}

pub fn claude_model_options(selection: &ModelSelection) -> ClaudeModelOptions {
    let entry = claude_entry(&selection.model);
    let descriptors: &[OptionDescriptor] = entry.map_or(&[], |entry| &entry.model.descriptors);
    let options = &selection.options;
    let raw_effort = options.get("effort").map(String::as_str);
    let resolved = option_value(descriptors, options, "effort");
    let effort = resolved.clone().and_then(|effort| {
        match entry.and_then(|entry| entry.profile.effort_map.get(&effort)) {
            Some(mapped) => mapped.clone(),
            None => Some(effort),
        }
    });
    let offers = |id: &str| {
        descriptors
            .iter()
            .any(|descriptor| matches!(descriptor, OptionDescriptor::Boolean(boolean) if boolean.id == id))
    };
    let boolean = |id: &str| match options.get(id).map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    };
    let mut settings = json!({});
    if offers("thinking")
        && let Some(thinking) = boolean("thinking")
    {
        settings["alwaysThinkingEnabled"] = json!(thinking);
    }
    if offers("fastMode")
        && let Some(fast) = boolean("fastMode")
    {
        settings["fastMode"] = json!(fast);
    }
    if resolved.as_deref() == Some("ultracode") {
        settings["ultracode"] = json!(true);
    }
    let slug = entry.map_or(selection.model.as_str(), |entry| &entry.model.slug);
    let suffix = entry.and_then(|entry| {
        entry
            .profile
            .model_suffixes
            .iter()
            .find_map(|(option, suffixes)| {
                suffixes
                    .get(&option_value(descriptors, options, option)?)
                    .cloned()
            })
    });
    let window = option_value(descriptors, options, "contextWindow");
    ClaudeModelOptions {
        model: format!("{slug}{}", suffix.unwrap_or_default()),
        effort,
        prompt_effort: prompt_injected_value(descriptors, raw_effort),
        settings,
        model_window: entry.and_then(|entry| {
            entry.profile.fixed_context_window_tokens.or_else(|| {
                window
                    .as_ref()
                    .and_then(|window| entry.profile.context_window_tokens.get(window).copied())
            })
        }),
        // The reported turn window keeps the reference's fixed 1M models.
        context_window: if matches!(slug, "claude-opus-4-6" | "claude-opus-4-7")
            || window.as_deref() == Some("1m")
        {
            1_000_000
        } else {
            200_000
        },
    }
}
/// `Ultrathink:` prefixes a prompt requesting that effort, except commands.
pub fn claude_prompt_effort(text: &str, prompt_effort: Option<&str>) -> String {
    let trimmed = text.trim();
    let command = trimmed.strip_prefix('/').is_some_and(|rest| {
        let name = rest.split(char::is_whitespace).next().unwrap_or_default();
        !name.is_empty() && !name.contains('/')
    });
    if trimmed.is_empty()
        || prompt_effort != Some("ultrathink")
        || command
        || trimmed.starts_with("Ultrathink:")
    {
        text.to_owned()
    } else {
        format!("Ultrathink:\n{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::Driver;
    fn selection(model: &str, options: &[(&str, &str)]) -> ModelSelection {
        ModelSelection {
            instance: "claude".into(),
            driver: Driver::Claude,
            model: model.into(),
            options: options
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        }
    }
    #[test]
    fn model_options_compile_suffixes_effort_and_settings() {
        let fable = claude_model_options(&selection(
            "claude-fable-5",
            &[("contextWindow", "1m"), ("effort", "ultracode")],
        ));
        assert_eq!(fable.model, "claude-fable-5[1m]");
        assert_eq!(fable.effort.as_deref(), Some("xhigh"));
        assert_eq!(fable.settings, json!({"ultracode":true}));
        assert_eq!(fable.context_window, 1_000_000);
        for fast in ["true", "false"] {
            assert_eq!(
                claude_model_options(&selection("claude-opus-4-6", &[("fastMode", fast)])).settings,
                json!({"fastMode": fast == "true"})
            );
        }
        let sonnet =
            claude_model_options(&selection("claude-sonnet-4-6", &[("effort", "ultrathink")]));
        assert_eq!(sonnet.effort.as_deref(), Some("high"));
        assert_eq!(sonnet.prompt_effort.as_deref(), Some("ultrathink"));
        assert_eq!(sonnet.model, "claude-sonnet-4-6");
        assert_eq!(sonnet.context_window, 200_000);
        assert_eq!(
            claude_model_options(&selection("claude-haiku-4-5", &[("thinking", "false")])).settings,
            json!({"alwaysThinkingEnabled":false})
        );
        let opus = claude_model_options(&selection("claude-opus-4-8", &[]));
        assert_eq!(
            (opus.context_window, opus.model_window),
            (200_000, Some(1_000_000))
        );
        assert_eq!(
            claude_prompt_effort("/compact", Some("ultrathink")),
            "/compact"
        );
        assert_eq!(
            claude_prompt_effort("Investigate this failure", Some("ultrathink")),
            "Ultrathink:\nInvestigate this failure"
        );
        assert_eq!(
            claude_prompt_effort("/home/theo/app.ts crashed on load", Some("ultrathink")),
            "Ultrathink:\n/home/theo/app.ts crashed on load"
        );
    }
}

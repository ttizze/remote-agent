//! Claude model options compiled into native launch values, from the model
//! catalog the reference client ships.
use agent_domain::ModelSelection;
use serde_json::{Value, json};

struct Profile {
    models: &'static [&'static str],
    efforts: &'static [&'static str],
    default_effort: Option<&'static str>,
    /// Effort values sent under another native name; `None` sends no effort.
    effort_map: &'static [(&'static str, Option<&'static str>)],
    /// Context window options with the `[1m]` model suffix; the first is the default.
    context_windows: &'static [&'static str],
    fixed_window: Option<u64>,
    fast_mode: bool,
    thinking: bool,
}
const FABLE_EFFORTS: &[&str] = &[
    "low",
    "medium",
    "high",
    "xhigh",
    "max",
    "ultracode",
    "ultrathink",
];
const FABLE_MAP: &[(&str, Option<&str>)] = &[("ultracode", Some("xhigh")), ("ultrathink", None)];
const PROFILES: &[Profile] = &[
    Profile {
        models: &["claude-fable-5-1", "claude-fable-5"],
        efforts: FABLE_EFFORTS,
        default_effort: Some("medium"),
        effort_map: FABLE_MAP,
        context_windows: &["1m", "200k"],
        fixed_window: None,
        fast_mode: false,
        thinking: false,
    },
    Profile {
        models: &["claude-opus-5-5"],
        efforts: FABLE_EFFORTS,
        default_effort: Some("medium"),
        effort_map: FABLE_MAP,
        context_windows: &["1m"],
        fixed_window: None,
        fast_mode: true,
        thinking: false,
    },
    Profile {
        models: &["claude-opus-5"],
        efforts: FABLE_EFFORTS,
        default_effort: Some("high"),
        effort_map: FABLE_MAP,
        context_windows: &["1m"],
        fixed_window: None,
        fast_mode: true,
        thinking: false,
    },
    Profile {
        models: &["claude-opus-4-8"],
        efforts: FABLE_EFFORTS,
        default_effort: Some("high"),
        effort_map: FABLE_MAP,
        context_windows: &[],
        fixed_window: Some(1_000_000),
        fast_mode: true,
        thinking: false,
    },
    Profile {
        models: &["claude-opus-4-7"],
        efforts: &["low", "medium", "high", "xhigh", "max", "ultrathink"],
        default_effort: Some("xhigh"),
        effort_map: &[("xhigh", Some("max")), ("ultrathink", None)],
        context_windows: &[],
        fixed_window: Some(1_000_000),
        fast_mode: true,
        thinking: false,
    },
    Profile {
        models: &["claude-opus-4-6"],
        efforts: &["low", "medium", "high", "max", "ultrathink"],
        default_effort: Some("high"),
        effort_map: &[("ultrathink", None)],
        context_windows: &["1m"],
        fixed_window: None,
        fast_mode: true,
        thinking: false,
    },
    Profile {
        models: &["claude-opus-4-5"],
        efforts: &["low", "medium", "high", "max"],
        default_effort: Some("high"),
        effort_map: &[],
        context_windows: &[],
        fixed_window: None,
        fast_mode: true,
        thinking: false,
    },
    Profile {
        models: &["claude-sonnet-5", "claude-sonnet-5-5"],
        efforts: &["low", "medium", "high", "xhigh", "max", "ultrathink"],
        default_effort: Some("high"),
        effort_map: &[("ultrathink", None)],
        context_windows: &["200k", "1m"],
        fixed_window: None,
        fast_mode: false,
        thinking: false,
    },
    Profile {
        models: &["claude-sonnet-4-6"],
        efforts: &["low", "medium", "high", "max", "ultrathink"],
        default_effort: Some("high"),
        effort_map: &[("max", Some("high")), ("ultrathink", None)],
        context_windows: &["200k"],
        fixed_window: None,
        fast_mode: false,
        thinking: false,
    },
    Profile {
        models: &["claude-haiku-4-5"],
        efforts: &[],
        default_effort: None,
        effort_map: &[],
        context_windows: &[],
        fixed_window: None,
        fast_mode: false,
        thinking: true,
    },
];
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
fn boolean(selection: &ModelSelection, key: &str) -> Option<bool> {
    match selection.options.get(key).map(String::as_str) {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}
pub fn claude_model_options(selection: &ModelSelection) -> ClaudeModelOptions {
    let profile = PROFILES
        .iter()
        .find(|profile| profile.models.contains(&selection.model.as_str()));
    let raw_effort = selection.options.get("effort").map(String::as_str);
    let resolved = profile.and_then(|profile| {
        raw_effort
            .filter(|effort| *effort != "ultrathink" && profile.efforts.contains(effort))
            .or(profile.default_effort)
    });
    let effort = resolved.and_then(|effort| {
        profile
            .and_then(|profile| profile.effort_map.iter().find(|(from, _)| *from == effort))
            .map_or(Some(effort), |(_, to)| *to)
    });
    let prompt_effort = (raw_effort == Some("ultrathink")
        && profile.is_some_and(|profile| profile.efforts.contains(&"ultrathink")))
    .then(|| "ultrathink".to_owned());
    let mut settings = json!({});
    if profile.is_some_and(|profile| profile.thinking)
        && let Some(thinking) = boolean(selection, "thinking")
    {
        settings["alwaysThinkingEnabled"] = json!(thinking);
    }
    if profile.is_some_and(|profile| profile.fast_mode)
        && let Some(fast) = boolean(selection, "fastMode")
    {
        settings["fastMode"] = json!(fast);
    }
    if resolved == Some("ultracode") {
        settings["ultracode"] = json!(true);
    }
    let window = profile.and_then(|profile| {
        let requested = selection.options.get("contextWindow").map(String::as_str);
        requested
            .filter(|value| profile.context_windows.contains(value))
            .or(profile.context_windows.first().copied())
    });
    let model = if window == Some("1m") {
        format!("{}[1m]", selection.model)
    } else {
        selection.model.clone()
    };
    ClaudeModelOptions {
        model,
        effort: effort.map(str::to_owned),
        prompt_effort,
        settings,
        model_window: profile
            .and_then(|profile| profile.fixed_window)
            .or(match window {
                Some("1m") => Some(1_000_000),
                Some("200k") => Some(200_000),
                _ => None,
            }),
        // The reported turn window keeps the reference's fixed 1M models.
        context_window: if matches!(
            selection.model.as_str(),
            "claude-opus-4-6" | "claude-opus-4-7"
        ) || window == Some("1m")
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

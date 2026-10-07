//! The skills and slash commands each provider instance offers per workspace,
//! kept for the 16 most recent directories of each instance.
use agent_protocol::workspace::{ProviderCommands, ProviderSkill, SlashCommand};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

const MAX_DIRECTORIES_PER_INSTANCE: usize = 16;

pub(super) fn compact() -> SlashCommand {
    SlashCommand {
        name: "compact".into(),
        description: Some("Summarize the conversation and reduce context usage".into()),
        input_hint: None,
    }
}

/// Codex's own commands; a shared ChatGPT login cannot send feedback.
pub(super) fn codex_commands(shares_tokens: bool) -> Vec<SlashCommand> {
    let mut commands = vec![compact()];
    if !shares_tokens {
        commands.push(SlashCommand {
            name: "feedback".into(),
            description: Some("Send this thread and Codex logs to OpenAI".into()),
            input_hint: Some("Describe the issue (optional)".into()),
        });
    }
    commands
}

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// One command per case-insensitive name; a later duplicate fills in what the
/// first lacks.
pub(super) fn dedupe(commands: Vec<SlashCommand>) -> Vec<SlashCommand> {
    let mut deduped: Vec<SlashCommand> = vec![];
    for command in commands {
        let name = command.name.trim().to_owned();
        if name.is_empty() {
            continue;
        }
        match deduped
            .iter_mut()
            .find(|existing| existing.name.eq_ignore_ascii_case(&name))
        {
            Some(existing) => {
                if existing.description.is_none() {
                    existing.description = command.description;
                }
                if existing.input_hint.is_none() {
                    existing.input_hint = command.input_hint;
                }
            }
            None => deduped.push(SlashCommand { name, ..command }),
        }
    }
    deduped
}

/// The commands Claude Code's `initialize` reports, after `/compact`.
pub(super) fn claude_commands(initialized: &Value) -> Vec<SlashCommand> {
    let reported = initialized["commands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|command| {
            Some(SlashCommand {
                name: text(&command["name"])?,
                description: text(&command["description"]),
                input_hint: text(&command["argumentHint"]),
            })
        });
    dedupe(std::iter::once(compact()).chain(reported).collect())
}

/// Codex's `skills/list`: the entry for `cwd`, otherwise every entry's skills.
pub(super) fn codex_skills(response: &Value, cwd: &str) -> Vec<ProviderSkill> {
    let entries = response["data"].as_array().cloned().unwrap_or_default();
    let skills: Vec<Value> = match entries.iter().find(|entry| entry["cwd"] == cwd) {
        Some(entry) => entry["skills"].as_array().cloned().unwrap_or_default(),
        None => entries
            .iter()
            .flat_map(|entry| entry["skills"].as_array().cloned().unwrap_or_default())
            .collect(),
    };
    skills
        .iter()
        .filter_map(|skill| {
            Some(ProviderSkill {
                name: skill["name"].as_str()?.to_owned(),
                path: skill["path"].as_str()?.to_owned(),
                enabled: skill["enabled"].as_bool()?,
                description: text(&skill["description"]),
                scope: text(&skill["scope"]),
                display_name: text(&skill["interface"]["displayName"]),
                short_description: text(&skill["shortDescription"])
                    .or_else(|| text(&skill["interface"]["shortDescription"])),
                user_invocation_only: false,
                user_invocable: true,
            })
        })
        .collect()
}

/// The latest scan per instance and directory, oldest first.
#[derive(Default)]
pub(super) struct CommandCache {
    entries: Mutex<HashMap<String, Vec<ProviderCommands>>>,
}

impl CommandCache {
    /// A complete earlier scan.
    pub(super) fn get(&self, instance: &str, cwd: &str) -> Option<ProviderCommands> {
        self.entries
            .lock()
            .unwrap()
            .get(instance)?
            .iter()
            .find(|entry| entry.cwd == cwd && !entry.slash_commands_pending)
            .cloned()
    }

    /// Stores a scan; a pending one keeps the commands the directory had.
    pub(super) fn put(&self, mut scan: ProviderCommands) -> ProviderCommands {
        let mut entries = self.entries.lock().unwrap();
        let list = entries.entry(scan.instance.clone()).or_default();
        if let Some(index) = list.iter().position(|entry| entry.cwd == scan.cwd) {
            let previous = list.remove(index);
            if scan.slash_commands_pending && !previous.slash_commands.is_empty() {
                scan.slash_commands = previous.slash_commands;
            }
        }
        list.push(scan.clone());
        let excess = list.len().saturating_sub(MAX_DIRECTORIES_PER_INSTANCE);
        list.drain(..excess);
        scan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ClaudeProvider.ts parseClaudeInitializationCommands and dedupeSlashCommands.
    #[test]
    fn claude_commands_follow_compact_and_merge_duplicates() {
        let commands = claude_commands(&json!({"commands": [
            {"name": " review ", "description": "", "argumentHint": "<pr>"},
            {"name": "Review", "description": "Review a pull request"},
            {"name": "compact", "description": "Claude's compact"},
            {"name": "  "}
        ]}));
        assert_eq!(
            commands,
            [
                compact(),
                SlashCommand {
                    name: "review".into(),
                    description: Some("Review a pull request".into()),
                    input_hint: Some("<pr>".into()),
                },
            ]
        );
        assert_eq!(claude_commands(&json!({})), [compact()]);
        assert_eq!(
            codex_commands(false)
                .iter()
                .map(|command| command.name.as_str())
                .collect::<Vec<_>>(),
            ["compact", "feedback"]
        );
        assert_eq!(codex_commands(true), [compact()]);
    }

    // CodexProvider.ts parseCodexSkillsListResponse.
    #[test]
    fn codex_skills_come_from_the_entry_of_the_directory() {
        let response = json!({"data": [
            {"cwd": "/other", "skills": [{"name": "other", "path": "/o", "enabled": true}]},
            {"cwd": "/repo", "skills": [{
                "name": "deploy", "path": "/repo/.codex/skills/deploy", "enabled": false,
                "description": "Deploy", "scope": "repo",
                "interface": {"displayName": "Deploy app", "shortDescription": "Ships it"}
            }]}
        ]});
        let skills = codex_skills(&response, "/repo");
        assert_eq!(
            skills,
            [ProviderSkill {
                name: "deploy".into(),
                path: "/repo/.codex/skills/deploy".into(),
                enabled: false,
                description: Some("Deploy".into()),
                scope: Some("repo".into()),
                display_name: Some("Deploy app".into()),
                short_description: Some("Ships it".into()),
                user_invocation_only: false,
                user_invocable: true,
            }]
        );
        assert_eq!(codex_skills(&response, "/missing").len(), 2);
    }

    // ProviderRegistry.ts upsertProviderWorkspaceSnapshot.
    #[test]
    fn a_pending_scan_keeps_the_directorys_commands_and_the_cache_is_bounded() {
        let cache = CommandCache::default();
        let scan = |cwd: &str, pending: bool, commands: Vec<SlashCommand>| ProviderCommands {
            instance: "claude".into(),
            cwd: cwd.into(),
            slash_commands: commands,
            slash_commands_pending: pending,
            skills: vec![],
        };
        cache.put(scan("/repo", false, vec![compact()]));
        let kept = cache.put(scan("/repo", true, vec![]));
        assert_eq!(kept.slash_commands, [compact()]);
        assert!(cache.get("claude", "/repo").is_none());
        for index in 0..20 {
            cache.put(scan(&format!("/dir-{index}"), false, vec![]));
        }
        assert!(cache.get("claude", "/dir-3").is_none());
        assert!(cache.get("claude", "/dir-19").is_some());
    }
}

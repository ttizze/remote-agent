//! The Claude Code skills a prompt can invoke: one directory
//! per skill with a `SKILL.md`, from `<config dir>/skills` and then
//! `<cwd>/.claude/skills`, the first of a name winning, switched off by the
//! `skillOverrides` of the settings files Claude Code merges.
use agent_providers::{claude_skill_frontmatter, claude_skill_overrides};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where an administrator installs the policy that outranks every other file.
fn managed_settings() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some("/Library/Application Support/ClaudeCode/managed-settings.json".into())
    } else if cfg!(windows) {
        std::env::var_os("PROGRAMDATA")
            .filter(|value| !value.is_empty())
            .map(|data| {
                Path::new(&data)
                    .join("ClaudeCode")
                    .join("managed-settings.json")
            })
    } else {
        Some("/etc/claude-code/managed-settings.json".into())
    }
}

/// The nearest ancestor of `cwd` (inclusive) holding a `.git` entry.
fn repository_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_owned)
}

/// Settings files in increasing precedence: user, project, project-local, the
/// repository root's local file from a nested workspace, then managed policy.
fn settings_paths(config_dir: &Path, cwd: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = vec![config_dir.join("settings.json")];
    if let Some(cwd) = cwd {
        paths.push(cwd.join(".claude").join("settings.json"));
        paths.push(cwd.join(".claude").join("settings.local.json"));
        if let Some(root) = repository_root(cwd).filter(|root| root != cwd) {
            paths.push(root.join(".claude").join("settings.local.json"));
        }
    }
    paths.extend(managed_settings());
    paths
}

/// Enabled skills a user may invoke, by name; read again for every prompt.
pub(crate) fn user_invocable_skills(config_dir: &Path, cwd: Option<&Path>) -> Vec<String> {
    let mut overrides = BTreeMap::new();
    for path in settings_paths(config_dir, cwd) {
        if let Ok(contents) = std::fs::read_to_string(path) {
            overrides.extend(claude_skill_overrides(&contents));
        }
    }
    let mut roots = vec![config_dir.join("skills")];
    roots.extend(cwd.map(|cwd| cwd.join(".claude").join("skills")));
    let mut skills = BTreeMap::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for entry in names {
            let Ok(contents) = std::fs::read_to_string(root.join(&entry).join("SKILL.md")) else {
                continue;
            };
            let Some(frontmatter) = claude_skill_frontmatter(&contents) else {
                continue;
            };
            let name = entry.trim().to_owned();
            if name.is_empty() || skills.contains_key(&name) {
                continue;
            }
            let enabled = overrides.get(&name).is_none_or(|(enabled, _)| *enabled);
            skills.insert(name, enabled && frontmatter.user_invocable);
        }
    }
    skills
        .into_iter()
        .filter_map(|(name, invocable)| invocable.then_some(name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(root: &Path, name: &str, lines: &[&str]) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), lines.join("\n")).unwrap();
    }

    // Discovery, precedence and override cases for the names a prompt may
    // invoke.
    #[test]
    fn discovers_user_and_project_skills_a_prompt_may_invoke() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("claude-home");
        let workspace = temp.path().join("workspace");
        let user = config.join("skills");
        let project = workspace.join(".claude").join("skills");
        skill(
            &user,
            "codex-review",
            &["---", "description: Ask Codex for a review.", "---"],
        );
        skill(
            &project,
            "deploy",
            &["---", "description: Deploy the app.", "---"],
        );
        skill(
            &project,
            "codex-review",
            &["---", "user-invocable: no", "---"],
        );
        skill(&user, "no-frontmatter", &["# Just a heading"]);
        skill(&user, "broken-yaml", &["---", "name: [unclosed", "---"]);
        skill(&user, "agent-only", &["---", "user-invocable: no", "---"]);
        skill(&user, "off-by-user", &["---", "name: off-by-user", "---"]);
        skill(
            &project,
            "off-by-project",
            &["---", "name: off-by-project", "---"],
        );
        skill(
            &user,
            "user-only",
            &["---", "disable-model-invocation: true", "---"],
        );
        std::fs::create_dir_all(workspace.join(".agents").join("skills").join("codex")).unwrap();
        std::fs::write(
            workspace
                .join(".agents")
                .join("skills")
                .join("codex")
                .join("SKILL.md"),
            "---\n---\n",
        )
        .unwrap();
        std::fs::write(user.join("README.md"), "not a skill").unwrap();
        std::fs::write(
            config.join("settings.json"),
            r#"{ "skillOverrides": { "off-by-user": "off", "user-only": "user-invocable-only" } }"#,
        )
        .unwrap();
        std::fs::write(
            workspace.join(".claude").join("settings.json"),
            r#"{ "skillOverrides": { "off-by-project": "off" } }"#,
        )
        .unwrap();
        assert_eq!(
            user_invocable_skills(&config, Some(&workspace)),
            ["codex-review", "deploy", "no-frontmatter", "user-only"]
        );
        assert!(user_invocable_skills(&temp.path().join("missing"), None).is_empty());
    }

    // "reads repository root settings from a nested workspace".
    #[test]
    fn a_nested_workspace_reads_the_repository_root_local_settings() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("claude-home");
        let root = temp.path().join("repo");
        let nested = root.join("packages").join("app");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        skill(&config.join("skills"), "root-off", &["---", "---"]);
        std::fs::create_dir_all(nested.join(".claude")).unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(
            nested.join(".claude").join("settings.local.json"),
            r#"{ "skillOverrides": { "root-off": "on" } }"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".claude").join("settings.local.json"),
            r#"{ "skillOverrides": { "root-off": "off" } }"#,
        )
        .unwrap();
        assert!(user_invocable_skills(&config, Some(&nested)).is_empty());
    }
}

//! Project actions: the scripts a project runs from the header, a shortcut or
//! worktree creation, and the add/edit form that changes them.
use crate::models::{ProjectScript, ProjectScriptIcon};
use crate::state::Snapshot;
use std::collections::{BTreeMap, BTreeSet};

/// The longest script id a run shortcut can name.
pub const MAX_SCRIPT_ID_LENGTH: usize = 24;
const RUN_COMMAND_PREFIX: &str = "script.";
const RUN_COMMAND_SUFFIX: &str = ".run";

/// What the add/edit form submits.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectScriptInput {
    pub name: String,
    pub command: String,
    pub icon: ProjectScriptIcon,
    pub run_on_worktree_create: bool,
    /// Setup scripts only: hold the agent until the script exits.
    pub wait_for_setup: bool,
    pub preview_url: Option<String>,
    pub auto_open_preview: bool,
}

impl Default for ProjectScriptInput {
    fn default() -> Self {
        Self {
            name: String::new(),
            command: String::new(),
            icon: ProjectScriptIcon::Play,
            run_on_worktree_create: false,
            wait_for_setup: false,
            preview_url: None,
            auto_open_preview: false,
        }
    }
}

/// `async: false` is recorded only for a setup script that holds the agent.
pub fn build_project_script(id: &str, input: &ProjectScriptInput) -> ProjectScript {
    ProjectScript {
        id: id.into(),
        name: input.name.clone(),
        command: input.command.clone(),
        icon: input.icon,
        run_on_worktree_create: input.run_on_worktree_create,
        run_async: (input.run_on_worktree_create && input.wait_for_setup).then_some(false),
        preview_url: input.preview_url.clone(),
        auto_open_preview: input.preview_url.as_ref().map(|_| input.auto_open_preview),
    }
}

/// The form's starting values for editing a script.
pub fn project_script_input(script: &ProjectScript) -> ProjectScriptInput {
    ProjectScriptInput {
        name: script.name.clone(),
        command: script.command.clone(),
        icon: script.icon,
        run_on_worktree_create: script.run_on_worktree_create,
        wait_for_setup: script.run_on_worktree_create && script.run_async == Some(false),
        preview_url: script.preview_url.clone(),
        auto_open_preview: script.auto_open_preview.unwrap_or(false),
    }
}

/// Trims the form and drops options its other fields disable.
pub fn validate_project_script_input(
    input: &ProjectScriptInput,
) -> Result<ProjectScriptInput, String> {
    let name = input.name.trim();
    let command = input.command.trim();
    if name.is_empty() {
        return Err("Name is required.".into());
    }
    if command.is_empty() {
        return Err("Command is required.".into());
    }
    let preview_url = input
        .preview_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    Ok(ProjectScriptInput {
        name: name.into(),
        command: command.into(),
        icon: input.icon,
        run_on_worktree_create: input.run_on_worktree_create,
        wait_for_setup: input.run_on_worktree_create && input.wait_for_setup,
        preview_url: preview_url.map(str::to_owned),
        auto_open_preview: preview_url.is_some() && input.auto_open_preview,
    })
}

fn is_script_run_command(command: &str) -> bool {
    command
        .strip_prefix(RUN_COMMAND_PREFIX)
        .and_then(|rest| rest.strip_suffix(RUN_COMMAND_SUFFIX))
        .is_some_and(|id| {
            let bytes = id.as_bytes();
            !bytes.is_empty()
                && id.chars().count() <= MAX_SCRIPT_ID_LENGTH
                && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
                && bytes
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        })
}

/// The shortcut command that runs a script. Older ids that a command cannot
/// name stay runnable without a shortcut.
pub fn command_for_project_script(script_id: &str) -> Option<String> {
    let command = format!("{RUN_COMMAND_PREFIX}{script_id}{RUN_COMMAND_SUFFIX}");
    is_script_run_command(&command).then_some(command)
}

pub fn project_script_id_from_command(command: &str) -> Option<String> {
    let trimmed = command.trim();
    if !is_script_run_command(trimmed) {
        return None;
    }
    Some(trimmed[RUN_COMMAND_PREFIX.len()..trimmed.len() - RUN_COMMAND_SUFFIX.len()].into())
}

fn normalize_script_id(value: &str) -> String {
    let lower = value.trim().to_lowercase();
    let mut cleaned = String::new();
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            cleaned.push(c);
        } else if !cleaned.ends_with('-') {
            cleaned.push('-');
        }
    }
    let cleaned = cleaned.trim_matches('-');
    if cleaned.is_empty() {
        return "script".into();
    }
    if cleaned.len() <= MAX_SCRIPT_ID_LENGTH {
        return cleaned.into();
    }
    match cleaned[..MAX_SCRIPT_ID_LENGTH].trim_end_matches('-') {
        "" => "script".into(),
        id => id.into(),
    }
}

/// A slug of `name` that no existing script uses, short enough for a shortcut.
pub fn next_project_script_id<'a>(
    name: &str,
    existing_ids: impl IntoIterator<Item = &'a str>,
) -> String {
    let taken: BTreeSet<&str> = existing_ids.into_iter().collect();
    let base = normalize_script_id(name);
    if !taken.contains(base.as_str()) {
        return base;
    }
    (2u64..)
        .map(|suffix| {
            let candidate = format!("{base}-{suffix}");
            if candidate.len() <= MAX_SCRIPT_ID_LENGTH {
                return candidate;
            }
            let keep = MAX_SCRIPT_ID_LENGTH
                .saturating_sub(suffix.to_string().len() + 1)
                .max(1);
            format!("{}-{suffix}", &base[..keep.min(base.len())])
        })
        .find(|candidate| !taken.contains(candidate.as_str()))
        .expect("an unused suffix exists")
}

/// The script the header runs: the first that is not the setup, else the setup.
pub fn primary_project_script(scripts: &[ProjectScript]) -> Option<&ProjectScript> {
    scripts
        .iter()
        .find(|script| !script.run_on_worktree_create)
        .or(scripts.first())
}

pub fn setup_project_script(scripts: &[ProjectScript]) -> Option<&ProjectScript> {
    scripts.iter().find(|script| script.run_on_worktree_create)
}

/// Scripts run in the thread's worktree when it has one.
pub fn project_script_cwd(project_root: &str, worktree_path: Option<&str>) -> String {
    worktree_path.unwrap_or(project_root).into()
}

/// The environment a script runs with; `extra` wins.
pub fn project_script_runtime_env(
    project_root: &str,
    worktree_path: Option<&str>,
    extra: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([("PROJECT_ROOT".to_owned(), project_root.to_owned())]);
    if let Some(path) = worktree_path.filter(|path| !path.is_empty()) {
        env.insert("WORKTREE_PATH".into(), path.into());
    }
    env.extend(
        extra
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    env
}

/// The list after adding a script; a new setup script replaces the old one.
pub fn add_project_script(
    scripts: &[ProjectScript],
    input: &ProjectScriptInput,
) -> Vec<ProjectScript> {
    let id = next_project_script_id(&input.name, scripts.iter().map(|s| s.id.as_str()));
    let mut next: Vec<_> = scripts
        .iter()
        .map(|script| ProjectScript {
            run_on_worktree_create: script.run_on_worktree_create && !input.run_on_worktree_create,
            ..script.clone()
        })
        .collect();
    next.push(build_project_script(&id, input));
    next
}

pub fn update_project_script(
    scripts: &[ProjectScript],
    script_id: &str,
    input: &ProjectScriptInput,
) -> Result<Vec<ProjectScript>, String> {
    if !scripts.iter().any(|script| script.id == script_id) {
        return Err("Script not found.".into());
    }
    Ok(scripts
        .iter()
        .map(|script| {
            if script.id == script_id {
                build_project_script(script_id, input)
            } else {
                ProjectScript {
                    run_on_worktree_create: script.run_on_worktree_create
                        && !input.run_on_worktree_create,
                    ..script.clone()
                }
            }
        })
        .collect())
}

pub fn delete_project_script(scripts: &[ProjectScript], script_id: &str) -> Vec<ProjectScript> {
    scripts
        .iter()
        .filter(|script| script.id != script_id)
        .cloned()
        .collect()
}

pub fn project_script_menu_label(script: &ProjectScript) -> String {
    if script.run_on_worktree_create {
        format!("{} (setup)", script.name)
    } else {
        script.name.clone()
    }
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectScriptRow {
    pub script: ProjectScript,
    /// "Test", or "Setup (setup)" for the worktree setup script.
    pub label: String,
    /// The shortcut command, when the id can have one.
    pub shortcut_command: Option<String>,
    /// "Run Test".
    pub run_label: String,
    /// "Edit Test".
    pub edit_label: String,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectScriptsView {
    pub project_id: String,
    /// The root scripts run in when the thread has no worktree.
    pub project_root: String,
    pub rows: Vec<ProjectScriptRow>,
    /// The header's run button; the last script run here wins when it still exists.
    pub primary_script_id: Option<String>,
    pub setup_script_id: Option<String>,
}

/// The project's actions, or `None` when the project is unknown.
pub fn project_scripts(
    snapshot: &Snapshot,
    project_id: &str,
    last_run_script_id: Option<&str>,
) -> Option<ProjectScriptsView> {
    let shell = snapshot.shell_view()?;
    let project = shell.projects.iter().find(|p| p.id == project_id)?;
    let scripts = &project.scripts;
    let primary = last_run_script_id
        .and_then(|id| scripts.iter().find(|script| script.id == id))
        .or_else(|| primary_project_script(scripts));
    Some(ProjectScriptsView {
        project_id: project.id.clone(),
        project_root: project
            .roots
            .first()
            .map(|root| root.path.clone())
            .unwrap_or_default(),
        rows: scripts
            .iter()
            .map(|script| ProjectScriptRow {
                label: project_script_menu_label(script),
                shortcut_command: command_for_project_script(&script.id),
                run_label: format!("Run {}", script.name),
                edit_label: format!("Edit {}", script.name),
                script: script.clone(),
            })
            .collect(),
        primary_script_id: primary.map(|script| script.id.clone()),
        setup_script_id: setup_project_script(scripts).map(|script| script.id.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(id: &str, name: &str, command: &str, setup: bool) -> ProjectScript {
        ProjectScript {
            id: id.into(),
            name: name.into(),
            command: command.into(),
            icon: ProjectScriptIcon::Play,
            run_on_worktree_create: setup,
            run_async: None,
            preview_url: None,
            auto_open_preview: None,
        }
    }

    #[test]
    fn builds_scripts_with_preview_settings() {
        assert_eq!(
            build_project_script(
                "dev",
                &ProjectScriptInput {
                    name: "Dev server".into(),
                    command: "pnpm dev".into(),
                    icon: ProjectScriptIcon::Debug,
                    run_on_worktree_create: false,
                    wait_for_setup: false,
                    preview_url: Some("http://localhost:5733".into()),
                    auto_open_preview: true,
                }
            ),
            ProjectScript {
                id: "dev".into(),
                name: "Dev server".into(),
                command: "pnpm dev".into(),
                icon: ProjectScriptIcon::Debug,
                run_on_worktree_create: false,
                run_async: None,
                preview_url: Some("http://localhost:5733".into()),
                auto_open_preview: Some(true),
            }
        );
    }

    #[test]
    fn omits_preview_settings_when_no_preview_url_is_configured() {
        assert_eq!(
            build_project_script(
                "test",
                &ProjectScriptInput {
                    name: "Test".into(),
                    command: "pnpm test".into(),
                    icon: ProjectScriptIcon::Test,
                    ..ProjectScriptInput::default()
                }
            ),
            ProjectScript {
                icon: ProjectScriptIcon::Test,
                ..script("test", "Test", "pnpm test", false)
            }
        );
    }

    #[test]
    fn only_records_async_false_for_setup_scripts_that_should_block_the_agent() {
        let input = ProjectScriptInput {
            name: "Setup".into(),
            command: "pnpm i".into(),
            icon: ProjectScriptIcon::Configure,
            ..ProjectScriptInput::default()
        };
        let build = |run_on_worktree_create, wait_for_setup| {
            build_project_script(
                "setup",
                &ProjectScriptInput {
                    run_on_worktree_create,
                    wait_for_setup,
                    ..input.clone()
                },
            )
        };
        let blocking = build(true, true);
        assert!(blocking.run_on_worktree_create);
        assert_eq!(blocking.run_async, Some(false));
        assert_eq!(build(true, false).run_async, None);
        assert_eq!(build(false, true).run_async, None);
    }

    #[test]
    fn builds_and_parses_script_run_commands() {
        let command = command_for_project_script("lint");
        assert_eq!(command.as_deref(), Some("script.lint.run"));
        assert_eq!(
            project_script_id_from_command(command.as_deref().unwrap_or("")).as_deref(),
            Some("lint")
        );
        assert_eq!(project_script_id_from_command("terminal.toggle"), None);
    }

    #[rstest::rstest]
    #[case("install-javascript-dependencies")]
    #[case("A")]
    #[case("a.b")]
    #[case("a b")]
    #[case("-a")]
    #[case("")]
    #[case(&"a".repeat(25))]
    fn omits_the_shortcut_for_legacy_script_ids_without_crashing_script_menus(#[case] id: &str) {
        let commands: Vec<_> = ["lint", id, "test"]
            .into_iter()
            .map(command_for_project_script)
            .collect();
        assert_eq!(
            commands,
            [
                Some("script.lint.run".into()),
                None,
                Some("script.test.run".into())
            ]
        );
    }

    #[test]
    fn preserves_the_exact_id_at_the_shortcut_length_limit() {
        let id = "a".repeat(MAX_SCRIPT_ID_LENGTH);
        let command = command_for_project_script(&id).unwrap_or_default();
        assert_eq!(project_script_id_from_command(&command), Some(id));
    }

    #[test]
    fn slugifies_and_dedupes_project_script_ids() {
        assert_eq!(next_project_script_id("Run Tests", []), "run-tests");
        assert_eq!(
            next_project_script_id("Run Tests", ["run-tests"]),
            "run-tests-2"
        );
        assert_eq!(next_project_script_id("!!!", []), "script");
    }

    #[test]
    fn resolves_primary_and_setup_scripts() {
        let scripts = [
            ProjectScript {
                icon: ProjectScriptIcon::Configure,
                ..script("setup", "Setup", "bun install", true)
            },
            ProjectScript {
                icon: ProjectScriptIcon::Test,
                ..script("test", "Test", "bun test", false)
            },
        ];
        assert_eq!(
            primary_project_script(&scripts).map(|s| s.id.as_str()),
            Some("test")
        );
        assert_eq!(
            setup_project_script(&scripts).map(|s| s.id.as_str()),
            Some("setup")
        );
    }

    #[test]
    fn builds_default_runtime_env_for_scripts() {
        let env = project_script_runtime_env("/repo", Some("/repo/worktree-a"), &BTreeMap::new());
        assert_eq!(env["PROJECT_ROOT"], "/repo");
        assert_eq!(env["WORKTREE_PATH"], "/repo/worktree-a");
    }

    #[test]
    fn allows_overriding_runtime_env_values() {
        let env = project_script_runtime_env(
            "/repo",
            None,
            &BTreeMap::from([
                ("PROJECT_ROOT".into(), "/custom-root".into()),
                ("CUSTOM_FLAG".into(), "1".into()),
            ]),
        );
        assert_eq!(env["PROJECT_ROOT"], "/custom-root");
        assert_eq!(env["CUSTOM_FLAG"], "1");
        assert_eq!(env.get("WORKTREE_PATH"), None);
    }

    #[test]
    fn prefers_the_worktree_path_for_script_cwd_resolution() {
        assert_eq!(
            project_script_cwd("/repo", Some("/repo/worktree-a")),
            "/repo/worktree-a"
        );
        assert_eq!(project_script_cwd("/repo", None), "/repo");
    }

    #[test]
    fn a_new_id_shortens_its_base_to_fit_the_suffix() {
        let base = "a".repeat(MAX_SCRIPT_ID_LENGTH);
        let next = next_project_script_id(&base, [base.as_str()]);
        assert_eq!(next, format!("{}-2", "a".repeat(MAX_SCRIPT_ID_LENGTH - 2)));
        assert!(command_for_project_script(&next).is_some());
    }

    #[test]
    fn the_form_requires_a_name_and_a_command_and_trims_its_fields() {
        let input = ProjectScriptInput {
            name: "  Test ".into(),
            command: " bun test ".into(),
            wait_for_setup: true,
            preview_url: Some("   ".into()),
            auto_open_preview: true,
            ..ProjectScriptInput::default()
        };
        assert_eq!(
            validate_project_script_input(&input),
            Ok(ProjectScriptInput {
                name: "Test".into(),
                command: "bun test".into(),
                ..ProjectScriptInput::default()
            })
        );
        assert_eq!(
            validate_project_script_input(&ProjectScriptInput {
                name: " ".into(),
                ..input.clone()
            }),
            Err("Name is required.".into())
        );
        assert_eq!(
            validate_project_script_input(&ProjectScriptInput {
                command: "".into(),
                ..input
            }),
            Err("Command is required.".into())
        );
    }

    #[test]
    fn editing_starts_from_the_saved_script() {
        let saved = ProjectScript {
            run_async: Some(false),
            preview_url: Some("http://localhost:3000".into()),
            auto_open_preview: Some(true),
            ..script("setup", "Setup", "pnpm i", true)
        };
        let input = project_script_input(&saved);
        assert!(input.wait_for_setup);
        assert!(input.auto_open_preview);
        assert_eq!(build_project_script("setup", &input), saved);
    }

    #[test]
    fn a_new_setup_script_replaces_the_previous_setup() {
        let scripts = [
            script("setup", "Setup", "pnpm i", true),
            script("test", "Test", "pnpm test", false),
        ];
        let added = add_project_script(
            &scripts,
            &ProjectScriptInput {
                name: "Test".into(),
                command: "bun i".into(),
                run_on_worktree_create: true,
                ..ProjectScriptInput::default()
            },
        );
        assert_eq!(
            added
                .iter()
                .map(|s| (s.id.as_str(), s.run_on_worktree_create))
                .collect::<Vec<_>>(),
            [("setup", false), ("test", false), ("test-2", true)]
        );
        let kept = add_project_script(
            &scripts,
            &ProjectScriptInput {
                name: "Lint".into(),
                command: "pnpm lint".into(),
                ..ProjectScriptInput::default()
            },
        );
        assert!(kept[0].run_on_worktree_create);
    }

    #[test]
    fn updates_and_deletes_scripts_by_id() {
        let scripts = [
            script("setup", "Setup", "pnpm i", true),
            script("test", "Test", "pnpm test", false),
        ];
        let input = ProjectScriptInput {
            name: "Unit".into(),
            command: "pnpm unit".into(),
            run_on_worktree_create: true,
            ..ProjectScriptInput::default()
        };
        let updated = update_project_script(&scripts, "test", &input).unwrap();
        assert_eq!(updated[1].id, "test");
        assert_eq!(updated[1].name, "Unit");
        assert!(!updated[0].run_on_worktree_create);
        assert_eq!(
            update_project_script(&scripts, "missing", &input),
            Err("Script not found.".into())
        );
        assert_eq!(
            delete_project_script(&scripts, "setup")
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["test"]
        );
    }

    #[test]
    fn the_view_lists_the_projects_scripts_and_prefers_the_last_run() {
        use crate::models::{Project, ProjectRoot};
        use agent_protocol::conversation::ShellSnapshot;
        let shell = ShellSnapshot {
            projects: vec![Project {
                id: "p".into(),
                name: "repo".into(),
                roots: vec![ProjectRoot {
                    path: "/repo".into(),
                }],
                scripts: vec![
                    script("setup", "Setup", "pnpm i", true),
                    script("test", "Test", "pnpm test", false),
                    script("lint", "Lint", "pnpm lint", false),
                ],
                ..Project::default()
            }],
            snapshot_sequence: 0,
            threads: vec![],
        };
        let mut snapshot = Snapshot::default();
        std::sync::Arc::make_mut(&mut snapshot.shell).snapshot = Some(shell);
        let view = project_scripts(&snapshot, "p", None).unwrap();
        assert_eq!(view.project_root, "/repo");
        assert_eq!(view.primary_script_id.as_deref(), Some("test"));
        assert_eq!(view.setup_script_id.as_deref(), Some("setup"));
        assert_eq!(view.rows[0].label, "Setup (setup)");
        assert_eq!(view.rows[1].run_label, "Run Test");
        assert_eq!(
            view.rows[2].shortcut_command.as_deref(),
            Some("script.lint.run")
        );
        let preferred = project_scripts(&snapshot, "p", Some("lint")).unwrap();
        assert_eq!(preferred.primary_script_id.as_deref(), Some("lint"));
        let removed = project_scripts(&snapshot, "p", Some("gone")).unwrap();
        assert_eq!(removed.primary_script_id.as_deref(), Some("test"));
        assert!(project_scripts(&snapshot, "other", None).is_none());
    }
}

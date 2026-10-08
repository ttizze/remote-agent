//! The composer's `/`, `$` and `@` menu for the current draft.
use super::commands::{
    ComposerCommandItem, ComposerCommandMenuInput, ComposerPathEntry, ComposerSkill,
    ComposerTrigger, ComposerTriggerKind, ProviderSlashCommand, composer_command_items,
    detect_composer_trigger, has_compactable_conversation, pull_request_items,
};
use crate::state::Snapshot;
use crate::view::timeline::rows::TimelineLayout;
use agent_domain::ThreadShell;
use agent_protocol::workspace as w;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerMenuView {
    pub trigger: Option<ComposerTrigger>,
    pub items: Vec<ComposerCommandItem>,
    /// What the open menu says without items: its loading text, or that
    /// nothing matched.
    pub empty_label: Option<String>,
    /// The search behind the trigger has not answered yet.
    pub loading: bool,
}

/// The empty menu's text for a trigger in each layout's wording.
pub fn composer_menu_empty_label(
    kind: ComposerTriggerKind,
    layout: TimelineLayout,
) -> &'static str {
    match (layout, kind) {
        (_, ComposerTriggerKind::Path) => "No matching files or folders.",
        (TimelineLayout::Desktop, ComposerTriggerKind::Skill) => {
            "No skills found. Try / to browse provider commands."
        }
        (TimelineLayout::Desktop, ComposerTriggerKind::PullRequest) => {
            "Pull requests are not available for this project."
        }
        (TimelineLayout::Desktop, _) => "No matching command.",
        (TimelineLayout::Mobile, ComposerTriggerKind::Skill) => "No skills found.",
        (TimelineLayout::Mobile, ComposerTriggerKind::PullRequest) => {
            "Pull requests are unavailable for this project."
        }
        (TimelineLayout::Mobile, _) => "No matching commands.",
    }
}

/// The menu's text while its search runs.
pub fn composer_menu_loading_label(
    kind: ComposerTriggerKind,
    layout: TimelineLayout,
) -> &'static str {
    match (layout, kind) {
        (TimelineLayout::Mobile, ComposerTriggerKind::Path) => "Searching files…",
        (TimelineLayout::Mobile, _) => "Loading…",
        (TimelineLayout::Desktop, ComposerTriggerKind::Skill) => "Searching workspace skills...",
        (TimelineLayout::Desktop, ComposerTriggerKind::PullRequest) => "Finding pull request...",
        (TimelineLayout::Desktop, _) => "Searching workspace files...",
    }
}

/// The menu for `text` with the cursor at `cursor` (UTF-16), from the
/// provider's skills and commands and the `@` path search that
/// `Intent::UpdateComposerMenu` loads, worded for the layout it named.
pub fn composer_menu(snapshot: &Snapshot, text: &str, cursor: u32) -> ComposerMenuView {
    let Some(trigger) = detect_composer_trigger(text, cursor) else {
        return ComposerMenuView::default();
    };
    let layout = snapshot.sources.composer_layout;
    let loading = if trigger.kind == ComposerTriggerKind::PullRequest {
        snapshot
            .selected_project
            .as_deref()
            .or(snapshot.pull_requests.selected_project.as_deref())
            .is_some_and(|project| !snapshot.pull_requests.by_project.contains_key(project))
    } else {
        trigger.kind == ComposerTriggerKind::Path
            && !trigger.query.trim().is_empty()
            && !snapshot.composer_cwd().is_empty()
            && path_entries(snapshot, &trigger).is_none()
    };
    let items = composer_menu_items(snapshot, &trigger);
    ComposerMenuView {
        empty_label: items.is_empty().then(|| {
            if loading {
                composer_menu_loading_label(trigger.kind, layout)
            } else {
                composer_menu_empty_label(trigger.kind, layout)
            }
            .into()
        }),
        trigger: Some(trigger),
        items,
        loading,
    }
}

/// The path search's answer for the trigger's query, once it has one.
fn path_entries<'a>(
    snapshot: &'a Snapshot,
    trigger: &ComposerTrigger,
) -> Option<&'a [w::WorkspaceEntry]> {
    let cwd = snapshot.composer_cwd();
    let query = trigger.query.trim();
    snapshot
        .sources
        .entries
        .result
        .as_ref()
        .filter(|(asked, _)| asked.cwd == cwd && asked.query == query)
        .map(|(_, entries)| entries.as_slice())
}

fn composer_skill(skill: &w::ProviderSkill) -> ComposerSkill {
    ComposerSkill {
        source: super::commands::skill_source_kind(&skill.path, skill.scope.as_deref()),
        name: skill.name.clone(),
        display_name: skill.display_name.clone(),
        short_description: skill.short_description.clone(),
        description: skill.description.clone(),
        enabled: skill.enabled,
        user_invocable: Some(skill.user_invocable),
    }
}

pub(crate) fn composer_menu_items(
    snapshot: &Snapshot,
    trigger: &ComposerTrigger,
) -> Vec<ComposerCommandItem> {
    let draft = snapshot.current_draft();
    let thread = snapshot.selected_thread.as_ref();
    let compactable = thread.is_some_and(|id| {
        let sync = snapshot.thread(id);
        let latest_user_message_at = snapshot
            .thread_row(id)
            .and_then(|row| row.latest_user_message_at.as_ref());
        sync.and_then(|sync| sync.state.as_deref())
            .is_some_and(|state| {
                has_compactable_conversation(
                    state,
                    sync.is_some_and(|sync| sync.history.has_more),
                    latest_user_message_at,
                )
            })
    });
    let threads: Vec<ThreadShell> = snapshot
        .shell_view()
        .map(|shell| shell.threads.clone())
        .unwrap_or_default();
    let cwd = snapshot.composer_cwd();
    let commands = snapshot.sources.provider_commands(&draft.instance_id, &cwd);
    let skills: Vec<ComposerSkill> = commands
        .map(|commands| commands.skills.iter().map(composer_skill).collect())
        .unwrap_or_default();
    let slash_commands: Vec<ProviderSlashCommand> = commands
        .map(|commands| {
            commands
                .slash_commands
                .iter()
                .map(|command| ProviderSlashCommand {
                    name: command.name.clone(),
                    description: command.description.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let path_entries: Vec<ComposerPathEntry> = path_entries(snapshot, trigger)
        .unwrap_or_default()
        .iter()
        .map(|entry| ComposerPathEntry {
            path: entry.path.clone(),
            directory: entry.kind == w::EntryKind::Directory,
        })
        .collect();
    let allow_interaction_mode = crate::view::models::catalog(snapshot)
        .instance(&draft.instance_id)
        .is_none_or(|instance| instance.show_interaction_mode_toggle);
    let mut items = composer_command_items(&ComposerCommandMenuInput {
        trigger,
        driver: (!draft.instance_id.is_empty()).then_some(draft.driver),
        has_thread: thread.is_some(),
        has_compactable_conversation: compactable,
        allow_interaction_mode,
        skills: &skills,
        slash_commands: &slash_commands,
        path_entries: &path_entries,
        threads: &threads,
        current_thread: thread.map(|id| id.as_str()),
    });
    if trigger.kind == ComposerTriggerKind::PullRequest {
        let project_id = snapshot
            .selected_project
            .as_deref()
            .or(snapshot.pull_requests.selected_project.as_deref());
        items = project_id
            .map(|project_id| {
                pull_request_items(&crate::view::pull_requests::composer_pull_request_matches(
                    snapshot,
                    project_id,
                    &trigger.query,
                ))
            })
            .unwrap_or_default();
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_menu_names_what_its_trigger_searched() {
        let snapshot = Snapshot::default();
        let path = composer_menu(&snapshot, "see @nothing-here", 17);
        assert_eq!(
            path.trigger.map(|trigger| trigger.kind),
            Some(ComposerTriggerKind::Path)
        );
        assert!(path.items.is_empty());
        assert_eq!(
            path.empty_label.as_deref(),
            Some("No matching files or folders.")
        );
        assert_eq!(
            composer_menu_empty_label(ComposerTriggerKind::Skill, TimelineLayout::Desktop),
            "No skills found. Try / to browse provider commands."
        );
        assert_eq!(
            composer_menu_empty_label(ComposerTriggerKind::SlashCommand, TimelineLayout::Desktop),
            "No matching command."
        );
        assert_eq!(
            composer_menu_empty_label(ComposerTriggerKind::SlashCommand, TimelineLayout::Mobile),
            "No matching commands."
        );
        assert_eq!(
            composer_menu(&snapshot, "plain text", 10),
            ComposerMenuView::default()
        );
    }

    #[test]
    fn the_menu_lists_the_providers_skills_commands_and_found_paths() {
        use crate::state::{EntryQuery, ProviderCommandsEntry};
        let mut snapshot = Snapshot::default();
        snapshot.default_draft.instance_id = "claude".into();
        snapshot.default_draft.driver = agent_domain::Driver::Claude;
        snapshot.selected_project = Some("app".into());
        snapshot.shell = std::sync::Arc::new(crate::sync::ShellCache::from_cache(
            agent_protocol::conversation::ShellSnapshot {
                snapshot_sequence: 1,
                projects: vec![crate::models::Project {
                    id: "app".into(),
                    name: "App".into(),
                    roots: vec![crate::models::ProjectRoot {
                        path: "/repo".into(),
                    }],
                    ..Default::default()
                }],
                threads: vec![],
            },
        ));
        snapshot.sources.provider_commands.insert(
            ("claude".into(), "/repo".into()),
            ProviderCommandsEntry {
                commands: Some(w::ProviderCommands {
                    instance: "claude".into(),
                    cwd: "/repo".into(),
                    slash_commands: vec![w::SlashCommand {
                        name: "review".into(),
                        description: Some("Review the diff".into()),
                        input_hint: None,
                    }],
                    slash_commands_pending: false,
                    skills: vec![w::ProviderSkill {
                        name: "deploy".into(),
                        path: "/repo/.claude/skills/deploy/SKILL.md".into(),
                        enabled: true,
                        description: Some("Ship it".into()),
                        scope: Some("project".into()),
                        display_name: None,
                        short_description: None,
                        user_invocation_only: false,
                        user_invocable: true,
                    }],
                }),
                in_flight: false,
                retry_at_ms: None,
            },
        );
        let slash = composer_menu(&snapshot, "/", 1);
        let labels: Vec<&str> = slash.items.iter().map(|item| item.label.as_str()).collect();
        assert!(labels.contains(&"/review"), "{labels:?}");
        assert!(labels.contains(&"skill:deploy"), "{labels:?}");
        let skill = composer_menu(&snapshot, "$dep", 4);
        assert_eq!(skill.items[0].label, "deploy");

        snapshot.sources.composer_layout = TimelineLayout::Mobile;
        let pending = composer_menu(&snapshot, "@src", 4);
        assert!(pending.loading);
        assert_eq!(pending.empty_label.as_deref(), Some("Searching files…"));
        snapshot.sources.entries.result = Some((
            EntryQuery {
                cwd: "/repo".into(),
                query: "src".into(),
                limit: 20,
            },
            vec![w::WorkspaceEntry {
                path: "src/main.rs".into(),
                kind: w::EntryKind::File,
            }],
        ));
        let found = composer_menu(&snapshot, "@src", 4);
        assert!(!found.loading);
        assert_eq!(found.items[0].label, "main.rs");
        assert_eq!(found.items[0].description, "src");
    }

    #[test]
    fn a_menu_with_items_has_no_empty_label() {
        let menu = composer_menu(&Snapshot::default(), "/", 1);
        assert!(!menu.items.is_empty());
        assert_eq!(menu.empty_label, None);
    }
}

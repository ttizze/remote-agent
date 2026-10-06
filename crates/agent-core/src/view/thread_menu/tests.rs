use super::*;
use crate::view::search::fixtures::{project, row, snapshot};
use crate::view::thread_summary::fixtures::ms;
use chrono::Utc;

fn base_state() -> ThreadMenuState {
    ThreadMenuState {
        thread_id: "thread-1".into(),
        title: "Release prep".into(),
        project_id: "project-1".into(),
        branch: None,
        worktree_path: None,
        workspace_path: Some("/repo".into()),
        project_filter: None,
        is_pinned: false,
        is_settled: false,
        auto_settle_enabled: true,
        is_snoozed: false,
        can_snooze_now: true,
        is_regenerating_title: false,
        is_running: false,
        snooze_presets: vec![SnoozePreset {
            id: SnoozePresetId::Hour,
            label: "In 1 hour".into(),
            when_label: "3:00 PM".into(),
            snoozed_until: "2026-08-07T15:00:00Z".into(),
        }],
        confirm_unpin: false,
        confirm_archive: false,
        confirm_delete: true,
    }
}

fn ids(state: &ThreadMenuState) -> Vec<String> {
    build_thread_menu(state)
        .iter()
        .map(|item| item.id.key())
        .collect()
}

fn all_ids(state: &ThreadMenuState) -> Vec<String> {
    build_thread_menu(state)
        .iter()
        .flat_map(|item| {
            std::iter::once(item.id.key()).chain(item.children.iter().map(|child| child.id.key()))
        })
        .collect()
}

fn find(items: &[ThreadMenuItem], id: ThreadMenuItemId) -> (usize, &ThreadMenuItem) {
    items
        .iter()
        .enumerate()
        .find(|(_, item)| item.id == id)
        .unwrap()
}

#[test]
fn groups_project_settings_with_utility_actions_before_archive() {
    let items = build_thread_menu(&base_state());
    let (copy, _) = find(&items, ThreadMenuItemId::Copy);
    let settings = &items[copy + 1];
    assert_eq!(settings.id, ThreadMenuItemId::ProjectSettings);
    assert_eq!(settings.label, "Project settings");
    assert_eq!(settings.icon.as_deref(), Some("settings"));
    assert_eq!(items[copy + 2].id, ThreadMenuItemId::Archive);
}

#[test]
fn offers_project_filtering_only_for_surfaces_with_a_scoped_thread_list() {
    assert!(!ids(&base_state()).contains(&"filter-by-project".into()));
    let items = build_thread_menu(&ThreadMenuState {
        project_filter: Some(ProjectFilter {
            label: "Beta Project".into(),
            is_active: false,
        }),
        ..base_state()
    });
    let (_, filter) = find(&items, ThreadMenuItemId::FilterByProject);
    assert_eq!(filter.label, "Filter by Beta Project");
    assert_eq!(filter.icon.as_deref(), Some("folder-tree"));
    assert_eq!(
        filter.action,
        Some(ThreadMenuAction::FilterProject {
            project_id: Some("project-1".into())
        })
    );
}

#[test]
fn offers_the_way_back_to_all_projects_once_the_list_is_scoped() {
    let items = build_thread_menu(&ThreadMenuState {
        project_filter: Some(ProjectFilter {
            label: "Beta Project".into(),
            is_active: true,
        }),
        ..base_state()
    });
    let (index, filter) = find(&items, ThreadMenuItemId::FilterByProject);
    assert_eq!(filter.label, "Show all projects");
    assert_eq!(filter.icon.as_deref(), Some("folder-tree"));
    assert_eq!(
        filter.action,
        Some(ThreadMenuAction::FilterProject { project_id: None })
    );
    assert_eq!(items[index - 1].id, ThreadMenuItemId::MarkUnread);
    assert_eq!(items[index + 1].id, ThreadMenuItemId::AutoSettle);
}

#[test]
fn includes_branch_items_only_for_threads_with_a_branch() {
    let with_branch = all_ids(&ThreadMenuState {
        branch: Some("feat/menu".into()),
        ..base_state()
    });
    assert!(with_branch.contains(&"new-thread-on-branch".into()));
    assert!(with_branch.contains(&"copy-branch".into()));
    assert!(!all_ids(&base_state()).contains(&"new-thread-on-branch".into()));
    assert!(!all_ids(&base_state()).contains(&"copy-branch".into()));
}

#[test]
fn flips_lifecycle_labels_with_thread_state() {
    let flipped = ids(&ThreadMenuState {
        is_pinned: true,
        is_settled: true,
        is_snoozed: true,
        ..base_state()
    });
    for id in ["unpin", "unsettle", "unsnooze"] {
        assert!(flipped.contains(&id.into()), "{id}");
    }
    let base = ids(&base_state());
    for id in ["pin", "settle", "snooze"] {
        assert!(base.contains(&id.into()), "{id}");
    }
}

#[test]
fn offers_auto_settle_as_a_submenu_with_the_current_option_checked() {
    let checked = |state: &ThreadMenuState| {
        let items = build_thread_menu(state);
        let (_, auto) = find(&items, ThreadMenuItemId::AutoSettle);
        assert_eq!(auto.label, "Auto-settle behavior");
        auto.children
            .iter()
            .map(|child| (child.id.key(), child.checked))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        checked(&base_state()),
        [
            ("auto-settle:enabled".to_string(), Some(true)),
            ("auto-settle:disabled".to_string(), Some(false)),
        ]
    );
    let off = checked(&ThreadMenuState {
        auto_settle_enabled: false,
        ..base_state()
    });
    assert_eq!(
        off.iter().map(|(_, checked)| *checked).collect::<Vec<_>>(),
        [Some(false), Some(true)]
    );
    let items = build_thread_menu(&base_state());
    let (unread, _) = find(&items, ThreadMenuItemId::MarkUnread);
    assert_eq!(items[unread + 1].id, ThreadMenuItemId::AutoSettle);
}

#[test]
fn disables_snooze_when_the_thread_cannot_snooze_keeping_presets_visible() {
    let items = build_thread_menu(&ThreadMenuState {
        can_snooze_now: false,
        ..base_state()
    });
    let (_, snooze) = find(&items, ThreadMenuItemId::Snooze);
    assert!(!snooze.enabled);
    assert_eq!(
        snooze
            .children
            .iter()
            .map(|child| child.id.key())
            .collect::<Vec<_>>(),
        ["snooze:hour", "snooze:custom"]
    );
}

#[test]
fn disables_title_regeneration_while_one_is_in_flight() {
    let items = build_thread_menu(&ThreadMenuState {
        is_regenerating_title: true,
        ..base_state()
    });
    let (_, item) = find(&items, ThreadMenuItemId::RegenerateTitle);
    assert_eq!(item.label, "Regenerating…");
    assert!(!item.enabled);
}

#[test]
fn marks_delete_as_destructive_and_keeps_it_last() {
    let items = build_thread_menu(&ThreadMenuState {
        branch: Some("main".into()),
        ..base_state()
    });
    let last = items.last().unwrap();
    assert_eq!(last.id, ThreadMenuItemId::Delete);
    assert!(last.destructive);
}

#[test]
fn offers_archive_as_a_non_destructive_action_right_before_delete() {
    let items = build_thread_menu(&base_state());
    let archive = &items[items.len() - 2];
    assert_eq!(archive.id, ThreadMenuItemId::Archive);
    assert_eq!(archive.icon.as_deref(), Some("archive"));
    assert!(archive.separator_before);
    assert!(!archive.destructive);
    assert_eq!(items.last().unwrap().id, ThreadMenuItemId::Delete);
}

#[test]
fn disables_archive_while_the_thread_is_running() {
    let items = build_thread_menu(&ThreadMenuState {
        is_running: true,
        ..base_state()
    });
    assert!(!find(&items, ThreadMenuItemId::Archive).1.enabled);
}

#[test]
fn draft_menu_offers_only_the_copy_values_the_draft_has() {
    let items = build_draft_menu(false, true, true);
    assert_eq!(items[0].id, DraftMenuItemId::Copy);
    assert!(items[0].enabled);
    assert_eq!(
        items[0]
            .children
            .iter()
            .map(|child| child.id.key())
            .collect::<Vec<_>>(),
        ["copy-branch"]
    );
    let no_copy = build_draft_menu(false, false, true);
    assert_eq!(no_copy[0].id, DraftMenuItemId::Copy);
    assert!(!no_copy[0].enabled);
    assert!(no_copy[0].children.is_empty());
}

#[test]
fn draft_menu_drops_project_settings_without_a_project_and_keeps_discard_last() {
    let items = build_draft_menu(true, false, false);
    assert_eq!(
        items.iter().map(|item| item.id.key()).collect::<Vec<_>>(),
        ["copy", "discard"]
    );
    let last = items.last().unwrap();
    assert_eq!(last.label, "Discard draft");
    assert!(last.destructive);
}

#[test]
fn unpin_asks_only_when_enabled_and_names_the_thread() {
    let unpin = |confirm_unpin| {
        let items = build_thread_menu(&ThreadMenuState {
            is_pinned: true,
            confirm_unpin,
            ..base_state()
        });
        find(&items, ThreadMenuItemId::Unpin).1.confirmation.clone()
    };
    assert_eq!(unpin(false), None);
    assert_eq!(
        unpin(true).unwrap().message,
        "Unpin thread \"Release prep\"?\nThis will move the thread out of your pinned section."
    );
}

#[test]
fn delete_asks_by_default_and_archive_only_when_enabled() {
    let items = build_thread_menu(&base_state());
    assert_eq!(
        find(&items, ThreadMenuItemId::Delete).1.confirmation,
        Some(ThreadMenuConfirmation {
            title: None,
            message: "Delete thread \"Release prep\"?\nThis permanently clears conversation history for this thread.".into(),
            destructive: true,
        })
    );
    assert_eq!(find(&items, ThreadMenuItemId::Archive).1.confirmation, None);
    let items = build_thread_menu(&ThreadMenuState {
        confirm_archive: true,
        ..base_state()
    });
    assert_eq!(
        find(&items, ThreadMenuItemId::Archive)
            .1
            .confirmation
            .as_ref()
            .map(|c| c.message.as_str()),
        Some("Archive thread \"Release prep\"?")
    );
}

#[test]
fn items_name_the_thread_actions_they_send() {
    let items = build_thread_menu(&ThreadMenuState {
        branch: Some("feat/menu".into()),
        worktree_path: Some("/wt".into()),
        ..base_state()
    });
    assert_eq!(
        find(&items, ThreadMenuItemId::NewThreadOnBranch).1.action,
        Some(ThreadMenuAction::NewThreadOnBranch {
            project_id: "project-1".into(),
            branch: "feat/menu".into(),
            worktree_path: Some("/wt".into()),
        })
    );
    let (_, snooze) = find(&items, ThreadMenuItemId::Snooze);
    assert_eq!(snooze.action, None);
    assert_eq!(
        snooze.children[0].action,
        ThreadMenuAction::Thread {
            action: ThreadAction::Snooze {
                until: "2026-08-07T15:00:00Z".into()
            }
        }
    );
    assert_eq!(snooze.children[0].label, "In 1 hour (3:00 PM)");
    assert!(snooze.children[1].separator_before);
    assert_eq!(snooze.children[1].action, ThreadMenuAction::CustomSnooze);
    let (_, copy) = find(&items, ThreadMenuItemId::Copy);
    assert_eq!(
        copy.children
            .iter()
            .map(|child| child.action.clone())
            .collect::<Vec<_>>(),
        [
            ThreadMenuAction::CopyPath {
                path: Some("/repo".into())
            },
            ThreadMenuAction::CopyBranch {
                branch: "feat/menu".into()
            },
            ThreadMenuAction::CopyThreadId {
                thread_id: "thread-1".into()
            },
        ]
    );
}

const NOW: &str = "2026-04-10T12:00:00.000Z";

fn at_now() -> chrono::DateTime<Utc> {
    Utc.timestamp_millis_opt(ms(NOW)).unwrap()
}

#[test]
fn reads_the_thread_from_the_active_shell() {
    let mut pinned = row("thread-1", "project-1", "Pinned");
    pinned.pinned_at = Some(agent_domain::Timestamp::parse(NOW).unwrap());
    pinned.workspace = Some(agent_domain::Workspace {
        cwd: "/repo".into(),
        worktree_path: None,
        branch: Some("feat/x".into()),
    });
    let mut archived = row("thread-2", "project-1", "Archived");
    archived.archived_at = Some(agent_domain::Timestamp::parse(NOW).unwrap());
    let snapshot = snapshot(
        vec![project("project-1", "Alpha", "/alpha")],
        vec![pinned, archived],
    );
    let view = thread_menu_at(
        &snapshot,
        "thread-1",
        &at_now(),
        &ThreadMenuOptions::default(),
    )
    .unwrap();
    assert_eq!(view.title, "Pinned");
    let ids: Vec<_> = view.items.iter().map(|item| item.id.key()).collect();
    assert_eq!(
        ids,
        [
            "new-thread-on-branch",
            "unpin",
            "settle",
            "snooze",
            "rename",
            "regenerate-title",
            "mark-unread",
            "filter-by-project",
            "auto-settle",
            "copy",
            "project-settings",
            "archive",
            "delete",
        ]
    );
    let (_, filter) = find(&view.items, ThreadMenuItemId::FilterByProject);
    assert_eq!(filter.label, "Filter by Alpha");
    let (_, copy) = find(&view.items, ThreadMenuItemId::Copy);
    assert_eq!(
        copy.children[0].action,
        ThreadMenuAction::CopyPath {
            path: Some("/alpha".into())
        }
    );
    let (_, snooze) = find(&view.items, ThreadMenuItemId::Snooze);
    assert_eq!(
        snooze.children[0].action,
        ThreadMenuAction::Thread {
            action: ThreadAction::Snooze {
                until: "2026-04-10T13:00:00.000Z".into()
            }
        }
    );
    assert!(
        thread_menu_at(
            &snapshot,
            "thread-2",
            &at_now(),
            &ThreadMenuOptions::default()
        )
        .is_none()
    );
    assert!(
        thread_menu_at(
            &snapshot,
            "missing",
            &at_now(),
            &ThreadMenuOptions::default()
        )
        .is_none()
    );
}

#[test]
fn the_list_reads_settlement_from_its_shelf_and_the_header_from_the_override() {
    let mut thread = row("thread-1", "project-1", "Thread");
    thread.settled = Some(true);
    thread.snoozed_until =
        Some(agent_domain::Timestamp::parse("2026-04-11T09:00:00.000Z").unwrap());
    thread.snoozed_at = Some(agent_domain::Timestamp::parse(NOW).unwrap());
    let mut snapshot = snapshot(vec![project("project-1", "Alpha", "/alpha")], vec![thread]);
    snapshot.selected_project = Some("project-1".into());
    let list = thread_menu_at(
        &snapshot,
        "thread-1",
        &at_now(),
        &ThreadMenuOptions::default(),
    )
    .unwrap();
    let list_ids: Vec<_> = list.items.iter().map(|item| item.id.key()).collect();
    assert!(list_ids.contains(&"settle".to_string()));
    assert!(list_ids.contains(&"unsnooze".to_string()));
    assert_eq!(
        find(&list.items, ThreadMenuItemId::FilterByProject).1.label,
        "Show all projects"
    );
    let header = thread_menu_at(
        &snapshot,
        "thread-1",
        &at_now(),
        &ThreadMenuOptions {
            surface: ThreadMenuSurface::Header,
            ..ThreadMenuOptions::default()
        },
    )
    .unwrap();
    let header_ids: Vec<_> = header.items.iter().map(|item| item.id.key()).collect();
    assert!(header_ids.contains(&"unsettle".to_string()));
    assert!(header_ids.contains(&"unsnooze".to_string()));
    assert!(!header_ids.contains(&"filter-by-project".to_string()));
}

#[test]
fn a_waiting_or_running_thread_cannot_snooze_or_archive() {
    let mut thread = row("thread-1", "project-1", "Thread");
    thread.pending_request = Some(agent_domain::PendingRequestSummary {
        id: agent_domain::RuntimeRequestId::new("request").unwrap(),
        kind: "user_input".into(),
        created_at: agent_domain::Timestamp::parse(NOW).unwrap(),
    });
    thread.latest_run = Some(agent_domain::RunId::new("run").unwrap());
    thread.status = Some(agent_domain::RunStatus::Running);
    let snapshot = snapshot(vec![], vec![thread]);
    let view = thread_menu_at(
        &snapshot,
        "thread-1",
        &at_now(),
        &ThreadMenuOptions::default(),
    )
    .unwrap();
    assert!(!find(&view.items, ThreadMenuItemId::Snooze).1.enabled);
    assert!(!find(&view.items, ThreadMenuItemId::Archive).1.enabled);
    assert!(
        view.items
            .iter()
            .all(|item| item.id != ThreadMenuItemId::FilterByProject)
    );
}

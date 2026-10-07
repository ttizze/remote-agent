use super::*;
use crate::state::{Draft, Shared};
use crate::sync::ShellCache;
use agent_domain::{RunId, RunStatus, ThreadId, ThreadShell, Timestamp};
use agent_protocol::conversation::{SearchMatch, ShellSnapshot};
use agent_protocol::models::Project;
use std::sync::Arc;

const HOUR: i64 = 3_600_000;

fn at(offset_ms: i64) -> Timestamp {
    Timestamp::from_millis(crate::sync::fixtures::at().millis() + offset_ms).unwrap()
}

fn now() -> i64 {
    at(HOUR).millis()
}

fn thread(id: &str, project: &str) -> ThreadShell {
    let mut shell = agent_domain::shell(&crate::sync::fixtures::thread_state("Thread")).unwrap();
    shell.id = ThreadId::new(id).unwrap();
    shell.project = project.into();
    shell.title = format!("Title {id}");
    shell
}

fn pinned(id: &str) -> ThreadShell {
    ThreadShell {
        pinned_at: Some(at(0)),
        ..thread(id, "alpha")
    }
}

fn snoozed(id: &str, wake_in: i64) -> ThreadShell {
    ThreadShell {
        snoozed_at: Some(at(0)),
        snoozed_until: Some(at(HOUR + wake_in)),
        ..thread(id, "alpha")
    }
}

fn settled(id: &str, settled_at: i64) -> ThreadShell {
    ThreadShell {
        settled: Some(true),
        settled_at: Some(at(settled_at)),
        ..thread(id, "alpha")
    }
}

fn running(id: &str) -> ThreadShell {
    let run = RunId::new(format!("run-{id}")).unwrap();
    ThreadShell {
        latest_run: Some(run.clone()),
        active_run: Some(run),
        status: Some(RunStatus::Running),
        activity_run_status: Some(RunStatus::Running),
        activity_run_started_at: Some(at(HOUR - 90_000)),
        latest_run_requested_at: Some(at(HOUR - 90_000)),
        latest_run_started_at: Some(at(HOUR - 90_000)),
        ..thread(id, "alpha")
    }
}

fn project(id: &str, name: &str) -> Project {
    Project {
        id: id.into(),
        name: name.into(),
        ..Project::default()
    }
}

fn snapshot(threads: Vec<ThreadShell>) -> Snapshot {
    with_projects(
        threads,
        vec![project("alpha", "Alpha"), project("beta", "Beta")],
    )
}

fn with_projects(threads: Vec<ThreadShell>, projects: Vec<Project>) -> Snapshot {
    Snapshot {
        shell: Arc::new(ShellCache::from_cache(ShellSnapshot {
            snapshot_sequence: 1,
            projects,
            threads,
        })),
        ..Snapshot::default()
    }
}

fn view(snapshot: &Snapshot, options: &SidebarOptions) -> SidebarView {
    sidebar(snapshot, now(), options, &InboxReturns::default())
}

fn expanded() -> SidebarOptions {
    SidebarOptions {
        working_expanded: true,
        snoozed_expanded: true,
        settled_expanded: true,
        ..SidebarOptions::default()
    }
}

/// The list as markers, shelf labels and `id:variant` rows.
fn outline(view: &SidebarView) -> Vec<String> {
    view.items
        .iter()
        .map(|item| match item {
            SidebarItem::Boundary { marker } => format!("{marker:?}"),
            SidebarItem::Shelf { header } => format!(
                "[{}{}]",
                header.label,
                if header.bottom_anchor { " ^" } else { "" }
            ),
            SidebarItem::Thread { row } => format!("{}:{:?}", row.id, row.variant),
            SidebarItem::ShowMore { count } => format!("+{count}"),
        })
        .collect()
}

#[test]
fn threads_split_into_pinned_active_snoozed_and_settled_shelves() {
    let snapshot = snapshot(vec![
        pinned("p"),
        thread("a", "alpha"),
        snoozed("z", HOUR),
        settled("s", 0),
    ]);
    assert_eq!(
        outline(&view(&snapshot, &expanded())),
        [
            "PinnedHeader",
            "p:Card",
            "PinnedDivider",
            "ActivePlaceholder",
            "a:Card",
            "[Snoozed ^]",
            "z:Slim",
            "[Settled]",
            "SettledPlaceholder",
            "s:Slim",
        ]
    );
    assert_eq!(
        outline(&view(&snapshot, &SidebarOptions::default())),
        [
            "PinnedHeader",
            "p:Card",
            "PinnedDivider",
            "ActivePlaceholder",
            "a:Card",
            "[Snoozed (1) ^]",
            "[Settled (1)]",
            "SettledPlaceholder",
        ]
    );
}

#[test]
fn the_settled_header_stays_while_other_shelves_come_and_go() {
    let only_active = snapshot(vec![thread("a", "alpha")]);
    assert_eq!(
        outline(&view(&only_active, &SidebarOptions::default())),
        [
            "PinnedHeader",
            "PinnedDivider",
            "ActivePlaceholder",
            "a:Card",
            "[Settled (0) ^]",
            "SettledPlaceholder",
        ]
    );
    assert!(
        view(&snapshot(vec![]), &SidebarOptions::default())
            .items
            .is_empty()
    );
}

#[test]
fn collapsed_shelves_keep_the_open_threads_row() {
    let mut snapshot = snapshot(vec![
        snoozed("z1", HOUR),
        snoozed("z2", 2 * HOUR),
        settled("s", 0),
    ]);
    snapshot.selected_thread = Some(ThreadId::new("z2").unwrap());
    assert_eq!(
        outline(&view(&snapshot, &SidebarOptions::default())),
        [
            "PinnedHeader",
            "PinnedDivider",
            "ActivePlaceholder",
            "[Snoozed (2) ^]",
            "z2:Slim",
            "[Settled (1)]",
            "SettledPlaceholder",
        ]
    );
    snapshot.selected_thread = Some(ThreadId::new("s").unwrap());
    let collapsed = view(&snapshot, &SidebarOptions::default());
    assert_eq!(collapsed.thread_ids(), ["s"]);
}

#[test]
fn snoozed_rows_sort_by_the_soonest_wake_and_show_when_they_return() {
    let view = view(
        &snapshot(vec![
            snoozed("later", 3 * HOUR),
            snoozed("soon", 30 * 60_000),
        ]),
        &expanded(),
    );
    assert_eq!(view.thread_ids(), ["soon", "later"]);
    assert_eq!(
        view.row("soon").unwrap().trailing,
        SidebarRowTrailing::WakesIn {
            label: "30m".into()
        }
    );
    assert_eq!(view.row("soon").unwrap().actions, [SidebarRowAction::Wake]);
}

#[test]
fn the_settled_tail_pages_ten_then_twenty_five_and_keeps_the_open_thread() {
    let threads: Vec<_> = (0..40)
        .map(|index| settled(&format!("s{index:02}"), -index * 60_000))
        .collect();
    let mut snapshot = snapshot(threads);
    let first = view(&snapshot, &expanded());
    assert_eq!(first.thread_ids().len(), 10);
    assert_eq!(first.thread_ids()[0], "s00");
    assert_eq!(outline(&first).last().unwrap(), "+25");
    snapshot.selected_thread = Some(ThreadId::new("s30").unwrap());
    let with_open = view(&snapshot, &expanded());
    assert_eq!(with_open.thread_ids().len(), 11);
    assert_eq!(with_open.thread_ids().last().unwrap(), "s30");
    assert_eq!(outline(&with_open).last().unwrap(), "+25");
    let second = view(
        &snapshot,
        &SidebarOptions {
            settled_pages: 1,
            ..expanded()
        },
    );
    assert_eq!(second.thread_ids().len(), 35);
    assert_eq!(outline(&second).last().unwrap(), "+5");
    let all = view(
        &snapshot,
        &SidebarOptions {
            settled_pages: 2,
            ..expanded()
        },
    );
    assert_eq!(all.thread_ids().len(), 40);
    assert!(!matches!(
        all.items.last(),
        Some(SidebarItem::ShowMore { .. })
    ));
    let collapsed = view(&snapshot, &SidebarOptions::default());
    assert_eq!(collapsed.thread_ids(), ["s30"]);
    assert!(!matches!(
        collapsed.items.last(),
        Some(SidebarItem::ShowMore { .. })
    ));
}

#[test]
fn the_working_section_folds_inbox_work_but_leaves_pins_in_place() {
    let snapshot = snapshot(vec![
        running("w"),
        ThreadShell {
            pinned_at: Some(at(0)),
            ..running("pw")
        },
        thread("a", "alpha"),
        snoozed("z", HOUR),
    ]);
    let options = SidebarOptions {
        working_section: true,
        ..SidebarOptions::default()
    };
    assert_eq!(
        outline(&view(&snapshot, &options)),
        [
            "PinnedHeader",
            "pw:Card",
            "PinnedDivider",
            "ActivePlaceholder",
            "a:Card",
            "[Working (1) ^]",
            "[Snoozed (1)]",
            "[Settled (0)]",
            "SettledPlaceholder",
        ]
    );
    let open = view(
        &snapshot,
        &SidebarOptions {
            working_expanded: true,
            ..options
        },
    );
    let row = open.row("w").unwrap();
    assert_eq!(
        (row.section, row.variant, row.draggable),
        (SidebarSection::Working, SidebarRowVariant::Card, false)
    );
    assert!(open.inbox_time_ordered);
    assert_eq!(
        outline(&view(&snapshot, &SidebarOptions::default()))[..6],
        [
            "PinnedHeader",
            "pw:Card",
            "PinnedDivider",
            "ActivePlaceholder",
            "a:Card",
            "w:Card"
        ]
    );
}

#[test]
fn a_working_row_shows_its_status_and_duration_and_recedes_unless_open() {
    let mut snapshot = snapshot(vec![running("w")]);
    let row = view(&snapshot, &SidebarOptions::default())
        .row("w")
        .unwrap()
        .clone();
    assert_eq!(
        row.trailing,
        SidebarRowTrailing::Status {
            status: SidebarTopStatus::Working,
            label: "Working".into(),
            duration: Some("1m".into()),
        }
    );
    assert_eq!(row.working_started_at, Some(now() - 90_000));
    assert!(row.recede && row.faded);
    assert_eq!(row.surface, SidebarRowSurface::Receded);
    assert_eq!(row.title_tone, SidebarTitleTone::Secondary);
    assert_eq!(row.accessibility_label, "Title w, Working, Alpha");
    snapshot.selected_thread = Some(ThreadId::new("w").unwrap());
    let open = view(&snapshot, &SidebarOptions::default())
        .row("w")
        .unwrap()
        .clone();
    assert!(!open.recede && open.active);
    assert_eq!(open.surface, SidebarRowSurface::Active);
}

#[test]
fn an_unread_completion_reads_done_and_a_quiet_row_shows_its_age() {
    let done = ThreadShell {
        latest_run: Some(RunId::new("run").unwrap()),
        status: Some(RunStatus::Completed),
        latest_run_completed_at: Some(at(HOUR - 60_000)),
        last_visited_at: Some(at(0)),
        ..thread("done", "alpha")
    };
    let quiet = ThreadShell {
        latest_user_message_at: Some(at(HOUR - 5 * 60_000)),
        ..thread("quiet", "alpha")
    };
    let view = view(&snapshot(vec![done, quiet]), &SidebarOptions::default());
    let done = view.row("done").unwrap();
    assert!(done.unread && !done.recede);
    assert_eq!(done.top_status, Some(SidebarTopStatus::Done));
    assert_eq!(done.title_tone, SidebarTitleTone::Prominent);
    let quiet = view.row("quiet").unwrap();
    assert_eq!(
        quiet.trailing,
        SidebarRowTrailing::Time { label: "5m".into() }
    );
    assert_eq!(quiet.surface, SidebarRowSurface::Receded);
    assert_eq!(
        quiet.actions,
        [SidebarRowAction::Snooze, SidebarRowAction::Settle]
    );
}

#[test]
fn a_woken_thread_carries_the_wake_until_visited() {
    let woke = ThreadShell {
        snoozed_at: Some(at(0)),
        snoozed_until: Some(at(HOUR - 60_000)),
        ..thread("woke", "alpha")
    };
    let view = view(&snapshot(vec![woke]), &SidebarOptions::default());
    let row = view.row("woke").unwrap();
    assert_eq!(row.section, SidebarSection::Active);
    assert_eq!(row.woke_at, Some(now() - 60_000));
    assert_eq!(row.top_status, Some(SidebarTopStatus::Woke));
    assert!(!row.recede);
}

#[test]
fn an_unsent_draft_marks_the_row_unless_it_is_open() {
    let mut snapshot = snapshot(vec![thread("a", "alpha")]);
    snapshot.drafts = Shared::from(BTreeMap::from([(
        "a".to_string(),
        Draft {
            text: "half a thought".into(),
            ..Draft::default()
        },
    )]));
    let row = view(&snapshot, &SidebarOptions::default())
        .row("a")
        .unwrap()
        .clone();
    assert!(row.has_unsent_draft);
    assert_eq!(row.surface, SidebarRowSurface::Draft);
    assert_eq!(row.actions[0], SidebarRowAction::DiscardDraft);
    snapshot.selected_thread = Some(ThreadId::new("a").unwrap());
    assert!(
        !view(&snapshot, &SidebarOptions::default())
            .row("a")
            .unwrap()
            .has_unsent_draft
    );
}

#[test]
fn new_thread_drafts_lead_the_sidebar_except_the_one_being_typed() {
    let mut snapshot = snapshot(vec![]);
    snapshot.drafts = Shared::from(BTreeMap::from([
        (
            "new:alpha".to_string(),
            Draft {
                text: "\n  Plan the release\nmore".into(),
                ..Draft::default()
            },
        ),
        ("new:beta".to_string(), Draft::default()),
        (
            "new:chats".to_string(),
            Draft {
                text: "typing".into(),
                ..Draft::default()
            },
        ),
    ]));
    let view = view(&snapshot, &SidebarOptions::default());
    assert_eq!(
        view.drafts,
        [SidebarDraftRow {
            draft_key: "new:alpha".into(),
            project_id: "alpha".into(),
            project_name: Some("Alpha".into()),
            preview: "Plan the release".into(),
            active: false,
            accessibility_label: "Plan the release, Unsent draft, Alpha".into(),
        }]
    );
    assert_eq!(view.empty_state, None);
}

// web Sidebar SidebarDraftBlock: drafts are newest first, and the open draft
// shows the row it had when it was opened, which never repaints while typing.
#[test]
fn drafts_are_newest_first_and_the_open_one_is_frozen_as_it_was_opened() {
    let draft = |text: &str, at: i64| Draft {
        text: text.into(),
        created_at_ms: Some(at),
        ..Draft::default()
    };
    let mut snapshot = snapshot(vec![thread("a", "alpha")]);
    snapshot.drafts = Shared::from(BTreeMap::from([
        ("new:chats".to_string(), draft("older", 1)),
        ("new:alpha".to_string(), draft("newer", 2)),
    ]));
    snapshot.freeze_open_draft();
    snapshot.drafts.get_mut("new:chats").unwrap().text = "older, edited".into();
    let previews = |snapshot: &Snapshot| {
        view(snapshot, &SidebarOptions::default())
            .drafts
            .into_iter()
            .map(|row| (row.preview, row.active))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        previews(&snapshot),
        [("newer".to_string(), false), ("older".to_string(), true)]
    );
    snapshot.selected_thread = Some(ThreadId::new("a").unwrap());
    snapshot.freeze_open_draft();
    assert_eq!(
        previews(&snapshot),
        [
            ("newer".to_string(), false),
            ("older, edited".to_string(), false)
        ]
    );
    snapshot.drafts.get_mut("new:chats").unwrap().text.clear();
    snapshot.selected_thread = None;
    snapshot.freeze_open_draft();
    snapshot.drafts.get_mut("new:chats").unwrap().text = "typed after opening".into();
    assert_eq!(
        previews(&snapshot),
        [("newer".to_string(), false)],
        "a draft opened empty has no row while it is open"
    );
}

#[test]
fn the_project_scope_filters_rows_and_falls_back_to_all_when_the_project_is_gone() {
    let mut snapshot = snapshot(vec![thread("a", "alpha"), thread("b", "beta")]);
    snapshot.selected_project = Some("beta".into());
    let scoped = view(&snapshot, &SidebarOptions::default());
    assert_eq!(scoped.thread_ids(), ["b"]);
    assert_eq!(
        scoped
            .project_scope
            .iter()
            .map(|item| (item.label.as_str(), item.selected))
            .collect::<Vec<_>>(),
        [("All projects", false), ("Alpha", false), ("Beta", true)]
    );
    snapshot.selected_project = Some("gone".into());
    assert_eq!(
        view(&snapshot, &SidebarOptions::default())
            .thread_ids()
            .len(),
        2
    );
}

#[test]
fn projects_follow_the_saved_order_in_manual_mode() {
    let snapshot = snapshot(vec![]);
    let labels = |options: &SidebarOptions| {
        view(&snapshot, options)
            .project_scope
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        labels(&SidebarOptions::default()),
        ["All projects", "Alpha", "Beta"]
    );
    assert_eq!(
        labels(&SidebarOptions {
            project_sort_order: SidebarProjectSortOrder::Manual,
            project_order: vec!["beta".into()],
            ..SidebarOptions::default()
        }),
        ["All projects", "Beta", "Alpha"]
    );
}

#[test]
fn an_empty_sidebar_explains_why() {
    assert_eq!(
        view(&with_projects(vec![], vec![]), &SidebarOptions::default()).empty_state,
        Some(SidebarEmptyState::NoProjects)
    );
    let mut empty = snapshot(vec![thread("b", "beta")]);
    assert_eq!(view(&empty, &SidebarOptions::default()).empty_state, None);
    empty.selected_project = Some("alpha".into());
    let state = view(&empty, &SidebarOptions::default())
        .empty_state
        .unwrap();
    assert_eq!(state.label(), "No threads in Alpha yet");
    assert_eq!(
        view(&snapshot(vec![]), &SidebarOptions::default()).empty_state,
        Some(SidebarEmptyState::NoThreads)
    );
}

#[test]
fn search_lists_title_matches_then_message_matches_across_every_shelf() {
    let mut snapshot = snapshot(vec![
        ThreadShell {
            title: "Fix the deploy".into(),
            ..settled("s", 0)
        },
        ThreadShell {
            title: "Unrelated".into(),
            ..thread("m", "alpha")
        },
        thread("x", "alpha"),
    ]);
    snapshot.search = " deploy ".into();
    snapshot.search_matches = vec![SearchMatch {
        thread_id: ThreadId::new("m").unwrap(),
        project_id: "alpha".into(),
        source: SearchSource::Assistant,
        snippet: "the deploy failed".into(),
        message_created_at: None,
    }];
    let search = view(&snapshot, &SidebarOptions::default()).search.unwrap();
    assert_eq!(
        search
            .results
            .iter()
            .map(|result| result.id.as_str())
            .collect::<Vec<_>>(),
        ["s", "m"]
    );
    assert_eq!(
        search.results[1].snippet.as_deref(),
        Some("the deploy failed")
    );
    assert_eq!(
        search.results[1].snippet_source,
        Some(SidebarMatchSource::Assistant)
    );
    snapshot.search = "nothing".into();
    snapshot.search_matches.clear();
    let none = view(&snapshot, &SidebarOptions::default());
    assert_eq!(
        none.search.unwrap().empty_label.as_deref(),
        Some("No threads found")
    );
    assert_eq!(none.empty_state, None);
}

#[test]
fn the_bulk_menu_counts_only_rendered_selected_rows() {
    let snapshot = snapshot(vec![pinned("p"), thread("a", "alpha"), settled("s", 0)]);
    let selection = ["p", "a", "s"].map(String::from).to_vec();
    let view = view(
        &snapshot,
        &SidebarOptions {
            selection: selection.clone(),
            ..SidebarOptions::default()
        },
    );
    assert!(view.row("a").unwrap().selected);
    assert_eq!(view.row("a").unwrap().surface, SidebarRowSurface::Selected);
    let labels: Vec<_> = view
        .multi_select_menu(&selection)
        .into_iter()
        .map(|item| item.label)
        .collect();
    assert_eq!(
        labels,
        [
            "Unpin (1)",
            "Settle (2)",
            "Snooze (2)",
            "Regenerate titles (2)",
            "Mark unread (2)",
            "Delete (2)",
        ]
    );
    assert!(view.multi_select_menu(&["s".into()]).is_empty());
}

#[test]
fn parking_the_open_thread_moves_to_the_next_card_or_a_new_thread() {
    let snapshot = snapshot(vec![pinned("p"), thread("a", "alpha"), snoozed("z", HOUR)]);
    let view = view(&snapshot, &expanded());
    assert_eq!(view.forward_target("a", Some("p"), &[]), None);
    assert_eq!(
        view.forward_target("a", Some("a"), &[]),
        Some(ForwardNavigation::Thread { id: "p".into() })
    );
    assert_eq!(
        view.forward_target("a", Some("a"), &["p".into()]),
        Some(ForwardNavigation::NewThread {
            project_id: "alpha".into()
        })
    );
}

#[test]
fn a_settle_sweep_stays_in_the_pressed_rows_section() {
    let snapshot = snapshot(vec![pinned("p"), thread("a", "alpha"), thread("b", "beta")]);
    let view = view(&snapshot, &SidebarOptions::default());
    let ids = view.thread_ids();
    assert_eq!(
        view.sweep_keys(&ids[2], &ids[0]),
        [ids[1].clone(), ids[2].clone()]
    );
}

#[test]
fn dropping_an_active_row_on_the_pins_plans_a_keyed_pin() {
    let snapshot = snapshot(vec![
        ThreadShell {
            pin_order: Some("m".into()),
            ..pinned("p")
        },
        thread("a", "alpha"),
    ]);
    let view = view(&snapshot, &SidebarOptions::default());
    assert_eq!(
        view.list_items()
            .iter()
            .map(SidebarListItem::id)
            .collect::<Vec<_>>(),
        [
            "sidebar-marker-pinned-header",
            "p",
            "sidebar-marker-pinned-divider",
            "sidebar-marker-active-placeholder",
            "a",
            "sidebar-marker-settled-header",
            "sidebar-marker-settled-placeholder",
        ]
    );
    let SidebarThreadDropPlan::Pin {
        order, order_key, ..
    } = plan_sidebar_drop(&snapshot, &view, "a", "p")
    else {
        panic!("expected a pin");
    };
    assert_eq!(order, ["a", "p"]);
    assert!(order_key.is_some_and(|key| key.as_str() < "m"));
    assert_eq!(
        plan_sidebar_drop(&snapshot, &view, "a", "sidebar-marker-settled-placeholder"),
        SidebarThreadDropPlan::Settle
    );
}

#[test]
fn inbox_returns_reorder_the_inbox_once_a_thread_stops_working() {
    let options = SidebarOptions {
        working_section: true,
        ..SidebarOptions::default()
    };
    let old = ThreadShell {
        created_at: at(-10 * HOUR),
        latest_run_requested_at: Some(at(-9 * HOUR)),
        latest_run_started_at: Some(at(-9 * HOUR)),
        ..running("old")
    };
    let fresh = ThreadShell {
        created_at: at(-HOUR),
        ..thread("fresh", "alpha")
    };
    let mut returns = InboxReturns::default();
    let before = snapshot(vec![old.clone(), fresh.clone()]);
    observe_inbox_returns(&before, now(), true, &mut returns);
    let stopped = ThreadShell {
        active_run: None,
        activity_run_status: None,
        activity_run_started_at: None,
        status: Some(RunStatus::Interrupted),
        ..old
    };
    let after = snapshot(vec![stopped, fresh]);
    observe_inbox_returns(&after, now(), true, &mut returns);
    assert_eq!(
        sidebar(&after, now(), &options, &returns).thread_ids(),
        ["old", "fresh"]
    );
    assert_eq!(
        sidebar(&after, now(), &options, &InboxReturns::default()).thread_ids(),
        ["fresh", "old"]
    );
}

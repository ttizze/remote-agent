use super::*;
use crate::view::inbox::{InboxReturns, is_thread_working, sort_inbox_threads_by_return};
use crate::view::thread_sort::{pin_order_key_between, sort_pinned_threads_by_order_key};
use crate::view::thread_summary::{
    RunSummary, RuntimeSummary,
    fixtures::{ms, run, runtime, summary},
};
use agent_domain::BackgroundKind;
use proptest::prelude::*;

fn ids<T: AsRef<ThreadSummary>>(threads: &[T]) -> Vec<&str> {
    threads
        .iter()
        .map(|thread| thread.as_ref().id.as_str())
        .collect()
}

#[test]
fn row_accessibility_leads_with_the_title_without_folding_row_actions_into_its_name() {
    assert_eq!(
        sidebar_row_accessibility("Can you audit the UI?", Some("Working"), Some("Code"), true),
        RowAccessibility {
            label: "Can you audit the UI?, Working, Code".into(),
            current_page: true,
        }
    );
    assert_eq!(
        sidebar_row_accessibility("The audit is done", None, Some("Code"), false),
        RowAccessibility {
            label: "The audit is done, Code".into(),
            current_page: false,
        }
    );
    assert_eq!(
        sidebar_row_accessibility("Untitled task", None, None, false).label,
        "Untitled task"
    );
}

#[test]
fn a_pinned_thread_stays_in_the_pinned_shelf() {
    assert_eq!(
        resolve_sidebar_thread_section(false, false, true),
        SidebarSection::Pinned
    );
}

#[test]
fn lifecycle_shelves_are_authoritative_over_a_stale_pin() {
    assert_eq!(
        resolve_sidebar_thread_section(true, true, true),
        SidebarSection::Snoozed
    );
    assert_eq!(
        resolve_sidebar_thread_section(false, true, true),
        SidebarSection::Settled
    );
}

#[test]
fn bulk_unpin_counts_only_the_pinned_rows_of_a_mixed_selection() {
    assert_eq!(
        bulk_unpin_menu_item(2),
        Some(SidebarMenuItem::new(
            SidebarMenuAction::Unpin,
            "Unpin (2)".into()
        ))
    );
}

#[test]
fn bulk_unpin_is_omitted_when_nothing_selected_is_pinned() {
    assert_eq!(bulk_unpin_menu_item(0), None);
}

#[test]
fn bulk_title_regeneration_counts_only_threads_that_can_start_a_new_regeneration() {
    assert_eq!(
        bulk_title_regeneration_menu_item(4, 3),
        Some(SidebarMenuItem::new(
            SidebarMenuAction::RegenerateTitle,
            "Regenerate titles (3)".into()
        ))
    );
}

#[test]
fn bulk_title_regeneration_shows_a_disabled_progress_item_when_every_thread_is_pending() {
    assert_eq!(
        bulk_title_regeneration_menu_item(2, 0),
        Some(SidebarMenuItem {
            disabled: true,
            ..SidebarMenuItem::new(
                SidebarMenuAction::RegenerateTitle,
                "Regenerating… (2)".into()
            )
        })
    );
}

#[test]
fn bulk_title_regeneration_is_omitted_when_nothing_selected_supports_it() {
    assert_eq!(bulk_title_regeneration_menu_item(0, 0), None);
}

#[test]
fn multi_select_offers_bulk_archive_with_the_selected_count() {
    assert!(
        multi_select_thread_menu_items(3, false).contains(&SidebarMenuItem::new(
            SidebarMenuAction::Archive,
            "Archive (3)".into()
        ))
    );
}

#[test]
fn multi_select_disables_bulk_archive_when_a_selected_thread_is_running() {
    assert!(
        multi_select_thread_menu_items(2, true).contains(&SidebarMenuItem {
            disabled: true,
            ..SidebarMenuItem::new(SidebarMenuAction::Archive, "Archive (2)".into())
        })
    );
}

#[test]
fn the_project_scope_keeps_only_top_level_unarchived_threads() {
    let root = ThreadSummary {
        project: "project-visible".into(),
        ..summary("thread-parent")
    };
    let subagent = ThreadSummary {
        subagent: true,
        parent: Some("thread-parent".into()),
        ..root.clone()
    };
    let fork = ThreadSummary {
        id: "thread-fork".into(),
        parent: Some("thread-parent".into()),
        forked: true,
        ..root.clone()
    };
    let archived = ThreadSummary {
        id: "thread-archived".into(),
        archived_at: Some(ms("2026-01-02T00:00:00.000Z")),
        ..root.clone()
    };
    let other = ThreadSummary {
        id: "thread-other-project".into(),
        project: "project-other".into(),
        ..root.clone()
    };
    let visible = filter_sidebar_visible_threads(
        vec![root, subagent, fork, archived, other],
        Some("project-visible"),
    );
    assert_eq!(ids(&visible), ["thread-parent", "thread-fork"]);
}

#[test]
fn subagent_threads_are_hidden_from_the_sidebar() {
    let subagent = ThreadSummary {
        subagent: true,
        ..summary("thread-subagent")
    };
    assert!(filter_sidebar_visible_threads(vec![subagent], None).is_empty());
    assert_eq!(
        filter_sidebar_visible_threads(vec![summary("thread")], None).len(),
        1
    );
}

#[test]
fn the_fork_parent_comes_from_the_fork_lineage() {
    let fork = ThreadSummary {
        parent: Some("thread-fallback-parent".into()),
        forked: true,
        ..summary("thread")
    };
    assert_eq!(
        sidebar_fork_parent_thread_id(&fork),
        Some("thread-fallback-parent")
    );
    assert_eq!(sidebar_fork_parent_thread_id(&summary("thread")), None);
}

fn latest_run(completed_at: Option<&str>, started_at: Option<&str>) -> RunSummary {
    RunSummary {
        requested_at: Some(ms("2026-03-09T10:00:00.000Z")),
        started_at: started_at.map(ms),
        completed_at: completed_at.map(ms),
        ..run("turn-1", RuntimeStatus::Completed)
    }
}

fn completed_run() -> RunSummary {
    latest_run(
        Some("2026-03-09T10:05:00.000Z"),
        Some("2026-03-09T10:00:00.000Z"),
    )
}

#[test]
fn a_thread_completed_after_its_last_visit_is_unseen() {
    let thread = ThreadSummary {
        latest_run: Some(completed_run()),
        last_visited_at: Some(ms("2026-03-09T10:04:00.000Z")),
        ..summary("thread")
    };
    assert!(thread.has_unseen_completion());
}

#[test]
fn a_missing_visit_marker_reads_as_seen() {
    let thread = ThreadSummary {
        latest_run: Some(completed_run()),
        ..summary("thread")
    };
    assert!(!thread.has_unseen_completion());
}

#[test]
fn inactive_working_and_waiting_threads_recede_even_when_unread_and_woke() {
    for status in [SidebarThreadStatus::Working, SidebarThreadStatus::Waiting] {
        assert!(should_recede_sidebar_thread(
            status, true, true, false, false
        ));
    }
}

#[test]
fn unread_ready_approval_and_input_threads_stay_prominent() {
    for status in [
        SidebarThreadStatus::Ready,
        SidebarThreadStatus::Approval,
        SidebarThreadStatus::Input,
    ] {
        assert!(!should_recede_sidebar_thread(
            status, true, false, false, false
        ));
    }
}

#[test]
fn active_and_selected_working_threads_stay_prominent() {
    let working = SidebarThreadStatus::Working;
    assert!(!should_recede_sidebar_thread(
        working, true, true, true, false
    ));
    assert!(!should_recede_sidebar_thread(
        working, true, true, false, true
    ));
}

#[test]
fn input_required_threads_stay_prominent_read_or_unread() {
    for unread in [false, true] {
        assert!(!should_recede_sidebar_thread(
            SidebarThreadStatus::Input,
            unread,
            false,
            false,
            false
        ));
    }
}

#[test]
fn prewarm_takes_the_first_visible_rows_up_to_the_limit() {
    let visible = ["row-1", "row-2", "row-3"];
    assert_eq!(
        sidebar_thread_ids_to_prewarm(&visible, 2),
        ["row-1", "row-2"]
    );
    assert_eq!(
        sidebar_thread_ids_to_prewarm(&visible[..2], 10),
        ["row-1", "row-2"]
    );
    assert!(sidebar_thread_ids_to_prewarm(&visible[..2], 0).is_empty());
    assert_eq!(SIDEBAR_THREAD_PREWARM_LIMIT, 3);
}

#[derive(Debug, PartialEq)]
struct Named {
    id: &'static str,
    cwd: &'static str,
}

fn named(entries: &[(&'static str, &'static str)]) -> Vec<Named> {
    entries.iter().map(|(id, cwd)| Named { id, cwd }).collect()
}

fn order_ids(items: Vec<Named>) -> Vec<&'static str> {
    items.into_iter().map(|item| item.id).collect()
}

#[test]
fn preferred_ids_lead_stale_ids_are_skipped_and_the_rest_keep_their_order() {
    let ordered = order_items_by_preferred_ids(
        named(&[("project-1", ""), ("project-2", ""), ("project-3", "")]),
        &["project-3", "project-missing", "project-1"],
        |item| vec![item.id],
    );
    assert_eq!(order_ids(ordered), ["project-3", "project-1", "project-2"]);
}

#[test]
fn repeated_preferred_ids_do_not_duplicate_items() {
    let ordered = order_items_by_preferred_ids(
        named(&[("project-1", ""), ("project-2", "")]),
        &["project-2", "project-1", "project-2"],
        |item| vec![item.id],
    );
    assert_eq!(order_ids(ordered), ["project-2", "project-1"]);
}

#[test]
fn a_manual_project_order_keyed_by_location_is_honored() {
    let key = |item: &Named| format!("environment-local:{}", item.cwd);
    let ordered = order_items_by_preferred_ids(
        named(&[
            ("id-alpha", "/work/alpha"),
            ("id-beta", "/work/beta"),
            ("id-gamma", "/work/gamma"),
        ]),
        &[
            "environment-local:/work/gamma".to_string(),
            "environment-local:/work/alpha".to_string(),
        ],
        |item| vec![key(item)],
    );
    assert_eq!(order_ids(ordered), ["id-gamma", "id-alpha", "id-beta"]);
}

#[test]
fn preference_aliases_resolve_to_their_items() {
    let ordered = order_items_by_preferred_ids(
        named(&[
            ("physical-a", "/work/a"),
            ("physical-b", "/work/b"),
            ("physical-c", "/work/c"),
        ]),
        &["legacy:/work/c".to_string(), "legacy:/work/a".to_string()],
        |item| vec![item.id.to_string(), format!("legacy:{}", item.cwd)],
    );
    assert_eq!(
        order_ids(ordered),
        ["physical-c", "physical-a", "physical-b"]
    );
}

proptest! {
    #[test]
    fn preferred_ordering_is_a_permutation_that_leads_with_present_preferences(
        items in proptest::collection::vec(0u8..8, 0..12),
        preferred in proptest::collection::vec(0u8..10, 0..12),
    ) {
        let indexed: Vec<(usize, u8)> = items.iter().copied().enumerate().collect();
        let ordered = order_items_by_preferred_ids(indexed.clone(), &preferred, |item| vec![item.1]);
        let mut positions: Vec<usize> = ordered.iter().map(|item| item.0).collect();
        positions.sort_unstable();
        prop_assert_eq!(positions, (0..items.len()).collect::<Vec<_>>());
        let mut remaining = items.clone();
        let mut expected_head = vec![];
        for id in &preferred {
            if let Some(index) = remaining.iter().position(|item| item == id) {
                remaining.remove(index);
                expected_head.push(*id);
            }
        }
        let head: Vec<u8> = ordered.iter().take(expected_head.len()).map(|item| item.1).collect();
        prop_assert_eq!(head, expected_head);
    }
}

#[test]
fn adjacent_thread_ids_follow_the_ordered_sidebar() {
    let threads = ["thread-1", "thread-2", "thread-3"].map(String::from);
    let adjacent = |current, direction| resolve_adjacent_thread_id(&threads, current, direction);
    assert_eq!(
        adjacent(Some("thread-2"), TraversalDirection::Previous),
        Some("thread-1")
    );
    assert_eq!(
        adjacent(Some("thread-2"), TraversalDirection::Next),
        Some("thread-3")
    );
    assert_eq!(adjacent(None, TraversalDirection::Next), Some("thread-1"));
    assert_eq!(
        adjacent(None, TraversalDirection::Previous),
        Some("thread-3")
    );
    assert_eq!(
        adjacent(Some("thread-1"), TraversalDirection::Previous),
        None
    );
}

fn with_runtime(status: RuntimeStatus) -> ThreadSummary {
    ThreadSummary {
        runtime: Some(runtime(status)),
        ..summary("thread")
    }
}

#[test]
fn approval_outranks_a_running_runtime() {
    let thread = ThreadSummary {
        has_pending_approvals: true,
        ..with_runtime(RuntimeStatus::Running)
    };
    assert_eq!(
        resolve_sidebar_thread_status(&thread),
        SidebarThreadStatus::Approval
    );
}

#[test]
fn awaiting_input_outranks_a_running_runtime_below_approval() {
    let mut thread = ThreadSummary {
        has_pending_user_input: true,
        ..with_runtime(RuntimeStatus::Running)
    };
    assert_eq!(
        resolve_sidebar_thread_status(&thread),
        SidebarThreadStatus::Input
    );
    thread.has_pending_approvals = true;
    assert_eq!(
        resolve_sidebar_thread_status(&thread),
        SidebarThreadStatus::Approval
    );
}

#[test]
fn running_and_starting_runtimes_are_working() {
    for status in [RuntimeStatus::Running, RuntimeStatus::Starting] {
        assert_eq!(
            resolve_sidebar_thread_status(&with_runtime(status)),
            SidebarThreadStatus::Working
        );
    }
}

#[test]
fn usage_limit_stops_stay_limited_and_visible_until_the_thread_recovers() {
    let limited = |status| ThreadSummary {
        runtime: Some(RuntimeSummary {
            last_error: Some("Plan limit reached".into()),
            last_error_class: Some("usage_limit".into()),
            ..runtime(status)
        }),
        ..summary("thread")
    };
    assert_eq!(
        resolve_sidebar_thread_status(&limited(RuntimeStatus::Failed)),
        SidebarThreadStatus::Limited
    );
    assert_eq!(
        resolve_sidebar_thread_status(&limited(RuntimeStatus::Running)),
        SidebarThreadStatus::Working
    );
    assert_eq!(
        resolve_sidebar_thread_status(&limited(RuntimeStatus::Completed)),
        SidebarThreadStatus::Ready
    );
    assert_eq!(
        resolve_sidebar_top_status(SidebarThreadStatus::Limited, false, false),
        Some(SidebarTopStatus::Limited)
    );
    assert!(!should_recede_sidebar_thread(
        SidebarThreadStatus::Limited,
        false,
        false,
        false,
        false
    ));
}

#[test]
fn failed_only_while_the_latest_run_failed() {
    let errored = |status| ThreadSummary {
        runtime: Some(RuntimeSummary {
            last_error: Some("boom".into()),
            ..runtime(status)
        }),
        ..summary("thread")
    };
    assert_eq!(
        resolve_sidebar_thread_status(&errored(RuntimeStatus::Failed)),
        SidebarThreadStatus::Failed
    );
    assert_eq!(
        resolve_sidebar_thread_status(&errored(RuntimeStatus::Completed)),
        SidebarThreadStatus::Ready
    );
    assert_eq!(
        resolve_sidebar_thread_status(&errored(RuntimeStatus::Idle)),
        SidebarThreadStatus::Waiting
    );
}

#[test]
fn no_runtime_is_ready() {
    assert_eq!(
        resolve_sidebar_thread_status(&summary("thread")),
        SidebarThreadStatus::Ready
    );
}

#[test]
fn a_waiting_runtime_shows_ahead_of_unread_and_woke() {
    assert_eq!(
        resolve_sidebar_top_status(SidebarThreadStatus::Waiting, true, true),
        Some(SidebarTopStatus::Waiting)
    );
}

#[test]
fn waiting_stays_static_while_working_shows_its_duration() {
    assert!(!should_show_sidebar_duration(SidebarThreadStatus::Waiting));
    assert!(should_show_sidebar_duration(SidebarThreadStatus::Working));
}

fn content(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| id.to_string()).collect()
}

fn scope_items() -> Vec<SidebarProjectScopeItem> {
    [
        (None, "All projects"),
        (Some("alpha"), "Alpha workspace"),
        (Some("beta"), "Beta tools"),
    ]
    .map(|(id, label)| SidebarProjectScopeItem {
        project_id: id.map(String::from),
        label: label.into(),
        selected: false,
    })
    .to_vec()
}

fn filter_scope(query: &str) -> Vec<SidebarProjectScopeItem> {
    filter_sidebar_project_scope_items(&scope_items(), query, project_scope_label_matches)
}

#[test]
fn the_default_scope_row_leads_while_the_query_is_empty() {
    assert_eq!(filter_scope(""), scope_items());
    assert_eq!(filter_scope("   "), scope_items());
}

#[test]
fn the_default_scope_row_hides_while_filtering() {
    assert!(filter_scope("all").is_empty());
}

#[test]
fn matching_projects_keep_source_order_and_a_miss_is_empty() {
    assert_eq!(filter_scope("WORK"), vec![scope_items()[1].clone()]);
    assert!(filter_scope("missing").is_empty());
}

#[test]
fn closing_the_scope_menu_clears_the_query() {
    let queried = ProjectScopeMenuState {
        open: true,
        query: "alpha".into(),
    };
    assert_eq!(
        reduce_project_scope_menu_state(
            queried.clone(),
            ProjectScopeMenuAction::OpenChanged { open: false }
        ),
        ProjectScopeMenuState::default()
    );
    assert_eq!(
        reduce_project_scope_menu_state(queried, ProjectScopeMenuAction::ProjectSettingsOpened),
        ProjectScopeMenuState::default()
    );
}

#[test]
fn the_scope_menu_stays_open_while_the_query_changes() {
    assert_eq!(
        reduce_project_scope_menu_state(
            ProjectScopeMenuState {
                open: true,
                query: String::new(),
            },
            ProjectScopeMenuAction::QueryChanged {
                query: "beta".into()
            }
        ),
        ProjectScopeMenuState {
            open: true,
            query: "beta".into(),
        }
    );
}

fn running_turn() -> RuntimeSummary {
    RuntimeSummary {
        active_run: Some("turn-1".into()),
        updated_at: ms("2026-03-09T10:02:00.000Z"),
        ..runtime(RuntimeStatus::Running)
    }
}

#[test]
fn working_time_uses_the_running_runs_start() {
    let thread = ThreadSummary {
        latest_run: Some(latest_run(None, Some("2026-03-09T10:00:00.000Z"))),
        runtime: Some(running_turn()),
        ..summary("thread")
    };
    assert_eq!(
        thread.working_started_at(),
        Some(ms("2026-03-09T10:00:00.000Z"))
    );
}

#[test]
fn working_time_uses_the_request_while_a_run_awaits_adoption() {
    let thread = ThreadSummary {
        latest_run: Some(latest_run(None, None)),
        runtime: Some(running_turn()),
        ..summary("thread")
    };
    assert_eq!(
        thread.working_started_at(),
        Some(ms("2026-03-09T10:00:00.000Z"))
    );
}

#[test]
fn working_time_is_not_invented_when_the_newest_run_completed() {
    let thread = ThreadSummary {
        latest_run: Some(completed_run()),
        runtime: Some(running_turn()),
        ..summary("thread")
    };
    assert_eq!(thread.working_started_at(), None);
}

#[test]
fn working_time_shares_the_activity_start_when_a_newer_run_is_queued_or_cancelled() {
    let activity_started_at = ms("2026-03-09T10:00:00.000Z");
    for (status, completed_at) in [
        (RuntimeStatus::Queued, None),
        (
            RuntimeStatus::Cancelled,
            Some(ms("2026-03-09T10:05:00.000Z")),
        ),
    ] {
        for updated_at in ["2026-03-09T10:30:00.000Z", "2026-03-09T10:50:00.000Z"] {
            let thread = ThreadSummary {
                latest_run: Some(RunSummary {
                    started_at: None,
                    completed_at,
                    ..latest_run(None, None)
                })
                .map(|run| RunSummary {
                    id: "newer-run".into(),
                    status,
                    ..run
                }),
                runtime: Some(RuntimeSummary {
                    activity_started_at: Some(Some(activity_started_at)),
                    updated_at: ms(updated_at),
                    ..running_turn()
                }),
                ..summary("thread")
            };
            assert_eq!(thread.working_started_at(), Some(activity_started_at));
        }
    }
}

#[test]
fn working_time_is_none_without_a_run_or_runtime() {
    assert_eq!(summary("thread").working_started_at(), None);
}

#[test]
fn working_durations_read_in_seconds_minutes_and_hours() {
    assert_eq!(format_working_duration_label(0), "0s");
    assert_eq!(format_working_duration_label(42_000), "42s");
    assert_eq!(format_working_duration_label(5 * 60_000), "5m");
    assert_eq!(format_working_duration_label(90 * 60_000), "1h 30m");
}

#[test]
fn negative_working_durations_clamp_to_zero() {
    assert_eq!(format_working_duration_label(-5_000), "0s");
}

#[test]
fn row_ages_compact_the_relative_label() {
    let now = ms("2026-03-09T12:00:00.000Z");
    assert_eq!(
        crate::view::time::compact_relative_time_label(now + 5_000, now),
        "now"
    );
    assert_eq!(
        crate::view::time::compact_relative_time_label(now - 59_000, now),
        "now"
    );
    assert_eq!(
        crate::view::time::compact_relative_time_label(now - 5 * 60_000, now),
        "5m"
    );
    assert_eq!(
        crate::view::time::compact_relative_time_label(now - 3 * 3_600_000, now),
        "3h"
    );
    assert_eq!(
        crate::view::time::compact_relative_time_label(now - 2 * 86_400_000, now),
        "2d"
    );
}

fn plan_thread() -> ThreadSummary {
    ThreadSummary {
        interaction_mode: InteractionMode::Plan,
        runtime: Some(running_turn()),
        ..summary("thread")
    }
}

fn idle_runtime(status: RuntimeStatus) -> Option<RuntimeSummary> {
    Some(RuntimeSummary {
        active_run: None,
        ..runtime(status)
    })
}

#[test]
fn pending_approval_shows_before_every_other_pill() {
    let thread = ThreadSummary {
        has_pending_approvals: true,
        has_pending_user_input: true,
        ..plan_thread()
    };
    let pill = resolve_thread_status_pill(&thread).unwrap();
    assert_eq!((pill.label(), pill.pulse()), ("Pending Approval", false));
}

#[test]
fn awaiting_input_shows_when_plan_mode_is_blocked_on_answers() {
    let thread = ThreadSummary {
        has_pending_user_input: true,
        ..plan_thread()
    };
    let pill = resolve_thread_status_pill(&thread).unwrap();
    assert_eq!((pill.label(), pill.pulse()), ("Awaiting Input", false));
}

#[test]
fn a_running_thread_without_blockers_is_working() {
    let pill = resolve_thread_status_pill(&plan_thread()).unwrap();
    assert_eq!((pill.label(), pill.pulse()), ("Working", true));
}

#[test]
fn an_idle_thread_with_background_tasks_is_waiting() {
    let thread = ThreadSummary {
        pending_background: vec![BackgroundKind::Monitor],
        runtime: idle_runtime(RuntimeStatus::Idle),
        ..plan_thread()
    };
    let pill = resolve_thread_status_pill(&thread).unwrap();
    assert_eq!((pill.label(), pill.pulse()), ("Waiting", false));
}

#[test]
fn an_active_turn_stays_working_beside_background_tasks() {
    let thread = ThreadSummary {
        pending_background: vec![BackgroundKind::Command],
        ..plan_thread()
    };
    let pill = resolve_thread_status_pill(&thread).unwrap();
    assert_eq!((pill.label(), pill.pulse()), ("Working", true));
}

#[test]
fn waiting_ends_when_the_background_roster_clears() {
    let thread = ThreadSummary {
        runtime: idle_runtime(RuntimeStatus::Idle),
        ..plan_thread()
    };
    assert_eq!(resolve_thread_status_pill(&thread), None);
}

#[test]
fn a_settled_plan_turn_with_a_proposed_plan_is_plan_ready() {
    let thread = ThreadSummary {
        has_actionable_proposed_plan: true,
        latest_run: Some(completed_run()),
        runtime: idle_runtime(RuntimeStatus::Completed),
        ..plan_thread()
    };
    let pill = resolve_thread_status_pill(&thread).unwrap();
    assert_eq!((pill.label(), pill.pulse()), ("Plan Ready", false));
}

#[test]
fn completion_is_not_manufactured_without_a_visit_marker() {
    let thread = ThreadSummary {
        latest_run: Some(completed_run()),
        runtime: idle_runtime(RuntimeStatus::Completed),
        ..plan_thread()
    };
    assert_eq!(resolve_thread_status_pill(&thread), None);
}

#[test]
fn an_unseen_completion_without_blockers_is_completed() {
    let thread = ThreadSummary {
        interaction_mode: InteractionMode::Default,
        latest_run: Some(completed_run()),
        last_visited_at: Some(ms("2026-03-09T10:04:00.000Z")),
        runtime: idle_runtime(RuntimeStatus::Completed),
        ..plan_thread()
    };
    let pill = resolve_thread_status_pill(&thread).unwrap();
    assert_eq!((pill.label(), pill.pulse()), ("Completed", false));
}

#[test]
fn a_project_without_notable_threads_has_no_indicator() {
    assert_eq!(resolve_project_status_indicator(&[None, None]), None);
}

#[test]
fn a_project_surfaces_its_most_urgent_thread() {
    assert_eq!(
        resolve_project_status_indicator(&[
            Some(ThreadStatusPill::Completed),
            Some(ThreadStatusPill::PendingApproval),
            Some(ThreadStatusPill::Working),
        ]),
        Some(ThreadStatusPill::PendingApproval)
    );
}

#[test]
fn plan_ready_outranks_completed() {
    assert_eq!(
        resolve_project_status_indicator(&[
            Some(ThreadStatusPill::Completed),
            Some(ThreadStatusPill::PlanReady),
        ]),
        Some(ThreadStatusPill::PlanReady)
    );
}

#[test]
fn waiting_ranks_below_active_work_and_above_plan_ready() {
    assert_eq!(
        resolve_project_status_indicator(&[
            Some(ThreadStatusPill::Waiting),
            Some(ThreadStatusPill::Working),
        ]),
        Some(ThreadStatusPill::Working)
    );
    assert_eq!(
        resolve_project_status_indicator(&[
            Some(ThreadStatusPill::PlanReady),
            Some(ThreadStatusPill::Waiting),
        ]),
        Some(ThreadStatusPill::Waiting)
    );
}

fn created(id: &str, project: &str, created_at: &str) -> ThreadSummary {
    ThreadSummary {
        project: project.into(),
        created_at: ms(created_at),
        updated_at: ms("2026-03-09T10:00:00.000Z"),
        ..summary(id)
    }
}

#[test]
fn delete_falls_back_to_the_top_remaining_thread_of_the_project() {
    let threads = [
        created("thread-oldest", "project-1", "2026-03-09T10:00:00.000Z"),
        created("thread-active", "project-1", "2026-03-09T10:05:00.000Z"),
        created("thread-newest", "project-1", "2026-03-09T10:10:00.000Z"),
        created(
            "thread-other-project",
            "project-2",
            "2026-03-09T10:20:00.000Z",
        ),
    ];
    assert_eq!(
        fallback_thread_after_delete(
            &threads,
            "thread-active",
            ThreadSortOrder::CreatedAt,
            &BTreeSet::new()
        )
        .as_deref(),
        Some("thread-newest")
    );
}

#[test]
fn delete_fallback_skips_threads_deleted_in_the_same_action() {
    let threads = [
        created("thread-active", "project-1", "2026-03-09T10:05:00.000Z"),
        created("thread-newest", "project-1", "2026-03-09T10:10:00.000Z"),
        created("thread-next", "project-1", "2026-03-09T10:07:00.000Z"),
    ];
    assert_eq!(
        fallback_thread_after_delete(
            &threads,
            "thread-active",
            ThreadSortOrder::CreatedAt,
            &content(&["thread-active", "thread-newest"])
        )
        .as_deref(),
        Some("thread-next")
    );
}

fn project(id: &str, title: &str, updated_at: Option<&str>) -> SidebarProjectInput {
    SidebarProjectInput {
        id: id.into(),
        title: title.into(),
        created_at: Some(ms("2026-03-09T10:00:00.000Z")),
        updated_at: updated_at.map(ms),
    }
}

fn project_ids(projects: &[SidebarProjectInput]) -> Vec<&str> {
    projects.iter().map(|project| project.id.as_str()).collect()
}

fn active_thread(id: &str, project: &str, updated_at: &str) -> ThreadSummary {
    ThreadSummary {
        project: project.into(),
        updated_at: ms(updated_at),
        ..summary(id)
    }
}

#[test]
fn projects_sort_by_the_latest_user_message_across_their_threads() {
    let projects = vec![
        project(
            "project-1",
            "Older project",
            Some("2026-03-09T10:00:00.000Z"),
        ),
        project(
            "project-2",
            "Newer project",
            Some("2026-03-09T10:00:00.000Z"),
        ),
    ];
    let threads = [
        ThreadSummary {
            latest_user_message_at: Some(ms("2026-03-09T10:01:00.000Z")),
            ..active_thread("thread-1", "project-1", "2026-03-09T10:20:00.000Z")
        },
        ThreadSummary {
            latest_user_message_at: Some(ms("2026-03-09T10:05:00.000Z")),
            ..active_thread("thread-2", "project-2", "2026-03-09T10:05:00.000Z")
        },
    ];
    let sorted = sort_projects_for_sidebar(projects, &threads, SidebarProjectSortOrder::UpdatedAt);
    assert_eq!(project_ids(&sorted), ["project-2", "project-1"]);
}

#[test]
fn projects_without_threads_sort_by_their_own_stamps() {
    let sorted = sort_projects_for_sidebar(
        vec![
            project(
                "project-1",
                "Older project",
                Some("2026-03-09T10:01:00.000Z"),
            ),
            project(
                "project-2",
                "Newer project",
                Some("2026-03-09T10:05:00.000Z"),
            ),
        ],
        &[] as &[ThreadSummary],
        SidebarProjectSortOrder::UpdatedAt,
    );
    assert_eq!(project_ids(&sorted), ["project-2", "project-1"]);
}

#[test]
fn projects_without_stamps_sort_by_name_then_id() {
    let unstamped = |id: &str, title: &str| SidebarProjectInput {
        created_at: None,
        ..project(id, title, None)
    };
    let sorted = sort_projects_for_sidebar(
        vec![
            unstamped("project-2", "Beta"),
            unstamped("project-1", "Alpha"),
        ],
        &[] as &[ThreadSummary],
        SidebarProjectSortOrder::UpdatedAt,
    );
    assert_eq!(project_ids(&sorted), ["project-1", "project-2"]);
}

#[test]
fn the_manual_project_order_is_kept() {
    let sorted = sort_projects_for_sidebar(
        vec![
            project("project-2", "Second", None),
            project("project-1", "First", None),
        ],
        &[] as &[ThreadSummary],
        SidebarProjectSortOrder::Manual,
    );
    assert_eq!(project_ids(&sorted), ["project-2", "project-1"]);
}

#[test]
fn archived_threads_do_not_count_as_project_activity() {
    let projects = vec![
        project(
            "project-1",
            "Visible project",
            Some("2026-03-09T10:01:00.000Z"),
        ),
        project(
            "project-2",
            "Archived-only project",
            Some("2026-03-09T10:00:00.000Z"),
        ),
    ];
    let threads = [
        active_thread("thread-visible", "project-1", "2026-03-09T10:02:00.000Z"),
        ThreadSummary {
            archived_at: Some(ms("2026-03-09T10:11:00.000Z")),
            ..active_thread("thread-archived", "project-2", "2026-03-09T10:10:00.000Z")
        },
    ];
    let sorted =
        sort_sidebar_project_groups(projects, &threads, SidebarProjectSortOrder::UpdatedAt);
    assert_eq!(project_ids(&sorted), ["project-1", "project-2"]);
}

#[test]
fn project_sorting_matches_the_per_comparison_order_on_a_shuffled_list_with_ties() {
    let minute = |value: usize| ms(&format!("2026-03-09T10:0{value}:00.000Z"));
    for (order, thread_order) in [
        (
            SidebarProjectSortOrder::UpdatedAt,
            ThreadSortOrder::UpdatedAt,
        ),
        (
            SidebarProjectSortOrder::CreatedAt,
            ThreadSortOrder::CreatedAt,
        ),
    ] {
        let projects: Vec<_> = (0..24)
            .map(|index| {
                let n = (index * 7) % 24;
                SidebarProjectInput {
                    id: format!("project-{n}"),
                    title: if n % 2 == 0 { "Alpha" } else { "Beta" }.into(),
                    created_at: Some(minute(n % 3)),
                    updated_at: (n % 5 != 0).then(|| minute(n % 2)),
                }
            })
            .collect();
        let threads: Vec<_> = (0..48)
            .map(|n| ThreadSummary {
                project: format!("project-{}", n % 16),
                created_at: minute(n % 6),
                updated_at: minute(n % 3),
                latest_user_message_at: (n % 4 != 0).then(|| minute(n % 5)),
                ..summary(&format!("thread-{n}"))
            })
            .collect();
        let timestamp = |project: &SidebarProjectInput| {
            let own: Vec<_> = threads
                .iter()
                .filter(|thread| thread.project == project.id)
                .collect();
            project_sort_timestamp(project, &own, thread_order)
        };
        let mut expected = projects.clone();
        expected.sort_by(|left, right| {
            timestamp(right)
                .cmp(&timestamp(left))
                .then_with(|| locale_compare(&left.title, &right.title))
                .then_with(|| locale_compare(&left.id, &right.id))
        });
        assert_eq!(
            sort_projects_for_sidebar(projects, &threads, order),
            expected
        );
    }
}

#[test]
fn a_project_without_threads_reports_its_own_stamp() {
    assert_eq!(
        project_sort_timestamp(
            &project("project-1", "Project", Some("2026-03-09T10:10:00.000Z")),
            &[],
            ThreadSortOrder::UpdatedAt
        ),
        ms("2026-03-09T10:10:00.000Z")
    );
}

#[test]
fn the_saved_project_order_applies_only_in_manual_mode() {
    let projects = vec![
        project("project-older", "Older project", None),
        project("project-newer", "Newer project", None),
    ];
    let threads = [
        active_thread("thread-1", "project-older", "2026-03-09T10:01:00.000Z"),
        active_thread("thread-newer", "project-newer", "2026-03-09T10:05:00.000Z"),
    ];
    assert_eq!(
        sort_sidebar_project_groups(projects.clone(), &threads, SidebarProjectSortOrder::Manual),
        projects
    );
    assert_eq!(
        project_ids(&sort_sidebar_project_groups(
            projects,
            &threads,
            SidebarProjectSortOrder::UpdatedAt
        )),
        ["project-newer", "project-older"]
    );
}

#[test]
fn a_hidden_subagent_thread_does_not_reorder_projects() {
    let projects = vec![
        project("project-older", "A older project", None),
        project("project-newer", "Z newer project", None),
    ];
    let threads = [
        active_thread(
            "thread-older-root",
            "project-older",
            "2026-03-09T10:01:00.000Z",
        ),
        active_thread(
            "thread-newer-root",
            "project-newer",
            "2026-03-09T10:05:00.000Z",
        ),
        ThreadSummary {
            subagent: true,
            parent: Some("thread-older-root".into()),
            ..active_thread(
                "thread-hidden-subagent",
                "project-older",
                "2026-03-09T10:10:00.000Z",
            )
        },
    ];
    assert_eq!(
        project_ids(&sort_sidebar_project_groups(
            projects,
            &threads,
            SidebarProjectSortOrder::UpdatedAt
        )),
        ["project-newer", "project-older"]
    );
}

#[test]
fn pin_order_keys_sort_between_their_bounds() {
    let middle = pin_order_key_between(None, None).unwrap();
    let top = pin_order_key_between(None, Some(&middle)).unwrap();
    let bottom = pin_order_key_between(Some(&middle), None).unwrap();
    assert!(top < middle && middle < bottom);
    let between = pin_order_key_between(Some(&top), Some(&middle)).unwrap();
    assert!(top < between && between < middle);
}

#[test]
fn pin_order_keys_extend_into_new_digits_between_adjacent_bounds() {
    let key = pin_order_key_between(Some("g"), Some("h")).unwrap();
    assert!("g" < key.as_str() && key.as_str() < "h");
}

#[test]
fn pin_order_keys_stay_ordered_under_repeated_top_insertion() {
    let mut head: Option<String> = None;
    let mut keys = BTreeSet::new();
    for _ in 0..100 {
        let key = pin_order_key_between(None, head.as_deref()).unwrap();
        if let Some(head) = &head {
            assert!(&key < head);
        }
        keys.insert(key.clone());
        head = Some(key);
    }
    assert_eq!(keys.len(), 100);
}

#[test]
fn pin_order_keys_stay_ordered_under_repeated_middle_insertion() {
    let mut low = pin_order_key_between(None, None).unwrap();
    let mut high = pin_order_key_between(Some(&low), None).unwrap();
    for index in 0..100 {
        let key = pin_order_key_between(Some(&low), Some(&high)).unwrap();
        assert!(low < key && key < high);
        if index % 2 == 0 {
            low = key;
        } else {
            high = key;
        }
    }
}

#[test]
fn pin_order_keys_refuse_corrupt_or_unordered_bounds() {
    assert_eq!(pin_order_key_between(Some("z"), Some("a")), None);
    assert_eq!(pin_order_key_between(Some("A!"), None), None);
    assert_eq!(pin_order_key_between(None, Some("ma")), None);
    assert_eq!(pin_order_key_between(Some("m"), Some("m")), None);
}

fn pinnable(id: &str, created_at: &str, key: Option<&str>) -> ThreadSummary {
    ThreadSummary {
        created_at: ms(created_at),
        pin_order_key: key.map(String::from),
        ..summary(id)
    }
}

#[test]
fn keyed_pins_sort_by_key_ahead_of_keyless_pins_newest_first() {
    let sorted = sort_pinned_threads_by_order_key(vec![
        pinnable("keyless-old", "2026-03-09T08:00:00.000Z", None),
        pinnable("second", "2026-03-09T09:00:00.000Z", Some("t")),
        pinnable("keyless-new", "2026-03-09T12:00:00.000Z", None),
        pinnable("first", "2026-03-09T07:00:00.000Z", Some("g")),
    ]);
    assert_eq!(
        ids(&sorted),
        ["first", "second", "keyless-new", "keyless-old"]
    );
}

#[test]
fn equal_pin_keys_break_by_id() {
    let sorted = sort_pinned_threads_by_order_key(vec![
        pinnable("b", "2026-03-09T10:00:00.000Z", Some("m")),
        pinnable("a", "2026-03-09T11:00:00.000Z", Some("m")),
    ]);
    assert_eq!(ids(&sorted), ["a", "b"]);
}

#[test]
fn a_sweep_covers_every_row_between_the_press_and_the_pointer_either_way() {
    let ordered = ["a", "b", "c", "d", "blocked"].map(String::from);
    let can_settle = |key: &str| key != "blocked";
    assert_eq!(
        resolve_sidebar_sweep_keys(&ordered, "b", "b", can_settle),
        ["b"]
    );
    assert_eq!(
        resolve_sidebar_sweep_keys(&ordered, "b", "d", can_settle),
        ["b", "c", "d"]
    );
    assert_eq!(
        resolve_sidebar_sweep_keys(&ordered, "d", "a", can_settle),
        ["a", "b", "c", "d"]
    );
}

#[test]
fn a_sweep_leaves_out_rows_that_cannot_apply_or_left_the_list() {
    let ordered = ["a", "b", "c", "d", "blocked"].map(String::from);
    let can_settle = |key: &str| key != "blocked";
    assert_eq!(
        resolve_sidebar_sweep_keys(&ordered, "c", "blocked", can_settle),
        ["c", "d"]
    );
    assert!(resolve_sidebar_sweep_keys(&ordered, "gone", "a", can_settle).is_empty());
}

#[test]
fn parking_the_open_thread_navigates_only_once_it_parked() {
    let now = ms("2026-09-12T10:00:00.000Z");
    let cases = [
        (
            ParkAction::Settle,
            Some(SettledOverride::Settled),
            None,
            "thread",
            true,
            false,
        ),
        (
            ParkAction::Settle,
            Some(SettledOverride::Active),
            None,
            "thread",
            false,
            false,
        ),
        (
            ParkAction::Settle,
            Some(SettledOverride::Settled),
            None,
            "other-thread",
            false,
            false,
        ),
        (
            ParkAction::Snooze,
            None,
            Some("2099-01-01T00:00:00.000Z"),
            "thread",
            true,
            false,
        ),
        (ParkAction::Snooze, None, None, "thread", false, false),
        (
            ParkAction::Snooze,
            None,
            Some("2026-09-12T09:00:00.000Z"),
            "thread",
            false,
            false,
        ),
        (
            ParkAction::Snooze,
            None,
            Some("2099-01-01T00:00:00.000Z"),
            "thread",
            false,
            true,
        ),
        (
            ParkAction::Snooze,
            None,
            Some("2099-01-01T00:00:00.000Z"),
            "other-thread",
            false,
            false,
        ),
    ];
    for (action, settled_override, snoozed_until, current, expected, approvals) in cases {
        let thread = ThreadSummary {
            settled_override,
            snoozed_until: snoozed_until.map(ms),
            has_pending_approvals: approvals,
            ..summary("thread")
        };
        assert_eq!(
            should_navigate_after_thread_park("thread", Some(current), action, now, Some(&thread)),
            expected,
            "{action:?} {settled_override:?} {snoozed_until:?} {current}"
        );
    }
}

#[test]
fn a_completed_thread_reads_by_whether_its_background_roster_wakes_it() {
    for (kind, status, top, receded, pill) in [
        (
            BackgroundKind::Command,
            SidebarThreadStatus::Ready,
            SidebarTopStatus::Done,
            false,
            ThreadStatusPill::Completed,
        ),
        (
            BackgroundKind::Monitor,
            SidebarThreadStatus::Waiting,
            SidebarTopStatus::Waiting,
            true,
            ThreadStatusPill::Waiting,
        ),
    ] {
        let state = crate::sync::fixtures::thread_state("Thread");
        let mut shell = agent_domain::shell(&state).unwrap();
        shell.latest_run = Some(agent_domain::RunId::new("run-background-completion").unwrap());
        shell.status = Some(agent_domain::RunStatus::Completed);
        shell.latest_run_completed_at =
            Some(agent_domain::Timestamp::parse("2026-06-20T01:00:00.000Z").unwrap());
        shell.last_visited_at =
            Some(agent_domain::Timestamp::parse("2026-06-20T00:59:00.000Z").unwrap());
        shell.pending_background_work = vec![agent_domain::PendingBackgroundSummary {
            key: "background-work".into(),
            kind,
            description: String::new(),
        }];
        let thread = ThreadSummary::from_shell(&shell);
        let unread = thread.has_unseen_completion();
        let resolved = resolve_sidebar_thread_status(&thread);
        assert!(unread);
        assert_eq!(resolved, status);
        assert_eq!(
            resolve_sidebar_top_status(resolved, unread, false),
            Some(top)
        );
        assert_eq!(
            should_recede_sidebar_thread(resolved, unread, false, false, false),
            receded
        );
        assert_eq!(is_thread_working(&thread), receded);
        assert_eq!(resolve_thread_status_pill(&thread), Some(pill));
    }
}

fn idle_with_completed_run() -> ThreadSummary {
    ThreadSummary {
        latest_run: Some(completed_run()),
        ..summary("thread")
    }
}

fn waiting_on_background() -> ThreadSummary {
    ThreadSummary {
        runtime: Some(runtime(RuntimeStatus::Idle)),
        pending_background: vec![BackgroundKind::Monitor],
        ..idle_with_completed_run()
    }
}

#[test]
fn the_working_shelf_folds_away_running_threads_and_background_waits_only() {
    let running = ThreadSummary {
        runtime: Some(runtime(RuntimeStatus::Running)),
        ..idle_with_completed_run()
    };
    assert!(is_thread_working(&running));
    assert!(is_thread_working(&waiting_on_background()));
    assert!(!is_thread_working(&idle_with_completed_run()));
    assert!(!is_thread_working(&ThreadSummary {
        has_pending_approvals: true,
        ..running.clone()
    }));
    assert!(!is_thread_working(&ThreadSummary {
        has_pending_user_input: true,
        ..running
    }));
    assert!(!is_thread_working(&ThreadSummary {
        runtime: Some(RuntimeSummary {
            last_error: Some("boom".into()),
            ..runtime(RuntimeStatus::Failed)
        }),
        ..waiting_on_background()
    }));
}

#[test]
fn a_ready_plan_stays_in_the_inbox_while_background_work_runs() {
    assert!(!is_thread_working(&ThreadSummary {
        interaction_mode: InteractionMode::Plan,
        has_actionable_proposed_plan: true,
        ..waiting_on_background()
    }));
}

fn inbox_thread(
    id: &str,
    created_at: &str,
    completed_at: Option<Option<&str>>,
    unsettled_at: Option<&str>,
) -> ThreadSummary {
    ThreadSummary {
        created_at: ms(created_at),
        unsettled_at: unsettled_at.map(ms),
        latest_run: completed_at.map(|completed_at| RunSummary {
            requested_at: Some(ms(created_at)),
            ..latest_run(completed_at, Some("2026-03-09T10:00:00.000Z"))
        }),
        ..summary(id)
    }
}

#[test]
fn the_inbox_puts_the_thread_that_finished_last_on_top_whatever_its_age() {
    let sorted = sort_inbox_threads_by_return(
        vec![
            inbox_thread("new", "2026-03-09T11:00:00.000Z", None, None),
            inbox_thread(
                "old-finished-now",
                "2026-03-01T09:00:00.000Z",
                Some(Some("2026-03-09T12:00:00.000Z")),
                None,
            ),
            inbox_thread(
                "reopened",
                "2026-03-02T09:00:00.000Z",
                None,
                Some("2026-03-09T11:30:00.000Z"),
            ),
        ],
        &InboxReturns::default(),
    );
    assert_eq!(ids(&sorted), ["old-finished-now", "reopened", "new"]);
}

#[test]
fn the_inbox_counts_a_return_the_host_does_not_stamp() {
    let waiting = inbox_thread(
        "asks-approval",
        "2026-03-09T09:00:00.000Z",
        Some(None),
        None,
    );
    let finished = inbox_thread(
        "finished",
        "2026-03-09T09:30:00.000Z",
        Some(Some("2026-03-09T11:00:00.000Z")),
        None,
    );
    let sorted = sort_inbox_threads_by_return(
        vec![finished.clone(), waiting.clone()],
        &InboxReturns::default(),
    );
    assert_eq!(ids(&sorted), ["finished", "asks-approval"]);
    let mut returns = InboxReturns::default();
    let working = ThreadSummary {
        runtime: Some(runtime(RuntimeStatus::Running)),
        ..waiting.clone()
    };
    returns.observe(Some(&[finished.clone(), working]), 0);
    returns.observe(
        Some(&[finished.clone(), waiting.clone()]),
        ms("2026-03-09T11:05:00.000Z"),
    );
    let sorted = sort_inbox_threads_by_return(vec![finished, waiting], &returns);
    assert_eq!(ids(&sorted), ["asks-approval", "finished"]);
}

fn marker(marker: SidebarListMarker) -> SidebarListItem {
    SidebarListItem::Marker { marker }
}

fn row(key: &str, section: SidebarSection) -> SidebarListItem {
    SidebarListItem::Thread {
        key: key.into(),
        section,
    }
}

/// Pinned p1 | Active a1 a2 | Working w1 | Settled s1
fn drag_items() -> Vec<SidebarListItem> {
    vec![
        marker(SidebarListMarker::PinnedHeader),
        row("p1", SidebarSection::Pinned),
        marker(SidebarListMarker::PinnedDivider),
        row("a1", SidebarSection::Active),
        row("a2", SidebarSection::Active),
        marker(SidebarListMarker::WorkingHeader),
        row("w1", SidebarSection::Working),
        marker(SidebarListMarker::SettledHeader),
        row("s1", SidebarSection::Settled),
    ]
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn drops_never_land_in_the_working_shelf_which_stays_out_of_the_inbox_order() {
    let items = drag_items();
    assert_eq!(resolve_sidebar_drop_target(&items, "a1", "w1"), None);
    assert_eq!(
        resolve_sidebar_drop_target(&items, "p1", "a2"),
        Some(SidebarDropTarget {
            section: DropSection::Active,
            pinned_order: vec![],
            active_order: strings(&["a1", "a2", "p1"]),
        })
    );
    assert_eq!(
        resolve_sidebar_drop_verb(SidebarSection::Active, Some(SidebarSection::Working)),
        None
    );
}

fn keys(entries: &[(&str, &str)]) -> BTreeMap<String, Option<String>> {
    entries
        .iter()
        .map(|(id, key)| (id.to_string(), Some(key.to_string())))
        .collect()
}

#[test]
fn a_time_ordered_inbox_drop_only_changes_lifecycle() {
    let pinned_order = strings(&["p1"]);
    let active_order = strings(&["a1", "a2"]);
    let pinned_keys = keys(&[("p1", "m")]);
    let active_keys = keys(&[("a1", "f"), ("a2", "t")]);
    let plan = |active_key, active_section, target: &SidebarDropTarget| {
        plan_sidebar_thread_drop(&SidebarDropInput {
            active_key,
            active_section,
            active_pinned: None,
            active_settled: None,
            target,
            pinned_order: &pinned_order,
            pinned_keys: &pinned_keys,
            active_order: &active_order,
            active_keys: &active_keys,
            active_time_ordered: true,
        })
    };
    assert_eq!(
        plan(
            "a1",
            SidebarSection::Active,
            &SidebarDropTarget {
                section: DropSection::Active,
                pinned_order: strings(&["p1"]),
                active_order: strings(&["a2", "a1"]),
            }
        ),
        SidebarThreadDropPlan::None
    );
    assert_eq!(
        plan(
            "p1",
            SidebarSection::Pinned,
            &SidebarDropTarget {
                section: DropSection::Active,
                pinned_order: vec![],
                active_order: strings(&["a1", "p1", "a2"]),
            }
        ),
        SidebarThreadDropPlan::MoveActive {
            order: None,
            assignments: vec![],
            unpin: true,
            unsettle: false,
            unsnooze: false,
        }
    );
}

#[test]
fn drop_verbs_name_the_lifecycle_change() {
    use SidebarSection::*;
    assert_eq!(
        resolve_sidebar_drop_verb(Active, Some(Pinned)),
        Some(SidebarDropVerb::Pin)
    );
    assert_eq!(
        resolve_sidebar_drop_verb(Pinned, Some(Settled)),
        Some(SidebarDropVerb::Settle)
    );
    assert_eq!(
        resolve_sidebar_drop_verb(Pinned, Some(Active)),
        Some(SidebarDropVerb::Unpin)
    );
    assert_eq!(
        resolve_sidebar_drop_verb(Settled, Some(Active)),
        Some(SidebarDropVerb::Unsettle)
    );
    assert_eq!(
        resolve_sidebar_drop_verb(Snoozed, Some(Active)),
        Some(SidebarDropVerb::Wake)
    );
    assert_eq!(resolve_sidebar_drop_verb(Active, Some(Active)), None);
    assert_eq!(resolve_sidebar_drop_verb(Active, Some(Snoozed)), None);
    assert_eq!(resolve_sidebar_drop_verb(Active, None), None);
}

#[test]
fn a_keyed_drop_into_the_pins_carries_the_moved_rows_key_on_the_pin() {
    let items = drag_items();
    let target = resolve_sidebar_drop_target(&items, "a1", "p1").unwrap();
    assert_eq!(target.section, DropSection::Pinned);
    assert_eq!(target.pinned_order, strings(&["a1", "p1"]));
    let pinned_order = strings(&["p1"]);
    let active_order = strings(&["a1", "a2"]);
    let pinned_keys = keys(&[("p1", "m")]);
    let active_keys = keys(&[("a1", "f"), ("a2", "t")]);
    let SidebarThreadDropPlan::Pin {
        order,
        order_key: Some(order_key),
        extra_assignments,
    } = plan_sidebar_thread_drop(&SidebarDropInput {
        active_key: "a1",
        active_section: SidebarSection::Active,
        active_pinned: None,
        active_settled: None,
        target: &target,
        pinned_order: &pinned_order,
        pinned_keys: &pinned_keys,
        active_order: &active_order,
        active_keys: &active_keys,
        active_time_ordered: false,
    })
    else {
        panic!("expected a pin");
    };
    assert_eq!(order, strings(&["a1", "p1"]));
    assert!(order_key.as_str() < "m");
    assert!(extra_assignments.is_empty());
}

#[test]
fn dropping_onto_the_settled_shelf_settles_and_a_settled_row_back_there_is_a_no_op() {
    let items = drag_items();
    let target = resolve_sidebar_drop_target(&items, "a1", "s1").unwrap();
    assert_eq!(target.section, DropSection::Settled);
    let empty = BTreeMap::new();
    let plan = |section| {
        plan_sidebar_thread_drop(&SidebarDropInput {
            active_key: "a1",
            active_section: section,
            active_pinned: None,
            active_settled: None,
            target: &target,
            pinned_order: &[],
            pinned_keys: &empty,
            active_order: &[],
            active_keys: &empty,
            active_time_ordered: false,
        })
    };
    assert_eq!(plan(SidebarSection::Active), SidebarThreadDropPlan::Settle);
    assert_eq!(plan(SidebarSection::Settled), SidebarThreadDropPlan::None);
}

#[test]
fn a_drop_preview_settles_or_resumes_the_thread_like_the_host() {
    let now = 1_000;
    let pinned_snoozed = ThreadSummary {
        pinned_at: Some(10),
        pin_order_key: Some("m".into()),
        active_order_key: Some("f".into()),
        snoozed_at: Some(20),
        snoozed_until: Some(5_000),
        ..summary("thread")
    };
    let settled = apply_sidebar_thread_drop(&pinned_snoozed, DropSection::Settled, now, None);
    assert_eq!(
        (
            settled.pinned_at,
            settled.pin_order_key.clone(),
            settled.active_order_key.clone(),
            settled.settled_override,
            settled.settled_at,
            settled.snoozed_until,
        ),
        (
            None,
            None,
            None,
            Some(SettledOverride::Settled),
            Some(now),
            None
        )
    );
    let resumed = apply_sidebar_thread_drop(&settled, DropSection::Active, 2_000, Some("k"));
    assert_eq!(
        (
            resumed.settled_override,
            resumed.settled_at,
            resumed.unsettled_at,
            resumed.pinned_at,
            resumed.active_order_key.as_deref(),
        ),
        (
            Some(SettledOverride::Active),
            None,
            Some(2_000),
            None,
            Some("k")
        )
    );
    let pinned = apply_sidebar_thread_drop(&pinned_snoozed, DropSection::Pinned, now, None);
    assert_eq!(
        (
            pinned.pinned_at,
            pinned.pin_order_key.as_deref(),
            pinned.snoozed_at
        ),
        (Some(10), Some("m"), None)
    );
}

#[test]
fn a_new_thread_in_the_current_project_needs_shift_unless_there_is_one_project() {
    assert!(should_create_new_thread_in_current_project(true, 3));
    assert!(should_create_new_thread_in_current_project(false, 1));
    assert!(!should_create_new_thread_in_current_project(false, 2));
}

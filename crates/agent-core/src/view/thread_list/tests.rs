use super::*;
use crate::view::thread_menu::{ThreadMenuAction, ThreadMenuChild, ThreadMenuItemId};
use crate::view::thread_order::MoveDestination;
use crate::view::thread_sort::{MoveDirection, OrderAssignment, plan_pinned_move};
use crate::view::thread_summary::{
    RunSummary, RuntimeStatus, RuntimeSummary,
    fixtures::{ms, run, runtime, summary},
};
use crate::view::time::fixtures::local;
use chrono::{Duration, Utc};

const NOW: &str = "2026-06-02T00:00:00.000Z";
const MINUTE_MS: i64 = 60_000;

fn now() -> i64 {
    ms(NOW)
}
fn thread(id: &str, title: &str) -> ThreadSummary {
    ThreadSummary {
        title: title.into(),
        project: "project-test".into(),
        created_at: ms("2026-01-01T00:00:00.000Z"),
        updated_at: ms("2026-01-01T00:00:00.000Z"),
        ..summary(id)
    }
}
fn settled_at(mut thread: ThreadSummary, at: &str) -> ThreadSummary {
    thread.settled_override = Some(SettledOverride::Settled);
    thread.settled_at = Some(ms(at));
    thread
}
fn snoozed(mut thread: ThreadSummary, until: &str, at: &str) -> ThreadSummary {
    thread.snoozed_until = Some(ms(until));
    thread.snoozed_at = Some(ms(at));
    thread
}
fn running() -> Option<RuntimeSummary> {
    Some(runtime(RuntimeStatus::Running))
}
fn input(threads: &[ThreadSummary]) -> ThreadListInput<'_> {
    ThreadListInput {
        threads,
        now_ms: now(),
        ..ThreadListInput::default()
    }
}
fn ids(layout: &ThreadListLayout) -> Vec<String> {
    layout
        .items
        .iter()
        .map(|item| item.thread.id.clone())
        .collect()
}
fn section_ids(
    threads: &[ThreadSummary],
    section: OrderSection,
    queued: &BTreeSet<String>,
) -> Vec<String> {
    ordered_section(threads, section, None, now(), queued)
        .iter()
        .map(|thread| thread.id.clone())
        .collect()
}
fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}
fn pending_task(id: &str) -> PendingTaskRow {
    PendingTaskRow {
        key: format!("pending-task:{id}"),
        kind: PendingTaskKind::Queued {
            command_id: format!("command-{id}"),
            thread_id: format!("thread-{id}"),
        },
        project_id: "project-1".into(),
        project_title: "project-1".into(),
        title: id.into(),
        branch: None,
        created_at_ms: now(),
        show_pending_divider: false,
        show_trailing_divider: false,
    }
}
fn list_input<'a>(layout: &'a ThreadListLayout<'a>, clock: i64) -> ListItemsInput<'a> {
    ListItemsInput {
        layout,
        pending_tasks: vec![],
        now_ms: clock,
        wake_label_now_ms: clock,
        snooze_presets: &[],
        queued: None,
        move_availability: None,
        selected: None,
        active_reorderable: true,
        working_shelf_expanded: false,
        snoozed_shelf_expanded: false,
        settled_shelf_expanded: true,
        shelf_preferences_loading: false,
    }
}
fn label(item: &ThreadListItem) -> String {
    match item {
        ThreadListItem::Thread { row } => row.id.clone(),
        ThreadListItem::PendingTask { task } => task.title.clone(),
        shelf => shelf.key().to_owned(),
    }
}
fn kind(item: &ThreadListItem) -> &'static str {
    match item {
        ThreadListItem::Thread { .. } => "thread",
        ThreadListItem::PendingTask { .. } => "pending",
        ThreadListItem::WorkingShelf { .. } => "working-shelf",
        ThreadListItem::SnoozedShelf { .. } => "snoozed-shelf",
        ThreadListItem::SettledShelf { .. } => "settled-shelf",
    }
}
fn row<'a>(items: &'a [ThreadListItem], id: &str) -> &'a ThreadRow {
    items
        .iter()
        .find_map(|item| match item {
            ThreadListItem::Thread { row } if row.id == id => Some(row),
            _ => None,
        })
        .unwrap()
}

#[test]
fn accepts_a_displayed_evening_preset_while_its_wake_time_is_still_future() {
    let opened = local(2026, 5, 8, 16, 59) + Duration::seconds(30);
    let selected = local(2026, 5, 8, 17, 0) + Duration::seconds(30);
    let displayed = resolve_snooze_presets(&opened, TimestampFormat::Locale);
    let evening = displayed
        .iter()
        .find(|preset| preset.id == SnoozePresetId::Evening)
        .unwrap()
        .clone();
    assert_eq!(
        resolve_snooze_menu_selection(
            SnoozePresetId::Evening,
            &displayed,
            &selected,
            TimestampFormat::Locale
        ),
        SnoozeMenuSelection::Selected { preset: evening }
    );
}

#[test]
fn expires_a_displayed_preset_once_its_wake_time_has_passed() {
    let displayed = resolve_snooze_presets(
        &(local(2026, 5, 8, 16, 59) + Duration::seconds(30)),
        TimestampFormat::Locale,
    );
    assert_eq!(
        resolve_snooze_menu_selection(
            SnoozePresetId::Evening,
            &displayed,
            &(local(2026, 5, 8, 18, 0) + Duration::seconds(1)),
            TimestampFormat::Locale
        ),
        SnoozeMenuSelection::Expired
    );
}

#[test]
fn recomputes_presets_that_remain_available_instead_of_using_old_timestamps() {
    let displayed = resolve_snooze_presets(&local(2026, 5, 8, 10, 0), TimestampFormat::Locale);
    let selected = local(2026, 5, 8, 10, 30);
    let SnoozeMenuSelection::Selected { preset } = resolve_snooze_menu_selection(
        SnoozePresetId::Hour,
        &displayed,
        &selected,
        TimestampFormat::Locale,
    ) else {
        panic!("expected a selection");
    };
    assert_eq!(
        preset.snoozed_until,
        crate::view::thread_summary::iso(selected.timestamp_millis() + 60 * MINUTE_MS)
    );
}

#[test]
fn distinguishes_usage_limits_from_ordinary_failures_and_clears_the_label_after_recovery() {
    let mut thread = thread("limited", "Limited");
    thread.runtime = Some(RuntimeSummary {
        last_error: Some("Plan limit reached".into()),
        last_error_class: Some("usage_limit".into()),
        ..runtime(RuntimeStatus::Failed)
    });
    assert_eq!(thread_list_status(&thread), ThreadListStatus::Limited);
    thread.runtime.as_mut().unwrap().last_error_class = None;
    assert_eq!(thread_list_status(&thread), ThreadListStatus::Failed);
    thread.runtime.as_mut().unwrap().status = RuntimeStatus::Completed;
    assert_eq!(thread_list_status(&thread), ThreadListStatus::Ready);
}

#[test]
fn prioritizes_approval_over_a_running_runtime() {
    let thread = ThreadSummary {
        has_pending_approvals: true,
        runtime: Some(RuntimeSummary {
            active_run: Some("run-t".into()),
            ..runtime(RuntimeStatus::Running)
        }),
        ..thread("t", "t")
    };
    assert_eq!(thread_list_status(&thread), ThreadListStatus::Approval);
}

#[test]
fn reports_waiting_when_the_runtime_parks_idle_for_background_tasks() {
    let thread = ThreadSummary {
        pending_background: vec![agent_domain::BackgroundKind::Monitor],
        runtime: Some(runtime(RuntimeStatus::Idle)),
        ..thread("t", "t")
    };
    assert_eq!(thread_list_status(&thread), ThreadListStatus::Waiting);
}

#[test]
fn presents_an_unseen_completion_by_its_background_roster() {
    use agent_domain::{BackgroundKind, PendingBackgroundSummary, RunId, RunStatus};
    for (kind, status) in [
        (BackgroundKind::Command, ThreadListStatus::Ready),
        (BackgroundKind::Monitor, ThreadListStatus::Waiting),
    ] {
        let mut shell =
            agent_domain::shell(&crate::sync::fixtures::thread_state("Thread")).unwrap();
        shell.latest_run = Some(RunId::new("run-background-completion").unwrap());
        shell.status = Some(RunStatus::Completed);
        shell.latest_run_completed_at = Some(Timestamp::parse(NOW).unwrap());
        shell.last_visited_at = Some(Timestamp::parse("2026-06-01T23:59:00.000Z").unwrap());
        shell.pending_background_work = vec![PendingBackgroundSummary {
            key: "background-work".into(),
            kind,
            description: String::new(),
        }];
        let thread = ThreadSummary::from_shell(&shell);
        assert_eq!(thread_list_status(&thread), status, "{kind:?}");
        assert!(thread.has_unseen_completion());
    }
}

#[test]
fn resolves_ready_for_quiescent_threads() {
    assert_eq!(
        thread_list_status(&thread("t", "t")),
        ThreadListStatus::Ready
    );
}

fn queued_fixture() -> Vec<ThreadSummary> {
    vec![
        thread("active", "Active"),
        ThreadSummary {
            settled_override: Some(SettledOverride::Settled),
            ..thread("settled", "Settled")
        },
        ThreadSummary {
            settled_override: Some(SettledOverride::Settled),
            ..thread("settled-queued", "Settled with outbox")
        },
    ]
}

#[test]
fn queued_messages_list_a_settled_thread_in_the_active_block() {
    let threads = queued_fixture();
    let queued = set(&["settled-queued"]);
    let layout = build_thread_list_layout(&ThreadListInput {
        queued: Some(&queued),
        ..input(&threads)
    });
    let rows: Vec<(String, RowVariant)> = layout
        .items
        .iter()
        .map(|item| (item.thread.id.clone(), item.variant))
        .collect();
    assert_eq!(
        rows,
        [
            ("active".to_string(), RowVariant::Card),
            ("settled-queued".to_string(), RowVariant::Card),
            ("settled".to_string(), RowVariant::Slim),
        ]
    );
    assert_eq!(layout.settled_count, 1);
}

#[test]
fn queued_messages_keep_a_settled_thread_in_the_reorderable_active_section() {
    let threads = queued_fixture();
    assert_eq!(
        section_ids(&threads, OrderSection::Active, &set(&["settled-queued"])),
        ["active", "settled-queued"]
    );
    assert_eq!(
        section_ids(&threads, OrderSection::Active, &BTreeSet::new()),
        ["active"]
    );
}

fn swipe(primary: SwipeAction, secondary: Option<SwipeAction>) -> SwipeActions {
    SwipeActions { primary, secondary }
}

#[test]
fn offers_settle_and_snooze_for_an_active_snoozable_thread() {
    assert_eq!(
        resolve_swipe_actions(RowVariant::Card, true, false),
        swipe(SwipeAction::Settle, Some(SwipeAction::Snooze))
    );
}

#[test]
fn offers_unsettle_and_snooze_for_settled_history() {
    assert_eq!(
        resolve_swipe_actions(RowVariant::Slim, true, false),
        swipe(SwipeAction::Unsettle, Some(SwipeAction::Snooze))
    );
}

#[test]
fn omits_snooze_when_the_thread_does_not_allow_it() {
    assert_eq!(
        resolve_swipe_actions(RowVariant::Card, false, false),
        swipe(SwipeAction::Settle, None)
    );
}

#[test]
fn offers_wake_and_no_snooze_on_a_snoozed_row() {
    assert_eq!(
        resolve_swipe_actions(RowVariant::Slim, true, true),
        swipe(SwipeAction::Unsnooze, None)
    );
}

#[test]
fn reports_when_an_unadopted_turns_grace_window_lapses() {
    let thread = ThreadSummary {
        latest_user_message_at: Some(ms("2026-06-02T00:00:30.000Z")),
        ..thread("t", "t")
    };
    assert_eq!(
        snooze_gate_expiry_ms(&thread, ms("2026-06-02T00:01:00.000Z")),
        Some(ms("2026-06-02T00:02:30.000Z"))
    );
}

#[test]
fn has_no_gate_expiry_once_snoozable_or_when_only_data_can_unblock_it() {
    assert_eq!(
        snooze_gate_expiry_ms(&thread("ready", "Ready"), now()),
        None
    );
    let blocked = ThreadSummary {
        has_pending_approvals: true,
        latest_user_message_at: Some(now()),
        ..thread("blocked", "Blocked")
    };
    assert_eq!(snooze_gate_expiry_ms(&blocked, now()), None);
}

#[test]
fn honors_a_saved_active_order_and_leaves_new_threads_above_it() {
    let created = |id: &str, at: &str, key: Option<&str>| ThreadSummary {
        created_at: ms(at),
        active_order_key: key.map(String::from),
        ..thread(id, id)
    };
    let sorted = sort_active_threads_by_order_key(vec![
        created("newer-arranged", "2026-06-01T12:00:00.000Z", Some("t")),
        created("older-arranged", "2026-06-01T08:00:00.000Z", Some("f")),
        created("new", "2026-06-01T13:00:00.000Z", None),
    ]);
    let ids: Vec<&str> = sorted.iter().map(|thread| thread.id.as_str()).collect();
    assert_eq!(ids, ["new", "older-arranged", "newer-arranged"]);
}

#[test]
fn orders_active_threads_by_creation_time_newest_first_ignoring_activity() {
    let created = |id: &str, at: &str| ThreadSummary {
        created_at: ms(at),
        ..thread(id, id)
    };
    let sorted = sort_active_threads_by_order_key(vec![
        created("oldest", "2026-06-01T08:00:00.000Z"),
        created("newest", "2026-06-01T12:00:00.000Z"),
        created("middle", "2026-06-01T10:00:00.000Z"),
    ]);
    let ids: Vec<&str> = sorted.iter().map(|thread| thread.id.as_str()).collect();
    assert_eq!(ids, ["newest", "middle", "oldest"]);
}

#[test]
fn uses_each_saved_order_and_excludes_settled_snoozed_and_archived_rows() {
    let keyed = |id: &str, pin: Option<&str>, active: Option<&str>| ThreadSummary {
        pinned_at: pin.map(|_| now()),
        pin_order_key: pin.map(String::from),
        active_order_key: active.map(String::from),
        ..thread(id, id)
    };
    let threads = vec![
        keyed("active-later", None, Some("t")),
        keyed("active-first", None, Some("f")),
        keyed("active-new", None, None),
        keyed("pinned-later", Some("t"), Some("f")),
        keyed("pinned-first", Some("f"), Some("t")),
        ThreadSummary {
            settled_override: Some(SettledOverride::Settled),
            ..thread("settled", "Settled")
        },
        ThreadSummary {
            archived_at: Some(now()),
            ..thread("archived", "Archived")
        },
        snoozed(
            thread("snoozed", "Snoozed"),
            "2026-06-03T10:00:00.000Z",
            NOW,
        ),
        ThreadSummary {
            pinned_at: Some(now()),
            ..snoozed(
                thread("pinned-snoozed", "Pinned snoozed"),
                "2026-06-03T10:00:00.000Z",
                NOW,
            )
        },
    ];
    let none = BTreeSet::new();
    assert_eq!(
        section_ids(&threads, OrderSection::Active, &none),
        ["active-new", "active-first", "active-later"]
    );
    assert_eq!(
        section_ids(&threads, OrderSection::Pinned, &none),
        ["pinned-first", "pinned-later"]
    );
}

#[test]
fn places_a_persisted_settled_thread_in_the_settled_shelf() {
    let threads = vec![ThreadSummary {
        linked_pull_request: Some(agent_domain::LinkedPullRequest {
            project: "project-1".into(),
            repository: "acme/app".into(),
            number: 42,
            url: "https://github.com/acme/app/pull/42".into(),
        }),
        ..settled_at(thread("linked-merged", "Linked merged pull request"), NOW)
    }];
    let layout = build_thread_list_layout(&input(&threads));
    assert_eq!(layout.settled_count, 1);
    assert_eq!(layout.items[0].variant, RowVariant::Slim);
}

#[test]
fn hides_snoozed_threads_and_counts_them() {
    let threads = vec![
        thread("active", "Active"),
        snoozed(
            thread("snoozed", "Snoozed"),
            "2026-06-03T09:00:00.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
        snoozed(
            thread("woken", "Woken"),
            "2026-06-01T18:00:00.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
    ];
    let layout = build_thread_list_layout(&input(&threads));
    assert_eq!(ids(&layout), ["active", "woken"]);
    assert_eq!(layout.snoozed_count, 1);
}

#[test]
fn moves_a_settled_pinned_thread_into_the_settled_shelf() {
    let threads = vec![
        thread("active", "Active"),
        ThreadSummary {
            pinned_at: Some(ms("2026-06-01T12:00:00.000Z")),
            ..settled_at(
                thread("pinned-settled", "Pinned while settled"),
                "2026-06-01T12:00:00.000Z",
            )
        },
    ];
    let layout = build_thread_list_layout(&input(&threads));
    assert_eq!(ids(&layout), ["active", "pinned-settled"]);
    let pinned: Vec<bool> = layout.items.iter().map(|item| item.pinned).collect();
    assert_eq!(pinned, [false, false]);
    assert_eq!(layout.settled_count, 1);
}

#[test]
fn keeps_active_pinned_threads_in_the_pinned_block() {
    let threads = vec![ThreadSummary {
        pinned_at: Some(ms("2026-06-01T12:00:00.000Z")),
        ..thread("pinned", "Pinned thread")
    }];
    let layout = build_thread_list_layout(&input(&threads));
    assert_eq!(layout.items[0].thread.id, "pinned");
    assert_eq!(layout.items[0].variant, RowVariant::Card);
    assert!(layout.items[0].pinned);
    assert_eq!(layout.settled_count, 0);
}

#[test]
fn snooze_hides_a_pinned_thread_and_wake_restores_it_to_the_pinned_block() {
    let threads = vec![
        thread("active", "Active"),
        ThreadSummary {
            pinned_at: Some(ms("2026-06-01T12:00:00.000Z")),
            ..snoozed(
                thread("pinned-snoozed", "Pinned and snoozed"),
                "2026-06-03T09:00:00.000Z",
                "2026-06-01T11:00:00.000Z",
            )
        },
    ];
    let while_snoozed = build_thread_list_layout(&input(&threads));
    assert_eq!(ids(&while_snoozed), ["active"]);
    assert_eq!(while_snoozed.snoozed_count, 1);
    let after_wake = build_thread_list_layout(&ThreadListInput {
        now_ms: ms("2026-06-03T10:00:00.000Z"),
        ..input(&threads)
    });
    assert_eq!(ids(&after_wake), ["pinned-snoozed", "active"]);
    assert!(after_wake.items[0].pinned);
    assert_eq!(after_wake.snoozed_count, 0);
}

#[test]
fn classifies_snooze_with_the_second_precise_clock_and_reports_the_next_wake() {
    let threads = vec![
        snoozed(
            thread("just-woke", "Just woke"),
            "2026-06-02T00:00:30.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
        snoozed(
            thread("still-snoozed", "Still snoozed"),
            "2026-06-02T09:00:00.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
    ];
    let layout = build_thread_list_layout(&ThreadListInput {
        now_ms: ms("2026-06-02T00:01:07.500Z"),
        ..input(&threads)
    });
    assert_eq!(ids(&layout), ["just-woke"]);
    assert_eq!(layout.snoozed_count, 1);
    assert_eq!(
        layout.next_snooze_wake_at,
        Some(ms("2026-06-02T09:00:00.000Z"))
    );
}

#[test]
fn builds_snoozed_rows_between_active_and_settled_when_the_shelf_is_expanded() {
    let threads = vec![
        thread("active", "Active"),
        settled_at(thread("settled", "Settled"), NOW),
        snoozed(
            thread("later", "Wakes later"),
            "2026-06-03T09:00:00.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
        snoozed(
            thread("sooner", "Wakes sooner"),
            "2026-06-02T09:00:00.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
    ];
    let layout = build_thread_list_layout(&ThreadListInput {
        snoozed_shelf_expanded: true,
        ..input(&threads)
    });
    assert_eq!(ids(&layout), ["active", "sooner", "later", "settled"]);
    let snoozed: Vec<bool> = layout.items.iter().map(|item| item.snoozed).collect();
    assert_eq!(snoozed, [false, true, true, false]);
    assert_eq!(layout.snoozed_shelf_header_index, Some(1));
    assert_eq!(layout.snoozed_count, 2);
}

#[test]
fn collapses_to_a_header_only_shelf() {
    let threads = vec![snoozed(
        thread("snoozed", "Snoozed"),
        "2026-06-03T09:00:00.000Z",
        "2026-06-01T12:00:00.000Z",
    )];
    let layout = build_thread_list_layout(&input(&threads));
    assert!(layout.items.is_empty());
    assert_eq!(layout.snoozed_count, 1);
    assert_eq!(layout.snoozed_shelf_header_index, Some(0));
}

#[test]
fn keeps_the_selected_thread_on_a_collapsed_shelf() {
    let threads = vec![
        snoozed(
            thread("open", "Open"),
            "2026-06-03T09:00:00.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
        snoozed(
            thread("other", "Other"),
            "2026-06-03T10:00:00.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
    ];
    let layout = build_thread_list_layout(&ThreadListInput {
        selected: Some("open"),
        ..input(&threads)
    });
    assert_eq!(ids(&layout), ["open"]);
    assert!(layout.items[0].snoozed);
    assert_eq!(layout.snoozed_count, 2);
}

#[test]
fn partitions_settled_threads_into_a_slim_shelf() {
    let threads = vec![
        thread("active", "Active"),
        settled_at(thread("settled", "Settled"), NOW),
        settled_at(thread("settled-2", "Settled 2"), NOW),
    ];
    let layout = build_thread_list_layout(&input(&threads));
    let rows: Vec<(String, RowVariant)> = layout
        .items
        .iter()
        .map(|item| (item.thread.id.clone(), item.variant))
        .collect();
    assert_eq!(
        rows,
        [
            ("active".to_string(), RowVariant::Card),
            ("settled".to_string(), RowVariant::Slim),
            ("settled-2".to_string(), RowVariant::Slim),
        ]
    );
    let last: Vec<bool> = layout.items.iter().map(|item| item.is_last).collect();
    assert_eq!(last, [false, false, true]);
    assert_eq!(layout.settled_count, 2);
    assert_eq!(layout.settled_shelf_header_index, Some(1));
}

#[test]
fn collapses_settled_threads_to_a_counted_shelf_header() {
    let threads = vec![
        thread("active", "Active"),
        settled_at(thread("settled", "Settled"), NOW),
    ];
    let layout = build_thread_list_layout(&ThreadListInput {
        settled_shelf_expanded: false,
        ..input(&threads)
    });
    assert_eq!(ids(&layout), ["active"]);
    assert_eq!(layout.settled_count, 1);
    assert_eq!(layout.settled_shelf_header_index, Some(1));
}

#[test]
fn keeps_the_selected_settled_thread_visible_when_its_shelf_is_collapsed() {
    let threads = vec![
        settled_at(thread("selected", "Selected"), NOW),
        settled_at(thread("other", "Other"), NOW),
    ];
    let layout = build_thread_list_layout(&ThreadListInput {
        settled_shelf_expanded: false,
        selected: Some("selected"),
        ..input(&threads)
    });
    assert_eq!(ids(&layout), ["selected"]);
    assert_eq!(layout.settled_count, 2);
    assert_eq!(layout.settled_shelf_header_index, Some(0));
}

#[test]
fn keeps_cards_in_creation_order_while_settled_sorts_by_recency() {
    let threads = vec![
        ThreadSummary {
            created_at: ms("2026-06-01T08:00:00.000Z"),
            updated_at: now(),
            ..thread("older-created", "Older")
        },
        ThreadSummary {
            created_at: ms("2026-06-01T12:00:00.000Z"),
            ..thread("newer-created", "Newer")
        },
    ];
    assert_eq!(
        ids(&build_thread_list_layout(&input(&threads))),
        ["newer-created", "older-created"]
    );
}

#[test]
fn sorts_settled_threads_by_their_persisted_settlement_timestamp() {
    let threads = vec![
        ThreadSummary {
            latest_user_message_at: Some(ms("2026-06-01T08:00:00.000Z")),
            ..settled_at(
                thread("settled-newer", "Settled newer"),
                "2026-06-01T12:00:00.000Z",
            )
        },
        ThreadSummary {
            latest_user_message_at: Some(ms("2026-06-01T09:00:00.000Z")),
            ..settled_at(
                thread("settled-older", "Settled older"),
                "2026-06-01T10:00:00.000Z",
            )
        },
    ];
    assert_eq!(
        ids(&build_thread_list_layout(&input(&threads))),
        ["settled-newer", "settled-older"]
    );
}

#[test]
fn keeps_settled_threads_in_the_tail_and_filters_by_search_query() {
    let threads = vec![
        thread("match", "Fix login bug"),
        thread("miss", "Greeting"),
        settled_at(thread("settled", "Fix login again"), NOW),
    ];
    let layout = build_thread_list_layout(&ThreadListInput {
        search_query: "login",
        ..input(&threads)
    });
    let rows: Vec<(String, RowVariant)> = layout
        .items
        .iter()
        .map(|item| (item.thread.id.clone(), item.variant))
        .collect();
    assert_eq!(
        rows,
        [
            ("match".to_string(), RowVariant::Card),
            ("settled".to_string(), RowVariant::Slim),
        ]
    );
}

#[test]
fn includes_a_thread_matched_by_message_content() {
    let threads = vec![thread("content-match", "Unrelated title")];
    let matched = set(&["content-match"]);
    let layout = build_thread_list_layout(&ThreadListInput {
        search_query: "relay reconnect",
        matched: Some(&matched),
        ..input(&threads)
    });
    assert_eq!(ids(&layout), ["content-match"]);
}

#[test]
fn scopes_the_flat_list_to_one_project() {
    let threads = vec![
        ThreadSummary {
            project: "project-1".into(),
            ..thread("included", "Included")
        },
        ThreadSummary {
            project: "project-2".into(),
            ..thread("excluded", "Excluded")
        },
    ];
    let layout = build_thread_list_layout(&ThreadListInput {
        project: Some("project-1"),
        ..input(&threads)
    });
    assert_eq!(ids(&layout), ["included"]);
}

#[test]
fn caps_the_settled_tail_at_the_limit_and_reports_the_hidden_count() {
    let mut threads = vec![thread("active", "Active")];
    for index in 0..4 {
        let hour = |minute: u32| format!("2026-06-01T0{index}:{minute:02}:00.000Z");
        threads.push(ThreadSummary {
            latest_user_message_at: Some(ms(&hour(0))),
            // A run adopted the message, so it is not a queued turn start.
            latest_run: Some(RunSummary {
                requested_at: Some(ms(&hour(0))),
                started_at: Some(ms(&hour(0))),
                completed_at: Some(ms(&hour(10))),
                ..run(&format!("run-{index}"), RuntimeStatus::Completed)
            }),
            ..settled_at(thread(&format!("settled-{index}"), "Settled"), &hour(10))
        });
    }
    let layout = build_thread_list_layout(&ThreadListInput {
        settled_limit: Some(2),
        ..input(&threads)
    });
    assert_eq!(layout.hidden_settled_count, 2);
    assert_eq!(
        layout
            .items
            .iter()
            .filter(|item| item.variant == RowVariant::Slim)
            .count(),
        2
    );
    assert_eq!(ids(&layout), ["active", "settled-3", "settled-2"]);
}

fn active_and_settled() -> Vec<ThreadSummary> {
    vec![
        thread("active", "active"),
        settled_at(thread("settled", "settled"), NOW),
    ]
}

#[test]
fn splices_queued_tasks_between_the_active_block_and_the_settled_tail() {
    let threads = active_and_settled();
    let layout = build_thread_list_layout(&input(&threads));
    let items = build_list_items(ListItemsInput {
        pending_tasks: vec![pending_task("queued-1"), pending_task("queued-2")],
        ..list_input(&layout, now())
    });
    let labels: Vec<String> = items.iter().map(label).collect();
    assert_eq!(
        labels,
        ["active", "queued-1", "queued-2", "settled-shelf", "settled"]
    );
    let dividers = items
        .iter()
        .filter(|item| {
            matches!(item, ThreadListItem::PendingTask { task } if task.show_pending_divider)
        })
        .count();
    assert_eq!(dividers, 1);
}

#[test]
fn ends_the_list_with_queued_tasks_when_nothing_has_settled_yet() {
    let threads = vec![thread("active", "active")];
    let layout = build_thread_list_layout(&input(&threads));
    let items = build_list_items(ListItemsInput {
        pending_tasks: vec![pending_task("queued-1")],
        ..list_input(&layout, now())
    });
    let kinds: Vec<&str> = items.iter().map(kind).collect();
    assert_eq!(kinds, ["thread", "pending"]);
}

#[test]
fn keeps_the_settled_shelf_between_active_and_settled_rows_when_nothing_is_queued() {
    let threads = active_and_settled();
    let layout = build_thread_list_layout(&input(&threads));
    let items = build_list_items(list_input(&layout, now()));
    let keys: Vec<&str> = items.iter().map(ThreadListItem::key).collect();
    assert_eq!(keys, ["thread:active", "settled-shelf", "thread:settled"]);
}

#[test]
fn places_queued_tasks_before_a_collapsed_snoozed_shelf() {
    let threads = vec![
        thread("active", "active"),
        snoozed(
            thread("snoozed", "snoozed"),
            "2026-06-03T09:00:00.000Z",
            "2026-06-01T12:00:00.000Z",
        ),
        settled_at(thread("settled", "settled"), NOW),
    ];
    let layout = build_thread_list_layout(&input(&threads));
    let items = build_list_items(ListItemsInput {
        pending_tasks: vec![pending_task("queued")],
        ..list_input(&layout, now())
    });
    let kinds: Vec<&str> = items.iter().map(kind).collect();
    assert_eq!(
        kinds,
        [
            "thread",
            "pending",
            "snoozed-shelf",
            "settled-shelf",
            "thread"
        ]
    );
}

struct MoveFixture {
    rows: Vec<ThreadSummary>,
    assignments: Vec<OrderAssignment>,
    pending: PendingThreadOrder,
    section: OrderSection,
}
impl MoveFixture {
    fn new(section: OrderSection) -> Self {
        let rows: Vec<ThreadSummary> = ["a", "b", "c"]
            .iter()
            .enumerate()
            .map(|(index, id)| {
                let at = ms(&format!("2026-06-01T0{}:00:00.000Z", 3 - index));
                ThreadSummary {
                    created_at: at,
                    pinned_at: (section == OrderSection::Pinned).then_some(at),
                    ..thread(id, if *id == "a" { "hidden" } else { "match" })
                }
            })
            .collect();
        let ordered = ordered_section(&rows, section, None, now(), &BTreeSet::new());
        let ordered_ids: Vec<String> = ordered.iter().map(|row| row.id.clone()).collect();
        let keys = ordered_ids.iter().map(|id| (id.clone(), None)).collect();
        let moved = ordered_ids[2].clone();
        let assignments = plan_pinned_move(&ordered_ids, &keys, &moved, MoveDirection::Up).unwrap();
        let pending = PendingThreadOrder::begin(
            section,
            &ordered,
            &moved,
            &MoveDestination::Up,
            &assignments,
        )
        .unwrap();
        Self {
            rows,
            assignments,
            pending,
            section,
        }
    }
    fn update(&self, rows: &[ThreadSummary], assignment: &OrderAssignment) -> Vec<ThreadSummary> {
        rows.iter()
            .map(|row| {
                let mut row = row.clone();
                if row.id == assignment.id {
                    let key = Some(assignment.order_key.clone());
                    match self.section {
                        OrderSection::Pinned => row.pin_order_key = key,
                        OrderSection::Active => row.active_order_key = key,
                    }
                }
                row
            })
            .collect()
    }
    fn section(&self, rows: &[ThreadSummary]) -> Vec<ThreadSummary> {
        ordered_section(rows, self.section, None, now(), &BTreeSet::new())
            .into_iter()
            .cloned()
            .collect()
    }
}
fn layout_with(
    rows: &[ThreadSummary],
    pending: Option<&PendingThreadOrder>,
    search: &str,
) -> Vec<String> {
    ids(&build_thread_list_layout(&ThreadListInput {
        pending_order: pending,
        search_query: search,
        ..input(rows)
    }))
}

#[test]
fn holds_the_order_through_every_intermediate_key_upsert() {
    for section in [OrderSection::Active, OrderSection::Pinned] {
        let fixture = MoveFixture::new(section);
        let desired = fixture.pending.ordered_ids.clone();
        let mut current = fixture.rows.clone();
        let mut hold = fixture.pending.clone();
        assert_eq!(layout_with(&current, Some(&hold), ""), desired);
        for assignment in &fixture.assignments {
            current = fixture.update(&current, assignment);
            hold = hold.reconcile(&fixture.section(&current)).unwrap();
            assert_eq!(layout_with(&current, Some(&hold), ""), desired);
        }
        hold.commands_complete = true;
        assert_eq!(hold.reconcile(&current), None);
        assert_eq!(layout_with(&current, None, ""), desired);
    }
}

#[test]
fn keeps_the_action_guard_pending_when_receipts_precede_canonical_shells() {
    let fixture = MoveFixture::new(OrderSection::Active);
    let mut complete = fixture.pending.clone();
    complete.commands_complete = true;
    let mut hold = Some(complete);
    let mut current = fixture.rows.clone();
    assert_eq!(hold.as_ref().unwrap().reconcile(&current), hold);
    for (index, assignment) in fixture.assignments.iter().enumerate() {
        current = fixture.update(&current, assignment);
        hold = hold.unwrap().reconcile(&current);
        assert_eq!(hold.is_none(), index == fixture.assignments.len() - 1);
        assert_eq!(layout_with(&current, hold.as_ref(), ""), ["a", "c", "b"]);
        if hold.is_none() {
            break;
        }
    }
}

#[test]
fn keeps_search_results_in_the_full_pending_section_order() {
    let fixture = MoveFixture::new(OrderSection::Active);
    let current = fixture.update(
        &fixture.update(&fixture.rows, &fixture.assignments[0]),
        &fixture.assignments[1],
    );
    assert_eq!(
        layout_with(&current, Some(&fixture.pending), "match"),
        ["c", "b"]
    );
}

#[test]
fn releases_for_real_section_membership_and_foreign_key_changes() {
    let fixture = MoveFixture::new(OrderSection::Active);
    let rows = &fixture.rows;
    assert_eq!(fixture.pending.reconcile(&rows[1..]), None);
    let mut grown = rows.clone();
    grown.push(thread("new", "new"));
    assert_eq!(fixture.pending.reconcile(&grown), None);
    let mut foreign = rows.clone();
    foreign[0].active_order_key = Some("zz".into());
    assert_eq!(fixture.pending.reconcile(&foreign), None);
    let mut settled = rows.clone();
    settled[0].settled_override = Some(SettledOverride::Settled);
    assert_eq!(
        layout_with(&settled, Some(&fixture.pending), ""),
        layout_with(&settled, None, "")
    );
}

#[test]
fn does_not_hide_a_concurrent_return_to_a_previously_confirmed_key() {
    let fixture = MoveFixture::new(OrderSection::Active);
    let confirmed = fixture
        .pending
        .reconcile(&fixture.update(&fixture.rows, &fixture.assignments[0]))
        .unwrap();
    assert_eq!(confirmed.reconcile(&fixture.rows), None);
}

#[test]
fn preserves_the_hold_for_activity_but_releases_for_a_reopened_sort_anchor() {
    let fixture = MoveFixture::new(OrderSection::Active);
    let active: Vec<ThreadSummary> = fixture
        .rows
        .iter()
        .map(|row| ThreadSummary {
            updated_at: now(),
            ..row.clone()
        })
        .collect();
    assert_eq!(
        fixture.pending.reconcile(&active),
        Some(fixture.pending.clone())
    );
    let mut reopened = fixture.rows.clone();
    reopened[0].unsettled_at = Some(now());
    assert_eq!(fixture.pending.reconcile(&reopened), None);
}

#[test]
fn excludes_subagents_from_navigation_search_and_ordering_while_retaining_user_forks() {
    let threads = vec![
        thread("root", "Root"),
        ThreadSummary {
            parent: Some("root".into()),
            subagent: true,
            ..thread("child", "Child")
        },
        ThreadSummary {
            parent: Some("root".into()),
            forked: true,
            ..thread("fork", "Fork")
        },
    ];
    assert_eq!(
        ids(&build_thread_list_layout(&input(&threads))),
        ["fork", "root"]
    );
    assert!(
        build_thread_list_layout(&ThreadListInput {
            search_query: "Child",
            ..input(&threads)
        })
        .items
        .is_empty()
    );
    assert_eq!(
        section_ids(&threads, OrderSection::Active, &BTreeSet::new()),
        ["fork", "root"]
    );
}

fn base() -> i64 {
    now()
}
fn tick_threads() -> [ThreadSummary; 4] {
    [
        ThreadSummary {
            latest_user_message_at: Some(base() - 5 * MINUTE_MS),
            ..thread("tick-ready", "tick ready")
        },
        ThreadSummary {
            has_pending_approvals: true,
            latest_user_message_at: Some(base() - 5 * MINUTE_MS),
            ..thread("tick-approval", "tick approval")
        },
        ThreadSummary {
            settled_override: Some(SettledOverride::Settled),
            settled_at: Some(base() - 3 * 24 * 60 * MINUTE_MS),
            ..thread("tick-settled", "tick settled")
        },
        ThreadSummary {
            snoozed_at: Some(base() - MINUTE_MS),
            snoozed_until: Some(base() + 2 * 60 * MINUTE_MS),
            ..thread("tick-snoozed", "tick snoozed")
        },
    ]
}

#[derive(Default)]
struct Tick<'a> {
    /// Leaves the snooze presets out, as on a list whose rows cannot snooze.
    no_presets: bool,
    queued: Option<&'a BTreeSet<String>>,
    availability: Option<&'a BTreeMap<String, MoveAvailability>>,
    loading: bool,
}

fn tick_list(
    threads: &[ThreadSummary],
    clock: i64,
    tasks: Vec<PendingTaskRow>,
    tick: Tick,
) -> Vec<ThreadListItem> {
    let layout = build_thread_list_layout(&ThreadListInput {
        now_ms: clock,
        snoozed_shelf_expanded: true,
        ..input(threads)
    });
    let presets = if tick.no_presets {
        vec![]
    } else {
        resolve_snooze_presets(
            &Utc.timestamp_millis_opt(clock).unwrap(),
            TimestampFormat::Locale,
        )
    };
    build_list_items(ListItemsInput {
        pending_tasks: tasks,
        snooze_presets: &presets,
        snoozed_shelf_expanded: true,
        queued: tick.queued,
        move_availability: tick.availability,
        shelf_preferences_loading: tick.loading,
        ..list_input(&layout, clock)
    })
}

#[test]
fn rebuilt_items_over_identical_rows_are_equal() {
    let threads = vec![ThreadSummary {
        latest_user_message_at: Some(base() - 5 * MINUTE_MS),
        ..thread("eq", "eq")
    }];
    let build = || {
        tick_list(
            &threads,
            base(),
            vec![pending_task("eq-queued")],
            Tick::default(),
        )
    };
    assert_eq!(build(), build());
    let renamed = vec![thread("eq", "renamed")];
    let next = tick_list(&renamed, base(), vec![], Tick::default());
    assert_ne!(row(&build(), "eq"), row(&next, "eq"));
}

#[test]
fn notices_a_changed_wake_countdown_label() {
    let threads = vec![ThreadSummary {
        snoozed_at: Some(base()),
        snoozed_until: Some(base() + 61 * MINUTE_MS),
        ..thread("wake", "wake")
    }];
    let layout = build_thread_list_layout(&ThreadListInput {
        snoozed_shelf_expanded: true,
        ..input(&threads)
    });
    let at = |clock| {
        build_list_items(ListItemsInput {
            snoozed_shelf_expanded: true,
            ..list_input(&layout, clock)
        })
    };
    let (earlier, later) = (at(base()), at(base() + MINUTE_MS));
    assert_eq!(
        row(&earlier, "wake").snooze_wake_label.as_deref(),
        Some("2h")
    );
    assert_eq!(row(&later, "wake").snooze_wake_label.as_deref(), Some("1h"));
    assert_ne!(row(&earlier, "wake"), row(&later, "wake"));
}

#[test]
fn shelf_headers_differ_by_count_expansion_and_loading() {
    let shelf = ShelfHeader {
        count: 2,
        expanded: true,
        disabled: false,
    };
    assert_ne!(shelf, ShelfHeader { count: 3, ..shelf });
    assert_ne!(
        shelf,
        ShelfHeader {
            expanded: false,
            ..shelf
        }
    );
    assert_ne!(
        shelf,
        ShelfHeader {
            disabled: true,
            ..shelf
        }
    );
}

#[test]
fn flips_a_trailing_divider_when_a_neighbour_changes() {
    let bare_threads = vec![thread("flip-a", "flip a"), thread("flip-b", "flip b")];
    let bare = tick_list(&bare_threads, base(), vec![], Tick::default());
    let settled_threads = vec![
        thread("flip-a", "flip a"),
        settled_at(thread("flip-b", "flip b"), NOW),
    ];
    let with_settled = tick_list(&settled_threads, base(), vec![], Tick::default());
    assert!(row(&bare, "flip-a").show_trailing_divider);
    assert!(!row(&with_settled, "flip-a").show_trailing_divider);
}

#[test]
fn carries_snooze_choices_on_every_row_whose_swipe_offers_them() {
    let items = tick_list(
        &tick_threads(),
        base(),
        vec![pending_task("tick-queued")],
        Tick::default(),
    );
    assert!(!row(&items, "tick-ready").snooze_options.is_empty());
    // The swipe on a settled slim row offers snooze too.
    assert!(!row(&items, "tick-settled").snooze_options.is_empty());
    assert!(row(&items, "tick-approval").snooze_options.is_empty());
    assert!(row(&items, "tick-snoozed").snooze_options.is_empty());
}

#[test]
fn blanks_the_time_for_rows_that_render_a_label_instead() {
    let [ready, _, settled, snoozed] = tick_threads();
    let working = ThreadSummary {
        latest_user_message_at: Some(base() - 5 * MINUTE_MS),
        runtime: running(),
        ..thread("tick-working", "tick working")
    };
    let items = tick_list(
        &[ready, working, settled, snoozed],
        base(),
        vec![pending_task("tick-queued")],
        Tick::default(),
    );
    assert_eq!(row(&items, "tick-ready").time_label, "5m");
    assert_eq!(row(&items, "tick-working").time_label, "");
    assert_eq!(row(&items, "tick-settled").time_label, "3d");
    assert_eq!(row(&items, "tick-snoozed").time_label, "");
    assert_eq!(
        row(&items, "tick-snoozed").snooze_wake_label.as_deref(),
        Some("2h")
    );
}

#[test]
fn a_minute_tick_changes_only_rows_whose_clock_driven_content_moved() {
    let threads = tick_threads();
    let tasks = || vec![pending_task("tick-queued")];
    let at_start = tick_list(&threads, base(), tasks(), Tick::default());
    let at_next = tick_list(&threads, base() + MINUTE_MS, tasks(), Tick::default());
    assert_eq!(at_start.len(), at_next.len());
    let changed: Vec<&str> = at_start
        .iter()
        .zip(&at_next)
        .filter(|(start, next)| start != next)
        .map(|(start, _)| start.key())
        .collect();
    assert_eq!(changed, ["thread:tick-ready", "thread:tick-settled"]);
}

#[test]
fn keeps_unread_completion_labels_stable_across_a_minute_tick() {
    let unread = vec![ThreadSummary {
        last_visited_at: Some(base() - 10 * MINUTE_MS),
        latest_run: Some(RunSummary {
            requested_at: Some(base() - 6 * MINUTE_MS),
            started_at: Some(base() - 6 * MINUTE_MS),
            completed_at: Some(base() - 5 * MINUTE_MS),
            ..run("tick-unread-run", RuntimeStatus::Completed)
        }),
        ..thread("tick-unread", "Unread completion")
    }];
    let tick = || Tick {
        no_presets: true,
        ..Tick::default()
    };
    let first = tick_list(&unread, base(), vec![], tick());
    let next = tick_list(&unread, base() + MINUTE_MS, vec![], tick());
    assert_eq!(row(&first, "tick-unread").time_label, "");
    assert_eq!(
        row(&first, "tick-unread").status_label.as_deref(),
        Some("Done")
    );
    assert_eq!(first, next);
}

#[test]
fn keeps_hour_granularity_rows_stable_across_a_minute_tick_without_snooze_choices() {
    let stale = vec![ThreadSummary {
        latest_user_message_at: Some(base() - 3 * 60 * MINUTE_MS),
        ..thread("tick-stale", "tick stale")
    }];
    let tick = || Tick {
        no_presets: true,
        ..Tick::default()
    };
    assert_eq!(
        tick_list(&stale, base(), vec![], tick()),
        tick_list(&stale, base() + MINUTE_MS, vec![], tick())
    );
}

#[test]
fn changes_the_snoozed_countdown_row_when_the_wake_label_advances() {
    let snoozed = vec![ThreadSummary {
        snoozed_at: Some(base() - MINUTE_MS),
        snoozed_until: Some(base() + 120 * MINUTE_MS),
        ..thread("tick-wake", "tick wake")
    }];
    let at = |clock| tick_list(&snoozed, clock, vec![], Tick::default());
    let (start, still_two_hours, one_hour) = (
        at(base()),
        at(base() + MINUTE_MS),
        at(base() + 61 * MINUTE_MS),
    );
    assert_eq!(row(&start, "tick-wake"), row(&still_two_hours, "tick-wake"));
    assert_eq!(
        row(&one_hour, "tick-wake").snooze_wake_label.as_deref(),
        Some("59m")
    );
    assert_ne!(
        row(&still_two_hours, "tick-wake"),
        row(&one_hour, "tick-wake")
    );
}

#[test]
fn trailing_dividers_follow_the_final_neighbour_order() {
    let threads = vec![thread("div-a", "a"), thread("div-b", "b")];
    let items = tick_list(
        &threads,
        base(),
        vec![pending_task("div-q1"), pending_task("div-q2")],
        Tick::default(),
    );
    let dividers: Vec<bool> = items
        .iter()
        .map(|item| match item {
            ThreadListItem::Thread { row } => row.show_trailing_divider,
            ThreadListItem::PendingTask { task } => task.show_trailing_divider,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(dividers, [true, false, true, false]);
}

fn stamp_threads() -> Vec<ThreadSummary> {
    vec![
        ThreadSummary {
            latest_user_message_at: Some(base() - 5 * MINUTE_MS),
            ..thread("stamp-ready", "stamp ready")
        },
        ThreadSummary {
            settled_override: Some(SettledOverride::Settled),
            settled_at: Some(base() - 3 * 24 * 60 * MINUTE_MS),
            ..thread("stamp-settled", "stamp settled")
        },
    ]
}

#[test]
fn stamps_queued_outbox_messages_onto_the_matching_row() {
    let threads = stamp_threads();
    let queued = set(&["stamp-ready"]);
    let none = BTreeSet::new();
    let with = tick_list(
        &threads,
        base(),
        vec![],
        Tick {
            queued: Some(&queued),
            ..Tick::default()
        },
    );
    let without = tick_list(
        &threads,
        base(),
        vec![],
        Tick {
            queued: Some(&none),
            ..Tick::default()
        },
    );
    assert!(row(&with, "stamp-ready").has_queued_messages);
    assert!(!row(&without, "stamp-ready").has_queued_messages);
    assert_eq!(row(&with, "stamp-settled"), row(&without, "stamp-settled"));
}

#[test]
fn names_each_swipe_action_and_what_a_swipe_offers_for_assistive_technologies() {
    let items = tick_list(&tick_threads(), base(), vec![], Tick::default());
    let ready = row(&items, "tick-ready");
    assert_eq!(ready.swipe_primary.accessibility_label, "Settle tick ready");
    assert_eq!(
        ready
            .swipe_secondary
            .as_ref()
            .map(|button| button.accessibility_label.as_str()),
        Some("Choose when to snooze tick ready")
    );
    assert_eq!(
        ready.swipe_hint,
        "Opens the thread. Swipe left for settle and snooze actions."
    );
    let settled = row(&items, "tick-settled");
    assert_eq!(
        settled.swipe_primary.accessibility_label,
        "Un-settle tick settled"
    );
    let snoozed = row(&items, "tick-snoozed");
    assert_eq!(
        snoozed.swipe_primary.accessibility_label,
        "Wake tick snoozed now"
    );
    assert_eq!(snoozed.swipe_hint, "Opens the thread. Swipe left to wake.");
}

#[test]
fn shows_the_last_error_of_failed_and_limited_threads_only() {
    let failed = |id: &str, class: Option<&str>, status: RuntimeStatus| ThreadSummary {
        runtime: Some(RuntimeSummary {
            last_error: Some(format!("{id} error")),
            last_error_class: class.map(String::from),
            ..runtime(status)
        }),
        ..thread(id, id)
    };
    let threads = vec![
        failed("failed", None, RuntimeStatus::Failed),
        failed("limited", Some("usage_limit"), RuntimeStatus::Failed),
        failed("recovered", None, RuntimeStatus::Completed),
    ];
    let items = tick_list(&threads, base(), vec![], Tick::default());
    assert_eq!(
        row(&items, "failed").error_text.as_deref(),
        Some("failed error")
    );
    assert_eq!(
        row(&items, "limited").error_text.as_deref(),
        Some("limited error")
    );
    assert_eq!(row(&items, "recovered").error_text, None);
}

#[test]
fn stamps_move_availability_on_card_rows_only() {
    let threads = stamp_threads();
    let open_moves = MoveAvailability {
        can_move_up: true,
        can_move_down: true,
    };
    let permissive = BTreeMap::from([
        ("stamp-ready".to_string(), open_moves),
        ("stamp-settled".to_string(), open_moves),
    ]);
    let blocked = BTreeMap::from([("stamp-settled".to_string(), open_moves)]);
    let open = tick_list(
        &threads,
        base(),
        vec![],
        Tick {
            availability: Some(&permissive),
            ..Tick::default()
        },
    );
    let closed = tick_list(
        &threads,
        base(),
        vec![],
        Tick {
            availability: Some(&blocked),
            ..Tick::default()
        },
    );
    assert!(row(&open, "stamp-ready").can_move_up);
    assert!(!row(&closed, "stamp-ready").can_move_up);
    assert!(!row(&open, "stamp-settled").can_move_up);
    assert_eq!(row(&open, "stamp-settled"), row(&closed, "stamp-settled"));
}

#[test]
fn keeps_the_settled_rows_swipe_snooze_choices_fresh_across_a_tick() {
    let threads = vec![stamp_threads().remove(1)];
    let start = tick_list(&threads, base(), vec![], Tick::default());
    let next = tick_list(&threads, base() + MINUTE_MS, vec![], Tick::default());
    let (start, next) = (row(&start, "stamp-settled"), row(&next, "stamp-settled"));
    assert_eq!(start.variant, RowVariant::Slim);
    assert_eq!(
        start.swipe_secondary.as_ref().map(|button| button.action),
        Some(SwipeAction::Snooze)
    );
    assert_ne!(start.snooze_options, next.snooze_options);
}

#[test]
fn stamps_the_shelf_loading_state_onto_headers() {
    let threads = vec![stamp_threads().remove(1)];
    let shelf = |loading| {
        tick_list(
            &threads,
            base(),
            vec![],
            Tick {
                loading,
                ..Tick::default()
            },
        )
        .into_iter()
        .find_map(|item| match item {
            ThreadListItem::SettledShelf { shelf } => Some(shelf),
            _ => None,
        })
        .unwrap()
    };
    assert!(shelf(true).disabled);
    assert!(!shelf(false).disabled);
}

fn working_threads() -> Vec<ThreadSummary> {
    let finished = |id: &str, completed: &str| ThreadSummary {
        created_at: ms("2026-06-01T00:00:00.000Z"),
        latest_run: Some(RunSummary {
            requested_at: Some(ms("2026-06-01T00:00:00.000Z")),
            started_at: Some(ms("2026-06-01T00:00:00.000Z")),
            completed_at: Some(ms(completed)),
            ..run(&format!("run-{id}"), RuntimeStatus::Completed)
        }),
        ..thread(id, id)
    };
    vec![
        finished("finished-early", "2026-06-01T01:00:00.000Z"),
        finished("finished-late", "2026-06-01T03:00:00.000Z"),
        ThreadSummary {
            runtime: running(),
            ..thread("working", "working")
        },
        ThreadSummary {
            created_at: ms("2026-06-01T02:00:00.000Z"),
            runtime: running(),
            has_pending_approvals: true,
            ..thread("asks-approval", "asks-approval")
        },
        ThreadSummary {
            runtime: running(),
            pinned_at: Some(ms("2026-06-01T00:00:00.000Z")),
            ..thread("pinned-working", "pinned-working")
        },
    ]
}
fn beta(threads: &[ThreadSummary]) -> ThreadListInput<'_> {
    ThreadListInput {
        working_shelf_enabled: true,
        ..input(threads)
    }
}

#[test]
fn folds_unpinned_working_threads_into_a_collapsed_shelf() {
    let threads = working_threads();
    let layout = build_thread_list_layout(&beta(&threads));
    assert_eq!(
        ids(&layout),
        [
            "pinned-working",
            "finished-late",
            "asks-approval",
            "finished-early"
        ]
    );
    assert_eq!(layout.working_count, 1);
    assert_eq!(layout.working_shelf_header_index, Some(4));
    let off = build_thread_list_layout(&input(&threads));
    assert!(ids(&off).contains(&"working".to_string()));
    assert_eq!(off.working_count, 0);
}

#[test]
fn shows_working_rows_as_cards_when_expanded_or_only_the_selected_one_when_collapsed() {
    let threads = working_threads();
    let expanded = build_thread_list_layout(&ThreadListInput {
        working_shelf_expanded: true,
        ..beta(&threads)
    });
    let last = expanded.items.last().unwrap();
    assert_eq!(last.thread.id, "working");
    assert_eq!(last.variant, RowVariant::Card);
    let selected = build_thread_list_layout(&ThreadListInput {
        selected: Some("working"),
        ..beta(&threads)
    });
    assert_eq!(ids(&selected).last().unwrap(), "working");
}

#[test]
fn orders_the_inbox_by_the_latest_return_this_device_observed() {
    let threads = working_threads();
    let mut returns = InboxReturns::default();
    let still_working = ThreadSummary {
        runtime: running(),
        ..threads[0].clone()
    };
    returns.observe(Some(&[still_working]), ms("2026-06-01T00:30:00.000Z"));
    returns.observe(Some(&threads[..1]), ms("2026-06-01T04:00:00.000Z"));
    let layout = build_thread_list_layout(&ThreadListInput {
        inbox_returns: Some(&returns),
        ..beta(&threads)
    });
    assert_eq!(
        ids(&layout)[1..],
        ["finished-early", "finished-late", "asks-approval"]
    );
}

#[test]
fn places_the_working_shelf_after_queued_tasks_and_before_snoozed_and_settled_threads() {
    let threads = vec![
        thread("active", "active"),
        ThreadSummary {
            runtime: running(),
            ..thread("working", "working")
        },
        ThreadSummary {
            runtime: running(),
            ..snoozed(
                thread("snoozed", "snoozed"),
                "2026-06-03T09:00:00.000Z",
                "2026-06-01T12:00:00.000Z",
            )
        },
        settled_at(thread("settled", "settled"), NOW),
    ];
    let layout = build_thread_list_layout(&ThreadListInput {
        working_shelf_expanded: true,
        snoozed_shelf_expanded: true,
        ..beta(&threads)
    });
    let items = build_list_items(ListItemsInput {
        pending_tasks: vec![pending_task("queued")],
        working_shelf_expanded: true,
        snoozed_shelf_expanded: true,
        ..list_input(&layout, now())
    });
    let labels: Vec<String> = items.iter().map(label).collect();
    assert_eq!(
        labels,
        [
            "active",
            "queued",
            "working-shelf",
            "working",
            "snoozed-shelf",
            "snoozed",
            "settled-shelf",
            "settled"
        ]
    );
}

fn actions(menu: &[ThreadMenuItem]) -> Vec<ThreadMenuItemId> {
    menu.iter().map(|item| item.id).collect()
}

#[test]
fn builds_each_rows_long_press_menu() {
    use ThreadMenuItemId::*;
    fn context(
        variant: RowVariant,
        snoozed: bool,
        snooze_options: &[ThreadMenuChild],
    ) -> RowMenuContext<'_> {
        RowMenuContext {
            variant,
            snoozed,
            reorderable: true,
            can_move_up: false,
            can_move_down: true,
            snooze_options,
        }
    }
    let options = snooze_menu_options(&[]);
    let card = thread_row_menu(
        &ThreadSummary {
            branch: Some("feature".into()),
            ..thread("t", "t")
        },
        &context(RowVariant::Card, false, &options),
    );
    assert_eq!(
        actions(&card),
        [
            NewThreadOnBranch,
            CopyThreadId,
            Settle,
            Snooze,
            Arrange,
            MoveUp,
            MoveDown,
            Pin,
            Rename,
            RegenerateTitle,
            AutoSettle,
            Delete
        ]
    );
    assert_eq!(card[0].label, "New thread on branch");
    assert!(!card[5].enabled && card[6].enabled);
    assert!(card.last().unwrap().destructive);
    let unsnoozable = thread_row_menu(&thread("t", "t"), &context(RowVariant::Card, false, &[]));
    assert!(!actions(&unsnoozable).contains(&Snooze));
    let pinned = ThreadSummary {
        pinned_at: Some(now()),
        ..thread("t", "t")
    };
    assert_eq!(
        actions(&thread_row_menu(
            &pinned,
            &context(RowVariant::Slim, false, &options)
        )),
        [
            CopyThreadId,
            Unsettle,
            Arrange,
            Unpin,
            Rename,
            RegenerateTitle,
            AutoSettle,
            Delete
        ]
    );
    assert_eq!(
        actions(&thread_row_menu(
            &pinned,
            &context(RowVariant::Slim, true, &[])
        )),
        [
            CopyThreadId,
            Unsnooze,
            Rename,
            RegenerateTitle,
            AutoSettle,
            Delete
        ]
    );
}

#[test]
fn checks_the_current_auto_settle_choice() {
    let menu = thread_row_menu(
        &ThreadSummary {
            auto_settle_disabled: true,
            ..thread("t", "t")
        },
        &RowMenuContext {
            variant: RowVariant::Card,
            snoozed: false,
            reorderable: false,
            can_move_up: false,
            can_move_down: false,
            snooze_options: &[],
        },
    );
    let auto_settle = menu
        .iter()
        .find(|item| item.id == ThreadMenuItemId::AutoSettle)
        .unwrap();
    let checked: Vec<(&str, Option<bool>)> = auto_settle
        .children
        .iter()
        .map(|option| (option.label.as_str(), option.checked))
        .collect();
    assert_eq!(
        checked,
        [("Enabled", Some(false)), ("Disabled", Some(true))]
    );
}

#[test]
fn offers_title_regeneration() {
    assert_eq!(
        title_regeneration_menu_item(false),
        ThreadMenuItem {
            id: ThreadMenuItemId::RegenerateTitle,
            label: "Regenerate title".into(),
            icon: None,
            enabled: true,
            destructive: false,
            separator_before: false,
            action: Some(ThreadMenuAction::Thread {
                action: crate::state::ThreadAction::RegenerateTitle
            }),
            confirmation: None,
            children: vec![],
        }
    );
}

#[test]
fn shows_and_disables_the_pending_regeneration() {
    let item = title_regeneration_menu_item(true);
    assert_eq!(item.label, "Regenerating…");
    assert!(!item.enabled);
}

#[test]
fn resolves_provider_drivers_only_when_the_current_instance_is_known() {
    let thread = ThreadSummary {
        provider_instance_history: vec!["claude".into(), "gone".into()],
        ..thread("t", "t")
    };
    let providers = BTreeMap::from([
        ("codex".to_string(), Driver::Codex),
        ("claude".to_string(), Driver::Claude),
    ]);
    assert_eq!(
        provider_drivers(&thread, &providers),
        [Driver::Claude, Driver::Codex]
    );
    let unknown = BTreeMap::from([("claude".to_string(), Driver::Claude)]);
    assert!(provider_drivers(&thread, &unknown).is_empty());
}

mod snapshot {
    use super::*;
    use crate::commands::build::{
        StartTurn, TurnDispatch, TurnMessage, dispatch as dispatch_command, send_command,
    };
    use crate::commands::outbox::{Outbox, PendingCommand};
    use crate::sync::ShellCache;
    use agent_domain::{CommandId, MessageId, ThreadId, ThreadShell};
    use agent_protocol::conversation::{Launch, SearchMatch, SearchSource, ShellSnapshot};
    use std::sync::Arc;

    fn shell_row(id: &str, title: &str) -> ThreadShell {
        let mut row = agent_domain::shell(&crate::sync::fixtures::thread_state(title)).unwrap();
        row.id = ThreadId::new(id).unwrap();
        row
    }
    fn send(thread: &str) -> PendingCommand {
        let command = send_command(StartTurn {
            message: TurnMessage {
                id: MessageId::new(format!("message-{thread}")).unwrap(),
                text: "hello".into(),
                attachments: vec![],
                context: None,
            },
            selection: None,
            title_seed: None,
            source_plan: None,
            dispatch: TurnDispatch::Start,
            continuation: None,
            creation_source: "mobile".into(),
        });
        let id = ThreadId::new(thread).unwrap();
        let command_id = CommandId::new(format!("send-{thread}")).unwrap();
        PendingCommand::new(
            id.clone(),
            Request::Dispatch(Box::new(dispatch_command(id, command_id, command))),
            Timestamp::parse(NOW).unwrap(),
        )
    }
    fn launch(thread: &str, project: &str, title: &str) -> PendingCommand {
        let id = ThreadId::new(thread).unwrap();
        PendingCommand::new(
            id.clone(),
            Request::Launch(Box::new(Launch {
                command_id: CommandId::new(format!("launch-{thread}")).unwrap(),
                thread_id: Some(id),
                project_id: project.into(),
                title: title.into(),
                selection: crate::sync::fixtures::selection("codex"),
                runtime_mode: agent_domain::RuntimeMode::FullAccess,
                interaction_mode: agent_domain::InteractionMode::Default,
                workspace: WorkspaceStrategy::Root {
                    branch: Some("feature".into()),
                },
                message: None,
            })),
            Timestamp::parse(NOW).unwrap(),
        )
    }
    fn snapshot() -> Snapshot {
        let mut settled = shell_row("settled-queued", "Settled with outbox");
        settled.settled = Some(true);
        let mut outbox = Outbox::default();
        for entry in [
            send("settled-queued"),
            launch("new-thread", "project", "Fix login"),
            launch("listed", "project", "Already listed"),
            launch("elsewhere", "other", "Fix login elsewhere"),
        ] {
            outbox.enqueue(entry).unwrap();
        }
        Snapshot {
            shell: Arc::new(ShellCache::from_cache(ShellSnapshot {
                snapshot_sequence: 1,
                projects: vec![],
                threads: vec![
                    shell_row("listed", "Login page"),
                    settled,
                    shell_row("other", "Greeting"),
                ],
            })),
            outbox: Arc::new(outbox),
            selected_thread: Some(ThreadId::new("listed").unwrap()),
            ..Snapshot::default()
        }
    }

    #[test]
    fn lists_the_shell_with_unsent_messages_and_new_threads_from_the_outbox() {
        let view = thread_list_at(
            &snapshot(),
            &Utc.timestamp_millis_opt(now()).unwrap(),
            &ThreadListOptions::default(),
            ThreadListHolds::default(),
        );
        let labels: Vec<String> = view.items.iter().map(label).collect();
        assert_eq!(
            labels,
            [
                "listed",
                "other",
                "settled-queued",
                "Fix login elsewhere",
                "Fix login"
            ]
        );
        assert!(row(&view.items, "settled-queued").has_queued_messages);
        assert!(row(&view.items, "listed").selected);
        let ThreadListItem::PendingTask { task } = &view.items[3] else {
            panic!("expected a pending task");
        };
        assert_eq!(task.branch.as_deref(), Some("feature"));
        assert!(view.has_threads);
    }

    #[test]
    fn filters_rows_and_new_threads_by_project_and_search() {
        let mut snapshot = snapshot();
        snapshot.selected_project = Some("project".into());
        snapshot.search = "login".into();
        snapshot.search_matches = vec![SearchMatch {
            thread_id: ThreadId::new("other").unwrap(),
            project_id: "project".into(),
            source: SearchSource::User,
            snippet: "say login".into(),
            message_created_at: None,
        }];
        let view = thread_list_at(
            &snapshot,
            &Utc.timestamp_millis_opt(now()).unwrap(),
            &ThreadListOptions::default(),
            ThreadListHolds::default(),
        );
        let labels: Vec<String> = view.items.iter().map(label).collect();
        assert_eq!(labels, ["listed", "other", "Fix login"]);
        assert_eq!(
            row(&view.items, "other").search_snippet.as_deref(),
            Some("say login")
        );
    }

    fn new_task_draft(
        project_id: &str,
        text: &str,
        created_at_ms: i64,
    ) -> crate::state::Draft {
        crate::state::Draft {
            text: text.into(),
            project_id: Some(project_id.into()),
            created_at_ms: Some(created_at_ms),
            ..Default::default()
        }
    }

    fn tasks(snapshot: &Snapshot) -> Vec<(bool, String, Option<String>)> {
        pending_tasks(snapshot, &BTreeSet::new())
            .into_iter()
            .map(|task| {
                (
                    matches!(task.kind, PendingTaskKind::Draft { .. }),
                    task.title,
                    task.branch,
                )
            })
            .collect()
    }

    // mobile pending-new-tasks-model.test.ts "surfaces every new-task draft
    // with content alongside queued creations".
    #[test]
    fn surfaces_every_new_task_draft_with_content_alongside_queued_creations() {
        let mut outbox = Outbox::default();
        outbox.enqueue(launch("a", "project", "queued a")).unwrap();
        let mut old = new_task_draft("project-old", "first idea", now() - 3_600_000);
        old.workspace = Some(crate::state::DraftWorkspace {
            mode: crate::view::projects::selection::ThreadWorkspaceMode::Worktree,
            branch: Some("main".into()),
            worktree_path: None,
            start_from_origin: false,
            start_from_origin_choice: None,
        });
        let snapshot = Snapshot {
            outbox: Arc::new(outbox),
            drafts: std::collections::BTreeMap::from([
                ("new:project-old".to_string(), old),
                (
                    "new:project-new".to_string(),
                    new_task_draft("project-new", "second idea", now() + 3_600_000),
                ),
            ])
            .into(),
            ..Snapshot::default()
        };
        assert_eq!(
            tasks(&snapshot),
            [
                (true, "second idea".to_string(), None),
                (true, "first idea".to_string(), Some("main".to_string())),
                (false, "queued a".to_string(), Some("feature".to_string())),
            ]
        );
        let task = &pending_tasks(&snapshot, &BTreeSet::new())[1];
        assert_eq!(
            (task.key.as_str(), task.project_id.as_str(), &task.kind),
            (
                "draft-task:new:project-old",
                "project-old",
                &PendingTaskKind::Draft {
                    draft_key: "new:project-old".into()
                }
            )
        );
        assert_eq!(task.created_at_ms, now() - 3_600_000);
    }

    // mobile pending-new-tasks-model.test.ts "hides settings-only drafts,
    // unstamped drafts, and drafts for other surfaces" and "titles an
    // attachment-only draft by its attachment count".
    #[test]
    fn hides_settings_only_drafts_and_titles_attachment_only_ones_by_count() {
        let settings_only = crate::state::Draft {
            model: "gpt".into(),
            ..new_task_draft("settings", "", now())
        };
        let mut with_image = new_task_draft("with-image", "", now());
        with_image.attachments.push(crate::state::DraftAttachment {
            id: "image-1".into(),
            remote_id: None,
            name: "image-1.png".into(),
            mime_type: "image/png".into(),
            kind: "image".into(),
            size_bytes: 1,
            local_path: "/tmp/image-1.png".into(),
            status: "ready".into(),
            error: None,
        });
        let mut snapshot = Snapshot {
            drafts: std::collections::BTreeMap::from([
                ("new:settings-only".to_string(), settings_only),
                (
                    "new:blank".to_string(),
                    new_task_draft("blank", "   ", now()),
                ),
                (
                    "thread-1".to_string(),
                    new_task_draft("thread", "thread composer text", now()),
                ),
            ])
            .into(),
            ..Snapshot::default()
        };
        assert!(tasks(&snapshot).is_empty());
        snapshot.drafts.insert("new:with-image".into(), with_image);
        assert_eq!(tasks(&snapshot), [(true, "1 attachment".to_string(), None)]);
    }

    // mobile projectThreadStartTurn.test.ts "keeps ordinary titles and the
    // empty-prompt fallback".
    #[test]
    fn keeps_ordinary_titles_and_the_empty_prompt_fallback() {
        assert_eq!(
            derive_thread_title_from_prompt("  Fix\n the parser  "),
            "Fix the parser"
        );
        assert_eq!(derive_thread_title_from_prompt(" \n "), "New thread");
        assert_eq!(
            derive_thread_title_from_prompt(&"a".repeat(80)),
            format!("{}...", "a".repeat(69))
        );
    }

    #[test]
    fn a_new_thread_draft_is_stamped_when_it_gains_work_and_forgets_it_when_emptied() {
        let mut snapshot = Snapshot::default();
        snapshot
            .drafts
            .insert("new:app".into(), new_task_draft("app", "", 0));
        snapshot.drafts.get_mut("new:app").unwrap().created_at_ms = None;
        snapshot.settle_new_thread_drafts(5);
        assert_eq!(snapshot.drafts["new:app"].created_at_ms, None);
        snapshot.drafts.get_mut("new:app").unwrap().text = "idea".into();
        snapshot.settle_new_thread_drafts(7);
        snapshot.settle_new_thread_drafts(9);
        assert_eq!(snapshot.drafts["new:app"].created_at_ms, Some(7));
        snapshot.drafts.get_mut("new:app").unwrap().text.clear();
        snapshot.settle_new_thread_drafts(11);
        assert_eq!(snapshot.drafts["new:app"].created_at_ms, None);
    }

    #[test]
    fn offers_moves_by_the_section_order() {
        let view = thread_list_at(
            &snapshot(),
            &Utc.timestamp_millis_opt(now()).unwrap(),
            &ThreadListOptions::default(),
            ThreadListHolds::default(),
        );
        let first = row(&view.items, "listed");
        assert!(!first.can_move_up && first.can_move_down);
    }
}

#[test]
fn an_empty_list_names_the_connection_search_or_project_that_left_it_empty() {
    let mut snapshot = Snapshot::default();
    assert_eq!(
        thread_list_empty(&snapshot, false).title,
        "Connecting to environment"
    );
    assert!(thread_list_empty(&snapshot, false).loading);
    snapshot.error = Some("The Host is offline.".into());
    assert_eq!(
        thread_list_empty(&snapshot, false),
        ThreadListEmpty {
            title: "Environment unavailable".into(),
            detail: "The Host is offline.".into(),
            loading: false,
        }
    );
    snapshot.error = None;
    snapshot.shell = std::sync::Arc::new(crate::sync::ShellCache::from_cache(
        agent_protocol::conversation::ShellSnapshot {
            snapshot_sequence: 1,
            projects: vec![],
            threads: vec![],
        },
    ));
    assert_eq!(
        thread_list_empty(&snapshot, false).title,
        "No projects found"
    );
    assert_eq!(thread_list_empty(&snapshot, true).title, "No threads yet");
    snapshot.selected_project = Some("app".into());
    assert_eq!(
        thread_list_empty(&snapshot, true).title,
        "No threads in app"
    );
    snapshot.search = " fix ".into();
    assert_eq!(
        thread_list_empty(&snapshot, true).detail,
        "No threads matching \"fix\"."
    );
}

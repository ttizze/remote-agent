use super::*;
use crate::sync::fixtures::{at, command_item, run, thread_state};
use agent_domain::{
    Attachment, AttachmentKind, InputIntent, Item, Message, MessageAuthor, MessageId, Notification,
    NotificationOutcome, NotificationSource, Request, RequestBody, ResponseCapability, Role,
    RunAttemptId, RunId, RuntimeRequestId, ThreadId, Timestamp,
};
use rstest::rstest;
use std::collections::BTreeMap;

fn status(
    phase: ConnectionStatus,
    connection_error: Option<&str>,
    environment_label: Option<&str>,
) -> Option<FloatingWorkingStatus> {
    connection_floating_status(phase, connection_error, environment_label)
}

fn connection(tone: ConnectionTone, label: &str) -> Option<FloatingWorkingStatus> {
    Some(FloatingWorkingStatus::Connection {
        tone,
        label: label.into(),
    })
}

#[test]
fn yields_the_pill_to_sync_and_working_state_once_connected() {
    assert_eq!(
        status(ConnectionStatus::Connected, None, Some("Mac mini")),
        None
    );
}

#[test]
fn names_the_environment_it_is_retrying_and_says_so_only_after_a_failure() {
    assert_eq!(
        status(ConnectionStatus::Connecting, None, Some("Mac mini")),
        connection(ConnectionTone::Reconnecting, "Reconnecting to Mac mini...")
    );
    assert_eq!(
        status(
            ConnectionStatus::Reconnecting,
            Some("ECONNREFUSED"),
            Some("Mac mini")
        ),
        connection(
            ConnectionTone::Reconnecting,
            "Failed to connect. Retrying Mac mini..."
        )
    );
}

#[test]
fn reports_why_the_environment_is_unreachable() {
    assert_eq!(
        status(ConnectionStatus::Offline, None, Some("Mac mini")),
        connection(ConnectionTone::Unavailable, "You are offline")
    );
    assert_eq!(
        status(ConnectionStatus::Available, None, Some("Mac mini")),
        connection(ConnectionTone::Unavailable, "Mac mini is not connected")
    );
    assert_eq!(
        status(
            ConnectionStatus::Error,
            Some("handshake timed out"),
            Some("Mac mini")
        ),
        connection(
            ConnectionTone::Unavailable,
            "Failed to connect to Mac mini: handshake timed out"
        )
    );
    assert_eq!(
        status(ConnectionStatus::Error, None, Some("Mac mini")),
        connection(ConnectionTone::Unavailable, "Failed to connect to Mac mini")
    );
}

#[test]
fn falls_back_to_a_generic_name_when_the_environment_has_no_label() {
    assert_eq!(
        status(ConnectionStatus::Error, None, None),
        connection(
            ConnectionTone::Unavailable,
            "Failed to connect to Environment"
        )
    );
}

/// The pill's reconnect handler becomes the interactive connection variant.
#[test]
fn carries_the_reconnect_handler_so_the_pill_can_trigger_it() {
    let pill = status(ConnectionStatus::Offline, None, Some("Mac mini"));
    assert!(matches!(
        pill,
        Some(FloatingWorkingStatus::Connection { .. })
    ));
    let view = working_control(pill, None, 0, false, 0).unwrap();
    assert!(view.status_interactive);
}

fn task(
    task_id: &str,
    kind: BackgroundTaskKind,
    description: Option<&str>,
    child_thread: Option<&str>,
) -> PendingBackgroundTask {
    PendingBackgroundTask {
        task_id: task_id.into(),
        kind,
        description: description.map(Into::into),
        child_thread: child_thread.map(|id| ThreadId::new(id).unwrap()),
    }
}

#[rstest]
#[case("Subagent:")]
#[case("Subagent:   ")]
fn falls_back_to_the_subagent_noun_when_it_has_no_display_name(#[case] description: &str) {
    let presentation = present_pending_background_work(&[task(
        "unnamed",
        BackgroundTaskKind::Subagent,
        Some(description),
        None,
    )])
    .unwrap();
    assert_eq!(presentation.title, "Waiting on a subagent");
    assert_eq!(presentation.items[0].label, "subagent");
}

#[rstest]
#[case("/root/luna_window_properties")]
#[case("Subagent: /root/luna_window_properties")]
#[case("/root/parent/luna_window_properties")]
fn uses_the_subagent_display_name(#[case] description: &str) {
    let presentation = present_pending_background_work(&[task(
        "luna",
        BackgroundTaskKind::Subagent,
        Some(description),
        Some("thread:luna"),
    )]);
    assert_eq!(
        presentation,
        Some(PendingBackgroundWork {
            title: "Waiting on subagent Luna Window Properties".into(),
            items: vec![PendingBackgroundWorkItem {
                task_id: "luna".into(),
                kind: BackgroundWorkKind::Subagent,
                label: "Luna Window Properties".into(),
                child_thread: Some("thread:luna".into()),
            }],
            waiting: true,
        })
    );
}

fn labels(work: &PendingBackgroundWork) -> Vec<&str> {
    work.items.iter().map(|item| item.label.as_str()).collect()
}

#[test]
fn formats_subagent_names_in_a_mixed_roster_and_preserves_command_descriptions() {
    let presentation = present_pending_background_work(&[
        task(
            "cmd",
            BackgroundTaskKind::Command,
            Some("/root/run_tests"),
            None,
        ),
        task(
            "luna",
            BackgroundTaskKind::Subagent,
            Some("/root/luna_window_properties"),
            None,
        ),
        task(
            "review",
            BackgroundTaskKind::Subagent,
            Some("Review src/math.ts"),
            None,
        ),
    ])
    .unwrap();
    assert_eq!(presentation.title, "Waiting on 2 subagents and 1 command");
    assert_eq!(
        labels(&presentation),
        [
            "Luna Window Properties",
            "Review src/math.ts",
            "/root/run_tests"
        ]
    );
}

#[test]
fn names_a_single_piece_of_work_by_kind() {
    let title = |tasks: &[PendingBackgroundTask]| {
        present_pending_background_work(tasks).map(|work| work.title)
    };
    assert_eq!(
        title(&[task(
            "a",
            BackgroundTaskKind::Subagent,
            Some("Review src/math.ts"),
            None
        )])
        .as_deref(),
        Some("Waiting on subagent Review src/math.ts")
    );
    assert_eq!(
        title(&[task("a", BackgroundTaskKind::Monitor, None, None)]).as_deref(),
        Some("Waiting on a monitor")
    );
    assert_eq!(present_pending_background_work(&[]), None);
}

#[test]
fn says_only_commands_are_running_not_waited_on() {
    let present = |tasks: &[PendingBackgroundTask]| {
        let work = present_pending_background_work(tasks).unwrap();
        (work.title, work.waiting)
    };
    assert_eq!(
        present(&[task(
            "dev",
            BackgroundTaskKind::Command,
            Some("Start the shared dev server"),
            None
        )]),
        ("Running: Start the shared dev server".into(), false)
    );
    assert_eq!(
        present(&[task("a", BackgroundTaskKind::Command, None, None)]),
        ("Running a command".into(), false)
    );
    assert_eq!(
        present(&[
            task("a", BackgroundTaskKind::Command, Some("vp run dev"), None),
            task(
                "b",
                BackgroundTaskKind::Command,
                Some("tailscale serve"),
                None
            ),
        ]),
        ("Running 2 commands".into(), false)
    );
    assert_eq!(
        present(&[
            task("a", BackgroundTaskKind::Command, Some("vp run dev"), None),
            task(
                "b",
                BackgroundTaskKind::Monitor,
                Some("Watch PR checks"),
                None
            ),
        ]),
        ("Waiting on 1 command and 1 monitor".into(), true)
    );
}

#[test]
fn groups_work_by_kind_subagents_first_and_keeps_each_name() {
    let presentation = present_pending_background_work(&[
        task("cmd", BackgroundTaskKind::Command, Some("npm test"), None),
        task(
            "b",
            BackgroundTaskKind::Subagent,
            Some("Write tests"),
            Some("thread:b"),
        ),
        task(
            "a",
            BackgroundTaskKind::Subagent,
            Some("Review src/math.ts"),
            None,
        ),
    ])
    .unwrap();
    assert_eq!(presentation.title, "Waiting on 2 subagents and 1 command");
    assert_eq!(
        presentation
            .items
            .iter()
            .map(|item| (item.kind, item.label.as_str(), item.child_thread.as_deref()))
            .collect::<Vec<_>>(),
        [
            (
                BackgroundWorkKind::Subagent,
                "Write tests",
                Some("thread:b")
            ),
            (BackgroundWorkKind::Subagent, "Review src/math.ts", None),
            (BackgroundWorkKind::Command, "npm test", None),
        ]
    );
}

#[test]
fn names_generic_work_including_rosters_that_predate_kinds() {
    assert_eq!(
        present_pending_background_work(&[
            task(
                "bash",
                BackgroundTaskKind::Command,
                Some("Background sleep"),
                None
            ),
            task("watch", BackgroundTaskKind::Monitor, None, None),
            task("other", BackgroundTaskKind::BackgroundTask, None, None),
        ])
        .unwrap()
        .title,
        "Waiting on 1 command, 1 monitor and 1 background task"
    );
    assert_eq!(
        present_pending_background_work(&[task(
            "old",
            BackgroundTaskKind::BackgroundTask,
            None,
            None
        )])
        .unwrap()
        .title,
        "Waiting on a background task"
    );
}

#[rstest]
#[case(0, "0s")]
#[case(-5_000, "0s")]
#[case(12_999, "12s")]
#[case(59_000, "59s")]
#[case(60_000, "1m 00s")]
#[case(724_000, "12m 04s")]
#[case(3_599_000, "59m 59s")]
#[case(3_600_000, "1h")]
#[case(3_723_000, "1h 2m 3s")]
#[case(7_260_000, "2h 1m")]
fn formats_the_working_duration(#[case] elapsed_ms: i64, #[case] expected: &str) {
    let started = 1_000_000;
    assert_eq!(
        format_working_duration(started, started + elapsed_ms),
        expected
    );
}

fn ms(value: &str) -> i64 {
    Timestamp::parse(value).unwrap().millis()
}

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn message(id: &str, text: &str) -> Message {
    Message {
        scheduled_task: None,
        notification: None,
        id: MessageId::new(id).unwrap(),
        run: None,
        role: Role::User,
        text: text.into(),
        attachments: vec![],
        intent: InputIntent::TurnStart,
        streaming: false,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
        created_at: at(),
        updated_at: at(),
        context: None,
    }
}

fn user_item(id: &str, run_id: &str, message_id: &str, ordinal: u64) -> Item {
    Item {
        kind: ItemKind::UserMessage {
            message: MessageId::new(message_id).unwrap(),
        },
        run: Some(RunId::new(run_id).unwrap()),
        ..command_item(id, ordinal)
    }
}

fn input() -> FloatingStatusInput {
    FloatingStatusInput {
        connection_phase: ConnectionStatus::Connected,
        connection_error: None,
        environment_label: Some("Mac mini".into()),
        sync_label: None,
        content: ThreadContentKind::Ready,
        creation: None,
        worktree_setup_visible: false,
        setup_awaiting_turn: false,
    }
}

fn running_state() -> State {
    let mut state = thread_state("Thread");
    let mut active = run("one", 1, RunStatus::Running);
    active.started_at = Some(timestamp("2026-06-20T00:01:00Z"));
    state.runs.push(active);
    state
}

#[test]
fn counts_the_timer_from_the_active_run_start() {
    let state = running_state();
    assert_eq!(
        active_work_started_at(&state),
        Some(ms("2026-06-20T00:01:00Z"))
    );
    assert_eq!(
        floating_working_status(Some(&state), &input()),
        Some(FloatingWorkingStatus::Working {
            started_at_ms: ms("2026-06-20T00:01:00Z")
        })
    );
}

#[test]
fn a_wake_keeps_the_start_of_the_work_it_continues() {
    let mut state = thread_state("Thread");
    let mut first = run("first", 1, RunStatus::Completed);
    first.started_at = Some(timestamp("2026-06-20T00:01:00Z"));
    let mut wake = run("wake", 2, RunStatus::Running);
    wake.started_at = Some(timestamp("2026-06-20T00:05:00Z"));
    let mut notice = message("message-wake", "");
    notice.notification = Some(Notification {
        source: NotificationSource::Native(agent_domain::BackgroundKind::Command),
        child_thread: None,
        outcome: NotificationOutcome::Completed,
        summary: "done".into(),
        detail: None,
    });
    state.messages.push(notice);
    state.runs.extend([first, wake]);
    assert_eq!(
        active_work_started_at(&state),
        Some(ms("2026-06-20T00:01:00Z"))
    );

    state.messages.clear();
    assert_eq!(
        active_work_started_at(&state),
        Some(ms("2026-06-20T00:05:00Z"))
    );
    state.runs[1].restart_of = Some(RunId::new("first").unwrap());
    assert_eq!(
        active_work_started_at(&state),
        Some(ms("2026-06-20T00:01:00Z"))
    );
}

#[test]
fn a_run_without_a_start_counts_from_its_request() {
    let mut state = thread_state("Thread");
    let mut preparing = run("one", 1, RunStatus::Preparing);
    preparing.started_at = None;
    preparing.requested_at = timestamp("2026-06-20T00:02:00Z");
    state.runs.push(preparing);
    assert_eq!(
        active_work_started_at(&state),
        Some(ms("2026-06-20T00:02:00Z"))
    );
}

#[test]
fn compacting_lasts_until_the_compaction_finishes() {
    let mut state = running_state();
    state.messages.push(message("compact", " /COMPACT "));
    state.items.push(user_item("user", "one", "compact", 1));
    assert!(is_compacting(&state));
    assert_eq!(
        floating_working_status(Some(&state), &input()),
        Some(FloatingWorkingStatus::Compacting)
    );

    let mut compaction = command_item("compaction", 2);
    compaction.run = Some(RunId::new("one").unwrap());
    compaction.kind = ItemKind::Compaction {
        before: None,
        after: None,
    };
    compaction.status = ItemStatus::Running;
    state.items.push(compaction);
    assert!(is_compacting(&state));
    state.items[1].status = ItemStatus::Completed;
    assert!(!is_compacting(&state));
}

#[test]
fn a_compact_message_with_attachments_is_an_ordinary_turn() {
    let mut state = running_state();
    let mut compact = message("compact", "/compact");
    compact.attachments = vec![Attachment {
        kind: AttachmentKind::File,
        source: None,
        id: "a".into(),
        name: "a.txt".into(),
        mime_type: "text/plain".into(),
        path: "a.txt".into(),
        size: 1,
    }];
    state.messages.push(compact);
    state.items.push(user_item("user", "one", "compact", 1));
    assert!(!is_compacting(&state));
}

#[test]
fn a_pending_request_hides_the_status() {
    let mut state = running_state();
    state.requests.push(Request {
        owner_path: vec![],
        id: RuntimeRequestId::new("request").unwrap(),
        attempt: RunAttemptId::new("attempt").unwrap(),
        native_key: "request".into(),
        body: RequestBody::Questions { questions: vec![] },
        capability: ResponseCapability::Live,
        status: RequestStatus::Pending,
        decision: None,
        answers: None,
        attachments: BTreeMap::new(),
        created_at: at(),
        resolved_at: None,
    });
    assert_eq!(floating_working_status(Some(&state), &input()), None);
    assert!(
        floating_working_status(
            Some(&state),
            &FloatingStatusInput {
                connection_phase: ConnectionStatus::Offline,
                ..input()
            }
        )
        .is_some()
    );
}

#[test]
fn a_thread_being_created_shows_its_preparation() {
    let preparing = |worktree, worktree_setup_visible| {
        floating_working_status(
            None,
            &FloatingStatusInput {
                creation: Some(ThreadCreation::Preparing { worktree }),
                worktree_setup_visible,
                ..input()
            },
        )
    };
    assert_eq!(
        preparing(true, false),
        Some(FloatingWorkingStatus::Preparing {
            label: "Setting up worktree…".into()
        })
    );
    assert_eq!(
        preparing(false, false),
        Some(FloatingWorkingStatus::Preparing {
            label: "Starting…".into()
        })
    );
    assert_eq!(preparing(true, true), None);
    assert_eq!(
        floating_working_status(
            None,
            &FloatingStatusInput {
                creation: Some(ThreadCreation::Failed),
                ..input()
            }
        ),
        None
    );
}

#[test]
fn sync_precedes_work_and_work_waits_for_ready_content() {
    let state = running_state();
    assert_eq!(
        floating_working_status(
            Some(&state),
            &FloatingStatusInput {
                sync_label: Some("Syncing messages...".into()),
                ..input()
            }
        ),
        Some(FloatingWorkingStatus::Syncing {
            label: "Syncing messages...".into()
        })
    );
    assert_eq!(
        floating_working_status(
            Some(&state),
            &FloatingStatusInput {
                content: ThreadContentKind::Loading,
                ..input()
            }
        ),
        None
    );
    assert_eq!(
        floating_working_status(
            Some(&state),
            &FloatingStatusInput {
                setup_awaiting_turn: true,
                ..input()
            }
        ),
        None
    );
}

#[test]
fn a_settled_turn_reports_the_work_it_left_running() {
    let mut state = thread_state("Thread");
    state.runs.push(run("one", 1, RunStatus::Completed));
    let mut server = command_item("dev", 1);
    server.run = Some(RunId::new("one").unwrap());
    server.status = ItemStatus::Running;
    server.kind = ItemKind::CommandExecution {
        command: "npm run dev".into(),
        cwd: None,
        exit_code: None,
        title: Some("Start the dev server".into()),
    };
    state.items.push(server);
    assert_eq!(
        floating_working_status(Some(&state), &input()),
        Some(FloatingWorkingStatus::Background {
            label: "Running: Start the dev server".into(),
            accessibility_label: "Running: Start the dev server: Start the dev server".into(),
            waiting: false,
        })
    );
}

#[rstest]
#[case(
    ThreadStatus::Empty,
    ThreadContentKind::Loading,
    Some("Loading messages...")
)]
#[case(
    ThreadStatus::Cached,
    ThreadContentKind::Ready,
    Some("Syncing messages...")
)]
#[case(
    ThreadStatus::Synchronizing,
    ThreadContentKind::Ready,
    Some("Syncing messages...")
)]
#[case(ThreadStatus::Synchronizing, ThreadContentKind::Unavailable, None)]
#[case(ThreadStatus::Live, ThreadContentKind::Ready, None)]
#[case(ThreadStatus::Deleted, ThreadContentKind::Unavailable, None)]
fn labels_the_sync_by_whether_messages_are_on_screen(
    #[case] status: ThreadStatus,
    #[case] content: ThreadContentKind,
    #[case] expected: Option<&str>,
) {
    assert_eq!(thread_sync_label(status, content).as_deref(), expected);
}

#[test]
fn the_control_labels_the_timer_queue_and_agents() {
    let view = working_control(
        Some(FloatingWorkingStatus::Working {
            started_at_ms: 1_000,
        }),
        Some(PillSegment {
            label: "2/3".into(),
            accessibility_label: "2 of 3 agents working".into(),
        }),
        3,
        true,
        1_000 + 724_000,
    )
    .unwrap();
    assert_eq!(view.status_label.as_deref(), Some("Working 12m 04s"));
    assert_eq!(view.status_key.as_deref(), Some("working"));
    assert!(!view.status_interactive);
    assert_eq!(
        view.agents,
        Some(PillSegment {
            label: "2/3".into(),
            accessibility_label: "Open agents, 2 of 3 agents working".into(),
        })
    );
    assert_eq!(
        view.queue,
        Some(PillSegment {
            label: "3 queued".into(),
            accessibility_label: "Open queue, 3 messages".into(),
        })
    );
    assert!(view.has_capsule && view.show_scroll_to_end);
}

#[test]
fn the_control_hides_without_content_or_scroll_target() {
    assert_eq!(working_control(None, None, 0, false, 0), None);
    let scroll_only = working_control(None, None, 0, true, 0).unwrap();
    assert!(!scroll_only.has_capsule && scroll_only.show_scroll_to_end);
    let compacting =
        working_control(Some(FloatingWorkingStatus::Compacting), None, 0, false, 0).unwrap();
    assert_eq!(compacting.status_label.as_deref(), Some("Compacting…"));
    assert_eq!(
        compacting.status_accessibility_label.as_deref(),
        Some("Compacting")
    );
}

#[test]
fn counts_only_messages_the_user_queued() {
    let mut state = running_state();
    state.runs.push(run("two", 2, RunStatus::Queued));
    let mut wake = run("three", 3, RunStatus::Queued);
    wake.message = MessageId::new("wake").unwrap();
    let mut notice = message("wake", "");
    notice.notification = Some(Notification {
        source: NotificationSource::Native(agent_domain::BackgroundKind::Monitor),
        child_thread: None,
        outcome: NotificationOutcome::Updated,
        summary: "update".into(),
        detail: None,
    });
    state.messages.push(notice);
    state.runs.push(wake);
    assert_eq!(queued_count(&state), 1);
}

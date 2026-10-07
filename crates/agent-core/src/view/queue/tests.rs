use super::*;
use crate::commands::build::{StartTurn, TurnDispatch, TurnMessage, dispatch, send_command};
use crate::commands::outbox::PendingCommand;
use crate::sync::fixtures::*;
use crate::view::thread_order::drag_gap_offset;
use agent_domain::{
    Attempt, AttemptStatus, CommandId, InputIntent, Message, MessageAuthor, MessageId, Role,
    RunAttemptId, RunStatus,
};

fn controls(
    busy: bool,
    can_promote_to_steer: bool,
    index: usize,
    is_editing: bool,
    queued_count: usize,
    text: &str,
) -> QueueRowControls {
    queue_row_controls(QueueRowControlsInput {
        busy,
        can_promote_to_steer,
        can_reorder: true,
        index,
        is_editing,
        queued_count,
        text,
    })
}

#[test]
fn preserves_queue_reorder_and_steer_controls_with_removal() {
    let controls = controls(
        false,
        true,
        1,
        false,
        3,
        "Please review the follow-up change.",
    );
    assert_eq!(controls.display_text, "Please review the follow-up change.");
    assert!(controls.can_move_up);
    assert!(controls.can_move_down);
    assert!(controls.can_steer);
    assert!(controls.can_dismiss);
    assert_eq!(
        controls.dismiss_accessibility_label,
        REMOVE_QUEUED_MESSAGE_ACCESSIBILITY_LABEL
    );
}

#[test]
fn disables_edge_reorder_controls_and_busy_dismissal() {
    let first = controls(false, false, 0, false, 2, "First");
    let busy = controls(true, true, 0, false, 1, "Queued message");
    assert!(!first.can_move_up);
    assert!(first.can_move_down);
    assert!(!first.can_steer);
    assert!(!busy.can_dismiss);
    assert!(!busy.can_move_up);
    assert!(!busy.can_steer);
}

#[test]
fn keeps_the_row_already_open_in_the_composer_from_being_reopened_or_steered() {
    let editing = controls(false, true, 1, true, 3, "Being edited");
    assert!(editing.is_editing);
    assert!(!editing.can_edit);
    assert!(!editing.can_steer);
    // Reordering and removing a message stay available while it is edited.
    assert!(editing.can_move_up);
    assert!(editing.can_dismiss);
}

fn layouts() -> Vec<QueueRowLayout> {
    [
        ("first", 0.0, 80.0),
        ("second", 80.0, 140.0),
        ("third", 220.0, 80.0),
    ]
    .into_iter()
    .map(|(id, y, height)| QueueRowLayout {
        id: id.into(),
        y: Some(y),
        height: Some(height),
    })
    .collect()
}

fn before(id: &str) -> Option<QueueDropTarget> {
    Some(QueueDropTarget::Before { run_id: id.into() })
}

#[test]
fn moves_between_variable_height_rows_and_to_either_end() {
    let rows = layouts();
    assert_eq!(queue_drop_target(&rows, "first", 140.0), before("third"));
    assert_eq!(
        queue_drop_target(&rows, "first", 300.0),
        Some(QueueDropTarget::End)
    );
    assert_eq!(queue_drop_target(&rows, "third", -300.0), before("first"));
}

#[test]
fn opens_the_destination_gap_while_the_dragged_row_crosses_other_rows() {
    let rows = layouts();
    let offsets = |run_id: &str, translation: f64| -> Option<Vec<f64>> {
        let target = queue_drag_target(&rows, run_id, translation)?;
        let source = rows.iter().find(|row| row.id == run_id).unwrap();
        let last = rows.last().unwrap();
        let insertion = match target {
            QueueDropTarget::End => last.y.unwrap() + last.height.unwrap(),
            QueueDropTarget::Before { run_id } => {
                rows.iter().find(|row| row.id == run_id).unwrap().y.unwrap()
            }
        };
        Some(
            rows.iter()
                .map(|row| {
                    drag_gap_offset(
                        row.y.unwrap(),
                        source.y.unwrap(),
                        source.height.unwrap(),
                        insertion,
                    )
                })
                .collect(),
        )
    };
    assert_eq!(queue_drag_target(&rows, "first", 0.0), before("second"));
    assert_eq!(offsets("first", 140.0), Some(vec![0.0, -80.0, 0.0]));
    assert_eq!(offsets("first", 300.0), Some(vec![0.0, -80.0, -80.0]));
    assert_eq!(offsets("third", -300.0), Some(vec![80.0, 80.0, 0.0]));
    assert_eq!(offsets("second", 0.0), Some(vec![0.0, 0.0, 0.0]));
}

#[test]
fn does_not_send_a_reorder_for_an_unchanged_or_unmeasured_drop() {
    let rows = layouts();
    assert_eq!(queue_drop_target(&rows, "second", 0.0), None);
    assert_eq!(queue_drop_target(&rows, "third", 20.0), None);
    let unmeasured = [QueueRowLayout {
        id: "first".into(),
        y: None,
        height: None,
    }];
    assert_eq!(queue_drop_target(&unmeasured, "first", 10.0), None);
    assert_eq!(queue_drop_target(&rows, "missing", 100.0), None);
}

fn ids(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).into()).collect()
}

#[test]
fn arrow_keys_and_drops_anchor_before_the_following_row() {
    let order = ids(&["a", "b", "c"]);
    assert_eq!(queue_step_target(&order, "a", true), None);
    assert_eq!(queue_step_target(&order, "b", true), before("a"));
    assert_eq!(queue_step_target(&order, "a", false), before("c"));
    assert_eq!(
        queue_step_target(&order, "b", false),
        Some(QueueDropTarget::End)
    );
    assert_eq!(queue_step_target(&order, "c", false), None);
    assert_eq!(queue_insert_target(&order, "a", 0), None);
    assert_eq!(queue_insert_target(&order, "a", 1), None);
    assert_eq!(queue_insert_target(&order, "a", 2), before("c"));
    assert_eq!(
        queue_insert_target(&order, "a", 3),
        Some(QueueDropTarget::End)
    );
    assert_eq!(
        queue_order_after_move(&order, "a", &QueueDropTarget::End),
        ids(&["b", "c", "a"])
    );
    assert_eq!(
        queue_order_after_move(&order, "c", &before("a").unwrap()),
        ids(&["c", "a", "b"])
    );
}

fn message(id: &str, text: &str, attachments: Vec<Attachment>) -> Message {
    Message {
        notification: None,
        id: MessageId::new(id).unwrap(),
        run: None,
        role: Role::User,
        text: text.into(),
        attachments,
        intent: InputIntent::QueuedTurn,
        streaming: false,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
        created_at: at(),
        updated_at: at(),
        context: None,
    }
}

fn attachment(id: &str, kind: AttachmentKind) -> Attachment {
    Attachment {
        kind,
        source: None,
        id: id.into(),
        name: format!("{id}.png"),
        mime_type: "image/png".into(),
        path: String::new(),
        size: 4,
    }
}

fn queued(state: &mut State, id: &str, ordinal: u64, text: &str, attachments: Vec<Attachment>) {
    let mut queued = run(id, ordinal, RunStatus::Queued);
    queued.message = MessageId::new(format!("message-{id}")).unwrap();
    state.runs.push(queued);
    state
        .messages
        .push(message(&format!("message-{id}"), text, attachments));
}

/// A running Codex turn the provider accepted, with two queued messages.
fn running_with_queue() -> State {
    let mut state = thread_state("Thread");
    let mut active = run("active", 1, RunStatus::Running);
    active.attempt = Some(RunAttemptId::new("attempt").unwrap());
    state.runs.push(active);
    state.attempts.push(Attempt {
        id: RunAttemptId::new("attempt").unwrap(),
        run: RunId::new("active").unwrap(),
        ordinal: 1,
        status: AttemptStatus::Running,
        native_thread: None,
        native_turn: None,
        native_head: None,
        accepted: true,
        usage: None,
        context_usage: None,
        turn_usage: None,
        usage_accumulator: None,
        usage_observed: false,
        rejected_limits: Default::default(),
        started_at: at(),
        completed_at: None,
    });
    queued(&mut state, "first", 2, "First", vec![]);
    queued(&mut state, "second", 3, "Second", vec![]);
    state
}

fn outbox(commands: Vec<Command>) -> Outbox {
    Outbox {
        entries: commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                let id = CommandId::new(format!("command-{index}")).unwrap();
                PendingCommand::new(
                    thread_id(),
                    Request::Dispatch(Box::new(dispatch(thread_id(), id, command))),
                    at(),
                )
            })
            .collect(),
    }
}

fn queued_send(id: &str, text: &str, dispatch: TurnDispatch) -> Command {
    send_command(StartTurn {
        message: TurnMessage {
            id: MessageId::new(id).unwrap(),
            text: text.into(),
            attachments: vec![attachment("pending-image", AttachmentKind::Image)],
            context: None,
        },
        selection: None,
        title_seed: None,
        source_plan: None,
        dispatch,
        continuation: None,
        creation_source: "desktop".into(),
    })
}

fn keys(view: &QueueView) -> Vec<(String, bool)> {
    view.rows
        .iter()
        .map(|row| (row.key.clone(), row.pending))
        .collect()
}

#[test]
fn lists_confirmed_runs_then_unconfirmed_queued_sends() {
    let state = running_with_queue();
    let sends = outbox(vec![
        queued_send("message-first", "First", TurnDispatch::Queue),
        queued_send("message-unsent", "Unsent", TurnDispatch::Queue),
        queued_send("message-steer", "Steer", TurnDispatch::Steer),
    ]);
    let view = queue_view(&state, &sends, None, QueueShortcuts::default());
    assert_eq!(
        keys(&view),
        [
            ("first".into(), false),
            ("second".into(), false),
            ("message-unsent".into(), true)
        ]
    );
    assert_eq!((view.count, view.queued_count), (3, 2));
    assert_eq!(view.title, "Queued");
    assert_eq!(view.region_accessibility_label, "3 queued messages");
    let pending = &view.rows[2];
    assert_eq!(pending.run_id, None);
    assert_eq!(
        pending.thumbnails,
        [QueueThumbnail {
            attachment_id: "pending-image".into(),
            name: "pending-image.png".into()
        }]
    );
    assert!(!pending.controls.can_edit && !pending.controls.can_dismiss);
    assert!(!pending.controls.can_steer && !pending.controls.can_move_up);
}

#[test]
fn previews_context_links_by_label_and_drops_images_shown_as_thumbnails() {
    let text = "  See ![shot.png](context://v1/image/ctx_1) and [notes](context://v1/file/ctx_2)  ";
    assert_eq!(queue_preview_text(text, true), "See  and notes");
    assert_eq!(queue_preview_text(text, false), "See shot.png and notes");
}

#[test]
fn compact_rows_fall_back_to_attachments_and_count_unshown_files() {
    let mut state = thread_state("Thread");
    let images = (0..4)
        .map(|index| attachment(&format!("image-{index}"), AttachmentKind::Image))
        .chain([attachment("notes", AttachmentKind::File)])
        .collect();
    queued(&mut state, "images", 1, "", images);
    queued(
        &mut state,
        "file",
        2,
        "",
        vec![attachment("only", AttachmentKind::File)],
    );
    queued(&mut state, "empty", 3, "", vec![]);
    let view = queue_view(&state, &Outbox::default(), None, QueueShortcuts::default());
    let compact: Vec<_> = view
        .rows
        .iter()
        .map(|row| (row.title.as_str(), row.compact_overflow.as_deref()))
        .collect();
    assert_eq!(
        compact,
        [
            ("Attachments", Some("+2")),
            ("Attachments", Some("1")),
            ("Queued message", None)
        ]
    );
    assert_eq!(view.rows[0].thumbnails.len(), 4);
    assert_eq!(
        view.rows[0].reorder_accessibility_label,
        "Reorder Attachments"
    );
}

#[test]
fn a_held_queue_offers_resume_until_the_resume_is_sent() {
    let mut state = thread_state("Thread");
    queued(&mut state, "held", 1, "Saved", vec![]);
    state.runs[0].queue_held = true;
    let view = queue_view(&state, &Outbox::default(), None, QueueShortcuts::default());
    assert_eq!(
        view.held_notice.as_deref(),
        Some("Queue held after restart")
    );
    assert!(view.can_resume);
    let resuming = outbox(vec![Command::ResumeQueue]);
    assert!(!queue_view(&state, &resuming, None, QueueShortcuts::default()).can_resume);
    state.runs[0].status = RunStatus::Cancelled;
    assert_eq!(
        queue_view(&state, &Outbox::default(), None, QueueShortcuts::default()).held_notice,
        None
    );
}

#[test]
fn a_waiting_queue_command_disables_every_row() {
    let state = running_with_queue();
    let idle = queue_view(&state, &Outbox::default(), None, QueueShortcuts::default());
    assert!(!idle.busy);
    assert!(idle.rows.iter().all(|row| row.controls.can_dismiss));
    assert_eq!(idle.edit_latest_run_id.as_deref(), Some("second"));
    let removing = outbox(vec![Command::CancelQueued {
        run: RunId::new("first").unwrap(),
    }]);
    let busy = queue_view(&state, &removing, None, QueueShortcuts::default());
    assert!(busy.busy);
    assert!(busy.rows.iter().all(|row| {
        !row.controls.can_dismiss && !row.controls.can_edit && !row.controls.can_steer
    }));
    assert_eq!(busy.edit_latest_run_id, None);
}

#[test]
fn marks_the_edited_row_and_names_the_shortcut_targets() {
    let state = running_with_queue();
    let editing = RunId::new("second").unwrap();
    let shortcuts = QueueShortcuts {
        steer: Some("⌘⇧Enter"),
        edit: Some("↑"),
    };
    let view = queue_view(&state, &Outbox::default(), Some(&editing), shortcuts);
    let editing_rows: Vec<_> = view.rows.iter().map(|row| row.editing).collect();
    assert_eq!(editing_rows, [false, true]);
    assert!(!view.rows[1].controls.can_edit && !view.rows[1].controls.can_steer);
    assert_eq!(view.editing_run_id.as_deref(), Some("second"));
    assert_eq!(view.edit_latest_run_id, None);
    assert_eq!(view.steer_next_run_id.as_deref(), Some("first"));
    assert_eq!(
        view.rows[0].steer_tooltip,
        "Send as a steer instead (⌘⇧Enter)"
    );
    assert_eq!(view.rows[1].steer_tooltip, "Send as a steer instead");
    assert_eq!(view.rows[0].edit_tooltip, "Edit in the composer");
    assert_eq!(view.rows[1].edit_tooltip, "Edit in the composer (↑)");
    assert_eq!(
        view.rows[1].steer_accessibility_label,
        "Steer with message 2 now"
    );
}

#[test]
fn rows_cannot_steer_without_an_active_run() {
    let mut state = thread_state("Thread");
    queued(&mut state, "only", 1, "Only", vec![]);
    let view = queue_view(&state, &Outbox::default(), None, QueueShortcuts::default());
    assert!(!view.can_promote_to_steer);
    assert_eq!(view.steer_next_run_id, None);
    assert_eq!(
        view.rows[0].steer_tooltip,
        "There is no active run to steer"
    );
    assert!(!view.rows[0].controls.can_steer);
    assert_eq!(view.region_accessibility_label, "1 queued message");
}

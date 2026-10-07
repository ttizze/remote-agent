use super::*;
use crate::state::{Draft, QuestionDrafts};
use crate::sync::fixtures::{run, thread_id};
use crate::view::composer::actions::{ComposerPrimaryAction, SendIcon};
use crate::view::requests::{QuestionDraft, option_value};
use crate::view::timeline::rows::TimelineRowKind;
use crate::view::work_log::fixtures::*;
use agent_domain::{Question, QuestionOption, Role, RunStatus, State};
use std::sync::Arc;

/// A finished turn: the user's message and the answer.
fn finished_state() -> State {
    let mut state = state(vec![
        user_message("item-user", "message-user")
            .run("run-1")
            .ordinal(1)
            .started("2026-09-06T10:00:00Z"),
        assistant_message("item-answer", "message-answer")
            .run("run-1")
            .ordinal(2)
            .started("2026-09-06T10:00:02Z"),
    ]);
    let mut first = run("run-1", 1, RunStatus::Completed);
    first.started_at = Some(timestamp("2026-09-06T10:00:00Z"));
    first.completed_at = Some(timestamp("2026-09-06T10:00:03Z"));
    state.runs.push(first);
    let mut user = message("message-user", Role::User, "Run the tests");
    user.run = Some(RunId::new("run-1").unwrap());
    let mut answer = message("message-answer", Role::Assistant, "All green.");
    answer.run = Some(RunId::new("run-1").unwrap());
    state.messages.extend([user, answer]);
    state
}

fn showing(state: State) -> Snapshot {
    let mut sync = ThreadSync::default();
    sync.state = Some(Arc::new(state));
    sync.cursor = 1;
    sync.status = ThreadStatus::Live;
    let mut snapshot = Snapshot {
        connected: true,
        selected_thread: Some(thread_id()),
        ..Snapshot::default()
    };
    snapshot.threads.insert(thread_id(), Arc::new(sync));
    snapshot
}

fn view(snapshot: &Snapshot) -> ThreadView {
    thread_view(snapshot, &thread_id(), 0, &ThreadViewOptions::default()).unwrap()
}

#[test]
fn a_thread_with_messages_shows_its_rows_header_and_a_send_button() {
    let mut snapshot = showing(finished_state());
    snapshot.drafts.insert(
        thread_id().to_string(),
        Draft {
            text: "Now fix the lint".into(),
            ..Draft::default()
        },
    );
    let view = view(&snapshot);
    assert_eq!(view.sync_status, ThreadStatus::Live);
    assert_eq!(view.header.unwrap().title, "Thread");
    assert!(view.rows.iter().any(|row| matches!(
        &row.kind,
        TimelineRowKind::UserMessage(message) if message.text == "Run the tests"
    )));
    assert!(view.rows.iter().any(|row| matches!(
        &row.kind,
        TimelineRowKind::AssistantMessage(message) if message.text == "All green."
    )));
    assert_eq!(view.composer.text, "Now fix the lint");
    match view.composer.primary_action {
        ComposerPrimaryAction::Send(send) => {
            assert_eq!(send.icon, SendIcon::Send);
            assert_eq!(send.label, "Submit message");
            assert!(!send.disabled);
        }
        action => panic!("expected send, got {action:?}"),
    }
    assert!(view.requests.questions.is_none());
    assert!(view.error_banner.is_none());
}

#[test]
fn clones_of_a_snapshot_share_the_built_rows() {
    let snapshot = showing(finished_state());
    let first = view(&snapshot);
    let again = view(&snapshot.clone());
    assert_eq!(first.rows_revision, again.rows_revision);
    assert_eq!(first.rows, again.rows);
}

#[test]
fn an_unknown_thread_has_no_view() {
    let snapshot = showing(finished_state());
    let missing = ThreadId::new("missing").unwrap();
    assert!(thread_view(&snapshot, &missing, 0, &ThreadViewOptions::default()).is_none());
}

#[test]
fn a_pending_question_can_be_submitted_once_answered() {
    let mut state = finished_state();
    state.requests.push(questions(
        "questions",
        vec![Question {
            required: true,
            id: "scope".into(),
            header: "Scope".into(),
            question: "What should the plan target first?".into(),
            multiple: false,
            options: vec![QuestionOption {
                label: "Orchestration-first".into(),
                description: None,
            }],
        }],
    ));
    let mut snapshot = showing(state);
    let unanswered = view(&snapshot).requests.questions.unwrap();
    assert!(!unanswered.submit_enabled);
    snapshot.question_drafts.insert(
        "questions".into(),
        QuestionDrafts {
            drafts: vec![QuestionDraft {
                question_id: "scope".into(),
                selected_option_values: vec![option_value("Orchestration-first")],
                ..QuestionDraft::default()
            }],
            question_index: 0,
        },
    );
    let view = view(&snapshot);
    let answered = view.requests.questions.unwrap();
    assert!(answered.submit_enabled);
    assert!(answered.answers.is_some());
    match view.composer.primary_action {
        ComposerPrimaryAction::Answer {
            submit_disabled,
            submit_label,
            ..
        } => {
            assert!(!submit_disabled);
            assert_eq!(submit_label, "Submit answer");
        }
        action => panic!("expected an answer, got {action:?}"),
    }
    assert_eq!(
        view.composer.editor.placeholder,
        "Type your own answer, or leave this blank to use the selected option"
    );
}

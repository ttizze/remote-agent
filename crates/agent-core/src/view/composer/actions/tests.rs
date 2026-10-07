use super::*;
use crate::sync::fixtures::*;
use agent_domain::{BackgroundWork, Item, ItemKind, ItemStatus, RunAttemptId, shell};

fn input() -> PrimaryActionInput {
    PrimaryActionInput {
        compact: true,
        pending_answer: None,
        is_running: false,
        can_interrupt: false,
        follow_up: FollowUpBehavior::Steer,
        alternate_modifier: false,
        alternate_shortcut_label: None,
        show_plan_follow_up_prompt: false,
        prompt_has_text: true,
        is_send_busy: false,
        send_disabled_reason: None,
        is_connecting: false,
        is_environment_unavailable: false,
        is_preparing_worktree: false,
        has_sendable_content: true,
        can_resume: false,
        is_editing_queued_message: false,
    }
}

fn pending_actions(is_running: bool) -> ComposerPrimaryAction {
    composer_primary_action(&PrimaryActionInput {
        pending_answer: Some(PendingAnswerProgress {
            question_index: 0,
            is_last_question: true,
            can_advance: true,
            is_responding: false,
            is_complete: true,
        }),
        is_running,
        can_interrupt: is_running,
        prompt_has_text: false,
        has_sendable_content: false,
        ..input()
    })
}

fn send(input: &PrimaryActionInput) -> SendButton {
    match composer_primary_action(input) {
        ComposerPrimaryAction::Send(button) => button,
        other => panic!("expected the send button, got {other:?}"),
    }
}

#[test]
fn disables_and_labels_the_send_button_while_feedback_is_uploading() {
    let button = send(&PrimaryActionInput {
        send_disabled_reason: Some("Sending feedback".into()),
        ..input()
    });
    assert!(button.disabled);
    assert_eq!(button.label, "Sending feedback");
}

#[test]
fn offers_stop_generation_while_a_running_turn_is_waiting_for_user_input() {
    assert!(matches!(
        pending_actions(true),
        ComposerPrimaryAction::Answer {
            show_stop: true,
            ..
        }
    ));
}

#[test]
fn does_not_offer_stop_generation_for_a_pending_request_without_a_running_turn() {
    assert!(matches!(
        pending_actions(false),
        ComposerPrimaryAction::Answer {
            show_stop: false,
            ..
        }
    ));
}

#[test]
fn labels_answer_submission_by_question_position_and_layout() {
    let answer =
        |compact, question_index, is_last_question, is_responding| match composer_primary_action(
            &PrimaryActionInput {
                compact,
                pending_answer: Some(PendingAnswerProgress {
                    question_index,
                    is_last_question,
                    can_advance: true,
                    is_responding,
                    is_complete: false,
                }),
                ..input()
            },
        ) {
            ComposerPrimaryAction::Answer {
                previous,
                submit_label,
                submit_disabled,
                ..
            } => (
                previous.map(|p| (p.label, p.icon_only)),
                submit_label,
                submit_disabled,
            ),
            other => panic!("{other:?}"),
        };
    assert_eq!(answer(true, 0, false, false), (None, "Next".into(), false));
    assert_eq!(answer(true, 1, true, false).1, "Submit");
    assert_eq!(
        answer(true, 1, true, false).0,
        Some(("Previous question".into(), true))
    );
    assert_eq!(answer(false, 0, false, false).1, "Next question");
    assert_eq!(answer(false, 0, true, false).1, "Submit answer");
    assert_eq!(
        answer(false, 2, true, false),
        (
            Some(("Previous".into(), false)),
            "Submit answers".into(),
            true
        )
    );
    assert_eq!(answer(false, 2, true, true).1, "Submitting...");
}

#[test]
fn offers_refine_with_text_and_implement_with_its_new_thread_menu_otherwise() {
    let plan = PrimaryActionInput {
        show_plan_follow_up_prompt: true,
        ..input()
    };
    assert_eq!(
        composer_primary_action(&plan),
        ComposerPrimaryAction::Refine {
            label: "Refine".into(),
            disabled: false
        }
    );
    assert_eq!(
        composer_primary_action(&PrimaryActionInput {
            prompt_has_text: false,
            has_sendable_content: false,
            is_send_busy: true,
            ..plan.clone()
        }),
        ComposerPrimaryAction::Implement {
            label: "Sending...".into(),
            disabled: true,
            new_thread_label: "Implement in a new thread".into(),
        }
    );
    let resume = PrimaryActionInput {
        prompt_has_text: false,
        has_sendable_content: false,
        can_resume: true,
        ..plan
    };
    assert!(send(&resume).resumes);
}

#[test]
fn stops_only_without_sendable_content_and_outside_a_queued_edit() {
    let running = PrimaryActionInput {
        is_running: true,
        can_interrupt: true,
        prompt_has_text: false,
        has_sendable_content: false,
        ..input()
    };
    assert_eq!(
        composer_primary_action(&running),
        ComposerPrimaryAction::Stop {
            label: "Stop generation".into(),
            tooltip: "Interrupt".into()
        }
    );
    let editing = send(&PrimaryActionInput {
        is_editing_queued_message: true,
        ..running.clone()
    });
    assert_eq!(
        (editing.icon, editing.label.as_str(), editing.disabled),
        (SendIcon::Check, "Update queued message", true)
    );
    let steer = send(&PrimaryActionInput {
        has_sendable_content: true,
        prompt_has_text: true,
        ..running
    });
    assert_eq!(steer.icon, SendIcon::Steer);
}

#[test]
fn flips_the_send_icon_between_steer_and_queue_with_the_alternate_modifier() {
    let running = PrimaryActionInput {
        is_running: true,
        can_interrupt: true,
        follow_up: FollowUpBehavior::Queue,
        alternate_shortcut_label: Some("⌘⇧↵".into()),
        ..input()
    };
    let queue = send(&running);
    assert_eq!(queue.icon, SendIcon::Queue);
    assert_eq!(queue.label, "Queue message");
    assert_eq!(
        queue.tooltip,
        "Click to queue, Ctrl/⌘-click or ⌘⇧↵ to steer"
    );
    let steer = send(&PrimaryActionInput {
        alternate_modifier: true,
        alternate_shortcut_label: None,
        ..running
    });
    assert_eq!(steer.icon, SendIcon::Steer);
    assert_eq!(steer.label, "Steer message");
    assert_eq!(steer.tooltip, "Click to queue, Ctrl/⌘-click to steer");
}

#[test]
fn reports_why_sending_waits_and_resumes_an_empty_resumable_thread() {
    let status = |input: PrimaryActionInput| {
        let button = send(&input);
        (button.label, button.tooltip, button.icon, button.disabled)
    };
    assert_eq!(
        status(PrimaryActionInput {
            is_environment_unavailable: true,
            send_disabled_reason: Some("Select a model".into()),
            ..input()
        })
        .0,
        "Environment disconnected"
    );
    assert_eq!(
        status(PrimaryActionInput {
            is_connecting: true,
            ..input()
        }),
        (
            "Connecting".into(),
            "Connecting".into(),
            SendIcon::Spinner,
            true
        )
    );
    assert_eq!(
        status(PrimaryActionInput {
            is_preparing_worktree: true,
            ..input()
        })
        .0,
        "Preparing worktree"
    );
    assert_eq!(
        status(PrimaryActionInput {
            is_send_busy: true,
            ..input()
        })
        .0,
        "Submitting message"
    );
    assert_eq!(
        status(PrimaryActionInput {
            prompt_has_text: false,
            has_sendable_content: false,
            can_resume: true,
            ..input()
        }),
        (
            "Resume thread".into(),
            "Resume thread".into(),
            SendIcon::Resume,
            false
        )
    );
    assert_eq!(
        status(PrimaryActionInput {
            prompt_has_text: false,
            has_sendable_content: false,
            ..input()
        }),
        (
            "Submit message".into(),
            "Submit message".into(),
            SendIcon::Send,
            true
        )
    );
}

fn runtime(status: Option<RunStatus>, active_run: Option<&str>) -> ThreadRuntime {
    ThreadRuntime {
        status,
        active_run: active_run.map(|id| RunId::new(id).unwrap()),
    }
}

#[test]
fn offers_stop_while_a_run_is_preparing_or_starting_not_just_once_it_is_running() {
    let run = Some("run-stop-while-starting");
    for status in [
        RunStatus::Preparing,
        RunStatus::Starting,
        RunStatus::Running,
    ] {
        assert!(can_interrupt_running_thread(
            true,
            Some(&runtime(Some(status), run))
        ));
    }
    assert!(!can_interrupt_running_thread(
        false,
        Some(&runtime(Some(RunStatus::Running), run))
    ));
    assert_eq!(
        session_phase(Some(&runtime(Some(RunStatus::Queued), None))),
        SessionPhase::Connecting
    );
    assert!(!can_interrupt_running_thread(
        true,
        Some(&runtime(Some(RunStatus::Queued), None))
    ));
    assert!(can_interrupt_running_thread(
        true,
        Some(&runtime(Some(RunStatus::Queued), run))
    ));
    assert_eq!(
        session_phase(Some(&runtime(Some(RunStatus::Waiting), None))),
        SessionPhase::Running
    );
    assert!(can_interrupt_running_thread(
        true,
        Some(&runtime(Some(RunStatus::Waiting), None))
    ));
    assert!(!can_interrupt_running_thread(true, None));
}

fn runtime_of(state: &State) -> Option<ThreadRuntime> {
    thread_runtime(&shell(state).unwrap())
}

fn usage_limit_error(run: &str, class: &str) -> Item {
    let mut item = command_item("limit-error", 1);
    item.run = Some(RunId::new(run).unwrap());
    item.status = ItemStatus::Failed;
    item.kind = ItemKind::Error {
        message: "Plan limit reached".into(),
        retry: None,
        code: Some("usageLimitExceeded".into()),
        class: Some(class.into()),
        retryable: None,
        reset_at: None,
    };
    item
}

#[test]
fn keeps_a_subscription_limit_visible_while_later_messages_stay_queued() {
    let mut state = thread_state("Thread");
    let mut queued = run("queued", 2, RunStatus::Queued);
    queued.started_at = None;
    state
        .runs
        .extend([run("limited", 1, RunStatus::Failed), queued]);
    state
        .items
        .push(usage_limit_error("limited", "usage_limit"));
    assert_eq!(
        runtime_of(&state),
        Some(runtime(Some(RunStatus::Failed), None))
    );
    assert_eq!(resumable_run(&state).unwrap().as_str(), "limited");
    state.items[0] = usage_limit_error("limited", "provider_error");
    assert_eq!(
        runtime_of(&state),
        Some(runtime(Some(RunStatus::Queued), None))
    );
    assert_eq!(resumable_run(&state), None);
}

#[test]
fn keeps_live_activity_attached_to_an_executing_run_when_a_newer_run_is_queued() {
    let mut state = thread_state("Thread");
    state.runs.extend([
        run("run-queued", 2, RunStatus::Queued),
        run("run-running", 1, RunStatus::Running),
    ]);
    let runtime_now = runtime_of(&state);
    assert_eq!(
        runtime_now,
        Some(runtime(Some(RunStatus::Running), Some("run-running")))
    );
    assert!(can_interrupt_running_thread(true, runtime_now.as_ref()));
}

#[test]
fn presents_a_held_queue_as_the_stopped_run_instead_of_queued_work() {
    let mut state = thread_state("Thread");
    let mut held = run("run-held", 2, RunStatus::Queued);
    held.queue_held = true;
    state
        .runs
        .extend([run("run-interrupted", 1, RunStatus::Interrupted), held]);
    assert_eq!(
        runtime_of(&state),
        Some(runtime(Some(RunStatus::Interrupted), None))
    );
    assert!(can_resume(&state));
    state.runs[1].queue_held = false;
    assert_eq!(
        runtime_of(&state),
        Some(runtime(Some(RunStatus::Queued), None))
    );
    state.runs.remove(0);
    state.runs[0].queue_held = true;
    assert_eq!(runtime_of(&state), None);
    assert!(can_resume(&state));
}

#[test]
fn does_not_expose_a_queued_only_or_checkpoint_waiting_run_as_interruptible() {
    for status in [RunStatus::Queued, RunStatus::Waiting] {
        let mut state = thread_state("Thread");
        state.runs.push(run("run", 1, status));
        let runtime_now = runtime_of(&state).unwrap();
        assert_eq!(runtime_now, runtime(Some(status), None));
        assert!(!(runtime_is_active(&runtime_now) && runtime_now.active_run.is_some()));
    }
}

#[test]
fn background_work_that_wakes_the_agent_parks_the_thread_at_idle() {
    let mut state = thread_state("Thread");
    state.runs.push(run("run", 1, RunStatus::Completed));
    let work = |kind| BackgroundWork {
        key: "work".into(),
        tool: "Task".into(),
        description: "Review".into(),
        kind,
        attempt: RunAttemptId::new("attempt").unwrap(),
    };
    state
        .background_work
        .insert("work".into(), work(BackgroundKind::Command));
    assert_eq!(
        runtime_of(&state),
        Some(runtime(Some(RunStatus::Completed), None))
    );
    state
        .background_work
        .insert("work".into(), work(BackgroundKind::Subagent));
    assert_eq!(runtime_of(&state), Some(runtime(None, None)));
    assert_eq!(
        session_phase(runtime_of(&state).as_ref()),
        SessionPhase::Ready
    );
}

fn idle() -> MobileSendInput {
    MobileSendInput {
        editing_queued_message: false,
        running: false,
        can_steer: false,
        follow_up: FollowUpBehavior::Queue,
        delivery_deferred: false,
    }
}

#[test]
fn sends_plainly_while_the_thread_is_idle() {
    let presentation = mobile_send_presentation(idle());
    assert_eq!(presentation.label, "Send");
    assert_eq!(presentation.icon, MobileSendIcon::ArrowUp);
    assert!(!presentation.offers_follow_up_choice);
    assert_eq!(presentation.action, None);
}

#[test]
fn says_queue_while_the_outbox_is_holding_the_message_back() {
    let presentation = mobile_send_presentation(MobileSendInput {
        delivery_deferred: true,
        ..idle()
    });
    assert_eq!(presentation.label, "Queue");
}

#[test]
fn follows_the_configured_behavior_once_a_turn_is_running() {
    let running = |follow_up| {
        mobile_send_presentation(MobileSendInput {
            running: true,
            can_steer: true,
            follow_up,
            ..idle()
        })
    };
    let queueing = running(FollowUpBehavior::Queue);
    let steering = running(FollowUpBehavior::Steer);
    assert_eq!(queueing.label, "Queue");
    assert_eq!(queueing.icon, MobileSendIcon::ListNumber);
    assert_eq!(queueing.action, Some(FollowUpBehavior::Queue));
    assert_eq!(queueing.alternate, Some(FollowUpBehavior::Steer));
    assert_eq!(steering.label, "Steer");
    assert_eq!(steering.icon, MobileSendIcon::ArrowTurnLeftUp);
    assert_eq!(steering.action, Some(FollowUpBehavior::Steer));
    assert_eq!(steering.alternate, Some(FollowUpBehavior::Queue));
    assert!(steering.offers_follow_up_choice);
}

#[test]
fn never_promises_steering_the_provider_cannot_do() {
    let presentation = mobile_send_presentation(MobileSendInput {
        running: true,
        can_steer: false,
        follow_up: FollowUpBehavior::Steer,
        ..idle()
    });
    assert_eq!(presentation.label, "Queue");
    assert_eq!(presentation.action, Some(FollowUpBehavior::Queue));
    assert_eq!(presentation.alternate, None);
    assert!(!presentation.offers_follow_up_choice);
}

#[test]
fn keeps_the_save_affordance_while_a_queued_message_is_being_edited() {
    let presentation = mobile_send_presentation(MobileSendInput {
        editing_queued_message: true,
        running: true,
        can_steer: true,
        follow_up: FollowUpBehavior::Steer,
        ..idle()
    });
    assert_eq!(presentation.label, "Update queued message");
    assert_eq!(presentation.icon, MobileSendIcon::Checkmark);
    assert!(!presentation.offers_follow_up_choice);
}

#[test]
fn mobile_stop_replaces_send_only_for_an_empty_draft_outside_an_edit() {
    assert!(mobile_shows_stop(false, true, false));
    assert!(!mobile_shows_stop(true, true, false));
    assert!(!mobile_shows_stop(false, true, true));
    assert!(!mobile_shows_stop(false, false, false));
}

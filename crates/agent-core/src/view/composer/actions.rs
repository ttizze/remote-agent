//! The composer's primary action: send, steer, queue, stop, resume, answer and
//! the plan follow-up pills.
use crate::commands::build::{
    ComposerDispatchMode, FollowUpBehavior, alternate_follow_up, resolve_composer_dispatch_mode,
};
use agent_domain::{
    BackgroundKind, RunId, RunStatus, State, ThreadShell, failure_class, latest_executed_run,
};

/// The thread's conversation phase as the composer sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SessionPhase {
    Disconnected,
    Connecting,
    Running,
    Ready,
}

/// What the thread is doing, summarised from its list row. `status` is `None`
/// while the thread is idle.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadRuntime {
    pub status: Option<RunStatus>,
    pub active_run: Option<RunId>,
}

fn holds_completion(kind: BackgroundKind) -> bool {
    !matches!(kind, BackgroundKind::Command)
}

/// The runtime summary of a thread that has run at least once. A usage-limit
/// failure stays the presented status; background work that will wake the
/// agent parks the thread at idle.
pub fn thread_runtime(shell: &ThreadShell) -> Option<ThreadRuntime> {
    shell.latest_run.as_ref()?;
    let usage_limited = shell.status == Some(RunStatus::Failed)
        && shell.last_error_class.as_deref() == Some("usage_limit");
    let holds = shell.active_run.is_none()
        && shell
            .pending_background_work
            .iter()
            .any(|work| holds_completion(work.kind));
    let status = if usage_limited {
        Some(RunStatus::Failed)
    } else if holds && shell.status != Some(RunStatus::Failed) {
        None
    } else {
        shell.activity_run_status.or(shell.status)
    };
    Some(ThreadRuntime {
        status,
        active_run: shell.active_run.clone(),
    })
}

pub fn session_phase(runtime: Option<&ThreadRuntime>) -> SessionPhase {
    let Some(runtime) = runtime else {
        return SessionPhase::Disconnected;
    };
    match runtime.status {
        Some(RunStatus::Preparing | RunStatus::Starting | RunStatus::Queued) => {
            SessionPhase::Connecting
        }
        Some(RunStatus::Running | RunStatus::Waiting) => SessionPhase::Running,
        _ => SessionPhase::Ready,
    }
}

fn runtime_is_active(runtime: &ThreadRuntime) -> bool {
    runtime.status.is_some_and(|status| !status.terminal())
}

/// Stop is offered while a run is preparing or starting too, since the Host
/// settles those on interrupt; a queued thread offers it only while an earlier
/// run is still interruptible.
pub fn can_interrupt_running_thread(
    has_active_thread: bool,
    runtime: Option<&ThreadRuntime>,
) -> bool {
    has_active_thread
        && (session_phase(runtime) == SessionPhase::Running
            || runtime
                .is_some_and(|runtime| runtime_is_active(runtime) && runtime.active_run.is_some()))
}

/// The latest executed run when it was interrupted or stopped by a usage limit.
pub fn resumable_run(state: &State) -> Option<RunId> {
    let run = latest_executed_run(state)?;
    let resumable = run.status == RunStatus::Interrupted
        || (run.status == RunStatus::Failed
            && failure_class(state, &run.id).as_deref() == Some("usage_limit"));
    resumable.then(|| run.id.clone())
}

/// Resume continues a stopped run or releases a queue held after a restart.
pub fn can_resume(state: &State) -> bool {
    resumable_run(state).is_some()
        || state
            .runs
            .iter()
            .any(|run| run.status == RunStatus::Queued && run.queue_held)
}

/// Progress of the question being answered in the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PendingAnswerProgress {
    pub question_index: u32,
    pub is_last_question: bool,
    pub can_advance: bool,
    pub is_responding: bool,
    pub is_complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimaryActionInput {
    pub compact: bool,
    pub pending_answer: Option<PendingAnswerProgress>,
    /// A turn is in flight: sending steers or queues instead of starting one.
    pub is_running: bool,
    pub can_interrupt: bool,
    pub follow_up: FollowUpBehavior,
    /// The alternate send modifier (Ctrl/⌘) is held.
    pub alternate_modifier: bool,
    pub alternate_shortcut_label: Option<String>,
    pub show_plan_follow_up_prompt: bool,
    pub prompt_has_text: bool,
    pub is_send_busy: bool,
    pub send_disabled_reason: Option<String>,
    pub is_connecting: bool,
    pub is_environment_unavailable: bool,
    pub is_preparing_worktree: bool,
    pub has_sendable_content: bool,
    pub can_resume: bool,
    pub is_editing_queued_message: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PreviousQuestionButton {
    pub label: String,
    /// Compact layouts show a chevron with the label as its accessible name.
    pub icon_only: bool,
    pub disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SendIcon {
    Spinner,
    Resume,
    Check,
    Queue,
    Steer,
    Send,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SendButton {
    /// Pressing it resumes the thread instead of submitting the draft.
    pub resumes: bool,
    pub icon: SendIcon,
    pub label: String,
    pub tooltip: String,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ComposerPrimaryAction {
    Answer {
        show_stop: bool,
        previous: Option<PreviousQuestionButton>,
        submit_label: String,
        submit_disabled: bool,
    },
    Refine {
        label: String,
        disabled: bool,
    },
    /// A split button whose menu offers implementing in a new thread.
    Implement {
        label: String,
        disabled: bool,
        new_thread_label: String,
    },
    Stop {
        label: String,
        tooltip: String,
    },
    Send(SendButton),
}

pub const STOP_LABEL: &str = "Stop generation";
pub const STOP_TOOLTIP: &str = "Interrupt";
pub const IMPLEMENT_IN_NEW_THREAD_LABEL: &str = "Implement in a new thread";

fn pending_answer_label(compact: bool, progress: &PendingAnswerProgress) -> &'static str {
    if progress.is_responding {
        "Submitting..."
    } else if compact {
        if progress.is_last_question {
            "Submit"
        } else {
            "Next"
        }
    } else if !progress.is_last_question {
        "Next question"
    } else if progress.question_index > 0 {
        "Submit answers"
    } else {
        "Submit answer"
    }
}

fn behavior_name(behavior: FollowUpBehavior) -> &'static str {
    match behavior {
        FollowUpBehavior::Queue => "queue",
        FollowUpBehavior::Steer => "steer",
        FollowUpBehavior::Restart => "restart",
    }
}

pub fn composer_primary_action(input: &PrimaryActionInput) -> ComposerPrimaryAction {
    if let Some(progress) = &input.pending_answer {
        return ComposerPrimaryAction::Answer {
            show_stop: input.can_interrupt,
            previous: (progress.question_index > 0).then(|| PreviousQuestionButton {
                label: if input.compact {
                    "Previous question"
                } else {
                    "Previous"
                }
                .into(),
                icon_only: input.compact,
                disabled: progress.is_responding,
            }),
            submit_label: pending_answer_label(input.compact, progress).into(),
            submit_disabled: input.is_environment_unavailable
                || progress.is_responding
                || if progress.is_last_question {
                    !progress.is_complete
                } else {
                    !progress.can_advance
                },
        };
    }
    let send_blocked = input.is_send_busy
        || input.send_disabled_reason.is_some()
        || input.is_connecting
        || input.is_environment_unavailable;
    let sending = input.is_connecting || input.is_send_busy;
    if input.show_plan_follow_up_prompt && (input.prompt_has_text || !input.can_resume) {
        let label = |idle: &str| if sending { "Sending..." } else { idle }.to_string();
        return if input.prompt_has_text {
            ComposerPrimaryAction::Refine {
                label: label("Refine"),
                disabled: send_blocked,
            }
        } else {
            ComposerPrimaryAction::Implement {
                label: label("Implement"),
                disabled: send_blocked,
                new_thread_label: IMPLEMENT_IN_NEW_THREAD_LABEL.into(),
            }
        };
    }
    if input.can_interrupt && !input.has_sendable_content && !input.is_editing_queued_message {
        return ComposerPrimaryAction::Stop {
            label: STOP_LABEL.into(),
            tooltip: STOP_TOOLTIP.into(),
        };
    }
    let editing = input.is_editing_queued_message;
    let queuing = !editing
        && resolve_composer_dispatch_mode(
            input.is_running,
            input.alternate_modifier,
            Some(input.follow_up),
        ) == ComposerDispatchMode::Queue;
    let resumes = input.can_resume && !input.has_sendable_content && !editing;
    let label = if resumes {
        "Resume thread"
    } else if editing {
        "Update queued message"
    } else if queuing {
        "Queue message"
    } else if input.is_running {
        "Steer message"
    } else {
        "Submit message"
    };
    let status = if input.is_environment_unavailable {
        Some("Environment disconnected".to_string())
    } else {
        input.send_disabled_reason.clone().or_else(|| {
            if input.is_connecting {
                Some("Connecting")
            } else if input.is_preparing_worktree {
                Some("Preparing worktree")
            } else if input.is_send_busy {
                Some(if editing {
                    "Updating queued message"
                } else {
                    "Submitting message"
                })
            } else {
                None
            }
            .map(Into::into)
        })
    };
    let tooltip = status.clone().unwrap_or_else(|| {
        if input.is_running && !editing {
            let shortcut = input
                .alternate_shortcut_label
                .as_deref()
                .map(|label| format!(" or {label}"))
                .unwrap_or_default();
            format!(
                "Click to {}, Ctrl/⌘-click{shortcut} to {}",
                behavior_name(input.follow_up),
                behavior_name(alternate_follow_up(Some(input.follow_up)))
            )
        } else {
            label.into()
        }
    });
    let icon = if sending {
        SendIcon::Spinner
    } else if resumes {
        SendIcon::Resume
    } else if editing {
        SendIcon::Check
    } else if input.is_running {
        if queuing {
            SendIcon::Queue
        } else {
            SendIcon::Steer
        }
    } else {
        SendIcon::Send
    };
    ComposerPrimaryAction::Send(SendButton {
        resumes,
        icon,
        label: status.unwrap_or_else(|| label.into()),
        tooltip,
        disabled: send_blocked || (!input.has_sendable_content && !resumes),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum MobileSendIcon {
    ArrowUp,
    Checkmark,
    ListNumber,
    ArrowTurnLeftUp,
}

/// The mobile send button: its label and icon, and what a tap and the
/// long-press (or Command chord) do while a turn runs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct MobileSendPresentation {
    pub label: String,
    pub icon: MobileSendIcon,
    pub action: Option<FollowUpBehavior>,
    pub alternate: Option<FollowUpBehavior>,
    pub offers_follow_up_choice: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MobileSendInput {
    pub editing_queued_message: bool,
    pub running: bool,
    pub can_steer: bool,
    pub follow_up: FollowUpBehavior,
    /// The outbox holds the send back (offline, an earlier queued message, an
    /// upload).
    pub delivery_deferred: bool,
}

/// Steering is offered only when the provider can steer the live turn, so the
/// button never promises what the Host would downgrade.
pub fn mobile_send_presentation(input: MobileSendInput) -> MobileSendPresentation {
    if input.editing_queued_message {
        return MobileSendPresentation {
            label: "Update queued message".into(),
            icon: MobileSendIcon::Checkmark,
            action: None,
            alternate: None,
            offers_follow_up_choice: false,
        };
    }
    if !input.running {
        return MobileSendPresentation {
            label: if input.delivery_deferred {
                "Queue"
            } else {
                "Send"
            }
            .into(),
            icon: MobileSendIcon::ArrowUp,
            action: None,
            alternate: None,
            offers_follow_up_choice: false,
        };
    }
    let action = if input.can_steer {
        input.follow_up
    } else {
        FollowUpBehavior::Queue
    };
    MobileSendPresentation {
        label: match action {
            FollowUpBehavior::Queue => "Queue",
            FollowUpBehavior::Steer => "Steer",
            FollowUpBehavior::Restart => "Restart",
        }
        .into(),
        icon: if action == FollowUpBehavior::Steer {
            MobileSendIcon::ArrowTurnLeftUp
        } else {
            MobileSendIcon::ListNumber
        },
        action: Some(action),
        alternate: input.can_steer.then(|| alternate_follow_up(Some(action))),
        offers_follow_up_choice: input.can_steer,
    }
}

/// Stop replaces send on mobile only while there is nothing to send and no
/// queued message is being edited.
pub fn mobile_shows_stop(has_content: bool, can_stop_thread: bool, editing_queued: bool) -> bool {
    !has_content && can_stop_thread && !editing_queued
}

#[cfg(test)]
mod tests;

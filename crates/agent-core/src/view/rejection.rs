//! What the user reads when the Host rejects a command. The Host replies with
//! a reason code (`thread-archived`) or, for a few refusals, with prose that is
//! already written for the user.
use agent_domain::Driver;

const FALLBACK: &str = "Failed to dispatch the command.";

fn provider_name(driver: Driver) -> &'static str {
    match driver {
        Driver::Codex => "Codex",
        Driver::Claude => "Claude",
    }
}

/// Capability refusals name the provider that cannot do the requested thing.
fn capability_message(reason: &str, provider: &str) -> Option<String> {
    match reason {
        "restart-unsupported" => Some(format!(
            "{provider} cannot redirect an active run. Stop it first, then send the message."
        )),
        _ => None,
    }
}

fn reason_message(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "thread-not-found" => "No orchestration projection exists for this thread.",
        "thread-deleted" => "This thread is deleted.",
        "thread-archived" => "This thread is archived.",
        "thread-already-archived" => "This thread is already archived.",
        "thread-not-archived" => "This thread is not archived.",
        "thread-already-exists" => "This thread already exists.",
        "thread-has-activity" => "This imported thread already contains non-imported activity.",
        "thread-has-active-work" => "This thread has active or blocked work and cannot be settled.",
        "thread-not-pinned" => "This thread is not pinned and cannot be reordered.",
        "thread-not-active" => "This thread is not active.",
        "thread-not-empty" => "This thread is no longer empty.",
        "thread-changed" => "This thread changed before automatic settlement.",
        "thread-in-another-project" => "The target thread belongs to another project.",
        "title-required" => "Thread title cannot be empty.",
        "snooze-must-be-future" => "The snooze wake time is not in the future.",
        "pending-work-cannot-snooze" => {
            "This thread has a pending approval, user-input request or queued run and cannot be snoozed."
        }
        "no-completed-run" => "This thread has no completed run to mark unread.",
        "invalid-visit-time" => "The visit time is not a valid timestamp.",
        "provider-session-not-found" => "The provider session does not belong to this thread.",
        "worktree-changed" => {
            "This thread's worktree changed before the metadata update could be applied."
        }
        "invalid-workspace" => "The branch or worktree path is not valid.",
        "invalid-limit-recovery" => "A recovery update must include autoResume or snooze.",
        "limit-reset-passed" => "The reset time has passed. Retry the thread manually.",
        "limit-changed" => "The provider limit changed before recovery could be configured.",
        "message-id-conflict" => "This message was already sent.",
        "command-id-conflict" => "This command was already handled and cannot be replayed.",
        "invalid-message-context" => "The message context is not valid.",
        "empty-message" => "A queued run cannot be edited to an empty message.",
        "plan-not-found" => "The proposed plan does not exist.",
        "plan-in-another-project" => "The proposed plan belongs to a different project.",
        "plan-not-active" => "The proposed plan is not active.",
        "continuation-unavailable" => "This thread can no longer be resumed from that run.",
        "merge-back-pending" => {
            "This thread has a pending merge-back transfer; queued merge-back consumption is not implemented yet."
        }
        "merge-backs-from-multiple-forks" => {
            "This thread has pending merge-back transfers from multiple forks."
        }
        "no-running-provider-turn" => "No running provider turn found for the active run.",
        "maintenance-must-run-separately" => {
            "Context compaction and signing out must run as a separate turn. Queue it or wait for the active turn to finish."
        }
        "maintenance-in-progress" => {
            "Wait for context compaction or sign-out to finish before steering the thread."
        }
        "rollback-pending" => "Wait for the rollback to finish.",
        "run-not-found" => "The run was not found.",
        "run-not-preparing" => "This run is not awaiting workspace preparation.",
        "run-not-retryable" => "This run has no failed workspace preparation to retry.",
        "run-not-active" => "This run is not interruptible.",
        "usage-limited" => "Continue the limited thread before resuming its queue.",
        "queued-run-not-found" => "This run is not queued.",
        "automatic-delivery-not-reorderable" => {
            "Automatic completion deliveries cannot be reordered."
        }
        "cannot-reorder-ahead-of-automatic-delivery" => {
            "Queued messages cannot be reordered ahead of automatic completion delivery."
        }
        "automatic-delivery-not-editable" => "Automatic completion deliveries cannot be edited.",
        "automatic-delivery-not-promotable" => {
            "Automatic completion deliveries cannot be promoted to Steer."
        }
        "request-not-ready" => "This runtime request was not found or is no longer pending.",
        "invalid-approval-decision" => "This request requires an approval decision.",
        "missing-question-answer" => "Answer each question before sending.",
        "request-not-resumable" => "This request can no longer be answered.",
        "question-not-found" => "The question for this request was not found.",
        "question-already-answered" => "This question has already been answered.",
        "question-needs-answer" => "This question needs an answer. Answer it or stop the turn.",
        "provider-work-active" => "Cannot roll back while a provider turn is active.",
        "no-active-provider-thread" => "No active provider thread exists for rollback.",
        "checkpoint-not-found" => "This checkpoint was not found.",
        "checkpoint-not-ready" => "This checkpoint is not ready and cannot be restored.",
        "rollback-provider-turn-unavailable" => {
            "Cannot roll back to this checkpoint: its provider turn is unavailable."
        }
        "rollback-provider-thread-mismatch" => {
            "Cannot roll back to this checkpoint: its provider turn belongs to another provider thread."
        }
        "no-stable-source-run" => "No stable source run was found.",
        "fork-source-not-ready" => "In-progress and rolled-back runs cannot be forked.",
        "not-a-fork-of-target" => "This thread is not a fork of the target thread.",
        "merge-back-source-not-finished" => {
            "The merge-back source run has not finished; only provider-finished runs are supported."
        }
        "no-fork-transfer" => "No fork transfer exists between these threads.",
        "no-active-run" => "The parent run is not active.",
        "no-active-attempt" => "The parent run has no active attempt.",
        "task-already-exists" => "This delegated task already exists.",
        "task-required" => "The delegated task cannot be empty.",
        "task-not-found" => "This delegated task is not an app-owned task of this thread.",
        "invalid-completion-cohort" => "Delegated completion delivery is no longer dispatchable.",
        "invalid-terminal-status" => "The provider reported a turn end that is not final.",
        "incomplete-checkpoint-baseline" => {
            "The checkpoint is missing the baseline of an earlier run."
        }
        "invalid-checkpoint-baseline" => "The checkpoint baseline is not valid.",
        "invalid-checkpoint-scope" => "The checkpoint workspace is not valid.",
        "too-many-attachments" => {
            "You can attach up to 100 files per message or question response."
        }
        "duplicate-attachment-id" => "Duplicate attachment ids are not allowed.",
        "invalid-attachment" => "This attachment cannot be sent.",
        "image-too-large" => "This image exceeds the 10 MB attachment limit.",
        "file-too-large" => "This file exceeds the 50 MB attachment limit.",
        "total-images-too-large" => {
            "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once."
        }
        "attachment-unavailable" => "This attachment is no longer available. Attach it again.",
        "internal-command" => "This command can only come from the Host.",
        "thread-mismatch" => "The command names a different thread.",
        "native-session-owned" => "This session is already bound to another thread.",
        _ => return None,
    })
}

/// The Host's rejection as the user reads it. Prose reasons pass through.
pub fn rejection_message(reason: &str) -> String {
    message(reason, "The provider")
}

/// Like [`rejection_message`], naming the provider of the run the command
/// targeted in capability refusals.
pub fn provider_rejection_message(reason: &str, driver: Driver) -> String {
    message(reason, provider_name(driver))
}

fn message(reason: &str, provider: &str) -> String {
    let reason = reason.trim();
    if let Some(message) = capability_message(reason, provider) {
        return message;
    }
    if let Some(message) = reason_message(reason) {
        return message.into();
    }
    if reason.contains(char::is_whitespace) {
        return reason.into();
    }
    FALLBACK.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// The Host forwards the deepest domain cause as prose; it reaches the user
    /// unchanged.
    #[test]
    fn returns_the_deepest_actionable_domain_error_instead_of_generic_dispatch_wrappers() {
        let cause = "claudeAgent cannot satisfy rollback for command command-1: provider conversation rollback is unavailable";
        assert_eq!(rejection_message(cause), cause);
    }

    #[test]
    fn translates_policy_capability_rejections_into_provider_named_prose() {
        assert_eq!(
            provider_rejection_message("restart-unsupported", Driver::Claude),
            "Claude cannot redirect an active run. Stop it first, then send the message."
        );
        assert_eq!(
            provider_rejection_message("restart-unsupported", Driver::Codex),
            "Codex cannot redirect an active run. Stop it first, then send the message."
        );
        assert_eq!(
            rejection_message("restart-unsupported"),
            "The provider cannot redirect an active run. Stop it first, then send the message."
        );
    }

    #[test]
    fn uses_explicit_detail_fields_as_user_facing_messages() {
        assert_eq!(
            rejection_message(" Claude provider thread provider-thread-1 has no live query. "),
            "Claude provider thread provider-thread-1 has no live query."
        );
        assert_eq!(
            rejection_message(
                "File restore requires an isolated worktree. This workspace may contain changes from another thread. Rewind the conversation without restoring files instead."
            ),
            "File restore requires an isolated worktree. This workspace may contain changes from another thread. Rewind the conversation without restoring files instead."
        );
    }

    #[rstest]
    #[case("unknown-reason")]
    #[case("")]
    #[case("   ")]
    fn unknown_codes_fall_back_to_a_generic_failure(#[case] reason: &str) {
        assert_eq!(rejection_message(reason), FALLBACK);
    }

    /// Every reason the thread machine, the thread actor and attachment
    /// validation can reply with.
    #[rstest]
    #[case(
        "thread-not-found",
        "No orchestration projection exists for this thread."
    )]
    #[case("thread-deleted", "This thread is deleted.")]
    #[case("thread-archived", "This thread is archived.")]
    #[case("thread-already-archived", "This thread is already archived.")]
    #[case("thread-not-archived", "This thread is not archived.")]
    #[case("thread-already-exists", "This thread already exists.")]
    #[case(
        "thread-has-activity",
        "This imported thread already contains non-imported activity."
    )]
    #[case(
        "thread-has-active-work",
        "This thread has active or blocked work and cannot be settled."
    )]
    #[case(
        "thread-not-pinned",
        "This thread is not pinned and cannot be reordered."
    )]
    #[case("thread-not-active", "This thread is not active.")]
    #[case("thread-not-empty", "This thread is no longer empty.")]
    #[case("thread-changed", "This thread changed before automatic settlement.")]
    #[case(
        "thread-in-another-project",
        "The target thread belongs to another project."
    )]
    #[case("title-required", "Thread title cannot be empty.")]
    #[case("snooze-must-be-future", "The snooze wake time is not in the future.")]
    #[case(
        "pending-work-cannot-snooze",
        "This thread has a pending approval, user-input request or queued run and cannot be snoozed."
    )]
    #[case("no-completed-run", "This thread has no completed run to mark unread.")]
    #[case("invalid-visit-time", "The visit time is not a valid timestamp.")]
    #[case(
        "provider-session-not-found",
        "The provider session does not belong to this thread."
    )]
    #[case(
        "worktree-changed",
        "This thread's worktree changed before the metadata update could be applied."
    )]
    #[case("invalid-workspace", "The branch or worktree path is not valid.")]
    #[case(
        "invalid-limit-recovery",
        "A recovery update must include autoResume or snooze."
    )]
    #[case(
        "limit-reset-passed",
        "The reset time has passed. Retry the thread manually."
    )]
    #[case(
        "limit-changed",
        "The provider limit changed before recovery could be configured."
    )]
    #[case("message-id-conflict", "This message was already sent.")]
    #[case(
        "command-id-conflict",
        "This command was already handled and cannot be replayed."
    )]
    #[case("invalid-message-context", "The message context is not valid.")]
    #[case("empty-message", "A queued run cannot be edited to an empty message.")]
    #[case("plan-not-found", "The proposed plan does not exist.")]
    #[case(
        "plan-in-another-project",
        "The proposed plan belongs to a different project."
    )]
    #[case("plan-not-active", "The proposed plan is not active.")]
    #[case(
        "continuation-unavailable",
        "This thread can no longer be resumed from that run."
    )]
    #[case(
        "merge-back-pending",
        "This thread has a pending merge-back transfer; queued merge-back consumption is not implemented yet."
    )]
    #[case(
        "merge-backs-from-multiple-forks",
        "This thread has pending merge-back transfers from multiple forks."
    )]
    #[case(
        "no-running-provider-turn",
        "No running provider turn found for the active run."
    )]
    #[case(
        "maintenance-must-run-separately",
        "Context compaction and signing out must run as a separate turn. Queue it or wait for the active turn to finish."
    )]
    #[case(
        "maintenance-in-progress",
        "Wait for context compaction or sign-out to finish before steering the thread."
    )]
    #[case(
        "restart-unsupported",
        "The provider cannot redirect an active run. Stop it first, then send the message."
    )]
    #[case("rollback-pending", "Wait for the rollback to finish.")]
    #[case("run-not-found", "The run was not found.")]
    #[case("run-not-preparing", "This run is not awaiting workspace preparation.")]
    #[case(
        "run-not-retryable",
        "This run has no failed workspace preparation to retry."
    )]
    #[case("run-not-active", "This run is not interruptible.")]
    #[case(
        "usage-limited",
        "Continue the limited thread before resuming its queue."
    )]
    #[case("queued-run-not-found", "This run is not queued.")]
    #[case(
        "automatic-delivery-not-reorderable",
        "Automatic completion deliveries cannot be reordered."
    )]
    #[case(
        "cannot-reorder-ahead-of-automatic-delivery",
        "Queued messages cannot be reordered ahead of automatic completion delivery."
    )]
    #[case(
        "automatic-delivery-not-editable",
        "Automatic completion deliveries cannot be edited."
    )]
    #[case(
        "automatic-delivery-not-promotable",
        "Automatic completion deliveries cannot be promoted to Steer."
    )]
    #[case(
        "request-not-ready",
        "This runtime request was not found or is no longer pending."
    )]
    #[case(
        "invalid-approval-decision",
        "This request requires an approval decision."
    )]
    #[case("missing-question-answer", "Answer each question before sending.")]
    #[case("request-not-resumable", "This request can no longer be answered.")]
    #[case("question-not-found", "The question for this request was not found.")]
    #[case(
        "question-already-answered",
        "This question has already been answered."
    )]
    #[case(
        "question-needs-answer",
        "This question needs an answer. Answer it or stop the turn."
    )]
    #[case(
        "provider-work-active",
        "Cannot roll back while a provider turn is active."
    )]
    #[case(
        "no-active-provider-thread",
        "No active provider thread exists for rollback."
    )]
    #[case("checkpoint-not-found", "This checkpoint was not found.")]
    #[case(
        "checkpoint-not-ready",
        "This checkpoint is not ready and cannot be restored."
    )]
    #[case(
        "rollback-provider-turn-unavailable",
        "Cannot roll back to this checkpoint: its provider turn is unavailable."
    )]
    #[case(
        "rollback-provider-thread-mismatch",
        "Cannot roll back to this checkpoint: its provider turn belongs to another provider thread."
    )]
    #[case("no-stable-source-run", "No stable source run was found.")]
    #[case(
        "fork-source-not-ready",
        "In-progress and rolled-back runs cannot be forked."
    )]
    #[case(
        "not-a-fork-of-target",
        "This thread is not a fork of the target thread."
    )]
    #[case(
        "merge-back-source-not-finished",
        "The merge-back source run has not finished; only provider-finished runs are supported."
    )]
    #[case("no-fork-transfer", "No fork transfer exists between these threads.")]
    #[case("no-active-run", "The parent run is not active.")]
    #[case("no-active-attempt", "The parent run has no active attempt.")]
    #[case("task-already-exists", "This delegated task already exists.")]
    #[case("task-required", "The delegated task cannot be empty.")]
    #[case(
        "task-not-found",
        "This delegated task is not an app-owned task of this thread."
    )]
    #[case(
        "invalid-completion-cohort",
        "Delegated completion delivery is no longer dispatchable."
    )]
    #[case(
        "invalid-terminal-status",
        "The provider reported a turn end that is not final."
    )]
    #[case(
        "incomplete-checkpoint-baseline",
        "The checkpoint is missing the baseline of an earlier run."
    )]
    #[case("invalid-checkpoint-baseline", "The checkpoint baseline is not valid.")]
    #[case("invalid-checkpoint-scope", "The checkpoint workspace is not valid.")]
    #[case(
        "too-many-attachments",
        "You can attach up to 100 files per message or question response."
    )]
    #[case("duplicate-attachment-id", "Duplicate attachment ids are not allowed.")]
    #[case("invalid-attachment", "This attachment cannot be sent.")]
    #[case("image-too-large", "This image exceeds the 10 MB attachment limit.")]
    #[case("file-too-large", "This file exceeds the 50 MB attachment limit.")]
    #[case(
        "total-images-too-large",
        "Images can total up to 80 MiB per message or question response. Use smaller images or send fewer at once."
    )]
    #[case(
        "attachment-unavailable",
        "This attachment is no longer available. Attach it again."
    )]
    #[case("internal-command", "This command can only come from the Host.")]
    #[case("thread-mismatch", "The command names a different thread.")]
    #[case(
        "native-session-owned",
        "This session is already bound to another thread."
    )]
    #[case(
        "This subagent is run by its provider and cannot take messages. Message the parent thread instead.",
        "This subagent is run by its provider and cannot take messages. Message the parent thread instead."
    )]
    fn words_every_host_rejection(#[case] reason: &str, #[case] expected: &str) {
        assert_eq!(rejection_message(reason), expected);
    }
}

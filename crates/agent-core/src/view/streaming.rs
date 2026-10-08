//! Publication boundaries for the Host's response-streaming preference.
use agent_domain::{RunStatus, State};
use agent_protocol::models::ResponseStreamingMode;

/// Whether a folded thread state is ready to reach native clients.
/// Providers still stream and the owner still folds every fact; this only
/// controls when a client snapshot is published.
pub fn should_publish(mode: ResponseStreamingMode, state: Option<&State>) -> bool {
    let Some(state) = state else {
        return true;
    };
    let Some(run) = state.active_run() else {
        return true;
    };
    if run.status.terminal() {
        return true;
    }
    if run.status == RunStatus::Waiting {
        return true;
    }
    let Some(message) = state
        .messages
        .iter()
        .rev()
        .find(|message| message.role == agent_domain::Role::Assistant)
    else {
        return false;
    };
    if !message.streaming {
        return true;
    }
    match mode {
        ResponseStreamingMode::Turn => false,
        ResponseStreamingMode::Paragraph => paragraph_boundary(&message.text),
    }
}

fn paragraph_boundary(text: &str) -> bool {
    // Keep paragraph separators while ignoring only horizontal/trailing
    // whitespace after the separator.
    let trimmed = text.trim_end_matches([' ', '\t', '\r']);
    trimmed.ends_with("\n\n")
        || (trimmed.ends_with('`')
            && trimmed.matches("```").count() >= 2
            && trimmed.matches("```").count().is_multiple_of(2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{
        Driver, InputIntent, Message, MessageAuthor, MessageId, ModelSelection, Role, Run, RunId,
        RuntimeMode, Thread, ThreadId, Timestamp,
    };
    use std::collections::BTreeMap;

    fn state(text: &str, streaming: bool, status: RunStatus) -> State {
        let thread = Thread {
            id: ThreadId::new("thread:streaming").unwrap(),
            project: "project".into(),
            title: "title".into(),
            selection: ModelSelection {
                instance: "codex".into(),
                driver: Driver::Codex,
                model: "model".into(),
                options: BTreeMap::new(),
            },
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: agent_domain::InteractionMode::Default,
            created_at: Timestamp::from_millis(0).unwrap(),
            updated_at: Timestamp::from_millis(0).unwrap(),
            archived_at: None,
            deleted_at: None,
            settled: None,
            settled_at: None,
            unsettled_at: None,
            snoozed_until: None,
            pinned_at: None,
            pin_order: None,
            active_order: None,
            last_visited_at: None,
            auto_settle: false,
            parent: None,
            fork_boundary: None,
            workspace: None,
            title_request: None,
            imported: false,
            created_by: MessageAuthor::User,
            creation_source: "test".into(),
            snoozed_at: None,
            limit_recovery: None,
            linked_pull_request: None,
        };
        State {
            thread: Some(thread),
            runs: vec![Run {
                restart_of: None,
                restart_cancelled_work: vec![],
                checkpoint_scope: None,
                native_baseline_heads: BTreeMap::new(),
                id: RunId::new("run:streaming").unwrap(),
                ordinal: 1,
                message: MessageId::new("message:streaming").unwrap(),
                selection: ModelSelection {
                    instance: "codex".into(),
                    driver: Driver::Codex,
                    model: "model".into(),
                    options: BTreeMap::new(),
                },
                status,
                attempt: None,
                queue_position: None,
                queue_held: false,
                requested_at: Timestamp::from_millis(0).unwrap(),
                started_at: Some(Timestamp::from_millis(0).unwrap()),
                completed_at: None,
                source_plan: None,
                checkpoint: None,
                continuation: false,
            }],
            messages: vec![Message {
                notification: None,
                id: MessageId::new("message:assistant").unwrap(),
                run: Some(RunId::new("run:streaming").unwrap()),
                role: Role::Assistant,
                text: text.into(),
                attachments: vec![],
                intent: InputIntent::TurnStart,
                streaming,
                created_by: MessageAuthor::Agent,
                creation_source: "test".into(),
                created_at: Timestamp::from_millis(0).unwrap(),
                updated_at: Timestamp::from_millis(0).unwrap(),
                context: None,
                scheduled_task: None,
            }],
            ..State::default()
        }
    }

    #[test]
    fn turn_mode_waits_for_completion() {
        assert!(!should_publish(
            ResponseStreamingMode::Turn,
            Some(&state("partial", true, RunStatus::Running))
        ));
        assert!(should_publish(
            ResponseStreamingMode::Turn,
            Some(&state("approval requested", true, RunStatus::Waiting))
        ));
        assert!(should_publish(
            ResponseStreamingMode::Turn,
            Some(&state("done", false, RunStatus::Completed))
        ));
    }

    #[test]
    fn paragraph_mode_flushes_only_at_boundaries() {
        assert!(!should_publish(
            ResponseStreamingMode::Paragraph,
            Some(&state("partial", true, RunStatus::Running))
        ));
        assert!(!should_publish(
            ResponseStreamingMode::Paragraph,
            Some(&state("inline `", true, RunStatus::Running))
        ));
        assert!(should_publish(
            ResponseStreamingMode::Paragraph,
            Some(&state("first paragraph\n\n", true, RunStatus::Running))
        ));
        assert!(should_publish(
            ResponseStreamingMode::Paragraph,
            Some(&state(
                "```rust\nlet value = 1;\n```",
                true,
                RunStatus::Running
            ))
        ));
    }
}

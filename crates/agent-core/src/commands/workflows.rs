//! What the queue, stop, merge-back and fork controls act on, derived from the
//! folded thread state.
use agent_domain::{
    Attachment, AttemptStatus, BackgroundKind, Item, ItemKind, MessageContext, MessageId, Run,
    RunId, RunStatus, State, ThreadId, ThreadShell, TurnSupport,
};

/// The newest preparing, starting, running or waiting run.
pub fn active_run(state: &State) -> Option<&Run> {
    state.active_run()
}

/// The newest run the provider finished (`waiting` while its checkpoint is
/// captured), unless a newer run is preparing, starting or running.
pub fn latest_merge_back_run(state: &State) -> Option<&Run> {
    let latest = state
        .runs
        .iter()
        .filter(|run| matches!(run.status, RunStatus::Waiting | RunStatus::Completed))
        .max_by_key(|run| run.ordinal)?;
    let newer_active = state.runs.iter().any(|run| {
        run.ordinal > latest.ordinal
            && matches!(
                run.status,
                RunStatus::Preparing | RunStatus::Starting | RunStatus::Running
            )
    });
    (!newer_active).then_some(latest)
}

/// Queued runs the user sent; automatic completion and notification deliveries
/// are not part of the user's queue.
pub fn user_queued_runs(state: &State) -> Vec<&Run> {
    state
        .runs
        .iter()
        .filter(|run| run.status == RunStatus::Queued)
        .filter(|run| {
            state
                .message(&run.message)
                .is_none_or(|message| message.notification.is_none())
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueuedRun {
    pub run: RunId,
    pub message: MessageId,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub context: Option<MessageContext>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueWorkflow {
    pub active_run: Option<RunId>,
    pub queued: Vec<QueuedRun>,
    pub held: bool,
    pub can_reorder: bool,
    pub can_promote_to_steer: bool,
}

fn support(state: &State) -> Option<TurnSupport> {
    let driver = active_run(state)
        .map(|run| run.selection.driver)
        .or_else(|| state.thread.as_ref().map(|t| t.selection.driver))?;
    Some(TurnSupport::for_driver(driver))
}

pub fn queue_workflow(state: &State) -> QueueWorkflow {
    let active = active_run(state);
    let support = support(state);
    let steerable = active.is_some_and(|run| {
        run.status == RunStatus::Running
            && run.attempt.as_ref().is_some_and(|attempt| {
                state.attempts.iter().any(|candidate| {
                    &candidate.id == attempt
                        && candidate.accepted
                        && candidate.status == AttemptStatus::Running
                })
            })
    });
    let mut queued = user_queued_runs(state);
    queued.sort_by_key(|run| (run.queue_position.unwrap_or(run.ordinal), run.ordinal));
    QueueWorkflow {
        active_run: active.map(|run| run.id.clone()),
        queued: queued
            .into_iter()
            .map(|run| {
                let message = state.message(&run.message);
                QueuedRun {
                    run: run.id.clone(),
                    message: run.message.clone(),
                    text: message.map_or_else(|| "Queued message".into(), |m| m.text.clone()),
                    attachments: message.map(|m| m.attachments.clone()).unwrap_or_default(),
                    context: message.and_then(|m| m.context.clone()),
                }
            })
            .collect(),
        held: state
            .runs
            .iter()
            .any(|run| run.status == RunStatus::Queued && run.queue_held),
        can_reorder: support.is_some_and(|support| support.queue),
        can_promote_to_steer: steerable
            && support.is_some_and(|support| support.steer || support.restart),
    }
}

/// A completed assistant message of a run can start a fork.
pub fn can_fork_from_item(item: &Item) -> bool {
    matches!(item.kind, ItemKind::AssistantMessage { .. })
        && item.run.is_some()
        && item.status == agent_domain::ItemStatus::Completed
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundTaskKind {
    Command,
    Monitor,
    Subagent,
    BackgroundTask,
}
impl From<BackgroundKind> for BackgroundTaskKind {
    fn from(kind: BackgroundKind) -> Self {
        match kind {
            BackgroundKind::Command => Self::Command,
            BackgroundKind::Monitor => Self::Monitor,
            BackgroundKind::Subagent => Self::Subagent,
            BackgroundKind::BackgroundTask => Self::BackgroundTask,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingBackgroundTask {
    pub task_id: String,
    pub kind: BackgroundTaskKind,
    pub description: Option<String>,
    pub child_thread: Option<ThreadId>,
}

fn background_item(item: &Item) -> bool {
    matches!(
        item.kind,
        ItemKind::CommandExecution { .. }
            | ItemKind::DynamicTool { .. }
            | ItemKind::Subagent { .. }
    )
}

/// Background-type items still running outside rolled-back runs. Stop ends
/// exactly these, so the pending list and Stop agree.
pub fn pending_background_items(state: &State) -> Vec<&Item> {
    state
        .items
        .iter()
        .filter(|item| background_item(item) && !item.status.terminal())
        .filter(|item| !matches!(&item.kind, ItemKind::DynamicTool { input, .. } if input.0["persistent"] == true))
        .filter(|item| {
            item.run.as_ref().is_none_or(|run| {
                !state
                    .runs
                    .iter()
                    .any(|candidate| &candidate.id == run && candidate.status == RunStatus::RolledBack)
            })
        })
        .collect()
}

fn trimmed(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// Work a settled latest run left behind: the native background roster and
/// background-type items still running, once per native task.
pub fn pending_background_work(state: &State) -> Vec<PendingBackgroundTask> {
    if state.runs.iter().any(|run| {
        matches!(
            run.status,
            RunStatus::Preparing | RunStatus::Starting | RunStatus::Running
        )
    }) {
        return vec![];
    }
    let settled = state
        .runs
        .iter()
        .max_by_key(|run| run.ordinal)
        .is_some_and(|run| {
            matches!(
                run.status,
                RunStatus::Cancelled
                    | RunStatus::Completed
                    | RunStatus::Failed
                    | RunStatus::Interrupted
                    | RunStatus::Waiting
            )
        });
    if !settled {
        return vec![];
    }
    let mut tasks: Vec<PendingBackgroundTask> = vec![];
    for work in state.background_work.values() {
        if work.key.is_empty() || tasks.iter().any(|task| task.task_id == work.key) {
            continue;
        }
        tasks.push(PendingBackgroundTask {
            task_id: work.key.clone(),
            kind: work.kind.into(),
            description: trimmed(&work.description),
            child_thread: None,
        });
    }
    for item in pending_background_items(state) {
        let task_id = if item.native_key.is_empty() {
            item.id.to_string()
        } else {
            item.native_key.clone()
        };
        if tasks.iter().any(|task| task.task_id == task_id) {
            continue;
        }
        let (kind, description, child_thread) = match &item.kind {
            ItemKind::CommandExecution { command, title, .. } => (
                BackgroundTaskKind::Command,
                title
                    .as_deref()
                    .and_then(trimmed)
                    .or_else(|| trimmed(command)),
                None,
            ),
            ItemKind::DynamicTool { name, .. } => {
                (BackgroundTaskKind::BackgroundTask, trimmed(name), None)
            }
            ItemKind::Subagent { task } => {
                let task = state.tasks.iter().find(|t| &t.id == task);
                (
                    BackgroundTaskKind::Subagent,
                    task.and_then(|t| trimmed(&t.prompt)),
                    task.map(|t| t.child_thread.clone()),
                )
            }
            _ => continue,
        };
        tasks.push(PendingBackgroundTask {
            task_id,
            kind,
            description,
            child_thread,
        });
    }
    tasks
}

/// The run Stop interrupts: the newest blocking run, or else the latest run
/// when it left background work behind.
pub fn interrupt_target(state: &State) -> Option<RunId> {
    if let Some(run) = active_run(state) {
        return Some(run.id.clone());
    }
    (!pending_background_work(state).is_empty())
        .then(|| state.runs.iter().max_by_key(|run| run.ordinal))
        .flatten()
        .map(|run| run.id.clone())
}

/// The pinned block: arranged keys first by key then id, then keyless threads
/// newest-created first.
pub fn sort_pinned_by_order(threads: &mut [&ThreadShell]) {
    threads.sort_by(|left, right| match (&left.pin_order, &right.pin_order) {
        (Some(a), Some(b)) => a.cmp(b).then_with(|| left.id.cmp(&right.id)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.id.cmp(&right.id)),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::fixtures::*;
    use agent_domain::{Attempt, AttemptStatus, BackgroundWork, Message, MessageAuthor, Role};

    fn message(id: &str, text: &str) -> Message {
        Message {
            scheduled_task: None,
            notification: None,
            id: MessageId::new(id).unwrap(),
            run: None,
            role: Role::User,
            text: text.into(),
            attachments: vec![],
            intent: agent_domain::InputIntent::QueuedTurn,
            streaming: false,
            created_by: MessageAuthor::User,
            creation_source: "desktop".into(),
            created_at: at(),
            updated_at: at(),
            context: None,
        }
    }
    fn queued(id: &str, ordinal: u64, position: Option<u64>, message: &str) -> Run {
        let mut run = run(id, ordinal, RunStatus::Queued);
        run.queue_position = position;
        run.message = MessageId::new(message).unwrap();
        run
    }
    fn attempt(id: &str, run: &str, accepted: bool, status: AttemptStatus) -> Attempt {
        Attempt {
            id: agent_domain::RunAttemptId::new(id).unwrap(),
            run: RunId::new(run).unwrap(),
            ordinal: 1,
            status,
            native_thread: None,
            native_turn: None,
            native_head: None,
            accepted,
            usage: None,
            context_usage: None,
            turn_usage: None,
            usage_accumulator: None,
            usage_observed: false,
            rejected_limits: Default::default(),
            started_at: at(),
            completed_at: None,
        }
    }
    fn active(status: RunStatus, accepted: bool) -> State {
        let mut state = thread_state("Thread");
        let mut active = run("active", 1, status);
        active.attempt = Some(agent_domain::RunAttemptId::new("attempt-active").unwrap());
        state.runs.push(active);
        state.attempts.push(attempt(
            "attempt-active",
            "active",
            accepted,
            if accepted {
                AttemptStatus::Running
            } else {
                AttemptStatus::Pending
            },
        ));
        state
    }
    fn ids(workflow: &QueueWorkflow) -> Vec<(String, String)> {
        workflow
            .queued
            .iter()
            .map(|q| (q.run.to_string(), q.text.clone()))
            .collect()
    }

    #[test]
    fn sorts_queued_messages_and_gates_reorder_and_promotion() {
        let mut state = active(RunStatus::Running, true);
        state.runs.push(queued("later", 3, None, "message-later"));
        state
            .runs
            .push(queued("first", 2, Some(1), "message-first"));
        let mut first = message("message-first", "First");
        first.attachments.push(Attachment {
            kind: agent_domain::AttachmentKind::Image,
            source: None,
            id: "attachment-first".into(),
            name: "first.png".into(),
            mime_type: "image/png".into(),
            path: String::new(),
            size: 64,
        });
        state
            .messages
            .extend([first, message("message-later", "Later")]);
        let workflow = queue_workflow(&state);
        assert_eq!(
            ids(&workflow),
            [
                ("first".into(), "First".into()),
                ("later".into(), "Later".into())
            ]
        );
        let attachments: Vec<Vec<String>> = workflow
            .queued
            .iter()
            .map(|q| q.attachments.iter().map(|a| a.id.clone()).collect())
            .collect();
        assert_eq!(attachments, [vec!["attachment-first".to_string()], vec![]]);
        assert_eq!(workflow.active_run.unwrap().as_str(), "active");
        assert!(workflow.can_reorder);
        assert!(workflow.can_promote_to_steer);
    }

    #[test]
    fn keeps_held_messages_visible_and_clears_the_hold_when_they_leave_the_queue() {
        for status in [RunStatus::Queued, RunStatus::Cancelled, RunStatus::Starting] {
            let mut state = thread_state("Thread");
            let mut held = run("held", 1, status);
            held.queue_held = true;
            held.message = MessageId::new("message").unwrap();
            state.runs.push(held);
            state.messages.push(message("message", "Saved message"));
            let workflow = queue_workflow(&state);
            assert_eq!(workflow.held, status == RunStatus::Queued);
            let texts: Vec<_> = workflow.queued.iter().map(|q| q.text.clone()).collect();
            assert_eq!(
                texts,
                if status == RunStatus::Queued {
                    vec!["Saved message".to_string()]
                } else {
                    vec![]
                }
            );
        }
    }

    #[test]
    fn hides_automatic_completion_delivery_from_the_visible_queue() {
        let mut state = thread_state("Thread");
        state.runs.extend([
            queued("provider-wake", 4, Some(3), "provider-wake"),
            queued("automatic", 2, Some(1), "message-automatic"),
            queued("visible", 3, Some(2), "message-visible"),
        ]);
        let notification = |source| agent_domain::Notification {
            source,
            child_thread: None,
            outcome: agent_domain::NotificationOutcome::Updated,
            summary: "Monitor updated".into(),
            detail: None,
        };
        let mut wake = message("provider-wake", "Model-facing text");
        wake.notification = Some(notification(agent_domain::NotificationSource::Native(
            BackgroundKind::Monitor,
        )));
        let mut automatic = message(
            "message-automatic",
            "A delegated task reached a terminal state.",
        );
        automatic.notification = Some(notification(agent_domain::NotificationSource::Delegated {
            task_ids: vec![agent_domain::NodeId::new("task:child").unwrap()],
        }));
        state.messages.extend([
            wake,
            automatic,
            message("message-visible", "Visible queued message"),
        ]);
        assert_eq!(
            ids(&queue_workflow(&state)),
            [("visible".into(), "Visible queued message".into())]
        );
    }

    #[test]
    fn removes_only_the_promoted_head_from_the_visible_queue() {
        let mut state = thread_state("Thread");
        let mut promoted = run("promoted", 2, RunStatus::Starting);
        promoted.message = MessageId::new("message-promoted").unwrap();
        state.runs.push(promoted);
        state
            .runs
            .push(queued("still-queued", 3, Some(2), "message-still-queued"));
        state.messages.extend([
            message("message-promoted", "Run now"),
            message("message-still-queued", "Wait longer"),
        ]);
        let workflow = queue_workflow(&state);
        assert_eq!(workflow.active_run.unwrap().as_str(), "promoted");
        assert_eq!(
            ids(&queue_workflow(&state)),
            [("still-queued".into(), "Wait longer".into())]
        );
    }

    #[test]
    fn does_not_promote_queued_work_into_a_preparing_starting_or_waiting_run() {
        for status in [
            RunStatus::Preparing,
            RunStatus::Starting,
            RunStatus::Waiting,
        ] {
            let mut state = active(status, status == RunStatus::Waiting);
            state.runs.push(queued("queued", 2, None, "message"));
            assert!(!queue_workflow(&state).can_promote_to_steer);
        }
    }

    #[test]
    fn does_not_promote_queued_work_until_the_provider_accepts_the_turn() {
        let mut state = active(RunStatus::Running, false);
        state.runs.push(queued("queued", 2, None, "message"));
        assert!(!queue_workflow(&state).can_promote_to_steer);
    }

    #[test]
    fn allows_forks_only_from_completed_assistant_messages_of_a_run() {
        let mut item = command_item("assistant", 1);
        item.kind = ItemKind::AssistantMessage {
            message: MessageId::new("message").unwrap(),
        };
        item.run = Some(RunId::new("run").unwrap());
        assert!(can_fork_from_item(&item));
        item.status = agent_domain::ItemStatus::Running;
        assert!(!can_fork_from_item(&item));
    }

    #[test]
    fn merges_the_newest_provider_finished_run_while_its_checkpoint_is_pending() {
        let mut state = thread_state("Thread");
        state.runs.extend([
            run("newest-queued", 3, RunStatus::Queued),
            run("older-completed", 1, RunStatus::Completed),
            run("newest-finished", 2, RunStatus::Waiting),
        ]);
        assert_eq!(
            latest_merge_back_run(&state).unwrap().id.as_str(),
            "newest-finished"
        );
        state.runs.retain(|run| run.id.as_str() != "newest-queued");
        state.runs.reverse();
        assert_eq!(
            latest_merge_back_run(&state).unwrap().id.as_str(),
            "newest-finished"
        );
    }

    #[test]
    fn does_not_merge_older_history_while_a_newer_run_is_active() {
        for status in [
            RunStatus::Preparing,
            RunStatus::Starting,
            RunStatus::Running,
        ] {
            let mut state = thread_state("Thread");
            state.runs.extend([
                run("older-completed", 1, RunStatus::Completed),
                run("newer-active", 2, status),
            ]);
            assert!(latest_merge_back_run(&state).is_none());
        }
    }

    fn background(status: RunStatus) -> State {
        let mut state = thread_state("Thread");
        state.runs.push(run("run-waiting", 1, status));
        let mut item = command_item("background-command", 1);
        item.run = Some(RunId::new("run-waiting").unwrap());
        item.status = agent_domain::ItemStatus::Running;
        item.native_key = String::new();
        item.kind = ItemKind::CommandExecution {
            command: "vp run dev".into(),
            cwd: None,
            exit_code: None,
            title: None,
        };
        state.items.push(item);
        state
    }

    #[test]
    fn stop_targets_runs_with_background_commands_except_rolled_back_ones() {
        for status in [
            RunStatus::Waiting,
            RunStatus::Completed,
            RunStatus::Failed,
            RunStatus::Interrupted,
            RunStatus::Cancelled,
            RunStatus::RolledBack,
        ] {
            let state = background(status);
            let expected = (status != RunStatus::RolledBack).then(|| "run-waiting".to_string());
            let target = interrupt_target(&state).map(|run| run.to_string());
            assert_eq!(target, expected, "{status:?}");
        }
    }

    #[test]
    fn pending_work_names_the_roster_and_running_items_once_per_task() {
        let mut state = background(RunStatus::Completed);
        state.background_work.insert(
            "background-command".into(),
            BackgroundWork {
                key: "background-command".into(),
                tool: "Bash".into(),
                description: " dev server ".into(),
                kind: BackgroundKind::Command,
                attempt: agent_domain::RunAttemptId::new("attempt").unwrap(),
            },
        );
        let work = pending_background_work(&state);
        assert_eq!(
            work,
            [PendingBackgroundTask {
                task_id: "background-command".into(),
                kind: BackgroundTaskKind::Command,
                description: Some("dev server".into()),
                child_thread: None,
            }]
        );
        state.runs.push(run("next", 2, RunStatus::Running));
        assert!(pending_background_work(&state).is_empty());
    }

    #[test]
    fn sorts_pinned_threads_by_key_then_newest_keyless() {
        let row = |id: &str, key: Option<&str>, created: i64| {
            let mut row = agent_domain::shell(&thread_state("Thread")).unwrap();
            row.id = ThreadId::new(id).unwrap();
            row.pin_order = key.map(Into::into);
            row.created_at = agent_domain::Timestamp::from_millis(created).unwrap();
            row
        };
        let rows = [
            row("old-keyless", None, 1),
            row("b", Some("n"), 5),
            row("new-keyless", None, 9),
            row("a", Some("n"), 2),
            row("first", Some("c"), 3),
        ];
        let mut sorted: Vec<_> = rows.iter().collect();
        sort_pinned_by_order(&mut sorted);
        let ids: Vec<_> = sorted.iter().map(|row| row.id.to_string()).collect();
        assert_eq!(ids, ["first", "a", "b", "new-keyless", "old-keyless"]);
    }
}

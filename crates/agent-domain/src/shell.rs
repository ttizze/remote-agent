//! Thread list summaries, derived only from the folded thread state.
use crate::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRequestSummary {
    pub id: RuntimeRequestId,
    pub kind: String,
    pub created_at: Timestamp,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingBackgroundSummary {
    pub key: String,
    pub kind: BackgroundKind,
    pub description: String,
}
/// Message bodies stay in the thread detail; unread state is derived by
/// clients from `last_visited_at` and the latest completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadShell {
    pub id: ThreadId,
    pub project: String,
    pub title: String,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace: Option<Workspace>,
    pub parent: Option<ThreadId>,
    pub fork_boundary: Option<u64>,
    pub imported: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub deleted_at: Option<Timestamp>,
    pub settled: Option<bool>,
    pub settled_at: Option<Timestamp>,
    pub snoozed_until: Option<Timestamp>,
    pub pinned_at: Option<Timestamp>,
    pub pin_order: Option<String>,
    pub active_order: Option<String>,
    pub last_visited_at: Option<Timestamp>,
    pub auto_settle: bool,
    pub title_regenerating: bool,
    pub latest_run: Option<RunId>,
    pub latest_run_requested_at: Option<Timestamp>,
    pub latest_run_started_at: Option<Timestamp>,
    pub latest_run_completed_at: Option<Timestamp>,
    /// The latest run's status; `None` for a thread without runs.
    pub status: Option<RunStatus>,
    pub active_run: Option<RunId>,
    pub activity_run_status: Option<RunStatus>,
    pub activity_run_started_at: Option<Timestamp>,
    pub last_error: Option<String>,
    pub last_error_class: Option<String>,
    pub pending_request: Option<PendingRequestSummary>,
    pub latest_user_message_at: Option<Timestamp>,
    pub latest_user_authored_message_at: Option<Timestamp>,
    pub has_actionable_proposed_plan: bool,
    pub pending_background_work: Vec<PendingBackgroundSummary>,
    pub provider_instance_history: Vec<String>,
    pub item_count: usize,
    pub visible_item_count: usize,
}
/// The run a list row presents: a usage-limit failure that later runs have
/// not replaced, otherwise the highest run that is not held in the queue.
fn presented_run(state: &State) -> Option<&Run> {
    let latest_unheld = state
        .runs
        .iter()
        .filter(|run| !(run.status == RunStatus::Queued && run.queue_held))
        .max_by_key(|run| run.ordinal);
    latest_executed_run(state)
        .filter(|run| {
            run.status == RunStatus::Failed
                && failure_class(state, &run.id).as_deref() == Some("usage_limit")
                && state.runs.iter().any(|later| later.ordinal > run.ordinal)
        })
        .or(latest_unheld)
}
pub fn shell(state: &State) -> Option<ThreadShell> {
    let thread = state.thread.as_ref()?;
    let latest = presented_run(state);
    let highest = |statuses: &[RunStatus]| {
        state
            .runs
            .iter()
            .filter(|run| statuses.contains(&run.status))
            .max_by_key(|run| run.ordinal)
    };
    let active = highest(&[
        RunStatus::Preparing,
        RunStatus::Starting,
        RunStatus::Running,
    ]);
    let activity = highest(&[
        RunStatus::Preparing,
        RunStatus::Starting,
        RunStatus::Running,
        RunStatus::Waiting,
    ]);
    let failure = latest
        .filter(|run| run.status == RunStatus::Failed)
        .and_then(|run| {
            state
                .items
                .iter()
                .filter(|item| {
                    item.run.as_ref() == Some(&run.id) && item.status == ItemStatus::Failed
                })
                .filter_map(|item| match &item.kind {
                    ItemKind::Error { message, class, .. } => {
                        Some((message.clone(), class.clone()))
                    }
                    _ => None,
                })
                .next_back()
        });
    let users = || {
        state
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
    };
    let mut instances: Vec<String> = vec![];
    let mut runs = state.runs.iter().collect::<Vec<_>>();
    runs.sort_by_key(|run| run.ordinal);
    for run in runs {
        if !instances.contains(&run.selection.instance) {
            instances.push(run.selection.instance.clone());
        }
    }
    let visible = state.visible_items();
    Some(ThreadShell {
        id: thread.id.clone(),
        project: thread.project.clone(),
        title: thread.title.clone(),
        selection: thread.selection.clone(),
        runtime_mode: thread.runtime_mode,
        interaction_mode: thread.interaction_mode,
        workspace: thread.workspace.clone(),
        parent: thread.parent.clone(),
        fork_boundary: thread.fork_boundary,
        imported: thread.imported,
        created_at: thread.created_at.clone(),
        updated_at: thread.updated_at.clone(),
        archived_at: thread.archived_at.clone(),
        deleted_at: thread.deleted_at.clone(),
        settled: thread.settled,
        settled_at: thread.settled_at.clone(),
        snoozed_until: thread.snoozed_until.clone(),
        pinned_at: thread.pinned_at.clone(),
        pin_order: thread.pin_order.clone(),
        active_order: thread.active_order.clone(),
        last_visited_at: thread.last_visited_at.clone(),
        auto_settle: thread.auto_settle,
        title_regenerating: thread.title_request.is_some(),
        latest_run: latest.map(|run| run.id.clone()),
        latest_run_requested_at: latest.map(|run| run.requested_at.clone()),
        latest_run_started_at: latest.and_then(|run| run.started_at.clone()),
        latest_run_completed_at: latest.and_then(|run| run.completed_at.clone()),
        status: latest.map(|run| run.status),
        active_run: active.map(|run| run.id.clone()),
        activity_run_status: activity.map(|run| run.status),
        activity_run_started_at: activity
            .map(|run| run.started_at.clone().unwrap_or(run.requested_at.clone())),
        last_error: failure.as_ref().map(|(message, _)| message.clone()),
        last_error_class: failure.and_then(|(_, class)| class),
        pending_request: state
            .requests
            .iter()
            .filter(|request| request.status == RequestStatus::Pending)
            .max_by(|a, b| a.created_at.cmp(&b.created_at))
            .map(|request| PendingRequestSummary {
                id: request.id.clone(),
                kind: match &request.body {
                    RequestBody::Approval { kind, .. } => kind.clone(),
                    RequestBody::Questions { .. } => "user_input".into(),
                },
                created_at: request.created_at.clone(),
            }),
        latest_user_message_at: users().map(|message| message.updated_at.clone()).max(),
        latest_user_authored_message_at: users()
            .filter(|message| message.created_by == MessageAuthor::User)
            .map(|message| message.updated_at.clone())
            .max(),
        has_actionable_proposed_plan: state
            .plans
            .iter()
            .any(|plan| plan.kind == PlanKind::Proposed && plan.implemented_by.is_none()),
        pending_background_work: state
            .background_work
            .values()
            .map(|work| PendingBackgroundSummary {
                key: work.key.clone(),
                kind: work.kind,
                description: work.description.clone(),
            })
            .collect(),
        provider_instance_history: instances,
        item_count: visible
            .iter()
            .filter(|item| state.items.iter().any(|local| local.id == item.id))
            .count(),
        visible_item_count: visible.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn at() -> Timestamp {
        Timestamp::parse("2026-10-05T00:00:00Z").unwrap()
    }
    fn step(state: &mut State, key: &str, input: Input) -> Reply {
        let step = ThreadMachine::step(
            state,
            &InputEnvelope {
                at: at(),
                key: key.into(),
                input,
            },
        );
        *state = fold(state, &step.facts).unwrap();
        step.reply
    }
    fn command(state: &mut State, key: &str, command: Command) -> Reply {
        step(
            state,
            key,
            Input::Command {
                id: CommandId::new(key).unwrap(),
                command: Box::new(command),
                receipt: None,
            },
        )
    }
    fn send(key: &str, mode: DispatchMode) -> Command {
        Command::Send(SendMessage {
            created_by: MessageAuthor::User,
            creation_source: "client".into(),
            id: MessageId::new(key).unwrap(),
            text: key.into(),
            attachments: vec![],
            selection: None,
            mode,
            intent: None,
            source_plan: None,
            title_seed: None,
        })
    }
    fn provider(state: &mut State, key: &str, attempt: &RunAttemptId, event: ProviderEvent) {
        step(
            state,
            key,
            Input::Provider {
                attempt: attempt.clone(),
                event: Box::new(event),
            },
        );
    }
    fn thread() -> State {
        let mut state = State::default();
        command(
            &mut state,
            "create",
            Command::Create {
                thread: ThreadId::new("thread").unwrap(),
                project: "project".into(),
                title: "Thread".into(),
                selection: ModelSelection {
                    instance: "codex".into(),
                    driver: Driver::Codex,
                    model: "gpt".into(),
                    options: BTreeMap::new(),
                },
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
                workspace: None,
            },
        );
        state
    }
    fn fail(state: &mut State, key: &str, class: &str) {
        let attempt = state.active_run().unwrap().attempt.clone().unwrap();
        provider(
            state,
            &format!("{key}-error"),
            &attempt,
            ProviderEvent::ItemFinished {
                key: "error".into(),
                kind: ProviderItem::Error {
                    message: format!("{class} message"),
                    retry: None,
                    code: None,
                    class: Some(class.into()),
                    retryable: None,
                },
                text: None,
                status: ItemStatus::Failed,
            },
        );
        provider(
            state,
            &format!("{key}-failed"),
            &attempt,
            ProviderEvent::TurnFinished {
                status: RunStatus::Failed,
                native_head: None,
            },
        );
    }
    // T3 ProjectionStore.test.ts shell cases.
    #[test]
    fn shell_presents_unheld_runs_errors_requests_and_activity() {
        assert_eq!(shell(&State::default()), None);
        let mut state = thread();
        let row = shell(&state).unwrap();
        assert_eq!((row.status, row.latest_run.clone()), (None, None));
        command(
            &mut state,
            "first",
            send("first", DispatchMode::StartImmediately),
        );
        let attempt = state.active_run().unwrap().attempt.clone().unwrap();
        provider(
            &mut state,
            "approval",
            &attempt,
            ProviderEvent::RequestOpened {
                owner_path: vec![],
                key: "1".into(),
                body: RequestBody::Approval {
                    kind: "file-change".into(),
                    title: "write".into(),
                    detail: None,
                    options: vec![],
                    input: Json(serde_json::json!({})),
                },
                capability: ResponseCapability::Live,
            },
        );
        let row = shell(&state).unwrap();
        assert_eq!(row.active_run, Some(state.runs[0].id.clone()));
        assert_eq!(row.activity_run_status, Some(RunStatus::Starting));
        assert_eq!(row.pending_request.unwrap().kind, "file-change");
        command(
            &mut state,
            "queued",
            send("queued", DispatchMode::QueueAfterActive),
        );
        fail(&mut state, "first", "provider_error");
        let row = shell(&state).unwrap();
        assert!(state.runs[1].queue_held);
        assert_eq!(row.latest_run, Some(state.runs[0].id.clone()));
        assert_eq!(row.status, Some(RunStatus::Failed));
        assert_eq!(row.last_error.as_deref(), Some("provider_error message"));
        assert_eq!(row.last_error_class.as_deref(), Some("provider_error"));
        assert_eq!(row.active_run, None);
        assert_eq!(row.provider_instance_history, ["codex"]);
        assert_eq!(row.item_count, row.visible_item_count);
        assert!(row.latest_user_authored_message_at.is_some());
    }
    #[test]
    fn a_usage_limit_failure_stays_the_presented_run_until_replaced() {
        let mut state = thread();
        command(
            &mut state,
            "first",
            send("first", DispatchMode::StartImmediately),
        );
        command(
            &mut state,
            "queued",
            send("queued", DispatchMode::QueueAfterActive),
        );
        fail(&mut state, "first", "usage_limit");
        let row = shell(&state).unwrap();
        assert_eq!(row.latest_run, Some(state.runs[0].id.clone()));
        assert_eq!(row.last_error_class.as_deref(), Some("usage_limit"));
    }
}

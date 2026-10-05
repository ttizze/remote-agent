//! T3 presentation rules shared by GPUI, SwiftUI and Compose.
use crate::state::{Draft, SendBehavior, Snapshot, TerminalPhase, TerminalView};
use orchestration::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ShelfKind {
    Pinned,
    Active,
    Working,
    Snoozed,
    Settled,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadRow {
    pub id: String,
    pub title: String,
    pub project_id: String,
    pub branch: Option<String>,
    pub worktree: Option<String>,
    pub provider: String,
    pub provider_kind: crate::provider::ProviderKind,
    pub preview: String,
    pub status: String,
    pub tone: StatusTone,
    pub duration_ms: Option<u64>,
    pub unread: bool,
    pub selected: bool,
    pub slim: bool,
    pub wake_label: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    pub settled: bool,
    pub snoozed: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum StatusTone {
    Muted,
    Info,
    Warning,
    Input,
    Error,
    Success,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Shelf {
    pub kind: ShelfKind,
    pub title: String,
    pub rows: Vec<ThreadRow>,
    pub total: u64,
    pub has_more: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RowKind {
    User,
    Assistant,
    Work,
    Plan,
    Approval,
    Question,
    Notice,
    Error,
    Diff,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub status: String,
    pub kind: String,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ApprovalChoice {
    pub decision: String,
    pub label: String,
    pub warning: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InputOption {
    pub label: String,
    pub description: String,
    pub value: String,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct InputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<InputOption>,
    pub multi_select: bool,
    pub allow_custom_answer: bool,
    pub required: bool,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn question_error(
    questions: Vec<InputQuestion>,
    answers: Vec<crate::state::QuestionAnswer>,
) -> Option<String> {
    for answer in &answers {
        if !questions
            .iter()
            .any(|question| question.id == answer.question_id)
        {
            return Some("Unknown question".into());
        }
        if answers
            .iter()
            .filter(|candidate| candidate.question_id == answer.question_id)
            .count()
            > 1
        {
            return Some("Duplicate answer".into());
        }
    }
    for question in questions {
        let values = answers
            .iter()
            .find(|answer| answer.question_id == question.id)
            .map(|answer| {
                answer
                    .values
                    .iter()
                    .filter(|value| !value.trim().is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if question.required && values.is_empty() {
            return Some(format!("Answer {}", question.header));
        }
        if !question.multi_select && values.len() > 1 {
            return Some(format!("Select one answer for {}", question.header));
        }
        if !question.allow_custom_answer
            && values.iter().any(|value| {
                !question
                    .options
                    .iter()
                    .any(|option| &option.value == *value)
            })
        {
            return Some(format!("Choose an option for {}", question.header));
        }
    }
    None
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TimelineRow {
    pub id: String,
    pub kind: RowKind,
    pub text: String,
    pub title: String,
    pub status: String,
    pub streaming: bool,
    pub collapsible: bool,
    pub work: Vec<WorkItem>,
    pub request_id: Option<String>,
    pub choices: Vec<ApprovalChoice>,
    pub questions: Vec<InputQuestion>,
    pub response_mode_message: bool,
    pub actionable: bool,
    pub run_id: Option<String>,
    pub rollback_checkpoint_id: Option<String>,
    pub fork_source_thread_id: Option<String>,
    pub duration_ms: Option<u64>,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueRow {
    pub run_id: String,
    pub text: String,
    pub model: String,
    pub held: bool,
    pub can_steer: bool,
    pub editing: bool,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ComposerView {
    pub pending_deliveries: Vec<String>,
    pub draft: Draft,
    pub send_label: String,
    pub plan_follow_up: bool,
    pub plan_send_label: String,
    pub placeholder: String,
    pub enabled: bool,
    pub can_edit: bool,
    pub can_stop: bool,
    pub can_steer: bool,
    pub can_restart: bool,
    pub queue_count: u64,
    pub queue_held: bool,
    pub editing: bool,
    pub working: bool,
    pub notice: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConversationView {
    pub thread_id: Option<String>,
    pub title: String,
    pub project: String,
    pub cwd: String,
    pub rows: Vec<TimelineRow>,
    pub requests: Vec<TimelineRow>,
    pub queue: Vec<QueueRow>,
    pub composer: ComposerView,
    pub loading: bool,
    pub has_more_history: bool,
    pub archived: bool,
    pub pinned: bool,
    pub settled: bool,
    pub snoozed: bool,
    pub auto_settle: bool,
    pub can_merge_back: bool,
}

pub fn working(shell: &ThreadShell) -> bool {
    shell.pending_runtime_request.is_none()
        && (shell.active_run_id.is_some() || !shell.pending_background_tasks.is_empty())
        && !(shell.thread.interaction_mode == InteractionMode::Plan
            && shell.has_actionable_proposed_plan
            && shell.active_run_id.is_none())
}
pub fn snoozed(shell: &ThreadShell, now: &Timestamp) -> bool {
    shell
        .thread
        .snoozed_until
        .as_ref()
        .is_some_and(|until| until > now)
        && shell.pending_runtime_request.is_none()
        && !(matches!(shell.status, Some(RunStatus::Failed | RunStatus::Completed))
            && shell.thread.snoozed_at.as_ref().is_none_or(|at| {
                shell
                    .latest_run_completed_at
                    .as_ref()
                    .is_some_and(|completed| completed > at)
            }))
}
pub fn shelf_kind(shell: &ThreadShell, now: &Timestamp) -> ShelfKind {
    if snoozed(shell, now) {
        ShelfKind::Snoozed
    } else if shell.thread.settled_override == Some(SettledOverride::Settled) {
        ShelfKind::Settled
    } else if shell.thread.pinned_at.is_some() {
        ShelfKind::Pinned
    } else if working(shell) {
        ShelfKind::Working
    } else {
        ShelfKind::Active
    }
}
pub fn dispatch_mode(active: Option<&RunId>, behavior: SendBehavior) -> DispatchMode {
    match (active, behavior) {
        (Some(id), SendBehavior::Steer) => DispatchMode::SteerActive {
            target_run_id: id.clone(),
        },
        (Some(id), SendBehavior::Restart) => DispatchMode::RestartActive {
            target_run_id: id.clone(),
        },
        (Some(_), SendBehavior::Default) => DispatchMode::QueueAfterActive,
        (None, _) => DispatchMode::StartImmediately,
    }
}
fn wake_label(until: &Timestamp, now: &Timestamp) -> String {
    let millis = until.millis().saturating_sub(now.millis()).max(0) as u64;
    if millis == 0 {
        "now".into()
    } else if millis < 3_600_000 {
        format!("{}m", millis.div_ceil(60_000))
    } else if millis < 86_400_000 {
        format!("{}h", millis.div_ceil(3_600_000))
    } else {
        format!("{}d", millis.div_ceil(86_400_000))
    }
}
pub fn shelves(snapshot: &Snapshot, now: &Timestamp, settled_limit: usize) -> Vec<Shelf> {
    let Some(shell) = &snapshot.shell else {
        return vec![];
    };
    let query = snapshot.search.to_lowercase();
    [
        ShelfKind::Pinned,
        ShelfKind::Active,
        ShelfKind::Working,
        ShelfKind::Snoozed,
        ShelfKind::Settled,
    ]
    .into_iter()
    .map(|kind| {
        let mut threads: Vec<_> = shell
            .threads
            .iter()
            .filter(|s| {
                snapshot
                    .selected_project
                    .as_ref()
                    .is_none_or(|p| p == s.thread.project_id.as_str())
                    && shelf_kind(s, now) == kind
                    && (query.is_empty()
                        || s.thread.title.to_lowercase().contains(&query)
                        || s.latest_visible_message
                            .as_ref()
                            .is_some_and(|m| m.text.to_lowercase().contains(&query))
                        || snapshot
                            .search_matches
                            .iter()
                            .any(|m| m.thread_id == s.thread.id))
            })
            .collect();
        threads.sort_by(|a, b| {
            let custom = match kind {
                ShelfKind::Pinned => match (&a.thread.pin_order_key, &b.thread.pin_order_key) {
                    (Some(a), Some(b)) => a.cmp(b),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => b.thread.created_at.cmp(&a.thread.created_at),
                },
                _ => std::cmp::Ordering::Equal,
            };
            if kind == ShelfKind::Pinned {
                return custom.then_with(|| a.thread.id.cmp(&b.thread.id));
            }
            if kind == ShelfKind::Snoozed {
                return a
                    .thread
                    .snoozed_until
                    .cmp(&b.thread.snoozed_until)
                    .then_with(|| a.thread.id.cmp(&b.thread.id));
            }
            let timestamp = |s: &ThreadShell| {
                match kind {
                    ShelfKind::Working => s
                        .latest_user_message_at
                        .as_ref()
                        .unwrap_or(&s.thread.created_at),
                    ShelfKind::Settled => s
                        .thread
                        .settled_at
                        .as_ref()
                        .or_else(|| {
                            [
                                s.latest_user_message_at.as_ref(),
                                s.latest_run_requested_at.as_ref(),
                                s.latest_run_started_at.as_ref(),
                                s.latest_run_completed_at.as_ref(),
                            ]
                            .into_iter()
                            .flatten()
                            .max()
                        })
                        .unwrap_or(&s.thread.updated_at),
                    ShelfKind::Active => [
                        &s.thread.created_at,
                        s.thread
                            .unsettled_at
                            .as_ref()
                            .unwrap_or(&s.thread.created_at),
                        s.latest_run_requested_at
                            .as_ref()
                            .unwrap_or(&s.thread.created_at),
                        s.latest_run_completed_at
                            .as_ref()
                            .unwrap_or(&s.thread.created_at),
                        snapshot
                            .observed_returns
                            .get(&s.thread.id)
                            .unwrap_or(&s.thread.created_at),
                    ]
                    .into_iter()
                    .max()
                    .unwrap(),
                    _ => &s.thread.updated_at,
                }
                .clone()
            };
            custom
                .then_with(|| timestamp(b).cmp(&timestamp(a)))
                .then_with(|| a.thread.id.cmp(&b.thread.id))
        });
        let total = threads.len();
        if kind == ShelfKind::Settled {
            threads.truncate(settled_limit)
        }
        let rows = threads
            .into_iter()
            .map(|s| thread_row(s, snapshot.selected_thread.as_ref(), kind, now, false))
            .collect::<Vec<_>>();
        Shelf {
            kind,
            title: format!("{kind:?}"),
            has_more: rows.len() < total,
            total: total as u64,
            rows,
        }
    })
    .collect()
}

fn thread_row(
    s: &ThreadShell,
    selected: Option<&ThreadId>,
    kind: ShelfKind,
    now: &Timestamp,
    archived: bool,
) -> ThreadRow {
    let unread = s.thread.last_visited_at.as_ref().is_some_and(|visit| {
        s.latest_run_completed_at
            .as_ref()
            .is_some_and(|completed| completed > visit)
    });
    let (status, tone) = if let Some(request) = &s.pending_runtime_request {
        if request.kind == RequestKind::UserInput {
            ("Input", StatusTone::Input)
        } else {
            ("Approval", StatusTone::Warning)
        }
    } else {
        match s.status {
            Some(RunStatus::Preparing | RunStatus::Starting | RunStatus::Running) => {
                ("Working", StatusTone::Info)
            }
            Some(RunStatus::Waiting) => ("Waiting", StatusTone::Muted),
            Some(RunStatus::Failed) => ("Failed", StatusTone::Error),
            Some(RunStatus::Completed) if unread => ("Done", StatusTone::Success),
            _ => ("", StatusTone::Muted),
        }
    };
    ThreadRow {
        id: s.thread.id.to_string(),
        title: s.thread.title.clone(),
        project_id: s.thread.project_id.to_string(),
        branch: s.thread.branch.clone(),
        worktree: s.thread.worktree_path.clone(),
        provider: s.thread.provider_instance_id.to_string(),
        provider_kind: provider_kind(s.thread.provider_instance_id.as_str()),
        preview: s
            .latest_visible_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default(),
        status: status.into(),
        tone,
        duration_ms: s
            .active_run_started_at
            .as_ref()
            .map(|start| (now.millis() - start.millis()).max(0) as u64),
        unread,
        selected: selected == Some(&s.thread.id),
        slim: archived || matches!(kind, ShelfKind::Snoozed | ShelfKind::Settled),
        wake_label: if kind == ShelfKind::Snoozed {
            s.thread.snoozed_until.as_ref().map(|t| wake_label(t, now))
        } else {
            None
        },
        pinned: s.thread.pinned_at.is_some(),
        archived,
        settled: s.thread.settled_override == Some(SettledOverride::Settled),
        snoozed: snoozed(s, now),
    }
}
pub fn archived_threads(snapshot: &Snapshot, now: &Timestamp) -> Vec<ThreadRow> {
    let query = snapshot.search.to_lowercase();
    let mut rows = snapshot
        .shell
        .as_ref()
        .map(|shell| {
            shell
                .archived_threads
                .iter()
                .filter(|s| {
                    snapshot
                        .selected_project
                        .as_ref()
                        .is_none_or(|p| p == s.thread.project_id.as_str())
                        && (query.is_empty() || s.thread.title.to_lowercase().contains(&query))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| b.thread.archived_at.cmp(&a.thread.archived_at));
    rows.into_iter()
        .map(|s| {
            thread_row(
                s,
                snapshot.selected_thread.as_ref(),
                ShelfKind::Settled,
                now,
                true,
            )
        })
        .collect()
}

fn detail(item: &TurnItem) -> (String, String) {
    match &item.body {
        TurnItemBody::Reasoning { text, .. } => ("Thinking".into(), text.clone()),
        TurnItemBody::CommandExecution { input, output, .. } => {
            (input.clone(), output.clone().unwrap_or_default())
        }
        TurnItemBody::FileSearch { pattern, results } => (
            format!("Search {}", pattern.as_deref().unwrap_or("files")),
            results
                .iter()
                .map(|r| {
                    format!(
                        "{} {}",
                        r.file_name,
                        r.preview.as_deref().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        TurnItemBody::WebSearch { patterns, results } => (
            format!("Search {}", patterns.join(", ")),
            results
                .iter()
                .map(|r| r.title.clone().or(r.url.clone()).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        TurnItemBody::DynamicTool {
            tool_name,
            input,
            output,
            ..
        } => (
            tool_name.clone().unwrap_or_else(|| "Tool".into()),
            output
                .as_ref()
                .map(|j| j.0.to_string())
                .unwrap_or_else(|| input.0.to_string()),
        ),
        TurnItemBody::TodoList {
            steps, explanation, ..
        } => (
            "Plan".into(),
            format!(
                "{}\n{}",
                explanation.as_deref().unwrap_or_default(),
                steps
                    .iter()
                    .map(|s| format!(
                        "{} {}",
                        if s.status == StepStatus::Completed {
                            "✓"
                        } else {
                            "○"
                        },
                        s.text
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        ),
        TurnItemBody::Subagent {
            prompt,
            progress,
            result,
            ..
        } => (
            "Agent".into(),
            result
                .clone()
                .or(progress.clone())
                .unwrap_or_else(|| prompt.clone()),
        ),
        TurnItemBody::Compaction { summary, .. } => (
            "Context compacted".into(),
            summary.clone().unwrap_or_default(),
        ),
        TurnItemBody::Checkpoint { files, .. } => (
            format!("{} changed files", files.len()),
            files
                .iter()
                .map(|file| format!("{}  +{} −{}", file.path, file.additions, file.deletions))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        TurnItemBody::Notification {
            summary, detail, ..
        } => (summary.clone(), detail.clone().unwrap_or_default()),
        TurnItemBody::ApprovalRequest { prompt, .. } => {
            ("Approval".into(), prompt.clone().unwrap_or_default())
        }
        TurnItemBody::UserInputRequest {
            questions,
            question_answer,
            ..
        } => (
            "Questions".into(),
            questions
                .iter()
                .map(|q| q.question.clone())
                .chain(
                    question_answer
                        .iter()
                        .map(|answer| serde_json::to_string(answer).unwrap_or_default()),
                )
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => (
            item.title.clone().unwrap_or_else(|| "Work".into()),
            String::new(),
        ),
    }
}
pub fn timeline(projection: &ThreadProjection) -> Vec<TimelineRow> {
    let mut rows: Vec<TimelineRow> = vec![];
    for projected in orchestration::projector::visible_items(projection) {
        let item = projected.item;
        let request = match &item.body {
            TurnItemBody::ApprovalRequest { request_id, .. }
            | TurnItemBody::UserInputRequest { request_id, .. } => projection
                .runtime_requests
                .iter()
                .find(|r| r.id == *request_id),
            _ => None,
        };
        let actionable = request.is_some_and(|r| {
            r.status == RequestStatus::Pending
                && !matches!(
                    r.response_capability,
                    ResponseCapability::NotResumable { .. }
                )
        });
        let mut row = TimelineRow {
            id: format!("{}:{}", projected.source_thread_id, item.id),
            kind: RowKind::Work,
            text: String::new(),
            title: String::new(),
            status: request
                .map_or(item.status.as_str(), |request| request.status.as_str())
                .into(),
            streaming: false,
            collapsible: false,
            work: vec![],
            request_id: request.map(|r| r.id.to_string()),
            choices: vec![],
            questions: vec![],
            response_mode_message: false,
            actionable,
            run_id: item.run_id.as_ref().map(ToString::to_string),
            fork_source_thread_id: item
                .run_id
                .as_ref()
                .filter(|id| {
                    projection
                        .runs
                        .iter()
                        .find(|r| &r.id == *id)
                        .is_some_and(|r| orchestration::context::forkable(r.status))
                        || projected.visibility == Visibility::Inherited
                })
                .map(|_| projected.source_thread_id.to_string()),
            rollback_checkpoint_id: item
                .run_id
                .as_ref()
                .and_then(|id| projection.runs.iter().find(|r| &r.id == id))
                .and_then(|r| {
                    if projection.thread.rollback_request_id.is_some() {
                        return None;
                    }
                    projection
                        .checkpoints
                        .iter()
                        .find(|c| {
                            c.id == orchestration::checkpoint::before_run_id(&c.scope_id, &r.id)
                                && c.status == CheckpointStatus::Ready
                                && orchestration::rollback::target(projection, &c.scope_id, &c.id)
                                    .is_ok()
                                && projection
                                    .checkpoint_scopes
                                    .iter()
                                    .any(|s| s.id == c.scope_id && s.kind == ScopeKind::RootRun)
                        })
                        .map(|c| c.id.to_string())
                }),
            duration_ms: item
                .started_at
                .as_ref()
                .zip(item.completed_at.as_ref())
                .map(|(a, b)| (b.millis() - a.millis()).max(0) as u64),
        };
        match &item.body {
            TurnItemBody::UserMessage {
                text, input_intent, ..
            } => {
                row.kind = RowKind::User;
                row.text = text.clone();
                row.title = if *input_intent == InputIntent::Steer {
                    "Steer".into()
                } else {
                    String::new()
                };
            }
            TurnItemBody::AssistantMessage {
                text, streaming, ..
            } => {
                row.kind = RowKind::Assistant;
                row.text = text.clone();
                row.streaming = *streaming;
            }
            TurnItemBody::ProposedPlan {
                markdown,
                streaming,
                ..
            } => {
                row.kind = RowKind::Plan;
                row.title = "Plan".into();
                row.text = markdown.clone();
                row.streaming = *streaming;
            }
            TurnItemBody::ApprovalRequest {
                prompt, options, ..
            } if request.is_some_and(|request| request.status == RequestStatus::Pending) => {
                row.kind = RowKind::Approval;
                row.title = "Approval required".into();
                row.text = prompt.clone().unwrap_or_default();
                row.choices = options
                    .iter()
                    .map(|o| ApprovalChoice {
                        decision: o.decision.as_str().into(),
                        label: o.label.clone(),
                        warning: o.warning.clone(),
                    })
                    .collect();
            }
            TurnItemBody::UserInputRequest {
                questions,
                question_answer,
                response_mode_message,
                ..
            } if request.is_some_and(|request| request.status == RequestStatus::Pending) => {
                row.kind = RowKind::Question;
                row.title = "Questions".into();
                row.response_mode_message = *response_mode_message;
                row.questions = questions
                    .iter()
                    .map(|q| InputQuestion {
                        id: q.id.clone(),
                        header: q.header.clone(),
                        question: q.question.clone(),
                        options: q
                            .options
                            .iter()
                            .map(|o| InputOption {
                                label: o.label.clone(),
                                description: o.description.clone(),
                                value: o.value.clone().unwrap_or_else(|| o.label.clone()),
                            })
                            .collect(),
                        multi_select: q.multi_select,
                        allow_custom_answer: q.allow_custom_answer,
                        required: q.required,
                    })
                    .collect();
                row.text = question_answer
                    .as_ref()
                    .map(|a| serde_json::to_string(a).unwrap_or_default())
                    .unwrap_or_default();
            }
            TurnItemBody::FileChange {
                file_name,
                additions,
                deletions,
                diff_str,
                ..
            } => {
                row.kind = RowKind::Diff;
                row.title = format!(
                    "{file_name} +{} −{}",
                    additions.unwrap_or(0),
                    deletions.unwrap_or(0)
                );
                row.text = diff_str.clone().unwrap_or_default();
                row.collapsible = true;
            }
            TurnItemBody::SystemNotice { message }
            | TurnItemBody::RunInterruptRequest { message }
            | TurnItemBody::RunInterruptResult { message } => {
                row.kind = RowKind::Notice;
                row.text = message.clone();
            }
            TurnItemBody::Error { failure, .. } => {
                row.kind = RowKind::Error;
                row.text = failure.message.clone();
            }
            _ => {
                let (title, text) = detail(&item);
                let work = WorkItem {
                    id: item.id.to_string(),
                    title,
                    detail: text,
                    status: row.status.clone(),
                    kind: match &item.body {
                        TurnItemBody::Reasoning { .. } => "reasoning",
                        TurnItemBody::CommandExecution { .. } => "command",
                        _ => "tool",
                    }
                    .into(),
                };
                if let Some(previous) = rows
                    .last_mut()
                    .filter(|r| r.kind == RowKind::Work && r.run_id == row.run_id)
                {
                    previous.work.push(work);
                    previous.status = row.status;
                    continue;
                }
                row.title = "Work log".into();
                row.collapsible = true;
                row.work.push(work);
            }
        }
        rows.push(row);
    }
    rows
}

pub fn actionable_plan(
    plans: &[PlanArtifact],
    mode: InteractionMode,
    has_blocking_run: bool,
) -> Option<&PlanArtifact> {
    if mode != InteractionMode::Plan || has_blocking_run {
        return None;
    }
    plans.iter().rev().find(|p| {
        p.status == PlanStatus::Active
            && matches!(&p.body,PlanBody::ProposedPlan{markdown} if !markdown.trim().is_empty())
    })
}
pub fn conversation(snapshot: &Snapshot, now: &Timestamp) -> ConversationView {
    let projection = snapshot.projection();
    let draft = snapshot.current_draft();
    let active = projection.and_then(|p| p.runs.iter().find(|r| r.status.is_blocking()));
    let pending = projection.and_then(|p| {
        p.runtime_requests
            .iter()
            .find(|r| r.status == RequestStatus::Pending)
    });
    let turns = active.and_then(|run| {
        let projection = projection?;
        let thread = projection
            .provider_threads
            .iter()
            .find(|t| Some(&t.id) == run.provider_thread_id.as_ref())?;
        projection
            .provider_sessions
            .iter()
            .find(|session| Some(&session.id) == thread.provider_session_id.as_ref())
            .map(|session| &session.capabilities.turns)
    });
    let live_turn = active.is_some_and(|run| {
        projection.is_some_and(|p| {
            p.provider_turns.iter().any(|turn| {
                turn.run_attempt_id.as_ref() == run.active_attempt_id.as_ref()
                    && turn.status == TurnStatus::Running
            })
        })
    });
    let can_steer = live_turn && turns.is_some_and(|t| t.supports_active_steering);
    let can_restart = live_turn && turns.is_some_and(|t| t.supports_steering_by_interrupt_restart);
    let mut queued: Vec<_> = projection
        .map(|p| {
            p.runs
                .iter()
                .filter(|r| r.status == RunStatus::Queued)
                .collect()
        })
        .unwrap_or_default();
    queued.sort_by_key(|r| r.queue_position);
    let queue = queued
        .iter()
        .map(|r| QueueRow {
            run_id: r.id.to_string(),
            text: projection
                .unwrap()
                .messages
                .iter()
                .find(|m| m.id == r.user_message_id)
                .map(|m| m.text.clone())
                .unwrap_or_default(),
            model: r.model_selection.model.clone(),
            held: r.queue_held,
            can_steer,
            editing: snapshot.editing_run.as_ref() == Some(&r.id),
        })
        .collect::<Vec<_>>();
    let thread = projection.map(|p| &p.thread);
    let archived = thread.is_some_and(|t| t.archived_at.is_some());
    let live_request =
        pending.is_some_and(|r| r.response_capability != ResponseCapability::Message);
    let editing = snapshot.editing_run.is_some();
    let creating = snapshot.selected_thread.is_none() && snapshot.pending_launches.iter().any(|launch|matches!(&launch.create.body,CommandBody::ThreadCreate{project_id,..} if project_id.as_str()==snapshot.selected_project.as_deref().unwrap_or("bex:chats")));
    let composer = ComposerView {
        pending_deliveries: snapshot
            .uncertain_commands
            .iter()
            .filter(|id| {
                snapshot.pending_commands.iter().any(|c| {
                    &c.command_id == *id && Some(&c.thread_id) == snapshot.selected_thread.as_ref()
                }) || snapshot.selected_thread.is_none()
                    && snapshot
                        .pending_launches
                        .iter()
                        .any(|l| &l.create.command_id == *id)
            })
            .map(ToString::to_string)
            .collect(),
        draft: draft.clone(),
        plan_follow_up: snapshot.connected
            && !snapshot.draft_pending()
            && !archived
            && !live_request
            && !draft.model.is_empty()
            && projection.is_some_and(|p| {
                p.thread.rollback_request_id.is_none()
                    && actionable_plan(&p.plans, p.thread.interaction_mode, active.is_some())
                        .is_some()
            }),
        plan_send_label: if draft.text.trim().is_empty() {
            "Implement"
        } else {
            "Refine"
        }
        .into(),
        send_label: if creating {
            "Creating thread…"
        } else if editing {
            "Update queued message"
        } else if active.is_some() {
            "Queue"
        } else {
            "Send"
        }
        .into(),
        placeholder: if pending
            .is_some_and(|r| r.response_capability == ResponseCapability::Message)
        {
            "Reply to the agent…"
        } else {
            "Ask anything…"
        }
        .into(),
        can_edit: snapshot.connected
            && !archived
            && !live_request
            && thread.is_none_or(|t| t.rollback_request_id.is_none()),
        enabled: !snapshot.draft_pending()
            && snapshot.connected
            && !creating
            && !archived
            && thread.is_none_or(|t| t.rollback_request_id.is_none())
            && !live_request
            && !draft.text.trim().is_empty()
            && !draft.model.is_empty(),
        can_stop: active.is_some() && snapshot.connected,
        can_steer: can_steer && snapshot.connected && !live_request,
        can_restart: can_restart && snapshot.connected && !live_request,
        queue_count: queue.len() as u64,
        queue_held: queue.iter().any(|r| r.held),
        editing,
        working: active.is_some(),
        notice: if let Some(error) = thread.and_then(|t| t.rollback_failure.clone()) {
            Some(error)
        } else if thread.is_some_and(|t| t.rollback_request_id.is_some()) {
            Some("Reverting thread…".into())
        } else if archived {
            Some("Archived thread".into())
        } else if live_request {
            Some("Respond to the pending request to continue".into())
        } else {
            None
        },
    };
    let (requests, rows) = projection
        .map(|p| {
            let mut rows = timeline(p);
            if snapshot.context_pending(&p.thread.id) {
                for row in &mut rows {
                    row.fork_source_thread_id = None;
                }
            }
            rows
        })
        .unwrap_or_default()
        .into_iter()
        .partition(|row| matches!(row.kind, RowKind::Approval | RowKind::Question));
    ConversationView {
        requests,
        thread_id: snapshot.selected_thread.as_ref().map(ToString::to_string),
        title: thread
            .map(|t| t.title.clone())
            .unwrap_or_else(|| "New thread".into()),
        project: thread
            .and_then(|t| {
                snapshot
                    .projects
                    .iter()
                    .find(|p| p.id == t.project_id.as_str())
            })
            .or_else(|| {
                snapshot
                    .projects
                    .iter()
                    .find(|p| Some(&p.id) == snapshot.selected_project.as_ref())
            })
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "Chats".into()),
        cwd: snapshot.cwd(),
        rows,
        queue,
        composer,
        loading: snapshot
            .selected_thread
            .as_ref()
            .is_some_and(|id| snapshot.threads.get(id).is_none_or(|c| !c.synchronized)),
        has_more_history: snapshot
            .selected_thread
            .as_ref()
            .and_then(|id| snapshot.threads.get(id))
            .is_some_and(|c| c.has_more_history),
        archived,
        pinned: thread.is_some_and(|t| t.pinned_at.is_some()),
        settled: thread.is_some_and(|t| t.settled_override == Some(SettledOverride::Settled)),
        snoozed: snapshot
            .shell
            .as_ref()
            .and_then(|shell| {
                shell
                    .threads
                    .iter()
                    .find(|s| Some(&s.thread.id) == snapshot.selected_thread.as_ref())
            })
            .is_some_and(|shell| snoozed(shell, now)),
        auto_settle: thread.is_none_or(|t| t.auto_settle_disabled_at.is_none()),
        can_merge_back: projection.is_some_and(|p| {
            !snapshot.context_pending(&p.thread.id)
                && p.thread.lineage.relationship_to_parent == Some(Relationship::Fork)
                && orchestration::context::merge_back_run(&p.runs).is_some()
        }),
    }
}
impl Snapshot {
    pub fn terminal_view(&self, handle: &str, after: u64) -> TerminalView {
        let terminal = self.terminals.get(handle);
        TerminalView {
            status: terminal.map(|t| match &t.phase {
                TerminalPhase::Starting => "Starting".into(),
                TerminalPhase::Running => "Running".into(),
                TerminalPhase::Suspended => "Waiting for reconnect".into(),
                TerminalPhase::Detached => "Detached".into(),
                TerminalPhase::Exited(code) => format!("Exited · {code}"),
                TerminalPhase::Failed(message) => message.clone(),
            }),
            loading: terminal.is_some_and(|t| t.phase == TerminalPhase::Starting),
            accepts_input: self.connected
                && terminal.is_some_and(|t| t.phase == TerminalPhase::Running),
            output: terminal
                .map(|t| {
                    t.output
                        .iter()
                        .filter(|o| o.sequence > after)
                        .map(|output| output.as_ref().clone())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    #[test]
    fn only_pending_requests_use_the_composer_drawer_and_resolved_requests_join_work_log() {
        let mut p = projection();
        let request = RuntimeRequest {
            id: RuntimeRequestId::new("approval").unwrap(),
            node_id: NodeId::new("approval-node").unwrap(),
            provider_turn_id: None,
            native_request_ref: None,
            kind: RequestKind::Command,
            status: RequestStatus::Pending,
            response_capability: ResponseCapability::Message,
            created_at: now(),
            resolved_at: None,
            decision: None,
            answers: None,
        };
        let approval = item(
            "approval-item",
            1,
            TurnItemBody::ApprovalRequest {
                request_id: request.id.clone(),
                request_kind: RequestKind::Command,
                prompt: Some("Run tests?".into()),
                app_name: None,
                options: vec![],
            },
        );
        p.runtime_requests.push(request);
        p.turn_items.push(approval);
        p.visible_turn_items = projector::visible_items(&p);
        let id = p.thread.id.clone();
        let mut state = Snapshot {
            selected_thread: Some(id.clone()),
            ..Default::default()
        };
        crate::sync::thread(
            &mut state,
            &id,
            ThreadStreamItem::Snapshot {
                snapshot_sequence: 1,
                projection: Box::new(p.clone()),
                history_cursor: None,
                has_more_history: false,
                latest_local_turn_ordinal: None,
            },
        );
        let view = conversation(&state, &now());
        assert_eq!(view.requests.len(), 1);
        assert!(view.rows.is_empty());
        p.runtime_requests[0].status = RequestStatus::Resolved;
        crate::sync::thread(
            &mut state,
            &id,
            ThreadStreamItem::Snapshot {
                snapshot_sequence: 2,
                projection: Box::new(p),
                history_cursor: None,
                has_more_history: false,
                latest_local_turn_ordinal: None,
            },
        );
        let view = conversation(&state, &now());
        assert!(view.requests.is_empty());
        assert_eq!(view.rows[0].kind, RowKind::Work);
        assert_eq!(view.rows[0].work[0].detail, "Run tests?");
        assert_eq!(view.rows[0].work[0].status, "resolved");
    }
    #[test]
    fn unread_is_a_completion_watermark_and_never_visited_is_not_unread() {
        let mut shell = projector::shell(&projection());
        shell.latest_run_completed_at = Some(now());
        assert!(!thread_row(&shell, None, ShelfKind::Active, &now(), false).unread);
        shell.thread.last_visited_at = Some(Timestamp::from_millis(now().millis() - 1).unwrap());
        assert!(thread_row(&shell, None, ShelfKind::Active, &now(), false).unread);
        shell.thread.last_visited_at = Some(now());
        assert!(!thread_row(&shell, None, ShelfKind::Active, &now(), false).unread);
    }
    #[test]
    fn t3_snooze_and_settle_take_precedence_over_pinning() {
        let mut shell = projector::shell(&projection());
        shell.thread.pinned_at = Some(now());
        shell.thread.settled_override = Some(SettledOverride::Settled);
        assert_eq!(shelf_kind(&shell, &now()), ShelfKind::Settled);
        shell.thread.snoozed_until = Some(Timestamp::from_millis(now().millis() + 1000).unwrap());
        assert_eq!(shelf_kind(&shell, &now()), ShelfKind::Snoozed);
    }
    #[test]
    fn default_followup_queues_while_explicit_actions_keep_the_target() {
        let run = RunId::new("run").unwrap();
        assert_eq!(
            dispatch_mode(Some(&run), SendBehavior::Default),
            DispatchMode::QueueAfterActive
        );
        assert_eq!(
            dispatch_mode(Some(&run), SendBehavior::Steer),
            DispatchMode::SteerActive {
                target_run_id: run.clone()
            }
        );
        assert_eq!(
            dispatch_mode(None, SendBehavior::Steer),
            DispatchMode::StartImmediately
        );
    }
    #[test]
    fn approval_wakes_snoozed_threads_without_changing_parked_shelf_precedence() {
        let mut shell = projector::shell(&projection());
        shell.thread.snoozed_until = Some(Timestamp::parse("2026-10-06T00:00:00Z").unwrap());
        shell.thread.snoozed_at = Some(now());
        shell.thread.pinned_at = Some(now());
        assert_eq!(shelf_kind(&shell, &now()), ShelfKind::Snoozed);
        shell.thread.settled_override = Some(SettledOverride::Settled);
        assert_eq!(shelf_kind(&shell, &now()), ShelfKind::Snoozed);
        let mut settled = shell.clone();
        settled.thread.snoozed_until = None;
        settled.thread.snoozed_at = None;
        assert_eq!(shelf_kind(&settled, &now()), ShelfKind::Settled);
        shell.thread.pinned_at = None;
        shell.thread.settled_override = None;
        shell.pending_runtime_request = Some(PendingRuntimeRequest {
            id: RuntimeRequestId::new("request").unwrap(),
            kind: RequestKind::Command,
            created_at: now(),
        });
        assert_eq!(shelf_kind(&shell, &now()), ShelfKind::Active);
        shell.thread.pinned_at = Some(now());
        assert_eq!(shelf_kind(&shell, &now()), ShelfKind::Pinned);
    }
    #[test]
    fn pinned_keys_outrank_keyless_rows_and_equal_keys_use_identity() {
        let mut a = projector::shell(&projection());
        a.thread.id = ThreadId::new("a").unwrap();
        a.thread.pinned_at = Some(now());
        let mut b = a.clone();
        b.thread.id = ThreadId::new("b").unwrap();
        b.thread.pin_order_key = Some("n".into());
        let mut c = b.clone();
        c.thread.id = ThreadId::new("c").unwrap();
        c.thread.updated_at = Timestamp::parse("2026-10-06T00:00:00Z").unwrap();
        let snapshot = Snapshot {
            shell: Some(std::sync::Arc::new(ShellSnapshot {
                schema_version: 2,
                snapshot_sequence: 0,
                threads: vec![a, c, b],
                archived_threads: vec![],
            })),
            ..Snapshot::default()
        };
        let rows = shelves(&snapshot, &now(), 10).remove(0).rows;
        assert_eq!(
            rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["b", "c", "a"]
        );
    }
    #[test]
    fn snoozed_rows_wake_soonest_first_and_settled_rows_use_completion_time() {
        let mut a = projector::shell(&projection());
        a.thread.id = ThreadId::new("a").unwrap();
        a.thread.snoozed_until = Some(Timestamp::parse("2026-10-06T01:00:00Z").unwrap());
        let mut b = a.clone();
        b.thread.id = ThreadId::new("b").unwrap();
        b.thread.snoozed_until = Some(Timestamp::parse("2026-10-06T00:00:00Z").unwrap());
        let mut snapshot = Snapshot {
            shell: Some(std::sync::Arc::new(ShellSnapshot {
                schema_version: 2,
                snapshot_sequence: 0,
                threads: vec![a, b],
                archived_threads: vec![],
            })),
            ..Snapshot::default()
        };
        let snoozed = shelves(&snapshot, &now(), 10)
            .into_iter()
            .find(|s| s.kind == ShelfKind::Snoozed)
            .unwrap();
        assert_eq!(snoozed.rows[0].id, "b");
        let shell = std::sync::Arc::make_mut(snapshot.shell.as_mut().unwrap());
        for thread in &mut shell.threads {
            thread.thread.snoozed_until = None;
            thread.thread.settled_override = Some(SettledOverride::Settled);
            thread.thread.settled_at = None;
        }
        shell.threads[0].latest_run_completed_at =
            Some(Timestamp::parse("2026-10-05T02:00:00Z").unwrap());
        shell.threads[0].thread.updated_at = Timestamp::parse("2026-10-06T12:00:00Z").unwrap();
        shell.threads[1].latest_run_completed_at =
            Some(Timestamp::parse("2026-10-05T03:00:00Z").unwrap());
        let settled = shelves(&snapshot, &now(), 10)
            .into_iter()
            .find(|s| s.kind == ShelfKind::Settled)
            .unwrap();
        assert_eq!(settled.rows[0].id, "b");
    }
    #[test]
    fn work_logs_group_between_messages_without_hiding_final_text() {
        let mut p = projection();
        p.turn_items = vec![
            item(
                "u",
                1,
                TurnItemBody::UserMessage {
                    created_by: CreatedBy::User,
                    creation_source: CreationSource::Desktop,
                    message_id: MessageId::new("u").unwrap(),
                    input_intent: InputIntent::TurnStart,
                    text: "Build".into(),
                    context: None,
                    attachments: vec![],
                },
            ),
            item(
                "think",
                2,
                TurnItemBody::Reasoning {
                    text: "Thinking".into(),
                    streaming: false,
                },
            ),
            item(
                "cmd",
                3,
                TurnItemBody::CommandExecution {
                    input: "cargo check".into(),
                    output: Some("success".into()),
                    output_omitted: false,
                    output_indicates_failure: false,
                    exit_code: Some(0),
                },
            ),
            item(
                "a",
                4,
                TurnItemBody::AssistantMessage {
                    message_id: MessageId::new("a").unwrap(),
                    text: "Done".into(),
                    attachments: vec![],
                    streaming: false,
                },
            ),
        ];
        let rows = timeline(&p);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].work.len(), 2);
        assert!(rows[1].collapsible);
        assert_eq!(rows[2].text, "Done");
        assert!(!rows[2].collapsible);
    }
    #[test]
    fn settled_shelf_starts_with_ten_then_accepts_twenty_five_more() {
        let mut threads = vec![];
        for index in 0..40 {
            let mut shell = projector::shell(&projection());
            shell.thread.id = ThreadId::new(format!("thread-{index}")).unwrap();
            shell.thread.settled_override = Some(SettledOverride::Settled);
            threads.push(shell);
        }
        let snapshot = Snapshot {
            shell: Some(std::sync::Arc::new(ShellSnapshot {
                schema_version: 2,
                snapshot_sequence: 0,
                threads,
                archived_threads: vec![],
            })),
            ..Snapshot::default()
        };
        assert_eq!(shelves(&snapshot, &now(), 10)[4].rows.len(), 10);
        assert_eq!(shelves(&snapshot, &now(), 35)[4].rows.len(), 35);
        assert!(shelves(&snapshot, &now(), 35)[4].has_more);
    }
    #[test]
    fn required_and_single_choice_answers_are_validated_in_core() {
        let question = InputQuestion {
            id: "q".into(),
            header: "Location".into(),
            question: "Where?".into(),
            options: vec![InputOption {
                label: "Here".into(),
                description: String::new(),
                value: "here".into(),
            }],
            multi_select: false,
            allow_custom_answer: false,
            required: true,
        };
        assert!(question_error(vec![question.clone()], vec![]).is_some());
        assert!(
            question_error(
                vec![question.clone()],
                vec![crate::state::QuestionAnswer {
                    question_id: "q".into(),
                    values: vec!["outside".into()]
                }]
            )
            .is_some()
        );
        assert!(
            question_error(
                vec![question],
                vec![crate::state::QuestionAnswer {
                    question_id: "q".into(),
                    values: vec!["here".into()]
                }]
            )
            .is_none()
        );
    }
}

/// Provider selection and labels belong to core, not native views.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ModelChoice {
    pub instance_id: String,
    pub model: crate::models::Model,
    pub selected: bool,
}
pub fn model_choices(snapshot: &Snapshot) -> Vec<ModelChoice> {
    let draft = snapshot.current_draft();
    snapshot
        .models
        .iter()
        .map(|model| {
            let instance_id = match model.model.provider {
                crate::provider::ProviderKind::Codex => "codex",
                crate::provider::ProviderKind::Claude => "claude",
            }
            .to_owned();
            ModelChoice {
                selected: draft.instance_id == instance_id && draft.model == model.id,
                instance_id,
                model: model.clone(),
            }
        })
        .collect()
}
fn provider_kind(instance: &str) -> crate::provider::ProviderKind {
    match instance {
        "claude" => crate::provider::ProviderKind::Claude,
        _ => crate::provider::ProviderKind::Codex,
    }
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RuntimeModeChoice {
    pub id: String,
    pub label: String,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn runtime_mode_choices() -> Vec<RuntimeModeChoice> {
    [
        ("approval-required", "Supervised"),
        ("auto-accept-edits", "Auto-accept edits"),
        ("auto", "Auto"),
        ("full-access", "Full access"),
    ]
    .into_iter()
    .map(|(id, label)| RuntimeModeChoice {
        id: id.into(),
        label: label.into(),
    })
    .collect()
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn interaction_mode_choices() -> Vec<RuntimeModeChoice> {
    [("default", "Chat"), ("plan", "Plan")]
        .into_iter()
        .map(|(id, label)| RuntimeModeChoice {
            id: id.into(),
            label: label.into(),
        })
        .collect()
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn question_answer_values(selected: Vec<String>, custom: String, multi: bool) -> Vec<String> {
    let mut values = if !multi && !custom.trim().is_empty() {
        vec![]
    } else {
        selected
    };
    if !custom.trim().is_empty() {
        values.push(custom);
    }
    values
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn question_option_selected(
    selected: Vec<String>,
    custom: String,
    multi: bool,
    value: String,
) -> bool {
    (multi || custom.trim().is_empty()) && selected.contains(&value)
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn safe_markdown_url(url: String) -> bool {
    url.split_once(':').is_some_and(|(scheme, _)| {
        ["http", "https", "mailto"]
            .iter()
            .any(|allowed| scheme.eq_ignore_ascii_case(allowed))
    })
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn snapshot_is_newer(current: u64, incoming: u64) -> bool {
    incoming > current
}
#[cfg(test)]
mod review_presentation_tests {
    use super::*;
    #[test]
    fn custom_single_answer_deselects_option_but_multi_preserves_it() {
        let selected = vec!["one".into()];
        assert!(!question_option_selected(
            selected.clone(),
            "custom".into(),
            false,
            "one".into()
        ));
        assert_eq!(
            question_answer_values(selected.clone(), "custom".into(), false),
            vec!["custom"]
        );
        assert_eq!(
            question_answer_values(selected, "custom".into(), true),
            vec!["one", "custom"]
        );
    }
    #[test]
    fn unsafe_markdown_links_are_rejected() {
        for url in [
            "javascript:alert(1)",
            "file:/etc/passwd",
            "intent:danger",
            "data:text/html,hi",
        ] {
            assert!(!safe_markdown_url(url.into()));
        }
        for url in [
            "https://example.org",
            "http://localhost",
            "mailto:me@example.org",
        ] {
            assert!(safe_markdown_url(url.into()));
        }
    }
    #[test]
    fn stale_or_duplicate_snapshots_are_rejected() {
        assert!(!snapshot_is_newer(10, 9));
        assert!(!snapshot_is_newer(10, 10));
        assert!(snapshot_is_newer(10, 11));
    }
    #[test]
    fn runtime_labels_match_t3() {
        assert_eq!(runtime_mode_choices()[0].label, "Supervised");
    }
}

/// Native edits based on an older visible buffer must preserve a core append
/// (for example dictation) that arrived while the native edit was in flight.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn merge_draft_text(base: String, edited: String, current: String) -> String {
    if current != base && current.starts_with(&base) {
        format!("{}{}", edited, &current[base.len()..])
    } else {
        edited
    }
}
#[cfg(test)]
mod draft_merge_tests {
    #[test]
    fn delayed_native_edit_preserves_transcription_append() {
        assert_eq!(
            super::merge_draft_text(
                "hello".into(),
                "hello there".into(),
                "hello\ntranscript".into()
            ),
            "hello there\ntranscript"
        );
        assert_eq!(
            super::merge_draft_text("hello".into(), "".into(), "hello".into()),
            ""
        );
    }
}

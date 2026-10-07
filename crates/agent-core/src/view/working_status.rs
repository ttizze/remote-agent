//! The floating pill above the composer. Connection, syncing, compaction, the
//! working timer and background work share one status slot so the label swaps
//! in place; the agents and queue counts and the scroll-to-end control sit
//! beside it.
use crate::commands::workflows::{BackgroundTaskKind, PendingBackgroundTask, user_queued_runs};
use crate::sync::thread::ThreadStatus;
use crate::view::agents::format_subagent_display_title;
use crate::view::time::format_duration;
use agent_domain::{ItemKind, ItemStatus, RequestStatus, Run, RunStatus, State};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConnectionPhase {
    Connecting,
    Reconnecting,
    Offline,
    Unsupported,
    Error,
    Available,
    Connected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConnectionTone {
    Reconnecting,
    Unavailable,
}

/// What the pill's status slot says. Tapping `Connection` reconnects.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum FloatingWorkingStatus {
    Working {
        started_at_ms: i64,
    },
    Syncing {
        label: String,
    },
    Compacting,
    /// The turn settled while work it started still runs. `waiting` is false
    /// when only commands remain, such as a dev server: the agent is done.
    Background {
        label: String,
        accessibility_label: String,
        waiting: bool,
    },
    /// A thread the Host has not created yet: no turn to time.
    Preparing {
        label: String,
    },
    Connection {
        tone: ConnectionTone,
        label: String,
    },
}

/// The pill's connection variant, or `None` once connected.
pub fn connection_floating_status(
    phase: ConnectionPhase,
    connection_error: Option<&str>,
    environment_label: Option<&str>,
) -> Option<FloatingWorkingStatus> {
    let environment = environment_label.unwrap_or("Environment");
    let unavailable = |label: String| FloatingWorkingStatus::Connection {
        tone: ConnectionTone::Unavailable,
        label,
    };
    Some(match phase {
        ConnectionPhase::Connecting | ConnectionPhase::Reconnecting => {
            FloatingWorkingStatus::Connection {
                tone: ConnectionTone::Reconnecting,
                label: match connection_error {
                    None => format!("Reconnecting to {environment}..."),
                    Some(_) => format!("Failed to connect. Retrying {environment}..."),
                },
            }
        }
        ConnectionPhase::Offline => unavailable("You are offline".into()),
        ConnectionPhase::Unsupported => unavailable("Client not supported".into()),
        ConnectionPhase::Error => unavailable(match connection_error.filter(|e| !e.is_empty()) {
            Some(error) => format!("Failed to connect to {environment}: {error}"),
            None => format!("Failed to connect to {environment}"),
        }),
        ConnectionPhase::Available => unavailable(format!("{environment} is not connected")),
        ConnectionPhase::Connected => return None,
    })
}

/// How the thread's conversation area presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadContentKind {
    Ready,
    Loading,
    Unavailable,
}

/// The sync label while a fetch runs: "Loading messages..." before any data,
/// "Syncing messages..." while cached data reconciles. Clients show it only
/// once it has lasted 400 ms, and keep it at least 400 ms.
pub fn thread_sync_label(status: ThreadStatus, content: ThreadContentKind) -> Option<String> {
    match status {
        ThreadStatus::Empty | ThreadStatus::Cached | ThreadStatus::Synchronizing => match content {
            ThreadContentKind::Ready => Some("Syncing messages...".into()),
            ThreadContentKind::Loading => Some("Loading messages...".into()),
            ThreadContentKind::Unavailable => None,
        },
        ThreadStatus::Live | ThreadStatus::Deleted => None,
    }
}

/// A thread the Host has not created yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadCreation {
    /// The queued creation is being delivered; a worktree may be checking out.
    Preparing { worktree: bool },
    /// The creation was rejected and its content went back to the draft.
    Failed,
}

/// Inputs the folded thread does not hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FloatingStatusInput {
    pub connection_phase: ConnectionPhase,
    pub connection_error: Option<String>,
    pub environment_label: Option<String>,
    /// From [`thread_sync_label`], after the client's show delay.
    pub sync_label: Option<String>,
    pub content: ThreadContentKind,
    pub creation: Option<ThreadCreation>,
    /// The worktree setup card reports progress in the feed.
    pub worktree_setup_visible: bool,
    /// The setup card is shown and the agent's turn has not started.
    pub setup_awaiting_turn: bool,
}

/// One pill: the connection phase while disconnected, the sync state while
/// messages load, then compaction, the working timer or background work.
pub fn floating_working_status(
    state: Option<&State>,
    input: &FloatingStatusInput,
) -> Option<FloatingWorkingStatus> {
    if let Some(connection) = connection_floating_status(
        input.connection_phase,
        input.connection_error.as_deref(),
        input.environment_label.as_deref(),
    ) {
        return Some(connection);
    }
    if state.is_some_and(|state| {
        state
            .requests
            .iter()
            .any(|request| request.status == RequestStatus::Pending)
    }) {
        return None;
    }
    match input.creation {
        Some(ThreadCreation::Preparing { worktree }) => {
            if input.worktree_setup_visible {
                return None;
            }
            return Some(FloatingWorkingStatus::Preparing {
                label: if worktree {
                    "Setting up worktree…"
                } else {
                    "Starting…"
                }
                .into(),
            });
        }
        Some(ThreadCreation::Failed) => return None,
        None => {}
    }
    if let Some(label) = &input.sync_label {
        return Some(FloatingWorkingStatus::Syncing {
            label: label.clone(),
        });
    }
    let ready = input.content == ThreadContentKind::Ready;
    let state = state?;
    if is_compacting(state) && ready {
        return Some(FloatingWorkingStatus::Compacting);
    }
    if !input.setup_awaiting_turn
        && let Some(started_at_ms) = active_work_started_at(state)
        && ready
    {
        return Some(FloatingWorkingStatus::Working { started_at_ms });
    }
    let background = present_pending_background_work(
        &crate::commands::workflows::pending_background_work(state),
    )?;
    ready.then(|| FloatingWorkingStatus::Background {
        accessibility_label: format!(
            "{}: {}",
            background.title,
            background
                .items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        label: background.title,
        waiting: background.waiting,
    })
}

/// When the work a run belongs to started. A wake (a notification, delegated
/// result or restart continuation) continues the work of the newest started
/// run before it, so the timer counts from the prompt that started it.
fn run_work_started_at(state: &State, run: &Run) -> i64 {
    let wake = run.restart_of.is_some()
        || state
            .message(&run.message)
            .is_some_and(|message| message.notification.is_some());
    if wake
        && let Some(previous) = state
            .runs
            .iter()
            .filter(|candidate| candidate.ordinal < run.ordinal)
            .filter_map(|candidate| Some((candidate.started_at.as_ref()?, candidate)))
            .max_by(|(left_at, left), (right_at, right)| {
                left_at.cmp(right_at).then(left.ordinal.cmp(&right.ordinal))
            })
            .map(|(_, candidate)| candidate)
    {
        return run_work_started_at(state, previous);
    }
    run.started_at
        .as_ref()
        .unwrap_or(&run.requested_at)
        .millis()
}

/// The working timer's start: the work start of the newest preparing,
/// starting, running or waiting run.
pub fn active_work_started_at(state: &State) -> Option<i64> {
    state
        .active_run()
        .map(|run| run_work_started_at(state, run))
}

fn is_compact_command(text: &str, attachments: usize) -> bool {
    attachments == 0 && text.trim().eq_ignore_ascii_case("/compact")
}

/// A `/compact` turn is running and its compaction has not finished.
pub fn is_compacting(state: &State) -> bool {
    let Some(run) = state
        .runs
        .iter()
        .filter(|run| {
            matches!(
                run.status,
                RunStatus::Preparing | RunStatus::Starting | RunStatus::Running
            )
        })
        .max_by_key(|run| run.ordinal)
    else {
        return false;
    };
    let items = state.visible_items();
    let compact_message = items.iter().rev().any(|item| {
        item.run.as_ref() == Some(&run.id)
            && matches!(&item.kind, ItemKind::UserMessage { message }
            if state.message(message).is_some_and(|message| {
                is_compact_command(&message.text, message.attachments.len())
            }))
    });
    compact_message
        && !items.iter().any(|item| {
            item.run.as_ref() == Some(&run.id)
                && matches!(item.kind, ItemKind::Compaction { .. })
                && matches!(item.status, ItemStatus::Completed | ItemStatus::Failed)
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum BackgroundWorkKind {
    Subagent,
    Command,
    Monitor,
    BackgroundTask,
}
impl From<BackgroundTaskKind> for BackgroundWorkKind {
    fn from(kind: BackgroundTaskKind) -> Self {
        match kind {
            BackgroundTaskKind::Subagent => Self::Subagent,
            BackgroundTaskKind::Command => Self::Command,
            BackgroundTaskKind::Monitor => Self::Monitor,
            BackgroundTaskKind::BackgroundTask => Self::BackgroundTask,
        }
    }
}
impl BackgroundWorkKind {
    /// Agents first, loose tasks last.
    fn order(self) -> u8 {
        match self {
            Self::Subagent => 0,
            Self::Command => 1,
            Self::Monitor => 2,
            Self::BackgroundTask => 3,
        }
    }
    fn singular(self) -> &'static str {
        match self {
            Self::Subagent => "subagent",
            Self::Command => "command",
            Self::Monitor => "monitor",
            Self::BackgroundTask => "background task",
        }
    }
    fn plural(self) -> &'static str {
        match self {
            Self::Subagent => "subagents",
            Self::Command => "commands",
            Self::Monitor => "monitors",
            Self::BackgroundTask => "background tasks",
        }
    }
    /// Work that wakes the agent when it finishes; a command does not.
    fn holds_completion(self) -> bool {
        self != Self::Command
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PendingBackgroundWorkItem {
    pub task_id: String,
    pub kind: BackgroundWorkKind,
    /// The work's name, or its noun when the provider gave none.
    pub label: String,
    pub child_thread: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PendingBackgroundWork {
    /// "Waiting on subagent Review src/math.ts", "Waiting on 2 subagents and
    /// 1 command", or "Running: Start the dev server" when only commands remain.
    pub title: String,
    pub items: Vec<PendingBackgroundWorkItem>,
    /// The work will wake the agent (subagents, monitors).
    pub waiting: bool,
}

fn join_with_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Names what a settled thread still runs, grouped by kind.
pub fn present_pending_background_work(
    tasks: &[PendingBackgroundTask],
) -> Option<PendingBackgroundWork> {
    if tasks.is_empty() {
        return None;
    }
    let waiting = tasks
        .iter()
        .any(|task| BackgroundWorkKind::from(task.kind).holds_completion());
    let mut items: Vec<PendingBackgroundWorkItem> = tasks
        .iter()
        .map(|task| {
            let kind = BackgroundWorkKind::from(task.kind);
            let description = task.description.as_deref().map(str::trim);
            let label = match description {
                Some(description) if kind == BackgroundWorkKind::Subagent => {
                    format_subagent_display_title(description).trim().to_owned()
                }
                description => description.unwrap_or_default().to_owned(),
            };
            PendingBackgroundWorkItem {
                task_id: task.task_id.clone(),
                kind,
                label: if label.is_empty() {
                    kind.singular().into()
                } else {
                    label
                },
                child_thread: (kind == BackgroundWorkKind::Subagent)
                    .then(|| task.child_thread.as_ref().map(ToString::to_string))
                    .flatten(),
            }
        })
        .collect();
    items.sort_by_key(|item| item.kind.order());
    if let [only] = items.as_slice() {
        let noun = only.kind.singular();
        let named = only.label != noun;
        let title = match (waiting, named) {
            (true, true) => format!("Waiting on {noun} {}", only.label),
            (true, false) => format!("Waiting on a {noun}"),
            (false, true) => format!("Running: {}", only.label),
            (false, false) => format!("Running a {noun}"),
        };
        return Some(PendingBackgroundWork {
            title,
            items,
            waiting,
        });
    }
    let mut counts: Vec<(BackgroundWorkKind, usize)> = vec![];
    for item in &items {
        match counts.iter_mut().find(|(kind, _)| *kind == item.kind) {
            Some((_, count)) => *count += 1,
            None => counts.push((item.kind, 1)),
        }
    }
    let groups: Vec<String> = counts
        .into_iter()
        .map(|(kind, count)| {
            let noun = if count == 1 {
                kind.singular()
            } else {
                kind.plural()
            };
            format!("{count} {noun}")
        })
        .collect();
    Some(PendingBackgroundWork {
        title: format!(
            "{} {}",
            if waiting { "Waiting on" } else { "Running" },
            join_with_and(&groups)
        ),
        items,
        waiting,
    })
}

/// "12s", "12m 04s", then "1h 2m 3s" from an hour.
pub fn format_working_duration(started_at_ms: i64, now_ms: i64) -> String {
    if now_ms <= started_at_ms {
        return "0s".into();
    }
    let total_seconds = (now_ms - started_at_ms) / 1_000;
    if total_seconds < 60 {
        return format!("{total_seconds}s");
    }
    if total_seconds >= 3_600 {
        return format_duration(total_seconds * 1_000);
    }
    format!("{}m {:02}s", total_seconds / 60, total_seconds % 60)
}

/// A count segment of the pill: the agents (`2/3`, `3 done`).
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct PillSegment {
    pub label: String,
    pub accessibility_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkingControlView {
    pub status: Option<FloatingWorkingStatus>,
    /// "Working 12m 04s", "Compacting…" or the status label.
    pub status_label: Option<String>,
    pub status_accessibility_label: Option<String>,
    /// Swapping keys cross-fades the label; the timer keeps one key while it ticks.
    pub status_key: Option<String>,
    pub status_interactive: bool,
    /// "Open agents, 2 of 3 agents working".
    pub agents: Option<PillSegment>,
    pub queued_count: u32,
    /// "3 queued", opening the queue sheet.
    pub queue: Option<PillSegment>,
    /// The capsule (status, agents or queue) is shown.
    pub has_capsule: bool,
    pub show_scroll_to_end: bool,
}

/// The pill's content, or `None` when neither the capsule nor the
/// scroll-to-end control shows.
pub fn working_control(
    status: Option<FloatingWorkingStatus>,
    agents: Option<PillSegment>,
    queued_count: u32,
    show_scroll_to_end: bool,
    now_ms: i64,
) -> Option<WorkingControlView> {
    let has_queue = queued_count > 0;
    let has_capsule = status.is_some() || has_queue || agents.is_some();
    if !has_capsule && !show_scroll_to_end {
        return None;
    }
    let (label, accessibility_label, key) = match &status {
        None => (None, None, None),
        Some(FloatingWorkingStatus::Working { started_at_ms }) => {
            let label = format!(
                "Working {}",
                format_working_duration(*started_at_ms, now_ms)
            );
            (Some(label.clone()), Some(label), Some("working".to_owned()))
        }
        Some(FloatingWorkingStatus::Compacting) => (
            Some("Compacting…".to_owned()),
            Some("Compacting".to_owned()),
            Some("compacting".to_owned()),
        ),
        Some(FloatingWorkingStatus::Syncing { label }) => (
            Some(label.clone()),
            Some(label.clone()),
            Some(format!("syncing:{label}")),
        ),
        Some(FloatingWorkingStatus::Preparing { label }) => (
            Some(label.clone()),
            Some(label.clone()),
            Some(format!("preparing:{label}")),
        ),
        Some(FloatingWorkingStatus::Connection { label, .. }) => (
            Some(label.clone()),
            Some(label.clone()),
            Some(format!("connection:{label}")),
        ),
        Some(FloatingWorkingStatus::Background {
            label,
            accessibility_label,
            ..
        }) => (
            Some(label.clone()),
            Some(accessibility_label.clone()),
            Some(format!("background:{label}")),
        ),
    };
    Some(WorkingControlView {
        status_interactive: matches!(status, Some(FloatingWorkingStatus::Connection { .. })),
        status,
        status_label: label,
        status_accessibility_label: accessibility_label,
        status_key: key,
        agents: agents.map(|agents| PillSegment {
            accessibility_label: format!("Open agents, {}", agents.accessibility_label),
            label: agents.label,
        }),
        queued_count,
        queue: has_queue.then(|| PillSegment {
            label: format!("{queued_count} queued"),
            accessibility_label: format!("Open queue, {queued_count} messages"),
        }),
        has_capsule,
        show_scroll_to_end,
    })
}

/// The messages the user queued behind the active run.
pub fn queued_count(state: &State) -> u32 {
    u32::try_from(user_queued_runs(state).len()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;

//! The worktree setup card: which setup snapshot a thread shows and how its
//! stages, output and actions read.
use crate::commands::outbox::Request;
use crate::state::Snapshot;
use crate::view::time::format_duration;
use agent_domain::{
    Role, Run, RunStatus, State, ThreadId, Timestamp, WorktreeSetupPhase, WorktreeSetupSnapshot,
    WorktreeSetupStage, WorktreeSetupStageId, WorktreeSetupStageStatus,
};
use agent_protocol::conversation::WorkspaceStrategy;

/// The Host keeps this many trailing lines; the output box has exactly as many rows.
pub const SETUP_OUTPUT_TAIL_LINES: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SetupStageId {
    Fetch,
    Checkout,
    Submodules,
    SetupScript,
    Agent,
}
impl From<WorktreeSetupStageId> for SetupStageId {
    fn from(id: WorktreeSetupStageId) -> Self {
        match id {
            WorktreeSetupStageId::Fetch => Self::Fetch,
            WorktreeSetupStageId::Checkout => Self::Checkout,
            WorktreeSetupStageId::Submodules => Self::Submodules,
            WorktreeSetupStageId::SetupScript => Self::SetupScript,
            WorktreeSetupStageId::Agent => Self::Agent,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SetupStageStatus {
    Pending,
    Running,
    Done,
    Skipped,
    Warning,
    Failed,
}
impl From<WorktreeSetupStageStatus> for SetupStageStatus {
    fn from(status: WorktreeSetupStageStatus) -> Self {
        match status {
            WorktreeSetupStageStatus::Pending => Self::Pending,
            WorktreeSetupStageStatus::Running => Self::Running,
            WorktreeSetupStageStatus::Done => Self::Done,
            WorktreeSetupStageStatus::Skipped => Self::Skipped,
            WorktreeSetupStageStatus::Warning => Self::Warning,
            WorktreeSetupStageStatus::Failed => Self::Failed,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SetupPhase {
    Running,
    Done,
    Failed,
    Cancelled,
}
impl From<WorktreeSetupPhase> for SetupPhase {
    fn from(phase: WorktreeSetupPhase) -> Self {
        match phase {
            WorktreeSetupPhase::Running => Self::Running,
            WorktreeSetupPhase::Done => Self::Done,
            WorktreeSetupPhase::Failed => Self::Failed,
            WorktreeSetupPhase::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SetupTone {
    Muted,
    Warning,
    Destructive,
}

/// Desktop shows every stage in the timeline; mobile leaves the agent stage
/// to the working header and keeps the output in a details sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SetupCardLayout {
    Desktop,
    Mobile,
}

/// The setup script's last lines, padded to a fixed number of rows with
/// empty strings.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SetupOutputTail {
    pub lines: Vec<String>,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SetupStageView {
    pub id: SetupStageId,
    pub status: SetupStageStatus,
    pub label: String,
    /// A percentage, file count, exit code or "skipped".
    pub trailing: Option<String>,
    pub elapsed: Option<String>,
    pub output: Option<SetupOutputTail>,
}

/// The one-line outcome of a settled setup under a live turn.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SetupSummaryRow {
    pub status: SetupStageStatus,
    pub label: String,
    pub elapsed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SetupDetail {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SetupCardView {
    pub thread_id: String,
    pub phase: SetupPhase,
    pub sequence: u64,
    /// Desktop draws the card in the timeline only when this is set; otherwise
    /// the script still runs after the handoff and the working header carries
    /// `background_script`.
    pub show_in_timeline: bool,
    /// Until the agent's turn is live the card sits under the working header.
    pub owns_working_slot: bool,
    /// The agent's turn is live and owns the working header.
    pub handed_off: bool,
    pub title: String,
    pub tone: SetupTone,
    /// The card brings its own header row.
    pub show_header: bool,
    pub elapsed: Option<String>,
    pub show_stages: bool,
    pub stages: Vec<SetupStageView>,
    /// Desktop's collapsed form after the handoff.
    pub summary: Option<SetupSummaryRow>,
    pub error: Option<String>,
    /// Branch, base, path and setup command, shown under "Details".
    pub details: Vec<SetupDetail>,
    /// The chip label while the script runs after the handoff.
    pub background_script: Option<String>,
    pub can_cancel: bool,
    pub can_work_locally: bool,
    /// "Open terminal" opens this thread terminal.
    pub open_terminal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SetupView {
    pub card: Option<SetupCardView>,
    /// The working header reads "Setting up worktree…".
    pub preparing_worktree: bool,
    /// Sends wait for the agent handoff, not for the setup script.
    pub blocks_send: bool,
}

pub fn stage_label(id: WorktreeSetupStageId) -> &'static str {
    match id {
        WorktreeSetupStageId::Fetch => "Fetch base branch",
        WorktreeSetupStageId::Checkout => "Check out files",
        WorktreeSetupStageId::Submodules => "Init submodules",
        WorktreeSetupStageId::SetupScript => "Run setup script",
        WorktreeSetupStageId::Agent => "Start agent",
    }
}

pub fn agent_started(snapshot: &WorktreeSetupSnapshot) -> bool {
    snapshot.stages.iter().any(|stage| {
        stage.id == WorktreeSetupStageId::Agent && stage.status == WorktreeSetupStageStatus::Done
    })
}

fn has_failed_stage(snapshot: &WorktreeSetupSnapshot) -> bool {
    snapshot
        .stages
        .iter()
        .any(|stage| stage.status == WorktreeSetupStageStatus::Failed)
}

/// The newer of the latest streamed snapshot and the one held from earlier,
/// for this thread only. A stream can close or replay an older value during
/// the handoff.
pub fn resolve_setup_snapshot<'a>(
    thread: &ThreadId,
    latest: Option<&'a WorktreeSetupSnapshot>,
    held: Option<&'a WorktreeSetupSnapshot>,
) -> Option<&'a WorktreeSetupSnapshot> {
    let latest = latest.filter(|snapshot| &snapshot.thread == thread);
    let held = held.filter(|snapshot| &snapshot.thread == thread);
    match (latest, held) {
        (Some(latest), Some(held)) if latest.sequence < held.sequence => Some(held),
        (Some(latest), _) => Some(latest),
        (None, held) => held,
    }
}

/// The snapshot to show, and whether the worktree is still being prepared
/// across the local send, the durable preparation and the live stream.
pub fn resolve_setup_progress<'a>(
    thread: &ThreadId,
    local_preparing: bool,
    run_status: Option<RunStatus>,
    latest: Option<&'a WorktreeSetupSnapshot>,
    held: Option<&'a WorktreeSetupSnapshot>,
) -> (Option<&'a WorktreeSetupSnapshot>, bool) {
    let snapshot = resolve_setup_snapshot(thread, latest, held);
    let preparing = local_preparing
        || run_status == Some(RunStatus::Preparing)
        || snapshot.is_some_and(|snapshot| {
            snapshot.phase == WorktreeSetupPhase::Running && !agent_started(snapshot)
        });
    (snapshot, preparing)
}

/// A running setup always shows. The setup belongs to the first turn: after
/// a follow-up it is history. Within the first turn a clean finish leaves once
/// the turn is live, while a failed script, a failed setup or a cancelled one
/// stays so its outcome stays reachable.
pub fn resolve_visible_setup(
    snapshot: Option<&WorktreeSetupSnapshot>,
    turn_started: bool,
    follow_up_sent: bool,
) -> Option<&WorktreeSetupSnapshot> {
    let snapshot = snapshot?;
    if snapshot.phase == WorktreeSetupPhase::Running {
        return Some(snapshot);
    }
    if follow_up_sent {
        return None;
    }
    if snapshot.phase != WorktreeSetupPhase::Done || !turn_started {
        return Some(snapshot);
    }
    has_failed_stage(snapshot).then_some(snapshot)
}

/// The run that owns live work, or the newest run that is not held in the
/// queue once the thread is idle.
fn activity_run(state: &State) -> Option<&Run> {
    state.active_run().or_else(|| {
        state
            .runs
            .iter()
            .filter(|run| !(run.status == RunStatus::Queued && run.queue_held))
            .max_by_key(|run| run.ordinal)
    })
}

/// The setup a thread shows and where its first turn stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupProgress<'a> {
    pub visible: Option<&'a WorktreeSetupSnapshot>,
    pub preparing_worktree: bool,
    /// The thread's activity run has started.
    pub turn_started: bool,
    /// When the agent's live turn started, if one is running.
    pub working_since_ms: Option<i64>,
}

impl SetupProgress<'_> {
    /// Sends wait for the agent handoff, not for the setup script.
    pub fn blocks_send(&self) -> bool {
        match self.visible {
            Some(setup) => setup.phase == WorktreeSetupPhase::Running && !agent_started(setup),
            None => self.preparing_worktree,
        }
    }

    pub fn view(&self, layout: SetupCardLayout, now_ms: i64) -> SetupView {
        SetupView {
            card: self.visible.map(|setup| {
                setup_card(
                    setup,
                    &CardContext {
                        layout,
                        turn_started: self.turn_started,
                        working_since_ms: self.working_since_ms,
                        now_ms,
                    },
                )
            }),
            preparing_worktree: self.preparing_worktree,
            blocks_send: self.blocks_send(),
        }
    }
}

/// The setup of `thread` as the thread screen shows it.
pub fn setup_view(
    snapshot: &Snapshot,
    thread: &ThreadId,
    layout: SetupCardLayout,
    now_ms: i64,
) -> SetupView {
    setup_progress(snapshot, thread).view(layout, now_ms)
}

pub fn setup_progress<'a>(snapshot: &'a Snapshot, thread: &ThreadId) -> SetupProgress<'a> {
    let state = snapshot.thread_state(thread);
    let run = state.and_then(activity_run);
    let local_preparing = snapshot.outbox.pending_launches().any(|entry| {
        &entry.thread == thread
            && matches!(&entry.request, Request::Launch(launch)
                if matches!(launch.workspace, WorkspaceStrategy::Worktree { .. }))
    });
    let (live, preparing_worktree) = resolve_setup_progress(
        thread,
        local_preparing,
        run.map(|run| run.status),
        snapshot.setups.get(thread),
        snapshot.held_setups.get(thread),
    );
    let turn_started = run.is_some_and(|run| run.started_at.is_some());
    let user_messages = state.map_or(0, |state| {
        state
            .messages
            .iter()
            .filter(|message| message.role == Role::User && message.notification.is_none())
            .filter(|message| {
                !state.runs.iter().any(|run| {
                    message.run.as_ref() == Some(&run.id) && run.status == RunStatus::RolledBack
                })
            })
            .count()
    });
    let follow_up_sent =
        user_messages + snapshot.outbox.undelivered_messages(thread, state).len() > 1;
    SetupProgress {
        visible: resolve_visible_setup(live, turn_started, follow_up_sent),
        preparing_worktree,
        turn_started,
        working_since_ms: state
            .and_then(State::active_run)
            .and_then(|run| run.started_at.as_ref())
            .map(Timestamp::millis),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardContext {
    pub layout: SetupCardLayout,
    /// The thread's activity run has started.
    pub turn_started: bool,
    /// When the agent's live turn started, if one is running.
    pub working_since_ms: Option<i64>,
    pub now_ms: i64,
}

pub fn setup_card(snapshot: &WorktreeSetupSnapshot, context: &CardContext) -> SetupCardView {
    let running = snapshot.phase == WorktreeSetupPhase::Running;
    let handed_off = context.turn_started && agent_started(snapshot);
    let desktop = context.layout == SetupCardLayout::Desktop;
    let script_name = snapshot
        .setup_script
        .as_ref()
        .map(|script| script.name.as_str());
    let total = format_duration(
        snapshot
            .ended_at
            .as_ref()
            .map_or(context.now_ms, Timestamp::millis)
            .saturating_sub(snapshot.started_at.millis())
            .max(0),
    );
    let failed_stage = has_failed_stage(snapshot);
    let setup_stage = snapshot
        .stages
        .iter()
        .find(|stage| stage.id == WorktreeSetupStageId::SetupScript);
    let stages = snapshot
        .stages
        .iter()
        .filter(|stage| desktop || stage.id != WorktreeSetupStageId::Agent)
        .map(|stage| stage_view(stage, script_name, context))
        .collect();
    let (title, tone, show_header, elapsed, show_stages, summary, can_cancel, open_terminal_id);
    if desktop {
        title = desktop_title(snapshot).to_string();
        tone = if snapshot.phase == WorktreeSetupPhase::Failed {
            SetupTone::Destructive
        } else if snapshot.phase == WorktreeSetupPhase::Done && failed_stage {
            SetupTone::Warning
        } else {
            SetupTone::Muted
        };
        show_header = !handed_off && !running && snapshot.phase != WorktreeSetupPhase::Done;
        elapsed = Some(total.clone());
        let collapsed = handed_off && !running;
        show_stages = !collapsed;
        summary = collapsed.then(|| SetupSummaryRow {
            status: if matches!(
                snapshot.phase,
                WorktreeSetupPhase::Failed | WorktreeSetupPhase::Cancelled
            ) || setup_stage
                .is_some_and(|stage| stage.status == WorktreeSetupStageStatus::Failed)
            {
                SetupStageStatus::Failed
            } else {
                SetupStageStatus::Done
            },
            label: title.clone(),
            elapsed: Some(total),
        });
        can_cancel = !handed_off && running;
        open_terminal_id = snapshot
            .setup_script
            .as_ref()
            .and_then(|script| script.terminal_id.clone())
            .filter(|_| {
                setup_stage.is_some_and(|stage| stage.status != WorktreeSetupStageStatus::Pending)
            });
    } else {
        let failed = snapshot.phase == WorktreeSetupPhase::Failed || failed_stage;
        title = match context.working_since_ms.filter(|_| handed_off) {
            Some(since) => format!(
                "Working for {}",
                format_duration(context.now_ms.saturating_sub(since).max(0))
            ),
            None => mobile_title(snapshot, handed_off).into(),
        };
        tone = if failed && context.working_since_ms.is_none() {
            SetupTone::Destructive
        } else {
            SetupTone::Muted
        };
        show_header = true;
        elapsed = (!handed_off).then_some(total);
        show_stages = !handed_off;
        summary = None;
        can_cancel = running && !context.turn_started;
        open_terminal_id = None;
    }
    let details = [
        ("Branch", snapshot.branch.as_deref()),
        ("Base", snapshot.base_ref.as_deref()),
        ("Path", snapshot.worktree_path.as_deref()),
        (
            "Setup",
            snapshot
                .setup_script
                .as_ref()
                .map(|script| script.command.as_str()),
        ),
    ]
    .into_iter()
    .filter_map(|(label, value)| {
        value.map(|value| SetupDetail {
            label: label.into(),
            value: value.into(),
        })
    })
    .collect();
    SetupCardView {
        thread_id: snapshot.thread.to_string(),
        phase: snapshot.phase.into(),
        sequence: snapshot.sequence,
        show_in_timeline: !(handed_off && running),
        owns_working_slot: !handed_off
            && matches!(
                snapshot.phase,
                WorktreeSetupPhase::Running | WorktreeSetupPhase::Done
            ),
        handed_off,
        title,
        tone,
        show_header,
        elapsed,
        show_stages,
        stages,
        summary,
        error: snapshot
            .error
            .clone()
            .filter(|_| snapshot.phase == WorktreeSetupPhase::Failed),
        details,
        background_script: (handed_off && running)
            .then(|| script_name.unwrap_or("Setup script").to_string()),
        can_cancel,
        can_work_locally: can_cancel,
        open_terminal_id,
    }
}

fn desktop_title(snapshot: &WorktreeSetupSnapshot) -> &'static str {
    match snapshot.phase {
        WorktreeSetupPhase::Running => "Setting up worktree…",
        WorktreeSetupPhase::Done if has_failed_stage(snapshot) => {
            "Worktree ready, setup script failed"
        }
        WorktreeSetupPhase::Done => "Worktree ready",
        WorktreeSetupPhase::Failed => "Worktree setup failed",
        WorktreeSetupPhase::Cancelled => "Worktree setup cancelled",
    }
}

fn mobile_title(snapshot: &WorktreeSetupSnapshot, handed_off: bool) -> &'static str {
    match snapshot.phase {
        WorktreeSetupPhase::Running if handed_off => "Setup continues…",
        WorktreeSetupPhase::Running => "Setting up worktree…",
        WorktreeSetupPhase::Cancelled => "Worktree setup cancelled",
        WorktreeSetupPhase::Failed => "Worktree setup failed",
        WorktreeSetupPhase::Done if has_failed_stage(snapshot) => "Setup script failed",
        WorktreeSetupPhase::Done => "Worktree ready",
    }
}

fn stage_view(
    stage: &WorktreeSetupStage,
    script_name: Option<&str>,
    context: &CardContext,
) -> SetupStageView {
    use WorktreeSetupStageStatus as Status;
    let label = match script_name {
        Some(name) if stage.id == WorktreeSetupStageId::SetupScript && !name.is_empty() => name,
        _ => stage_label(stage.id),
    };
    let trailing = match stage.status {
        Status::Pending => None,
        Status::Skipped => Some(stage.detail.clone().unwrap_or_else(|| "skipped".into())),
        Status::Running
            if stage.id == WorktreeSetupStageId::Checkout && stage.percent.is_some() =>
        {
            stage.percent.map(|percent| format!("{percent}%"))
        }
        _ => stage.detail.clone(),
    };
    let elapsed = match stage.status {
        Status::Pending | Status::Skipped => None,
        _ => stage.started_at.as_ref().map(|start| {
            format_duration(
                stage
                    .ended_at
                    .as_ref()
                    .map_or(context.now_ms, Timestamp::millis)
                    .saturating_sub(start.millis())
                    .max(0),
            )
        }),
    };
    let show_output = stage.id == WorktreeSetupStageId::SetupScript
        && match context.layout {
            SetupCardLayout::Desktop => {
                stage.status == Status::Failed
                    || (stage.status == Status::Running && !stage.tail.is_empty())
            }
            SetupCardLayout::Mobile => {
                matches!(stage.status, Status::Running | Status::Failed) || !stage.tail.is_empty()
            }
        };
    SetupStageView {
        id: stage.id.into(),
        status: stage.status.into(),
        label: label.into(),
        trailing,
        elapsed,
        output: show_output.then(|| SetupOutputTail {
            lines: output_rows(&stage.tail),
            failed: stage.status == Status::Failed,
        }),
    }
}

fn output_rows(tail: &[String]) -> Vec<String> {
    let skip = tail.len().saturating_sub(SETUP_OUTPUT_TAIL_LINES);
    let shown = &tail[skip..];
    let mut rows = vec![String::new(); SETUP_OUTPUT_TAIL_LINES - shown.len()];
    rows.extend(shown.iter().cloned());
    rows
}

#[cfg(test)]
mod tests;

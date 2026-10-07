//! One thread's screen: its header, timeline rows, composer and the panels
//! around them, from the published snapshot.
use crate::commands::outbox::Request;
use crate::state::Snapshot;
use crate::sync::{ThreadStatus, ThreadSync};
use crate::view::agents::{AgentRoster, agent_roster};
use crate::view::checkpoints::{
    DiffPanelSelection, DiffPanelView, checkpoint_summaries, diff_panel,
};
use crate::view::composer::view::{
    ComposerOptions, ComposerView, ThreadComposer, assemble, composer_draft,
};
use crate::view::header::{HeaderPanelState, ThreadHeaderView, snapshot_thread_header};
use crate::view::plan::{PlanView, plan_view};
use crate::view::projects::scripts::{ProjectScriptsView, project_scripts};
use crate::view::queue::{QueueShortcuts, QueueView, queue_view};
use crate::view::relationships::{LineagePanel, lineage_panel};
use crate::view::requests::{RequestsView, thread_requests_view};
use crate::view::setup_card::{SetupCardLayout, SetupView, setup_progress};
use crate::view::terminals::{TerminalTab, terminal_tabs};
use crate::view::timeline::banners::{
    ThreadErrorBanner, UsageLimitRecovery, thread_error_banner, usage_limit_recovery,
};
use crate::view::timeline::mobile_follow::{StreamingMessageMark, latest_streaming_message};
use crate::view::timeline::rows::{
    ChangedFilesExpansion, TimelineLayout, TimelineOptions, TimelineRow, TimelineSource,
};
use crate::view::working_status::{
    ConnectionStatus, FloatingStatusInput, PillSegment, ThreadContentKind, ThreadCreation,
    WorkingControlView, floating_working_status, queued_count, thread_sync_label, working_control,
};
use agent_domain::{RunAttemptId, RunId, ThreadId, ThreadShell};
use agent_protocol::conversation::WorkspaceStrategy;
use std::collections::{BTreeSet, HashMap};

/// The folder disclosure of one run's changed-files card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ChangedFilesDisclosure {
    pub run_id: String,
    pub all_expanded: bool,
    /// Folder path to expanded.
    pub overrides: HashMap<String, bool>,
}

/// What the client has expanded in the timeline.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TimelineDisclosure {
    pub expanded_runs: Vec<String>,
    pub expanded_attempts: Vec<String>,
    pub expanded_work_groups: Vec<String>,
    /// Work-log entries whose detail is open, by entry id.
    pub expanded_entries: Vec<String>,
    pub changed_files: Vec<ChangedFilesDisclosure>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadViewOptions {
    pub layout: TimelineLayout,
    pub disclosure: TimelineDisclosure,
    pub panels: HeaderPanelState,
    pub composer: ComposerOptions,
    /// The list is scrolled away from its end (the mobile working control).
    pub show_scroll_to_end: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadHistoryView {
    /// Older messages can be loaded.
    pub has_more: bool,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadView {
    pub thread_id: String,
    pub sync_status: ThreadStatus,
    /// `None` until the Host has the thread.
    pub header: Option<ThreadHeaderView>,
    pub rows: Vec<TimelineRow>,
    /// Advances whenever `rows` change; equal revisions mean equal rows.
    pub rows_revision: u64,
    /// The response streaming into the feed, for its haptic ticks.
    pub streaming_message: Option<StreamingMessageMark>,
    pub history: ThreadHistoryView,
    pub composer: ComposerView,
    pub queue: Option<QueueView>,
    pub requests: RequestsView,
    pub plan: Option<PlanView>,
    pub agents: Option<AgentRoster>,
    pub lineage: Option<LineagePanel>,
    pub setup: SetupView,
    pub working: Option<WorkingControlView>,
    pub error_banner: Option<ThreadErrorBanner>,
    pub limit_recovery: Option<UsageLimitRecovery>,
    pub diff: DiffPanelView,
    pub terminals: Vec<TerminalTab>,
    pub scripts: Option<ProjectScriptsView>,
}

/// The thread's list row as its folded state derives it, or else as listed.
pub fn thread_shell(snapshot: &Snapshot, thread: &ThreadId) -> Option<ThreadShell> {
    snapshot
        .thread_state(thread)
        .and_then(agent_domain::shell)
        .or_else(|| snapshot.thread_row(thread).cloned())
}

fn ids<T>(values: &[String], parse: impl Fn(String) -> Option<T>) -> BTreeSet<T>
where
    T: Ord,
{
    values.iter().cloned().filter_map(parse).collect()
}

fn timeline_options(
    snapshot: &Snapshot,
    thread: &ThreadId,
    layout: TimelineLayout,
    disclosure: &TimelineDisclosure,
) -> TimelineOptions {
    TimelineOptions {
        layout,
        expanded_runs: ids(&disclosure.expanded_runs, |id| RunId::new(id).ok()),
        expanded_attempts: ids(&disclosure.expanded_attempts, |id| {
            RunAttemptId::new(id).ok()
        }),
        expanded_work_groups: disclosure.expanded_work_groups.iter().cloned().collect(),
        expanded_entries: disclosure.expanded_entries.iter().cloned().collect(),
        changed_files: disclosure
            .changed_files
            .iter()
            .filter_map(|card| {
                Some((
                    RunId::new(card.run_id.clone()).ok()?,
                    ChangedFilesExpansion {
                        all_expanded: card.all_expanded,
                        overrides: card
                            .overrides
                            .iter()
                            .map(|(path, open)| (path.clone(), *open))
                            .collect(),
                    },
                ))
            })
            .collect(),
        workspace_root: Some(snapshot.thread_cwd(thread)).filter(|root| !root.is_empty()),
        reverting: snapshot
            .rollbacks
            .values()
            .any(|rollback| &rollback.thread == thread),
    }
}

/// `None` for a thread neither the Host nor this device's outbox knows.
pub fn thread_view(
    snapshot: &Snapshot,
    thread: &ThreadId,
    now_ms: i64,
    options: &ThreadViewOptions,
) -> Option<ThreadView> {
    let synced = snapshot.thread(thread);
    if synced.is_none() && snapshot.thread_row(thread).is_none() && !snapshot.outbox.creates(thread)
    {
        return None;
    }
    let unsynced = ThreadSync::default();
    let sync = synced.unwrap_or(&unsynced);
    let state = sync.state.as_deref();
    let shell = thread_shell(snapshot, thread);
    let pending = snapshot.outbox.undelivered_messages(thread, state);
    let progress = setup_progress(snapshot, thread);
    let send_started_at = snapshot
        .outbox
        .entries
        .iter()
        .filter(|entry| &entry.thread == thread && entry.message().is_some())
        .map(|entry| &entry.created_at)
        .max();
    let timeline = {
        let mut cache = snapshot
            .timelines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.retain(|id| id == thread || snapshot.threads.contains_key(id));
        cache.timeline(
            TimelineSource {
                thread,
                sync,
                shell: shell.as_ref(),
                pending: &pending,
                setup: progress.visible,
                send_started_at,
                now_ms,
            },
            &timeline_options(snapshot, thread, options.layout, &options.disclosure),
        )
    };

    let (draft_key, draft) = composer_draft(snapshot, Some(thread));
    let requests = state.map(|state| thread_requests_view(snapshot, state));
    let plan =
        state.map(|state| plan_view(state, draft.interaction_mode, !draft.attachments.is_empty()));
    let composer = assemble(
        snapshot,
        draft_key,
        draft,
        Some(ThreadComposer {
            thread,
            state,
            shell: shell.as_ref(),
            requests: requests.as_ref(),
            plan: plan.as_ref(),
            preparing_worktree: progress.preparing_worktree,
        }),
        &options.composer,
    );
    let editing = snapshot
        .editing_run
        .as_ref()
        .filter(|_| snapshot.selected_thread.as_ref() == Some(thread));
    let shortcuts = &options.composer.shortcuts;
    let queue = state.map(|state| {
        queue_view(
            state,
            &snapshot.outbox,
            editing,
            QueueShortcuts {
                steer: shortcuts.queue_steer.as_deref(),
                edit: shortcuts.queue_edit.as_deref(),
            },
        )
    });

    let agents = agent_roster(snapshot, thread, now_ms);
    let content = if state.is_some() {
        ThreadContentKind::Ready
    } else if sync.status == ThreadStatus::Deleted {
        ThreadContentKind::Unavailable
    } else {
        ThreadContentKind::Loading
    };
    let status = floating_working_status(
        state,
        &FloatingStatusInput {
            // Core tracks only whether the Host is reachable.
            connection_phase: if snapshot.connected {
                ConnectionStatus::Connected
            } else {
                ConnectionStatus::Reconnecting
            },
            connection_error: None,
            environment_label: snapshot.host_name.clone(),
            sync_label: thread_sync_label(sync.status, content),
            content,
            creation: snapshot
                .outbox
                .pending_launches()
                .find(|entry| &entry.thread == thread)
                .map(|entry| ThreadCreation::Preparing {
                    worktree: matches!(&entry.request, Request::Launch(launch)
                        if matches!(launch.workspace, WorkspaceStrategy::Worktree { .. })),
                }),
            worktree_setup_visible: progress.visible.is_some(),
            setup_awaiting_turn: progress.visible.is_some() && !progress.turn_started,
        },
    );
    let working = working_control(
        status,
        agents
            .as_ref()
            .and_then(|roster| roster.pill.as_ref())
            .map(|pill| PillSegment {
                label: pill.label.clone(),
                accessibility_label: pill.accessibility_label.clone(),
            }),
        state.map_or(0, queued_count),
        options.show_scroll_to_end,
        now_ms,
    );

    let unselected = DiffPanelSelection::default();
    let preferences = &snapshot.preferences;
    Some(ThreadView {
        thread_id: thread.to_string(),
        sync_status: sync.status,
        header: snapshot_thread_header(snapshot, thread, &options.panels),
        rows: timeline.rows.as_ref().clone(),
        rows_revision: timeline.revision,
        streaming_message: latest_streaming_message(&timeline.rows),
        history: ThreadHistoryView {
            has_more: sync.history.has_more,
            loading: sync.history.loading,
            error: sync.history.error.clone(),
        },
        composer,
        queue,
        requests: requests.unwrap_or(RequestsView {
            approval: None,
            approval_count: 0,
            questions: None,
            question_request_count: 0,
        }),
        plan,
        lineage: lineage_panel(snapshot, thread, now_ms),
        agents,
        setup: progress.view(
            match options.layout {
                TimelineLayout::Desktop => SetupCardLayout::Desktop,
                TimelineLayout::Mobile => SetupCardLayout::Mobile,
            },
            now_ms,
        ),
        working,
        error_banner: thread_error_banner(
            thread.as_str(),
            snapshot.error.as_deref(),
            shell.as_ref(),
            &snapshot.error_dismissals,
        ),
        limit_recovery: shell
            .as_ref()
            .and_then(|shell| usage_limit_recovery(shell, now_ms)),
        diff: crate::view::checkpoints::DiffPanelView {
            git: Some(crate::view::checkpoints::git_diff_view(
                snapshot,
                &snapshot.thread_cwd(thread),
                snapshot.diff_panels.get(thread).unwrap_or(&unselected),
            )),
            ..diff_panel(
                &state.map(checkpoint_summaries).unwrap_or_default(),
                snapshot.diff_panels.get(thread).unwrap_or(&unselected),
                preferences.diff_ignore_whitespace,
            )
        },
        terminals: terminal_tabs(snapshot, thread),
        scripts: snapshot.thread_project(thread).and_then(|project| {
            project_scripts(
                snapshot,
                project,
                preferences
                    .last_run_scripts
                    .get(project)
                    .map(String::as_str),
            )
        }),
    })
}

/// The selected thread's screen.
pub fn selected_thread_view(
    snapshot: &Snapshot,
    now_ms: i64,
    options: &ThreadViewOptions,
) -> Option<ThreadView> {
    thread_view(
        snapshot,
        snapshot.selected_thread.as_ref()?,
        now_ms,
        options,
    )
}

#[cfg(test)]
#[path = "thread_tests.rs"]
mod tests;

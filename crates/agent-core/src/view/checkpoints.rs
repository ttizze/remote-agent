//! Checkpoints of a thread's runs, the diff panel scope and the requests a
//! scope produces, and which user messages can be edited from.
use crate::state::Intent;
use agent_domain::{CheckpointStatus, InputIntent, ItemKind, Role, State, ThreadId};
use agent_protocol::conversation::{GetTurnDiff, TurnDiff};
use std::collections::BTreeMap;

/// The diff panel hides whitespace-only changes unless the user turns it off.
pub const DEFAULT_DIFF_IGNORE_WHITESPACE: bool = true;
pub const TURN_DIFF_RANGE_ERROR: &str =
    "from_run_ordinal must be less than or equal to to_run_ordinal";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum CheckpointState {
    Ready,
    Missing,
    Error,
    Stale,
}
impl From<CheckpointStatus> for CheckpointState {
    fn from(status: CheckpointStatus) -> Self {
        match status {
            CheckpointStatus::Ready => Self::Ready,
            CheckpointStatus::Missing => Self::Missing,
            CheckpointStatus::Error => Self::Error,
            CheckpointStatus::Stale => Self::Stale,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ChangedFile {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}

/// One run's checkpoint as review views show it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct CheckpointSummary {
    pub checkpoint_id: String,
    pub scope_id: Option<String>,
    pub run_id: String,
    /// The run ordinal the checkpoint was captured after.
    pub turn_count: u64,
    pub checkpoint_ref: String,
    pub status: CheckpointState,
    pub files: Vec<ChangedFile>,
    pub additions: u64,
    pub deletions: u64,
    /// "N changed files".
    pub changed_files_label: String,
    /// The run's last assistant message, which carries the changed-files card.
    pub assistant_message_id: Option<String>,
    /// When the run completed; the checkpoint is captured as it settles.
    pub completed_at_ms: Option<i64>,
    /// The checkpoint menu offers "Roll back".
    pub can_roll_back: bool,
}

/// Checkpoints that belong to a run, in capture order.
pub fn checkpoint_summaries(state: &State) -> Vec<CheckpointSummary> {
    state
        .checkpoints
        .iter()
        .filter_map(|checkpoint| {
            let run_id = checkpoint.run.as_ref()?;
            let run = state.runs.iter().find(|run| &run.id == run_id);
            let assistant_message_id = state
                .messages
                .iter()
                .rev()
                .find(|message| {
                    message.run.as_ref() == Some(run_id) && message.role == Role::Assistant
                })
                .map(|message| message.id.to_string());
            let files: Vec<ChangedFile> = checkpoint
                .files
                .iter()
                .map(|file| ChangedFile {
                    path: file.path.clone(),
                    kind: file.kind.clone(),
                    additions: file.additions,
                    deletions: file.deletions,
                })
                .collect();
            Some(CheckpointSummary {
                checkpoint_id: checkpoint.id.to_string(),
                scope_id: checkpoint.scope.as_ref().map(|scope| scope.id.to_string()),
                run_id: run_id.to_string(),
                turn_count: checkpoint.run_ordinal,
                checkpoint_ref: checkpoint.file_ref.clone(),
                status: checkpoint.status.into(),
                additions: files.iter().map(|file| file.additions).sum(),
                deletions: files.iter().map(|file| file.deletions).sum(),
                changed_files_label: changed_files_label(files.len()),
                files,
                assistant_message_id,
                completed_at_ms: run
                    .and_then(|run| run.completed_at.as_ref())
                    .map(|at| at.millis()),
                can_roll_back: checkpoint.status == CheckpointStatus::Ready,
            })
        })
        .collect()
}

fn changed_files_label(count: usize) -> String {
    format!("{count} changed file{}", if count == 1 { "" } else { "s" })
}

/// "Edit from here" on a user message rolls back to the checkpoint before its
/// run.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct RevertTarget {
    pub message_id: String,
    pub turn_count: u64,
}

/// The user messages that start a run with a ready checkpoint, in timeline
/// order. Steers into a run never become targets.
pub fn revert_targets(state: &State, checkpoints: &[CheckpointSummary]) -> Vec<RevertTarget> {
    let ready: BTreeMap<&str, &CheckpointSummary> = checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.status == CheckpointState::Ready)
        .map(|checkpoint| (checkpoint.run_id.as_str(), checkpoint))
        .collect();
    let mut targets: Vec<RevertTarget> = Vec::new();
    for item in state.visible_items() {
        let ItemKind::UserMessage { message } = &item.kind else {
            continue;
        };
        let Some(message) = state.message(message) else {
            continue;
        };
        if message.role != Role::User
            || !matches!(
                message.intent,
                InputIntent::TurnStart | InputIntent::QueuedTurn
            )
        {
            continue;
        }
        let Some(checkpoint) = message.run.as_ref().and_then(|run| ready.get(run.as_str())) else {
            continue;
        };
        let message_id = message.id.to_string();
        let turn_count = checkpoint.turn_count.saturating_sub(1);
        match targets
            .iter_mut()
            .find(|target| target.message_id == message_id)
        {
            Some(target) => target.turn_count = turn_count,
            None => targets.push(RevertTarget {
                message_id,
                turn_count,
            }),
        }
    }
    targets
}

/// A turn diff from the checkpoint after `from` to the one after `to`.
pub fn turn_diff_request(
    thread: &ThreadId,
    from_run_ordinal: u64,
    to_run_ordinal: u64,
    ignore_whitespace: bool,
) -> Result<GetTurnDiff, String> {
    if from_run_ordinal > to_run_ordinal {
        return Err(TURN_DIFF_RANGE_ERROR.into());
    }
    Ok(GetTurnDiff {
        thread_id: thread.clone(),
        from_run_ordinal,
        to_run_ordinal,
        ignore_whitespace: Some(ignore_whitespace),
    })
}

/// Everything the thread changed up to the checkpoint after `to`.
pub fn full_thread_diff_request(
    thread: &ThreadId,
    to_run_ordinal: u64,
    ignore_whitespace: bool,
) -> GetTurnDiff {
    GetTurnDiff {
        thread_id: thread.clone(),
        from_run_ordinal: 0,
        to_run_ordinal,
        ignore_whitespace: Some(ignore_whitespace),
    }
}

/// A received diff is usable only with an ordered range.
pub fn accepts_turn_diff(diff: &TurnDiff) -> bool {
    diff.from_run_ordinal <= diff.to_run_ordinal
}

/// What the diff panel shows for a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DiffSelection {
    /// "Changes": everything the checkout changed since its base.
    Branch { base_ref: Option<String> },
    /// "Uncommitted".
    Unstaged,
    Turn {
        run_id: String,
        file_path: Option<String>,
        /// Grows each time the same file is opened again, so the view scrolls
        /// to it again.
        reveal_request_id: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DiffGitScope {
    Branch,
    Unstaged,
}

/// The diff panel selection of one thread, remembering the branch base while
/// another scope is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiffPanelSelection {
    pub selection: DiffSelection,
    pub branch_base_ref: Option<String>,
}
impl Default for DiffPanelSelection {
    fn default() -> Self {
        Self {
            selection: DiffSelection::Branch { base_ref: None },
            branch_base_ref: None,
        }
    }
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Into::into)
}

impl DiffPanelSelection {
    pub fn select_git_scope(&mut self, scope: DiffGitScope) {
        let base_ref = match &self.selection {
            DiffSelection::Branch { base_ref } => {
                self.branch_base_ref = base_ref.clone();
                base_ref.clone()
            }
            _ => self.branch_base_ref.clone(),
        };
        self.selection = match scope {
            DiffGitScope::Branch => DiffSelection::Branch { base_ref },
            DiffGitScope::Unstaged => DiffSelection::Unstaged,
        };
    }
    pub fn select_branch_base_ref(&mut self, base_ref: Option<&str>) {
        let base_ref = trimmed(base_ref);
        self.branch_base_ref = base_ref.clone();
        self.selection = DiffSelection::Branch { base_ref };
    }
    pub fn select_turn(&mut self, run_id: &str, file_path: Option<&str>) {
        let reveal_request_id = match &self.selection {
            DiffSelection::Turn {
                reveal_request_id, ..
            } => reveal_request_id + 1,
            _ => 1,
        };
        self.selection = DiffSelection::Turn {
            run_id: run_id.into(),
            file_path: trimmed(file_path),
            reveal_request_id,
        };
    }
    /// A selected turn that no longer exists moves to the latest turn.
    /// `available` is newest first.
    pub fn reconcile_turn_selection(&mut self, available: &[String]) {
        let DiffSelection::Turn { run_id, .. } = &mut self.selection else {
            return;
        };
        let Some(latest) = available.first() else {
            return;
        };
        if !available.contains(run_id) {
            *run_id = latest.clone();
        }
    }
    /// Applies a choice from the scope menu. `turns` is newest first.
    pub fn select_scope(&mut self, choice: &DiffScopeChoice, turns: &[CheckpointSummary]) {
        match choice {
            DiffScopeChoice::Branch => self.select_git_scope(DiffGitScope::Branch),
            DiffScopeChoice::Unstaged => self.select_git_scope(DiffGitScope::Unstaged),
            DiffScopeChoice::LatestTurn => {
                if let Some(latest) = turns.first() {
                    self.select_turn(&latest.run_id, None);
                }
            }
            DiffScopeChoice::Turn { run_id } => {
                if turns.iter().any(|turn| &turn.run_id == run_id) {
                    self.select_turn(run_id, None);
                }
            }
        }
    }
}

/// Checkpoints newest first: by turn count, then by completion.
pub fn ordered_turns(checkpoints: &[CheckpointSummary]) -> Vec<CheckpointSummary> {
    let mut turns = checkpoints.to_vec();
    turns.sort_by(|left, right| {
        right
            .turn_count
            .cmp(&left.turn_count)
            .then_with(|| right.completed_at_ms.cmp(&left.completed_at_ms))
    });
    turns
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DiffScopeChoice {
    Branch,
    Unstaged,
    LatestTurn,
    Turn { run_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiffScopeOption {
    pub choice: DiffScopeChoice,
    pub label: String,
    pub selected: bool,
}

/// An entry of the "Turn" submenu.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiffTurnOption {
    pub run_id: String,
    pub turn_count: u64,
    pub label: String,
    pub completed_at_ms: Option<i64>,
    pub selected: bool,
}

/// The diff the selected scope needs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DiffRequest {
    Branch {
        base_ref: Option<String>,
        ignore_whitespace: bool,
    },
    Unstaged {
        ignore_whitespace: bool,
    },
    Turn {
        run_id: String,
        from_run_ordinal: u64,
        to_run_ordinal: u64,
        ignore_whitespace: bool,
    },
}
impl DiffRequest {
    /// The intent that loads this diff. `cwd` is the thread's checkout. The
    /// Host has no branch-range diff, so "Changes" loads nothing.
    pub fn intent(&self, cwd: Option<&str>) -> Option<Intent> {
        match self {
            Self::Branch { .. } => None,
            Self::Unstaged { .. } => cwd.map(|cwd| Intent::ReviewWorkspace { cwd: cwd.into() }),
            Self::Turn {
                from_run_ordinal,
                to_run_ordinal,
                ignore_whitespace,
                ..
            } => Some(Intent::ReadTurnDiff {
                from_run_ordinal: *from_run_ordinal,
                to_run_ordinal: *to_run_ordinal,
                ignore_whitespace: *ignore_whitespace,
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DiffPanelView {
    /// The scope menu button; its accessibility label is "Diff scope: <label>".
    pub scope_label: String,
    pub section_title: String,
    /// "Changes", "Uncommitted" and "Latest turn", in menu order.
    pub scopes: Vec<DiffScopeOption>,
    /// The "Turn" submenu, newest first.
    pub turns: Vec<DiffTurnOption>,
    pub selected_file_path: Option<String>,
    pub reveal_request_id: u64,
    pub request: Option<DiffRequest>,
    pub empty_message: Option<String>,
    pub ignore_whitespace: bool,
    /// The whitespace toggle's label and tooltip.
    pub whitespace_toggle_label: String,
}

pub fn diff_panel(
    checkpoints: &[CheckpointSummary],
    selection: &DiffPanelSelection,
    ignore_whitespace: bool,
) -> DiffPanelView {
    let turns = ordered_turns(checkpoints);
    let latest = turns.first();
    let (selected_run, git_scope, base_ref, file_path, reveal_request_id) =
        match &selection.selection {
            DiffSelection::Branch { base_ref } => {
                (None, DiffGitScope::Branch, base_ref.clone(), None, 0)
            }
            DiffSelection::Unstaged => (None, DiffGitScope::Unstaged, None, None, 0),
            DiffSelection::Turn {
                run_id,
                file_path,
                reveal_request_id,
            } => (
                Some(run_id.as_str()),
                DiffGitScope::Branch,
                None,
                file_path.clone(),
                *reveal_request_id,
            ),
        };
    let selected_turn = selected_run.and_then(|run_id| {
        turns
            .iter()
            .find(|turn| turn.run_id == run_id)
            .or(turns.first())
    });
    let is_latest = selected_turn.map(|turn| &turn.run_id) == latest.map(|turn| &turn.run_id);
    let git_label = match git_scope {
        DiffGitScope::Unstaged => "Uncommitted",
        DiffGitScope::Branch => "Changes",
    };
    let scope_label = match (selected_run, selected_turn) {
        (None, _) => git_label.into(),
        (Some(_), Some(turn)) if !is_latest => turn_label(turn.turn_count),
        (Some(_), _) => "Latest turn".into(),
    };
    let section_title =
        selected_turn.map_or_else(|| git_label.into(), |turn| turn_label(turn.turn_count));
    let selected_scope = match selected_run {
        None => Some(match git_scope {
            DiffGitScope::Branch => DiffScopeChoice::Branch,
            DiffGitScope::Unstaged => DiffScopeChoice::Unstaged,
        }),
        Some(_) if is_latest => Some(DiffScopeChoice::LatestTurn),
        Some(_) => None,
    };
    let scopes = [
        (DiffScopeChoice::Branch, "Changes"),
        (DiffScopeChoice::Unstaged, "Uncommitted"),
        (DiffScopeChoice::LatestTurn, "Latest turn"),
    ]
    .into_iter()
    .map(|(choice, label)| DiffScopeOption {
        selected: selected_scope.as_ref() == Some(&choice),
        choice,
        label: label.into(),
    })
    .collect();
    let request = match (selected_run, selected_turn) {
        (None, _) => Some(match git_scope {
            DiffGitScope::Branch => DiffRequest::Branch {
                base_ref,
                ignore_whitespace,
            },
            DiffGitScope::Unstaged => DiffRequest::Unstaged { ignore_whitespace },
        }),
        (Some(_), Some(turn)) => Some(DiffRequest::Turn {
            run_id: turn.run_id.clone(),
            from_run_ordinal: turn.turn_count.saturating_sub(1),
            to_run_ordinal: turn.turn_count,
            ignore_whitespace,
        }),
        (Some(_), None) => None,
    };
    DiffPanelView {
        scope_label,
        section_title,
        scopes,
        turns: turns
            .iter()
            .map(|turn| DiffTurnOption {
                run_id: turn.run_id.clone(),
                turn_count: turn.turn_count,
                label: turn_label(turn.turn_count),
                completed_at_ms: turn.completed_at_ms,
                selected: selected_turn.is_some_and(|selected| selected.run_id == turn.run_id),
            })
            .collect(),
        selected_file_path: file_path,
        reveal_request_id,
        request,
        empty_message: (selected_run.is_some() && turns.is_empty())
            .then(|| "No completed turns yet.".into()),
        ignore_whitespace,
        whitespace_toggle_label: if ignore_whitespace {
            "Show whitespace changes"
        } else {
            "Hide whitespace changes"
        }
        .into(),
    }
}

fn turn_label(turn_count: u64) -> String {
    format!("Turn {turn_count}")
}

#[cfg(test)]
mod tests;

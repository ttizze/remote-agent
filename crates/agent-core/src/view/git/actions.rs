use super::terminology::{ChangeRequestTerminology, change_request_terminology};
use agent_protocol::vcs::{ActionProgressEvent, ActionProgressKind, StackedAction};
use agent_protocol::workspace::VcsStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum GitAction {
    Commit,
    Push,
    CreatePr,
    CommitPush,
    CommitPushPr,
    OpenPr,
}

impl GitAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Push => "push",
            Self::CreatePr => "create_pr",
            Self::CommitPush => "commit_push",
            Self::CommitPushPr => "commit_push_pr",
            Self::OpenPr => "open_pr",
        }
    }

    pub fn protocol(self) -> Option<StackedAction> {
        match self {
            Self::Commit => Some(StackedAction::Commit),
            Self::Push => Some(StackedAction::Push),
            Self::CreatePr => Some(StackedAction::CreatePr),
            Self::CommitPush => Some(StackedAction::CommitPush),
            Self::CommitPushPr => Some(StackedAction::CommitPushPr),
            Self::OpenPr => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum GitActionIcon {
    Commit,
    Push,
    PullRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GitActionMenuItem {
    pub id: String,
    pub label: String,
    pub disabled: bool,
    pub icon: GitActionIcon,
    pub action: GitAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum GitQuickActionKind {
    RunAction,
    Pull,
    OpenPr,
    OpenPublish,
    Hint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GitQuickAction {
    pub label: String,
    pub disabled: bool,
    pub kind: GitQuickActionKind,
    pub action: Option<GitAction>,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DefaultBranchActionCopy {
    pub title: String,
    pub description: String,
    pub continue_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GitActionProgressView {
    pub phase: Option<String>,
    pub label: Option<String>,
    pub status: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub finished: bool,
    pub failed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct GitActionResultTiming {
    pub dismiss_after_visible_ms: Option<u64>,
}

fn terminology(status: &VcsStatus) -> ChangeRequestTerminology {
    change_request_terminology(
        status
            .source_control_provider
            .as_ref()
            .map(|provider| provider.kind),
    )
}

fn action_item(
    id: &str,
    label: String,
    disabled: bool,
    icon: GitActionIcon,
    action: GitAction,
) -> GitActionMenuItem {
    GitActionMenuItem {
        id: id.into(),
        label,
        disabled,
        icon,
        action,
    }
}

/// Menu entries match the available refs, working tree and provider state.
pub fn build_menu_items(
    status: Option<&VcsStatus>,
    busy: bool,
    has_primary_remote: bool,
) -> Vec<GitActionMenuItem> {
    let Some(status) = status else { return vec![] };
    let terminology = terminology(status);
    let has_branch = status.ref_name.is_some();
    let has_changes = status.has_working_tree_changes;
    let has_open_pr = status
        .pr
        .as_ref()
        .is_some_and(|pr| matches!(pr.state, agent_protocol::vcs::ChangeRequestState::Open));
    let can_commit = !busy && has_changes;
    let can_push_without_upstream = has_primary_remote && !status.has_upstream;
    let can_push = !busy
        && has_branch
        && !has_changes
        && status.behind_count == 0
        && status.ahead_count > 0
        && (status.has_upstream || can_push_without_upstream);
    let can_create_pr = !busy
        && has_branch
        && !has_changes
        && !has_open_pr
        && status.ahead_count > 0
        && status.behind_count == 0
        && (status.has_upstream || can_push_without_upstream);
    let commit = action_item(
        "commit",
        "Commit".into(),
        !can_commit,
        GitActionIcon::Commit,
        GitAction::Commit,
    );
    let push = action_item(
        "push",
        "Push".into(),
        !can_push,
        GitActionIcon::Push,
        GitAction::Push,
    );
    vec![
        commit,
        push,
        action_item(
            "pr",
            if has_open_pr {
                format!("View {}", terminology.short_label)
            } else {
                format!("Create {}", terminology.short_label)
            },
            if has_open_pr { busy } else { !can_create_pr },
            GitActionIcon::PullRequest,
            if has_open_pr {
                GitAction::OpenPr
            } else {
                GitAction::CreatePr
            },
        ),
    ]
}

/// The primary action shown beside the branch/status control.
pub fn resolve_quick_action(
    status: Option<&VcsStatus>,
    busy: bool,
    is_default_ref: bool,
    has_primary_remote: bool,
) -> GitQuickAction {
    if busy {
        return hint("Commit", "Git action in progress.");
    }
    let Some(status) = status else {
        return hint("Commit", "Git status is unavailable.");
    };
    let terminology = terminology(status);
    let Some(_) = status.ref_name else {
        return hint(
            "Commit",
            format!(
                "Create and checkout a ref before pushing or opening a {}.",
                terminology.singular
            ),
        );
    };
    let has_changes = status.has_working_tree_changes;
    let has_open_pr = status
        .pr
        .as_ref()
        .is_some_and(|pr| matches!(pr.state, agent_protocol::vcs::ChangeRequestState::Open));
    let is_ahead = status.ahead_count > 0;
    let has_default_delta = status.ahead_of_default_count.unwrap_or(status.ahead_count) > 0;
    let is_behind = status.behind_count > 0;
    if has_changes {
        if !status.has_upstream && !has_primary_remote {
            return run("Commit", GitAction::Commit);
        }
        if has_open_pr || is_default_ref {
            return run("Commit & push", GitAction::CommitPush);
        }
        return run(
            &format!("Commit, push & {}", terminology.short_label),
            GitAction::CommitPushPr,
        );
    }
    if !status.has_upstream {
        if !has_primary_remote {
            return publish();
        }
        if !is_ahead {
            if has_open_pr {
                return open_pr();
            }
            return hint("Push", "No local commits to push.");
        }
        if has_open_pr || is_default_ref {
            return run(
                "Push",
                if is_default_ref {
                    GitAction::CommitPush
                } else {
                    GitAction::Push
                },
            );
        }
        return run(
            &format!("Push & create {}", terminology.short_label),
            GitAction::CreatePr,
        );
    }
    if is_ahead && is_behind {
        return hint(
            "Sync ref",
            "Branch has diverged from upstream. Rebase/merge first.",
        );
    }
    if is_behind {
        return GitQuickAction {
            label: "Pull".into(),
            disabled: false,
            kind: GitQuickActionKind::Pull,
            action: None,
            hint: None,
        };
    }
    if is_ahead {
        if has_open_pr || is_default_ref {
            return run(
                "Push",
                if is_default_ref {
                    GitAction::CommitPush
                } else {
                    GitAction::Push
                },
            );
        }
        return run(
            &format!("Push & create {}", terminology.short_label),
            GitAction::CreatePr,
        );
    }
    if has_open_pr && status.has_upstream {
        return open_pr();
    }
    if has_default_delta && !is_default_ref {
        return run(
            &format!("Create {}", terminology.short_label),
            GitAction::CreatePr,
        );
    }
    hint("Commit", "Branch is up to date. No action needed.")
}

fn run(label: &str, action: GitAction) -> GitQuickAction {
    GitQuickAction {
        label: label.into(),
        disabled: false,
        kind: GitQuickActionKind::RunAction,
        action: Some(action),
        hint: None,
    }
}

fn open_pr() -> GitQuickAction {
    GitQuickAction {
        label: "View pull request".into(),
        disabled: false,
        kind: GitQuickActionKind::OpenPr,
        action: Some(GitAction::OpenPr),
        hint: None,
    }
}

fn publish() -> GitQuickAction {
    GitQuickAction {
        label: "Publish repository".into(),
        disabled: false,
        kind: GitQuickActionKind::OpenPublish,
        action: None,
        hint: None,
    }
}

fn hint(label: &str, message: impl Into<String>) -> GitQuickAction {
    GitQuickAction {
        label: label.into(),
        disabled: true,
        kind: GitQuickActionKind::Hint,
        action: None,
        hint: Some(message.into()),
    }
}

pub fn requires_default_branch_confirmation(action: GitAction, is_default_ref: bool) -> bool {
    is_default_ref
        && matches!(
            action,
            GitAction::Push | GitAction::CreatePr | GitAction::CommitPush | GitAction::CommitPushPr
        )
}

pub fn default_branch_action_copy(
    action: GitAction,
    branch: &str,
    includes_commit: bool,
    terminology: Option<ChangeRequestTerminology>,
) -> DefaultBranchActionCopy {
    let terminology = terminology.unwrap_or_else(|| change_request_terminology(None));
    let suffix = format!(
        " on \"{branch}\". You can continue on this ref or create a feature ref and run the same action there."
    );
    if matches!(action, GitAction::Push | GitAction::CommitPush) {
        if includes_commit {
            return DefaultBranchActionCopy {
                title: "Commit & push to default ref?".into(),
                description: format!("This action will commit and push changes{suffix}"),
                continue_label: format!("Commit & push to {branch}"),
            };
        }
        return DefaultBranchActionCopy {
            title: "Push to default ref?".into(),
            description: format!("This action will push local commits{suffix}"),
            continue_label: format!("Push to {branch}"),
        };
    }
    if includes_commit {
        DefaultBranchActionCopy {
            title: format!(
                "Commit, push & create {} from default ref?",
                terminology.short_label
            ),
            description: format!(
                "This action will commit, push, and create a {}{suffix}",
                terminology.singular
            ),
            continue_label: format!("Commit, push & create {}", terminology.short_label),
        }
    } else {
        DefaultBranchActionCopy {
            title: format!(
                "Push & create {} from default ref?",
                terminology.short_label
            ),
            description: format!(
                "This action will push local commits and create a {}{suffix}",
                terminology.singular
            ),
            continue_label: format!("Push & create {}", terminology.short_label),
        }
    }
}

/// Errors stay until dismissed; success remains visible long enough to open
/// the resulting request.
pub fn result_timing(success: bool) -> GitActionResultTiming {
    GitActionResultTiming {
        dismiss_after_visible_ms: success.then_some(10_000),
    }
}

pub fn format_elapsed(started_at_ms: Option<u64>, now_ms: u64) -> Option<String> {
    let started_at_ms = started_at_ms?;
    let seconds = now_ms.saturating_sub(started_at_ms) / 1_000;
    Some(if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    })
}

pub fn progress_view(event: Option<&ActionProgressEvent>) -> Option<GitActionProgressView> {
    let event = event?;
    let view = match &event.kind {
        ActionProgressKind::ActionStarted { phases } => GitActionProgressView {
            phase: phases.first().map(|phase| format!("{phase:?}")),
            label: None,
            status: "started".into(),
            output: None,
            error: None,
            finished: false,
            failed: false,
        },
        ActionProgressKind::PhaseStarted { phase, label } => GitActionProgressView {
            phase: Some(format!("{phase:?}")),
            label: Some(label.clone()),
            status: "phase_started".into(),
            output: None,
            error: None,
            finished: false,
            failed: false,
        },
        ActionProgressKind::HookStarted { hook_name } => GitActionProgressView {
            phase: None,
            label: Some(hook_name.clone()),
            status: "hook_started".into(),
            output: None,
            error: None,
            finished: false,
            failed: false,
        },
        ActionProgressKind::HookOutput { text, stream, .. } => GitActionProgressView {
            phase: None,
            label: None,
            status: format!("output_{stream:?}"),
            output: Some(text.clone()),
            error: None,
            finished: false,
            failed: false,
        },
        ActionProgressKind::HookFinished { hook_name, .. } => GitActionProgressView {
            phase: None,
            label: Some(hook_name.clone()),
            status: "hook_finished".into(),
            output: None,
            error: None,
            finished: false,
            failed: false,
        },
        ActionProgressKind::ActionFinished { .. } => GitActionProgressView {
            phase: None,
            label: None,
            status: "finished".into(),
            output: None,
            error: None,
            finished: true,
            failed: false,
        },
        ActionProgressKind::ActionFailed { phase, message } => GitActionProgressView {
            phase: phase.map(|phase| format!("{phase:?}")),
            label: None,
            status: "failed".into(),
            output: None,
            error: Some(message.clone()),
            finished: true,
            failed: true,
        },
    };
    Some(view)
}

pub fn thread_branch_update(result: &agent_protocol::vcs::StackedActionResult) -> Option<String> {
    matches!(
        result.branch.status,
        agent_protocol::vcs::BranchStepStatus::Created
    )
    .then(|| result.branch.name.clone())
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::vcs::{ChangeRequestState, VcsStatusChangeRequest};
    use agent_protocol::workspace::{VcsStatus, WorkingTreeChanges};

    fn status() -> VcsStatus {
        VcsStatus {
            is_repo: true,
            source_control_provider: None,
            has_primary_remote: true,
            is_default_ref: false,
            ref_name: Some("feature/test".into()),
            has_working_tree_changes: false,
            working_tree: WorkingTreeChanges {
                files: vec![],
                insertions: 0,
                deletions: 0,
            },
            branch_changes: None,
            has_upstream: true,
            ahead_count: 0,
            behind_count: 0,
            ahead_of_default_count: Some(0),
            pr: None,
        }
    }

    #[test]
    fn clean_branch_with_default_delta_offers_a_pr() {
        let mut status = status();
        status.ahead_of_default_count = Some(2);
        assert_eq!(
            resolve_quick_action(Some(&status), false, false, true).action,
            Some(GitAction::CreatePr)
        );
    }

    #[test]
    fn open_pr_removes_the_create_entry() {
        let mut status = status();
        status.pr = Some(VcsStatusChangeRequest {
            number: 4,
            title: "Open".into(),
            url: "https://github.com/a/b/pull/4".into(),
            base_ref: "main".into(),
            head_ref: "feature/test".into(),
            state: ChangeRequestState::Open,
            is_draft: false,
            updated_at: None,
        });
        let items = build_menu_items(Some(&status), false, true);
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].action, GitAction::OpenPr);
    }

    #[test]
    fn clean_branch_without_a_remote_offers_publish() {
        let mut status = status();
        status.has_primary_remote = false;
        status.has_upstream = false;
        assert_eq!(
            resolve_quick_action(Some(&status), false, false, false).kind,
            GitQuickActionKind::OpenPublish
        );
        assert_eq!(
            resolve_quick_action(Some(&status), false, false, false).label,
            "Publish repository"
        );
    }

    #[test]
    fn elapsed_time_does_not_go_negative() {
        assert_eq!(format_elapsed(Some(10_000), 9_000).as_deref(), Some("0s"));
        assert_eq!(
            format_elapsed(Some(10_000), 75_000).as_deref(),
            Some("1m 5s")
        );
    }
}

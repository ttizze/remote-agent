//! A list row's facts in the shape the list rules read: the latest run, the
//! runtime the row presents and its lifecycle stamps, as epoch milliseconds.
use agent_domain::{
    BackgroundKind, InteractionMode, LinkedPullRequest, RunStatus, ThreadShell, Timestamp,
};

/// A run status, or `Idle` when background work holds the run's completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum RuntimeStatus {
    Preparing,
    Queued,
    Starting,
    Running,
    Waiting,
    Completed,
    Interrupted,
    Failed,
    Cancelled,
    RolledBack,
    Idle,
}
impl From<RunStatus> for RuntimeStatus {
    fn from(status: RunStatus) -> Self {
        match status {
            RunStatus::Preparing => Self::Preparing,
            RunStatus::Queued => Self::Queued,
            RunStatus::Starting => Self::Starting,
            RunStatus::Running => Self::Running,
            RunStatus::Waiting => Self::Waiting,
            RunStatus::Completed => Self::Completed,
            RunStatus::Interrupted => Self::Interrupted,
            RunStatus::Failed => Self::Failed,
            RunStatus::Cancelled => Self::Cancelled,
            RunStatus::RolledBack => Self::RolledBack,
        }
    }
}
impl RuntimeStatus {
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Queued | Self::Starting | Self::Running | Self::Waiting
        )
    }
}

/// Colour distinguishes approval, input, active work and failures; ready is
/// the unlabeled resting state, and waiting is the agent parked on open
/// background work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadListStatus {
    Approval,
    Input,
    Working,
    Waiting,
    Failed,
    Limited,
    Ready,
}
impl ThreadListStatus {
    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Approval => Some("Approval"),
            Self::Input => Some("Input"),
            Self::Working => Some("Working"),
            Self::Failed => Some("Failed"),
            Self::Limited => Some("Limited"),
            Self::Waiting | Self::Ready => None,
        }
    }
}

pub fn thread_list_status(thread: &ThreadSummary) -> ThreadListStatus {
    if thread.has_pending_approvals {
        return ThreadListStatus::Approval;
    }
    if thread.has_pending_user_input {
        return ThreadListStatus::Input;
    }
    let Some(runtime) = &thread.runtime else {
        return ThreadListStatus::Ready;
    };
    match runtime.status {
        status if status.is_active() => ThreadListStatus::Working,
        RuntimeStatus::Idle => ThreadListStatus::Waiting,
        RuntimeStatus::Failed if runtime.last_error_class.as_deref() == Some("usage_limit") => {
            ThreadListStatus::Limited
        }
        RuntimeStatus::Failed => ThreadListStatus::Failed,
        _ => ThreadListStatus::Ready,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub id: String,
    pub status: RuntimeStatus,
    pub requested_at: Option<i64>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSummary {
    pub status: RuntimeStatus,
    pub active_run: Option<String>,
    /// `None` reads the work start from the latest run.
    pub activity_started_at: Option<Option<i64>>,
    pub provider_instance: String,
    pub last_error: Option<String>,
    pub last_error_class: Option<String>,
    pub usage_limit_reset_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettledOverride {
    Settled,
    Active,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadSummary {
    pub id: String,
    pub project: String,
    pub title: String,
    pub instance: String,
    pub model: String,
    pub interaction_mode: InteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub parent: Option<String>,
    /// The parent is a fork source rather than a subagent's owner.
    pub forked: bool,
    pub subagent: bool,
    pub latest_run: Option<RunSummary>,
    pub runtime: Option<RuntimeSummary>,
    pub latest_user_message_at: Option<i64>,
    /// `None` when unknown; then the latest run's request time stands in.
    pub latest_user_authored_message_at: Option<Option<i64>>,
    pub has_pending_approvals: bool,
    pub has_pending_user_input: bool,
    pub has_actionable_proposed_plan: bool,
    pub pending_background: Vec<BackgroundKind>,
    pub provider_instance_history: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
    pub settled_override: Option<SettledOverride>,
    pub settled_at: Option<i64>,
    pub unsettled_at: Option<i64>,
    pub snoozed_until: Option<i64>,
    pub snoozed_at: Option<i64>,
    pub pinned_at: Option<i64>,
    pub auto_settle_disabled: bool,
    pub pin_order_key: Option<String>,
    pub active_order_key: Option<String>,
    pub linked_pull_request: Option<LinkedPullRequest>,
    pub last_visited_at: Option<i64>,
    pub title_regenerating: bool,
    pub limit_recovery_snooze: bool,
}

impl Default for ThreadSummary {
    fn default() -> Self {
        Self {
            id: String::new(),
            project: String::new(),
            title: String::new(),
            instance: String::new(),
            model: String::new(),
            interaction_mode: InteractionMode::Default,
            branch: None,
            worktree_path: None,
            parent: None,
            forked: false,
            subagent: false,
            latest_run: None,
            runtime: None,
            latest_user_message_at: None,
            latest_user_authored_message_at: None,
            has_pending_approvals: false,
            has_pending_user_input: false,
            has_actionable_proposed_plan: false,
            pending_background: vec![],
            provider_instance_history: vec![],
            created_at: 0,
            updated_at: 0,
            archived_at: None,
            settled_override: None,
            settled_at: None,
            unsettled_at: None,
            snoozed_until: None,
            snoozed_at: None,
            pinned_at: None,
            auto_settle_disabled: false,
            pin_order_key: None,
            active_order_key: None,
            linked_pull_request: None,
            last_visited_at: None,
            title_regenerating: false,
            limit_recovery_snooze: false,
        }
    }
}

fn millis(value: &Option<Timestamp>) -> Option<i64> {
    value.as_ref().map(Timestamp::millis)
}

pub fn iso(ms: i64) -> String {
    Timestamp::from_millis(ms)
        .map(|value| value.as_str().to_owned())
        .unwrap_or_default()
}

/// Commands an agent leaves running (a dev server) do not hold a finished
/// run; subagents, monitors and other background tasks wake the agent.
pub fn background_work_holds_completion(kinds: &[BackgroundKind]) -> bool {
    kinds.iter().any(|kind| *kind != BackgroundKind::Command)
}

impl ThreadSummary {
    pub fn from_shell(shell: &ThreadShell) -> Self {
        let updated_at = shell.updated_at.millis();
        let pending_background: Vec<_> = shell
            .pending_background_work
            .iter()
            .map(|work| work.kind)
            .collect();
        let latest_run = shell.latest_run.as_ref().map(|run| RunSummary {
            id: run.to_string(),
            status: shell.status.map_or(RuntimeStatus::Completed, Into::into),
            requested_at: millis(&shell.latest_run_requested_at),
            started_at: millis(&shell.latest_run_started_at),
            completed_at: millis(&shell.latest_run_completed_at),
        });
        let runtime = (shell.latest_run.is_some() || shell.active_run.is_some()).then(|| {
            let park_at_idle = background_work_holds_completion(&pending_background)
                && shell.status != Some(RunStatus::Failed);
            let status = if park_at_idle {
                RuntimeStatus::Idle
            } else {
                shell
                    .activity_run_status
                    .or(shell.status)
                    .map_or(RuntimeStatus::Idle, Into::into)
            };
            RuntimeSummary {
                status,
                active_run: shell.active_run.as_ref().map(ToString::to_string),
                activity_started_at: Some(millis(&shell.activity_run_started_at)),
                provider_instance: shell.selection.instance.clone(),
                last_error: shell.last_error.clone(),
                last_error_class: shell.last_error_class.clone(),
                usage_limit_reset_at: millis(&shell.usage_limit_reset_at),
                updated_at,
            }
        });
        let pending_kind = shell
            .pending_request
            .as_ref()
            .map(|request| request.kind.as_str());
        Self {
            id: shell.id.to_string(),
            project: shell.project.clone(),
            title: shell.title.clone(),
            instance: shell.selection.instance.clone(),
            model: shell.selection.model.clone(),
            interaction_mode: shell.interaction_mode,
            branch: shell
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.branch.clone()),
            worktree_path: shell
                .workspace
                .as_ref()
                .and_then(|workspace| workspace.worktree_path.clone()),
            parent: shell.parent.as_ref().map(ToString::to_string),
            forked: shell.parent.is_some() && shell.fork_boundary.is_some(),
            subagent: false,
            latest_run,
            runtime,
            latest_user_message_at: millis(&shell.latest_user_message_at),
            latest_user_authored_message_at: Some(millis(&shell.latest_user_authored_message_at)),
            has_pending_approvals: pending_kind
                .is_some_and(|kind| kind != "user_input" && kind != "auth_refresh"),
            has_pending_user_input: pending_kind == Some("user_input"),
            has_actionable_proposed_plan: shell.has_actionable_proposed_plan,
            pending_background,
            provider_instance_history: shell.provider_instance_history.clone(),
            created_at: shell.created_at.millis(),
            updated_at,
            archived_at: millis(&shell.archived_at),
            settled_override: shell.settled.map(|settled| {
                if settled {
                    SettledOverride::Settled
                } else {
                    SettledOverride::Active
                }
            }),
            settled_at: millis(&shell.settled_at),
            unsettled_at: None,
            snoozed_until: millis(&shell.snoozed_until),
            snoozed_at: millis(&shell.snoozed_at),
            pinned_at: millis(&shell.pinned_at),
            auto_settle_disabled: !shell.auto_settle,
            pin_order_key: shell.pin_order.clone(),
            active_order_key: shell.active_order.clone(),
            linked_pull_request: shell.linked_pull_request.clone(),
            last_visited_at: millis(&shell.last_visited_at),
            title_regenerating: shell.title_regenerating,
            limit_recovery_snooze: shell
                .limit_recovery
                .as_ref()
                .is_some_and(|recovery| recovery.snooze),
        }
    }

    pub fn runtime_status(&self) -> Option<RuntimeStatus> {
        self.runtime.as_ref().map(|runtime| runtime.status)
    }

    pub fn runtime_is_active(&self) -> bool {
        self.runtime_status().is_some_and(RuntimeStatus::is_active)
    }

    /// Archiving may discard queued work, but must not detach a provider that
    /// is preparing, starting or running a turn.
    pub fn runtime_can_archive(&self) -> bool {
        match &self.runtime {
            Some(runtime) if runtime.status == RuntimeStatus::Queued => {
                runtime.active_run.is_none()
            }
            runtime => !matches!(
                runtime.as_ref().map(|runtime| runtime.status),
                Some(RuntimeStatus::Preparing | RuntimeStatus::Starting | RuntimeStatus::Running)
            ),
        }
    }

    /// Provider instances for a row's trailing stack, back to front: earlier
    /// owners oldest first, the newest two of them, then the current one.
    pub fn provider_stack(&self) -> Vec<String> {
        let current = self
            .runtime
            .as_ref()
            .map_or(&self.instance, |runtime| &runtime.provider_instance);
        let previous: Vec<_> = self
            .provider_instance_history
            .iter()
            .filter(|instance| *instance != current)
            .collect();
        previous[previous.len().saturating_sub(2)..]
            .iter()
            .map(|instance| (*instance).clone())
            .chain([current.clone()])
            .collect()
    }

    /// When the activity-owning run's work started; a wake keeps the start of
    /// the work it continues.
    pub fn working_started_at(&self) -> Option<i64> {
        if let Some(started) = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.activity_started_at)
        {
            return started;
        }
        let run = self.latest_run.as_ref()?;
        let active = self
            .runtime
            .as_ref()
            .and_then(|runtime| runtime.active_run.as_ref());
        (run.completed_at.is_none() && active == Some(&run.id))
            .then(|| run.started_at.or(run.requested_at))
            .flatten()
    }

    /// Completed-but-unseen. Never-visited threads count as read.
    pub fn has_unseen_completion(&self) -> bool {
        let Some(completed_at) = self.latest_run.as_ref().and_then(|run| run.completed_at) else {
            return false;
        };
        self.last_visited_at
            .is_some_and(|visited| completed_at > visited)
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    pub fn ms(iso: &str) -> i64 {
        Timestamp::parse(iso).unwrap().millis()
    }

    pub fn runtime(status: RuntimeStatus) -> RuntimeSummary {
        RuntimeSummary {
            status,
            active_run: None,
            activity_started_at: None,
            provider_instance: "codex".into(),
            last_error: None,
            last_error_class: None,
            usage_limit_reset_at: None,
            updated_at: 0,
        }
    }

    pub fn run(id: &str, status: RuntimeStatus) -> RunSummary {
        RunSummary {
            id: id.into(),
            status,
            requested_at: None,
            started_at: None,
            completed_at: None,
        }
    }

    pub fn summary(id: &str) -> ThreadSummary {
        ThreadSummary {
            id: id.into(),
            project: "project-1".into(),
            title: id.into(),
            instance: "codex".into(),
            model: "gpt-5".into(),
            ..ThreadSummary::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn the_provider_stack_ends_with_the_current_instance_and_keeps_the_newest_two_owners() {
        let mut thread = summary("t");
        thread.instance = "claude".into();
        thread.provider_instance_history = ["a", "b", "claude", "c"].map(String::from).to_vec();
        assert_eq!(thread.provider_stack(), ["b", "c", "claude"]);
        thread.runtime = Some(RuntimeSummary {
            provider_instance: "a".into(),
            ..runtime(RuntimeStatus::Running)
        });
        assert_eq!(thread.provider_stack(), ["claude", "c", "a"]);
    }

    #[test]
    fn working_time_counts_from_the_activity_run_and_falls_back_to_the_owning_run() {
        let mut thread = summary("t");
        thread.runtime = Some(RuntimeSummary {
            activity_started_at: Some(Some(5)),
            ..runtime(RuntimeStatus::Running)
        });
        assert_eq!(thread.working_started_at(), Some(5));
        thread.runtime = Some(RuntimeSummary {
            active_run: Some("run".into()),
            ..runtime(RuntimeStatus::Running)
        });
        thread.latest_run = Some(RunSummary {
            requested_at: Some(3),
            ..run("run", RuntimeStatus::Running)
        });
        assert_eq!(thread.working_started_at(), Some(3));
        thread.latest_run.as_mut().unwrap().completed_at = Some(4);
        assert_eq!(thread.working_started_at(), None);
    }

    #[test]
    fn archiving_waits_for_a_provider_turn_but_not_for_queued_work() {
        let mut thread = summary("t");
        assert!(thread.runtime_can_archive());
        for (status, active, allowed) in [
            (RuntimeStatus::Queued, None, true),
            (RuntimeStatus::Queued, Some("run"), false),
            (RuntimeStatus::Running, None, false),
            (RuntimeStatus::Waiting, None, true),
            (RuntimeStatus::Idle, None, true),
        ] {
            thread.runtime = Some(RuntimeSummary {
                active_run: active.map(String::from),
                ..runtime(status)
            });
            assert_eq!(thread.runtime_can_archive(), allowed, "{status:?}");
        }
    }

    #[test]
    fn a_shell_row_parks_at_idle_while_background_work_holds_its_completion() {
        let state = crate::sync::fixtures::thread_state("Thread");
        let mut shell = agent_domain::shell(&state).unwrap();
        shell.pending_background_work = vec![agent_domain::PendingBackgroundSummary {
            key: "monitor".into(),
            kind: BackgroundKind::Monitor,
            description: "watch".into(),
        }];
        shell.latest_run = Some(agent_domain::RunId::new("run").unwrap());
        shell.status = Some(RunStatus::Completed);
        let summary = ThreadSummary::from_shell(&shell);
        assert_eq!(summary.runtime_status(), Some(RuntimeStatus::Idle));
        shell.status = Some(RunStatus::Failed);
        let summary = ThreadSummary::from_shell(&shell);
        assert_eq!(summary.runtime_status(), Some(RuntimeStatus::Failed));
    }
}

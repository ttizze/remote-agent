//! Scheduled tasks: prompts the Host runs on an interval or at a fixed local
//! time, each run as a new thread or a message into an existing one.
use crate::conversation::WorkspaceStrategy;
use agent_domain::{
    CommandId, InteractionMode, MIN_SCHEDULED_TASK_INTERVAL_MS, MessageAuthor, ModelSelection,
    RuntimeMode, Schedule, ThreadId, Timestamp, parse_time_of_day,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledTaskRunStatus {
    Never,
    Running,
    Succeeded,
    Failed,
}
impl ScheduledTaskRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Never, Self::Running, Self::Succeeded, Self::Failed]
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTask {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub project_id: String,
    /// The thread each run sends into; `None` launches a new thread per run.
    pub thread_id: Option<ThreadId>,
    pub workspace: WorkspaceStrategy,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub created_by: MessageAuthor,
    pub creation_source: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub next_run_at: Option<Timestamp>,
    pub last_run_at: Option<Timestamp>,
    pub last_run_status: ScheduledTaskRunStatus,
    pub last_run_error: Option<String>,
    pub run_count: u64,
}

/// `host/scheduledTasks/subscribe`: the whole list first, then again after
/// every change (saves, run transitions, reschedules).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskList {
    pub tasks: Vec<ScheduledTask>,
}

/// `host/scheduledTasks/upsert`: creates a task, or replaces the definition of
/// the task `id` names while its run history stays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpsertScheduledTask {
    pub id: Option<String>,
    /// Reject the save if the task no longer exists, for edits from a client form.
    pub require_existing: bool,
    /// Stable id for an idempotent create retry. The Host derives the task id
    /// from it when `id` is omitted.
    pub command_id: Option<CommandId>,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub project_id: String,
    pub thread_id: Option<ThreadId>,
    pub workspace: WorkspaceStrategy,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub creation_source: String,
}
impl UpsertScheduledTask {
    /// Why the Host refuses the save, if it does.
    pub fn validate(&self) -> Result<(), String> {
        if self
            .id
            .as_deref()
            .is_some_and(|id| id.is_empty() || id.trim() != id)
        {
            return Err("Scheduled task id must not be empty or padded.".into());
        }
        if self.project_id.is_empty() || self.project_id.trim() != self.project_id {
            return Err("Scheduled task project id must not be empty or padded.".into());
        }
        if self.title.trim().is_empty() || self.title.trim() != self.title {
            return Err("Schedule task title must not be empty.".into());
        }
        if self.prompt.trim().is_empty() || self.prompt.trim() != self.prompt {
            return Err("Schedule task prompt must not be empty.".into());
        }
        let valid_ref = |field: &str, value: Option<&str>, required: bool| {
            if required && value.is_none_or(str::is_empty) {
                return Err(format!("Scheduled task {field} must not be empty."));
            }
            if value.is_some_and(|value| value.trim() != value || value.is_empty()) {
                return Err(format!("Scheduled task {field} must not be empty or padded."));
            }
            Ok(())
        };
        match &self.workspace {
            WorkspaceStrategy::Root { branch } => valid_ref("branch", branch.as_deref(), false)?,
            WorkspaceStrategy::ExistingWorktree {
                worktree_path,
                branch,
            } => {
                valid_ref("worktree path", Some(worktree_path), true)?;
                valid_ref("branch", branch.as_deref(), false)?;
            }
            WorkspaceStrategy::Worktree {
                base_ref,
                branch,
                ..
            } => {
                valid_ref("base ref", Some(base_ref), true)?;
                valid_ref("branch", branch.as_deref(), false)?;
            }
        }
        match &self.schedule {
            Schedule::Interval { every_ms } if *every_ms < MIN_SCHEDULED_TASK_INTERVAL_MS => {
                Err("Interval must be at least 60000 milliseconds (one minute).".into())
            }
            Schedule::Interval { .. } => Ok(()),
            Schedule::FixedTime {
                time_of_day,
                weekdays,
            } => {
                if time_of_day.trim() != time_of_day || parse_time_of_day(time_of_day).is_none() {
                    return Err("Time of day must be a 24-hour HH:MM time.".into());
                }
                if weekdays.iter().any(|day| *day > 6) {
                    return Err("Weekdays must be numbers from 0 (Sunday) to 6 (Saturday).".into());
                }
                Ok(())
            }
        }
    }
}

/// `host/scheduledTasks/setEnabled`: flips only the enabled flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetScheduledTaskEnabled {
    pub id: String,
    pub enabled: bool,
}

/// `host/scheduledTasks/delete` and `host/scheduledTasks/runNow`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskRef {
    pub id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::Driver;

    fn input(schedule: Schedule) -> UpsertScheduledTask {
        UpsertScheduledTask {
            id: None,
            require_existing: false,
            command_id: None,
            title: "Review".into(),
            prompt: "Review the open pull requests.".into(),
            enabled: true,
            schedule,
            project_id: "project".into(),
            thread_id: None,
            workspace: WorkspaceStrategy::Root { branch: None },
            selection: ModelSelection {
                instance: "codex".into(),
                driver: Driver::Codex,
                model: "gpt-5.4".into(),
                options: Default::default(),
            },
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            creation_source: "desktop".into(),
        }
    }

    #[test]
    fn writable_interval_schedules_run_at_most_once_per_minute() {
        assert!(
            input(Schedule::Interval { every_ms: 60_000 })
                .validate()
                .is_ok()
        );
        assert!(
            input(Schedule::Interval { every_ms: 59_999 })
                .validate()
                .is_err()
        );
    }

    #[test]
    fn fixed_time_schedules_need_a_valid_time_and_weekdays() {
        let fixed = |time: &str, weekdays: &[u8]| {
            input(Schedule::FixedTime {
                time_of_day: time.into(),
                weekdays: weekdays.to_vec(),
            })
            .validate()
        };
        assert!(fixed("9:30", &[1, 5]).is_ok());
        assert!(fixed("25:00", &[]).is_err());
        assert!(fixed("09:00", &[7]).is_err());
    }

    #[test]
    fn a_blank_title_or_prompt_is_refused() {
        let mut blank_title = input(Schedule::Interval { every_ms: 60_000 });
        blank_title.title = "  ".into();
        assert!(blank_title.validate().is_err());
        let mut blank_prompt = input(Schedule::Interval { every_ms: 60_000 });
        blank_prompt.prompt = String::new();
        assert!(blank_prompt.validate().is_err());
    }

    #[test]
    fn an_empty_or_padded_id_is_refused() {
        for id in [Some(String::new()), Some(" task ".into())] {
            let mut value = input(Schedule::Interval { every_ms: 60_000 });
            value.id = id;
            assert!(value.validate().is_err());
        }
    }

    #[test]
    fn a_workspace_path_or_base_ref_must_be_trimmed_and_non_empty() {
        let mut value = input(Schedule::Interval { every_ms: 60_000 });
        value.workspace = WorkspaceStrategy::ExistingWorktree {
            worktree_path: " /tmp/worktree".into(),
            branch: None,
        };
        assert!(value.validate().is_err());
        value.workspace = WorkspaceStrategy::Worktree {
            base_ref: String::new(),
            branch: None,
            start_from_origin: true,
        };
        assert!(value.validate().is_err());
    }
}

//! Shared scheduled-task list and editor values. All validation and protocol
//! conversion lives here so desktop and mobile only render and dispatch
//! intents.
use super::models::default_model;
use super::new_thread::{BranchChoice, branch_badge};
use crate::state::{
    ModelOption, RefScope, ScheduledTaskDraft, ScheduledTaskScheduleDraft,
    ScheduledTaskWorkspaceDraft, Snapshot,
};
use agent_domain::{
    CommandId, Driver, InteractionMode, MIN_SCHEDULED_TASK_INTERVAL_MS, ModelSelection,
    RuntimeMode, Schedule, ThreadId, describe_schedule, parse_time_of_day,
};
use agent_protocol::{
    conversation::WorkspaceStrategy,
    scheduled_tasks::{ScheduledTask, UpsertScheduledTask},
};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ScheduledTaskBranchView {
    pub project_id: String,
    pub root: Option<String>,
    pub query: String,
    pub branches: Vec<BranchChoice>,
    pub has_more: bool,
    pub loading: bool,
    pub loading_more: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ScheduledTaskRow {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: ScheduledTaskScheduleDraft,
    pub schedule_label: String,
    pub project_id: String,
    pub thread_id: Option<String>,
    pub workspace: ScheduledTaskWorkspaceDraft,
    pub instance_id: String,
    pub driver: Driver,
    pub model: String,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub next_run_at_ms: Option<i64>,
    pub last_run_at_ms: Option<i64>,
    pub last_run_status: String,
    pub last_run_error: Option<String>,
    pub run_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ScheduledTaskListView {
    pub tasks: Vec<ScheduledTaskRow>,
}

fn schedule(schedule: &Schedule) -> ScheduledTaskScheduleDraft {
    match schedule {
        Schedule::Interval { every_ms } => ScheduledTaskScheduleDraft::Interval {
            every_ms: *every_ms,
        },
        Schedule::FixedTime {
            time_of_day,
            weekdays,
        } => ScheduledTaskScheduleDraft::FixedTime {
            time_of_day: time_of_day.clone(),
            weekdays: canonical_draft_weekdays(weekdays),
        },
    }
}

fn canonical_draft_weekdays(weekdays: &[u8]) -> Vec<u8> {
    let mut weekdays = weekdays.to_vec();
    weekdays.sort_unstable();
    weekdays.dedup();
    if weekdays.is_empty() {
        (0..=6).collect()
    } else {
        weekdays
    }
}

fn workspace(workspace: &WorkspaceStrategy) -> ScheduledTaskWorkspaceDraft {
    match workspace {
        WorkspaceStrategy::Root { branch } => ScheduledTaskWorkspaceDraft::Root {
            branch: branch.clone(),
        },
        WorkspaceStrategy::ExistingWorktree {
            worktree_path,
            branch,
        } => ScheduledTaskWorkspaceDraft::ExistingWorktree {
            worktree_path: worktree_path.clone(),
            branch: branch.clone(),
        },
        WorkspaceStrategy::Worktree {
            base_ref,
            branch,
            start_from_origin,
        } => ScheduledTaskWorkspaceDraft::Worktree {
            base_ref: base_ref.clone(),
            branch: branch.clone(),
            start_from_origin: *start_from_origin,
        },
    }
}

pub fn row(task: &ScheduledTask) -> ScheduledTaskRow {
    ScheduledTaskRow {
        id: task.id.clone(),
        title: task.title.clone(),
        prompt: task.prompt.clone(),
        enabled: task.enabled,
        schedule: schedule(&task.schedule),
        schedule_label: describe_schedule(&task.schedule),
        project_id: task.project_id.clone(),
        thread_id: task.thread_id.as_ref().map(ToString::to_string),
        workspace: workspace(&task.workspace),
        instance_id: task.selection.instance.clone(),
        driver: task.selection.driver,
        model: task.selection.model.clone(),
        runtime_mode: task.runtime_mode,
        interaction_mode: task.interaction_mode,
        next_run_at_ms: task.next_run_at.as_ref().map(|at| at.millis()),
        last_run_at_ms: task.last_run_at.as_ref().map(|at| at.millis()),
        last_run_status: task.last_run_status.as_str().into(),
        last_run_error: task.last_run_error.clone(),
        run_count: task.run_count,
    }
}

pub fn list(snapshot: &Snapshot) -> ScheduledTaskListView {
    ScheduledTaskListView {
        tasks: snapshot.scheduled_tasks.iter().map(row).collect(),
    }
}

/// The branch choices for a scheduled task's worktree base. Ref listings are
/// already shared with the new-thread picker; this view only selects the
/// project root and marks the draft's current base.
pub fn branch_view(
    snapshot: &Snapshot,
    project_id: &str,
    selected_branch: &str,
) -> ScheduledTaskBranchView {
    let root = snapshot
        .shell_projects()
        .iter()
        .find(|project| project.id == project_id)
        .and_then(|project| project.roots.first())
        .map(|root| root.path.clone());
    let entry = root
        .as_deref()
        .and_then(|root| snapshot.sources.refs(root, RefScope::All));
    let branches: Vec<BranchChoice> = entry
        .and_then(|entry| entry.list.as_ref())
        .map(|list| {
            list.refs
                .iter()
                .filter(|reference| !reference.is_remote)
                .map(|reference| BranchChoice {
                    name: reference.name.clone(),
                    current: reference.current,
                    is_default: reference.is_default,
                    worktree_path: reference.worktree_path.clone(),
                    selected: reference.name == selected_branch,
                    badge: root
                        .as_deref()
                        .and_then(|root| branch_badge(reference, root))
                        .map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    ScheduledTaskBranchView {
        project_id: project_id.to_owned(),
        root,
        query: entry.map_or_else(String::new, |entry| entry.query.clone()),
        has_more: entry
            .and_then(|entry| entry.list.as_ref())
            .is_some_and(|list| list.next_cursor.is_some()),
        loading: entry.is_some_and(|entry| entry.in_flight) && branches.is_empty(),
        loading_more: entry.is_some_and(|entry| entry.in_flight) && !branches.is_empty(),
        error: entry.and_then(|entry| entry.error.clone()),
        branches,
    }
}

fn provider_can_run(provider: &crate::models::ProviderInstance) -> bool {
    provider.enabled
        && provider.installed
        && provider.unavailable_reason.is_none()
        && !matches!(
            provider.status,
            crate::models::ProviderStatus::Error | crate::models::ProviderStatus::Disabled
        )
}

fn selectable_selection(snapshot: &Snapshot, selection: &ModelSelection) -> bool {
    snapshot.providers.as_ref().is_some_and(|providers| {
        providers.iter().any(|provider| {
            provider.instance == selection.instance
                && provider.driver == selection.driver
                && provider_can_run(provider)
                && provider
                    .models
                    .iter()
                    .any(|model| model.slug == selection.model)
        })
    })
}

fn selection(snapshot: &Snapshot, task: Option<&ScheduledTask>) -> ModelSelection {
    if let Some(task) = task {
        return task.selection.clone();
    }
    if let Ok(selection) = snapshot.default_draft.selection()
        && selectable_selection(snapshot, &selection)
    {
        return selection;
    }

    snapshot
        .providers
        .as_deref()
        .and_then(default_model)
        .map_or(
            ModelSelection {
                instance: String::new(),
                driver: Driver::Codex,
                model: String::new(),
                options: Default::default(),
            },
            |(provider, model)| ModelSelection {
                instance: provider.instance.clone(),
                driver: provider.driver,
                model: model.slug.clone(),
                options: Default::default(),
            },
        )
}

pub fn draft(snapshot: &Snapshot, id: Option<&str>) -> ScheduledTaskDraft {
    let task = id.and_then(|id| snapshot.scheduled_tasks.iter().find(|task| task.id == id));
    let chosen = selection(snapshot, task);
    match task {
        Some(task) => ScheduledTaskDraft {
            id: Some(task.id.clone()),
            title: task.title.clone(),
            prompt: task.prompt.clone(),
            enabled: task.enabled,
            schedule: schedule(&task.schedule),
            project_id: task.project_id.clone(),
            thread_id: task.thread_id.as_ref().map(ToString::to_string),
            workspace: workspace(&task.workspace),
            instance_id: chosen.instance,
            driver: chosen.driver,
            model: chosen.model,
            options: chosen
                .options
                .into_iter()
                .map(|(key, value)| ModelOption { key, value })
                .collect(),
            runtime_mode: task.runtime_mode,
            interaction_mode: task.interaction_mode,
            creation_source: task.creation_source.clone(),
        },
        None => ScheduledTaskDraft {
            id: None,
            title: String::new(),
            prompt: String::new(),
            enabled: true,
            schedule: ScheduledTaskScheduleDraft::FixedTime {
                time_of_day: "09:00".into(),
                weekdays: vec![1, 2, 3, 4, 5],
            },
            project_id: snapshot
                .selected_project
                .clone()
                .or_else(|| snapshot.shell_projects().first().map(|p| p.id.clone()))
                .unwrap_or_default(),
            thread_id: None,
            workspace: ScheduledTaskWorkspaceDraft::Worktree {
                base_ref: "main".into(),
                branch: None,
                start_from_origin: true,
            },
            instance_id: chosen.instance,
            driver: chosen.driver,
            model: chosen.model,
            options: chosen
                .options
                .into_iter()
                .map(|(key, value)| ModelOption { key, value })
                .collect(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            creation_source: if cfg!(any(target_os = "ios", target_os = "android")) {
                "mobile"
            } else {
                "desktop"
            }
            .into(),
        },
    }
}

fn normalized_weekdays(days: &[u8]) -> Result<Vec<u8>, String> {
    if days.iter().any(|day| *day > 6) {
        return Err("Weekdays must be numbers from 0 (Sunday) to 6 (Saturday).".into());
    }
    if days.is_empty() {
        return Err("Choose at least one day for a fixed-time schedule.".into());
    }
    let mut days = days.to_vec();
    days.sort_unstable();
    days.dedup();
    if days.len() == 7 {
        days.clear();
    }
    Ok(days)
}

fn schedule_from_draft(draft: &ScheduledTaskDraft) -> Result<Schedule, String> {
    match &draft.schedule {
        ScheduledTaskScheduleDraft::Interval { every_ms } => {
            if *every_ms < MIN_SCHEDULED_TASK_INTERVAL_MS {
                return Err("Interval must be at least one minute.".into());
            }
            Ok(Schedule::Interval {
                every_ms: *every_ms,
            })
        }
        ScheduledTaskScheduleDraft::FixedTime {
            time_of_day,
            weekdays,
        } => {
            if time_of_day.trim() != time_of_day || parse_time_of_day(time_of_day).is_none() {
                return Err("Time of day must be a 24-hour HH:MM time.".into());
            }
            Ok(Schedule::FixedTime {
                time_of_day: time_of_day.clone(),
                weekdays: normalized_weekdays(weekdays)?,
            })
        }
    }
}

fn workspace_from_draft(
    workspace: &ScheduledTaskWorkspaceDraft,
) -> Result<WorkspaceStrategy, String> {
    let valid_optional_ref = |value: &Option<String>| {
        value
            .as_deref()
            .is_none_or(|value| !value.is_empty() && value.trim() == value)
    };
    Ok(match workspace {
        ScheduledTaskWorkspaceDraft::Root { branch } if valid_optional_ref(branch) => {
            WorkspaceStrategy::Root {
                branch: branch.clone(),
            }
        }
        ScheduledTaskWorkspaceDraft::ExistingWorktree {
            worktree_path,
            branch,
        } if !worktree_path.trim().is_empty()
            && worktree_path.trim() == worktree_path
            && valid_optional_ref(branch) =>
        {
            WorkspaceStrategy::ExistingWorktree {
                worktree_path: worktree_path.clone(),
                branch: branch.clone(),
            }
        }
        ScheduledTaskWorkspaceDraft::Worktree {
            base_ref,
            branch,
            start_from_origin,
        } if !base_ref.trim().is_empty()
            && base_ref.trim() == base_ref
            && valid_optional_ref(branch) =>
        {
            WorkspaceStrategy::Worktree {
                base_ref: base_ref.clone(),
                branch: branch.clone(),
                start_from_origin: *start_from_origin,
            }
        }
        ScheduledTaskWorkspaceDraft::Root { .. } => return Err("Choose a valid branch.".into()),
        ScheduledTaskWorkspaceDraft::ExistingWorktree { .. } => {
            return Err("Choose an existing worktree.".into());
        }
        ScheduledTaskWorkspaceDraft::Worktree { .. } => {
            return Err("Choose a base branch for the worktree.".into());
        }
    })
}

pub fn upsert(
    draft: &ScheduledTaskDraft,
    command_id: CommandId,
) -> Result<UpsertScheduledTask, String> {
    let title = draft.title.trim();
    if title.is_empty() {
        return Err("Schedule task title must not be empty.".into());
    }
    let prompt = draft.prompt.trim();
    if prompt.is_empty() {
        return Err("Schedule task prompt must not be empty.".into());
    }
    if draft.project_id.trim().is_empty() || draft.project_id.trim() != draft.project_id {
        return Err("Choose a project.".into());
    }
    let thread_id = draft
        .thread_id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .map(|id| ThreadId::new(id).map_err(|error| error.to_string()))
        .transpose()?;
    let selection = ModelSelection {
        instance: draft.instance_id.clone(),
        driver: draft.driver,
        model: draft.model.clone(),
        options: draft
            .options
            .iter()
            .map(|option| (option.key.clone(), option.value.clone()))
            .collect(),
    };
    if selection.instance.trim().is_empty() || selection.model.trim().is_empty() {
        return Err("Choose a model.".into());
    }
    Ok(UpsertScheduledTask {
        id: draft.id.clone(),
        require_existing: draft.id.is_some(),
        command_id: Some(command_id),
        title: title.to_owned(),
        prompt: prompt.to_owned(),
        enabled: draft.enabled,
        schedule: schedule_from_draft(draft)?,
        project_id: draft.project_id.clone(),
        thread_id,
        workspace: workspace_from_draft(&draft.workspace)?,
        selection,
        runtime_mode: draft.runtime_mode,
        interaction_mode: draft.interaction_mode,
        creation_source: if draft.creation_source.trim().is_empty() {
            if cfg!(any(target_os = "ios", target_os = "android")) {
                "mobile".into()
            } else {
                "desktop".into()
            }
        } else {
            draft.creation_source.clone()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{MessageAuthor, Timestamp};

    #[test]
    fn branch_view_reuses_project_refs_and_marks_the_selected_branch() {
        let mut snapshot = Snapshot::default();
        snapshot.shell = std::sync::Arc::new(crate::sync::ShellCache::from_cache(
            agent_protocol::conversation::ShellSnapshot {
                snapshot_sequence: 1,
                projects: vec![crate::models::Project {
                    id: "project".into(),
                    name: "Project".into(),
                    roots: vec![crate::models::ProjectRoot {
                        path: "/repo".into(),
                    }],
                    ..crate::models::Project::default()
                }],
                threads: vec![],
            },
        ));
        snapshot.sources.refs.insert(
            ("/repo".into(), RefScope::All),
            crate::state::RefsEntry {
                query: String::new(),
                list: Some(agent_protocol::workspace::RefList {
                    refs: vec![
                        agent_protocol::workspace::VcsRef {
                            name: "main".into(),
                            is_remote: false,
                            remote_name: None,
                            current: false,
                            is_default: true,
                            worktree_path: None,
                        },
                        agent_protocol::workspace::VcsRef {
                            name: "origin/main".into(),
                            is_remote: true,
                            remote_name: Some("origin".into()),
                            current: false,
                            is_default: false,
                            worktree_path: None,
                        },
                    ],
                    is_repo: true,
                    has_primary_remote: true,
                    next_cursor: None,
                    total_count: 2,
                }),
                error: None,
                in_flight: false,
            },
        );

        let view = branch_view(&snapshot, "project", "main");
        assert_eq!(view.branches.len(), 1);
        assert_eq!(view.branches[0].name, "main");
        assert!(view.branches[0].selected);
        assert_eq!(view.branches[0].badge.as_deref(), Some("default"));
    }

    #[test]
    fn editing_a_daily_task_uses_all_days_and_round_trips_daily() {
        let task = agent_protocol::scheduled_tasks::ScheduledTask {
            id: "daily".into(),
            title: "Daily review".into(),
            prompt: "Review the open pull requests.".into(),
            enabled: true,
            schedule: Schedule::FixedTime {
                time_of_day: "09:00".into(),
                weekdays: vec![],
            },
            project_id: "project".into(),
            thread_id: None,
            workspace: WorkspaceStrategy::Worktree {
                base_ref: "release".into(),
                branch: None,
                start_from_origin: false,
            },
            selection: ModelSelection {
                instance: "codex".into(),
                driver: Driver::Codex,
                model: "gpt-5.4".into(),
                options: [("reasoning", "high")]
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            },
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            created_by: MessageAuthor::User,
            creation_source: "desktop".into(),
            created_at: Timestamp::from_millis(0).unwrap(),
            updated_at: Timestamp::from_millis(0).unwrap(),
            next_run_at: None,
            last_run_at: None,
            last_run_status: agent_protocol::scheduled_tasks::ScheduledTaskRunStatus::Never,
            last_run_error: None,
            run_count: 0,
        };
        let mut snapshot = Snapshot::default();
        snapshot.scheduled_tasks.push(task);
        let mut draft = draft(&snapshot, Some("daily"));
        assert_eq!(
            draft.schedule,
            ScheduledTaskScheduleDraft::FixedTime {
                time_of_day: "09:00".into(),
                weekdays: vec![0, 1, 2, 3, 4, 5, 6],
            }
        );
        assert_eq!(draft.options[0].key, "reasoning");
        draft.title = "Daily review updated".into();
        draft.prompt = "Review the open pull requests.".into();
        let request = upsert(&draft, CommandId::new("command:daily").unwrap()).unwrap();
        assert_eq!(
            request.schedule,
            Schedule::FixedTime {
                time_of_day: "09:00".into(),
                weekdays: vec![],
            }
        );
        assert_eq!(
            request.workspace,
            WorkspaceStrategy::Worktree {
                base_ref: "release".into(),
                branch: None,
                start_from_origin: false,
            }
        );
        assert_eq!(request.selection.options["reasoning"], "high");
    }

    #[test]
    fn an_empty_fixed_time_day_selection_is_rejected() {
        let mut draft = draft(&Snapshot::default(), None);
        draft.title = "Task".into();
        draft.prompt = "Prompt".into();
        draft.instance_id = "codex".into();
        draft.model = "model".into();
        draft.schedule = ScheduledTaskScheduleDraft::FixedTime {
            time_of_day: "09:00".into(),
            weekdays: vec![],
        };
        assert!(upsert(&draft, CommandId::new("command:no-days").unwrap()).is_err());
    }

    #[test]
    fn a_new_task_uses_the_reference_worktree_defaults() {
        let draft = draft(&Snapshot::default(), None);
        assert_eq!(
            draft.workspace,
            ScheduledTaskWorkspaceDraft::Worktree {
                base_ref: "main".into(),
                branch: None,
                start_from_origin: true,
            }
        );
        assert_eq!(
            draft.schedule,
            ScheduledTaskScheduleDraft::FixedTime {
                time_of_day: "09:00".into(),
                weekdays: vec![1, 2, 3, 4, 5],
            }
        );
    }

    #[test]
    fn a_new_task_ignores_a_disabled_default_provider() {
        use crate::view::models::fixtures::{host_instance, host_model};

        let mut disabled = host_instance(
            "codex_disabled",
            Driver::Codex,
            vec![host_model("gpt-disabled", "Disabled")],
        );
        disabled.enabled = false;
        let available = host_instance(
            "codex_available",
            Driver::Codex,
            vec![host_model("gpt-available", "Available")],
        );
        let mut snapshot = Snapshot {
            providers: Some(vec![disabled, available]),
            ..Snapshot::default()
        };
        snapshot.default_draft.instance_id = "codex_disabled".into();
        snapshot.default_draft.driver = Driver::Codex;
        snapshot.default_draft.model = "gpt-disabled".into();

        let draft = draft(&snapshot, None);
        assert_eq!(draft.instance_id, "codex_available");
        assert_eq!(draft.model, "gpt-available");
    }

    #[test]
    fn all_weekdays_are_stored_as_daily_and_duplicates_are_removed() {
        let snapshot = Snapshot::default();
        let mut draft = draft(&snapshot, None);
        draft.schedule = ScheduledTaskScheduleDraft::FixedTime {
            time_of_day: "9:00".into(),
            weekdays: vec![5, 1, 1, 0, 2, 3, 4, 6],
        };
        draft.title = "Morning review".into();
        draft.prompt = "Review".into();
        draft.instance_id = "codex".into();
        draft.model = "model".into();
        let task = upsert(&draft, CommandId::new("command:scheduled").unwrap()).unwrap();
        assert_eq!(
            task.schedule,
            Schedule::FixedTime {
                time_of_day: "9:00".into(),
                weekdays: vec![],
            }
        );
    }

    #[test]
    fn invalid_fixed_time_and_short_interval_are_rejected_before_the_host_call() {
        let snapshot = Snapshot::default();
        let mut draft = draft(&snapshot, None);
        draft.title = "Task".into();
        draft.prompt = "Prompt".into();
        draft.instance_id = "codex".into();
        draft.model = "model".into();
        draft.schedule = ScheduledTaskScheduleDraft::FixedTime {
            time_of_day: "25:00".into(),
            weekdays: vec![],
        };
        assert!(upsert(&draft, CommandId::new("command:a").unwrap()).is_err());
        draft.schedule = ScheduledTaskScheduleDraft::Interval { every_ms: 59_999 };
        assert!(upsert(&draft, CommandId::new("command:b").unwrap()).is_err());
    }

    #[test]
    fn invalid_workspace_values_are_rejected_before_the_host_call() {
        let snapshot = Snapshot::default();
        let mut draft = draft(&snapshot, None);
        draft.title = "Task".into();
        draft.prompt = "Prompt".into();
        draft.instance_id = "codex".into();
        draft.model = "model".into();
        draft.workspace = ScheduledTaskWorkspaceDraft::Worktree {
            base_ref: " main ".into(),
            branch: None,
            start_from_origin: true,
        };
        assert!(upsert(&draft, CommandId::new("command:c").unwrap()).is_err());
    }
}

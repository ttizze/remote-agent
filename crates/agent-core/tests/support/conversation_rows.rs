#[path = "orchestration.rs"]
pub(crate) mod orchestration;
use agent_protocol::{execution::SessionStatus, models::ThreadResponse};
use std::sync::Arc;

pub(crate) fn shell(
    list: agent_protocol::models::ThreadList,
    sequence: u64,
) -> agent_protocol::orchestration::shell::ShellSnapshot {
    use agent_protocol::orchestration::shell::{ProjectShell, ShellSnapshot};
    ShellSnapshot {
        schema_version: 2,
        snapshot_sequence: sequence,
        threads: list
            .data
            .into_iter()
            .filter(|thread| thread.id.is_some())
            .map(|thread| Arc::new(shell_thread(thread)))
            .collect(),
        archived_threads: vec![],
        projects: list
            .projects
            .into_iter()
            .map(|project| {
                Arc::new(ProjectShell {
                    id: project.id,
                    title: project.name,
                    workspace_root: project
                        .roots
                        .first()
                        .map(|root| root.path.clone())
                        .unwrap_or_default(),
                    has_custom_title: false,
                    repository_identity: None,
                    default_model_selection: None,
                    default_thread_env_mode: None,
                    auto_pull: false,
                    favicon_path: None,
                    project_icon: None,
                    scripts: vec![],
                    pinned_at: None,
                    archived_at: None,
                    created_at: "2026-11-14T00:00:00Z".into(),
                    updated_at: "2026-11-14T00:00:00Z".into(),
                })
            })
            .collect(),
    }
}

pub(crate) fn shell_thread(
    thread: agent_protocol::models::Thread,
) -> agent_protocol::orchestration::shell::ThreadShell {
    use agent_protocol::orchestration::{RunStatus, shell::ThreadShell};
    let projection = orchestration::snapshot(
        ThreadResponse {
            thread: thread.clone(),
            ..Default::default()
        },
        0,
    )
    .projection;
    let active = match thread.status {
        SessionStatus::Preparing => Some(RunStatus::Preparing),
        SessionStatus::Running => Some(RunStatus::Running),
        _ => None,
    };
    let latest = projection.runs.last();
    let status = active.or_else(|| latest.map(|run| run.status));
    let mut header = projection.thread;
    if let Some(updated) = thread
        .updated_at
        .and_then(|at| chrono::DateTime::from_timestamp_millis((at * 1000.0) as i64))
    {
        header.updated_at = updated.to_rfc3339();
    }
    ThreadShell {
        latest_native_turn: None,
        thread: header,
        latest_run_id: latest.map(|run| run.id.clone()),
        latest_run_requested_at: latest.map(|run| run.requested_at.clone()),
        latest_run_started_at: latest.and_then(|run| run.started_at.clone()),
        latest_run_completed_at: latest.and_then(|run| run.completed_at.clone()),
        latest_run_stop_requested: false,
        active_run_id: active.map(|_| "fixture-active-run".into()),
        activity_run_status: active,
        activity_run_started_at: None,
        blocked_on_thread_wait: false,
        native_status: None,
        status: status
            .map(Into::into)
            .unwrap_or(agent_protocol::orchestration::shell::ShellStatus::Idle),
        last_error: None,
        last_error_class: None,
        last_error_code: None,
        usage_limit_reset_at: None,
        pending_runtime_request: None,
        latest_visible_message: None,
        latest_user_message_at: None,
        latest_user_authored_message_at: thread
            .last_input_at
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|at| at.to_rfc3339()),
        has_actionable_proposed_plan: false,
        pending_background_tasks: projection
            .provider_threads
            .into_iter()
            .flat_map(|thread| thread.pending_background_tasks)
            .collect(),
        provider_instance_history: vec![],
        item_count: 0,
        visible_item_count: 0,
    }
}

//! Disposable views of canonical shell facts, shared with Host search results.
use super::*;
/// Native display data is disposable; the shell remains the authoritative list.
pub fn summary(
    shell: &ThreadShell,
    project_root: Option<&str>,
) -> Result<crate::models::Thread, String> {
    use crate::{models, session};
    let provider = session::ProviderKind::from_instance_id(&shell.thread.provider_instance_id)?;
    let subagent = shell.thread.is_provider_subagent();
    Ok(models::Thread {
        id: Some(public_session_ref(&shell.thread.id)?),
        active_provider: Some(provider),
        name: Some(shell.thread.title.clone()),
        cwd: shell
            .thread
            .worktree_path
            .as_deref()
            .or(project_root)
            .map(str::to_owned),
        parent_id: if is_listed_lineage(
            shell.thread.lineage.relationship_to_parent.as_deref(),
            shell.thread.lineage.side_chat,
        ) {
            None
        } else {
            shell
                .thread
                .lineage
                .parent_thread_id
                .as_deref()
                .map(public_session_ref)
                .transpose()?
        },
        can_accept_direct_input: Some(!subagent),
        git_branch: shell.thread.branch.clone(),
        project_id: shell
            .thread
            .project_id
            .clone()
            .map(models::ProjectMembership::Assigned)
            .unwrap_or(models::ProjectMembership::Unassigned {}),
        status: shell.task_state().status,
        capabilities: Some(session::Capabilities {
            fork: !subagent,
            rename: !subagent,
            steer: false,
            model_change: false,
            rollback: false,
            feedback: false,
        }),
        updated_at: chrono::DateTime::parse_from_rfc3339(&shell.thread.updated_at)
            .ok()
            .map(|at| at.timestamp_millis() as f64 / 1_000.0),
        // T3 threadSort: the latest human input, else the thread's creation.
        last_input_at: crate::search::authored_activity_ms(
            shell.latest_user_authored_message_at.as_deref(),
            &shell.thread.created_at,
        ),
        history_read_state: Some(session::HistoryReadState::new(
            session::HistoryReadKind::Complete,
            Vec::new(),
        )),
        background: background_tasks(&shell.pending_background_tasks),
        ..Default::default()
    })
}

pub fn background_tasks<'a>(
    tasks: impl IntoIterator<Item = &'a crate::orchestration::PendingBackgroundTask>,
) -> Vec<crate::execution::BackgroundTask> {
    tasks
        .into_iter()
        .map(|task| crate::execution::BackgroundTask {
            id: task.task_id.clone(),
            kind: match task.kind.as_str() {
                "command" => crate::execution::BackgroundTaskKind::Command,
                "subagent" => crate::execution::BackgroundTaskKind::Subagent,
                "monitor" => crate::execution::BackgroundTaskKind::Monitor,
                _ => crate::execution::BackgroundTaskKind::BackgroundTask,
            },
            description: task.description.clone().or_else(|| task.command.clone()),
        })
        .collect()
}

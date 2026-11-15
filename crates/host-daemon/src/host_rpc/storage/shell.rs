//! Sidebar snapshots contain canonical metadata and counts, never transcripts.
use super::*;
use agent_protocol::orchestration::{self as model, shell::*};
use std::collections::BTreeMap;

const PRESENTED: &str = "NOT(candidate.status='queued' AND json_extract(candidate.payload_json,'$.queueHeld') IS 1)
    AND NOT(candidate.status='cancelled' AND json_extract(candidate.payload_json,'$.startedAt') IS NULL
        AND candidate.completed_at IS NOT NULL AND EXISTS(
            SELECT 1 FROM orchestration_v2_projection_runs earlier
            WHERE earlier.thread_id=candidate.thread_id AND earlier.ordinal<candidate.ordinal
              AND earlier.status!='queued'
              AND (earlier.completed_at IS NULL OR julianday(earlier.completed_at)>=julianday(candidate.completed_at))))";

fn last_run(
    connection: &Connection,
    thread: &str,
    predicate: &str,
) -> Result<Option<model::Run>, String> {
    let body = connection.query_row(&format!("SELECT candidate.payload_json FROM orchestration_v2_projection_runs candidate
        WHERE candidate.thread_id=?1 AND ({predicate}) ORDER BY candidate.ordinal DESC,candidate.run_id DESC LIMIT 1"),
        [thread], |row| row.get::<_,String>(0)).optional().map_err(failure)?;
    body.map(|body| serde_json::from_str(&body).map_err(failure))
        .transpose()
}

fn pending_request(
    connection: &Connection,
    thread: &str,
) -> Result<Option<PendingRequestSummary>, String> {
    let body = connection
        .query_row(
            "SELECT payload_json FROM orchestration_v2_projection_runtime_requests
        WHERE thread_id=?1 AND status='pending'
        ORDER BY (kind='user_input'),julianday(created_at) DESC,runtime_request_id DESC LIMIT 1",
            [thread],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(failure)?;
    body.map(|body| {
        let request: model::RuntimeRequest = serde_json::from_str(&body).map_err(failure)?;
        Ok(PendingRequestSummary {
            id: request.id,
            kind: request.kind,
            created_at: request.created_at,
            blocking: Some(!matches!(
                request.response_capability,
                model::ResponseCapability::Message
            )),
            user_only: None,
        })
    })
    .transpose()
}

fn latest_user_at(
    connection: &Connection,
    thread: &str,
    authored: bool,
) -> Result<Option<String>, String> {
    connection.query_row("SELECT i.updated_at FROM orchestration_v2_projection_turn_items i
        LEFT JOIN orchestration_v2_projection_runs r ON r.run_id=i.run_id AND r.thread_id=i.thread_id
        WHERE i.thread_id=?1 AND i.type='user_message' AND (r.status IS NULL OR r.status!='rolled_back')
          AND (?2=0 OR i.created_by='user')
        ORDER BY julianday(i.updated_at) DESC,i.ordinal DESC LIMIT 1",
        params![thread,authored],|row|row.get(0)).optional().map_err(failure)
}

fn unfinished_tasks(
    connection: &Connection,
    thread_id: &str,
) -> Result<Vec<model::PendingBackgroundTask>, String> {
    let mut query=connection.prepare("SELECT i.turn_item_id,i.type,json_extract(i.payload_json,'$.nativeItemRef.nativeId'),
        json_extract(i.payload_json,'$.title'),
        CASE i.type WHEN 'command_execution' THEN json_extract(i.payload_json,'$.input')
          WHEN 'subagent' THEN json_extract(i.payload_json,'$.prompt') ELSE json_extract(i.payload_json,'$.toolName') END,
        json_extract(i.payload_json,'$.childThreadId'),p.provider_instance_id
        FROM orchestration_v2_projection_turn_items i
        LEFT JOIN orchestration_v2_projection_runs r ON r.run_id=i.run_id AND r.thread_id=i.thread_id
        LEFT JOIN orchestration_v2_projection_provider_threads p ON p.provider_thread_id=i.provider_thread_id
        WHERE i.thread_id=?1 AND i.type IN ('command_execution','subagent','dynamic_tool')
          AND i.status IN ('pending','running','waiting') AND (r.status IS NULL OR r.status!='rolled_back')
          AND NOT(i.type='dynamic_tool' AND json_extract(i.payload_json,'$.input.persistent') IS 1)
        ORDER BY i.ordinal,i.turn_item_id").map_err(failure)?;
    let mut rows = query.query([thread_id]).map_err(failure)?;
    let mut tasks = Vec::new();
    while let Some(row) = rows.next().map_err(failure)? {
        let id: String = row.get(0).map_err(failure)?;
        let kind: String = row.get(1).map_err(failure)?;
        let native: Option<String> = row.get(2).map_err(failure)?;
        let title: Option<String> = row.get(3).map_err(failure)?;
        let text: Option<String> = row.get(4).map_err(failure)?;
        let description = title
            .into_iter()
            .chain(text)
            .map(|text| text.trim().to_owned())
            .find(|text| !text.is_empty());
        tasks.push(model::PendingBackgroundTask {
            kind: match kind.as_str() {
                "command_execution" => "command",
                "subagent" => "subagent",
                _ => "background_task",
            }
            .into(),
            task_id: native.filter(|native| !native.is_empty()).unwrap_or(id),
            description,
            provider_instance_id: if kind == "subagent" {
                row.get(6).map_err(failure)?
            } else {
                None
            },
            child_thread_id: if kind == "subagent" {
                row.get(5).map_err(failure)?
            } else {
                None
            },
            command: None,
            item_id: None,
            source_thread_id: None,
            parent_task_id: None,
            native_item_id: None,
            awaited: None,
        });
    }
    Ok(tasks)
}

fn last_failure(
    connection: &Connection,
    thread_id: &str,
    provider_id: &str,
    terminal_run: Option<(&str, &str)>,
) -> Result<Option<model::ProviderFailure>, String> {
    if let Some((run, root)) = terminal_run {
        let body=connection.query_row("SELECT json_extract(i.payload_json,'$.failure')
            FROM orchestration_v2_projection_turn_items i
            LEFT JOIN orchestration_v2_projection_nodes n ON n.node_id=i.node_id
            WHERE i.thread_id=?1 AND i.run_id=?2 AND i.type='error' AND (i.node_id=?3 OR n.root_node_id=?3)
            ORDER BY julianday(i.updated_at) DESC,i.ordinal DESC LIMIT 1",
            params![thread_id,run,root],|row|row.get::<_,String>(0)).optional().map_err(failure)?;
        if let Some(body) = body {
            return serde_json::from_str(&body).map(Some).map_err(failure);
        }
    }
    let body=connection.query_row("SELECT json_extract(s.payload_json,'$.lastError')
        FROM orchestration_v2_projection_provider_sessions s WHERE s.provider_instance_id=?1 AND EXISTS(
            SELECT 1 FROM orchestration_v2_projection_provider_session_bindings b WHERE b.provider_session_id=s.provider_session_id AND b.thread_id=?2)
        ORDER BY julianday(s.updated_at) DESC,s.provider_session_id DESC LIMIT 1",
        params![provider_id,thread_id],|row|row.get::<_,Option<String>>(0)).optional().map_err(failure)?.flatten();
    body.map(|body| serde_json::from_str(&body).map_err(failure))
        .transpose()
}

fn background(
    connection: &Connection,
    thread_id: &str,
    active_provider_thread_id: Option<&str>,
    show_work: bool,
) -> Result<(Vec<model::PendingBackgroundTask>, Vec<String>), String> {
    let mut query=connection.prepare("SELECT payload_json FROM orchestration_v2_projection_provider_threads WHERE thread_id=?1 ORDER BY julianday(json_extract(payload_json,'$.createdAt')),provider_thread_id").map_err(failure)?;
    let bodies = query
        .query_map([thread_id], |row| row.get::<_, String>(0))
        .map_err(failure)?;
    let mut tasks = BTreeMap::new();
    let mut history = Vec::new();
    for body in bodies {
        let provider: model::ProviderThread =
            serde_json::from_str(&body.map_err(failure)?).map_err(failure)?;
        if provider.app_thread_id.as_deref() != Some(thread_id) {
            continue;
        }
        if !history.contains(&provider.provider_instance_id) {
            history.push(provider.provider_instance_id.clone());
        }
        if !show_work || active_provider_thread_id.is_some_and(|id| id != provider.id) {
            continue;
        }
        for mut task in provider.pending_background_tasks {
            if task.task_id.is_empty() {
                continue;
            }
            if task.kind == "subagent" {
                task.provider_instance_id = Some(provider.provider_instance_id.clone());
            }
            task.description = task
                .description
                .map(|description| description.trim().to_owned())
                .filter(|description| !description.is_empty());
            tasks.entry(task.task_id.clone()).or_insert(task);
        }
    }
    if show_work {
        for task in unfinished_tasks(connection, thread_id)? {
            tasks.entry(task.task_id.clone()).or_insert(task);
        }
    }
    Ok((tasks.into_values().collect(), history))
}

pub(super) fn thread(
    connection: &Connection,
    thread: model::Thread,
) -> Result<ThreadShell, String> {
    let latest = last_run(connection, &thread.id, PRESENTED)?;
    let active = last_run(
        connection,
        &thread.id,
        "candidate.status IN ('preparing','starting','running')",
    )?;
    let activity = last_run(
        connection,
        &thread.id,
        "candidate.status IN ('preparing','starting','running','waiting')",
    )?;
    let show_background = active.is_none()
        && latest.as_ref().is_some_and(|run| {
            matches!(
                run.status,
                model::RunStatus::Completed
                    | model::RunStatus::Interrupted
                    | model::RunStatus::Failed
                    | model::RunStatus::Cancelled
                    | model::RunStatus::Waiting
            )
        });
    let (pending_background_tasks, provider_instance_history) = background(
        connection,
        &thread.id,
        thread.active_provider_thread_id.as_deref(),
        show_background,
    )?;
    let latest_run_stop_requested = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestration_v2_projection_turn_items
        WHERE thread_id=?1 AND run_id=?2 AND type='run_interrupt_request')",
            params![thread.id, latest.as_ref().map(|run| &run.id)],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let has_actionable_proposed_plan = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM orchestration_v2_projection_plans
        WHERE thread_id=?1 AND kind='proposed_plan' AND status='active')",
            [&thread.id],
            |row| row.get(0),
        )
        .map_err(failure)?;
    let (item_count, visible_item_count) = timeline::item_counts(connection, &thread.id)?;
    let pending_runtime_request = pending_request(connection, &thread.id)?;
    let latest_user_message_at = latest_user_at(connection, &thread.id, false)?;
    let latest_user_authored_message_at = latest_user_at(connection, &thread.id, true)?;
    let terminal_run = latest
        .as_ref()
        .filter(|run| run.status == model::RunStatus::Failed)
        .and_then(|run| {
            run.root_node_id
                .as_deref()
                .map(|root| (run.id.as_str(), root))
        });
    let failure = last_failure(
        connection,
        &thread.id,
        &thread.provider_instance_id,
        terminal_run,
    )?;
    let native_status = thread
        .active_provider_thread_id
        .as_deref()
        .map(|id| projections::read::<model::ProviderThread>(connection, id))
        .transpose()?
        .flatten()
        .filter(|provider| {
            provider.app_thread_id.as_deref() == Some(&thread.id)
                && provider.provider_instance_id == thread.provider_instance_id
                && ProviderKind::from_instance_id(&thread.provider_instance_id)
                    .is_ok_and(|kind| kind.driver() == provider.driver)
        })
        .map(|provider| provider.status);
    let latest_native_turn=connection.query_row("SELECT payload_json FROM orchestration_v2_projection_provider_turns WHERE provider_thread_id=?1 AND run_attempt_id IS NULL ORDER BY ordinal DESC LIMIT 1",
        [thread.active_provider_thread_id.as_deref()],|row|row.get::<_,String>(0)).optional().map_err(super::failure)?
        .map(|body|serde_json::from_str::<model::ProviderTurn>(&body).map_err(super::failure)).transpose()?
        .map(|turn|NativeTurnSummary {id:turn.id,status:turn.status,started_at:turn.started_at,completed_at:turn.completed_at});
    Ok(ThreadShell {
        latest_native_turn,
        native_status,
        latest_run_id: latest.as_ref().map(|run| run.id.clone()),
        latest_run_requested_at: latest.as_ref().map(|run| run.requested_at.clone()),
        latest_run_started_at: latest.as_ref().and_then(|run| run.started_at.clone()),
        latest_run_completed_at: latest.as_ref().and_then(|run| run.completed_at.clone()),
        latest_run_stop_requested,
        active_run_id: active.as_ref().map(|run| run.id.clone()),
        activity_run_started_at: activity
            .as_ref()
            .and_then(|run| run.work_started_at.clone().or(run.started_at.clone())),
        activity_run_status: activity.as_ref().map(|run| run.status),
        blocked_on_thread_wait: false,
        status: latest
            .as_ref()
            .map_or(ShellStatus::Idle, |run| run.status.into()),
        last_error: failure.as_ref().map(|failure| failure.message.clone()),
        last_error_class: failure.as_ref().map(|failure| failure.class.clone()),
        last_error_code: failure.as_ref().and_then(|failure| failure.code.clone()),
        usage_limit_reset_at: failure.and_then(|failure| failure.reset_at),
        pending_runtime_request,
        latest_visible_message: None,
        latest_user_message_at,
        latest_user_authored_message_at,
        has_actionable_proposed_plan,
        pending_background_tasks,
        provider_instance_history,
        item_count,
        visible_item_count,
        thread,
    })
}

pub(super) fn projects(connection: &Connection) -> Result<Vec<ProjectShell>, String> {
    let mut query=connection.prepare("SELECT project_id,title,workspace_root,scripts_json,created_at,updated_at,
        default_model_selection_json,default_thread_env_mode,favicon_path,auto_pull,project_icon_json,pinned_at,archived_at,has_custom_title
        FROM projection_projects WHERE deleted_at IS NULL ORDER BY rowid").map_err(failure)?;
    let mut rows = query.query([]).map_err(failure)?;
    let mut projects = Vec::new();
    while let Some(row) = rows.next().map_err(failure)? {
        let scripts: String = row.get(3).map_err(failure)?;
        let model: Option<String> = row.get(6).map_err(failure)?;
        let icon: Option<String> = row.get(10).map_err(failure)?;
        projects.push(ProjectShell {
            id: row.get(0).map_err(failure)?,
            title: row.get(1).map_err(failure)?,
            workspace_root: row.get(2).map_err(failure)?,
            scripts: serde_json::from_str(&scripts).map_err(failure)?,
            created_at: row.get(4).map_err(failure)?,
            updated_at: row.get(5).map_err(failure)?,
            default_model_selection: model
                .map(|model| serde_json::from_str(&model).map_err(failure))
                .transpose()?,
            default_thread_env_mode: row.get(7).map_err(failure)?,
            favicon_path: row.get(8).map_err(failure)?,
            auto_pull: row.get(9).map_err(failure)?,
            project_icon: icon
                .map(|icon| serde_json::from_str(&icon).map_err(failure))
                .transpose()?,
            pinned_at: row.get(11).map_err(failure)?,
            archived_at: row.get(12).map_err(failure)?,
            has_custom_title: row.get(13).map_err(failure)?,
            repository_identity: None,
        });
    }
    Ok(projects)
}

pub(super) fn head(connection: &Connection) -> Result<u64, String> {
    connection
        .query_row(
            "SELECT coalesce(max(sequence),0) FROM orchestration_events",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(failure)?
        .try_into()
        .map_err(failure)
}

pub(super) fn snapshot(connection: &Connection) -> Result<ShellSnapshot, String> {
    let mut query=connection.prepare("SELECT payload_json FROM orchestration_v2_projection_threads WHERE deleted_at IS NULL ORDER BY created_at,thread_id").map_err(failure)?;
    let bodies = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(failure)?;
    let mut threads = Vec::new();
    let mut archived_threads = Vec::new();
    for body in bodies {
        let shell = thread(
            connection,
            serde_json::from_str(&body.map_err(failure)?).map_err(failure)?,
        )?;
        if shell.thread.archived_at.is_some() {
            archived_threads.push(Arc::new(shell));
        } else {
            threads.push(Arc::new(shell));
        }
    }
    Ok(ShellSnapshot {
        schema_version: 2,
        snapshot_sequence: head(connection)?,
        projects: projects(connection)?.into_iter().map(Arc::new).collect(),
        threads,
        archived_threads,
    })
}

impl Storage {
    pub(in crate::host_rpc) fn shell_snapshot(&self) -> Result<ShellSnapshot, String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let snapshot = snapshot(&transaction)?;
            transaction.commit().map_err(failure)?;
            Ok(snapshot)
        })
    }

    pub(in crate::host_rpc) fn open_shell(
        &self,
        after: Option<u64>,
    ) -> Result<(ShellSnapshot, Vec<ShellStreamItem>), String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let current = snapshot(&transaction)?;
            let mut initial = match after
                .map(|after| {
                    if within_resume_budget(&transaction, after, current.snapshot_sequence)? {
                        dirty(&transaction, after, current.snapshot_sequence)
                    } else {
                        Ok(None)
                    }
                })
                .transpose()?
                .flatten()
            {
                Some(changed) => changed
                    .into_iter()
                    .flat_map(|change| delta(&current, &change))
                    .collect(),
                None => vec![ShellStreamItem::Snapshot {
                    snapshot: current.clone(),
                    resolved_repository_identity_roots: None,
                }],
            };
            initial.push(ShellStreamItem::Synchronized);
            transaction.commit().map_err(failure)?;
            Ok((current, initial))
        })
    }

    pub(in crate::host_rpc) fn shell_changes(
        &self,
        previous: &ShellSnapshot,
    ) -> Result<(ShellSnapshot, Vec<ShellStreamItem>), String> {
        self.with(|connection| {
            let transaction = connection.transaction().map_err(failure)?;
            let through = head(&transaction)?;
            let Some(changed) = dirty(&transaction, previous.snapshot_sequence, through)? else {
                let current = snapshot(&transaction)?;
                let initial = vec![
                    ShellStreamItem::Snapshot {
                        snapshot: current.clone(),
                        resolved_repository_identity_roots: None,
                    },
                    ShellStreamItem::Synchronized,
                ];
                transaction.commit().map_err(failure)?;
                return Ok((current, initial));
            };
            let mut current = previous.clone();
            if changed.iter().any(|change| change.kind == "project") {
                current.projects = projects(&transaction)?.into_iter().map(Arc::new).collect();
            }
            for change in changed.iter().filter(|change| change.kind == "thread") {
                let updated = projections::read::<model::Thread>(&transaction, &change.id)?
                    .filter(|thread| thread.deleted_at.is_none())
                    .map(|record| thread(&transaction, record).map(Arc::new))
                    .transpose()?;
                let old = current
                    .threads
                    .iter()
                    .chain(&current.archived_threads)
                    .find(|thread| thread.thread.id == change.id);
                if old == updated.as_ref() {
                    continue;
                }
                current
                    .threads
                    .retain(|thread| thread.thread.id != change.id);
                current
                    .archived_threads
                    .retain(|thread| thread.thread.id != change.id);
                if let Some(updated) = updated {
                    if updated.thread.archived_at.is_some() {
                        current.archived_threads.push(updated);
                    } else {
                        current.threads.push(updated);
                    }
                }
            }
            current.snapshot_sequence = through;
            let updates = changed
                .iter()
                .filter(|change| match change.kind.as_str() {
                    "thread" => true,
                    _ => project_changed(&previous.projects, &current.projects, &change.id),
                })
                .flat_map(|change| delta(&current, change))
                .collect();
            transaction.commit().map_err(failure)?;
            Ok((current, updates))
        })
    }
}

struct Dirty {
    sequence: u64,
    kind: String,
    id: String,
}

/// Reconnect replay is bounded by persisted event bytes. A live subscriber
/// already owns its shell and only queries changed aggregate identities; a
/// burst of text events must not force a full sidebar snapshot onto its stream.
fn within_resume_budget(connection: &Connection, after: u64, through: u64) -> Result<bool, String> {
    if after > through || through - after > 1_000 {
        return Ok(false);
    }
    let bytes:i64=connection.query_row("SELECT coalesce(sum(length(CAST(payload_json AS BLOB))+length(CAST(metadata_json AS BLOB))),0)
        FROM orchestration_events WHERE sequence>?1 AND sequence<=?2",params![integer(after)?,integer(through)?],|row|row.get(0)).map_err(failure)?;
    Ok(bytes <= 8 * 1024 * 1024)
}

fn dirty(connection: &Connection, after: u64, through: u64) -> Result<Option<Vec<Dirty>>, String> {
    if after == through {
        return Ok(Some(Vec::new()));
    }
    if after > through {
        return Ok(None);
    }
    let pruned:i64=connection.query_row("SELECT coalesce((SELECT last_sequence FROM orchestration_v2_projection_metadata WHERE projection_name='event-log-pruned-through'),0)",[],|row|row.get(0)).map_err(failure)?;
    if after < u64::try_from(pruned).map_err(failure)? {
        return Ok(None);
    }
    let mut query=connection.prepare("SELECT max(sequence),aggregate_kind,stream_id FROM orchestration_events
        WHERE sequence>?1 AND sequence<=?2 AND ((aggregate_kind='thread' AND application_event_version=2)
            OR (aggregate_kind='project' AND application_event_version=1))
        GROUP BY aggregate_kind,stream_id ORDER BY max(sequence)").map_err(failure)?;
    query
        .query_map(params![integer(after)?, integer(through)?], |row| {
            Ok((row.get::<_, i64>(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(failure)?
        .map(|row| {
            let (sequence, kind, id) = row.map_err(failure)?;
            Ok(Dirty {
                sequence: sequence.try_into().map_err(failure)?,
                kind,
                id,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn project_changed(
    previous: &[Arc<ProjectShell>],
    current: &[Arc<ProjectShell>],
    id: &str,
) -> bool {
    previous.iter().find(|project| project.id == id)
        != current.iter().find(|project| project.id == id)
}

fn delta(current: &ShellSnapshot, change: &Dirty) -> Vec<ShellStreamItem> {
    if change.kind == "project" {
        return vec![match current
            .projects
            .iter()
            .find(|project| project.id == change.id)
        {
            Some(project) => ShellStreamItem::ProjectUpdated {
                sequence: change.sequence,
                project: project.as_ref().clone(),
            },
            None => ShellStreamItem::ProjectRemoved {
                sequence: change.sequence,
                project_id: change.id.clone(),
            },
        }];
    }
    let active = current
        .threads
        .iter()
        .find(|thread| thread.thread.id == change.id);
    let archived = current
        .archived_threads
        .iter()
        .find(|thread| thread.thread.id == change.id);
    match active
        .map(|thread| (thread, ShellLocation::Active))
        .or_else(|| archived.map(|thread| (thread, ShellLocation::Archive)))
    {
        Some((thread, location)) => vec![ShellStreamItem::ThreadUpdated {
            sequence: change.sequence,
            location,
            thread: thread.as_ref().clone(),
        }],
        None => vec![
            ShellStreamItem::ThreadRemoved {
                sequence: change.sequence,
                location: ShellLocation::Active,
                thread_id: change.id.clone(),
            },
            ShellStreamItem::ThreadRemoved {
                sequence: change.sequence,
                location: ShellLocation::Archive,
                thread_id: change.id.clone(),
            },
        ],
    }
}

#[cfg(test)]
mod tests;

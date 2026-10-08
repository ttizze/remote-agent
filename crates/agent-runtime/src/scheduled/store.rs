//! The `scheduled_tasks` table: the definition as JSON beside the columns the
//! due query and the run transitions touch.
use super::{ScheduledTask, ScheduledTaskRunStatus};
use crate::{Store, StoreError, WorkspaceStrategy};
use agent_domain::{
    InteractionMode, MessageAuthor, ModelSelection, RuntimeMode, Schedule, ThreadId, Timestamp,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

/// What a save replaces; the run state lives in its own columns.
#[derive(Debug, Serialize, Deserialize)]
struct Definition {
    title: String,
    prompt: String,
    schedule: Schedule,
    project: String,
    thread: Option<ThreadId>,
    workspace: WorkspaceStrategy,
    selection: ModelSelection,
    runtime_mode: RuntimeMode,
    interaction_mode: InteractionMode,
    created_by: MessageAuthor,
    creation_source: String,
    created_at: Timestamp,
}

const COLUMNS: &str = "task_id, enabled, definition, updated_at, next_run_at, last_run_at,
    last_run_status, last_run_error, run_count";

type Raw = (
    String,
    bool,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    i64,
);

fn raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn corrupt(error: impl std::fmt::Display) -> StoreError {
    StoreError::Corrupt(error.to_string())
}

fn stamp(value: &str) -> Result<Timestamp, StoreError> {
    Timestamp::parse(value).map_err(corrupt)
}

fn decode(
    (id, enabled, definition, updated_at, next_run_at, last_run_at, status, error, run_count): Raw,
) -> Result<ScheduledTask, StoreError> {
    let definition: Definition = serde_json::from_str(&definition)?;
    Ok(ScheduledTask {
        id,
        title: definition.title,
        prompt: definition.prompt,
        enabled,
        schedule: definition.schedule,
        project: definition.project,
        thread: definition.thread,
        workspace: definition.workspace,
        selection: definition.selection,
        runtime_mode: definition.runtime_mode,
        interaction_mode: definition.interaction_mode,
        created_by: definition.created_by,
        creation_source: definition.creation_source,
        created_at: definition.created_at,
        updated_at: stamp(&updated_at)?,
        next_run_at: next_run_at.as_deref().map(stamp).transpose()?,
        last_run_at: last_run_at.as_deref().map(stamp).transpose()?,
        last_run_status: ScheduledTaskRunStatus::parse(&status)
            .ok_or_else(|| corrupt(format!("unknown scheduled task run status {status}")))?,
        last_run_error: error,
        run_count: u64::try_from(run_count).unwrap_or_default(),
    })
}

fn query(
    c: &Connection,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<ScheduledTask>, StoreError> {
    let mut statement = c.prepare_cached(sql)?;
    let rows = statement.query_map(params, raw)?;
    rows.map(|row| decode(row?)).collect()
}

impl Store {
    /// Every task, most recently changed first.
    pub fn scheduled_tasks(&self) -> Result<Vec<ScheduledTask>, StoreError> {
        self.read(|c| {
            query(
                c,
                &format!("SELECT {COLUMNS} FROM scheduled_tasks ORDER BY updated_at DESC, task_id"),
                [],
            )
        })
    }

    pub fn scheduled_task(&self, id: &str) -> Result<Option<ScheduledTask>, StoreError> {
        self.read(|c| {
            c.query_row(
                &format!("SELECT {COLUMNS} FROM scheduled_tasks WHERE task_id = ?1"),
                [id],
                raw,
            )
            .optional()?
            .map(decode)
            .transpose()
        })
    }

    /// Enabled tasks due at or before `at` that are not running, earliest first.
    pub(crate) fn due_scheduled_tasks(
        &self,
        at: &Timestamp,
    ) -> Result<Vec<ScheduledTask>, StoreError> {
        self.read(|c| {
            query(
                c,
                &format!(
                    "SELECT {COLUMNS} FROM scheduled_tasks
                     WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1
                       AND last_run_status <> 'running'
                     ORDER BY next_run_at, task_id"
                ),
                [at.as_str()],
            )
        })
    }

    /// Tasks a previous process left mid-run.
    pub(crate) fn running_scheduled_tasks(&self) -> Result<Vec<ScheduledTask>, StoreError> {
        self.read(|c| {
            query(
                c,
                &format!(
                    "SELECT {COLUMNS} FROM scheduled_tasks WHERE last_run_status = 'running'
                     ORDER BY task_id"
                ),
                [],
            )
        })
    }
}

/// Writes the definition, keeping the run state of an existing row. False when
/// `require_existing` found no row.
pub(super) fn save(
    tx: &Transaction<'_>,
    task: &ScheduledTask,
    require_existing: bool,
) -> Result<bool, StoreError> {
    let definition = serde_json::to_string(&Definition {
        title: task.title.clone(),
        prompt: task.prompt.clone(),
        schedule: task.schedule.clone(),
        project: task.project.clone(),
        thread: task.thread.clone(),
        workspace: task.workspace.clone(),
        selection: task.selection.clone(),
        runtime_mode: task.runtime_mode,
        interaction_mode: task.interaction_mode,
        created_by: task.created_by,
        creation_source: task.creation_source.clone(),
        created_at: task.created_at.clone(),
    })?;
    let written = tx.execute(
        "INSERT INTO scheduled_tasks (task_id, enabled, definition, updated_at, next_run_at,
             last_run_at, last_run_status, last_run_error, run_count)
         SELECT ?1, ?2, ?3, ?4, ?5, NULL, 'never', NULL, 0
         WHERE ?6 = 0 OR EXISTS (SELECT 1 FROM scheduled_tasks WHERE task_id = ?1)
         ON CONFLICT (task_id) DO UPDATE SET
             enabled = excluded.enabled,
             definition = excluded.definition,
             updated_at = excluded.updated_at,
             next_run_at = excluded.next_run_at",
        params![
            task.id,
            task.enabled,
            definition,
            task.updated_at.as_str(),
            task.next_run_at.as_ref().map(Timestamp::as_str),
            require_existing,
        ],
    )?;
    Ok(written == 1)
}

/// False when the task no longer exists.
pub(super) fn set_enabled(
    tx: &Transaction<'_>,
    id: &str,
    enabled: bool,
    next_run_at: Option<Timestamp>,
    updated_at: &Timestamp,
) -> Result<bool, StoreError> {
    Ok(tx.execute(
        "UPDATE scheduled_tasks SET enabled = ?2, next_run_at = ?3, updated_at = ?4
         WHERE task_id = ?1",
        params![
            id,
            enabled,
            next_run_at.as_ref().map(Timestamp::as_str),
            updated_at.as_str()
        ],
    )? == 1)
}

pub(super) fn delete(tx: &Transaction<'_>, id: &str) -> Result<(), StoreError> {
    tx.execute("DELETE FROM scheduled_tasks WHERE task_id = ?1", [id])?;
    Ok(())
}

pub(super) fn reschedule(
    tx: &Transaction<'_>,
    id: &str,
    next_run_at: Option<Timestamp>,
    updated_at: &Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE scheduled_tasks SET next_run_at = ?2, updated_at = ?3 WHERE task_id = ?1",
        params![
            id,
            next_run_at.as_ref().map(Timestamp::as_str),
            updated_at.as_str()
        ],
    )?;
    Ok(())
}

pub(super) fn mark_running(
    tx: &Transaction<'_>,
    id: &str,
    started_at: &Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE scheduled_tasks
         SET updated_at = ?2, last_run_at = ?2, last_run_status = 'running', last_run_error = NULL
         WHERE task_id = ?1",
        params![id, started_at.as_str()],
    )?;
    Ok(())
}

/// Records the end of the run that started at `started_at`; a row another run
/// or a recreate replaced is left alone.
pub(super) fn mark_completed(
    tx: &Transaction<'_>,
    id: &str,
    started_at: &Timestamp,
    completed_at: &Timestamp,
    next_run_at: Option<Timestamp>,
    status: ScheduledTaskRunStatus,
    error: Option<&str>,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE scheduled_tasks
         SET updated_at = ?3, next_run_at = ?4, last_run_status = ?5, last_run_error = ?6,
             run_count = run_count + 1
         WHERE task_id = ?1 AND last_run_status = 'running' AND last_run_at = ?2",
        params![
            id,
            started_at.as_str(),
            completed_at.as_str(),
            next_run_at.as_ref().map(Timestamp::as_str),
            status.as_str(),
            error,
        ],
    )?;
    Ok(())
}

/// Ends a run the process did not finish as failed with `error`.
pub(super) fn release(
    tx: &Transaction<'_>,
    id: &str,
    error: &str,
    next_run_at: Option<Timestamp>,
    updated_at: &Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE scheduled_tasks
         SET last_run_status = 'failed', last_run_error = ?2, next_run_at = ?3, updated_at = ?4,
             run_count = run_count + 1
         WHERE task_id = ?1 AND last_run_status = 'running'",
        params![
            id,
            error,
            next_run_at.as_ref().map(Timestamp::as_str),
            updated_at.as_str()
        ],
    )?;
    Ok(())
}

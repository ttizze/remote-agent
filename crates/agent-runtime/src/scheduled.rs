//! Scheduled tasks: durable rows the shared five-second sweep polls in the
//! Host's local time zone. A due run launches a thread or sends into one and
//! records how it went; every change wakes the subscribers of the list.
mod store;

use crate::{
    Clock, CommandOrigin, ExecutorContext, InitialMessage, LaunchThread, Preparations, StoreError,
    WorkspaceStrategy,
};
use agent_domain::{
    Command, CommandId, DeliveryIntent, DispatchMode, InteractionMode, MessageAuthor, MessageId,
    ModelSelection, Reply, RuntimeMode, Schedule, SendMessage, ThreadId, Timestamp,
    missed_fixed_time_run, next_run_at, same_schedule,
};
use chrono::{DateTime, Local, TimeZone};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn parse(value: &str) -> Option<Self> {
        [Self::Never, Self::Running, Self::Succeeded, Self::Failed]
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

/// A task as the Host stores it; the run state columns change only through
/// run transitions.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledTask {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub project: String,
    /// The thread each run sends into; `None` launches a new thread per run.
    pub thread: Option<ThreadId>,
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

/// A saved definition: a new task, or the replacement of the task `id` names
/// while its run history stays.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledTaskInput {
    pub id: Option<String>,
    /// Refuse the save when the task no longer exists (an edit from a form).
    pub require_existing: bool,
    /// Stable id for an idempotent create retry. When `id` is omitted the
    /// scheduler derives the task id from this command id.
    pub command_id: Option<CommandId>,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: Schedule,
    pub project: String,
    pub thread: Option<ThreadId>,
    pub workspace: WorkspaceStrategy,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub created_by: MessageAuthor,
    pub creation_source: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduledTaskError {
    #[error("Schedule task not found.")]
    NotFound(String),
    #[error("Schedule task is already running.")]
    AlreadyRunning(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}
impl ScheduledTaskError {
    /// The task the failure is about, when it names one.
    pub fn task(&self) -> Option<&str> {
        match self {
            Self::NotFound(id) | Self::AlreadyRunning(id) => Some(id),
            Self::Store(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Scheduled,
    Manual,
}
impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Manual => "manual",
        }
    }
}

/// The definition the sweep and the clients share.
pub struct ScheduledTasks {
    executors: ExecutorContext,
    preparations: Arc<Preparations>,
    clock: Arc<dyn Clock>,
    /// Bumped after every change; subscribers list again.
    changed: watch::Sender<u64>,
    /// Tasks with a run in flight in this process.
    active: Mutex<HashSet<String>>,
}

/// Releases a task's in-flight reservation when its run ends however it ends.
struct Reservation<'a> {
    tasks: &'a ScheduledTasks,
    id: String,
}
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.tasks
            .active
            .lock()
            .expect("active scheduled runs")
            .remove(&self.id);
    }
}

fn timestamp(at: &DateTime<Local>) -> Timestamp {
    Timestamp::from_millis(at.timestamp_millis()).expect("local time is in range")
}

/// The next occurrence of an enabled task after `from`; an occurrence beyond
/// the representable range means no next run.
fn next_run(enabled: bool, schedule: &Schedule, from: &DateTime<Local>) -> Option<Timestamp> {
    if !enabled {
        return None;
    }
    let next = next_run_at(schedule, from)?;
    Timestamp::from_millis(next.timestamp_millis()).ok()
}

impl ScheduledTasks {
    pub(crate) fn new(
        executors: ExecutorContext,
        preparations: Arc<Preparations>,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Arc::new(Self {
            executors,
            preparations,
            clock,
            changed: watch::Sender::new(0),
            active: Mutex::default(),
        })
    }

    fn now(&self) -> DateTime<Local> {
        Local
            .timestamp_millis_opt(self.clock.now().millis())
            .single()
            .expect("clock time is in range")
    }

    fn notify(&self) {
        self.changed.send_modify(|revision| *revision += 1);
    }

    /// Every task, most recently changed first.
    pub async fn list(&self) -> Result<Vec<ScheduledTask>, StoreError> {
        self.executors
            .store
            .blocking(|store| store.scheduled_tasks())
            .await
    }

    /// Changes after the current revision; list again on each.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    async fn find(&self, id: &str) -> Result<Option<ScheduledTask>, StoreError> {
        let id = id.to_owned();
        self.executors
            .store
            .blocking(move |store| store.scheduled_task(&id))
            .await
    }

    async fn load(&self, id: &str) -> Result<ScheduledTask, ScheduledTaskError> {
        self.find(id)
            .await?
            .ok_or_else(|| ScheduledTaskError::NotFound(id.to_owned()))
    }

    /// Saves a task. Editing anything but the schedule or the enabled flag
    /// keeps the pending run: a title change must not postpone a due run.
    pub async fn upsert(
        &self,
        input: ScheduledTaskInput,
    ) -> Result<ScheduledTask, ScheduledTaskError> {
        let now = self.now();
        let id = input
            .id
            .clone()
            .unwrap_or_else(|| {
                input.command_id.as_ref().map_or_else(
                    || format!("scheduled-task:{}", uuid::Uuid::new_v4()),
                    |command| format!("scheduled-task:{command}"),
                )
            });
        let existing = self.find(&id).await?;
        if input.require_existing && existing.is_none() {
            return Err(ScheduledTaskError::NotFound(id));
        }
        let unchanged = existing.as_ref().is_some_and(|existing| {
            existing.enabled == input.enabled && same_schedule(&existing.schedule, &input.schedule)
        });
        let next_run_at = if unchanged {
            existing.as_ref().and_then(|task| task.next_run_at.clone())
        } else {
            next_run(input.enabled, &input.schedule, &now)
        };
        let task = ScheduledTask {
            id,
            title: input.title,
            prompt: input.prompt,
            enabled: input.enabled,
            schedule: input.schedule,
            project: input.project,
            thread: input.thread,
            workspace: input.workspace,
            selection: input.selection,
            runtime_mode: input.runtime_mode,
            interaction_mode: input.interaction_mode,
            created_by: existing
                .as_ref()
                .map_or(input.created_by, |task| task.created_by),
            creation_source: input.creation_source,
            created_at: existing
                .as_ref()
                .map_or_else(|| timestamp(&now), |task| task.created_at.clone()),
            updated_at: timestamp(&now),
            next_run_at,
            last_run_at: existing.as_ref().and_then(|task| task.last_run_at.clone()),
            last_run_status: existing
                .as_ref()
                .map_or(ScheduledTaskRunStatus::Never, |task| task.last_run_status),
            last_run_error: existing
                .as_ref()
                .and_then(|task| task.last_run_error.clone()),
            run_count: existing.as_ref().map_or(0, |task| task.run_count),
        };
        let saved = task.clone();
        // Existence is checked in the write itself so an edit cannot undo a
        // deletion that landed after the task was loaded.
        let written = self
            .executors
            .store
            .write(move |tx| store::save(tx, &saved, input.require_existing))
            .await?;
        if !written {
            return Err(ScheduledTaskError::NotFound(task.id));
        }
        self.notify();
        Ok(task)
    }

    /// Flips only the enabled flag; a change restarts the schedule clock.
    pub async fn set_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<ScheduledTask, ScheduledTaskError> {
        let existing = self.load(id).await?;
        if existing.enabled == enabled {
            return Ok(existing);
        }
        let now = self.now();
        let next = next_run(enabled, &existing.schedule, &now);
        let (task_id, next_run_at, updated_at) = (id.to_owned(), next.clone(), timestamp(&now));
        let written = self
            .executors
            .store
            .write(move |tx| store::set_enabled(tx, &task_id, enabled, next_run_at, &updated_at))
            .await?;
        if !written {
            return Err(ScheduledTaskError::NotFound(id.to_owned()));
        }
        self.notify();
        Ok(ScheduledTask {
            enabled,
            next_run_at: next,
            updated_at: timestamp(&now),
            ..existing
        })
    }

    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let id = id.to_owned();
        self.executors
            .store
            .write(move |tx| store::delete(tx, &id))
            .await?;
        self.notify();
        Ok(())
    }

    /// Runs the task now; a task already running is refused.
    pub async fn run_now(&self, id: &str) -> Result<ScheduledTask, ScheduledTaskError> {
        let task = self.load(id).await?;
        self.run(task, Trigger::Manual).await
    }

    /// A due fixed-time run long past its slot (the Host was off or asleep) is
    /// aimed at its next occurrence, not fired late.
    async fn reschedule_missed(
        &self,
        task: &ScheduledTask,
        now: &DateTime<Local>,
    ) -> Result<(), StoreError> {
        let next = next_run(task.enabled, &task.schedule, now);
        tracing::info!(
            task = %task.id,
            missed = ?task.next_run_at,
            rescheduled = ?next,
            "skipping a missed scheduled task run"
        );
        let (id, updated_at) = (task.id.clone(), timestamp(now));
        self.executors
            .store
            .write(move |tx| store::reschedule(tx, &id, next, &updated_at))
            .await?;
        self.notify();
        Ok(())
    }

    /// The sweep: every due task runs in turn, or is rescheduled when missed.
    pub(crate) async fn run_due(&self) -> Result<(), StoreError> {
        let now = self.now();
        let due_before = timestamp(&now);
        let due = self
            .executors
            .store
            .blocking(move |store| store.due_scheduled_tasks(&due_before))
            .await?;
        for task in due {
            let Some(due_at) = task.next_run_at.as_ref().map(Timestamp::millis) else {
                continue;
            };
            let result = if missed_fixed_time_run(&task.schedule, due_at, now.timestamp_millis()) {
                self.reschedule_missed(&task, &now).await.map(|()| ())
            } else {
                self.run(task.clone(), Trigger::Scheduled)
                    .await
                    .map(|_| ())
                    .map_err(|error| match error {
                        ScheduledTaskError::Store(error) => error,
                        other => StoreError::Corrupt(other.to_string()),
                    })
            };
            if let Err(error) = result {
                tracing::warn!(task = %task.id, %error, "a scheduled task run failed");
            }
        }
        Ok(())
    }

    /// Rows a previous process left in `running` would be skipped by the due
    /// filter forever. The dispatch may have gone out before the process
    /// ended, so the next run is aimed ahead and the attempt counted.
    pub(crate) async fn release_interrupted(&self) -> Result<(), StoreError> {
        let stuck = self
            .executors
            .store
            .blocking(|store| store.running_scheduled_tasks())
            .await?;
        if stuck.is_empty() {
            return Ok(());
        }
        let now = self.now();
        let mut first_error = None;
        let mut changed = false;
        for interrupted in stuck {
            let next = interrupted
                .task
                .as_ref()
                .ok()
                .and_then(|task| next_run(task.enabled, &task.schedule, &now));
            let (id, updated_at) = (interrupted.id, timestamp(&now));
            let result = self
                .executors
                .store
                .write(move |tx| {
                    store::release(
                        tx,
                        id.as_deref(),
                        "Run was interrupted by a server restart.",
                        next,
                        &updated_at,
                    )
                })
                .await;
            match result {
                Ok(()) => changed = true,
                Err(error) => {
                    tracing::warn!(%error, "Could not release an interrupted scheduled task run");
                    first_error.get_or_insert(error);
                }
            }
        }
        if changed {
            self.notify();
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    /// Best-effort escape hatch for failures after a run is marked `running`.
    /// A dispatch may already have reached the conversation runtime, so the
    /// attempt is counted and the next occurrence is advanced before the
    /// scheduler can poll the row again.
    async fn release_stuck_run(&self, task: &ScheduledTask, error: &str) {
        let now = self.now();
        let source = match self.find(&task.id).await {
            Ok(Some(current)) => current,
            Ok(None) => return,
            Err(find_error) => {
                tracing::warn!(
                    task = %task.id,
                    %find_error,
                    "Could not reread a failed scheduled task run"
                );
                task.clone()
            }
        };
        let next = next_run(source.enabled, &source.schedule, &now);
        let (id, error, updated_at) = (task.id.clone(), error.to_owned(), timestamp(&now));
        match self
            .executors
            .store
            .write(move |tx| store::release(tx, Some(&id), &error, next, &updated_at))
            .await
        {
            Ok(()) => self.notify(),
            Err(store_error) => tracing::warn!(
                task = %task.id,
                %store_error,
                "Could not release a failed scheduled task run"
            ),
        }
    }

    async fn run(
        &self,
        task: ScheduledTask,
        trigger: Trigger,
    ) -> Result<ScheduledTask, ScheduledTaskError> {
        if !self
            .active
            .lock()
            .expect("active scheduled runs")
            .insert(task.id.clone())
        {
            return match trigger {
                Trigger::Manual => Err(ScheduledTaskError::AlreadyRunning(task.id)),
                Trigger::Scheduled => Ok(task),
            };
        }
        let _reservation = Reservation {
            tasks: self,
            id: task.id.clone(),
        };
        let started = self.now();
        let started_at = timestamp(&started);
        // The poll's snapshot may be stale: the task may have been deleted,
        // paused or postponed since. None of those may fire.
        let Some(active) = self.find(&task.id).await? else {
            return match trigger {
                Trigger::Manual => Err(ScheduledTaskError::NotFound(task.id)),
                Trigger::Scheduled => Ok(task),
            };
        };
        if trigger == Trigger::Scheduled
            && (!active.enabled
                || active
                    .next_run_at
                    .as_ref()
                    .is_none_or(|next| next > &started_at))
        {
            return Ok(active);
        }
        let (id, at) = (active.id.clone(), started_at.clone());
        let marked = self
            .executors
            .store
            .write(move |tx| store::mark_running(tx, &id, &at))
            .await?;
        if !marked {
            return match trigger {
                Trigger::Manual => Err(ScheduledTaskError::NotFound(task.id)),
                Trigger::Scheduled => Ok(task),
            };
        }
        self.notify();

        let finished = async {
            let fire_key = format!(
                "{}:{}:{}",
                active.id,
                started.timestamp_millis(),
                trigger.as_str()
            );
            let error = self.dispatch(&active, &fire_key).await.err();
            let completed = self.now();
            let status = if error.is_none() {
                ScheduledTaskRunStatus::Succeeded
            } else {
                ScheduledTaskRunStatus::Failed
            };
            // The next run follows the schedule as it is now: the task may have
            // been edited or deleted while the run was dispatched.
            let current = self.find(&task.id).await?;
            let source = current.clone().unwrap_or_else(|| active.clone());
            let next = next_run(source.enabled, &source.schedule, &completed);
            if current.is_some() {
                // Guarded by the start time, so a task deleted mid-run and recreated
                // with the same id is not stamped.
                let (id, completed_at, completion_error) =
                    (task.id.clone(), timestamp(&completed), error.clone());
                let written = self
                    .executors
                    .store
                    .write(move |tx| {
                        store::mark_completed(
                            tx,
                            &id,
                            &started_at,
                            &completed_at,
                            next.clone(),
                            status,
                            completion_error.as_deref(),
                        )
                    })
                    .await?;
                if !written {
                    // The row was deleted or replaced while this run was in
                    // flight. Return the current row without attributing the
                    // stale completion to it.
                    return Ok(source);
                }
                self.notify();
            }
            Ok(ScheduledTask {
                updated_at: timestamp(&completed),
                last_run_at: Some(started_at.clone()),
                next_run_at: next,
                last_run_status: status,
                last_run_error: error.clone(),
                run_count: source.run_count + 1,
                ..source
            })
        }
        .await;
        if let Err(error) = &finished {
            self.release_stuck_run(&active, &error.to_string()).await;
        }
        finished
    }

    /// Sends the prompt: a new thread, or a message into the bound thread.
    async fn dispatch(&self, task: &ScheduledTask, fire_key: &str) -> Result<(), String> {
        let command = CommandId::new(format!("scheduled-task:{fire_key}")).expect("derived id");
        let message =
            MessageId::new(format!("scheduled-task-message:{fire_key}")).expect("derived id");
        match &task.thread {
            None => crate::launch::launch(
                &self.executors,
                &self.preparations,
                LaunchThread {
                    command,
                    thread: None,
                    project: task.project.clone(),
                    title: task.title.clone(),
                    generate_title: false,
                    selection: task.selection.clone(),
                    runtime_mode: task.runtime_mode,
                    interaction_mode: task.interaction_mode,
                    workspace: task.workspace.clone(),
                    initial_message: Some(InitialMessage {
                        id: Some(message),
                        text: task.prompt.clone(),
                        attachments: vec![],
                        created_by: task.created_by,
                        creation_source: task.creation_source.clone(),
                        context: None,
                        scheduled_task: Some(task.id.clone()),
                    }),
                    created_by: task.created_by,
                    creation_source: task.creation_source.clone(),
                },
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string()),
            Some(thread) => {
                let committed = self
                    .executors
                    .registry
                    .dispatch(
                        thread,
                        command,
                        Command::Send(SendMessage {
                            created_by: task.created_by,
                            creation_source: task.creation_source.clone(),
                            scheduled_task: Some(task.id.clone()),
                            id: message,
                            text: task.prompt.clone(),
                            attachments: vec![],
                            selection: Some(task.selection.clone()),
                            mode: DispatchMode::StartImmediately,
                            intent: Some(DeliveryIntent::Auto),
                            source_plan: None,
                            resolved_plan: None,
                            continuation: None,
                            title_seed: None,
                            context: None,
                        }),
                        CommandOrigin::Internal,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                match committed.reply {
                    Reply::Rejected { reason } => Err(reason),
                    _ => Ok(()),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;

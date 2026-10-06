//! Thread launch (T3 `ThreadLaunchService`): create the thread, send its first
//! message as a preparing run, and let `PrepareWorkspace` provision the workspace.
use crate::{
    CommandOrigin, Committed, ExecutorContext, Store, StoreError, derived_uuid, prepare_workspace,
};
use agent_domain::{
    Attachment, Command, CommandId, DispatchMode, InteractionMode, MessageAuthor, MessageId,
    ModelSelection, Reply, RuntimeMode, SendMessage, ThreadId, Workspace,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceStrategy {
    /// The project's checkout.
    Root { branch: Option<String> },
    ExistingWorktree {
        path: String,
        branch: Option<String>,
    },
    /// A new worktree from `base_ref`; without a branch the Host names it.
    Worktree {
        base_ref: String,
        branch: Option<String>,
        start_from_origin: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitialMessage {
    pub id: Option<MessageId>,
    pub text: String,
    pub attachments: Vec<Attachment>,
    pub created_by: MessageAuthor,
    pub creation_source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchThread {
    pub command: CommandId,
    /// `None` derives the thread id from the command id.
    pub thread: Option<ThreadId>,
    pub project: String,
    pub title: String,
    /// Shows `title` until a title is generated from the first message.
    pub generate_title: bool,
    pub selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace: WorkspaceStrategy,
    pub initial_message: Option<InitialMessage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchReply {
    pub thread: ThreadId,
    /// The last command the launch committed: the first message, or the create.
    pub committed: Committed,
    /// The launch or its first message was accepted before.
    pub resumed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchOperation {
    ResolveProject,
    ReadReceipt,
    CreateThread,
    DispatchMessage,
    ProvisionWorktree,
    RunSetupScript,
    UpdateThread,
}
impl LaunchOperation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResolveProject => "resolve-project",
            Self::ReadReceipt => "read-receipt",
            Self::CreateThread => "create-thread",
            Self::DispatchMessage => "dispatch-message",
            Self::ProvisionWorktree => "provision-worktree",
            Self::RunSetupScript => "run-setup-script",
            Self::UpdateThread => "update-thread",
        }
    }
}

/// Why a launch failed, for callers that answer with typed errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchFailure {
    ProjectNotFound,
    /// The command id belongs to another thread, project or command.
    Conflict,
    ThreadNotFound,
    /// The thread rejected the command.
    Rejected,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("Thread launch {command} failed during {}: {cause}", operation.as_str())]
pub struct LaunchError {
    pub kind: LaunchFailure,
    pub operation: LaunchOperation,
    pub command: CommandId,
    pub project: String,
    pub thread: Option<ThreadId>,
    pub cause: String,
}

/// The `launches` row of a launch command.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchRecord {
    pub command: CommandId,
    pub thread: ThreadId,
    pub project: String,
    pub strategy: WorkspaceStrategy,
    /// A worktree this launch created and recorded; a retried preparation reuses it.
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
}

const LAUNCH_COLUMNS: &str = "command_id, thread_id, project, strategy, worktree_path, branch";

type RawLaunch = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);

fn launch_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawLaunch> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}

fn decode_launch(
    (command, thread, project, strategy, worktree_path, branch): RawLaunch,
) -> Result<LaunchRecord, StoreError> {
    Ok(LaunchRecord {
        command: CommandId::new(command).map_err(|error| StoreError::Corrupt(error.to_string()))?,
        thread: crate::store::thread_id(thread)?,
        project,
        strategy: serde_json::from_str(&strategy)?,
        worktree_path,
        branch,
    })
}

impl Store {
    pub fn launch(&self, command: &CommandId) -> Result<Option<LaunchRecord>, StoreError> {
        self.read(|c| {
            c.query_row(
                &format!("SELECT {LAUNCH_COLUMNS} FROM launches WHERE command_id = ?1"),
                [command.as_str()],
                launch_row,
            )
            .optional()?
            .map(decode_launch)
            .transpose()
        })
    }

    /// The thread's latest launch.
    pub fn thread_launch(&self, thread: &ThreadId) -> Result<Option<LaunchRecord>, StoreError> {
        self.read(|c| {
            c.query_row(
                &format!(
                    "SELECT {LAUNCH_COLUMNS} FROM launches WHERE thread_id = ?1
                     ORDER BY created_at DESC, rowid DESC LIMIT 1"
                ),
                [thread.as_str()],
                launch_row,
            )
            .optional()?
            .map(decode_launch)
            .transpose()
        })
    }

    /// False when the command already has a launch.
    pub async fn insert_launch(
        &self,
        record: &LaunchRecord,
        request: &LaunchThread,
        at: i64,
    ) -> Result<bool, StoreError> {
        let (command, thread, project) = (
            record.command.to_string(),
            record.thread.to_string(),
            record.project.clone(),
        );
        let strategy = serde_json::to_string(&record.strategy)?;
        let request = serde_json::to_string(request)?;
        self.write(move |tx| {
            Ok(tx.execute(
                "INSERT OR IGNORE INTO launches (command_id, thread_id, project, strategy, status,
                     request, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 'accepted', ?5, ?6, ?6)",
                params![command, thread, project, strategy, request, at],
            )? == 1)
        })
        .await
    }

    pub(crate) async fn delete_launch(&self, command: &CommandId) -> Result<(), StoreError> {
        let command = command.to_string();
        self.write(move |tx| {
            tx.execute("DELETE FROM launches WHERE command_id = ?1", [command])?;
            Ok(())
        })
        .await
    }

    pub(crate) async fn record_launch_worktree(
        &self,
        command: &CommandId,
        path: &str,
        branch: Option<&str>,
        at: i64,
    ) -> Result<(), StoreError> {
        let (command, path, branch) = (
            command.to_string(),
            path.to_owned(),
            branch.map(str::to_owned),
        );
        self.write(move |tx| {
            tx.execute(
                "UPDATE launches SET worktree_path = ?2, branch = ?3, status = 'provisioned',
                     updated_at = ?4
                 WHERE command_id = ?1",
                params![command, path, branch, at],
            )?;
            Ok(())
        })
        .await
    }
}

fn derived<T>(
    kind: &str,
    command: &CommandId,
    make: impl FnOnce(String) -> Result<T, agent_domain::ContractError>,
) -> T {
    make(format!("{kind}:{}", derived_uuid(kind, command.as_str()))).expect("derived id")
}

/// The thread a launch creates when the request names none.
pub fn launch_thread_id(command: &CommandId) -> ThreadId {
    derived("thread", command, ThreadId::new)
}

/// The workspace a launch binds at creation; a new worktree is bound once provisioned.
fn initial_workspace(strategy: &WorkspaceStrategy, root: &str) -> Option<Workspace> {
    match strategy {
        WorkspaceStrategy::Root { branch } => Some(Workspace {
            cwd: root.to_owned(),
            worktree_path: None,
            branch: branch.clone(),
        }),
        WorkspaceStrategy::ExistingWorktree { path, branch } => Some(Workspace {
            cwd: path.clone(),
            worktree_path: Some(path.clone()),
            branch: branch.clone(),
        }),
        WorkspaceStrategy::Worktree { .. } => None,
    }
}

pub(crate) async fn launch(
    context: &ExecutorContext,
    request: LaunchThread,
) -> Result<LaunchReply, LaunchError> {
    let error = |kind, operation, thread: Option<&ThreadId>, cause: String| LaunchError {
        kind,
        operation,
        command: request.command.clone(),
        project: request.project.clone(),
        thread: thread.cloned(),
        cause,
    };
    let Some(project) = context.ops.project(&request.project) else {
        return Err(error(
            LaunchFailure::ProjectNotFound,
            LaunchOperation::ResolveProject,
            None,
            "Project not found.".into(),
        ));
    };
    let store = &context.store;
    let lookup = request.command.clone();
    let (record, receipt) = store
        .blocking(move |store| Ok((store.launch(&lookup)?, store.receipt(&lookup)?)))
        .await
        .map_err(|e| {
            error(
                LaunchFailure::Unavailable,
                LaunchOperation::ReadReceipt,
                request.thread.as_ref(),
                e.to_string(),
            )
        })?;
    let replay = |thread: &ThreadId| {
        format!(
            "Launch command {} cannot be replayed for thread {thread}.",
            request.command
        )
    };
    let thread = match &record {
        Some(record) => {
            if let Some(requested) = &request.thread
                && requested != &record.thread
            {
                return Err(error(
                    LaunchFailure::Conflict,
                    LaunchOperation::CreateThread,
                    Some(requested),
                    replay(requested),
                ));
            }
            if record.project != request.project {
                return Err(error(
                    LaunchFailure::Conflict,
                    LaunchOperation::ResolveProject,
                    Some(&record.thread),
                    "Project identity changed.".into(),
                ));
            }
            let state = context.registry.state(&record.thread).await.map_err(|e| {
                error(
                    LaunchFailure::Unavailable,
                    LaunchOperation::CreateThread,
                    Some(&record.thread),
                    e.to_string(),
                )
            })?;
            if state
                .thread
                .as_ref()
                .is_none_or(|thread| thread.deleted_at.is_some())
            {
                return Err(error(
                    LaunchFailure::ThreadNotFound,
                    LaunchOperation::CreateThread,
                    Some(&record.thread),
                    "Thread not found.".into(),
                ));
            }
            record.thread.clone()
        }
        None => {
            let thread = request
                .thread
                .clone()
                .unwrap_or_else(|| launch_thread_id(&request.command));
            // Only an accepted create of this thread can be resumed as its launch.
            if let Some(receipt) = &receipt
                && (receipt.thread != thread
                    || receipt.receipt.reply != Reply::Thread(thread.clone()))
            {
                return Err(error(
                    LaunchFailure::Conflict,
                    LaunchOperation::CreateThread,
                    Some(&thread),
                    replay(&thread),
                ));
            }
            thread
        }
    };
    let inserted = match &record {
        Some(_) => false,
        None => store
            .insert_launch(
                &LaunchRecord {
                    command: request.command.clone(),
                    thread: thread.clone(),
                    project: request.project.clone(),
                    strategy: request.workspace.clone(),
                    worktree_path: None,
                    branch: None,
                },
                &request,
                context.registry.context().clock.now().millis(),
            )
            .await
            .map_err(|e| {
                error(
                    LaunchFailure::Unavailable,
                    LaunchOperation::CreateThread,
                    Some(&thread),
                    e.to_string(),
                )
            })?,
    };
    let mut resumed = !inserted || receipt.is_some();
    let created = context
        .registry
        .dispatch(
            &thread,
            request.command.clone(),
            Command::Create {
                thread: thread.clone(),
                project: request.project.clone(),
                title: request.title.clone(),
                selection: request.selection.clone(),
                runtime_mode: request.runtime_mode,
                interaction_mode: request.interaction_mode,
                workspace: initial_workspace(&request.workspace, &project.root),
            },
            CommandOrigin::Client,
        )
        .await
        .map_err(|e| {
            error(
                LaunchFailure::Unavailable,
                LaunchOperation::CreateThread,
                Some(&thread),
                e.to_string(),
            )
        })?;
    if let Reply::Rejected { reason } = &created.reply {
        if inserted {
            let _ = store.delete_launch(&request.command).await;
        }
        let (kind, cause) = if reason == "command-id-conflict" {
            (LaunchFailure::Conflict, replay(&thread))
        } else {
            (LaunchFailure::Rejected, reason.clone())
        };
        return Err(error(
            kind,
            LaunchOperation::CreateThread,
            Some(&thread),
            cause,
        ));
    }
    let Some(message) = &request.initial_message else {
        if inserted {
            let (context, thread) = (context.clone(), thread.clone());
            tokio::spawn(async move {
                if let Err(error) = prepare_workspace(&context, &thread, None).await {
                    tracing::warn!(%thread, %error, "thread workspace preparation failed");
                }
            });
        }
        return Ok(LaunchReply {
            thread,
            committed: created,
            resumed,
        });
    };
    let message_command =
        CommandId::new(format!("{}:initial-message", request.command)).expect("derived command id");
    let lookup = message_command.clone();
    resumed |= store
        .blocking(move |store| store.receipt(&lookup))
        .await
        .map_err(|e| {
            error(
                LaunchFailure::Unavailable,
                LaunchOperation::ReadReceipt,
                Some(&thread),
                e.to_string(),
            )
        })?
        .is_some();
    let sent = context
        .registry
        .dispatch(
            &thread,
            message_command,
            Command::Send(SendMessage {
                created_by: message.created_by,
                creation_source: message.creation_source.clone(),
                id: message
                    .id
                    .clone()
                    .unwrap_or_else(|| derived("message", &request.command, MessageId::new)),
                text: message.text.clone(),
                attachments: message.attachments.clone(),
                selection: Some(request.selection.clone()),
                mode: DispatchMode::DeferStart,
                intent: None,
                source_plan: None,
                title_seed: request.generate_title.then(|| request.title.clone()),
            }),
            CommandOrigin::Client,
        )
        .await
        .map_err(|e| {
            error(
                LaunchFailure::Unavailable,
                LaunchOperation::DispatchMessage,
                Some(&thread),
                e.to_string(),
            )
        })?;
    match &sent.reply {
        Reply::Run(_) => Ok(LaunchReply {
            thread,
            committed: sent,
            resumed,
        }),
        Reply::Rejected { reason } => Err(error(
            LaunchFailure::Rejected,
            LaunchOperation::DispatchMessage,
            Some(&thread),
            reason.clone(),
        )),
        other => Err(error(
            LaunchFailure::Unavailable,
            LaunchOperation::DispatchMessage,
            Some(&thread),
            format!("Initial message was accepted without a durable run: {other:?}"),
        )),
    }
}

#[cfg(test)]
mod tests;

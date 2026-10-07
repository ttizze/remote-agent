//! Conversation RPCs: protocol records in, runtime calls, typed errors out.
//! Streams answer with their first update and stay open with the rest.
use super::{
    Conversation, ProjectCatalog,
    diff::{DiffUnavailable, diff_refs},
};
use crate::checkpoints::DiffFormat;
use crate::host_rpc::connections::{HostReply, HostSubscription};
use agent_domain::{Attachment, Command, MessageAuthor, MessageContext, Reply, ThreadId};
use agent_protocol::{
    conversation as wire,
    conversation::{ConversationError, FACTS_FRAME_BUDGET, fact_updates},
    protocol::{self, Body, Call, MAX_FRAME_BYTES, Response},
};
use agent_runtime::{
    ImportError, InitialMessage, LaunchFailure, LaunchThread, QueryError, RuntimeError,
    ShellSubscribe, ThreadSnapshot, ThreadSubscribe, WorkspaceStrategy, launch_thread_id,
};
use serde::Serialize;
use std::{collections::VecDeque, path::Path};
use tokio_util::sync::CancellationToken;

type Result<T> = std::result::Result<T, ConversationError>;

fn unavailable(error: impl std::fmt::Display) -> ConversationError {
    ConversationError::Unavailable(error.to_string())
}

fn committed(committed: agent_runtime::Committed) -> wire::Committed {
    wire::Committed {
        reply: committed.reply,
        thread_sequence: committed.thread_seq,
        sequence: committed.global_seq,
        replayed: committed.replayed,
    }
}

fn snapshot(snapshot: ThreadSnapshot) -> wire::ThreadSnapshot {
    wire::ThreadSnapshot {
        snapshot_sequence: snapshot.snapshot_seq,
        thread_sequence: snapshot.thread_seq,
        state: snapshot.state,
        window: snapshot.window.map(|window| wire::SnapshotWindow {
            history_cursor: window.history_cursor,
            has_more_history: window.has_more_history,
            latest_local_ordinal: window.latest_local_ordinal,
            payload_budget_exceeded: window.payload_budget_exceeded,
        }),
    }
}

/// One runtime update as stream items; facts are split to fit frames.
fn thread_updates(update: agent_runtime::ThreadUpdate) -> Vec<wire::ThreadUpdate> {
    match update {
        agent_runtime::ThreadUpdate::Snapshot(value) => {
            vec![wire::ThreadUpdate::Snapshot(snapshot(value))]
        }
        agent_runtime::ThreadUpdate::Facts(facts) => fact_updates(
            facts
                .iter()
                .map(|stored| wire::SequencedFact {
                    sequence: stored.global_seq,
                    thread_sequence: stored.thread_seq,
                    fact: stored.fact.clone(),
                })
                .collect(),
            FACTS_FRAME_BUDGET,
        ),
        agent_runtime::ThreadUpdate::Synchronized => vec![wire::ThreadUpdate::Synchronized],
    }
}

fn shell_update(update: agent_runtime::ShellUpdate, catalog: &ProjectCatalog) -> wire::ShellUpdate {
    use agent_runtime::ShellUpdate as Shell;
    match update {
        Shell::Snapshot(value) => wire::ShellUpdate::Snapshot(wire::ShellSnapshot {
            snapshot_sequence: value.snapshot_seq,
            projects: value
                .projects
                .into_iter()
                .map(|p| catalog.wire(p))
                .collect(),
            threads: value
                .threads
                .into_iter()
                .map(|thread| *thread.row.summary)
                .collect(),
        }),
        Shell::ThreadUpdated { sequence, thread } => wire::ShellUpdate::ThreadUpdated {
            sequence,
            thread: thread.row.summary,
        },
        Shell::ThreadRemoved { sequence, thread } => wire::ShellUpdate::ThreadRemoved {
            sequence,
            thread_id: thread,
        },
        Shell::ProjectUpdated {
            sequence,
            project: value,
        } => wire::ShellUpdate::ProjectUpdated {
            sequence,
            project: Box::new(catalog.wire(value)),
        },
        Shell::ProjectRemoved { sequence, project } => wire::ShellUpdate::ProjectRemoved {
            sequence,
            project_id: project,
        },
        Shell::Projects { sequence, projects } => wire::ShellUpdate::Projects {
            sequence,
            projects: projects.into_iter().map(|p| catalog.wire(p)).collect(),
        },
        Shell::Synchronized => wire::ShellUpdate::Synchronized,
    }
}

fn history_row(row: agent_runtime::HistoryRow) -> wire::HistoryRow {
    wire::HistoryRow {
        position: row.position as u64,
        source: row.source,
        inherited: row.inherited,
        item: row.item,
        message: row.message,
        plan: row.plan,
    }
}

fn overflowed<T>(receiver: agent_runtime::LiveReceiver<T>) -> bool {
    receiver.overflowed()
}

fn live_buffer_full() -> agent_protocol::error::RpcFailure {
    ConversationError::LiveBufferFull.into()
}

fn query_error(error: QueryError) -> ConversationError {
    match error {
        QueryError::Cursor(_) => ConversationError::InvalidCursor,
        QueryError::InvalidSearch => ConversationError::InvalidSearch,
        error => unavailable(error),
    }
}

/// A response whose first item answers the call and whose later items follow on
/// the same stream until the subscriber falls behind or disconnects.
fn stream<T: Serialize + Send + 'static>(
    mut first: VecDeque<T>,
    empty: T,
    mut next: impl FnMut() -> futures_util::future::BoxFuture<'static, Option<Vec<T>>> + Send + 'static,
    cancel: CancellationToken,
) -> HostReply
where
    Body: From<T>,
{
    let head = first.pop_front().unwrap_or(empty);
    let initial = match protocol::encode(Response::Success {
        result: Body::from(head),
    }) {
        Ok(bytes) if bytes.len() <= MAX_FRAME_BYTES => bytes,
        _ => {
            return Response::from_result::<(), _>(Err(ConversationError::ResponseTooLarge)).into();
        }
    };
    let (sender, receiver) = tokio::sync::mpsc::channel(2);
    let task = tokio::spawn(async move {
        loop {
            while let Some(item) = first.pop_front() {
                let Ok(frame) = protocol::encode(item) else {
                    return;
                };
                if frame.len() > MAX_FRAME_BYTES {
                    return;
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    sent = sender.send(frame) => if sent.is_err() { return },
                }
            }
            let items = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                items = next() => items,
            };
            match items {
                Some(items) => first.extend(items),
                None => return,
            }
        }
    });
    HostReply {
        initial,
        updates: Some(HostSubscription {
            receiver,
            _task: tokio_util::task::AbortOnDropHandle::new(task),
        }),
    }
}

impl Conversation {
    /// Answers a conversation call, or `None` for other calls.
    pub(crate) async fn call(&self, call: &Call, cancel: CancellationToken) -> Option<HostReply> {
        let reply = match call {
            Call::ThreadStream(params) => self.subscribe_thread(params, cancel).await,
            Call::ShellStream(params) => self.subscribe_shell(params, cancel).await,
            Call::Dispatch(params) => self.dispatch(params).await.map(reply),
            Call::Launch(params) => self.launch(params).await.map(reply),
            Call::GetThread(params) => self.get_thread(params).await.map(reply),
            Call::TurnItem(params) => self.turn_item(params).await.map(reply),
            Call::ReadHistory(params) => self.read_history(params).await.map(reply),
            Call::Search(params) => self.search(params).map(reply),
            Call::TurnDiff(params) => self.turn_diff(params).await.map(reply),
            Call::ScanAgentSessions(_) => self.scan().await.map(reply),
            Call::ImportAgentSessions(params) => self.import(params).await.map(reply),
            Call::SetupStream(params) => Ok(self.subscribe_setup(params, cancel)),
            Call::CancelSetup(params) => Ok(reply(wire::SetupCancelled {
                cancelled: self.runtime.cancel_setup(&params.thread_id).await,
            })),
            _ => return None,
        };
        Some(reply.unwrap_or_else(|error| Response::from_result::<(), _>(Err(error)).into()))
    }

    /// The thread's committed state; a thread never created is not found.
    async fn existing(&self, thread: &ThreadId) -> Result<agent_runtime::ThreadView> {
        let view = self.runtime.state(thread).await.map_err(unavailable)?;
        if view.state.thread.is_none() {
            return Err(ConversationError::ThreadNotFound(thread.clone()));
        }
        Ok(view)
    }

    /// Claims uploads into the thread's storage and rebinds the context records
    /// that named them.
    fn claim(
        &self,
        thread: &ThreadId,
        attachments: &mut Vec<Attachment>,
        context: Option<&mut MessageContext>,
    ) -> Result<()> {
        if attachments.is_empty() {
            return Ok(());
        }
        let before: Vec<String> = attachments.iter().map(|file| file.id.clone()).collect();
        *attachments = self
            .resources
            .files
            .claim(thread.as_str(), attachments)
            .map_err(|error| {
                ConversationError::AttachmentUnavailable(format!(
                    "attachment is unavailable: {error:#}"
                ))
            })?;
        if let Some(context) = context {
            let claimed = before
                .into_iter()
                .zip(attachments.iter())
                .map(|(before, file)| (before, file.id.clone()))
                .collect();
            context.remap_attachments(&claimed);
        }
        Ok(())
    }

    /// Message attachments are claimed into the thread's storage before the actor
    /// sees them.
    fn claim_command(&self, thread: &ThreadId, command: &mut Command) -> Result<()> {
        match command {
            Command::Send(message) => {
                self.claim(thread, &mut message.attachments, message.context.as_mut())
            }
            Command::EditQueued {
                attachments: Some(attachments),
                context,
                ..
            } => self.claim(thread, attachments, context.as_mut()),
            Command::Respond { attachments, .. } => attachments
                .values_mut()
                .try_for_each(|attachments| self.claim(thread, attachments, None)),
            _ => Ok(()),
        }
    }

    pub(crate) async fn dispatch(&self, params: &wire::Dispatch) -> Result<wire::Committed> {
        params.validate()?;
        let mut command = params.command.clone();
        if let Command::Create { project, .. } = &command
            && !self
                .resources
                .projects
                .list()
                .iter()
                .any(|candidate| candidate.id == *project)
        {
            return Err(ConversationError::ProjectNotFound(project.clone()));
        }
        self.claim_command(&params.thread_id, &mut command)?;
        let result = self
            .runtime
            .dispatch(params.thread_id.clone(), params.command_id.clone(), command)
            .await
            .map_err(|error| match error {
                RuntimeError::Closed => ConversationError::Unavailable(error.to_string()),
                RuntimeError::AttachmentUnavailable(message) => {
                    ConversationError::AttachmentUnavailable(message)
                }
                error => unavailable(error),
            })?;
        match &result.reply {
            Reply::Rejected { reason } if reason == "command-id-conflict" => Err(
                ConversationError::CommandIdConflict(params.command_id.clone()),
            ),
            Reply::Rejected { reason } if reason == "thread-not-found" => {
                Err(ConversationError::ThreadNotFound(params.thread_id.clone()))
            }
            _ => Ok(committed(result)),
        }
    }

    pub(crate) async fn launch(&self, params: &wire::Launch) -> Result<wire::Launched> {
        let thread = params
            .thread_id
            .clone()
            .unwrap_or_else(|| launch_thread_id(&params.command_id));
        let initial_message = match &params.message {
            Some(message) => {
                let mut attachments = message.attachments.clone();
                let mut context = message.context.clone();
                self.claim(&thread, &mut attachments, context.as_mut())?;
                Some(InitialMessage {
                    id: message.id.clone(),
                    text: message.text.clone(),
                    attachments,
                    created_by: MessageAuthor::User,
                    creation_source: message.creation_source.clone(),
                    context,
                })
            }
            None => None,
        };
        let title_seed = params
            .message
            .as_ref()
            .and_then(|message| message.title_seed.clone());
        let request = LaunchThread {
            command: params.command_id.clone(),
            thread: params.thread_id.clone(),
            project: params.project_id.clone(),
            generate_title: title_seed.is_some(),
            title: title_seed.unwrap_or_else(|| params.title.clone()),
            selection: params.selection.clone(),
            runtime_mode: params.runtime_mode,
            interaction_mode: params.interaction_mode,
            workspace: match &params.workspace {
                wire::WorkspaceStrategy::Root { branch } => WorkspaceStrategy::Root {
                    branch: branch.clone(),
                },
                wire::WorkspaceStrategy::ExistingWorktree {
                    worktree_path,
                    branch,
                } => WorkspaceStrategy::ExistingWorktree {
                    path: worktree_path.clone(),
                    branch: branch.clone(),
                },
                wire::WorkspaceStrategy::Worktree {
                    base_ref,
                    branch,
                    start_from_origin,
                } => WorkspaceStrategy::Worktree {
                    base_ref: base_ref.clone(),
                    branch: branch.clone(),
                    start_from_origin: *start_from_origin,
                },
            },
            created_by: MessageAuthor::User,
            creation_source: params.message.as_ref().map_or_else(
                || "desktop".into(),
                |message| message.creation_source.clone(),
            ),
            initial_message,
        };
        let launched = self
            .runtime
            .launch(request)
            .await
            .map_err(|error| match error.kind {
                LaunchFailure::ProjectNotFound => ConversationError::ProjectNotFound(error.project),
                LaunchFailure::Conflict => ConversationError::CommandIdConflict(error.command),
                LaunchFailure::ThreadNotFound => {
                    ConversationError::ThreadNotFound(error.thread.unwrap_or(thread.clone()))
                }
                LaunchFailure::Rejected | LaunchFailure::Unavailable => unavailable(error),
            })?;
        Ok(wire::Launched {
            thread_id: launched.thread,
            committed: committed(launched.committed),
            resumed: launched.resumed,
        })
    }

    async fn subscribe_thread(
        &self,
        params: &wire::SubscribeThread,
        cancel: CancellationToken,
    ) -> Result<HostReply> {
        self.existing(&params.thread_id).await?;
        let mut subscription = self
            .runtime
            .subscribe_thread(
                params.thread_id.clone(),
                ThreadSubscribe {
                    after_global_seq: params.after_sequence,
                    request_completion_marker: params.request_completion_marker,
                    accept_bounded_snapshot: params.accept_bounded_snapshot,
                    ..ThreadSubscribe::default()
                },
            )
            .await
            .map_err(unavailable)?;
        // The actor queues the snapshot or replay before it returns the subscription.
        let mut first = VecDeque::new();
        while let Ok(update) = subscription.updates.try_recv() {
            first.extend(thread_updates(update));
        }
        let updates = std::sync::Arc::new(tokio::sync::Mutex::new(Some(subscription.updates)));
        Ok(stream(
            first,
            wire::ThreadUpdate::Facts(vec![]),
            move || {
                let updates = updates.clone();
                Box::pin(async move {
                    let mut updates = updates.lock().await;
                    let receiver = updates.as_mut()?;
                    match receiver.recv().await {
                        Some(update) => Some(thread_updates(update)),
                        None => overflowed(updates.take()?)
                            .then(|| vec![wire::ThreadUpdate::Failed(live_buffer_full())]),
                    }
                })
            },
            cancel,
        ))
    }

    async fn subscribe_shell(
        &self,
        params: &wire::SubscribeShell,
        cancel: CancellationToken,
    ) -> Result<HostReply> {
        let mut subscription = self
            .runtime
            .subscribe_shell(ShellSubscribe {
                after_global_seq: params.after_sequence,
                request_completion_marker: params.request_completion_marker,
                location: match params.location {
                    wire::ShellLocation::Active => agent_runtime::ShellLocation::Active,
                    wire::ShellLocation::Archived => agent_runtime::ShellLocation::Archived,
                },
                ..ShellSubscribe::default()
            })
            .await
            .map_err(unavailable)?;
        // The first update is queued before the subscription returns.
        let first = subscription
            .updates
            .recv()
            .await
            .map(|update| shell_update(update, &self.resources.projects))
            .ok_or_else(|| unavailable("the shell stream closed"))?;
        let updates = std::sync::Arc::new(tokio::sync::Mutex::new(Some(subscription.updates)));
        let catalog = self.resources.projects.clone();
        Ok(stream(
            VecDeque::from([first]),
            wire::ShellUpdate::Synchronized,
            move || {
                let (updates, catalog) = (updates.clone(), catalog.clone());
                Box::pin(async move {
                    let mut updates = updates.lock().await;
                    let receiver = updates.as_mut()?;
                    match receiver.recv().await {
                        Some(update) => Some(vec![shell_update(update, &catalog)]),
                        None => overflowed(updates.take()?)
                            .then(|| vec![wire::ShellUpdate::Failed(live_buffer_full())]),
                    }
                })
            },
            cancel,
        ))
    }

    /// Whole snapshots, so a slow client only holds the newest.
    fn subscribe_setup(
        &self,
        params: &wire::SubscribeSetup,
        cancel: CancellationToken,
    ) -> HostReply {
        let mut changes = self.runtime.subscribe_setup(&params.thread_id);
        let first = changes.borrow_and_update().clone();
        let changes = std::sync::Arc::new(tokio::sync::Mutex::new(changes));
        stream(
            VecDeque::from([first]),
            None,
            move || {
                let changes = changes.clone();
                Box::pin(async move {
                    let mut changes = changes.lock().await;
                    changes.changed().await.ok()?;
                    Some(vec![changes.borrow_and_update().clone()])
                })
            },
            cancel,
        )
    }

    async fn get_thread(&self, params: &wire::GetThread) -> Result<wire::ThreadSnapshot> {
        let view = self.existing(&params.thread_id).await?;
        Ok(snapshot(ThreadSnapshot::build(
            &view.state,
            view.head,
            params.bounded,
        )))
    }

    async fn turn_item(&self, params: &wire::GetTurnItem) -> Result<Option<wire::TurnItemDetail>> {
        let view = self.existing(&params.thread_id).await?;
        Ok(
            agent_runtime::turn_item(&view.state, &params.item_id).map(|detail| {
                wire::TurnItemDetail {
                    row: history_row(detail.row),
                    task: detail.task,
                }
            }),
        )
    }

    async fn read_history(&self, params: &wire::ReadHistory) -> Result<wire::HistoryPage> {
        self.existing(&params.thread_id).await?;
        let page = self
            .runtime
            .history(&params.thread_id, params.cursor.as_deref())
            .await
            .map_err(query_error)?;
        Ok(wire::HistoryPage {
            rows: page.rows.into_iter().map(history_row).collect(),
            next_cursor: page.next_cursor,
            has_more: page.has_more,
        })
    }

    fn search(&self, params: &wire::Search) -> Result<Vec<wire::SearchMatch>> {
        let matches = self
            .runtime
            .search(&params.query, params.limit.map(|limit| limit as usize))
            .map_err(query_error)?;
        Ok(matches
            .into_iter()
            .map(|found| wire::SearchMatch {
                thread_id: found.thread,
                project_id: found.project,
                source: match found.source {
                    agent_runtime::SearchSource::User => wire::SearchSource::User,
                    agent_runtime::SearchSource::Assistant => wire::SearchSource::Assistant,
                },
                snippet: found.snippet,
                message_created_at: found.message_created_at,
            })
            .collect())
    }

    /// Equal ordinals are an empty diff.
    pub(crate) async fn turn_diff(&self, params: &wire::GetTurnDiff) -> Result<wire::TurnDiff> {
        let result = |diff: String| wire::TurnDiff {
            thread_id: params.thread_id.clone(),
            from_run_ordinal: params.from_run_ordinal,
            to_run_ordinal: params.to_run_ordinal,
            diff,
        };
        if params.from_run_ordinal == params.to_run_ordinal {
            return Ok(result(String::new()));
        }
        let view = self.existing(&params.thread_id).await?;
        let refs = diff_refs(&view.state, params.from_run_ordinal, params.to_run_ordinal).map_err(
            |unavailable| {
                ConversationError::CheckpointUnavailable(match unavailable {
                    DiffUnavailable::Range { requested, .. } => requested,
                    DiffUnavailable::Checkpoint { ordinal, .. } => ordinal,
                    DiffUnavailable::WorkspaceMissing => params.to_run_ordinal,
                })
            },
        )?;
        let diff = self
            .resources
            .checkpoints
            .diff(
                Path::new(&refs.cwd),
                &refs.from,
                &refs.to,
                params.ignore_whitespace.unwrap_or(true),
                DiffFormat::Patch,
                false,
            )
            .await
            .map_err(|error| ConversationError::DiffFailed(format!("{error:#}")))?;
        Ok(result(diff))
    }

    async fn scan(&self) -> Result<wire::SessionScan> {
        let scan = self.runtime.scan().await.map_err(unavailable)?;
        Ok(wire::SessionScan {
            candidates: scan
                .candidates
                .into_iter()
                .map(|candidate| wire::SessionCandidate {
                    path: candidate.path.to_string_lossy().into_owned(),
                    title: candidate.title,
                    project_id: candidate.project,
                    sources: candidate.sources,
                    thread_count: candidate.thread_count as u64,
                    last_active_at: candidate.last_active_at,
                    already_imported: candidate.already_imported,
                    git: candidate.git.map(|git| wire::ProjectGit {
                        remote_key: git.remote_key,
                        repository: git.repository,
                    }),
                })
                .collect(),
            scanned_at: scan.scanned_at,
            truncated: scan.truncated,
        })
    }

    async fn import(&self, params: &wire::ImportAgentSessions) -> Result<wire::ImportCounts> {
        let counts = self
            .runtime
            .import(
                &params.project_id,
                params.expected_root.as_deref().map(Path::new),
            )
            .await
            .map_err(|error| match error {
                ImportError::ProjectNotFound(project) => {
                    ConversationError::ProjectNotFound(project)
                }
                ImportError::ProjectChanged(project) => ConversationError::ProjectChanged(project),
                error => unavailable(error),
            })?;
        Ok(wire::ImportCounts {
            imported: counts.imported as u64,
            skipped: counts.skipped as u64,
        })
    }
}

fn reply<T>(value: T) -> HostReply
where
    Body: From<T>,
{
    Response::from_result::<T, ConversationError>(Ok(value)).into()
}

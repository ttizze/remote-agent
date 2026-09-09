//! The single state owner. RPC work runs concurrently; publication follows wire order.
use crate::{
    client::*,
    peer::{PeerError, PeerEvent, RpcPeer},
    state::{Effect, Event, Intent, Snapshot, reduce},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use host_protocol::{RpcMessageKind, classify_message};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex, RwLock},
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::{CancellationToken, DropGuard};

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Applied,
    StartedThread(String),
    Submitted(Option<String>),
    RemoteHostPaired(String),
    SessionImages(Vec<SessionImage>),
}
struct Dispatch {
    intent: Intent,
    complete: oneshot::Sender<Result<Outcome, PeerError>>,
}
struct Applied {
    sequence: Option<u64>,
    event: Option<Event>,
    outcome: Outcome,
}
struct Completed {
    result: Result<Applied, PeerError>,
    request_id: Option<u64>,
    failed_submission: Option<String>,
    terminal: Option<String>,
    complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
}
struct Scheduled {
    effect: Effect,
    complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
}
pub struct Store {
    snapshot: Arc<RwLock<Arc<Snapshot>>>,
    updates: watch::Sender<Arc<Snapshot>>,
    commands: mpsc::UnboundedSender<Dispatch>,
    stop: CancellationToken,
    _close_on_drop: DropGuard,
    finished: watch::Receiver<Option<Result<(), String>>>,
}
impl Store {
    pub fn new(peer: RpcPeer, snapshot: Snapshot) -> Self {
        Self::start(peer, snapshot, None, None)
    }
    fn start(
        peer: RpcPeer,
        snapshot: Snapshot,
        session: Option<crate::transport::Session>,
        endpoint: Option<crate::transport::Endpoint>,
    ) -> Self {
        let snapshot = Arc::new(RwLock::new(Arc::new(snapshot)));
        let (updates, _) = watch::channel(snapshot.read().unwrap().clone());
        let (commands, incoming) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let (finished_tx, finished) = watch::channel(None);
        let run = run(
            peer,
            snapshot.clone(),
            updates.clone(),
            incoming,
            stop.clone(),
            session,
            endpoint,
        );
        tokio::spawn(async move {
            finished_tx.send_replace(Some(run.await.map_err(|error| error.to_string())));
        });
        Self {
            snapshot,
            updates,
            commands,
            _close_on_drop: stop.clone().drop_guard(),
            stop,
            finished,
        }
    }
    pub async fn connect(
        endpoint: crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        snapshot: Snapshot,
        invitation: Option<uuid::Uuid>,
    ) -> Result<Self, crate::transport::TransportError> {
        let session = match endpoint.connect(ticket).await {
            Ok(session) => session,
            Err(error) => {
                endpoint.close().await;
                return Err(error);
            }
        };
        let setup = async {
            let peer = session
                .open_peer(std::time::Duration::from_secs(30), 64)
                .await?;
            if let Some(invitation) = invitation {
                peer.request::<_, <Pair as Operation>::Output>(Pair::METHOD, &Pair { invitation })
                    .await?;
            }
            Ok::<_, crate::transport::TransportError>(peer)
        }
        .await;
        match setup {
            Ok(peer) => Ok(Self::start(peer, snapshot, Some(session), Some(endpoint))),
            Err(error) => {
                session.close();
                endpoint.close().await;
                Err(error)
            }
        }
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.updates.subscribe()
    }
    /// Enqueue at the call site so UI intent order does not depend on executor
    /// polling order. Dropping the returned receipt does not cancel the intent.
    pub fn dispatch(
        &self,
        intent: Intent,
    ) -> impl Future<Output = Result<Outcome, PeerError>> + Send + use<> {
        let (complete, result) = oneshot::channel();
        let queued = self.commands.send(Dispatch { intent, complete });
        async move {
            queued.map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?;
            result
                .await
                .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?
        }
    }
    pub async fn close(&self) -> Result<(), PeerError> {
        self.stop.cancel();
        let mut finished = self.finished.clone();
        loop {
            if let Some(result) = finished.borrow_and_update().clone() {
                break result.map_err(PeerError::ConnectionClosed);
            }
            finished
                .changed()
                .await
                .map_err(|_| PeerError::ConnectionClosed("store task stopped".into()))?;
        }
    }
}

fn apply(
    snapshot: &RwLock<Arc<Snapshot>>,
    updates: &watch::Sender<Arc<Snapshot>>,
    event: Event,
) -> Vec<Effect> {
    let mut current = snapshot.write().unwrap();
    let (next, effects) = reduce(&current, event);
    let same_threads = match (&current.threads, &next.threads) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    if Arc::ptr_eq(&current.terminals, &next.terminals)
        && Arc::ptr_eq(&current.conversations, &next.conversations)
        && same_threads
        && Arc::ptr_eq(&current.models, &next.models)
        && Arc::ptr_eq(&current.requests, &next.requests)
        && Arc::ptr_eq(&current.drafts, &next.drafts)
        && Arc::ptr_eq(&current.pending_submissions, &next.pending_submissions)
        && Arc::ptr_eq(&current.file_drafts, &next.file_drafts)
        && Arc::ptr_eq(&current.workspace, &next.workspace)
        && Arc::ptr_eq(&current.navigation, &next.navigation)
        && Arc::ptr_eq(&current.activity, &next.activity)
        && Arc::ptr_eq(&current.management, &next.management)
        && Arc::ptr_eq(&current.list_query, &next.list_query)
        && current.list_request == next.list_request
        && current.connected == next.connected
        && current.error == next.error
    {
        return effects;
    }
    *current = Arc::new(next);
    updates.send_replace(current.clone());
    effects
}
fn completion_sequence(completed: &Completed) -> Option<u64> {
    completed.request_id?;
    match &completed.result {
        Ok(applied) => applied.sequence,
        Err(PeerError::Remote { sequence, .. } | PeerError::InvalidResponse { sequence, .. }) => {
            *sequence
        }
        _ => None,
    }
}
fn finish(
    ordered: &Mutex<BTreeSet<u64>>,
    snapshot: &RwLock<Arc<Snapshot>>,
    updates: &watch::Sender<Arc<Snapshot>>,
    completed: Completed,
) -> Vec<Scheduled> {
    if let Some(id) = completed.request_id {
        ordered.lock().unwrap().remove(&id);
    }
    let mut effects = Vec::new();
    let result = match completed.result {
        Ok(applied) => {
            if let Some(event) = applied.event {
                effects = apply(snapshot, updates, event);
            }
            Ok(applied.outcome)
        }
        Err(error) => {
            if let Some(handle) = completed.terminal {
                apply(
                    snapshot,
                    updates,
                    Event::TerminalFailed {
                        handle,
                        reason: error.to_string(),
                    },
                );
            }
            if let Some(id) = completed.failed_submission {
                apply(snapshot, updates, Event::SubmissionFailed(id));
            }
            effects = apply(snapshot, updates, Event::Failed(error.to_string()));
            Err(error)
        }
    };
    let continuation = effects.iter().position(|effect| {
        matches!(
            effect,
            Effect::StartSubmission { .. } | Effect::Submit { .. }
        )
    });
    let mut complete = completed.complete;
    if continuation.is_none()
        && let Some(complete) = complete.take()
    {
        let _ = complete.send(result);
    }
    effects
        .into_iter()
        .enumerate()
        .map(|(index, effect)| Scheduled {
            effect,
            complete: if Some(index) == continuation {
                complete.take()
            } else {
                None
            },
        })
        .collect()
}
fn decode_message(line: &str) -> Result<Event, PeerError> {
    let message =
        classify_message(line).map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    if message.kind() == RpcMessageKind::Request {
        return serde_json::from_str(line)
            .map(Event::ServerRequest)
            .map_err(|error| PeerError::InvalidMessage(error.to_string()));
    }
    #[derive(serde::Deserialize)]
    struct Notification {
        method: String,
        #[serde(default)]
        params: serde_json::Value,
    }
    let notification: Notification =
        serde_json::from_str(line).map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    Ok(Event::Notification {
        method: notification.method,
        params: notification.params,
    })
}
async fn run(
    peer: RpcPeer,
    snapshot: Arc<RwLock<Arc<Snapshot>>>,
    updates: watch::Sender<Arc<Snapshot>>,
    mut commands: mpsc::UnboundedReceiver<Dispatch>,
    stop: CancellationToken,
    session: Option<crate::transport::Session>,
    endpoint: Option<crate::transport::Endpoint>,
) -> Result<(), PeerError> {
    let mut events = peer.subscribe();
    let peer = Arc::new(peer);
    let client = Client::new(peer.clone());
    let ordered = Mutex::new(BTreeSet::new());
    let mut jobs = FuturesUnordered::new();
    let mut received = VecDeque::new();
    let mut completed = BTreeMap::new();
    let mut stream_open = true;
    let mut terminal_reason = None;
    let mut terminal_commands = VecDeque::new();
    let mut terminal_running = false;
    let mut effects: Vec<Scheduled> = apply(&snapshot, &updates, Event::Connected)
        .into_iter()
        .map(|effect| Scheduled {
            effect,
            complete: None,
        })
        .collect();
    let reason = loop {
        for Scheduled { effect, complete } in effects.drain(..) {
            if terminal_handle(&effect).is_some() {
                terminal_commands.push_back(Scheduled { effect, complete });
                continue;
            }
            jobs.push(perform(
                &client,
                &peer,
                &ordered,
                session.as_ref(),
                snapshot.read().unwrap().clone(),
                effect,
                complete,
            ));
        }
        if !terminal_running
            && let Some(Scheduled { effect, complete }) = terminal_commands.pop_front()
        {
            terminal_running = true;
            jobs.push(perform(
                &client,
                &peer,
                &ordered,
                session.as_ref(),
                snapshot.read().unwrap().clone(),
                effect,
                complete,
            ));
        }
        if !stream_open && jobs.is_empty() && completed.is_empty() && received.is_empty() {
            break terminal_reason.unwrap_or_else(|| "RPC stream closed".into());
        }
        tokio::select! {
            _ = stop.cancelled() => break "store closed".into(),
            command = commands.recv() => {
                let Some(command) = command else { break "store closed".into() };
                let pending = apply(&snapshot,&updates,Event::Intent(command.intent));
                let mut complete = Some(command.complete);
                if pending.is_empty() { let _ = complete.take().unwrap().send(Ok(Outcome::Applied)); }
                for effect in pending {
                    effects.push(Scheduled { effect, complete: complete.take() });
                }
            }
            result = jobs.next(), if !jobs.is_empty() => {
                let result = result.unwrap();
                if result.terminal.is_some() { terminal_running = false; }
                if let Some(sequence) = completion_sequence(&result) { completed.insert(sequence,result); }
                else { effects.extend(finish(&ordered,&snapshot,&updates,result)); }
            }
            event = events.recv(), if stream_open => match event {
                Ok(event) => received.push_back(event),
                Err(broadcast::error::RecvError::Lagged(count)) => break format!("lost {count} RPC events; reconnect and reload state"),
                Err(broadcast::error::RecvError::Closed) => stream_open = false,
            }
        }
        while let Some(event) = received.front() {
            let ordered_response = match event {
                PeerEvent::Response {
                    sequence,
                    request_id: Some(id),
                    ..
                } => ordered.lock().unwrap().contains(id).then_some(*sequence),
                _ => None,
            };
            if let Some(sequence) = ordered_response {
                let Some(result) = completed.remove(&sequence) else {
                    break;
                };
                effects.extend(finish(&ordered, &snapshot, &updates, result));
            }
            match received.pop_front().unwrap() {
                PeerEvent::Message(frame) => {
                    let event = decode_message(&frame.value)
                        .unwrap_or_else(|error| Event::Failed(error.to_string()));
                    effects.extend(apply(&snapshot, &updates, event).into_iter().map(|effect| {
                        Scheduled {
                            effect,
                            complete: None,
                        }
                    }));
                }
                PeerEvent::Closed(reason) => {
                    terminal_reason = Some(reason);
                }
                PeerEvent::Response { .. } => {}
            }
        }
    };
    // Let an already issued PTY command finish before killing its process.
    // In particular, a spawn must register its handle before cleanup can kill it.
    // The peer's request deadline also bounds this wait.
    while terminal_running {
        let Some(completed) = jobs.next().await else {
            break;
        };
        if completed.terminal.is_some() {
            terminal_running = false;
        }
    }
    drop(jobs);
    // PTYs live in the daemon's upstream connection, so closing this view's
    // connection alone does not terminate them.
    let terminals = snapshot.read().unwrap().terminals.clone();
    for (handle, terminal) in terminals.iter() {
        if !matches!(
            terminal.phase,
            crate::state::TerminalPhase::Exited(_) | crate::state::TerminalPhase::Closed
        ) {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client.call(&KillTerminal {
                    process_handle: handle,
                }),
            )
            .await;
        }
    }
    let result = peer.close().await;
    if let Some(session) = session {
        session.close();
    }
    if let Some(endpoint) = endpoint {
        endpoint.close().await;
    }
    apply(&snapshot, &updates, Event::Disconnected(reason));
    result
}

fn terminal_handle(effect: &Effect) -> Option<&str> {
    match effect {
        Effect::Execute(
            Intent::StartTerminal { handle, .. }
            | Intent::WriteTerminal { handle, .. }
            | Intent::ResizeTerminal { handle, .. }
            | Intent::CloseTerminal(handle),
        ) => Some(handle),
        _ => None,
    }
}

async fn perform(
    client: &Client,
    peer: &RpcPeer,
    ordered: &Mutex<BTreeSet<u64>>,
    session: Option<&crate::transport::Session>,
    snapshot: Arc<Snapshot>,
    effect: Effect,
    complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
) -> Completed {
    let terminal = terminal_handle(&effect).map(str::to_owned);
    let failed_submission = match &effect {
        Effect::StartSubmission {
            client_user_message_id,
            ..
        }
        | Effect::Submit {
            client_user_message_id,
            ..
        } => Some(client_user_message_id.clone()),
        _ => None,
    };
    let mut request_id = None;
    let result = async {
        let applied = match effect {
            Effect::StartSubmission {
                draft_key,
                cwd,
                generation,
                client_user_message_id,
                draft,
            } => {
                let reply = call_ordered(
                    client,
                    ordered,
                    &mut request_id,
                    &StartThread {
                        cwd: cwd.as_deref(),
                        model: draft.model.as_deref(),
                    },
                )
                .await?;
                let id = reply.value.thread.id.clone().expect("validated thread ID");
                Applied {
                    sequence: Some(reply.sequence),
                    event: Some(Event::DraftThreadCreated {
                        thread: reply.value.thread,
                        draft_key,
                        generation,
                        client_user_message_id,
                        draft,
                    }),
                    outcome: Outcome::StartedThread(id),
                }
            }
            Effect::Submit {
                thread_id,
                client_user_message_id,
                draft,
            } => {
                let target = submission_target(
                    snapshot.conversations.get(&thread_id).map(Arc::as_ref),
                    snapshot.threads.as_ref().and_then(|list| {
                        list.data
                            .iter()
                            .find(|thread| thread.id.as_ref() == Some(&thread_id))
                    }),
                    snapshot.activity.active.get(&thread_id).copied(),
                )?;
                let mut input = Vec::with_capacity(
                    draft.attachments.len() + usize::from(!draft.text.is_empty()),
                );
                if !draft.text.is_empty() {
                    input.push(Input::Text {
                        text: &draft.text,
                        text_elements: &[],
                    });
                }
                for attachment in &draft.attachments {
                    input.push(if attachment.is_image {
                        Input::LocalImage {
                            path: &attachment.path,
                        }
                    } else {
                        Input::Mention {
                            path: &attachment.path,
                            name: &attachment.name,
                        }
                    });
                }
                let reply = client
                    .submit(
                        &Submission {
                            thread_id: &thread_id,
                            client_user_message_id: &client_user_message_id,
                            input: &input,
                            model: draft.model.as_deref(),
                            effort: draft.effort.as_deref(),
                            service_tier: draft.service_tier.as_deref(),
                        },
                        target,
                    )
                    .await?;
                Applied {
                    sequence: Some(reply.sequence),
                    event: Some(Event::Submitted {
                        thread_id,
                        client_user_message_id,
                        draft,
                        turn_id: reply.value.clone(),
                    }),
                    outcome: Outcome::Submitted(reply.value),
                }
            }
            Effect::Execute(intent) => match intent {
                Intent::StartTerminal { handle, cwd, size } => {
                    let reply = call_ordered(
                        client,
                        ordered,
                        &mut request_id,
                        &StartTerminal {
                            process_handle: &handle,
                            cwd: &cwd,
                            size,
                        },
                    )
                    .await?;
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::TerminalStarted(handle)),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::WriteTerminal { handle, data } => {
                    use base64::Engine;
                    // Requests are serialized by the Store actor, including chunks
                    // from a single paste, so shell bytes cannot overtake each other.
                    for chunk in data.chunks(16 * 1024) {
                        client
                            .call(&WriteTerminal {
                                process_handle: &handle,
                                delta_base64: &base64::engine::general_purpose::STANDARD
                                    .encode(chunk),
                            })
                            .await?;
                    }
                    Applied {
                        sequence: None,
                        event: None,
                        outcome: Outcome::Applied,
                    }
                }
                Intent::ResizeTerminal { handle, size } => {
                    client
                        .call(&ResizeTerminal {
                            process_handle: &handle,
                            size,
                        })
                        .await?;
                    Applied {
                        sequence: None,
                        event: None,
                        outcome: Outcome::Applied,
                    }
                }
                Intent::CloseTerminal(handle) => {
                    let reply = call_ordered(
                        client,
                        ordered,
                        &mut request_id,
                        &KillTerminal {
                            process_handle: &handle,
                        },
                    )
                    .await?;
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::TerminalClosed(handle)),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::Transcribe {
                    draft_key,
                    audio,
                    send,
                    client_user_message_id,
                } => {
                    let draft = snapshot.drafts.get(&draft_key).cloned().unwrap_or_default();
                    let reply = client.call(&Transcribe { audio: &audio }).await?;
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::Transcribed {
                            draft_key,
                            generation: snapshot.navigation.generation,
                            draft,
                            text: reply.value.text,
                            send,
                            client_user_message_id,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::UploadAttachment {
                    draft_key,
                    mut attachment,
                    directory,
                } => {
                    let session = session.ok_or_else(|| {
                        PeerError::InvalidMessage("binary transfers require an iroh session".into())
                    })?;
                    let uploaded = crate::transfers::upload_file(
                        peer,
                        || async { session.open_stream().await.map_err(std::io::Error::other) },
                        std::path::Path::new(&attachment.path),
                        std::path::Path::new(&directory),
                        &attachment.name,
                    )
                    .await
                    .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
                    #[derive(serde::Deserialize)]
                    struct Uploaded {
                        path: String,
                    }
                    let uploaded: Uploaded = serde_json::from_value(uploaded)
                        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
                    attachment.path = uploaded.path;
                    Applied {
                        sequence: None,
                        event: Some(Event::AttachmentUploaded {
                            draft_key,
                            attachment,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::DownloadFile {
                    source,
                    destination,
                } => {
                    let session = session.ok_or_else(|| {
                        PeerError::InvalidMessage("binary transfers require an iroh session".into())
                    })?;
                    crate::transfers::download_file(
                        peer,
                        || async { session.open_stream().await.map_err(std::io::Error::other) },
                        &source,
                        &destination,
                    )
                    .await
                    .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
                    Applied {
                        sequence: None,
                        event: None,
                        outcome: Outcome::Applied,
                    }
                }

                Intent::LoadSessionImages(thread_id) => Applied {
                    sequence: None,
                    event: None,
                    outcome: Outcome::SessionImages(client.session_images(&thread_id).await?),
                },
                Intent::LoadHostManagement => {
                    let (status, remotes) = tokio::try_join!(
                        client.call(&ReadHostStatus {}),
                        client.call(&ListRemoteHosts {})
                    )?;
                    Applied {
                        sequence: None,
                        event: Some(Event::HostManagementLoaded {
                            generation: snapshot.management.generation,
                            status: status.value,
                            remotes: remotes.value,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::CreateInvitation => {
                    let reply = client.call(&CreateInvitation {}).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::InvitationCreated(reply.value)),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::PairRemoteHost { invitation, name } => {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if now >= invitation.expires_at {
                        return Err(PeerError::InvalidMessage("invitation expired".into()));
                    }
                    let local = session.ok_or_else(|| {
                        PeerError::InvalidMessage("pairing requires an iroh session".into())
                    })?;
                    let ticket = invitation.endpoint.parse().map_err(
                        |error: crate::transport::TransportError| {
                            PeerError::InvalidMessage(error.to_string())
                        },
                    )?;
                    {
                        let remote = scopeguard::guard(
                            local
                                .connect(&ticket)
                                .await
                                .map_err(|error| PeerError::ConnectionClosed(error.to_string()))?,
                            |session| session.close(),
                        );
                        let peer = remote
                            .open_peer(std::time::Duration::from_secs(20), 8)
                            .await
                            .map_err(|error| PeerError::ConnectionClosed(error.to_string()))?;
                        peer.request::<_, <Pair as Operation>::Output>(
                            Pair::METHOD,
                            &Pair {
                                invitation: invitation.invitation,
                            },
                        )
                        .await?;
                        peer.close().await?;
                    }
                    let reply = client
                        .call(&RegisterRemoteHost {
                            ticket: &invitation.endpoint,
                            name: &name,
                        })
                        .await?;
                    let id = reply.value.id.clone();
                    Applied {
                        sequence: None,
                        event: Some(Event::RemoteHostPaired(reply.value)),
                        outcome: Outcome::RemoteHostPaired(id),
                    }
                }
                Intent::RemoveRemoteHost(id) => {
                    client.call(&RemoveRemoteHost { id: &id }).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::RemoteHostRemoved(id)),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::RevokeDevice(id) => {
                    client.call(&RevokeDevice { node_id: &id }).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::DeviceRevoked(id)),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::OpenThread(thread_id) => {
                    let reply = call_ordered(
                        client,
                        ordered,
                        &mut request_id,
                        &ReadThread {
                            thread_id: &thread_id,
                            include_turns: true,
                            paginate_history: true,
                            defer_item_details: true,
                        },
                    )
                    .await?;
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::ThreadOpened {
                            generation: snapshot.navigation.generation,
                            thread: reply.value.thread,
                            model: reply.value.model,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::ListFiles(path) => {
                    let reply = client.call(&ListFiles { path: &path }).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::FilesLoaded {
                            request: snapshot.workspace.directory_request,
                            files: reply.value,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::ReadFile { path, .. } => {
                    let reply = client.call(&ReadFile { path: &path }).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::FileLoaded {
                            request: snapshot.workspace.file_request,
                            file: reply.value,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::SaveFile(path) => {
                    let submitted = snapshot.file_drafts.get(&path).ok_or_else(|| {
                        PeerError::InvalidMessage("file has no draft to save".into())
                    })?;
                    let reply = client
                        .call(&WriteFile {
                            path: &path,
                            revision: &submitted.revision,
                            text: &submitted.text,
                        })
                        .await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::FileSaved {
                            submitted: submitted.clone(),
                            file: reply.value,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::ReviewWorkspace(cwd) => {
                    let reply = client.call(&ReviewWorkspace { cwd: &cwd }).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::ReviewLoaded {
                            request: snapshot.workspace.review_request,
                            review: reply.value,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::ReadWorktreeSettings => {
                    let reply = client.call(&ReadWorktreeSettings {}).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::WorktreeSettingsLoaded {
                            request: snapshot.workspace.settings_request,
                            settings: reply.value,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::UpdateWorktreeSettings(settings) => {
                    let reply = client.call(&UpdateWorktreeSettings(&settings)).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::WorktreeSettingsLoaded {
                            request: snapshot.workspace.settings_request,
                            settings: reply.value,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::ListThreads(query) => {
                    let reply = call_ordered(
                        client,
                        ordered,
                        &mut request_id,
                        &ListThreads {
                            title_only: true,
                            query: &query,
                        },
                    )
                    .await?;
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::ThreadsLoaded {
                            request: snapshot.list_request,
                            threads: reply.value,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::StartThread { cwd, model } => {
                    let reply = call_ordered(
                        client,
                        ordered,
                        &mut request_id,
                        &StartThread {
                            cwd: cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()),
                            model: model.as_deref(),
                        },
                    )
                    .await?;
                    let id = reply.value.thread.id.clone().expect("validated thread ID");
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::ThreadRefreshed(reply.value.thread)),
                        outcome: Outcome::StartedThread(id),
                    }
                }
                Intent::ReadThread(thread_id) => {
                    let reply = call_ordered(
                        client,
                        ordered,
                        &mut request_id,
                        &ReadThread {
                            thread_id: &thread_id,
                            include_turns: true,
                            paginate_history: true,
                            defer_item_details: true,
                        },
                    )
                    .await?;
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::ThreadRefreshed(reply.value.thread)),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::ReadOlder {
                    thread_id,
                    turn_id,
                    cursor,
                } => {
                    let reply = if let Some(turn_id) = &turn_id {
                        call_ordered(
                            client,
                            ordered,
                            &mut request_id,
                            &OlderItems {
                                thread_id: &thread_id,
                                turn_id,
                                cursor: cursor.as_deref(),
                                defer_item_details: true,
                            },
                        )
                        .await?
                    } else {
                        call_ordered(
                            client,
                            ordered,
                            &mut request_id,
                            &OlderTurns {
                                thread_id: &thread_id,
                                turn_id: None,
                                cursor: cursor.as_deref(),
                                defer_item_details: true,
                            },
                        )
                        .await?
                    };
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::OlderLoaded {
                            thread_id,
                            thread: reply.value.thread,
                            turn_id,
                            cursor,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::ReadItem {
                    thread_id,
                    turn_id,
                    item_id,
                } => {
                    let reply = call_ordered(
                        client,
                        ordered,
                        &mut request_id,
                        &ReadItem {
                            thread_id: &thread_id,
                            turn_id: &turn_id,
                            item_id: &item_id,
                        },
                    )
                    .await?;
                    Applied {
                        sequence: Some(reply.sequence),
                        event: Some(Event::ItemLoaded {
                            thread_id,
                            turn_id,
                            item: reply.value.item,
                        }),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::LoadModels => Applied {
                    sequence: None,
                    event: Some(Event::ModelsLoaded(client.models().await?)),
                    outcome: Outcome::Applied,
                },
                Intent::Interrupt { thread_id, turn_id } => {
                    client
                        .call(&InterruptTurn {
                            thread_id: &thread_id,
                            turn_id: &turn_id,
                        })
                        .await?;
                    Applied {
                        sequence: None,
                        event: None,
                        outcome: Outcome::Applied,
                    }
                }
                Intent::Respond { request_id, answer } => {
                    let request =
                        snapshot
                            .requests
                            .get(&request_id.to_string())
                            .ok_or_else(|| {
                                PeerError::InvalidMessage(
                                    "server request is no longer pending".into(),
                                )
                            })?;
                    client.respond(request, &answer).await?;
                    Applied {
                        sequence: None,
                        event: Some(Event::RequestResolved(request_id)),
                        outcome: Outcome::Applied,
                    }
                }
                Intent::Watch {
                    thread_id,
                    watch_key,
                    watch_id,
                    path,
                } => {
                    client
                        .call(&WatchThread {
                            thread_id: &thread_id,
                            watch_key,
                            watch_id,
                            path: path.as_deref(),
                        })
                        .await?;
                    Applied {
                        sequence: None,
                        event: None,
                        outcome: Outcome::Applied,
                    }
                }
                Intent::Unwatch {
                    watch_key,
                    watch_id,
                } => {
                    client
                        .call(&UnwatchThread {
                            watch_key,
                            watch_id,
                        })
                        .await?;
                    Applied {
                        sequence: None,
                        event: None,
                        outcome: Outcome::Applied,
                    }
                }
                intent @ (Intent::AcknowledgeTerminal { .. }
                | Intent::Submit { .. }
                | Intent::AddAttachment { .. }
                | Intent::RemoveAttachment { .. }
                | Intent::NewChat(_)
                | Intent::SetDraft { .. }
                | Intent::SetDraftText { .. }
                | Intent::SelectModel { .. }
                | Intent::SelectEffort { .. }
                | Intent::SelectServiceTier { .. }
                | Intent::SetFileDraft { .. }) => Applied {
                    sequence: None,
                    event: Some(Event::Intent(intent)),
                    outcome: Outcome::Applied,
                },
            },
        };
        Ok(applied)
    }
    .await;
    Completed {
        result,
        request_id,
        failed_submission,
        terminal,
        complete,
    }
}

// Register before polling the request: a fast reply must never overtake its
// state publication. Gallery reads use the same RPC methods without publishing
// conversation snapshots, so method names cannot identify these requests.
async fn call_ordered<O: Operation + Sync>(
    client: &Client,
    ordered: &Mutex<BTreeSet<u64>>,
    request_id: &mut Option<u64>,
    operation: &O,
) -> Result<crate::peer::Reply<O::Output>, PeerError> {
    let request = client.call(operation);
    *request_id = request.wire_id();
    if let Some(id) = *request_id {
        ordered.lock().unwrap().insert(id);
    }
    request.await
}

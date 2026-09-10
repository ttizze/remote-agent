//! The single state owner. RPC work runs concurrently; publication follows wire order.
use crate::{
    client::{self as rpc, *},
    peer::{PeerError, PeerEvent, RpcPeer},
    state::{Effect, Event, Intent, Loaded, Snapshot, reduce},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use host_protocol::{RpcMessageKind, classify_message};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::{CancellationToken, DropGuard};

#[derive(Debug, Default, PartialEq)]
pub enum Outcome {
    #[default]
    Applied,
    StartedThread(String),
    Submitted(Option<String>),
    RemoteHostPaired(String),
    SessionImages(Vec<SessionImage>),
}
enum Command {
    Dispatch(Dispatch),
    Disconnect(oneshot::Sender<Result<(), PeerError>>),
    Attach {
        connection: Connection,
        complete: oneshot::Sender<Result<(), PeerError>>,
    },
}
struct Connection {
    peer: RpcPeer,
    session: Option<crate::transport::Session>,
}
impl Connection {
    async fn close(self) {
        let _ = self.peer.close().await;
        if let Some(session) = self.session {
            session.close();
        }
    }
}
struct Dispatch {
    effects: Vec<Effect>,
    snapshot: Arc<Snapshot>,
    complete: oneshot::Sender<Result<Outcome, PeerError>>,
}
#[derive(Default)]
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
    snapshot: Option<Arc<Snapshot>>,
    complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
}
pub struct Store {
    updates: watch::Receiver<Arc<Snapshot>>,
    publications: Mutex<Option<watch::Sender<Arc<Snapshot>>>>,
    commands: mpsc::UnboundedSender<Command>,
    stop: CancellationToken,
    _close_on_drop: DropGuard,
    finished: watch::Receiver<Option<Result<(), String>>>,
}
impl Store {
    pub fn new(peer: RpcPeer, snapshot: Snapshot) -> Self {
        Self::start(Some(peer), snapshot, None)
    }
    pub fn offline(snapshot: Snapshot) -> Self {
        let (snapshot, _) = reduce(&snapshot, Event::Disconnected("Host not connected".into()));
        Self::start(None, snapshot, None)
    }
    fn start(
        peer: Option<RpcPeer>,
        snapshot: Snapshot,
        session: Option<crate::transport::Session>,
    ) -> Self {
        let snapshot = if peer.is_some() {
            reduce(&snapshot, Event::Connected).0
        } else {
            snapshot
        };
        let (writer, updates) = watch::channel(Arc::new(snapshot));
        let (commands, mut incoming) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let (finished_tx, finished) = watch::channel(None);
        let publications = writer.clone();
        let shutdown = stop.clone();
        tokio::spawn(async move {
            let mut connection = peer.map(|peer| Connection { peer, session });
            let mut result = Ok(());
            loop {
                if let Some(Connection { peer, session }) = connection.take() {
                    result = run(
                        peer,
                        publications.clone(),
                        &mut incoming,
                        shutdown.clone(),
                        session,
                    )
                    .await;
                }
                // Local intents and reconnects share one state owner.
                connection = run_offline(&publications, &mut incoming, &shutdown).await;
                if connection.is_none() {
                    break;
                }
            }
            apply(&publications, Event::Disconnected("store closed".into()));
            finished_tx.send_replace(Some(result.map_err(|error| error.to_string())));
        });
        Self {
            updates,
            publications: Mutex::new(Some(writer)),
            commands,
            _close_on_drop: stop.clone().drop_guard(),
            stop,
            finished,
        }
    }
    pub async fn connect(
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        snapshot: Snapshot,
        invitation: Option<uuid::Uuid>,
    ) -> Result<Self, crate::transport::TransportError> {
        let store = Self::offline(snapshot);
        store.reconnect(endpoint, ticket, invitation).await?;
        Ok(store)
    }
    /// Prepare transport while the actor continues to accept local edits.
    pub async fn reconnect(
        &self,
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        invitation: Option<uuid::Uuid>,
    ) -> Result<(), crate::transport::TransportError> {
        let session = endpoint.connect(ticket).await?;
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
            Ok(peer) => {
                let (complete, result) = oneshot::channel();
                let command = Command::Attach {
                    connection: Connection {
                        peer,
                        session: Some(session),
                    },
                    complete,
                };
                if let Err(failed) = self.commands.send(command) {
                    if let Command::Attach { connection, .. } = failed.0 {
                        connection.close().await;
                    }
                    return Err(PeerError::ConnectionClosed("store is closed".into()).into());
                }
                result
                    .await
                    .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
                Ok(())
            }
            Err(error) => {
                session.close();
                Err(error)
            }
        }
    }
    /// Release the current transport while retaining offline editing and observers.
    pub async fn disconnect(&self) -> Result<(), PeerError> {
        let (complete, result) = oneshot::channel();
        self.commands
            .send(Command::Disconnect(complete))
            .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?;
        result
            .await
            .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.updates.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.updates.clone()
    }
    /// Publish the pure transition before returning to a native input control.
    /// The watch lock orders publication and effect enqueueing across callers.
    /// Watch releases that lock before waking potentially reentrant FFI consumers.
    /// Only effects wait for the executor; dropping a receipt does not cancel them.
    pub fn dispatch(
        &self,
        intent: Intent,
    ) -> impl Future<Output = Result<Outcome, PeerError>> + Send + use<> {
        let mut receipt = Err(PeerError::ConnectionClosed("store is closed".into()));
        let publications = self.publications.lock().unwrap().clone();
        if let Some(publications) = publications {
            receipt = Ok(None);
            publications.send_if_modified(|current| {
                if self.stop.is_cancelled() || self.commands.is_closed() {
                    receipt = Err(PeerError::ConnectionClosed("store is closed".into()));
                    return false;
                }
                let (effects, changed) = apply_locked(current, Event::Intent(intent));
                if !effects.is_empty() {
                    let (complete, result) = oneshot::channel();
                    receipt = self
                        .commands
                        .send(Command::Dispatch(Dispatch {
                            effects,
                            snapshot: current.clone(),
                            complete,
                        }))
                        .map(|()| Some(result))
                        .map_err(|_| PeerError::ConnectionClosed("store is closed".into()));
                }
                changed
            });
        }
        async move {
            match receipt? {
                None => Ok(Outcome::Applied),
                Some(result) => result
                    .await
                    .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?,
            }
        }
    }
    pub async fn close(&self) -> Result<(), PeerError> {
        self.stop.cancel();
        self.publications.lock().unwrap().take();
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

fn apply(updates: &watch::Sender<Arc<Snapshot>>, event: Event) -> Vec<Effect> {
    let mut effects = Vec::new();
    updates.send_if_modified(|current| {
        let (produced, changed) = apply_locked(current, event);
        effects = produced;
        changed
    });
    effects
}
fn apply_locked(current: &mut Arc<Snapshot>, event: Event) -> (Vec<Effect>, bool) {
    let (next, effects) = reduce(current, event);
    let same_threads = match (&current.threads, &next.threads) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    if Arc::ptr_eq(&current.terminals, &next.terminals)
        && Arc::ptr_eq(&current.account, &next.account)
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
        return (effects, false);
    }
    *current = Arc::new(next);
    (effects, true)
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
                effects = apply(updates, event);
            }
            Ok(applied.outcome)
        }
        Err(error) => {
            if let Some(handle) = completed.terminal {
                apply(
                    updates,
                    Event::TerminalFailed {
                        handle,
                        reason: error.to_string(),
                    },
                );
            }
            if let Some(id) = completed.failed_submission {
                apply(updates, Event::SubmissionFailed(id));
            }
            effects = apply(updates, Event::Failed(error.to_string()));
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
            snapshot: None,
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
    updates: watch::Sender<Arc<Snapshot>>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    stop: CancellationToken,
    session: Option<crate::transport::Session>,
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
    let mut disconnected = None;
    let mut effects: Vec<Scheduled> = Vec::new();
    let reason = loop {
        for Scheduled {
            effect,
            snapshot: captured,
            complete,
        } in effects.drain(..)
        {
            if terminal_handle(&effect).is_some() {
                terminal_commands.push_back(Scheduled {
                    effect,
                    snapshot: captured,
                    complete,
                });
                continue;
            }
            jobs.push(perform(
                Some(&client),
                Some(&peer),
                &ordered,
                session.as_ref(),
                captured.unwrap_or_else(|| updates.borrow().clone()),
                effect,
                complete,
            ));
        }
        if !terminal_running
            && let Some(Scheduled {
                effect,
                snapshot: captured,
                complete,
            }) = terminal_commands.pop_front()
        {
            terminal_running = true;
            jobs.push(perform(
                Some(&client),
                Some(&peer),
                &ordered,
                session.as_ref(),
                captured.unwrap_or_else(|| updates.borrow().clone()),
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
                let command = match command {
                    Command::Dispatch(command) => command,
                    Command::Disconnect(complete) => {
                        disconnected = Some(complete);
                        break "Host disconnected".into();
                    }
                    Command::Attach { connection, complete } => {
                        connection.close().await;
                        let _ = complete.send(Err(PeerError::ConnectionClosed("store is already connected".into())));
                        continue;
                    }
                };
                let mut complete = Some(command.complete);
                for effect in command.effects {
                    effects.push(Scheduled { effect, snapshot: Some(command.snapshot.clone()), complete: complete.take() });
                }
            }
            result = jobs.next(), if !jobs.is_empty() => {
                let result = result.unwrap();
                if result.terminal.is_some() { terminal_running = false; }
                if let Some(sequence) = completion_sequence(&result) { completed.insert(sequence,result); }
                else { effects.extend(finish(&ordered,&updates,result)); }
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
                effects.extend(finish(&ordered, &updates, result));
            }
            match received.pop_front().unwrap() {
                PeerEvent::Message(frame) => {
                    let event = decode_message(&frame.value)
                        .unwrap_or_else(|error| Event::Failed(error.to_string()));
                    effects.extend(apply(&updates, event).into_iter().map(|effect| Scheduled {
                        effect,
                        snapshot: None,
                        complete: None,
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
    let terminals = updates.borrow().terminals.clone();
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
    apply(&updates, Event::Disconnected(reason));
    if let Some(complete) = disconnected {
        let _ = complete.send(Ok(()));
    }
    result
}

async fn run_offline(
    updates: &watch::Sender<Arc<Snapshot>>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    stop: &CancellationToken,
) -> Option<Connection> {
    let ordered = Mutex::new(BTreeSet::new());
    while !stop.is_cancelled() {
        let command = tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            command = commands.recv() => match command { Some(command) => command, None => break },
        };
        let command = match command {
            Command::Dispatch(command) => command,
            Command::Disconnect(complete) => {
                let _ = complete.send(Ok(()));
                continue;
            }
            Command::Attach {
                connection,
                complete,
            } => {
                apply(updates, Event::Connected);
                let _ = complete.send(Ok(()));
                return Some(connection);
            }
        };
        let mut complete = Some(command.complete);
        for effect in command.effects {
            // Disconnect already removed the subscription on the Host.
            if matches!(effect, Effect::Execute(Intent::Unwatch { .. })) {
                continue;
            }
            let result = perform(
                None,
                None,
                &ordered,
                None,
                command.snapshot.clone(),
                effect,
                complete.take(),
            )
            .await;
            drop(finish(&ordered, updates, result));
        }
        if let Some(complete) = complete {
            let _ = complete.send(Ok(Outcome::Applied));
        }
    }
    None
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

macro_rules! operation_outcome {
    ($output:expr) => {
        Outcome::Applied
    };
    ($output:expr, $value:ident => $outcome:expr) => {{
        let $value = $output;
        $outcome
    }};
}
macro_rules! execute_operations {
    ($snapshot:ident;
     loaded { $(
        $variant:ident $(($($arg:ident: $ty:ty),* $(,)?))? $({$($field:ident: $field_ty:ty),* $(,)?})?
        $(prepare { $($prepare:tt)* })?
        => $operation:ty, $params:expr, $is_ordered:literal;
        ($($context:ident: $context_ty:ty = $capture:expr),* $(,)?) => $apply:ident
        $([$value:ident => $outcome:expr])?;
     )* }
     ignored { $(
        $ignored:ident $(($($iarg:ident: $ity:ty),* $(,)?))? $({$($ifield:ident: $ifield_ty:ty),* $(,)?})?
        => $ignore_params:expr;
     )* }
     other { $($other:tt)* }
     ($client:expr, $ordered:expr, $request_id:expr, $intent:expr)
     { $($compound:tt)* }
    ) => {
        match $intent {
            $(Intent::$variant $(($($arg),*))? $({$($field),*})? => {
                $($($prepare)*)?
                let reply = if $is_ordered {
                    call_ordered($client, $ordered, $request_id, &$params).await?
                } else { $client.call(&$params).await? };
                let outcome = operation_outcome!(&reply.value $(, $value => $outcome)?);
                Applied {
                    sequence: $is_ordered.then_some(reply.sequence),
                    outcome,
                    event: Some(Event::Loaded(Loaded::$variant { $($context: $capture,)* output: reply.value })),
                }
            },)*
            $(Intent::$ignored $(($($iarg),*))? $({$($ifield),*})? => {
                $client.call(&$ignore_params).await?;
                Applied::default()
            },)*
            $($compound)*
        }
    };
}

async fn perform(
    client: Option<&Client>,
    peer: Option<&RpcPeer>,
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
        let client =
            client.ok_or_else(|| PeerError::ConnectionClosed("Host not connected".into()))?;
        let peer = peer.ok_or_else(|| PeerError::ConnectionClosed("Host not connected".into()))?;
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
            Effect::Execute(intent) => crate::state::operations::operation_table!(execute_operations, snapshot, (client, ordered, &mut request_id, intent), {
                    Intent::WriteTerminal { handle, data } => {
                        use base64::Engine;
                        // Requests are serialized by the Store actor, including chunks
                        // from a single paste, so shell bytes cannot overtake each other.
                        for chunk in data.chunks(16 * 1024) {
                            client
                                .call(&WriteTerminal {
                                    process_handle: &handle,
                                    delta_base64: &base64::engine::general_purpose::STANDARD.encode(chunk),
                                })
                                .await?;
                        }
                        Applied::default()
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
                            event: Some(Event::AttachmentUploaded {
                                draft_key,
                                attachment,
                            }),
                            ..Applied::default()
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
                        Applied::default()
                    }
                    Intent::LoadSessionImages(thread_id) => Applied {
                        event: None,
                        outcome: Outcome::SessionImages(client.session_images(&thread_id).await?),
                        ..Applied::default()
                    },
                    Intent::LoadHostManagement => {
                        let (status, remotes) = tokio::try_join!(
                            client.call(&ReadHostStatus {}),
                            client.call(&ListRemoteHosts {})
                        )?;
                        Applied {
                            event: Some(Event::HostManagementLoaded {
                                generation: snapshot.management.generation,
                                status: status.value,
                                remotes: remotes.value,
                            }),
                            ..Applied::default()
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
                            event: Some(Event::RemoteHostPaired(reply.value)),
                            outcome: Outcome::RemoteHostPaired(id),
                            ..Applied::default()
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
                            ..Applied::default()
                        }
                    }
                    Intent::LoadModels => Applied {
                        event: Some(Event::ModelsLoaded(client.models().await?)),
                        ..Applied::default()
                    },
                    Intent::Respond { request_id, answer } => {
                        let request = snapshot
                            .requests
                            .get(&request_id.to_string())
                            .ok_or_else(|| {
                                PeerError::InvalidMessage("server request is no longer pending".into())
                            })?;
                        client.respond(request, &answer).await?;
                        Applied {
                            event: Some(Event::RequestResolved(request_id)),
                            ..Applied::default()
                        }
                    }
                    intent @ (Intent::AcknowledgeTerminal { .. }
                    | Intent::Submit { .. }
                    | Intent::AddAttachment { .. }
                    | Intent::RemoveAttachment { .. }
                    | Intent::NewChat(_)
                    | Intent::ShowThreadList
                    | Intent::SetDraft { .. }
                    | Intent::SetDraftText { .. }
                    | Intent::SelectModel { .. }
                    | Intent::SelectEffort { .. }
                    | Intent::SelectServiceTier { .. }
                    | Intent::SetFileDraft { .. }) => Applied {
                        event: Some(Event::Intent(intent)),
                        ..Applied::default()
                    },
            }),
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

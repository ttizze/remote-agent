//! The single state owner. RPC work runs concurrently; publication follows wire order.
use crate::peer::{RpcMessage, RpcMessageKind};
use crate::{
    client::*,
    peer::{PeerError, PeerEvent, RpcPeer},
    state::{Event, Intent, Snapshot, operations as op, reduce},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio_util::sync::{CancellationToken, DropGuard};

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Outcome {
    #[default]
    Applied,
    StartedThread {
        id: String,
    },
    Submitted {
        turn_id: Option<String>,
    },
    RemoteHostPaired {
        id: String,
    },
    Visualization {
        html: String,
    },
    SessionImages {
        images: Vec<SessionImage>,
    },
}
enum Command {
    Dispatch(Dispatch),
    ResumePeer {
        endpoint: crate::transport::Endpoint,
        remote: crate::transport::NodeId,
        complete: oneshot::Sender<Option<Arc<RpcPeer>>>,
    },
    Disconnect(oneshot::Sender<Result<(), PeerError>>),
    Attach {
        connection: Connection,
        attempt: CancellationToken,
        storage_scope: String,
        complete: oneshot::Sender<Result<(), PeerError>>,
    },
}
struct Connection {
    peer: Arc<RpcPeer>,
    session: Option<crate::transport::Session>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        // Includes candidates dropped from the command queue during shutdown.
        if let Some(session) = &self.session {
            session.close();
        }
    }
}
type CompletionSender = oneshot::Sender<Result<Outcome, PeerError>>;
struct Dispatch {
    effects: Vec<Effect>,
    snapshot: Arc<Snapshot>,
    complete: CompletionSender,
}
struct Applied {
    sequence: Option<u64>,
    application: Box<dyn Application>,
    outcome: Outcome,
}
#[derive(Clone, Default)]
struct Receipt(Arc<Mutex<Vec<CompletionSender>>>);
impl Receipt {
    fn new(sender: CompletionSender) -> Self {
        Self(Arc::new(Mutex::new(vec![sender])))
    }
    fn join(&self, other: Self) {
        if !Arc::ptr_eq(&self.0, &other.0) {
            let mut waiters = self.0.lock().unwrap();
            if waiters.len() >= 128 {
                drop(waiters);
                other.send(Err(PeerError::InvalidMessage(
                    "too many waiters for item read".into(),
                )));
            } else {
                waiters.extend(other.0.lock().unwrap().drain(..));
            }
        }
    }
    fn send(self, result: Result<Outcome, PeerError>) {
        for sender in self.0.lock().unwrap().drain(..) {
            let _ = sender.send(result.clone());
        }
    }
}
struct Completed {
    item_read: Option<op::ReadItem>,
    delivery_attempted: bool,
    epoch: u64,
    result: Result<Applied, PeerError>,
    request_id: Option<u64>,
    failed_submission: Option<String>,
    terminal: Option<String>,
    complete: Option<Receipt>,
}
struct Scheduled {
    effect: Effect,
    snapshot: Arc<Snapshot>,
    complete: Option<Receipt>,
}
pub struct Store {
    updates: watch::Receiver<Arc<Snapshot>>,
    publications: Mutex<Option<watch::Sender<Arc<Snapshot>>>>,
    commands: mpsc::UnboundedSender<Command>,
    connection_attempt: Mutex<CancellationToken>,
    stop: CancellationToken,
    _close_on_drop: DropGuard,
    finished: watch::Receiver<Option<Result<(), String>>>,
}
impl Store {
    pub fn new(peer: RpcPeer, snapshot: Snapshot) -> Self {
        Self::start(Some(peer), snapshot)
    }
    pub fn offline(snapshot: Snapshot) -> Self {
        let (snapshot, _) = reduce(&snapshot, Event::Disconnected("Host not connected".into()));
        Self::start(None, snapshot)
    }
    fn start(peer: Option<RpcPeer>, snapshot: Snapshot) -> Self {
        let (writer, updates) = watch::channel(Arc::new(snapshot));
        let connection = peer.map(|peer| {
            (
                Connection {
                    peer: Arc::new(peer),
                    session: None,
                },
                apply(&writer, Event::Connected),
            )
        });
        let (commands, mut incoming) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let (finished_tx, finished) = watch::channel(None);
        let publications = writer.clone();
        let shutdown = stop.clone();
        tokio::spawn(async move {
            let mut connection = connection;
            let mut result = Ok(());
            loop {
                if let Some((connection, effects)) = connection.take() {
                    result = run(
                        connection,
                        publications.clone(),
                        &mut incoming,
                        shutdown.clone(),
                        effects,
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
            connection_attempt: Mutex::new(stop.child_token()),
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
    /// Reuse a responsive Host connection, then refresh without blocking interaction.
    /// Only reads are retried; pending submissions retain their delivery evidence.
    pub async fn resume(
        &self,
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
    ) -> Result<(), crate::transport::TransportError> {
        let attempt = {
            let mut current = self.connection_attempt.lock().unwrap();
            current.cancel();
            *current = self.stop.child_token();
            current.clone()
        };
        let guard = attempt.clone().drop_guard();
        let (complete, receiver) = oneshot::channel();
        self.commands
            .send(Command::ResumePeer {
                endpoint: endpoint.clone(),
                remote: ticket.node_id(),
                complete,
            })
            .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?;
        let reusable = tokio::select! {
            biased;
            _ = attempt.cancelled() => return Err(PeerError::ConnectionClosed("connection attempt cancelled".into()).into()),
            result = receiver => result.unwrap_or(None),
        };
        if let Some(peer) = reusable {
            // Provider reads can be slow even when QUIC is healthy. Check the Host
            // itself so history/list latency cannot force transport replacement.
            let client = Client::new(peer);
            let responsive = tokio::select! {
                biased;
                _ = attempt.cancelled() => return Err(PeerError::ConnectionClosed("connection attempt cancelled".into()).into()),
                result = tokio::time::timeout(std::time::Duration::from_secs(1), client.call(&ReadHostStatus {})) => result,
            };
            match responsive {
                Ok(Ok(_)) => {
                    let snapshot = self.snapshot();
                    if let Some(id) = &snapshot.navigation.thread_id {
                        drop(self.dispatch(Intent::ReadThread(op::ReadThread::new(id.clone()))));
                    }
                    drop(self.dispatch(Intent::ListThreads(op::ListThreads::new(
                        (*snapshot.list_query).clone(),
                    ))));
                    drop(self.dispatch(Intent::LoadModels(op::LoadModels {})));
                    guard.disarm();
                    return Ok(());
                }
                // An application response proves reachability; reconnecting cannot fix it.
                Ok(Err(error @ (PeerError::Remote { .. } | PeerError::InvalidResponse { .. }))) => {
                    return Err(error.into());
                }
                _ => {}
            }
        }
        let disconnected = {
            // Atomically move this same recovery attempt into replacement. A newer
            // attempt must not be cancelled by an older read reaching its deadline.
            let _current = self.connection_attempt.lock().unwrap();
            if attempt.is_cancelled() {
                return Err(
                    PeerError::ConnectionClosed("connection attempt cancelled".into()).into(),
                );
            }
            self.request_disconnect()?
        };
        guard.disarm();
        self.attach_connection(endpoint, ticket, None, attempt, disconnected)
            .await
    }
    /// Replace transport while retaining local edits. A newer reconnect or
    /// disconnect cancels setup before it can attach an obsolete connection.
    pub async fn reconnect(
        &self,
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        invitation: Option<uuid::Uuid>,
    ) -> Result<(), crate::transport::TransportError> {
        let (attempt, disconnected) = {
            let mut current = self.connection_attempt.lock().unwrap();
            current.cancel();
            *current = self.stop.child_token();
            // Queue replacement under the same lock as cancellation so callers
            // cannot reorder a newer attempt behind an older disconnect.
            (current.clone(), self.request_disconnect()?)
        };
        self.attach_connection(endpoint, ticket, invitation, attempt, disconnected)
            .await
    }
    async fn attach_connection(
        &self,
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        invitation: Option<uuid::Uuid>,
        attempt: CancellationToken,
        disconnected: oneshot::Receiver<Result<(), PeerError>>,
    ) -> Result<(), crate::transport::TransportError> {
        let guard = attempt.clone().drop_guard();
        let setup = async {
            disconnected
                .await
                .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
            // Explicit replacement never waits for a read on the old transport.
            // Foreground resume has already applied its separate short deadline.
            let session = scopeguard::guard(endpoint.connect(ticket).await?, |session| {
                session.close();
            });
            let peer = session
                .open_peer(std::time::Duration::from_secs(30), 64)
                .await?;
            if let Some(invitation) = invitation {
                peer.request::<_, <Pair as RpcMethod>::Output>(Pair::METHOD, &Pair { invitation })
                    .await?;
            }
            let scope = peer
                .request::<_, String>("host/session/scope", &serde_json::json!({}))
                .await?
                .value;
            if scope.is_empty() || scope.len() > 256 {
                return Err(
                    PeerError::InvalidMessage("invalid provider storage scope".into()).into(),
                );
            }
            let (complete, result) = oneshot::channel();
            let command = Command::Attach {
                connection: Connection {
                    peer: Arc::new(peer),
                    session: Some(scopeguard::ScopeGuard::into_inner(session)),
                },
                attempt: attempt.clone(),
                storage_scope: format!("{}:{scope}", ticket.node_id()),
                complete,
            };
            self.commands
                .send(command)
                .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?;
            result
                .await
                .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
            Ok::<_, crate::transport::TransportError>(())
        };
        tokio::select! {
            biased;
            _ = attempt.cancelled() => Err(PeerError::ConnectionClosed("connection attempt cancelled".into()).into()),
            result = setup => {
                result?;
                guard.disarm();
                Ok(())
            }
        }
    }
    /// Release the current transport while retaining offline editing and observers.
    pub async fn disconnect(&self) -> Result<(), PeerError> {
        let result = {
            let current = self.connection_attempt.lock().unwrap();
            current.cancel();
            self.request_disconnect()?
        };
        result
            .await
            .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?
    }
    fn request_disconnect(&self) -> Result<oneshot::Receiver<Result<(), PeerError>>, PeerError> {
        let (complete, result) = oneshot::channel();
        self.commands
            .send(Command::Disconnect(complete))
            .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?;
        Ok(result)
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

fn apply(updates: &watch::Sender<Arc<Snapshot>>, event: Event) -> Vec<Scheduled> {
    let mut effects = Vec::new();
    updates.send_if_modified(|current| {
        let (produced, changed) = apply_locked(current, event);
        effects = produced
            .into_iter()
            .map(|effect| Scheduled {
                effect,
                snapshot: current.clone(),
                complete: None,
            })
            .collect();
        changed
    });
    effects
}
fn apply_locked(current: &mut Arc<Snapshot>, event: Event) -> (Vec<Effect>, bool) {
    let (next, effects) = reduce(current, event);
    publish_locked(current, next, effects)
}
fn publish_locked(
    current: &mut Arc<Snapshot>,
    next: Snapshot,
    effects: Vec<Effect>,
) -> (Vec<Effect>, bool) {
    // No `..`: adding a Snapshot field must update the publication contract.
    let Snapshot {
        storage_scope,
        archived_scopes,
        account,
        terminals,
        conversations,
        threads,
        models,
        model_errors,
        requests,
        drafts,
        pending_submissions,
        file_drafts,
        workspace,
        navigation,
        activity,
        management,
        list_query,
        epoch,
        connected,
        subscriptions,
        error,
    } = &next;
    let same_threads = match (&current.threads, threads) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    };
    if current.storage_scope == *storage_scope
        && Arc::ptr_eq(&current.archived_scopes, archived_scopes)
        && Arc::ptr_eq(&current.terminals, terminals)
        && Arc::ptr_eq(&current.subscriptions, subscriptions)
        && Arc::ptr_eq(&current.account, account)
        && Arc::ptr_eq(&current.conversations, conversations)
        && same_threads
        && Arc::ptr_eq(&current.models, models)
        && Arc::ptr_eq(&current.model_errors, model_errors)
        && Arc::ptr_eq(&current.requests, requests)
        && Arc::ptr_eq(&current.drafts, drafts)
        && Arc::ptr_eq(&current.pending_submissions, pending_submissions)
        && Arc::ptr_eq(&current.file_drafts, file_drafts)
        && Arc::ptr_eq(&current.workspace, workspace)
        && Arc::ptr_eq(&current.navigation, navigation)
        && Arc::ptr_eq(&current.activity, activity)
        && Arc::ptr_eq(&current.management, management)
        && Arc::ptr_eq(&current.list_query, list_query)
        && &current.epoch == epoch
        && &current.connected == connected
        && &current.error == error
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
    let mut scheduled = Vec::new();
    let mut result = Ok(Outcome::Applied);
    updates.send_if_modified(|snapshot| {
        // Dispatch and completion share this lock: navigation cannot change
        // between the epoch comparison and publication.
        let current = completed.epoch == snapshot.epoch;
        let mut next = snapshot.as_ref().clone();
        result = match completed.result {
            Ok(applied) => match applied.application.apply(&mut next, current) {
                Ok(next_effects) => {
                    effects = next_effects;
                    Ok(applied.outcome)
                }
                Err(error) => {
                    if current {
                        next.error = Some(error.to_string());
                    }
                    Err(error)
                }
            },
            Err(error) => {
                if let Some(handle) = completed.terminal {
                    next = reduce(
                        &next,
                        Event::TerminalFailed {
                            handle,
                            reason: error.to_string(),
                        },
                    )
                    .0;
                }
                if let Some(id) = completed.failed_submission {
                    let unknown = completed.delivery_attempted
                        && (matches!(
                            &error,
                            PeerError::ConnectionClosed(_)
                                | PeerError::RequestTimeout { .. }
                                | PeerError::InvalidResponse { .. }
                        ) || matches!(
                            &error,
                            PeerError::Remote {
                                delivery: crate::peer::Delivery::Unknown,
                                ..
                            }
                        ));
                    next = reduce(
                        &next,
                        if unknown {
                            Event::SubmissionUnknown(id)
                        } else {
                            Event::SubmissionFailed(id)
                        },
                    )
                    .0;
                }
                if current {
                    next.error = Some(error.to_string());
                }
                Err(error)
            }
        };
        let changed = publish_locked(snapshot, next, Vec::new()).1;
        scheduled = effects
            .drain(..)
            .map(|effect| Scheduled {
                effect,
                snapshot: snapshot.clone(),
                complete: None,
            })
            .collect();
        changed
    });
    let continuation = scheduled
        .iter()
        .position(|scheduled| scheduled.effect.1 || scheduled.effect.0.submission_id().is_some());
    let mut complete = completed.complete;
    if continuation.is_none()
        && let Some(complete) = complete.take()
    {
        complete.send(result);
    }
    if let Some(index) = continuation {
        scheduled[index].complete = complete;
    }
    scheduled
}
/// Bounded, connection-local item work. A continuation keeps its original
/// receipt and slot identity, while releasing the wire response barrier.
#[derive(Default)]
struct ItemReads {
    receipts: BTreeMap<op::ReadItem, Receipt>,
    running: BTreeSet<op::ReadItem>,
    pending: VecDeque<Scheduled>,
}
impl ItemReads {
    fn enqueue(&mut self, mut scheduled: Scheduled) -> Result<(), PeerError> {
        let key = scheduled.effect.0.item_read().unwrap().clone();
        if let Some(receipt) = self.receipts.get(&key) {
            if let Some(complete) = scheduled.complete.take() {
                receipt.join(complete);
            }
            if !scheduled.effect.1 {
                return Ok(());
            }
            scheduled.complete = Some(receipt.clone());
        } else {
            if self.receipts.len() >= 132 {
                let error = PeerError::InvalidMessage(format!(
                    "too many pending item reads: {}",
                    key.item_id
                ));
                if let Some(complete) = scheduled.complete {
                    complete.send(Err(error.clone()));
                }
                return Err(error);
            }
            let receipt = scheduled.complete.get_or_insert_default().clone();
            self.receipts.insert(key, receipt);
        }
        if scheduled.effect.1 {
            // Continue the same item before issuing new grants. Its slot covers
            // the control response, body transfer, and final application.
            self.pending.push_front(scheduled);
        } else {
            self.pending.push_back(scheduled);
        }
        Ok(())
    }
    fn next(&mut self) -> Option<Scheduled> {
        let next = self.pending.front()?;
        if self.running.len() >= 4 && !self.running.contains(next.effect.0.item_read().unwrap()) {
            return None;
        }
        let scheduled = self.pending.pop_front().unwrap();
        self.running
            .insert(scheduled.effect.0.item_read().unwrap().clone());
        Some(scheduled)
    }
    fn finish(
        &mut self,
        ordered: &Mutex<BTreeSet<u64>>,
        updates: &watch::Sender<Arc<Snapshot>>,
        completed: Completed,
    ) -> Vec<Scheduled> {
        let key = completed.item_read.clone();
        let effects = finish(ordered, updates, completed);
        if let Some(key) = key
            && !effects
                .iter()
                .any(|s| s.effect.1 && s.effect.0.item_read() == Some(&key))
        {
            self.running.remove(&key);
            self.receipts.remove(&key);
        }
        effects
    }
}
fn decode_message(line: &str) -> Result<Event, PeerError> {
    let message =
        RpcMessage::parse(line).map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    if message.kind() == RpcMessageKind::Request {
        return Err(PeerError::InvalidMessage(
            "provider requests must arrive as session updates".into(),
        ));
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
    connection: Connection,
    updates: watch::Sender<Arc<Snapshot>>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    stop: CancellationToken,
    mut effects: Vec<Scheduled>,
) -> Result<(), PeerError> {
    let peer = &connection.peer;
    let session = &connection.session;
    let mut events = peer.subscribe();
    let client = Client::new(peer.clone());
    let ordered = Mutex::new(BTreeSet::new());
    let mut jobs = FuturesUnordered::new();
    let mut received = VecDeque::new();
    let mut completed = BTreeMap::new();
    let mut stream_open = true;
    let mut terminal_reason = None;
    let mut terminal_commands = VecDeque::new();
    let mut item_reads = ItemReads::default();
    let mut terminal_running = false;
    let mut disconnected = None;
    let reason = loop {
        for Scheduled {
            effect,
            snapshot: captured,
            complete,
        } in effects.drain(..)
        {
            if effect.0.terminal_handle().is_some() {
                terminal_commands.push_back(Scheduled {
                    effect,
                    snapshot: captured,
                    complete,
                });
                continue;
            }
            if effect.0.item_read().is_some() {
                if let Err(error) = item_reads.enqueue(Scheduled {
                    effect,
                    snapshot: captured,
                    complete,
                }) {
                    apply(&updates, Event::Failed(error.to_string()));
                }
                continue;
            }
            jobs.push(perform(
                Some(&client),
                Some(peer),
                &ordered,
                session.as_ref(),
                captured,
                effect,
                complete,
            ));
        }
        while let Some(scheduled) = item_reads.next() {
            jobs.push(perform(
                Some(&client),
                Some(peer),
                &ordered,
                session.as_ref(),
                scheduled.snapshot,
                scheduled.effect,
                scheduled.complete,
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
                Some(peer),
                &ordered,
                session.as_ref(),
                captured,
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
                    Command::ResumePeer { endpoint, remote, complete } => {
                        let _ = complete.send(session.as_ref().filter(|session| session.uses_endpoint(&endpoint) && session.node_id() == remote).map(|_| peer.clone()));
                        continue;
                    }
                    Command::Disconnect(complete) => {
                        disconnected = Some(complete);
                        break "Host disconnected".into();
                    }
                    Command::Attach { connection, complete, .. } => {
                        drop(connection);
                        let _ = complete.send(Err(PeerError::ConnectionClosed("store is already connected".into())));
                        continue;
                    }
                };
                let mut complete = Some(Receipt::new(command.complete));
                for effect in command.effects {
                    effects.push(Scheduled { effect, snapshot: command.snapshot.clone(), complete: complete.take() });
                }
            }
            result = jobs.next(), if !jobs.is_empty() => {
                let result = result.unwrap();
                if result.terminal.is_some() { terminal_running = false; }
                if let Some(sequence) = completion_sequence(&result) { completed.insert(sequence,result); }
                else { effects.extend(item_reads.finish(&ordered,&updates,result)); }
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
                effects.extend(item_reads.finish(&ordered, &updates, result));
            }
            match received.pop_front().unwrap() {
                PeerEvent::Message(frame) => {
                    let event = decode_message(&frame.value)
                        .unwrap_or_else(|error| Event::Failed(error.to_string()));
                    effects.extend(apply(&updates, event));
                }
                PeerEvent::Closed(reason) => {
                    terminal_reason = Some(reason);
                }
                PeerEvent::Response { .. } => {}
            }
        }
    };
    // The Host owns connection-scoped PTYs and grants, including starts in flight.
    // Replacement must not wait for delivery acknowledgments from the old peer.
    let replacing = disconnected.is_some() && session.is_some();
    if replacing {
        session.as_ref().unwrap().close();
    }
    drop(jobs);
    let closed = peer.close().await;
    let result = if replacing { Ok(()) } else { closed };
    drop(connection);
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
) -> Option<(Connection, Vec<Scheduled>)> {
    let ordered = Mutex::new(BTreeSet::new());
    while !stop.is_cancelled() {
        let command = tokio::select! {
            biased;
            _ = stop.cancelled() => break,
            command = commands.recv() => match command { Some(command) => command, None => break },
        };
        let command = match command {
            Command::Dispatch(command) => command,
            Command::ResumePeer { complete, .. } => {
                let _ = complete.send(None);
                continue;
            }
            Command::Disconnect(complete) => {
                let _ = complete.send(Ok(()));
                continue;
            }
            Command::Attach {
                connection,
                attempt,
                complete,
                storage_scope,
            } => {
                if attempt.is_cancelled() {
                    drop(connection);
                    let _ = complete.send(Err(PeerError::ConnectionClosed(
                        "connection attempt cancelled".into(),
                    )));
                    continue;
                }
                apply(updates, Event::StorageScope(storage_scope));
                let effects = apply(updates, Event::Connected);
                let _ = complete.send(Ok(()));
                return Some((connection, effects));
            }
        };
        let mut complete = Some(Receipt::new(command.complete));
        for effect in command.effects {
            // Disconnect already removed the subscription on the Host.
            if effect.0.disconnected_is_complete() {
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
            complete.send(Ok(Outcome::Applied));
        }
    }
    None
}

async fn perform(
    client: Option<&Client>,
    peer: Option<&RpcPeer>,
    ordered: &Mutex<BTreeSet<u64>>,
    session: Option<&crate::transport::Session>,
    snapshot: Arc<Snapshot>,
    effect: Effect,
    complete: Option<Receipt>,
) -> Completed {
    let item_read = effect.0.item_read().cloned();
    let terminal = effect.0.terminal_handle().map(str::to_owned);
    let failed_submission = effect.0.submission_id().map(str::to_owned);
    let mut request_id = None;
    let result = async {
        let client =
            client.ok_or_else(|| PeerError::ConnectionClosed("Host not connected".into()))?;
        let peer = peer.ok_or_else(|| PeerError::ConnectionClosed("Host not connected".into()))?;
        let mut context = Execution {
            client,
            peer,
            session,
            snapshot: &snapshot,
            ordered,
            request_id: &mut request_id,
            sequence: None,
            ordered_call: false,
        };
        effect.0.run(&mut context).await
    }
    .await;
    Completed {
        item_read,
        delivery_attempted: client.is_some() && peer.is_some(),
        epoch: snapshot.epoch,
        result,
        request_id,
        failed_submission,
        terminal,
        complete,
    }
}

// The typed completion is executable Store state, never part of a replayable Event.
trait Application: Send + std::fmt::Debug {
    fn apply(
        self: Box<Self>,
        snapshot: &mut Snapshot,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError>;
}
#[derive(Debug)]
struct Completion<O: op::Operation> {
    operation: O,
    output: Option<O::Output>,
}
impl<O: op::Operation> Application for Completion<O> {
    fn apply(
        self: Box<Self>,
        snapshot: &mut Snapshot,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        let Self { operation, output } = *self;
        let output = output.expect("only completed operations are published");
        operation.complete(snapshot, output, current)
    }
}
// Intent is replayable data. Only the effect queue erases an operation's type;
// the same allocation carries its output until the response marker is applied.
#[derive(Debug)]
pub struct Effect(Box<dyn Pending>, bool);
impl Effect {
    /// Continue the dispatch receipt after the ordered control step is applied.
    pub(crate) fn continuation<O: op::Operation>(operation: O) -> Self {
        let mut effect = Self::execute(operation);
        effect.1 = true;
        effect
    }
    pub fn execute<O: op::Operation>(operation: O) -> Self {
        Self(
            Box::new(Completion {
                operation,
                output: None,
            }),
            false,
        )
    }
}
trait Pending: Application {
    fn item_read(&self) -> Option<&op::ReadItem>;
    fn submission_id(&self) -> Option<&str>;
    fn terminal_handle(&self) -> Option<&str>;
    fn disconnected_is_complete(&self) -> bool;
    fn run<'a>(
        self: Box<Self>,
        context: &'a mut Execution<'_>,
    ) -> futures_util::future::BoxFuture<'a, Result<Applied, PeerError>>;
}
impl<O: op::Operation> Pending for Completion<O> {
    fn item_read(&self) -> Option<&op::ReadItem> {
        self.operation.item_read()
    }
    fn submission_id(&self) -> Option<&str> {
        self.operation.submission_id()
    }
    fn terminal_handle(&self) -> Option<&str> {
        self.operation.terminal_handle()
    }
    fn disconnected_is_complete(&self) -> bool {
        self.operation.disconnected_is_complete()
    }
    fn run<'a>(
        mut self: Box<Self>,
        context: &'a mut Execution<'_>,
    ) -> futures_util::future::BoxFuture<'a, Result<Applied, PeerError>> {
        Box::pin(async move {
            context.ordered_call = O::ORDERED;
            let mut output = self.operation.run(context).await?;
            let outcome = O::outcome(&mut output);
            self.output = Some(output);
            Ok(Applied {
                sequence: context.sequence,
                outcome,
                application: self,
            })
        })
    }
}
pub struct Execution<'a> {
    pub(crate) client: &'a Client,
    pub(crate) peer: &'a RpcPeer,
    pub(crate) session: Option<&'a crate::transport::Session>,
    pub(crate) snapshot: &'a Snapshot,
    ordered: &'a Mutex<BTreeSet<u64>>,
    request_id: &'a mut Option<u64>,
    sequence: Option<u64>,
    ordered_call: bool,
}
impl Execution<'_> {
    pub(crate) async fn call<O: RpcMethod + Sync>(
        &mut self,
        operation: &O,
    ) -> Result<O::Output, PeerError> {
        let request = self.client.call(operation);
        if self.ordered_call {
            // Register before polling: a fast reply cannot overtake publication.
            *self.request_id = request.wire_id();
            if let Some(id) = *self.request_id {
                self.ordered.lock().unwrap().insert(id);
            }
        }
        let reply = request.await?;
        if self.ordered_call {
            self.sequence = Some(reply.sequence);
        }
        Ok(reply.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_snapshot_field_notifies_subscribers_independently() {
        type Change = fn(&mut Snapshot);
        let changes: &[(&str, Change)] = &[
            ("account", |snapshot| snapshot.account = Arc::default()),
            ("terminals", |snapshot| snapshot.terminals = Arc::default()),
            ("conversations", |snapshot| {
                snapshot.conversations = Arc::default()
            }),
            ("models", |snapshot| snapshot.models = Arc::default()),
            ("requests", |snapshot| snapshot.requests = Arc::default()),
            ("drafts", |snapshot| snapshot.drafts = Arc::default()),
            ("pending_submissions", |snapshot| {
                snapshot.pending_submissions = Arc::default()
            }),
            ("file_drafts", |snapshot| {
                snapshot.file_drafts = Arc::default()
            }),
            ("workspace", |snapshot| snapshot.workspace = Arc::default()),
            ("navigation", |snapshot| {
                snapshot.navigation = Arc::default()
            }),
            ("activity", |snapshot| snapshot.activity = Arc::default()),
            ("management", |snapshot| {
                snapshot.management = Arc::default()
            }),
            ("list_query", |snapshot| {
                snapshot.list_query = Arc::default()
            }),
            ("threads", |snapshot| {
                snapshot.threads = Some(Arc::new(crate::models::ThreadList {
                    data: Vec::new(),
                    projects: Vec::new(),
                    more_project_ids: Vec::new(),
                    has_more_chats: false,
                    has_more_projects: false,
                    extra: Default::default(),
                }))
            }),
            ("epoch", |snapshot| snapshot.epoch += 1),
            ("connected", |snapshot| snapshot.connected = true),
            ("error", |snapshot| {
                snapshot.error = Some("fixture failure".into())
            }),
        ];
        for (name, change) in changes {
            let previous = Arc::new(Snapshot::default());
            let (writer, reader) = watch::channel(previous.clone());
            writer.send_if_modified(|current| {
                publish_locked(current, previous.as_ref().clone(), Vec::new()).1
            });
            assert!(
                !reader.has_changed().unwrap(),
                "unchanged snapshot published"
            );
            let mut next = previous.as_ref().clone();
            change(&mut next);
            writer.send_if_modified(|current| publish_locked(current, next, Vec::new()).1);
            assert!(reader.has_changed().unwrap(), "{name} update was dropped");
        }
    }
}

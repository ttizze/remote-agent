//! The single state owner. Independent RPC work publishes completed results.
use crate::{
    client::*,
    peer::PeerError,
    state::{Event, Intent, Snapshot, operations as op, reduce},
};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};
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
    Disconnect(oneshot::Sender<Result<(), PeerError>>),
    Attach {
        connection: Box<Connection>,
        attempt: CancellationToken,
        storage_scope: String,
        complete: oneshot::Sender<Result<(), PeerError>>,
    },
}
struct Connection {
    peer: Arc<Client>,
    events: Updates,
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
    subscriptions: Vec<(uuid::Uuid, crate::client::Updates)>,
    item_read: Option<op::ReadItem>,
    delivery_attempted: bool,
    epoch: u64,
    result: Result<Applied, PeerError>,
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
    finished: watch::Receiver<bool>,
}
impl Store {
    pub fn new(peer: (Client, Updates), snapshot: Snapshot) -> Self {
        Self::start(Some(peer), snapshot)
    }
    pub fn offline(snapshot: Snapshot) -> Self {
        let (snapshot, _) = reduce(&snapshot, Event::Disconnected("Host not connected".into()));
        Self::start(None, snapshot)
    }
    fn start(peer: Option<(Client, Updates)>, snapshot: Snapshot) -> Self {
        let (writer, updates) = watch::channel(Arc::new(snapshot));
        let connection = peer.map(|(peer, events)| {
            (
                Connection {
                    peer: Arc::new(peer),
                    events,
                    session: None,
                },
                apply(&writer, Event::Connected),
            )
        });
        let (commands, mut incoming) = mpsc::unbounded_channel();
        let stop = CancellationToken::new();
        let (finished_tx, finished) = watch::channel(false);
        let publications = writer.clone();
        let shutdown = stop.clone();
        tokio::spawn(async move {
            let mut connection = connection;
            loop {
                if let Some((connection, effects)) = connection.take() {
                    run(
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
            finished_tx.send_replace(true);
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
        let guard = attempt.clone().drop_guard();
        let setup = async {
            disconnected
                .await
                .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
            // Do not probe a suspended connection: its RPC deadline would delay
            // foreground recovery. The actor releases it before setup begins.
            let session = scopeguard::guard(endpoint.connect(ticket).await?, |session| {
                session.close();
            });
            let (peer, events) = session
                .open_peer(std::time::Duration::from_secs(30), 64)
                .await?;
            if let Some(invitation) = invitation {
                peer.call(&Pair { invitation }).await?;
            }
            let scope = peer
                .request::<String>(&crate::protocol::Call::SessionScope(
                    crate::models::Empty {},
                ))
                .await?;
            if scope.is_empty() || scope.len() > 256 {
                return Err(
                    PeerError::InvalidMessage("invalid provider storage scope".into()).into(),
                );
            }
            let (complete, result) = oneshot::channel();
            let command = Command::Attach {
                connection: Box::new(Connection {
                    peer: Arc::new(peer),
                    events,
                    session: Some(scopeguard::ScopeGuard::into_inner(session)),
                }),
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
        finished
            .wait_for(|done| *done)
            .await
            .map(|_| ())
            .map_err(|_| PeerError::ConnectionClosed("store task stopped".into()))
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
fn finish(updates: &watch::Sender<Arc<Snapshot>>, completed: Completed) -> Vec<Scheduled> {
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
/// receipt and slot identity, without blocking unrelated work.
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
        updates: &watch::Sender<Arc<Snapshot>>,
        completed: Completed,
    ) -> Vec<Scheduled> {
        let key = completed.item_read.clone();
        let effects = finish(updates, completed);
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
async fn run(
    mut connection: Connection,
    updates: watch::Sender<Arc<Snapshot>>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    stop: CancellationToken,
    mut effects: Vec<Scheduled>,
) {
    let peer = &connection.peer;
    let session = &connection.session;
    let events = &mut connection.events;
    let mut jobs = FuturesUnordered::new();
    let mut subscriptions = tokio_stream::StreamMap::new();
    let mut terminal_commands = VecDeque::new();
    let mut item_reads = ItemReads::default();
    let mut terminal_running = false;
    let mut disconnected = None;
    let reason = loop {
        let unused: Vec<_> = subscriptions
            .keys()
            .filter(|id| {
                !updates
                    .borrow()
                    .subscriptions
                    .values()
                    .any(|current| current == *id)
            })
            .copied()
            .collect();
        for id in unused {
            subscriptions.remove(&id);
        }
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
                Some(peer),
                session.as_ref(),
                captured,
                effect,
                complete,
            ));
        }
        while let Some(scheduled) = item_reads.next() {
            jobs.push(perform(
                Some(peer),
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
                Some(peer),
                session.as_ref(),
                captured,
                effect,
                complete,
            ));
        }
        tokio::select! {
            biased;
            _ = stop.cancelled() => break "store closed".into(),
            command = commands.recv() => {
                let Some(command) = command else { break "store closed".into() };
                let command = match command {
                    Command::Dispatch(command) => command,
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
                let mut result = result.unwrap();
                for (id, stream) in result.subscriptions.drain(..) { subscriptions.insert(id, futures_util::stream::try_unfold(stream, |mut stream| async {
                        Ok(stream.read::<crate::session::SessionChange>().await?.map(|line| (line, stream)))
                    }).chain(futures_util::stream::once(async {
                    Err(std::io::Error::other("subscription ended"))
                })).boxed()); }
                if result.terminal.is_some() { terminal_running = false; }
                effects.extend(item_reads.finish(&updates, result));
            }
            Some((id, update)) = subscriptions.next(), if !subscriptions.is_empty() => {
                match update {
                    Ok(change) => effects.extend(apply(&updates, Event::SessionUpdate(Box::new(crate::session::SessionUpdate { subscription_id: id, change })))),
                    Err(error) => break error.to_string(),
                }
            }
            event = events.read::<crate::protocol::Notification>() => match event {
                Ok(Some(notification)) => effects.extend(apply(&updates, Event::Notification(notification))),
                Ok(None) => break "Host event stream ended".into(),
                Err(error) => break error.to_string(),
            }
        }
    };
    // The Host owns PTY cleanup, including starts still in flight. Closing
    // the connection cancels them without waiting for individual RPC replies.
    drop(jobs);
    peer.close().await;
    drop(connection);
    apply(&updates, Event::Disconnected(reason));
    if let Some(complete) = disconnected {
        let _ = complete.send(Ok(()));
    }
}

async fn run_offline(
    updates: &watch::Sender<Arc<Snapshot>>,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    stop: &CancellationToken,
) -> Option<(Connection, Vec<Scheduled>)> {
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
                return Some((*connection, effects));
            }
        };
        let mut complete = Some(Receipt::new(command.complete));
        for effect in command.effects {
            let result = perform(
                None,
                None,
                command.snapshot.clone(),
                effect,
                complete.take(),
            )
            .await;
            drop(finish(updates, result));
        }
        if let Some(complete) = complete {
            complete.send(Ok(Outcome::Applied));
        }
    }
    None
}

async fn perform(
    client: Option<&Client>,
    session: Option<&crate::transport::Session>,
    snapshot: Arc<Snapshot>,
    effect: Effect,
    complete: Option<Receipt>,
) -> Completed {
    let item_read = effect.0.item_read().cloned();
    let terminal = effect.0.terminal_handle().map(str::to_owned);
    let failed_submission = effect.0.submission_id().map(str::to_owned);
    let mut subscriptions = Vec::new();
    let result = async {
        let client =
            client.ok_or_else(|| PeerError::ConnectionClosed("Host not connected".into()))?;
        let mut context = Execution {
            client,
            session,
            snapshot: &snapshot,
            subscriptions: &mut subscriptions,
        };
        effect.0.run(&mut context).await
    }
    .await;
    Completed {
        subscriptions,
        item_read,
        delivery_attempted: client.is_some(),
        epoch: snapshot.epoch,
        result,
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
// the same allocation carries its output until its result is applied.
#[derive(Debug)]
pub struct Effect(Box<dyn Pending>, bool);
impl Effect {
    /// Continue the dispatch receipt after this step is applied.
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
    fn run<'a>(
        mut self: Box<Self>,
        context: &'a mut Execution<'_>,
    ) -> futures_util::future::BoxFuture<'a, Result<Applied, PeerError>> {
        Box::pin(async move {
            let mut output = self.operation.run(context).await?;
            let outcome = O::outcome(&mut output);
            self.output = Some(output);
            Ok(Applied {
                outcome,
                application: self,
            })
        })
    }
}
pub struct Execution<'a> {
    pub(crate) client: &'a Client,
    pub(crate) session: Option<&'a crate::transport::Session>,
    pub(crate) snapshot: &'a Snapshot,
    subscriptions: &'a mut Vec<(uuid::Uuid, crate::client::Updates)>,
}
impl Execution<'_> {
    pub(crate) async fn call<O: RpcMethod + Sync>(
        &mut self,
        operation: &O,
    ) -> Result<O::Output, PeerError> {
        if let Some((output, stream, id)) = self.client.open_subscription(operation).await? {
            self.subscriptions.push((id, stream));
            Ok(output)
        } else {
            Ok(self.client.call(operation).await?)
        }
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

                    provider_errors: None,
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

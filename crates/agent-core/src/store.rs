//! The single state owner. Independent RPC work publishes completed results.
use crate::{
    client::{ClientExt, SessionImage},
    diagnostics::{ConnectionPerformance, ConnectionPhase as Phase},
    peer::PeerError,
    state::{Event, Intent, Snapshot, operations as op, reduce},
};
use agent_protocol::operations::{Pair, RpcMethod};
use agent_transport::client::{Client, Updates};
use futures_util::{StreamExt, stream::FuturesUnordered};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::{CancellationToken, DropGuard};

const MAX_COMMANDS: usize = 256;
const MAX_RPC_JOBS: usize = 32;
const CONTROL_RESERVE: usize = 16;
const MAX_TERMINAL_JOBS: usize = 16;
const MAX_TERMINAL_QUEUE: usize = 128;
const MAX_ITEM_READS: usize = 132;
const MAX_ITEM_TRANSFERS: usize = 4;
const MAX_WAITERS: usize = 128;

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Outcome {
    LiveActivityRegistered {
        enabled: bool,
    },
    #[default]
    Applied,
    StartedThread {
        id: crate::session::SessionRef,
    },
    Submitted {
        turn_id: Option<agent_protocol::ids::TurnId>,
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
    PrepareDictation {
        id: String,
        cancel: CancellationToken,
    },
    ConnectionPerformance {
        epoch: u64,
        performance: ConnectionPerformance,
    },
    Browser {
        request: crate::browser::BrowserRequest,
        complete: oneshot::Sender<Result<crate::browser::BrowserFrame, PeerError>>,
    },
    Dispatch(Dispatch),
    ResumePeer {
        endpoint: crate::transport::Endpoint,
        remote: crate::transport::NodeId,
        complete: oneshot::Sender<Option<Arc<Client>>>,
    },
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
impl Connection {
    async fn open(
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        invitation: Option<uuid::Uuid>,
    ) -> Result<(Self, String, ConnectionPerformance), crate::transport::TransportError> {
        let (session, peer, events, scope, transport_ms, verification_ms) =
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let started = std::time::Instant::now();
                let session =
                    scopeguard::guard(endpoint.connect(ticket).await?, |session| session.close());
                let transport_ms = started.elapsed().as_millis() as u64;
                let started = std::time::Instant::now();
                let (peer, events) = session
                    .open_peer(std::time::Duration::from_secs(30), 64)
                    .await?;
                if let Some(invitation) = invitation {
                    peer.call(&Pair { invitation }).await?;
                }
                let scope = read_storage_scope(&peer).await?;
                Ok::<_, crate::transport::TransportError>((
                    scopeguard::ScopeGuard::into_inner(session),
                    peer,
                    events,
                    scope,
                    transport_ms,
                    started.elapsed().as_millis() as u64,
                ))
            })
            .await
            .map_err(|_| PeerError::RequestTimeout {
                method: "host/connect".into(),
            })??;
        let (route, rtt_ms) = peer.connection_path();
        let performance = ConnectionPerformance {
            transport_ms,
            verification_ms,
            route,
            rtt_ms,
            resolution_ms: session.resolution_ms(),
            connection_id: peer.diagnostic_id,
            ..Default::default()
        };
        Ok((
            Self {
                peer: Arc::new(peer),
                events,
                session: Some(session),
            },
            format!("{}:{scope}", ticket.node_id()),
            performance,
        ))
    }
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
    effects: Vec<Scheduled>,
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
            let remaining = MAX_WAITERS.saturating_sub(waiters.len());
            for (index, sender) in other.0.lock().unwrap().drain(..).enumerate() {
                if index < remaining {
                    waiters.push(sender);
                } else {
                    let _ = sender.send(Err(PeerError::InvalidMessage(
                        "too many waiters for operation".into(),
                    )));
                }
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
    subscriptions: Vec<(uuid::Uuid, agent_transport::client::Updates)>,
    scheduling: op::Scheduling,
    delivery_attempted: bool,
    scope: Scope,
    result: Result<Applied, PeerError>,
    rejection: Option<Box<dyn Application>>,
    failed_submission: Option<agent_protocol::ids::ClientInputId>,
    complete: Option<Receipt>,
}
struct Scheduled {
    effect: Effect,
    scope: Scope,
    complete: Option<Receipt>,
}
struct Scope {
    navigation: u64,
    operation: Option<(op::OperationKey, u64)>,
}
fn generation_current(
    operation: Option<&(op::OperationKey, u64)>,
    operations: &BTreeMap<op::OperationKey, op::OperationState>,
) -> bool {
    operation.is_none_or(|(key, generation)| {
        operations
            .get(key)
            .is_some_and(|state| state.generation == *generation)
    })
}
impl Scope {
    fn current(
        &self,
        navigation: u64,
        operations: &BTreeMap<op::OperationKey, op::OperationState>,
    ) -> bool {
        self.navigation == navigation && generation_current(self.operation.as_ref(), operations)
    }
    fn finish(
        &self,
        operations: &mut Arc<BTreeMap<op::OperationKey, op::OperationState>>,
        error: Option<&PeerError>,
    ) {
        if let Some((key, generation)) = &self.operation
            && operations
                .get(key)
                .is_some_and(|state| state.generation == *generation)
        {
            match error {
                None => {
                    Arc::make_mut(operations).remove(key);
                }
                Some(error) => {
                    Arc::make_mut(operations).get_mut(key).unwrap().phase =
                        op::OperationPhase::Failed {
                            message: error.to_string(),
                        };
                }
            }
        }
    }
}
impl Scheduled {
    fn new(mut effect: Effect, snapshot: &mut Snapshot, continuation: Option<&Scope>) -> Self {
        let operation = effect.operation.key().map(|key| {
            let generation = if (matches!(key, op::OperationKey::Item { .. })
                || continuation
                    .and_then(|scope| scope.operation.as_ref())
                    .is_some_and(|(previous, generation)| {
                        previous == &key
                            && snapshot
                                .operations
                                .get(&key)
                                .is_some_and(|state| state.generation == *generation)
                    }))
                && let Some(state) = snapshot.operations.get(&key)
                && state.phase == op::OperationPhase::Running
            {
                state.generation
            } else {
                snapshot.operation_sequence += 1;
                snapshot.operation_sequence
            };
            let state = op::OperationState {
                generation,
                phase: op::OperationPhase::Running,
            };
            if snapshot.operations.get(&key) != Some(&state) {
                Arc::make_mut(&mut snapshot.operations).insert(key.clone(), state);
            }
            (key, generation)
        });
        effect.operation.capture(snapshot);
        Self {
            effect,
            scope: Scope {
                navigation: snapshot.epoch,
                operation,
            },
            complete: None,
        }
    }
}
pub struct Store {
    updates: watch::Receiver<Arc<Snapshot>>,
    publications: Mutex<Option<watch::Sender<Arc<Snapshot>>>>,
    commands: mpsc::Sender<Command>,
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
        let (commands, mut incoming) = mpsc::channel(MAX_COMMANDS);
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
    #[cfg(all(test, feature = "bindings"))]
    pub(crate) fn mock_connection() -> (Self, impl Future<Output = ConnectionPerformance>) {
        let mut store = Self::start(
            None,
            Snapshot {
                connected: true,
                ..Default::default()
            },
        );
        let (commands, mut reports) = mpsc::channel(MAX_COMMANDS);
        let worker = std::mem::replace(&mut store.commands, commands);
        (store, async move {
            // Keep the actual Store worker alive for shutdown, while the mock
            // connection leaves its diagnostic command queue unread.
            let _worker = worker;
            match reports.recv().await.expect("missing diagnostic report") {
                Command::ConnectionPerformance { performance, .. } => performance,
                _ => panic!("unexpected connection command"),
            }
        })
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
    ) -> Result<ConnectionPerformance, crate::transport::TransportError> {
        let attempt = {
            let mut current = self.connection_attempt.lock().unwrap();
            current.cancel();
            *current = self.stop.child_token();
            current.clone()
        };
        let guard = attempt.clone().drop_guard();
        let setup = async {
            endpoint.network_change().await;
            let (complete, receiver) = oneshot::channel();
            self.commands
                .try_send(Command::ResumePeer {
                    endpoint: endpoint.clone(),
                    remote: ticket.node_id(),
                    complete,
                })
                .map_err(command_error)?;
            let reusable = receiver.await.unwrap_or(None);
            let replacement = async {
                let (connection, scope, performance) =
                    Connection::open(endpoint, ticket, None).await?;
                Ok::<_, crate::transport::TransportError>((Some((connection, scope)), performance))
            };
            let (prepared, performance) = if let Some(peer) = reusable {
                // Recovery and the liveness read start together. A dead old path
                // cannot add its probe deadline to a working replacement's latency.
                let scope = self.snapshot().storage_scope.clone();
                let probe = async {
                    let started = std::time::Instant::now();
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        read_storage_scope(&peer),
                    )
                    .await
                    {
                        Ok(Ok(current)) => Ok((scope == format!("{}:{current}", ticket.node_id()))
                            .then(|| {
                                let (route, rtt_ms) = peer.connection_path();
                                ConnectionPerformance {
                                    reused: true,
                                    connection_id: peer.diagnostic_id,
                                    verification_ms: started.elapsed().as_millis() as u64,
                                    route,
                                    rtt_ms,
                                    ..Default::default()
                                }
                            })),
                        Ok(Err(
                            error @ (PeerError::Remote { .. } | PeerError::InvalidResponse { .. }),
                        )) => Err(error),
                        _ => Ok(None),
                    }
                };
                tokio::pin!(probe, replacement);
                tokio::select! {
                    biased;
                    responsive = &mut probe => {
                        if let Some(performance) = responsive? { (None, performance) } else { replacement.await? }
                    }
                    candidate = &mut replacement => match candidate {
                        Ok(candidate) => candidate,
                        // A new connection can fail while the current one remains
                        // healthy. Never discard that working session on this error.
                        Err(error) => {
                            if let Some(performance) = probe.await? { (None, performance) } else { return Err(error); }
                        }
                    }
                }
            } else {
                replacement.await?
            };
            let Some((connection, storage_scope)) = prepared else {
                let snapshot = self.snapshot();
                if let Some(id) = &snapshot.navigation.thread_id
                    && !snapshot.subscriptions.contains_key(id)
                {
                    drop(self.dispatch(Intent::ReadThread(op::ReadThread::open(id.clone()))));
                }
                drop(self.dispatch(Intent::ReadTaskActivity(op::ReadTaskActivity {})));
                return Ok(performance);
            };
            let disconnected = {
                // Only a fully authorized replacement may retire the old session.
                // Cancellation and the disconnect enqueue share the attempt lock.
                let _current = self.connection_attempt.lock().unwrap();
                if attempt.is_cancelled() {
                    return Err(
                        PeerError::ConnectionClosed("connection attempt cancelled".into()).into(),
                    );
                }
                self.request_disconnect()?
            };
            disconnected
                .await
                .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
            self.attach_connection(connection, storage_scope, attempt.clone())
                .await?;
            Ok(performance)
        };
        tokio::select! {
            biased;
            _ = attempt.cancelled() => Err(PeerError::ConnectionClosed("connection attempt cancelled".into()).into()),
            result = setup => {
                let performance = result?;
                guard.disarm();
                Ok(performance)
            }
        }
    }
    /// Replace transport while retaining local edits. A newer reconnect or
    /// disconnect cancels setup before it can attach an obsolete connection.
    pub async fn reconnect(
        &self,
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        invitation: Option<uuid::Uuid>,
    ) -> Result<ConnectionPerformance, crate::transport::TransportError> {
        let (attempt, disconnected) = {
            let mut current = self.connection_attempt.lock().unwrap();
            current.cancel();
            *current = self.stop.child_token();
            // Explicit replacement releases the old connection before pairing.
            (current.clone(), self.request_disconnect()?)
        };
        let guard = attempt.clone().drop_guard();
        let setup = async {
            disconnected
                .await
                .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
            let (connection, scope, performance) =
                Connection::open(endpoint, ticket, invitation).await?;
            self.attach_connection(connection, scope, attempt.clone())
                .await?;
            Ok::<_, crate::transport::TransportError>(performance)
        };
        tokio::select! {
            biased;
            _ = attempt.cancelled() => Err(PeerError::ConnectionClosed("connection attempt cancelled".into()).into()),
            result = setup => {
                let performance = result?;
                guard.disarm();
                Ok(performance)
            }
        }
    }
    async fn attach_connection(
        &self,
        connection: Connection,
        storage_scope: String,
        attempt: CancellationToken,
    ) -> Result<(), crate::transport::TransportError> {
        let trace = connection.peer.trace.clone();
        let group = connection.peer.diagnostic_id;
        trace.record(Phase::AttachStart, group, 0, 0);
        let (complete, result) = oneshot::channel();
        self.commands
            .try_send(Command::Attach {
                connection: Box::new(connection),
                attempt,
                storage_scope,
                complete,
            })
            .map_err(command_error)?;
        result
            .await
            .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
        trace.record(Phase::AttachReady, group, 0, 1);
        Ok(())
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
            .try_send(Command::Disconnect(complete))
            .map_err(command_error)?;
        Ok(result)
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.updates.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.updates.clone()
    }
    pub fn prepare_dictation(&self) -> crate::client::DictationPreparation {
        let id = uuid::Uuid::new_v4().to_string();
        let cancel = self.stop.child_token();
        let preparation = crate::client::DictationPreparation {
            id: id.clone(),
            _cancel: cancel.clone().drop_guard(),
        };
        let _ = self
            .commands
            .try_send(Command::PrepareDictation { id, cancel });
        preparation
    }
    /// Best-effort diagnostics use the owned connection without delaying recovery.
    pub fn record_connection_performance(&self, performance: ConnectionPerformance) {
        let _ = self.commands.try_send(Command::ConnectionPerformance {
            epoch: self.snapshot().epoch,
            performance,
        });
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
                let mut candidate = current.clone();
                let (effects, changed) = apply_locked(&mut candidate, Event::Intent(intent));
                if !effects.is_empty() {
                    let permit = match self.commands.try_reserve() {
                        Ok(permit) => permit,
                        Err(error) => {
                            receipt = Err(command_error(error));
                            return false;
                        }
                    };
                    let (complete, result) = oneshot::channel();
                    permit.send(Command::Dispatch(Dispatch { effects, complete }));
                    receipt = Ok(Some(result));
                }
                *current = candidate;
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
    /// Ephemeral frames never enter persisted conversation snapshots.
    pub async fn browser(
        &self,
        request: crate::browser::BrowserRequest,
    ) -> Result<crate::browser::BrowserFrame, PeerError> {
        request.validate().map_err(PeerError::InvalidMessage)?;
        let (complete, result) = oneshot::channel();
        self.commands
            .try_send(Command::Browser { request, complete })
            .map_err(command_error)?;
        result
            .await
            .map_err(|_| PeerError::ConnectionClosed("browser connection ended".into()))?
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

async fn read_storage_scope(peer: &Client) -> Result<String, PeerError> {
    let scope = peer
        .request::<String>(&crate::protocol::Call::SessionScope(
            crate::models::Empty {},
        ))
        .await?;
    if scope.is_empty() || scope.len() > 256 {
        return Err(PeerError::InvalidMessage(
            "invalid provider storage scope".into(),
        ));
    }
    Ok(scope)
}

fn apply(updates: &watch::Sender<Arc<Snapshot>>, event: Event) -> Vec<Scheduled> {
    let mut effects = Vec::new();
    updates.send_if_modified(|current| {
        let (produced, changed) = apply_locked(current, event);
        effects = produced;
        changed
    });
    effects
}
fn apply_locked(current: &mut Arc<Snapshot>, event: Event) -> (Vec<Scheduled>, bool) {
    let (mut next, effects) = reduce(current, event);
    if next.epoch != current.epoch {
        Arc::make_mut(&mut next.operations).retain(|key, _| {
            !matches!(
                key,
                op::OperationKey::Directory
                    | op::OperationKey::File
                    | op::OperationKey::WorkspaceReview
                    | op::OperationKey::WorktreeSettings
                    | op::OperationKey::Worktrees
                    | op::OperationKey::Permissions { .. }
            )
        });
    }
    let scheduled = effects
        .into_iter()
        .map(|effect| Scheduled::new(effect, &mut next, None))
        .collect();
    let changed = publish_locked(current, next);
    (scheduled, changed)
}
fn publish_locked(current: &mut Arc<Snapshot>, next: Snapshot) -> bool {
    // No `..`: adding a Snapshot field must update the publication contract.
    let Snapshot {
        operations,
        operation_sequence,
        model_defaults,
        scoped_model_defaults,
        permission_settings,
        composer_catalog,
        host_name,
        task_activity,
        storage_scope,
        archived_scopes,
        account,
        terminals,
        conversations,
        threads,
        expanded_projects,
        project_threads,
        observed_agents,
        models,
        model_errors,
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
    if Arc::ptr_eq(&current.operations, operations)
        && current.operation_sequence == *operation_sequence
        && current.model_defaults == *model_defaults
        && Arc::ptr_eq(&current.scoped_model_defaults, scoped_model_defaults)
        && current.host_name == *host_name
        && current.task_activity == *task_activity
        && current.permission_settings == *permission_settings
        && current.composer_catalog == *composer_catalog
        && current.storage_scope == *storage_scope
        && Arc::ptr_eq(&current.archived_scopes, archived_scopes)
        && Arc::ptr_eq(&current.terminals, terminals)
        && Arc::ptr_eq(&current.subscriptions, subscriptions)
        && Arc::ptr_eq(&current.account, account)
        && Arc::ptr_eq(&current.conversations, conversations)
        && same_threads
        && Arc::ptr_eq(&current.expanded_projects, expanded_projects)
        && Arc::ptr_eq(&current.project_threads, project_threads)
        && current.observed_agents == *observed_agents
        && Arc::ptr_eq(&current.models, models)
        && Arc::ptr_eq(&current.model_errors, model_errors)
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
        return false;
    }
    *current = Arc::new(next);
    true
}
fn finish(updates: &watch::Sender<Arc<Snapshot>>, completed: Completed) -> Vec<Scheduled> {
    let mut effects = Vec::new();
    let mut scheduled = Vec::new();
    let mut result = Ok(Outcome::Applied);
    updates.send_if_modified(|snapshot| {
        // Dispatch and completion share this lock. List results and failures
        // belong to their query; view work belongs to the navigation epoch.
        let current = match &completed.scheduling {
            op::Scheduling::LatestList(_)
            | op::Scheduling::LatestProject(_)
            | op::Scheduling::LatestTaskActivity => {
                generation_current(completed.scope.operation.as_ref(), &snapshot.operations)
            }
            _ => completed
                .scope
                .current(snapshot.epoch, &snapshot.operations),
        };
        let global_error = current
            && !matches!(
                &completed.scheduling,
                op::Scheduling::LatestProject(_) | op::Scheduling::LatestTaskActivity
            );
        let mut next = snapshot.as_ref().clone();
        result = match completed.result {
            Ok(applied) => match applied.application.apply(&mut next, current) {
                Ok(next_effects) => {
                    effects = next_effects;
                    Ok(applied.outcome)
                }
                Err(error) => {
                    if global_error {
                        next.error = Some(error.to_string());
                    }
                    Err(error)
                }
            },
            Err(error) => {
                let error = match completed.rejection {
                    Some(application) => match application.apply(&mut next, current) {
                        Ok(next_effects) => {
                            effects.extend(next_effects);
                            error
                        }
                        Err(error) => error,
                    },
                    None => error,
                };
                if (completed.delivery_attempted
                    || matches!(
                        completed.scheduling,
                        op::Scheduling::Terminal { starts: true, .. }
                    ))
                    && let Some(handle) = completed.scheduling.terminal()
                {
                    next = reduce(
                        &next,
                        Event::TerminalFailed {
                            handle: handle.to_owned(),
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
                if global_error {
                    next.error = Some(error.to_string());
                }
                Err(error)
            }
        };
        effects.extend(op::prefetch_composer_catalog(&mut next));
        let continues = completed.scope.operation.as_ref().is_some_and(|(key, _)| {
            effects
                .iter()
                .any(|effect| effect.operation.key().as_ref() == Some(key))
        });
        if !continues {
            completed
                .scope
                .finish(&mut next.operations, result.as_ref().err());
        }
        let superseded = completed
            .scope
            .operation
            .as_ref()
            .is_some_and(|(key, generation)| {
                next.operations
                    .get(key)
                    .is_some_and(|state| state.generation != *generation)
            });
        scheduled = effects
            .drain(..)
            .filter(|effect| {
                !superseded
                    || effect.operation.key().as_ref()
                        != completed.scope.operation.as_ref().map(|(key, _)| key)
            })
            .map(|effect| Scheduled::new(effect, &mut next, Some(&completed.scope)))
            .collect();
        publish_locked(snapshot, next)
    });
    let continuation = scheduled.iter().position(|scheduled| {
        scheduled.effect.receipt == ReceiptPolicy::Continue
            || scheduled.effect.operation.submission_id().is_some()
    });
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
    fn enqueue(&mut self, mut scheduled: Scheduled) -> Result<(), Box<(Scheduled, PeerError)>> {
        let key = scheduled.effect.scheduling.item().unwrap().clone();
        if let Some(receipt) = self.receipts.get(&key) {
            if let Some(complete) = scheduled.complete.take() {
                receipt.join(complete);
            }
            if scheduled.effect.receipt != ReceiptPolicy::Continue {
                return Ok(());
            }
            scheduled.complete = Some(receipt.clone());
        } else {
            if self.receipts.len() >= MAX_ITEM_READS {
                let error = PeerError::InvalidMessage(format!(
                    "too many pending item reads: {}",
                    key.item_id
                ));
                return Err(Box::new((scheduled, error)));
            }
            let receipt = scheduled.complete.get_or_insert_default().clone();
            self.receipts.insert(key, receipt);
        }
        if scheduled.effect.receipt == ReceiptPolicy::Continue {
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
        if self.running.len() >= MAX_ITEM_TRANSFERS
            && !self
                .running
                .contains(next.effect.scheduling.item().unwrap())
        {
            return None;
        }
        let scheduled = self.pending.pop_front().unwrap();
        self.running
            .insert(scheduled.effect.scheduling.item().unwrap().clone());
        Some(scheduled)
    }
    fn finish(
        &mut self,
        updates: &watch::Sender<Arc<Snapshot>>,
        completed: Completed,
    ) -> Vec<Scheduled> {
        let key = completed.scheduling.item().cloned();
        let effects = finish(updates, completed);
        if let Some(key) = key
            && !effects.iter().any(|s| {
                s.effect.receipt == ReceiptPolicy::Continue
                    && s.effect.scheduling.item() == Some(&key)
            })
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
    commands: &mut mpsc::Receiver<Command>,
    stop: CancellationToken,
    mut effects: Vec<Scheduled>,
) {
    let peer = &connection.peer;
    let session = &connection.session;
    let events = &mut connection.events;
    let mut jobs = FuturesUnordered::new();
    let mut browser_jobs = FuturesUnordered::new();
    let mut diagnostic_jobs = FuturesUnordered::new();
    let mut preparation_jobs = FuturesUnordered::new();
    let mut subscriptions = tokio_stream::StreamMap::new();
    let mut terminal_commands = VecDeque::new();
    let mut item_reads = ItemReads::default();
    let mut terminal_running = BTreeSet::new();
    let mut latest_reads: BTreeMap<op::OperationKey, Option<Scheduled>> = BTreeMap::new();
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
        for mut scheduled in std::mem::take(&mut effects) {
            let unobserved = {
                let snapshot = updates.borrow();
                // Dispatch publishes before enqueueing. Newer notifications can
                // reach the executor before an older command; never let that
                // older work replace the latest pending read.
                (scheduled.effect.scheduling.latest_key().is_some()
                    && !generation_current(
                        scheduled.scope.operation.as_ref(),
                        &snapshot.operations,
                    ))
                    || matches!(&scheduled.effect.scheduling, op::Scheduling::LatestAgents(session)
                        if snapshot.observed_agents.as_ref() != Some(session))
            };
            if unobserved {
                updates.send_if_modified(|snapshot| {
                    let mut next = snapshot.as_ref().clone();
                    scheduled.scope.finish(&mut next.operations, None);
                    publish_locked(snapshot, next)
                });
                if let Some(complete) = scheduled.complete {
                    complete.send(Ok(Outcome::Applied));
                }
                continue;
            }
            if let Some(key) = scheduled.effect.scheduling.latest_key()
                && let Some(pending) = latest_reads.get_mut(&key)
            {
                if let Some(previous) = pending.take()
                    && let Some(complete) = previous.complete
                {
                    scheduled.complete.get_or_insert_default().join(complete);
                }
                *pending = Some(scheduled);
                continue;
            }
            let limit = match &scheduled.effect.scheduling {
                op::Scheduling::Terminal { .. } => {
                    if terminal_commands.len() >= MAX_TERMINAL_QUEUE {
                        effects.extend(finish(&updates, rejected(scheduled, busy_error())));
                    } else {
                        terminal_commands.push_back(scheduled);
                    }
                    continue;
                }
                op::Scheduling::Item(_) => {
                    if let Err(rejection) = item_reads.enqueue(scheduled) {
                        let (scheduled, error) = *rejection;
                        effects.extend(finish(&updates, rejected(scheduled, error)));
                    }
                    continue;
                }
                op::Scheduling::Control => MAX_RPC_JOBS + CONTROL_RESERVE,
                op::Scheduling::Concurrent
                | op::Scheduling::LatestTaskActivity
                | op::Scheduling::LatestList(_)
                | op::Scheduling::LatestAgents(_)
                | op::Scheduling::LatestProject(_)
                | op::Scheduling::LatestReview => MAX_RPC_JOBS,
            };
            if jobs.len() >= limit {
                effects.extend(finish(&updates, rejected(scheduled, busy_error())));
                continue;
            }
            if let Some(key) = scheduled.effect.scheduling.latest_key() {
                latest_reads.insert(key, None);
            }
            jobs.push(perform(Some(peer), session.as_ref(), scheduled));
        }
        while jobs.len() < MAX_RPC_JOBS {
            let Some(scheduled) = item_reads.next() else {
                break;
            };
            jobs.push(perform(Some(peer), session.as_ref(), scheduled));
        }
        while terminal_running.len() < MAX_TERMINAL_JOBS && jobs.len() < MAX_RPC_JOBS {
            let Some(index) = terminal_commands.iter().position(|scheduled| {
                !terminal_running.contains(scheduled.effect.scheduling.terminal().unwrap())
            }) else {
                break;
            };
            let scheduled = terminal_commands.remove(index).unwrap();
            terminal_running.insert(scheduled.effect.scheduling.terminal().unwrap().to_owned());
            jobs.push(perform(Some(peer), session.as_ref(), scheduled));
        }
        tokio::select! {
            _ = stop.cancelled() => break "store closed".into(),
            command = commands.recv() => {
                let Some(command) = command else { break "store closed".into() };
                let command = match command {
                    Command::PrepareDictation { id, cancel } => {
                        if preparation_jobs.len() < MAX_RPC_JOBS {
                            preparation_jobs.push(crate::client::prepare_dictation(peer, id, cancel));
                        }
                        continue;
                    }
                    Command::ConnectionPerformance { epoch, performance } => {
                        if epoch == updates.borrow().epoch && performance.connection_id == peer.diagnostic_id {
                            diagnostic_jobs.clear();
                            let peer = peer.clone();
                            diagnostic_jobs.push(async move {
                                peer.collect_connection_diagnostics(performance).await;
                            });
                        }
                        continue;
                    }
                    Command::Dispatch(command) => command,
                    Command::Browser { request, complete } => {
                        if browser_jobs.len() >= 16 {
                            let _ = complete.send(Err(PeerError::InvalidMessage("ブラウザ操作が混み合っています。少し待って再試行してください。".into())));
                        } else {
                            let peer = peer.clone();
                            browser_jobs.push(async move { let _ = complete.send(peer.call(&request).await); });
                        }
                        continue;
                    }
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
                for mut scheduled in command.effects {
                    scheduled.complete = if scheduled.effect.receipt == ReceiptPolicy::Background { None } else { complete.take() };
                    effects.push(scheduled);
                }
                if let Some(complete) = complete {
                    complete.send(Ok(Outcome::Applied));
                }
            }
            _ = browser_jobs.next(), if !browser_jobs.is_empty() => {},
            _ = diagnostic_jobs.next(), if !diagnostic_jobs.is_empty() => {},
            result = jobs.next(), if !jobs.is_empty() => {
                let mut result = result.unwrap();
                for (id, stream) in result.subscriptions.drain(..) { subscriptions.insert(id, futures_util::stream::try_unfold(stream, |mut stream| async {
                        Ok(stream.read::<crate::session::SessionChange>().await?.map(|line| (line, stream)))
                    }).chain(futures_util::stream::once(async {
                    Err(std::io::Error::other("subscription ended"))
                })).boxed()); }
                if let Some(handle) = result.scheduling.terminal() { terminal_running.remove(handle); }
                if let Some(key) = result.scheduling.latest_key()
                    && let Some(Some(scheduled)) = latest_reads.remove(&key) {
                    effects.push(scheduled);
                }
                effects.extend(item_reads.finish(&updates, result));
            }
            Some(()) = preparation_jobs.next(), if !preparation_jobs.is_empty() => {},
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
    // The Host owns connection-scoped PTYs and grants, including starts in flight.
    // Replacement must not wait for delivery acknowledgments from the old peer.
    let replacing = disconnected.is_some() && session.is_some();
    if replacing {
        session.as_ref().unwrap().close();
    }
    drop(jobs);
    drop(browser_jobs);
    drop(diagnostic_jobs);
    drop(preparation_jobs);
    peer.close().await;
    drop(connection);
    apply(&updates, Event::Disconnected(reason));
    if let Some(complete) = disconnected {
        let _ = complete.send(Ok(()));
    }
}

async fn run_offline(
    updates: &watch::Sender<Arc<Snapshot>>,
    commands: &mut mpsc::Receiver<Command>,
    stop: &CancellationToken,
) -> Option<(Connection, Vec<Scheduled>)> {
    while !stop.is_cancelled() {
        let command = tokio::select! {
            _ = stop.cancelled() => break,
            command = commands.recv() => match command { Some(command) => command, None => break },
        };
        let command = match command {
            Command::PrepareDictation { .. } => continue,
            Command::ConnectionPerformance { .. } => continue,
            Command::Dispatch(command) => command,
            Command::Browser { complete, .. } => {
                let _ = complete.send(Err(PeerError::ConnectionClosed(
                    "Hostに接続してください。".into(),
                )));
                continue;
            }
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
                return Some((*connection, effects));
            }
        };
        let mut complete = Some(Receipt::new(command.complete));
        for mut scheduled in command.effects {
            scheduled.complete = if scheduled.effect.receipt == ReceiptPolicy::Background {
                None
            } else {
                complete.take()
            };
            let result = perform(None, None, scheduled).await;
            drop(finish(updates, result));
        }
        if let Some(complete) = complete {
            complete.send(Ok(Outcome::Applied));
        }
    }
    None
}

fn busy_error() -> PeerError {
    PeerError::InvalidMessage("操作が混み合っています。少し待って再試行してください。".into())
}
fn command_error<T>(error: mpsc::error::TrySendError<T>) -> PeerError {
    match error {
        mpsc::error::TrySendError::Full(_) => busy_error(),
        mpsc::error::TrySendError::Closed(_) => {
            PeerError::ConnectionClosed("store is closed".into())
        }
    }
}
fn rejected(scheduled: Scheduled, error: PeerError) -> Completed {
    Completed {
        scheduling: scheduled.effect.scheduling,
        failed_submission: scheduled
            .effect
            .operation
            .submission_id()
            .map(agent_protocol::ids::ClientInputId::from),
        subscriptions: Vec::new(),
        delivery_attempted: false,
        scope: scheduled.scope,
        complete: scheduled.complete,
        result: Err(error),
        rejection: scheduled.effect.operation.rejected_output(),
    }
}

async fn perform(
    client: Option<&Client>,
    session: Option<&crate::transport::Session>,
    scheduled: Scheduled,
) -> Completed {
    let Scheduled {
        effect,
        scope,
        complete,
    } = scheduled;
    let scheduling = effect.scheduling;
    let failed_submission = effect
        .operation
        .submission_id()
        .map(agent_protocol::ids::ClientInputId::from);
    let mut subscriptions = Vec::new();
    let result = async {
        let client =
            client.ok_or_else(|| PeerError::ConnectionClosed("Host not connected".into()))?;
        let mut context = Execution {
            client,
            session,
            subscriptions: &mut subscriptions,
        };
        effect.operation.run(&mut context).await
    }
    .await;
    Completed {
        subscriptions,
        scheduling,
        delivery_attempted: client.is_some(),
        scope,
        result,
        rejection: None,
        failed_submission,
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
    input: Option<Result<O::Input, PeerError>>,
    output: Option<O::Output>,
}
impl<O: op::Operation> Application for Completion<O> {
    fn apply(
        self: Box<Self>,
        snapshot: &mut Snapshot,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        let Self {
            operation, output, ..
        } = *self;
        let output = output.expect("only completed operations are published");
        operation.complete(snapshot, output, current)
    }
}
// Intent is replayable data. Only the effect queue erases an operation's type;
// the same allocation carries its output until its result is applied.
#[derive(Debug)]
pub struct Effect {
    operation: Box<dyn Pending>,
    scheduling: op::Scheduling,
    receipt: ReceiptPolicy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReceiptPolicy {
    First,
    Continue,
    Background,
}
impl Effect {
    /// Continue the dispatch receipt after this step is applied.
    pub(crate) fn continuation<O: op::Operation>(operation: O) -> Self {
        let mut effect = Self::execute(operation);
        effect.receipt = ReceiptPolicy::Continue;
        effect
    }
    pub fn execute<O: op::Operation>(operation: O) -> Self {
        Self {
            scheduling: operation.scheduling(),
            operation: Box::new(Completion {
                operation,
                input: None,
                output: None,
            }),
            receipt: if O::BACKGROUND {
                ReceiptPolicy::Background
            } else {
                ReceiptPolicy::First
            },
        }
    }
}
trait Pending: Application {
    fn key(&self) -> Option<op::OperationKey>;
    fn capture(&mut self, snapshot: &Snapshot);
    fn submission_id(&self) -> Option<&str>;
    fn rejected_output(self: Box<Self>) -> Option<Box<dyn Application>>;
    fn run<'a>(
        self: Box<Self>,
        context: &'a mut Execution<'_>,
    ) -> futures_util::future::BoxFuture<'a, Result<Applied, PeerError>>;
}
impl<O: op::Operation> Pending for Completion<O> {
    fn key(&self) -> Option<op::OperationKey> {
        self.operation.key()
    }
    fn capture(&mut self, snapshot: &Snapshot) {
        self.input = Some(self.operation.capture(snapshot));
    }
    fn submission_id(&self) -> Option<&str> {
        self.operation.submission_id()
    }
    fn rejected_output(mut self: Box<Self>) -> Option<Box<dyn Application>> {
        let input = self.input.take()?.ok()?;
        self.output = self.operation.rejected_output(input);
        self.output.as_ref()?;
        Some(self)
    }
    fn run<'a>(
        mut self: Box<Self>,
        context: &'a mut Execution<'_>,
    ) -> futures_util::future::BoxFuture<'a, Result<Applied, PeerError>> {
        Box::pin(async move {
            let input = self
                .input
                .take()
                .expect("effects capture inputs before execution")?;
            let mut output = self.operation.run(input, context).await?;
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
    subscriptions: &'a mut Vec<(uuid::Uuid, agent_transport::client::Updates)>,
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

    #[allow(dead_code)]
    mod host_fixture {
        include!("../tests/support/host.rs");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn older_dispatched_list_cannot_replace_newer_notification_work() {
        use serde_json::json;
        let initial = Snapshot::default();
        let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
        let store = Store::new(peer, initial);
        let publications = store.publications.lock().unwrap().clone().unwrap();
        // Dispatch publishes before its command is consumed. A notification can
        // publish newer work on the executor before that older command arrives.
        let older = apply(
            &publications,
            Event::Intent(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            ))),
        );
        let newer = apply(
            &publications,
            Event::Notification(crate::protocol::Notification::SessionRenamed {
                session: crate::session::SessionRef::new(
                    crate::session::ProviderKind::Claude,
                    "changed".into(),
                )
                .unwrap(),
            }),
        );
        let mut receipts = Vec::new();
        for effects in [newer, older] {
            let (complete, receipt) = oneshot::channel();
            store
                .commands
                .send(Command::Dispatch(Dispatch { effects, complete }))
                .await
                .unwrap();
            receipts.push(receipt);
        }
        let replies = async {
            loop {
                let request = reader.read_request().await.unwrap().unwrap();
                let result = match request["method"].as_str().unwrap() {
                    "host/session/list" => {
                        json!({"data":[{"id":{"provider":"claude","id":"latest"}}],
                        "projects":[],"hasMore":false,"hasMoreProjects":false,"limit":5,"projectPages":{}})
                    }
                    "host/taskActivity/read" => json!({"revision":0,"statuses":[],
                        "display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
                    "host/model/list" => json!({"data":[]}),
                    "host/account/list" => json!({"accounts":[],"selected":{}}),
                    "host/name" => json!("Fixture"),
                    method => panic!("unexpected request: {method}"),
                };
                writer
                    .reply(&request, json!({"result":result}))
                    .await
                    .unwrap();
            }
        };
        let settled = async {
            for receipt in receipts {
                receipt.await.unwrap().unwrap();
            }
            let mut updates = store.subscribe();
            updates
                .wait_for(|snapshot| {
                    snapshot.threads.as_ref().is_some_and(|page| {
                        page.data
                            .iter()
                            .any(|thread| thread.id.as_ref().is_some_and(|id| id.id == "latest"))
                    }) && !snapshot
                        .operations
                        .contains_key(&op::OperationKey::SessionList)
                })
                .await
                .unwrap();
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! { _ = settled => {}, _ = replies => unreachable!() }
        })
        .await
        .expect("The newest list must finish even when an older dispatch arrives afterward");
        store.close().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_full_command_queue_keeps_the_draft_and_all_submission_state() {
        let mut initial = Snapshot::default();
        Arc::make_mut(&mut initial.drafts).insert(
            initial.navigation.draft_key.clone(),
            Arc::new(crate::state::Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "model".into(),
                }),
                ..Default::default()
            }),
        );
        let store = Store::offline(initial);
        let draft_key = store.snapshot().navigation.draft_key.clone();
        store
            .dispatch(Intent::SetDraftText {
                thread_id: draft_key,
                text: "must not be lost".into(),
            })
            .await
            .unwrap();
        for index in 0..MAX_COMMANDS {
            drop(store.dispatch(Intent::ReadFile(op::ReadFile {
                path: format!("/queued/{index}"),
                discard_draft: false,
            })));
        }
        let before = store.snapshot();
        let result = store
            .dispatch(Intent::Submit {
                thread_id: None,
                client_user_message_id: "input".into(),
            })
            .await;
        assert!(matches!(result, Err(PeerError::InvalidMessage(_))));
        assert!(Arc::ptr_eq(&before, &store.snapshot()));
        assert!(matches!(
            store.disconnect().await,
            Err(PeerError::InvalidMessage(_))
        ));
        store.close().await.unwrap();
    }
    #[test]
    fn every_snapshot_field_notifies_subscribers_independently() {
        type Change = fn(&mut Snapshot);
        let changes: &[(&str, Change)] = &[
            ("operations", |snapshot| {
                snapshot.operations = Arc::default()
            }),
            ("operation_sequence", |snapshot| {
                snapshot.operation_sequence += 1
            }),
            ("scoped_model_defaults", |snapshot| {
                snapshot.scoped_model_defaults = Arc::default()
            }),
            ("permission_settings", |snapshot| {
                snapshot.permission_settings = Some(Arc::new(
                    crate::state::operations::PermissionSettingsState {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        result: None,
                    },
                ));
            }),
            ("account", |snapshot| snapshot.account = Arc::default()),
            ("terminals", |snapshot| snapshot.terminals = Arc::default()),
            ("conversations", |snapshot| {
                snapshot.conversations = Arc::default()
            }),
            ("task_activity", |snapshot| {
                snapshot.task_activity =
                    Some(Arc::new(agent_protocol::live_activity::TaskActivityState {
                        statuses: Vec::new(),
                        revision: 1,
                        display: agent_protocol::live_activity::TaskActivitySummary {
                            running: 1,
                            ..Default::default()
                        }
                        .display(),
                    }));
            }),
            ("expanded_projects", |snapshot| {
                snapshot.expanded_projects = Arc::new([("p".into(), 5)].into());
            }),
            ("project_threads", |snapshot| {
                snapshot.project_threads = Arc::new(
                    [(
                        "p".into(),
                        Arc::new(crate::models::ThreadList {
                            data: vec![],
                            projects: vec![],
                            has_more: false,
                            has_more_projects: false,
                            limit: 5,
                            project_pages: Default::default(),
                            provider_errors: None,
                        }),
                    )]
                    .into(),
                );
            }),
            ("observed_agents", |snapshot| {
                snapshot.observed_agents = Some(crate::session::SessionRef {
                    provider: crate::session::ProviderKind::Codex,
                    id: "parent".into(),
                });
            }),
            ("models", |snapshot| snapshot.models = Arc::default()),
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
                    has_more: false,
                    has_more_projects: false,
                    limit: 5,
                    project_pages: Default::default(),

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
            writer.send_if_modified(|current| publish_locked(current, previous.as_ref().clone()));
            assert!(
                !reader.has_changed().unwrap(),
                "unchanged snapshot published"
            );
            let mut next = previous.as_ref().clone();
            change(&mut next);
            writer.send_if_modified(|current| publish_locked(current, next));
            assert!(reader.has_changed().unwrap(), "{name} update was dropped");
        }
    }
}

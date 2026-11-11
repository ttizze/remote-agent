//! The single state owner. Independent RPC work publishes completed results.
use crate::{
    client::{
        ClientExt, SessionImage,
        persistence::{DevicePreferences, ModelChoice, Preferences},
    },
    diagnostics::{ConnectionPerformance, ConnectionPhase as Phase},
    peer::PeerError,
    state::{Event, Intent, Snapshot, operations as op, reduce},
};
use agent_protocol::operations::{IdentifyDevice, Pair, RpcMethod};
use agent_transport::client::{Client, Updates};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::{CancellationToken, DropGuard};

mod browser;
mod pane;
pub use browser::{BrowserSession, BrowserUpdate};
pub use pane::{Pane, SideChatStart};

const MAX_COMMANDS: usize = 256;
const MAX_RPC_JOBS: usize = 32;
const CONTROL_RESERVE: usize = 16;
const MAX_TERMINAL_JOBS: usize = 16;
const MAX_TERMINAL_QUEUE: usize = 128;
const MAX_ITEM_READS: usize = 132;
const MAX_ITEM_TRANSFERS: usize = 4;

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
    ResolvedFile {
        entry: crate::models::FileEntry,
    },
    FileReferenceResolved {
        path: String,
        directory: bool,
    },
    FileRead {
        file: crate::models::FileContent,
    },
    Directory {
        listing: crate::models::FileList,
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
    /// Until the Host answers, cached storage may belong to an older Host
    /// configuration, so only the connection's own refresh reads run.
    introduction: Option<Introducing>,
}

/// Host facts that confirm or replace state cached from an earlier connection.
struct Introduction {
    storage_scope: String,
    ticket: Option<String>,
    home_directory: Option<String>,
}
type Introducing = futures_util::future::BoxFuture<'static, Result<Introduction, PeerError>>;

fn introduce(
    peer: Arc<Client>,
    node: crate::transport::NodeId,
    device_name: String,
) -> Introducing {
    async move {
        let identity = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            peer.call(&IdentifyDevice { name: device_name }),
        )
        .await
        .map_err(|_| PeerError::RequestTimeout {
            method: "host/device/identify".into(),
        })??;
        if identity.storage_scope.is_empty() || identity.storage_scope.len() > 256 {
            return Err(PeerError::InvalidMessage(
                "invalid provider storage scope".into(),
            ));
        }
        // Only addresses of the authenticated Host may replace a saved ticket.
        let ticket = identity
            .ticket
            .parse::<crate::transport::Ticket>()
            .ok()
            .filter(|ticket| ticket.node_id() == node)
            .map(|ticket| ticket.to_string());
        Ok(Introduction {
            storage_scope: format!("{node}:{}", identity.storage_scope),
            ticket,
            home_directory: identity.home_directory,
        })
    }
    .boxed()
}

impl Connection {
    /// A scope cached from this Host lets attach start before the Host confirms it.
    async fn open(
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        invitation: Option<uuid::Uuid>,
        device_name: &str,
        cached_scope: &str,
    ) -> Result<(Self, String, ConnectionPerformance), crate::transport::TransportError> {
        let node = ticket.node_id();
        let (session, peer, events, introduction, scope, transport_ms, verification_ms) =
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
                let peer = Arc::new(peer);
                let mut introduction = introduce(peer.clone(), node, device_name.to_owned());
                let scope = if cached_scope.starts_with(&format!("{node}:")) {
                    // Write the request now so it precedes the connection's own reads.
                    if let std::task::Poll::Ready(result) = futures_util::poll!(&mut introduction) {
                        introduction = futures_util::future::ready(result).boxed();
                    }
                    cached_scope.to_owned()
                } else {
                    let introduced = (&mut introduction).await?;
                    let scope = introduced.storage_scope.clone();
                    introduction = futures_util::future::ready(Ok(introduced)).boxed();
                    scope
                };
                Ok::<_, crate::transport::TransportError>((
                    scopeguard::ScopeGuard::into_inner(session),
                    peer,
                    events,
                    introduction,
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
                peer,
                events,
                session: Some(session),
                introduction: Some(introduction),
            },
            scope,
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
            self.0.lock().unwrap().append(&mut other.0.lock().unwrap());
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
    pane: String,
    connection: u64,
    navigation: Option<u64>,
    operation: Option<(op::OperationKey, u64)>,
    report_error: bool,
}
fn generation_current(
    operation: Option<&(op::OperationKey, u64)>,
    operations: &op::OperationStates,
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
        connection: u64,
        navigation: Option<u64>,
        operations: &op::OperationStates,
    ) -> bool {
        self.connection == connection
            && self
                .navigation
                .is_none_or(|epoch| navigation == Some(epoch))
            && generation_current(self.operation.as_ref(), operations)
    }
    fn finish(&self, operations: &mut op::OperationStates, error: Option<&PeerError>) {
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
    fn new(
        mut effect: Effect,
        snapshot: &mut Snapshot,
        pane_id: &str,
        continuation: Option<&Scope>,
    ) -> Self {
        let pane_id = effect.pane.take().unwrap_or_else(|| pane_id.into());
        // Stamp the input being sent before publication, including dictation.
        // Restoring cached input never schedules a send or invents a send time.
        if let Some(id) = effect.operation.submission_id()
            && snapshot
                .sessions
                .pending_submissions
                .get(id)
                .is_some_and(|pending| pending.submitted_at_ms.is_none())
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let inputs = Arc::make_mut(&mut snapshot.sessions.pending_submissions);
            Arc::make_mut(inputs.get_mut(id).unwrap()).submitted_at_ms = Some(now);
        }
        let operation = effect.key(&pane_id).map(|key| {
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
        effect.operation.capture(snapshot, &pane_id);
        Self {
            scope: Scope {
                pane: pane_id.clone(),
                connection: snapshot.connection.epoch,
                navigation: snapshot
                    .panes
                    .get(&pane_id)
                    .filter(|_| effect.operation.navigation_bound())
                    .map(|pane| pane.epoch),
                operation,
                report_error: effect.operation.report_error(),
            },
            effect,
            complete: None,
        }
    }
}
pub struct Store {
    preferences: Arc<DevicePreferences>,
    updates: watch::Receiver<Arc<Snapshot>>,
    presentations: watch::Receiver<Arc<crate::presentation::view::Presentation>>,
    publications: Mutex<Option<watch::Sender<Arc<Snapshot>>>>,
    commands: mpsc::Sender<Command>,
    connection_attempt: Mutex<CancellationToken>,
    stop: CancellationToken,
    _close_on_drop: DropGuard,
    finished: watch::Receiver<bool>,
}
impl Store {
    pub fn new(
        peer: (Client, Updates),
        snapshot: Snapshot,
        preferences: Option<Arc<DevicePreferences>>,
    ) -> Self {
        Self::start(Some(peer), snapshot, preferences)
    }
    pub fn offline(snapshot: Snapshot, preferences: Option<Arc<DevicePreferences>>) -> Self {
        let (snapshot, _) = reduce(&snapshot, "", Event::Disconnected);
        Self::start(None, snapshot, preferences)
    }
    fn start(
        peer: Option<(Client, Updates)>,
        snapshot: Snapshot,
        preferences: Option<Arc<DevicePreferences>>,
    ) -> Self {
        let preferences = preferences.unwrap_or_else(|| {
            DevicePreferences::memory(Preferences {
                models: snapshot.catalog.model_preferences.clone(),
                editor: crate::persistence::EditorPreferences {
                    preferred: snapshot.files.preferred_editor,
                },
                language: snapshot.catalog.ui_language,
                contrast: None,
                composer: snapshot.input.composer_preferences,
            })
        });
        let mut device_updates = preferences.subscribe();
        let mut snapshot = Arc::new(snapshot);
        adopt_preferences(&mut snapshot, &preferences.current());
        preferences.start_writer();
        let (writer, updates) = watch::channel(snapshot);
        let connection = peer.map(|(peer, events)| {
            (
                Connection {
                    peer: Arc::new(peer),
                    events,
                    session: None,
                    introduction: None,
                },
                apply(&writer, Event::Connected),
            )
        });
        let (commands, mut incoming) = mpsc::channel(MAX_COMMANDS);
        let stop = CancellationToken::new();
        let preference_writer = writer.clone();
        let device = preferences.clone();
        let preference_stop = stop.child_token();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = preference_stop.cancelled() => break,
                    result = device_updates.changed() => {
                        if result.is_err() { break; }
                        device_updates.borrow_and_update();
                        preference_writer.send_if_modified(|current| {
                            adopt_preferences(current, &device.current())
                        });
                    }
                }
            }
        });
        let presentations = crate::presentation::view::observe(
            updates.clone(),
            crate::presentation::view::Presentation::pending(updates.borrow().clone(), "")
                .expect("primary pane always exists"),
            stop.child_token(),
            crate::presentation::view::Presentation::project,
        );
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
            apply(&publications, Event::Disconnected);
            finished_tx.send_replace(true);
        });
        Self {
            preferences,
            updates,
            presentations,
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
                connection: crate::state::Connection {
                    connected: true,
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
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
        device_name: &str,
        preferences: Option<Arc<DevicePreferences>>,
    ) -> Result<Self, crate::transport::TransportError> {
        let store = Self::offline(snapshot, preferences);
        store
            .reconnect(endpoint, ticket, invitation, device_name)
            .await?;
        Ok(store)
    }
    /// Reuse a responsive Host connection, then refresh without blocking interaction.
    /// Only reads are retried; pending submissions retain their delivery evidence.
    pub async fn resume(
        &self,
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        device_name: &str,
    ) -> Result<ConnectionPerformance, crate::transport::TransportError> {
        let attempt = {
            let mut current = self.connection_attempt.lock().unwrap();
            current.cancel();
            *current = self.stop.child_token();
            current.clone()
        };
        // Attach acknowledges transferred ownership before successful setup returns.
        let _cancel = attempt.clone().drop_guard();
        if !self.snapshot().connection_enabled() {
            return Err(crate::transport::TransportError::Connection(
                "接続がオフになっています。".into(),
            ));
        }
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
            // Racing a live session, only a verified replacement may retire it.
            let cached_scope = if reusable.is_some() {
                String::new()
            } else {
                self.snapshot().connection.storage_scope.clone()
            };
            let replacement = async {
                let (connection, scope, performance) =
                    Connection::open(endpoint, ticket, None, device_name, &cached_scope).await?;
                Ok::<_, crate::transport::TransportError>((Some((connection, scope)), performance))
            };
            let (prepared, performance) = if let Some(peer) = reusable {
                // Recovery and the liveness read start together. A dead old path
                // cannot add its probe deadline to a working replacement's latency.
                let scope = self.snapshot().connection.storage_scope.clone();
                let probe = async {
                    let started = std::time::Instant::now();
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        introduce(peer.clone(), ticket.node_id(), device_name.to_owned()),
                    )
                    .await
                    {
                        Ok(Ok(current)) => Ok((scope == current.storage_scope).then(|| {
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
                if let Some(id) = &snapshot.panes.primary().navigation.thread_id
                    && !snapshot.sessions.subscriptions.contains_key(id)
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
        let result = tokio::select! {
            biased;
            _ = attempt.cancelled() => Err(PeerError::ConnectionClosed("connection attempt cancelled".into()).into()),
            result = tokio::time::timeout(std::time::Duration::from_secs(45), setup) => result
                .unwrap_or_else(|_| Err(crate::transport::TransportError::Connection("Host recovery timed out".into()))),
        };
        self.report_attempt(&attempt, &result);
        result
    }
    /// Replace transport while retaining local edits. A newer reconnect or
    /// disconnect cancels setup before it can attach an obsolete connection.
    pub async fn reconnect(
        &self,
        endpoint: &crate::transport::Endpoint,
        ticket: &crate::transport::Ticket,
        invitation: Option<uuid::Uuid>,
        device_name: &str,
    ) -> Result<ConnectionPerformance, crate::transport::TransportError> {
        let (attempt, disconnected) = {
            let mut current = self.connection_attempt.lock().unwrap();
            current.cancel();
            *current = self.stop.child_token();
            // Explicit replacement releases the old connection before pairing.
            (current.clone(), self.request_disconnect()?)
        };
        let _cancel = attempt.clone().drop_guard();
        if !self.snapshot().connection_enabled() {
            return Err(crate::transport::TransportError::Connection(
                "接続がオフになっています。".into(),
            ));
        }
        let setup = async {
            disconnected
                .await
                .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
            let (connection, scope, performance) = Connection::open(
                endpoint,
                ticket,
                invitation,
                device_name,
                &self.snapshot().connection.storage_scope,
            )
            .await?;
            self.attach_connection(connection, scope, attempt.clone())
                .await?;
            Ok::<_, crate::transport::TransportError>(performance)
        };
        let result = tokio::select! {
            biased;
            _ = attempt.cancelled() => Err(PeerError::ConnectionClosed("connection attempt cancelled".into()).into()),
            result = setup => result,
        };
        self.report_attempt(&attempt, &result);
        result
    }
    /// A newer attempt or disconnect owns the outcome of the attempt it cancelled.
    /// An attached connection reports through its own lifecycle, because its
    /// Host can still reject the introduction after setup returns.
    fn report_attempt(
        &self,
        attempt: &CancellationToken,
        result: &Result<ConnectionPerformance, crate::transport::TransportError>,
    ) {
        let failure = match result {
            Ok(performance) if !performance.reused => return,
            Ok(_) => None,
            Err(error) => Some(error.to_string()),
        };
        let _current = self.connection_attempt.lock().unwrap();
        let updates = self.publications.lock().unwrap().clone();
        if !attempt.is_cancelled()
            && let Some(updates) = updates
        {
            apply(&updates, Event::ConnectionChecked { failure });
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
    pub fn presentation(&self) -> Arc<crate::presentation::view::Presentation> {
        crate::presentation::view::Presentation::current(
            self.snapshot(),
            &self.presentations.borrow(),
        )
        .expect("primary pane always exists")
    }
    pub fn subscribe_presentation(
        &self,
    ) -> watch::Receiver<Arc<crate::presentation::view::Presentation>> {
        self.presentations.clone()
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
            epoch: self.snapshot().connection.epoch,
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
        self.dispatch_pane("", intent)
    }
    pub fn dispatch_pane(
        &self,
        pane: &str,
        intent: Intent,
    ) -> impl Future<Output = Result<Outcome, PeerError>> + Send + use<> {
        let choice = match &intent {
            Intent::SelectModel { thread_id, .. } => Some((thread_id.clone(), ModelChoice::Model)),
            Intent::SelectEffort { thread_id, .. } => {
                Some((thread_id.clone(), ModelChoice::Effort))
            }
            Intent::SelectServiceTier { thread_id, .. } => {
                Some((thread_id.clone(), ModelChoice::ServiceTier))
            }
            _ => None,
        };
        let editor = match &intent {
            Intent::SetPreferredEditor { editor } => Some(*editor),
            _ => None,
        };
        let composer = match &intent {
            Intent::SetComposerPreferences { preferences } => Some(*preferences),
            _ => None,
        };
        let mut preferences_changed = false;
        let pause = matches!(intent, Intent::SetConnectionEnabled { enabled: false });
        let attempt = pause.then(|| self.connection_attempt.lock().unwrap());
        let mut disconnected = None;
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
                if current.panes.get(pane).is_none() {
                    receipt = Err(PeerError::InvalidMessage("pane is closed".into()));
                    return false;
                }
                adopt_preferences(&mut candidate, &self.preferences.current());
                let (effects, _) = apply_pane_locked(&mut candidate, Event::Intent(intent), pane);
                if let Some(attempt) = &attempt {
                    attempt.cancel();
                    match self.request_disconnect() {
                        Ok(result) => disconnected = Some(result),
                        Err(error) => {
                            receipt = Err(error);
                            return false;
                        }
                    }
                }
                let permit = if effects.is_empty() {
                    None
                } else {
                    match self.commands.try_reserve() {
                        Ok(permit) => Some(permit),
                        Err(error) => {
                            receipt = Err(command_error(error));
                            return false;
                        }
                    }
                };
                let update = if let Some((thread, choice)) = choice {
                    candidate.input.drafts.get(&thread).and_then(|draft| {
                        draft.model.clone().map(|model| {
                            self.preferences.apply_model(
                                choice,
                                crate::state::ModelSelection {
                                    model,
                                    effort: draft.effort.clone(),
                                    service_tier: draft.service_tier.clone(),
                                },
                            )
                        })
                    })
                } else if let Some(editor) = editor {
                    Some(self.preferences.apply_editor(editor))
                } else {
                    composer.map(|composer| self.preferences.apply_composer(composer))
                };
                if let Some(update) = update {
                    match update {
                        Ok(preferences) => {
                            adopt_preferences(&mut candidate, &preferences);
                            preferences_changed = true;
                        }
                        Err(reason) => {
                            receipt = Err(PeerError::InvalidMessage(reason.to_string()));
                            return false;
                        }
                    }
                }
                if let Some(permit) = permit {
                    let (complete, result) = oneshot::channel();
                    permit.send(Command::Dispatch(Dispatch { effects, complete }));
                    receipt = Ok(Some(result));
                }
                // A stale preference may be adopted then explicitly selected back.
                // Only the final state determines whether readers need notification.
                let changed = candidate.changes_from(current).any;
                *current = candidate;
                changed
            });
        }
        if preferences_changed {
            // Watch notifications can synchronously read this Store again.
            self.preferences.publish();
        }
        drop(attempt);
        async move {
            if let Some(result) = disconnected {
                result
                    .await
                    .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))??;
            }
            match receipt? {
                None => Ok(Outcome::Applied),
                Some(result) => result
                    .await
                    .map_err(|_| PeerError::ConnectionClosed("store is closed".into()))?,
            }
        }
    }
    fn create_pane(
        &self,
        cwd: String,
    ) -> Result<Arc<crate::presentation::view::Presentation>, PeerError> {
        if self.stop.is_cancelled() {
            return Err(PeerError::ConnectionClosed("store is closed".into()));
        }
        let publications = self
            .publications
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| PeerError::ConnectionClosed("store is closed".into()))?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut initial = None;
        publications.send_if_modified(|current| {
            let mut next = current.as_ref().clone();
            next.open_pane(id.clone(), cwd);
            let changed = publish_locked(current, next);
            initial = Some(current.clone());
            changed
        });
        initial
            .and_then(|snapshot| crate::presentation::view::Presentation::pending(snapshot, &id))
            .ok_or_else(|| PeerError::ConnectionClosed("pane is closed".into()))
    }
    fn close_pane(&self, pane: &str) {
        if let Some(publications) = self.publications.lock().unwrap().clone() {
            publications.send_if_modified(|current| {
                let mut next = current.as_ref().clone();
                next.close_pane(pane);
                publish_locked(current, next)
            });
        }
    }
    fn subscribe_pane(
        &self,
        initial: Arc<crate::presentation::view::Presentation>,
        stop: CancellationToken,
    ) -> watch::Receiver<Arc<crate::presentation::view::Presentation>> {
        crate::presentation::view::observe(
            self.updates.clone(),
            initial,
            stop,
            crate::presentation::view::Presentation::project,
        )
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
    apply_pane_locked(current, event, "")
}
fn apply_pane_locked(
    current: &mut Arc<Snapshot>,
    event: Event,
    pane_id: &str,
) -> (Vec<Scheduled>, bool) {
    let (mut next, effects) = reduce(current, pane_id, event);
    let scheduled = effects
        .into_iter()
        .map(|effect| Scheduled::new(effect, &mut next, pane_id, None))
        .collect();
    let changed = publish_locked(current, next);
    (scheduled, changed)
}
fn adopt_preferences(snapshot: &mut Arc<Snapshot>, preferences: &Preferences) -> bool {
    if snapshot.catalog.model_preferences == preferences.models
        && snapshot.files.preferred_editor == preferences.editor.preferred
        && snapshot.catalog.ui_language == preferences.language
        && snapshot.input.composer_preferences == preferences.composer
    {
        return false;
    }
    let snapshot = Arc::make_mut(snapshot);
    snapshot.catalog.model_preferences = preferences.models.clone();
    snapshot.files.preferred_editor = preferences.editor.preferred;
    snapshot.catalog.ui_language = preferences.language;
    snapshot.input.composer_preferences = preferences.composer;
    true
}

/// Publish one consistent transition when any state group changed.
fn publish_locked(current: &mut Arc<Snapshot>, next: Snapshot) -> bool {
    let changed = next.changes_from(current).any;
    if changed {
        *current = Arc::new(next);
    }
    changed
}

fn finish_unobserved(updates: &watch::Sender<Arc<Snapshot>>, scheduled: Scheduled) {
    updates.send_if_modified(|snapshot| {
        let mut next = snapshot.as_ref().clone();
        scheduled.scope.finish(&mut next.operations, None);
        publish_locked(snapshot, next)
    });
    if let Some(complete) = scheduled.complete {
        complete.send(Ok(Outcome::Applied));
    }
}

fn finish(updates: &watch::Sender<Arc<Snapshot>>, completed: Completed) -> Vec<Scheduled> {
    let mut effects = Vec::new();
    let mut scheduled = Vec::new();
    let mut result = Ok(Outcome::Applied);
    updates.send_if_modified(|root| {
        let snapshot = root.as_ref();
        let pane_id = &completed.scope.pane;
        let pane = snapshot.panes.get(pane_id);
        let open = pane.is_some();
        let current = completed.scope.current(
            snapshot.connection.epoch,
            pane.map(|pane| pane.epoch),
            &snapshot.operations,
        );
        let global_error = current
            && open
            && completed.scope.report_error
            && completed.scope.operation.is_none()
            && !matches!(
                &completed.scheduling,
                op::Scheduling::LatestProject(_) | op::Scheduling::LatestHostState(_)
            );
        let mut next = snapshot.clone();
        result = match completed.result {
            Ok(applied) => match applied.application.apply(&mut next, pane_id, current) {
                Ok(next_effects) => {
                    effects = next_effects;
                    Ok(applied.outcome)
                }
                Err(error) => {
                    if global_error && let Some(pane) = next.panes.get_mut(pane_id) {
                        Arc::make_mut(pane).error = Some(error.to_string());
                    }
                    Err(error)
                }
            },
            Err(error) => {
                let error = match completed.rejection {
                    Some(application) => match application.apply(&mut next, pane_id, current) {
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
                    next = crate::state::reduce_event(
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
                    next = crate::state::reduce_event(
                        &next,
                        if unknown {
                            Event::SubmissionUnknown(id)
                        } else {
                            Event::SubmissionFailed(id)
                        },
                    )
                    .0;
                }
                if global_error && let Some(pane) = next.panes.get_mut(pane_id) {
                    Arc::make_mut(pane).error = Some(error.to_string());
                }
                Err(error)
            }
        };
        effects.extend(op::prefetch_composer_catalog(
            next.connection.connected,
            &mut next.catalog.composer_catalogs,
            next.panes
                .0
                .iter()
                .map(|(id, pane)| (id.as_str(), pane.navigation.as_ref())),
        ));
        effects.extend(op::prefetch_model_settings(
            next.connection.connected,
            next.panes
                .get(pane_id)
                .and_then(|pane| next.input.drafts.get(&pane.navigation.draft_key))
                .and_then(|draft| draft.model.as_ref()),
            &next.catalog.models,
            next.operations.keys(),
        ));
        let continues = completed.scope.operation.as_ref().is_some_and(|(key, _)| {
            effects
                .iter()
                .any(|effect| effect.key(pane_id).as_ref() == Some(key))
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
                    || effect.key(pane_id).as_ref()
                        != completed.scope.operation.as_ref().map(|(key, _)| key)
            })
            .map(|effect| Scheduled::new(effect, &mut next, pane_id, Some(&completed.scope)))
            .collect();
        publish_locked(root, next)
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
    fn release(&mut self, key: &op::ReadItem) {
        self.running.remove(key);
        self.receipts.remove(key);
    }
}
#[derive(Default)]
struct Scheduler {
    latest_reads: BTreeMap<op::OperationKey, Option<Scheduled>>,
    terminal_commands: VecDeque<Scheduled>,
    terminal_running: BTreeSet<String>,
    item_reads: ItemReads,
}

enum Admission {
    Run(Scheduled),
    Queued,
    Unobserved(Scheduled),
    Rejected(Box<(Scheduled, PeerError)>),
}

impl Scheduler {
    fn admit(
        &mut self,
        mut scheduled: Scheduled,
        snapshot: &Snapshot,
        running_jobs: usize,
    ) -> Admission {
        // Shared reads depend on interests in the one map, never a fallback pane.
        let unobserved = (scheduled
            .effect
            .scheduling
            .latest_key(&scheduled.scope.pane)
            .is_some()
            && !generation_current(scheduled.scope.operation.as_ref(), &snapshot.operations))
            || matches!(&scheduled.effect.scheduling, op::Scheduling::LatestAgents(session)
                if !snapshot.panes.0.values().any(|pane| pane.observed_agents.as_ref() == Some(session)));
        if unobserved {
            return Admission::Unobserved(scheduled);
        }
        if let Some(key) = scheduled
            .effect
            .scheduling
            .latest_key(&scheduled.scope.pane)
            && let Some(pending) = self.latest_reads.get_mut(&key)
        {
            if let Some(previous) = pending.take()
                && let Some(complete) = previous.complete
            {
                scheduled.complete.get_or_insert_default().join(complete);
            }
            *pending = Some(scheduled);
            return Admission::Queued;
        }
        let limit = match &scheduled.effect.scheduling {
            op::Scheduling::Terminal { .. } => {
                if self.terminal_commands.len() >= MAX_TERMINAL_QUEUE {
                    return Admission::Rejected(Box::new((scheduled, busy_error())));
                }
                self.terminal_commands.push_back(scheduled);
                return Admission::Queued;
            }
            op::Scheduling::Item(_) => {
                return match self.item_reads.enqueue(scheduled) {
                    Ok(()) => Admission::Queued,
                    Err(rejected) => Admission::Rejected(rejected),
                };
            }
            op::Scheduling::Control => MAX_RPC_JOBS + CONTROL_RESERVE,
            _ => MAX_RPC_JOBS,
        };
        if running_jobs >= limit {
            return Admission::Rejected(Box::new((scheduled, busy_error())));
        }
        if let Some(key) = scheduled
            .effect
            .scheduling
            .latest_key(&scheduled.scope.pane)
        {
            self.latest_reads.insert(key, None);
        }
        Admission::Run(scheduled)
    }

    fn next(&mut self, running_jobs: usize) -> Option<Scheduled> {
        if running_jobs >= MAX_RPC_JOBS {
            return None;
        }
        if let Some(scheduled) = self.item_reads.next() {
            return Some(scheduled);
        }
        if self.terminal_running.len() >= MAX_TERMINAL_JOBS {
            return None;
        }
        let index = self.terminal_commands.iter().position(|scheduled| {
            !self
                .terminal_running
                .contains(scheduled.effect.scheduling.terminal().unwrap())
        })?;
        let scheduled = self.terminal_commands.remove(index).unwrap();
        self.terminal_running
            .insert(scheduled.effect.scheduling.terminal().unwrap().to_owned());
        Some(scheduled)
    }

    fn complete(
        &mut self,
        pane: &str,
        scheduling: &op::Scheduling,
        followups: &[Scheduled],
    ) -> Vec<Scheduled> {
        if let Some(handle) = scheduling.terminal() {
            self.terminal_running.remove(handle);
        }
        let pending = scheduling
            .latest_key(pane)
            .and_then(|key| self.latest_reads.remove(&key))
            .flatten();
        if let Some(key) = scheduling.item()
            && !followups.iter().any(|scheduled| {
                scheduled.effect.receipt == ReceiptPolicy::Continue
                    && scheduled.effect.scheduling.item() == Some(key)
            })
        {
            self.item_reads.release(key);
        }
        pending.into_iter().collect()
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
    let mut scheduler = Scheduler::default();
    let mut disconnected = None;
    let mut introduction = connection.introduction.take();
    let mut held = Vec::new();
    loop {
        let unused: Vec<_> = subscriptions
            .keys()
            .filter(|id| {
                !updates
                    .borrow()
                    .sessions
                    .subscriptions
                    .values()
                    .any(|current| current == *id)
            })
            .copied()
            .collect();
        for id in unused {
            subscriptions.remove(&id);
        }
        for scheduled in std::mem::take(&mut effects) {
            let admission = scheduler.admit(scheduled, &updates.borrow(), jobs.len());
            // Publish each rejection before deciding the next effect: recovery
            // can change the operation generation used by admission.
            match admission {
                Admission::Run(scheduled) => {
                    jobs.push(perform(Some(peer), session.as_ref(), scheduled))
                }
                Admission::Queued => {}
                Admission::Unobserved(scheduled) => finish_unobserved(&updates, scheduled),
                Admission::Rejected(rejection) => {
                    let (scheduled, error) = *rejection;
                    effects.extend(finish(&updates, rejected(scheduled, error)));
                }
            }
        }
        while let Some(scheduled) = scheduler.next(jobs.len()) {
            jobs.push(perform(Some(peer), session.as_ref(), scheduled));
        }
        tokio::select! {
            _ = stop.cancelled() => break,
            command = commands.recv() => {
                let Some(command) = command else { break };
                let command = match command {
                    Command::PrepareDictation { id, cancel } => {
                        if preparation_jobs.len() < MAX_RPC_JOBS {
                            preparation_jobs.push(crate::client::prepare_dictation(peer, id, cancel));
                        }
                        continue;
                    }
                    Command::ConnectionPerformance { epoch, performance } => {
                        if epoch == updates.borrow().connection.epoch && performance.connection_id == peer.diagnostic_id {
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
                        break;
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
                    if introduction.is_some() { held.push(scheduled) } else { effects.push(scheduled) }
                }
                if let Some(complete) = complete {
                    complete.send(Ok(Outcome::Applied));
                }
            }
            introduced = async { introduction.as_mut().unwrap().await }, if introduction.is_some() => {
                introduction = None;
                let introduced = match introduced {
                    Ok(introduced) => introduced,
                    // The Host never confirmed this connection, so its setup failed.
                    Err(error) => {
                        apply(&updates, Event::ConnectionChecked { failure: Some(error.to_string()) });
                        break;
                    }
                };
                if introduced.storage_scope == updates.borrow().connection.storage_scope {
                    effects.append(&mut held);
                } else {
                    // Work started against the cached storage must not reach the Host's new one.
                    for scheduled in held.drain(..) {
                        let error = PeerError::InvalidMessage("ホストの保存先が変わったため、操作を取り消しました。".into());
                        effects.extend(finish(&updates, rejected(scheduled, error)));
                    }
                    apply(&updates, Event::StorageScope(introduced.storage_scope));
                    effects.extend(apply(&updates, Event::Connected));
                }
                if let Some(ticket) = introduced.ticket {
                    apply(&updates, Event::HostTicket(ticket));
                }
                apply(&updates, Event::HostHomeDirectory(introduced.home_directory));
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
                let scheduling = result.scheduling.clone();
                let pane = result.scope.pane.clone();
                let followups = finish(&updates, result);
                effects.extend(scheduler.complete(&pane, &scheduling, &followups));
                effects.extend(followups);
            }
            Some(()) = preparation_jobs.next(), if !preparation_jobs.is_empty() => {},
            Some((id, update)) = subscriptions.next(), if !subscriptions.is_empty() => {
                match update {
                    Ok(change) => effects.extend(apply(&updates, Event::SessionUpdate(Box::new(crate::session::SessionUpdate { subscription_id: id, change })))),
                    Err(_) => break,
                }
            }
            event = events.read::<crate::protocol::Notification>() => match event {
                Ok(Some(notification)) => effects.extend(apply(&updates, Event::Notification(notification))),
                Ok(None) | Err(_) => break,
            }
        }
    }
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
    // A missing CLOSE acknowledgement can delay transport cleanup. Native
    // input and recovery must observe the lost event stream immediately.
    apply(&updates, Event::Disconnected);
    peer.close().await;
    drop(connection);
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
                    "ホストに接続してください。".into(),
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
        pane_id: &str,
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
        pane_id: &str,
        current: bool,
    ) -> Result<Vec<Effect>, PeerError> {
        let Self {
            operation, output, ..
        } = *self;
        let output = output.expect("only completed operations are published");
        operation.complete(snapshot, pane_id, output, current)
    }
}
// Intent is replayable data. Only the effect queue erases an operation's type;
// the same allocation carries its output until its result is applied.
#[derive(Debug)]
pub struct Effect {
    pane: Option<String>,
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
    fn key(&self, pane: &str) -> Option<op::OperationKey> {
        self.operation
            .key()
            .map(|key| key.in_pane(self.pane.as_deref().unwrap_or(pane)))
    }
    pub(crate) fn in_pane(mut self, pane: &str) -> Self {
        self.pane = Some(pane.into());
        self
    }
    /// Continue the dispatch receipt after this step is applied.
    pub(crate) fn continuation<O: op::Operation>(operation: O) -> Self {
        let mut effect = Self::execute(operation);
        effect.receipt = ReceiptPolicy::Continue;
        effect
    }
    pub fn execute<O: op::Operation>(operation: O) -> Self {
        Self {
            pane: None,
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
    fn navigation_bound(&self) -> bool;
    fn key(&self) -> Option<op::OperationKey>;
    fn report_error(&self) -> bool;
    fn capture(&mut self, snapshot: &Snapshot, pane_id: &str);
    fn submission_id(&self) -> Option<&str>;
    fn rejected_output(self: Box<Self>) -> Option<Box<dyn Application>>;
    fn run<'a>(
        self: Box<Self>,
        context: &'a mut Execution<'_>,
    ) -> futures_util::future::BoxFuture<'a, Result<Applied, PeerError>>;
}
impl<O: op::Operation> Pending for Completion<O> {
    fn navigation_bound(&self) -> bool {
        self.operation.navigation_bound()
    }
    fn key(&self) -> Option<op::OperationKey> {
        self.operation.key()
    }
    fn report_error(&self) -> bool {
        O::REPORT_ERROR
    }
    fn capture(&mut self, snapshot: &Snapshot, pane_id: &str) {
        self.input = Some(self.operation.capture(snapshot, pane_id));
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

    #[test]
    fn joined_receipts_deliver_the_result_to_every_waiter() {
        let receipt = Receipt::default();
        let mut waiters = Vec::new();
        for _ in 0..256 {
            let (sender, receive) = oneshot::channel();
            receipt.join(Receipt::new(sender));
            waiters.push(receive);
        }
        for waiter in &mut waiters {
            assert!(matches!(
                waiter.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
        }
        receipt.join(receipt.clone());
        // A caller cancelling its wait must not affect the other callers.
        waiters.pop();
        receipt.send(Ok(Outcome::Applied));
        for mut waiter in waiters {
            assert_eq!(waiter.try_recv().unwrap().unwrap(), Outcome::Applied);
        }
    }

    #[tokio::test]
    async fn rejected_downloads_return_errors_without_overwriting_application_notices() {
        let mut snapshot = Snapshot {
            connection: crate::state::Connection {
                ..Default::default()
            },
            panes: crate::state::PaneStates::with_primary(Arc::new(crate::state::PaneState {
                error: Some("unrelated failure".into()),
                ..Default::default()
            })),
            ..Default::default()
        };
        let mut download = Scheduled::new(
            Effect::execute(op::DownloadFile {
                source: "/missing.png".into(),
                destination: "/preview.png".into(),
            }),
            &mut snapshot,
            "",
            None,
        );
        let (sender, receipt) = oneshot::channel();
        download.complete = Some(Receipt::new(sender));
        let (updates, _) = watch::channel(Arc::new(snapshot.clone()));
        assert!(finish(&updates, rejected(download, busy_error())).is_empty());
        assert_eq!(
            receipt.await.unwrap().unwrap_err().to_string(),
            busy_error().to_string()
        );
        assert_eq!(updates.borrow().as_ref(), &snapshot);
        let store = Store::offline(Snapshot::default(), None);
        assert!(
            store
                .dispatch(Intent::DownloadFile(op::DownloadFile {
                    source: "/missing.png".into(),
                    destination: "/preview.png".into(),
                }))
                .await
                .is_err()
        );
        assert!(store.snapshot().panes.primary().error.is_none());
        let error = store
            .dispatch(Intent::RemoveWorktree(op::RemoveWorktree {
                path: "/worktree".into(),
            }))
            .await
            .unwrap_err();
        assert_eq!(
            store.snapshot().panes.primary().error.as_deref(),
            Some(error.to_string().as_str())
        );
        store.close().await.unwrap();
    }

    #[test]
    fn shared_operation_failures_remain_visible_after_navigation() {
        for effect in [
            Effect::execute(op::RemoveWorktree {
                path: "/worktree".into(),
            }),
            Effect::execute(op::WriteTerminal {
                handle: "terminal".into(),
                data: Vec::new(),
            }),
        ] {
            let mut snapshot = Snapshot::default();
            let scheduled = Scheduled::new(effect, &mut snapshot, "", None);
            let (snapshot, _) = crate::state::reduce(
                &snapshot,
                "",
                Event::Intent(Intent::NewChat {
                    cwd: "/another".into(),
                }),
            );
            let (updates, _) = watch::channel(Arc::new(snapshot));
            finish(&updates, rejected(scheduled, busy_error()));
            assert_eq!(
                updates.borrow().panes.primary().error.as_deref(),
                Some(busy_error().to_string().as_str()),
            );
        }
    }

    #[tokio::test]
    async fn a_terminal_failure_does_not_discard_the_composer_catalog_load() {
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        Arc::make_mut(snapshot.panes.primary_mut()).navigation =
            Arc::new(crate::state::Navigation {
                cwd: "/fixture".into(),
                draft_key: "new:/fixture".into(),
                ..Default::default()
            });
        let scheduled = Scheduled::new(
            Effect::execute(op::StartTerminal {
                handle: "terminal".into(),
                cwd: "/fixture".into(),
                size: agent_protocol::operations::TerminalSize { cols: 80, rows: 24 },
            }),
            &mut snapshot,
            "",
            None,
        );
        let ((client, _events), mut reader, writer) = host_fixture::connect(&snapshot).await;
        reader.script_composer_catalog();
        let (updates, _) = watch::channel(Arc::new(snapshot));
        let followups = finish(&updates, rejected(scheduled, busy_error()));
        assert!(updates.borrow().catalog.composer_catalogs["/fixture"].loading);
        let loading = tokio::spawn(async move {
            for followup in followups {
                finish(&updates, perform(Some(&client), None, followup).await);
            }
            updates.borrow().clone()
        });
        let request =
            tokio::time::timeout(std::time::Duration::from_secs(2), reader.read_request())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert_eq!(request["method"], "host/composer/catalog");
        assert_eq!(request["params"]["cwd"], "/fixture");
        writer
            .reply(
                &request,
                serde_json::json!({"result":{"cwd":"/fixture","loading":false,"candidates":[],"errors":{}}}),
            )
            .await
            .unwrap();
        let loaded = tokio::time::timeout(std::time::Duration::from_secs(2), loading)
            .await
            .unwrap()
            .unwrap();
        assert!(!loaded.catalog.composer_catalogs["/fixture"].loading);
    }

    #[test]
    fn workspace_reads_keep_independent_statuses_and_retire_only_their_owner() {
        let mut snapshot = Snapshot::default();
        snapshot.open_pane("side".into(), "/fixture".into());
        Scheduled::new(
            Effect::execute(op::ReadPermissionSettings {
                provider: crate::session::ProviderKind::Codex,
            }),
            &mut snapshot,
            "side",
            None,
        );
        for pane in ["", "side"] {
            for effect in [
                Effect::execute(op::ListFiles {
                    path: "/fixture".into(),
                }),
                Effect::execute(op::ReadFile {
                    path: "/fixture/file".into(),
                    discard_draft: false,
                }),
                Effect::execute(op::ReviewWorkspace {
                    cwd: "/fixture".into(),
                }),
            ] {
                Scheduled::new(effect, &mut snapshot, pane, None);
            }
            for key in [
                op::OperationKey::Directory { pane: pane.into() },
                op::OperationKey::File { pane: pane.into() },
                op::OperationKey::WorkspaceReview { pane: pane.into() },
            ] {
                assert!(snapshot.operation_running(key));
            }
        }
        snapshot.invalidate_pane("side");
        for (pane, running) in [("", true), ("side", false)] {
            for key in [
                op::OperationKey::Directory { pane: pane.into() },
                op::OperationKey::File { pane: pane.into() },
                op::OperationKey::WorkspaceReview { pane: pane.into() },
            ] {
                assert_eq!(snapshot.operation_running(key), running);
            }
        }
        assert!(snapshot.operation_running(op::OperationKey::Permissions {
            provider: crate::session::ProviderKind::Codex,
        }));
    }

    fn queued(scheduling: op::Scheduling) -> Scheduled {
        let mut effect = Effect::execute(op::ReadTaskActivity {});
        effect.scheduling = scheduling;
        Scheduled {
            effect,
            scope: Scope {
                pane: String::new(),
                connection: 0,
                navigation: Some(0),
                operation: None,
                report_error: true,
            },
            complete: None,
        }
    }

    fn item(id: &str) -> op::ReadItem {
        op::ReadItem {
            thread_id: crate::session::SessionRef {
                provider: crate::session::ProviderKind::Codex,
                id: "chat".into(),
            },
            turn_id: "turn".into(),
            item_id: id.into(),
        }
    }

    fn enqueue(queue: &mut Scheduler, scheduling: op::Scheduling) {
        assert!(matches!(
            queue.admit(queued(scheduling), &Snapshot::default(), 0),
            Admission::Queued
        ));
    }

    #[test]
    fn scheduler_serializes_each_terminal_and_releases_completed_work() {
        let mut queue = Scheduler::default();
        let terminal = |handle: &str| op::Scheduling::Terminal {
            handle: handle.into(),
            starts: false,
        };
        for handle in ["a", "a", "b"] {
            enqueue(&mut queue, terminal(handle));
        }
        let first = queue.next(0).unwrap();
        assert_eq!(first.effect.scheduling.terminal(), Some("a"));
        assert_eq!(
            queue.next(1).unwrap().effect.scheduling.terminal(),
            Some("b")
        );
        assert!(queue.next(2).is_none());
        assert!(queue.complete("", &first.effect.scheduling, &[]).is_empty());
        assert_eq!(
            queue.next(1).unwrap().effect.scheduling.terminal(),
            Some("a")
        );
    }

    #[test]
    fn scheduler_reserves_control_capacity_and_bounds_terminal_concurrency() {
        let mut queue = Scheduler::default();
        assert!(matches!(
            queue.admit(
                queued(op::Scheduling::Concurrent),
                &Snapshot::default(),
                MAX_RPC_JOBS
            ),
            Admission::Rejected(_)
        ));
        assert!(matches!(
            queue.admit(
                queued(op::Scheduling::Control),
                &Snapshot::default(),
                MAX_RPC_JOBS
            ),
            Admission::Run(_)
        ));
        assert!(matches!(
            queue.admit(
                queued(op::Scheduling::Control),
                &Snapshot::default(),
                MAX_RPC_JOBS + CONTROL_RESERVE
            ),
            Admission::Rejected(_)
        ));
        let mut running = Vec::new();
        for index in 0..=MAX_TERMINAL_JOBS {
            enqueue(
                &mut queue,
                op::Scheduling::Terminal {
                    handle: index.to_string(),
                    starts: false,
                },
            );
        }
        for index in 0..MAX_TERMINAL_JOBS {
            running.push(queue.next(index).unwrap());
        }
        assert!(queue.next(MAX_TERMINAL_JOBS).is_none());
        queue.complete("", &running.pop().unwrap().effect.scheduling, &[]);
        assert!(queue.next(MAX_TERMINAL_JOBS - 1).is_some());
    }

    #[test]
    fn scheduler_coalesces_latest_reads_until_completion() {
        let mut queue = Scheduler::default();
        let latest = || {
            queued(op::Scheduling::LatestHostState(op::OperationKey::Models {
                provider: agent_protocol::session::ProviderKind::Codex,
            }))
        };
        let Admission::Run(first) = queue.admit(latest(), &Snapshot::default(), 0) else {
            panic!("initial read must run")
        };
        assert!(matches!(
            queue.admit(latest(), &Snapshot::default(), 1),
            Admission::Queued
        ));
        let mut newest = latest();
        newest.scope.navigation = Some(9);
        assert!(matches!(
            queue.admit(newest, &Snapshot::default(), 1),
            Admission::Queued
        ));
        assert!(queue.next(1).is_none());
        let mut followups = queue.complete("", &first.effect.scheduling, &[]);
        assert_eq!(followups.len(), 1);
        assert_eq!(followups[0].scope.navigation, Some(9));
        assert!(matches!(
            queue.admit(followups.pop().unwrap(), &Snapshot::default(), 0),
            Admission::Run(_)
        ));
    }

    #[test]
    fn scheduler_releases_item_slots_and_prioritizes_items_over_terminals() {
        let mut queue = Scheduler::default();
        enqueue(
            &mut queue,
            op::Scheduling::Terminal {
                handle: "terminal".into(),
                starts: false,
            },
        );
        for index in 0..MAX_ITEM_TRANSFERS {
            enqueue(&mut queue, op::Scheduling::Item(item(&index.to_string())));
        }
        assert!(queue.next(MAX_RPC_JOBS).is_none());
        let first = queue.next(0).unwrap();
        assert_eq!(first.effect.scheduling.item(), Some(&item("0")));
        for index in 1..MAX_ITEM_TRANSFERS {
            assert!(
                queue
                    .next(index)
                    .unwrap()
                    .effect
                    .scheduling
                    .item()
                    .is_some()
            );
        }
        queue.complete("", &first.effect.scheduling, &[]);
        enqueue(&mut queue, op::Scheduling::Item(item("new")));
        let replacement = queue.next(0).unwrap();
        assert_eq!(replacement.effect.scheduling.item(), Some(&item("new")));
        queue.complete("", &replacement.effect.scheduling, &[]);
        enqueue(&mut queue, op::Scheduling::Item(item("0")));
        assert_eq!(
            queue.next(0).unwrap().effect.scheduling.item(),
            Some(&item("0"))
        );
        assert_eq!(
            queue.next(0).unwrap().effect.scheduling.terminal(),
            Some("terminal")
        );
    }

    #[test]
    fn scheduler_continuations_keep_joined_receipts_and_release_unrelated_items() {
        let mut queue = Scheduler::default();
        let (first, mut first_receipt) = oneshot::channel();
        let mut scheduled = queued(op::Scheduling::Item(item("one")));
        scheduled.complete = Some(Receipt::new(first));
        assert!(matches!(
            queue.admit(scheduled, &Snapshot::default(), 0),
            Admission::Queued
        ));
        let reading = queue.next(0).unwrap();
        let mut continuation = queued(op::Scheduling::Item(item("one")));
        continuation.effect.receipt = ReceiptPolicy::Continue;
        continuation.complete = reading.complete;
        queue.complete(
            "",
            &reading.effect.scheduling,
            std::slice::from_ref(&continuation),
        );
        let (second, mut second_receipt) = oneshot::channel();
        let mut duplicate = queued(op::Scheduling::Item(item("one")));
        duplicate.complete = Some(Receipt::new(second));
        assert!(matches!(
            queue.admit(duplicate, &Snapshot::default(), 0),
            Admission::Queued
        ));
        assert!(queue.next(0).is_none());
        assert!(matches!(
            queue.admit(continuation, &Snapshot::default(), 0),
            Admission::Queued
        ));
        let continued = queue.next(0).unwrap();
        continued.complete.unwrap().send(Ok(Outcome::Applied));
        assert!(first_receipt.try_recv().unwrap().is_ok());
        assert!(second_receipt.try_recv().unwrap().is_ok());
        let mut unrelated = queued(op::Scheduling::Item(item("other")));
        unrelated.effect.receipt = ReceiptPolicy::Continue;
        queue.complete("", &continued.effect.scheduling, &[unrelated]);
        enqueue(&mut queue, op::Scheduling::Item(item("one")));
        assert_eq!(
            queue.next(0).unwrap().effect.scheduling.item(),
            Some(&item("one"))
        );
    }

    #[test]
    fn scheduler_ignores_superseded_and_unobserved_reads() {
        let mut queue = Scheduler::default();
        let mut scheduled = queued(op::Scheduling::LatestHostState(op::OperationKey::Models {
            provider: agent_protocol::session::ProviderKind::Codex,
        }));
        scheduled.scope.operation = Some((
            op::OperationKey::Models {
                provider: agent_protocol::session::ProviderKind::Codex,
            },
            1,
        ));
        assert!(matches!(
            queue.admit(scheduled, &Snapshot::default(), 0),
            Admission::Unobserved(_)
        ));
        assert!(matches!(
            queue.admit(
                queued(op::Scheduling::LatestAgents(item("one").thread_id)),
                &Snapshot::default(),
                0
            ),
            Admission::Unobserved(_)
        ));
    }

    #[allow(dead_code)]
    mod host_fixture {
        include!("../tests/support/host.rs");
    }

    #[tokio::test]
    async fn new_task_admits_the_initial_input_in_one_rpc() {
        use crate::state::operations::Operation;
        use serde_json::json;
        let ((client, _events), mut reader, writer) =
            host_fixture::connect(&Snapshot::default()).await;
        let mut subscriptions = Vec::new();
        let mut context = Execution {
            client: &client,
            session: None,
            subscriptions: &mut subscriptions,
        };
        let operation = op::StartSubmission {
            provider: agent_protocol::session::ProviderKind::Codex,
            draft_key: "local".into(),
            cwd: Some("/project".into()),
            client_user_message_id: "initial-input".into(),
            draft: Arc::new(crate::state::Draft {
                text: "initial message".into(),
                attachments: vec![crate::state::Attachment {
                    path: "/image.png".into(),
                    name: "image.png".into(),
                    is_image: true,
                }],
                model: Some(agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "chosen-model".into(),
                }),
                effort: Some("high".into()),
                service_tier: Some("fast".into()),
                ..Default::default()
            }),
        };
        let reply = async {
            let request = reader.read_request().await.unwrap().unwrap();
            assert_eq!(request["method"], "host/session/create");
            assert_eq!(
                request["params"]["initialInput"]["clientUserMessageId"],
                "initial-input"
            );
            assert_eq!(
                request["params"]["initialInput"]["input"]
                    .as_array()
                    .unwrap()
                    .len(),
                2
            );
            assert_eq!(request["params"]["initialInput"]["effort"], "high");
            assert_eq!(request["params"]["initialInput"]["serviceTier"], "fast");
            let mut submission = request["params"]["initialInput"].clone();
            submission["threadId"] = json!({"provider":"codex","id":"admitted"});
            submission["model"] = request["params"]["model"].clone();
            let tier = submission
                .as_object_mut()
                .unwrap()
                .remove("serviceTier")
                .unwrap();
            submission["serviceTierForTurn"] = tier;
            writer.reply(&request, json!({"result":{"thread":{"id":{"provider":"codex","id":"admitted"},"cwd":"/project","status":"preparing","preparation":{"submission":submission,"status":"preparing","stage":"workspace"}}}})).await.unwrap();
        };
        let (opened, ()) = tokio::join!(operation.run((), &mut context), reply);
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
            "initial-input".into(),
            Arc::new(crate::state::PendingSubmission {
                sequence: 0,
                submitted_at_ms: None,
                draft_key: "local".into(),
                draft: operation.draft.clone(),
                turn_id: None,
                after_item_id: None,
                accepted: false,
                delivery_unknown: false,
                queued: false,
            }),
        );
        let effects = operation.apply(&mut snapshot, "", opened.unwrap());
        assert!(snapshot.sessions.pending_submissions["initial-input"].accepted);
        let thread = snapshot.sessions.conversations.values().next().unwrap();
        assert_eq!(
            thread
                .preparation
                .as_ref()
                .unwrap()
                .submission
                .service_tier
                .as_deref(),
            Some("fast")
        );
        let rendered = crate::presentation::conversation::project_conversation(
            crate::localization::UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            thread.clone(),
            &None,
        );
        assert_eq!(rendered.queued.len(), 1);
        assert_eq!(
            rendered.queued[0].data.body.as_deref(),
            Some("initial message")
        );
        let mut accepted = thread.as_ref().clone();
        accepted.preparation = None;
        let rendered = crate::presentation::conversation::project_conversation(
            crate::localization::UiLanguage::Japanese,
            &snapshot.panes.primary().navigation.draft_key,
            &snapshot.sessions.pending_submissions,
            Arc::new(accepted),
            &Some(rendered),
        );
        assert_eq!(
            rendered.queued.len(),
            1,
            "acceptance can precede the provider echo"
        );
        assert_eq!(
            rendered.queued[0].data.body.as_deref(),
            Some("initial message")
        );
        assert!(
            effects.is_empty(),
            "acceptance must not schedule another send"
        );
        client.close().await;
    }

    #[rstest::rstest]
    #[case("ready")]
    #[case("failed")]
    #[case("rejected")]
    #[case("aborted")]
    #[case("disconnected")]
    #[tokio::test]
    async fn only_a_failed_connection_attempt_reports_that_the_host_is_unreachable(
        #[case] outcome: &str,
    ) {
        use crate::{
            presentation::error::ErrorScope,
            transport::{Endpoint, Identity, Relays, Trust},
        };
        use serde_json::json;
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let client = Endpoint::bind(Identity::generate(), Relays::Loopback)
                .await
                .unwrap();
            let host = Endpoint::bind(Identity::generate(), Relays::Loopback)
                .await
                .unwrap();
            let trust = Trust {
                allowed: [client.node_id()].into(),
                ..Default::default()
            };
            let mut initial = Snapshot::default();
            if outcome == "rejected" {
                // A cached scope attaches before the Host answers the introduction.
                initial.connection.storage_scope = format!("{}:cached", host.node_id());
            }
            let draft_key = initial.panes.primary().navigation.draft_key.clone();
            Arc::make_mut(&mut initial.input.drafts).insert(
                draft_key.clone(),
                Arc::new(crate::state::Draft {
                    text: "unsent draft".into(),
                    ..Default::default()
                }),
            );
            let (peer, _old_reader, old_writer) = host_fixture::connect(&initial).await;
            let store = Arc::new(Store::new(peer, initial, None));
            let mut snapshots = store.subscribe();
            drop(old_writer);
            snapshots
                .wait_for(|snapshot| !snapshot.connection.connected)
                .await
                .unwrap();
            let notices = || store.snapshot().error_notices("", ErrorScope::Application);
            assert!(
                notices().is_empty(),
                "losing an established connection, for example while suspended, is not a failure"
            );
            let recovering = tokio::spawn({
                let store = store.clone();
                let endpoint = client.clone();
                let ticket = host.local_ticket();
                async move { store.resume(&endpoint, &ticket, "Fixture device").await }
            });
            let incoming = host
                .accept()
                .await
                .unwrap()
                .unwrap()
                .authorize(&trust)
                .unwrap();
            let (session, mut reader, writer) = host_fixture::accept(incoming).await;
            let scope = reader.read_request().await.unwrap().unwrap();
            assert_eq!(scope["method"], "host/device/identify");
            assert!(notices().is_empty());
            match outcome {
                "ready" => {
                    writer
                        .reply(
                            &scope,
                            json!({"result":{"storageScope":"restored-storage","ticket":""}}),
                        )
                        .await
                        .unwrap();
                    recovering.await.unwrap().unwrap();
                    assert!(store.snapshot().connection.connected);
                    assert!(notices().is_empty());
                }
                "failed" => {
                    writer
                        .reply(
                            &scope,
                            json!({"error":{"code":"unavailable","message":"Host unavailable"}}),
                        )
                        .await
                        .unwrap();
                    assert!(recovering.await.unwrap().is_err());
                    let notices = notices();
                    assert_eq!(notices.len(), 1, "{notices:?}");
                    assert_eq!(notices[0].title, "ホストに接続できません");
                    assert!(
                        notices[0].text.to_string().contains("Host unavailable"),
                        "{notices:?}"
                    );
                }
                "rejected" => {
                    recovering.await.unwrap().unwrap();
                    writer
                        .reply(
                            &scope,
                            json!({"error":{"code":"unavailable","message":"Host unavailable"}}),
                        )
                        .await
                        .unwrap();
                    snapshots
                        .wait_for(|snapshot| !snapshot.connection.connected)
                        .await
                        .unwrap();
                    let notices = notices();
                    assert_eq!(notices.len(), 1, "{notices:?}");
                    assert_eq!(notices[0].title, "ホストに接続できません");
                    assert!(
                        notices[0].text.to_string().contains("Host unavailable"),
                        "{notices:?}"
                    );
                }
                "aborted" => {
                    recovering.abort();
                    assert!(recovering.await.unwrap_err().is_cancelled());
                    assert!(notices().is_empty());
                }
                "disconnected" => {
                    store.disconnect().await.unwrap();
                    assert!(recovering.await.unwrap().is_err());
                    assert!(notices().is_empty(), "a cancelled attempt is not a failure");
                }
                _ => unreachable!(),
            }
            assert_eq!(
                store.snapshot().input.drafts[&draft_key].text,
                "unsent draft"
            );
            store.close().await.unwrap();
            session.close();
            client.close().await;
            host.close().await;
        })
        .await
        .expect("connection recovery did not settle");
    }

    #[tokio::test]
    async fn restoring_offline_does_not_report_a_connection_failure() {
        use crate::presentation::error::ErrorScope;
        let initial = Snapshot {
            connection: crate::state::Connection {
                connected: true,
                ..Default::default()
            },
            panes: crate::state::PaneStates::with_primary(Arc::new(crate::state::PaneState {
                error: Some("previous runtime error".into()),
                ..Default::default()
            })),
            ..Default::default()
        };
        let store = Store::offline(initial, None);
        assert!(!store.snapshot().connection.connected);
        assert!(
            store
                .snapshot()
                .error_notices("", ErrorScope::Application)
                .is_empty()
        );
        store.close().await.unwrap();
        assert!(
            store
                .snapshot()
                .error_notices("", ErrorScope::Application)
                .is_empty()
        );
    }

    #[rstest::rstest]
    #[case("disconnect")]
    #[case("close")]
    #[case("transport loss")]
    #[tokio::test]
    async fn losing_a_connection_publishes_it_without_reporting_a_failure(#[case] action: &str) {
        use crate::presentation::error::ErrorScope;
        let initial = Snapshot::default();
        let (peer, _reader, writer) = host_fixture::connect(&initial).await;
        let store = Store::new(peer, initial, None);
        let mut snapshots = store.subscribe();
        match action {
            "disconnect" => store.disconnect().await.unwrap(),
            "close" => store.close().await.unwrap(),
            "transport loss" => drop(writer),
            _ => unreachable!(),
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            snapshots.wait_for(|snapshot| !snapshot.connection.connected),
        )
        .await
        .expect("the disconnection must be published")
        .unwrap();
        assert!(
            store
                .snapshot()
                .error_notices("", ErrorScope::Application)
                .is_empty()
        );
        store.close().await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn older_dispatched_list_cannot_replace_newer_published_work() {
        use serde_json::json;
        let initial = Snapshot::default();
        let (peer, mut reader, writer) = host_fixture::connect(&initial).await;
        let store = Store::new(peer, initial, None);
        let publications = store.publications.lock().unwrap().clone().unwrap();
        // Dispatch publishes before its command is consumed. Commands can arrive
        // out of order after newer work has already been published.
        let older = apply(
            &publications,
            Event::Intent(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            ))),
        );
        let newer = apply(
            &publications,
            Event::Intent(Intent::ListSessions(op::ListSessions::new(
                Default::default(),
            ))),
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
                    "host/taskActivity/read" => {
                        json!({"revision":0,"statuses":[],"waiting":[],"unread":[],
                        "display":agent_protocol::live_activity::TaskActivitySummary::default().display()})
                    }
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
                    snapshot.lists.threads.as_ref().is_some_and(|page| {
                        page.data
                            .iter()
                            .any(|thread| thread.id.as_ref().is_some_and(|id| id.id == "latest"))
                    }) && snapshot
                        .operations
                        .get(&op::OperationKey::SessionList)
                        .is_none()
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
        Arc::make_mut(&mut initial.input.drafts).insert(
            initial.panes.primary().navigation.draft_key.clone(),
            Arc::new(crate::state::Draft {
                model: Some(agent_protocol::models::ModelRef {
                    provider: agent_protocol::session::ProviderKind::Claude,
                    id: "model".into(),
                }),
                ..Default::default()
            }),
        );
        let store = Store::offline(initial, None);
        let draft_key = store
            .snapshot()
            .panes
            .primary()
            .navigation
            .draft_key
            .clone();
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
                alternate: false,
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
    fn scheduling_a_submission_dates_only_that_input_once() {
        let pending = |submitted_at_ms| {
            Arc::new(crate::state::PendingSubmission {
                sequence: 0,
                submitted_at_ms,
                draft_key: "draft".into(),
                draft: Arc::default(),
                turn_id: None,
                after_item_id: None,
                accepted: false,
                delivery_unknown: false,
                queued: false,
            })
        };
        let mut current = Snapshot {
            sessions: crate::state::Sessions {
                pending_submissions: Arc::new(
                    [
                        ("unknown-time".into(), pending(None)),
                        ("known-time".into(), pending(Some(12_000))),
                        ("new".into(), pending(None)),
                    ]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        let send = |id: &str| {
            Effect::execute(op::StartSubmission {
                provider: crate::session::ProviderKind::Codex,
                draft_key: "draft".into(),
                cwd: None,
                client_user_message_id: id.into(),
                draft: Arc::default(),
            })
        };
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        Scheduled::new(send("new"), &mut current, "", None);
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let submitted_at = current.sessions.pending_submissions["new"]
            .submitted_at_ms
            .unwrap();
        assert!((before..=after).contains(&submitted_at));
        assert_eq!(
            current.sessions.pending_submissions["unknown-time"].submitted_at_ms,
            None
        );
        Scheduled::new(send("known-time"), &mut current, "", None);
        assert_eq!(
            current.sessions.pending_submissions["known-time"].submitted_at_ms,
            Some(12_000)
        );
    }

    #[test]
    // Reading store.snapshot() after a receipt cannot prove subscribers were
    // notified. Keep publication coverage independent of operation behavior.
    fn every_snapshot_field_notifies_subscribers_independently() {
        type Change = fn(&mut Snapshot);
        let changes: &[(&str, Change)] = &[
            ("project_import", |snapshot| {
                snapshot.project_import = Arc::default()
            }),
            ("operations", |snapshot| {
                snapshot.operations = Default::default()
            }),
            ("operation_sequence", |snapshot| {
                snapshot.operation_sequence += 1
            }),
            ("model_preferences", |snapshot| {
                snapshot.catalog.model_preferences.provider =
                    Some(crate::session::ProviderKind::Claude);
            }),
            ("ui_language", |snapshot| {
                snapshot.catalog.ui_language = crate::localization::UiLanguage::English;
            }),
            ("permission_settings", |snapshot| {
                snapshot.catalog.permission_settings = Arc::new(
                    [(
                        agent_protocol::session::ProviderKind::Codex,
                        Arc::new(crate::state::operations::PermissionSettingsState {
                            result: None,
                        }),
                    )]
                    .into(),
                );
            }),
            ("account", |snapshot| {
                snapshot.catalog.account = Arc::default()
            }),
            ("terminals", |snapshot| snapshot.terminals = Arc::default()),
            ("conversations", |snapshot| {
                snapshot.sessions.conversations = Arc::default()
            }),
            ("task_activity", |snapshot| {
                snapshot.lists.task_activity =
                    Some(Arc::new(agent_protocol::live_activity::TaskActivityState {
                        attention: Vec::new(),
                        waiting: Vec::new(),
                        background: Vec::new(),
                        unread: Vec::new(),
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
                snapshot.lists.expanded_projects = Arc::new([("p".into(), 5)].into());
            }),
            ("project_threads", |snapshot| {
                snapshot.lists.project_threads = Arc::new(
                    [(
                        "p".into(),
                        Arc::new(crate::models::ThreadList {
                            revision: 0,
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
                Arc::make_mut(snapshot.panes.primary_mut()).observed_agents =
                    Some(crate::session::SessionRef {
                        provider: crate::session::ProviderKind::Codex,
                        id: "parent".into(),
                    });
            }),
            ("models", |snapshot| {
                snapshot.catalog.models = Arc::default()
            }),
            ("drafts", |snapshot| snapshot.input.drafts = Arc::default()),
            ("pending_submissions", |snapshot| {
                snapshot.sessions.pending_submissions = Arc::default()
            }),
            ("file_drafts", |snapshot| {
                snapshot.files.file_drafts = Arc::default()
            }),
            ("workspace", |snapshot| {
                snapshot.files.workspace = Arc::default()
            }),
            ("navigation", |snapshot| {
                Arc::make_mut(snapshot.panes.primary_mut()).navigation = Arc::default()
            }),
            ("activity", |snapshot| {
                snapshot.lists.activity = Arc::default()
            }),
            ("management", |snapshot| {
                snapshot.connection.management = Arc::default()
            }),
            ("list_query", |snapshot| {
                snapshot.lists.list_query = Arc::default()
            }),
            ("threads", |snapshot| {
                snapshot.lists.threads = Some(Arc::new(crate::models::ThreadList {
                    revision: 0,
                    data: Vec::new(),
                    projects: Vec::new(),
                    has_more: false,
                    has_more_projects: false,
                    limit: 5,
                    project_pages: Default::default(),

                    provider_errors: None,
                }))
            }),
            ("epoch", |snapshot| snapshot.connection.epoch += 1),
            ("connected", |snapshot| snapshot.connection.connected = true),
            ("connection_failure", |snapshot| {
                snapshot.connection.connection_failure = Some("fixture failure".into())
            }),
            ("error", |snapshot| {
                Arc::make_mut(snapshot.panes.primary_mut()).error = Some("fixture failure".into())
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

#[cfg(test)]
mod model_reconnection_tests {
    use super::*;

    #[test]
    fn reconnect_and_reopening_a_project_refresh_held_lists_without_clearing_them() {
        let page = Arc::new(crate::models::ThreadList {
            revision: 10,
            limit: 5,
            data: vec![],
            projects: vec![crate::models::Project {
                id: "p".into(),
                name: "Project".into(),
                ..Default::default()
            }],
            has_more: false,
            has_more_projects: false,
            project_pages: Default::default(),
            provider_errors: None,
        });
        let previous = Snapshot {
            lists: crate::state::Lists {
                threads: Some(page.clone()),
                project_threads: Arc::new([("p".into(), page)].into()),
                unconfirmed_lists: Arc::new(
                    [
                        op::OperationKey::SessionList,
                        op::OperationKey::ProjectList {
                            project_id: "p".into(),
                        },
                    ]
                    .into(),
                ),
                ..Default::default()
            },
            ..Default::default()
        };
        let (connected, effects) = reduce(&previous, "", Event::Connected);
        assert!(connected.lists.threads.is_some());
        assert_eq!(
            effects
                .iter()
                .filter(|effect| effect.operation.key() == Some(op::OperationKey::SessionList))
                .count(),
            1
        );
        let (opened, effects) = reduce(
            &connected,
            "",
            Event::Intent(Intent::SetProjectExpanded {
                project_id: "p".into(),
                expanded: true,
            }),
        );
        assert!(opened.lists.project_threads.contains_key("p"));
        assert_eq!(
            effects
                .iter()
                .filter(|effect| effect.operation.key()
                    == Some(op::OperationKey::ProjectList {
                        project_id: "p".into()
                    }))
                .count(),
            1
        );
    }

    #[test]
    fn reconnect_refreshes_a_cached_catalog_without_clearing_choices() {
        let models = serde_json::from_value(serde_json::json!([
            {"id":"old","model":{"provider":"codex","id":"gpt-6-sol"},
             "displayName":"GPT-6", "settings": { "defaultReasoningEffort":"medium",
             "supportedReasoningEfforts":[]}}
        ]))
        .unwrap();
        let previous = Snapshot {
            catalog: crate::state::Catalog {
                models: Arc::new(models),
                ..Default::default()
            },
            ..Default::default()
        };
        let (mut next, effects) = reduce(&previous, "", Event::Connected);
        assert_eq!(next.catalog.models, previous.catalog.models);
        assert_eq!(
            effects
                .iter()
                .filter(|effect| effect.operation.key()
                    == Some(op::OperationKey::Models {
                        provider: agent_protocol::session::ProviderKind::Codex
                    }))
                .count(),
            1
        );
        use op::Operation;
        op::LoadModels { provider: agent_protocol::session::ProviderKind::Codex }.apply(
            &mut next,
            "",
            agent_protocol::operations::ModelPage {
                data: serde_json::from_value(serde_json::json!([
                    {"id":"new","model":{"provider":"codex","id":"gpt-6.1-sol"},
                     "displayName":"GPT-6.1", "isDefault":true, "settings": { "defaultReasoningEffort":"medium",
                     "supportedReasoningEfforts":[]}}
                ]))
                .unwrap(),
                next_cursor: None,
                provider_errors: None,
            },
        );
        assert_eq!(
            crate::presentation::view::Presentation::from_snapshot(
                Arc::new(next.clone()),
                "".into()
            )
            .unwrap()
            .settings()
            .models_matching(None, "6.1".into())[0]
                .model
                .id,
            "gpt-6.1-sol"
        );
    }
}

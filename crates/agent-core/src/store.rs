//! The single state owner. Independent RPC work publishes completed results.
use crate::{
    client::{
        ClientExt, SessionImage, SubscriptionKind,
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
    Submitted,
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
    subscriptions: Vec<(
        uuid::Uuid,
        SubscriptionKind,
        agent_transport::client::Updates,
    )>,
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
        if let Some(scope) = &effect.subscription {
            use agent_protocol::sync::Scope;
            let generation = operation
                .as_ref()
                .expect("subscriptions have an operation key")
                .1;
            match scope {
                Scope::Conversation(target) => {
                    Arc::make_mut(&mut snapshot.sessions.subscription_generations)
                        .insert(target.clone(), generation);
                }
                Scope::List => {
                    snapshot.lists.generation = generation;
                }
            }
        }
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
                web_link_target: snapshot.catalog.web_link_target,
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
                    let thread = if thread.is_empty() {
                        candidate
                            .input
                            .new_drafts
                            .get(crate::state::WELCOME_DRAFT_SLOT)
                            .map(crate::state::DraftKey::from)
                            .unwrap_or(thread)
                    } else {
                        thread
                    };
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
        && snapshot.catalog.web_link_target == preferences.web_link_target
    {
        return false;
    }
    let snapshot = Arc::make_mut(snapshot);
    snapshot.catalog.model_preferences = preferences.models.clone();
    snapshot.files.preferred_editor = preferences.editor.preferred;
    snapshot.catalog.ui_language = preferences.language;
    snapshot.input.composer_preferences = preferences.composer;
    snapshot.catalog.web_link_target = preferences.web_link_target;
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
            && !matches!(&completed.scheduling, op::Scheduling::LatestHostState(_));
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
        let unused: Vec<_> = {
            let snapshot = updates.borrow().clone();
            subscriptions
                .keys()
                .filter(|id| {
                    !snapshot
                        .sessions
                        .subscriptions
                        .values()
                        .any(|current| current == *id)
                        && snapshot.lists.subscription.as_ref() != Some(*id)
                        && !snapshot
                            .sessions
                            .live_outputs
                            .values()
                            .any(|(current, _)| current == *id)
                })
                .copied()
                .collect()
        };
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
                for (id, kind, stream) in result.subscriptions.drain(..) {
                    let ended = matches!(kind, SubscriptionKind::Conversation | SubscriptionKind::List).then(|| Err(std::io::Error::other("subscription ended")));
                    subscriptions.insert(id, futures_util::stream::try_unfold(stream, move |mut stream| async move {
                        let event = match kind {
                            SubscriptionKind::List => stream.read::<agent_protocol::orchestration::shell::ShellStreamItem>().await?.map(|message| Event::ListStream { subscription_id: id, message: Box::new(message) }),
                            SubscriptionKind::Conversation => stream.read::<agent_protocol::orchestration::projection::ThreadStreamItem>().await?.map(|message| Event::ConversationRows { subscription_id: id, message: Box::new(message) }),
                            SubscriptionKind::Output => stream.read::<agent_protocol::sync::OutputUpdate>().await.ok().flatten().map(|update| Event::CommandOutput { subscription_id: id, update: Box::new(update) }),
                        };
                        Ok(event.map(|event| (event, stream)))
                    }).chain(futures_util::stream::iter(ended)).boxed());
                }
                let scheduling = result.scheduling.clone();
                let pane = result.scope.pane.clone();
                let followups = finish(&updates, result);
                effects.extend(scheduler.complete(&pane, &scheduling, &followups));
                effects.extend(followups);
            }
            Some(()) = preparation_jobs.next(), if !preparation_jobs.is_empty() => {},
            Some((_id, update)) = subscriptions.next(), if !subscriptions.is_empty() => {
                match update {
                    Ok(event) => effects.extend(apply(&updates, event)),
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
    subscription: Option<agent_protocol::sync::Scope<crate::session::SessionRef>>,
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
            subscription: operation.subscription_scope(),
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
    subscriptions: &'a mut Vec<(
        uuid::Uuid,
        SubscriptionKind,
        agent_transport::client::Updates,
    )>,
}
impl Execution<'_> {
    pub(crate) async fn call<O: RpcMethod + Sync>(
        &mut self,
        operation: &O,
    ) -> Result<O::Output, PeerError> {
        if let Some((output, stream, id, kind)) = self.client.open_subscription(operation).await? {
            self.subscriptions.push((id, kind, stream));
            Ok(output)
        } else {
            Ok(self.client.call(operation).await?)
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod model_reconnection_tests {
    use super::*;

    #[test]
    fn reconnect_refreshes_the_catalog_while_project_expansion_uses_cached_rows() {
        let page = Arc::new(crate::models::ThreadList {
            limit: 5,
            data: vec![crate::models::Thread {
                id: Some(
                    crate::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string())
                        .unwrap(),
                ),
                active_provider: Some(agent_protocol::session::ProviderKind::Codex),
                cwd: Some("/project".into()),
                project_id: crate::models::ProjectMembership::Assigned("p".into()),
                ..Default::default()
            }],
            projects: vec![crate::models::Project {
                id: "p".into(),
                name: "Project".into(),
                roots: vec![crate::models::ProjectRoot {
                    path: "/project".into(),
                }],
                ..Default::default()
            }],
            has_more: false,
            has_more_projects: false,
            provider_errors: None,
            ..Default::default()
        });
        let mut previous = Snapshot {
            lists: crate::state::Lists {
                threads: Some(page.clone()),
                project_threads: Arc::new([("p".into(), page)].into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let cached = previous.lists.threads.as_ref().unwrap().as_ref().clone();
        crate::state::list_sync::seed(&mut previous, cached);
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
                .filter(|effect| matches!(
                    effect.operation.key(),
                    Some(
                        op::OperationKey::SessionList
                            | op::OperationKey::ListSearch
                            | op::OperationKey::ListDecorations { .. }
                    )
                ))
                .count(),
            0
        );
    }

    #[test]
    fn retrying_details_reads_a_deferred_item_and_reopens_only_a_failed_live_output() {
        use crate::models::{Item, ItemBody, ItemStatus, Turn, TurnStatus};
        for deferred in [false, true] {
            let session =
                crate::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
            let read = op::ReadItem {
                thread_id: session.clone(),
                turn_id: "turn".into(),
                item_id: "command".into(),
            };
            let mut item = Item::new(
                read.item_id.clone(),
                ItemStatus::Running,
                ItemBody::CommandExecution {
                    command: "pwd".into(),
                    cwd: None,
                    output: String::new(),
                    output_omitted: false,
                    exit_code: None,
                },
            );
            if deferred {
                item.defer();
            }
            let mut state = Snapshot::default();
            state.connection.connected = true;
            crate::test_conversations::open(
                &mut state,
                crate::models::ThreadResponse {
                    thread: crate::models::Thread {
                        id: Some(session.clone()),
                        active_provider: Some(crate::session::ProviderKind::Codex),
                        turns: Some(vec![Arc::new(Turn {
                            id: read.turn_id.clone(),
                            status: TurnStatus::Running,
                            items: Some(vec![Arc::new(item)]),
                            ..Default::default()
                        })]),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
            let output = op::OperationKey::CommandOutput {
                session,
                item: read.item_id.clone(),
            };
            let detail = op::OperationKey::Item { item: read.clone() };
            for key in [output.clone(), detail.clone()] {
                Arc::make_mut(&mut state.operations).insert(
                    key,
                    op::OperationState {
                        generation: 1,
                        phase: op::OperationPhase::Failed {
                            message: "temporarily unavailable".into(),
                        },
                    },
                );
            }
            let (_, effects) = reduce(&state, "", Event::Intent(Intent::ReadItem(read)));
            let keys: Vec<_> = effects
                .iter()
                .filter_map(|effect| effect.operation.key())
                .collect();
            assert_eq!(
                keys.iter().filter(|key| **key == detail).count(),
                usize::from(deferred)
            );
            assert_eq!(
                keys.iter().filter(|key| **key == output).count(),
                usize::from(!deferred)
            );
        }
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

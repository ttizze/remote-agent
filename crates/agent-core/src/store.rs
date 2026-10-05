//! One owner applies device changes and completed network operations.
use crate::{
    peer::PeerError,
    protocol::{self, Call},
    state::*,
    transport,
};
use agent_protocol::{models as m, operations as op, orchestration as rpc};
use agent_transport::client::{Client, Updates};
use orchestration::*;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

#[derive(Debug, Clone, Default, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Outcome {
    #[default]
    Applied,
    StartedThread {
        id: String,
    },
    RemoteHostPaired {
        id: String,
    },
}
pub type Receipt = oneshot::Receiver<Result<Outcome, PeerError>>;
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}
struct Inner {
    sender: mpsc::Sender<OwnerEvent>,
    intents: mpsc::UnboundedSender<OwnerEvent>,
    snapshots: watch::Receiver<Arc<Snapshot>>,
    stop: CancellationToken,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
enum OwnerEvent {
    Dispatch(Intent, oneshot::Sender<Result<Outcome, PeerError>>),
    Attach {
        peer: Arc<Client>,
        host_name: String,
        ticket: transport::Ticket,
        session: transport::Session,
        events: Updates,
        complete: oneshot::Sender<()>,
    },
    Shell(u64, ShellStreamItem),
    Thread(u64, ThreadId, ThreadStreamItem),
    Notification(u64, protocol::Notification),
    Disconnected(u64, String),
    Finished(u64, Box<JobResult>),
    Close(oneshot::Sender<()>),
    Browser(
        crate::browser::BrowserRequest,
        oneshot::Sender<Result<crate::browser::BrowserFrame, PeerError>>,
    ),
    Dictation(String, CancellationToken),
    Transfer(FileTransfer, oneshot::Sender<Result<String, PeerError>>),
    Performance(crate::diagnostics::ConnectionPerformance),
    Resume {
        endpoint: transport::Endpoint,
        ticket: transport::Ticket,
        complete: oneshot::Sender<Option<crate::diagnostics::ConnectionPerformance>>,
    },
}
enum FileTransfer {
    Download {
        source: String,
        destination: String,
    },
    Upload {
        source: String,
        directory: String,
        file_name: String,
    },
}
type PreparedIntent = (Option<Call>, Option<(String, Draft)>, Option<ThreadId>);
struct JobResult {
    call: Call,
    result: Result<Reply, PeerError>,
    complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
    sent: Option<(String, Draft)>,
    launched: Option<ThreadId>,
}
enum Reply {
    Receipt(rpc::DispatchReceipt),
    History(ThreadId, ThreadHistoryPage),
    Item(ThreadId, Option<Box<TurnItem>>),
    Search(Vec<SearchMatch>),
    Models(op::ModelPage),
    Projects(Vec<m::Project>),
    ProjectAdded(String),
    Files(m::FileList),
    File(m::FileContent),
    Review(m::WorkspaceReview),
    TurnDiff(rpc::TurnDiff),
    WorktreeSettings(m::WorktreeSettings),
    Worktrees(Vec<m::Worktree>),
    Accounts(op::Accounts),
    Login(op::AccountLogin),
    HostStatus(m::HostStatus),
    Remotes(Vec<m::RemoteHost>),
    Remote(m::RemoteHost),
    Invitation(m::Invitation),
    Transcription(String),
    Done,
}
struct Network {
    peer: Arc<Client>,
    session: transport::Session,
    ticket: transport::Ticket,
    epoch: u64,
    tasks: Vec<AbortOnDropHandle<()>>,
    thread: Option<AbortOnDropHandle<()>>,
    mutations: BTreeMap<ThreadId, VecDeque<PendingJob>>,
    running_mutations: BTreeSet<ThreadId>,
}
struct PendingJob {
    call: Call,
    complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
    sent: Option<(String, Draft)>,
    launched: Option<ThreadId>,
}
fn mutation_thread(call: &Call) -> Option<ThreadId> {
    match call {
        Call::DispatchCommand(command) => Some(command.thread_id.clone()),
        Call::LaunchThread(launch) => Some(launch.create.thread_id.clone()),
        _ => None,
    }
}
struct Owner {
    state: Snapshot,
    snapshots: watch::Sender<Arc<Snapshot>>,
    sender: mpsc::Sender<OwnerEvent>,
    network: Option<Network>,
    epoch: u64,
    source: CreationSource,
    visited: BTreeMap<ThreadId, Timestamp>,
    rollback_receipts: BTreeMap<CommandId, u64>,
    dictations: BTreeMap<String, CancellationToken>,
}

fn invalid(error: impl std::fmt::Display) -> PeerError {
    PeerError::InvalidMessage(error.to_string())
}
fn now() -> Timestamp {
    Timestamp::from_millis(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
    )
    .expect("current timestamp")
}
fn id(prefix: &str) -> String {
    format!("{prefix}:{}", uuid::Uuid::new_v4())
}
fn command(thread_id: ThreadId, body: CommandBody) -> Command {
    Command {
        command_id: CommandId::new(id("command")).unwrap(),
        thread_id,
        body,
    }
}

impl Store {
    pub async fn download_file(
        &self,
        source: String,
        destination: String,
    ) -> Result<(), PeerError> {
        self.transfer(FileTransfer::Download {
            source,
            destination,
        })
        .await
        .map(|_| ())
    }
    pub async fn upload_file(
        &self,
        source: String,
        directory: String,
        file_name: String,
    ) -> Result<String, PeerError> {
        self.transfer(FileTransfer::Upload {
            source,
            directory,
            file_name,
        })
        .await
    }
    async fn transfer(&self, transfer: FileTransfer) -> Result<String, PeerError> {
        let (sender, receive) = oneshot::channel();
        self.inner
            .sender
            .send(OwnerEvent::Transfer(transfer, sender))
            .await
            .map_err(invalid)?;
        receive.await.map_err(invalid)?
    }
    pub async fn connect(
        endpoint: &transport::Endpoint,
        ticket: &transport::Ticket,
        snapshot: Snapshot,
        invitation: Option<uuid::Uuid>,
    ) -> Result<Self, PeerError> {
        let store = Self::offline(snapshot);
        store.reconnect(endpoint, ticket, invitation).await?;
        Ok(store)
    }
    pub fn offline(snapshot: Snapshot) -> Self {
        Self::offline_for(
            snapshot,
            if cfg!(any(target_os = "ios", target_os = "android")) {
                CreationSource::Mobile
            } else {
                CreationSource::Desktop
            },
        )
    }
    pub fn offline_for(mut snapshot: Snapshot, source: CreationSource) -> Self {
        snapshot.store_id = uuid::Uuid::new_v4().to_string();
        // Mobile starts on the list. A restored selection must not mark a hidden thread read.
        if source == CreationSource::Mobile {
            snapshot.selected_thread = None;
            snapshot.editing_run = None;
        }
        let (sender, mut receiver) = mpsc::channel(64);
        let (intents, mut input) = mpsc::unbounded_channel();
        let (snapshots, updates) = watch::channel(Arc::new(snapshot.clone()));
        let stop = CancellationToken::new();
        let mut owner = Owner {
            state: snapshot,
            snapshots,
            sender: sender.clone(),
            network: None,
            epoch: 0,
            source,
            visited: BTreeMap::new(),
            rollback_receipts: BTreeMap::new(),
            dictations: BTreeMap::new(),
        };
        let stopped = stop.clone();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {biased;_=stopped.cancelled()=>break,event=input.recv()=>match event{Some(e)=>e,None=>break},event=receiver.recv()=>match event{Some(e)=>e,None=>break}};
                if owner.handle(event).await {
                    break;
                }
            }
            if let Some(network) = owner.network.take() {
                network.peer.close().await;
            }
        });
        Self {
            inner: Arc::new(Inner {
                sender,
                intents,
                snapshots: updates,
                stop,
            }),
        }
    }
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.inner.snapshots.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<Snapshot>> {
        self.inner.snapshots.clone()
    }
    pub fn dispatch(&self, intent: Intent) -> Receipt {
        let (sender, receiver) = oneshot::channel();
        if let Err(error) = self
            .inner
            .intents
            .send(OwnerEvent::Dispatch(intent, sender))
        {
            let reason = invalid(&error);
            if let OwnerEvent::Dispatch(_, complete) = error.0 {
                let _ = complete.send(Err(reason));
            }
        }
        receiver
    }
    pub async fn reconnect(
        &self,
        endpoint: &transport::Endpoint,
        ticket: &transport::Ticket,
        invitation: Option<uuid::Uuid>,
    ) -> Result<crate::diagnostics::ConnectionPerformance, PeerError> {
        let started = std::time::Instant::now();
        let session = endpoint.connect(ticket).await.map_err(invalid)?;
        let (peer, events) = session
            .open_peer(Duration::from_secs(30), 32)
            .await
            .map_err(invalid)?;
        let peer = Arc::new(peer);
        if let Some(invitation) = invitation {
            peer.call(&op::Pair { invitation }).await?;
        }
        let host_name = peer.request::<String>(&Call::HostName(m::Empty {})).await?;
        let (complete, receiver) = oneshot::channel();
        self.inner
            .sender
            .send(OwnerEvent::Attach {
                peer: peer.clone(),
                host_name,
                ticket: ticket.clone(),
                session,
                events,
                complete,
            })
            .await
            .map_err(invalid)?;
        receiver.await.map_err(invalid)?;
        // Name is data fetched from the authenticated connection; not an authority token.
        let performance = crate::diagnostics::ConnectionPerformance {
            total_ms: started.elapsed().as_millis() as u64,
            connection_id: peer.diagnostic_id,
            ..Default::default()
        };
        Ok(performance)
    }
    pub async fn resume(
        &self,
        endpoint: &transport::Endpoint,
        ticket: &transport::Ticket,
    ) -> Result<crate::diagnostics::ConnectionPerformance, PeerError> {
        let (complete, result) = oneshot::channel();
        self.inner
            .sender
            .send(OwnerEvent::Resume {
                endpoint: endpoint.clone(),
                ticket: ticket.clone(),
                complete,
            })
            .await
            .map_err(invalid)?;
        if let Some(performance) = result.await.map_err(invalid)? {
            return Ok(performance);
        }
        self.reconnect(endpoint, ticket, None).await
    }
    pub async fn close(&self) -> Result<(), PeerError> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .sender
            .send(OwnerEvent::Close(sender))
            .await
            .map_err(invalid)?;
        receiver.await.map_err(invalid)
    }
    pub async fn browser(
        &self,
        request: crate::browser::BrowserRequest,
    ) -> Result<crate::browser::BrowserFrame, PeerError> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .sender
            .send(OwnerEvent::Browser(request, sender))
            .await
            .map_err(invalid)?;
        receiver.await.map_err(invalid)?
    }
    pub fn prepare_dictation(&self) -> crate::client::DictationPreparation {
        let cancel = CancellationToken::new();
        let recording = id("dictation");
        let _ = self
            .inner
            .intents
            .send(OwnerEvent::Dictation(recording.clone(), cancel.clone()));
        crate::client::DictationPreparation {
            id: recording,
            _cancel: cancel.drop_guard(),
        }
    }
    pub fn record_connection_performance(
        &self,
        performance: crate::diagnostics::ConnectionPerformance,
    ) {
        let _ = self
            .inner
            .sender
            .try_send(OwnerEvent::Performance(performance));
    }
}

impl Owner {
    fn reconcile_rollbacks(&mut self) {
        if self.rollback_receipts.is_empty() {
            return;
        }
        let pending: Vec<_> = self
            .state
            .pending_commands
            .iter()
            .filter(|c| self.rollback_receipts.contains_key(&c.command_id))
            .cloned()
            .collect();
        for command in pending {
            let CommandBody::CheckpointRollback { checkpoint_id, .. } = &command.body else {
                continue;
            };
            let Some(cache) = self.state.threads.get(&command.thread_id) else {
                continue;
            };
            if cache.sequence < self.rollback_receipts[&command.command_id]
                || cache.projection.thread.rollback_request_id.as_ref() == Some(&command.command_id)
            {
                continue;
            }
            let projection = cache.projection.clone();
            if projection.thread.rollback_failure.is_none() {
                let checkpoint = projection
                    .checkpoints
                    .iter()
                    .find(|c| c.id == *checkpoint_id);
                let restored = checkpoint.and_then(|checkpoint| {
                    let before_run = checkpoint.run_id.as_ref().filter(|id| {
                        checkpoint.id
                            == orchestration::checkpoint::before_run_id(&checkpoint.scope_id, id)
                    });
                    let run = before_run
                        .and_then(|id| projection.runs.iter().find(|r| &r.id == id))
                        .or_else(|| {
                            projection
                                .runs
                                .iter()
                                .filter(|r| {
                                    r.status == RunStatus::RolledBack
                                        && r.ordinal > checkpoint.app_run_ordinal.unwrap_or(0)
                                })
                                .min_by_key(|r| r.ordinal)
                        });
                    run.and_then(|run| {
                        projection
                            .messages
                            .iter()
                            .find(|m| m.id == run.user_message_id)
                    })
                });
                if let Some(message) = restored {
                    let mut draft = self.state.draft_for_thread(&command.thread_id);
                    if !draft.text.is_empty() && !message.text.is_empty() {
                        draft.text.push_str("\n\n");
                    }
                    draft.text.push_str(&message.text);
                    self.state
                        .drafts
                        .insert(command.thread_id.to_string(), draft);
                }
            }
            self.rollback_receipts.remove(&command.command_id);
            self.state
                .pending_commands
                .retain(|c| c.command_id != command.command_id);
        }
    }
    fn publish(&mut self) {
        self.state.revision += 1;
        self.snapshots.send_replace(Arc::new(self.state.clone()));
    }
    fn visit_selected(&mut self) {
        let Some(id) = self.state.selected_thread.clone() else {
            return;
        };
        let Some(shell) = self.state.shell.as_ref().and_then(|shell| {
            shell
                .threads
                .iter()
                .chain(&shell.archived_threads)
                .find(|s| s.thread.id == id)
        }) else {
            return;
        };
        let watermark = shell
            .latest_run_completed_at
            .as_ref()
            .map_or(&shell.thread.updated_at, |time| {
                time.max(&shell.thread.updated_at)
            })
            .clone();
        if shell
            .thread
            .last_visited_at
            .as_ref()
            .is_some_and(|visited| visited >= &watermark)
            || self
                .visited
                .get(&id)
                .is_some_and(|visited| visited >= &watermark)
        {
            return;
        }
        if self
            .job(
                Call::DispatchCommand(command(
                    id.clone(),
                    CommandBody::ThreadVisit {
                        visited_at: watermark.clone(),
                    },
                )),
                None,
                None,
                None,
            )
            .is_ok()
        {
            self.visited.insert(id, watermark);
        }
    }
    fn selected(&self) -> Result<ThreadId, PeerError> {
        self.state
            .selected_thread
            .clone()
            .ok_or_else(|| invalid("Open a thread"))
    }
    fn active(&self) -> Option<RunId> {
        self.state
            .projection()
            .and_then(|p| p.runs.iter().find(|r| r.status.is_blocking()))
            .map(|r| r.id.clone())
    }
    fn job(
        &mut self,
        call: Call,
        complete: Option<oneshot::Sender<Result<Outcome, PeerError>>>,
        sent: Option<(String, Draft)>,
        launched: Option<ThreadId>,
    ) -> Result<(), PeerError> {
        if self.network.is_none() {
            return Err(invalid("Connect to the Host"));
        }
        if !self.state.connected {
            return Err(invalid("Connect to the Host"));
        }
        if let Call::DispatchCommand(command) = &call
            && !self
                .state
                .pending_commands
                .iter()
                .any(|old| old.command_id == command.command_id)
        {
            self.state.pending_commands.push(command.clone());
        }
        if let Call::LaunchThread(launch) = &call
            && !self
                .state
                .pending_launches
                .iter()
                .any(|old| old.create.command_id == launch.create.command_id)
        {
            self.state.pending_launches.push(*launch.clone());
        }
        if let Some(thread) = mutation_thread(&call) {
            let network = self.network.as_mut().unwrap();
            if !network.running_mutations.insert(thread.clone()) {
                network
                    .mutations
                    .entry(thread)
                    .or_default()
                    .push_back(PendingJob {
                        call,
                        complete,
                        sent,
                        launched,
                    });
                return Ok(());
            }
        }
        let network = self.network.as_ref().unwrap();
        let epoch = network.epoch;
        let peer = network.peer.clone();
        let sender = self.sender.clone();
        let cancellation = match &call {
            Call::Transcribe(params) => params
                .preparation
                .as_ref()
                .and_then(|id| self.dictations.get(id))
                .cloned(),
            _ => None,
        }
        .unwrap_or_default();
        let task = tokio::spawn(async move {
            let mut delay = Duration::from_millis(250);
            let result = loop {
                let result = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => Err(invalid("Dictation cancelled")),
                    result = execute(&peer, &call) => result,
                };
                if mutation_thread(&call).is_some()
                    && matches!(result, Err(PeerError::RequestTimeout { .. }))
                    && !peer.is_closed()
                {
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(Duration::from_secs(5));
                    continue;
                }
                break result;
            };
            let _ = sender
                .send(OwnerEvent::Finished(
                    epoch,
                    Box::new(JobResult {
                        call,
                        result,
                        complete,
                        sent,
                        launched,
                    }),
                ))
                .await;
        });
        let network = self.network.as_mut().unwrap();
        network.tasks.retain(|t| !t.is_finished());
        network.tasks.push(AbortOnDropHandle::new(task));
        Ok(())
    }
    fn subscribe_thread(&mut self) {
        let Some(network) = self.network.as_mut() else {
            return;
        };
        network.thread.take();
        if let Some(id) = self.state.selected_thread.clone() {
            if let Some(cache) = self.state.threads.get_mut(&id) {
                cache.accessed_at = self.state.revision;
                cache.synchronized = false;
            }
            network.thread = Some(AbortOnDropHandle::new(tokio::spawn(thread_stream(
                network.peer.clone(),
                network.epoch,
                id,
                self.snapshots.subscribe(),
                self.sender.clone(),
            ))));
        }
    }
    fn refresh(&mut self) {
        for call in [
            Call::ListModels(op::ListModels {
                limit: 100,
                cursor: None,
            }),
            Call::ListProjects(m::Empty {}),
            Call::ListAccounts(m::Empty {}),
        ] {
            let _ = self.job(call, None, None, None);
        }
    }
    async fn handle(&mut self, event: OwnerEvent) -> bool {
        match event {
            OwnerEvent::Attach {
                peer,
                host_name,
                ticket,
                session,
                events,
                complete,
            } => {
                if let Some(network) = self.network.take() {
                    tokio::spawn(async move {
                        let peer = network.peer.clone();
                        drop(network);
                        peer.close().await;
                    });
                }
                self.epoch += 1;
                self.state.connected = true;
                self.state.host_name = Some(host_name);
                self.state.error = None;
                self.state.shell_synchronized = false;
                let epoch = self.epoch;
                let tasks = vec![
                    AbortOnDropHandle::new(tokio::spawn(shell_stream(
                        peer.clone(),
                        epoch,
                        self.snapshots.subscribe(),
                        self.sender.clone(),
                    ))),
                    AbortOnDropHandle::new(tokio::spawn(notifications(
                        events,
                        epoch,
                        self.sender.clone(),
                    ))),
                ];
                self.network = Some(Network {
                    peer,
                    session,
                    ticket,
                    epoch,
                    tasks,
                    thread: None,
                    mutations: BTreeMap::new(),
                    running_mutations: BTreeSet::new(),
                });
                self.subscribe_thread();
                self.refresh();
                for command in self.state.pending_commands.clone() {
                    let _ = self.job(Call::DispatchCommand(command), None, None, None);
                }
                for launch in self.state.pending_launches.clone() {
                    let thread = launch.create.thread_id.clone();
                    let draft = Draft {
                        text: launch.input.text.clone(),
                        ..self.state.default_draft.clone()
                    };
                    let key = format!(
                        "new:{}",
                        match &launch.create.body {
                            CommandBody::ThreadCreate { project_id, .. } => project_id.as_str(),
                            _ => "bex:chats",
                        }
                    );
                    let _ = self.job(
                        Call::LaunchThread(Box::new(launch)),
                        None,
                        Some((key, draft)),
                        Some(thread),
                    );
                }
                self.publish();
                let _ = complete.send(());
                return false;
            }
            OwnerEvent::Dispatch(intent, complete) => {
                if let Err((error, complete)) = self.intent(intent, complete) {
                    self.state.error = Some(crate::presentation::error::error_message(
                        &error.to_string(),
                    ));
                    self.publish();
                    let _ = complete.send(Err(error));
                }
                return false;
            }
            OwnerEvent::Shell(epoch, item) if epoch == self.epoch => {
                let selected = self.state.selected_thread.clone();
                crate::sync::shell(&mut self.state, item, &now());
                if selected != self.state.selected_thread {
                    self.subscribe_thread();
                }
                self.visit_selected();
            }
            OwnerEvent::Thread(epoch, id, item) if epoch == self.epoch => {
                crate::sync::thread(&mut self.state, &id, item);
                self.reconcile_rollbacks();
                self.visit_selected();
            }
            OwnerEvent::Notification(epoch, notification) if epoch == self.epoch => {
                self.notification(notification)
            }
            OwnerEvent::Disconnected(epoch, error) if epoch == self.epoch => {
                self.state.connected = false;
                self.state.error = Some(error);
                self.state.shell_synchronized = false;
                for terminal in self.state.terminals.values_mut() {
                    if matches!(
                        terminal.phase,
                        TerminalPhase::Starting | TerminalPhase::Running
                    ) {
                        terminal.phase = TerminalPhase::Suspended;
                    }
                }
            }
            OwnerEvent::Finished(epoch, result) if epoch == self.epoch => {
                self.finished(*result);
                return false;
            }
            OwnerEvent::Resume {
                endpoint,
                ticket,
                complete,
            } => {
                let candidate = self
                    .network
                    .as_ref()
                    .filter(|network| {
                        self.state.connected
                            && !network.peer.is_closed()
                            && network.session.uses_endpoint(&endpoint)
                            && network.ticket == ticket
                    })
                    .map(|network| network.peer.clone());
                tokio::spawn(async move {
                    let reused = if let Some(peer) = candidate {
                        let healthy = tokio::time::timeout(
                            Duration::from_secs(2),
                            peer.request::<m::HostStatus>(&Call::HostStatus(m::Empty {})),
                        )
                        .await
                        .is_ok_and(|result| result.is_ok());
                        healthy.then_some(crate::diagnostics::ConnectionPerformance {
                            reused: true,
                            connection_id: peer.diagnostic_id,
                            ..Default::default()
                        })
                    } else {
                        None
                    };
                    let _ = complete.send(reused);
                });
                return false;
            }
            OwnerEvent::Close(complete) => {
                if let Some(network) = self.network.take() {
                    tokio::spawn(async move {
                        let peer = network.peer.clone();
                        drop(network);
                        peer.close().await;
                        let _ = complete.send(());
                    });
                } else {
                    let _ = complete.send(());
                }
                self.state.connected = false;
                self.publish();
                return true;
            }
            OwnerEvent::Browser(request, complete) => {
                if let Some(network) = self.network.as_mut() {
                    let peer = network.peer.clone();
                    network
                        .tasks
                        .push(AbortOnDropHandle::new(tokio::spawn(async move {
                            let result = match request.validate() {
                                Ok(()) => peer.request(&Call::Browser(request)).await,
                                Err(e) => Err(invalid(e)),
                            };
                            let _ = complete.send(result);
                        })));
                } else {
                    let _ = complete.send(Err(invalid("Connect to the Host")));
                }
            }
            OwnerEvent::Dictation(id, cancel) => {
                self.dictations.retain(|_, token| !token.is_cancelled());
                self.dictations.insert(id.clone(), cancel.clone());
                if let Some(network) = &self.network {
                    tokio::spawn(crate::client::prepare_dictation(
                        network.peer.clone(),
                        id,
                        cancel,
                    ));
                }
            }
            OwnerEvent::Transfer(transfer, complete) => {
                if let Some(network) = self.network.as_mut() {
                    let peer = network.peer.clone();
                    let session = network.session.clone();
                    network.tasks.retain(|task| !task.is_finished());
                    network
                        .tasks
                        .push(AbortOnDropHandle::new(tokio::spawn(async move {
                            let open = || async {
                                session.open_stream().await.map_err(std::io::Error::other)
                            };
                            let result = match transfer {
                                FileTransfer::Download {
                                    source,
                                    destination,
                                } => agent_transport::transfers::download_file(
                                    &peer,
                                    open,
                                    std::path::Path::new(&source),
                                    std::path::Path::new(&destination),
                                )
                                .await
                                .map(|_| destination),
                                FileTransfer::Upload {
                                    source,
                                    directory,
                                    file_name,
                                } => agent_transport::transfers::upload_file(
                                    &peer,
                                    open,
                                    std::path::Path::new(&source),
                                    std::path::Path::new(&directory),
                                    &file_name,
                                )
                                .await
                                .map(|file| file.path),
                            }
                            .map_err(invalid);
                            let _ = complete.send(result);
                        })));
                } else {
                    let _ = complete.send(Err(invalid("Connect to the Host")));
                }
            }
            OwnerEvent::Performance(performance) => {
                if let Some(network) = &self.network {
                    let peer = network.peer.clone();
                    tokio::spawn(async move {
                        peer.collect_connection_diagnostics(performance).await;
                    });
                }
            }
            _ => {}
        }
        self.publish();
        false
    }
    fn intent(
        &mut self,
        intent: Intent,
        complete: oneshot::Sender<Result<Outcome, PeerError>>,
    ) -> Result<(), (PeerError, oneshot::Sender<Result<Outcome, PeerError>>)> {
        if let Intent::PairRemoteHost { invitation, name } = &intent {
            let Some(network) = self.network.as_mut() else {
                return Err((invalid("Connect to the Host"), complete));
            };
            if now().millis() / 1000 >= invitation.expires_at as i64 {
                return Err((invalid("Invitation expired"), complete));
            }
            let ticket = match invitation.endpoint.parse::<transport::Ticket>() {
                Ok(ticket) => ticket,
                Err(error) => return Err((invalid(error), complete)),
            };
            let session = network.session.clone();
            let invitation = invitation.invitation;
            let peer = network.peer.clone();
            let epoch = network.epoch;
            let sender = self.sender.clone();
            let call = Call::RegisterRemote(op::RegisterRemoteHost {
                ticket: ticket.to_string(),
                name: name.clone(),
            });
            network
                .tasks
                .push(AbortOnDropHandle::new(tokio::spawn(async move {
                    let result =
                        match crate::client::pair_remote(&session, &ticket, invitation).await {
                            Ok(()) => execute(&peer, &call).await,
                            Err(error) => Err(error),
                        };
                    let _ = sender
                        .send(OwnerEvent::Finished(
                            epoch,
                            Box::new(JobResult {
                                call,
                                result,
                                complete: Some(complete),
                                sent: None,
                                launched: None,
                            }),
                        ))
                        .await;
                })));
            return Ok(());
        }

        let prepared = self.prepare(intent);
        let (call, sent, launched) = match prepared {
            Ok(value) => value,
            Err(error) => return Err((error, complete)),
        };
        if let Some(call) = call {
            if mutation_thread(&call).is_some()
                && self.state.pending_commands.len() + self.state.pending_launches.len() >= 2048
            {
                return Err((
                    invalid("Too many pending commands; wait for the Host"),
                    complete,
                ));
            }
            // Keep the receipt available if the request cannot be scheduled.
            if self.network.is_none() || !self.state.connected {
                if let Call::DispatchCommand(command) = &call
                    && matches!(
                        command.body,
                        CommandBody::ThreadModelSelectionSet { .. }
                            | CommandBody::ProviderSwitch { .. }
                            | CommandBody::ThreadRuntimeModeSet { .. }
                            | CommandBody::ThreadInteractionModeSet { .. }
                    )
                {
                    self.state.pending_commands.push(command.clone());
                    self.publish();
                    let _ = complete.send(Ok(Outcome::Applied));
                    return Ok(());
                }
                return Err((invalid("Connect to the Host"), complete));
            }
            self.job(call, Some(complete), sent, launched)
                .expect("connection checked");
        } else {
            self.publish();
            let _ = complete.send(Ok(Outcome::Applied));
            return Ok(());
        }
        self.publish();
        Ok(())
    }
    fn prepare(&mut self, intent: Intent) -> Result<PreparedIntent, PeerError> {
        let mut body = None;
        let mut target = self.state.selected_thread.clone();
        let mut sent = None;
        let mut launched = None;
        let timestamp = now();
        let call = match intent {
            Intent::LeaveThread => {
                self.state.selected_thread = None;
                self.state.editing_run = None;
                self.state.workspace.review = None;
                self.state.workspace.diff_request = None;
                self.state.workspace.directory = None;
                self.state.workspace.file = None;
                self.state.workspace.requested_directory = None;
                self.state.workspace.requested_file = None;
                self.subscribe_thread();
                None
            }
            Intent::MovePinned { thread_id, up } => {
                let mut ids = crate::presentation::shelves(&self.state, &timestamp, 10)
                    .into_iter()
                    .find(|s| s.kind == crate::presentation::ShelfKind::Pinned)
                    .map(|s| s.rows.into_iter().map(|row| row.id).collect::<Vec<_>>())
                    .unwrap_or_default();
                let index = ids
                    .iter()
                    .position(|id| id == &thread_id)
                    .ok_or_else(|| invalid("Pinned thread is unavailable"))?;
                let other = if up {
                    index.checked_sub(1)
                } else {
                    index.checked_add(1).filter(|i| *i < ids.len())
                };
                if let Some(other) = other {
                    ids.swap(index, other);
                    return self.prepare(Intent::ReorderPinned {
                        thread_id,
                        before_thread_id: ids.get(other + 1).cloned(),
                    });
                }
                None
            }
            Intent::ReorderPinned {
                thread_id,
                before_thread_id,
            } => {
                let mut thread_ids = crate::presentation::shelves(&self.state, &timestamp, 10)
                    .into_iter()
                    .find(|s| s.kind == crate::presentation::ShelfKind::Pinned)
                    .map(|s| s.rows.into_iter().map(|row| row.id).collect::<Vec<_>>())
                    .unwrap_or_default();
                let from = thread_ids
                    .iter()
                    .position(|id| id == &thread_id)
                    .ok_or_else(|| invalid("Pinned thread is unavailable"))?;
                if before_thread_id.as_ref() == Some(&thread_id) {
                    return Ok((None, None, None));
                }
                thread_ids.remove(from);
                let to = match before_thread_id {
                    Some(id) => thread_ids
                        .iter()
                        .position(|current| current == &id)
                        .ok_or_else(|| invalid("Pinned list changed; try again"))?,
                    None => thread_ids.len(),
                };
                thread_ids.insert(to, thread_id.clone());
                let keys = self
                    .state
                    .shell
                    .as_ref()
                    .map(|s| {
                        s.threads
                            .iter()
                            .filter(|s| s.thread.pinned_at.is_some())
                            .map(|s| (s.thread.id.to_string(), s.thread.pin_order_key.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                let assignments = crate::ordering::reorder(&thread_ids, &keys, &thread_id);
                if assignments.len()
                    + self.state.pending_commands.len()
                    + self.state.pending_launches.len()
                    > 2048
                {
                    return Err(invalid("Wait for pending commands"));
                }
                for (id, key) in assignments {
                    self.job(
                        Call::DispatchCommand(command(
                            ThreadId::new(id).map_err(invalid)?,
                            CommandBody::ThreadPinReorder { order_key: key },
                        )),
                        None,
                        None,
                        None,
                    )?;
                }
                None
            }
            Intent::DiscardPending { command_id } => {
                let id = CommandId::new(command_id).map_err(invalid)?;
                if self.state.uncertain_commands.remove(&id) {
                    self.state
                        .pending_commands
                        .retain(|command| command.command_id != id);
                    self.state
                        .pending_launches
                        .retain(|launch| launch.create.command_id != id);
                    self.rollback_receipts.remove(&id);
                    if let Some(network) = &mut self.network {
                        for queue in network.mutations.values_mut() {
                            queue.retain(|job| match &job.call {
                                Call::DispatchCommand(c) => c.command_id != id,
                                Call::LaunchThread(l) => l.create.command_id != id,
                                _ => true,
                            });
                        }
                    }
                }
                None
            }
            Intent::Transcribe {
                draft_key,
                preparation,
                audio,
            } => {
                sent = Some((
                    draft_key.clone(),
                    self.state
                        .drafts
                        .get(&draft_key)
                        .cloned()
                        .unwrap_or_else(|| self.state.current_draft()),
                ));
                Some(Call::Transcribe(op::Transcribe { preparation, audio }))
            }
            Intent::OpenThread { thread_id } => {
                let id = ThreadId::new(thread_id).map_err(invalid)?;
                self.state.selected_thread = Some(id.clone());
                self.state.editing_run = None;
                self.state.workspace.review = None;
                self.state.workspace.diff_request = None;
                self.state.workspace.directory = None;
                self.state.workspace.file = None;
                self.state.workspace.requested_directory = None;
                self.state.workspace.requested_file = None;
                self.subscribe_thread();
                self.visited.remove(&id);
                target = Some(id);
                self.visit_selected();
                None
            }
            Intent::NewThread { project_id } => {
                self.state.selected_thread = None;
                self.state.selected_project = project_id;
                self.state.editing_run = None;
                self.state.workspace.review = None;
                self.state.workspace.diff_request = None;
                self.state.workspace.directory = None;
                self.state.workspace.file = None;
                self.state.workspace.requested_directory = None;
                self.state.workspace.requested_file = None;
                self.subscribe_thread();
                None
            }
            Intent::FilterProject { project_id } => {
                self.state.selected_project = project_id;
                None
            }
            Intent::Search { query } => {
                self.state.search = query.clone();
                self.state.search_matches.clear();
                if self.state.connected && (2..=200).contains(&query.trim().chars().count()) {
                    Some(Call::SearchThreads(rpc::SearchThreads {
                        query: query.trim().into(),
                        limit: 50,
                    }))
                } else {
                    None
                }
            }
            Intent::EditDraft { text, base_text } => {
                let mut draft = self.state.current_draft();
                draft.text = text;
                if let Some(base) = base_text {
                    draft.text = crate::presentation::merge_draft_text(
                        base,
                        draft.text,
                        self.state.current_draft().text,
                    );
                }
                self.state.drafts.insert(self.state.draft_key(), draft);
                None
            }
            Intent::Send { behavior } => {
                if self.state.draft_pending() {
                    return Ok((None, None, None));
                }
                if self.source == CreationSource::Desktop
                    && crate::presentation::conversation(&self.state, &now())
                        .composer
                        .plan_follow_up
                    && behavior == SendBehavior::Default
                    && self.state.editing_run.is_none()
                {
                    return self.prepare(Intent::PlanFollowUp { new_thread: false });
                }
                let slash = self.state.current_draft().text.trim().to_ascii_lowercase();
                if matches!(slash.as_str(), "/plan" | "/default") {
                    let interaction_mode = if slash == "/plan" {
                        InteractionMode::Plan
                    } else {
                        InteractionMode::Default
                    };
                    let key = self.state.draft_key();
                    let mut draft = self.state.current_draft();
                    draft.text.clear();
                    draft.interaction_mode = interaction_mode.as_str().into();
                    self.state.drafts.insert(key, draft);
                    body = target
                        .as_ref()
                        .map(|_| CommandBody::ThreadInteractionModeSet { interaction_mode });
                    return Ok((
                        body.map(|body| {
                            Call::DispatchCommand(command(target.expect("selected thread"), body))
                        }),
                        None,
                        None,
                    ));
                }
                if target.is_none() && self.state.pending_launches.iter().any(|launch|matches!(&launch.create.body,CommandBody::ThreadCreate{project_id,..} if project_id.as_str()==self.state.selected_project.as_deref().unwrap_or("bex:chats"))) {return Err(invalid("Thread is being created"))}
                if self.state.editing_run.is_some() {
                    return self.prepare(Intent::Queue {
                        action: QueueAction::SaveEdit,
                    });
                }
                let draft = self.state.current_draft();
                let mode = if behavior == SendBehavior::Default && target.is_some() {
                    DispatchMode::QueueAfterActive
                } else {
                    crate::presentation::dispatch_mode(self.active().as_ref(), behavior)
                };
                let input = crate::commands::message(
                    &draft,
                    MessageId::new(id("message")).unwrap(),
                    mode,
                    self.source,
                )
                .map_err(invalid)?;
                sent = Some((self.state.draft_key(), draft.clone()));
                if target.is_some() {
                    body = Some(CommandBody::MessageDispatch(input));
                    None
                } else {
                    let thread_id = ThreadId::new(id("thread")).unwrap();
                    let title = draft
                        .text
                        .lines()
                        .map(str::trim)
                        .find(|line| !line.is_empty())
                        .unwrap_or("New thread")
                        .chars()
                        .take(100)
                        .collect();
                    let create = command(
                        thread_id.clone(),
                        CommandBody::ThreadCreate {
                            created_by: CreatedBy::User,
                            creation_source: self.source,
                            project_id: ProjectId::new(
                                self.state
                                    .selected_project
                                    .clone()
                                    .unwrap_or_else(|| "bex:chats".into()),
                            )
                            .map_err(invalid)?,
                            title,
                            model_selection: draft.selection().map_err(invalid)?,
                            runtime_mode: crate::commands::runtime_mode(&draft.runtime_mode)
                                .map_err(invalid)?,
                            interaction_mode: crate::commands::interaction_mode(
                                &draft.interaction_mode,
                            )
                            .map_err(invalid)?,
                            branch: None,
                            worktree_path: None,
                        },
                    );
                    launched = Some(thread_id);
                    Some(Call::LaunchThread(Box::new(rpc::LaunchThread {
                        create,
                        input,
                    })))
                }
            }
            Intent::PlanFollowUp { new_thread } => {
                if self.state.draft_pending() {
                    return Ok((None, None, None));
                }
                let p = self
                    .state
                    .projection()
                    .ok_or_else(|| invalid("Thread not loaded"))?
                    .clone();
                let plan = crate::presentation::actionable_plan(
                    &p.plans,
                    p.thread.interaction_mode,
                    p.runs.iter().any(|r| r.status.is_blocking()),
                )
                .ok_or_else(|| invalid("No actionable plan"))?;
                let PlanBody::ProposedPlan { markdown } = &plan.body else {
                    unreachable!()
                };
                let original = self.state.current_draft();
                let mut draft = original.clone();
                let (text, mode, implement) =
                    crate::commands::plan_follow_up(&draft.text, markdown, new_thread);
                draft.text = text;
                let mut input = crate::commands::message(
                    &draft,
                    MessageId::new(id("message")).unwrap(),
                    DispatchMode::StartImmediately,
                    self.source,
                )
                .map_err(invalid)?;
                if implement {
                    input.source_plan_ref = Some(SourcePlanRef {
                        thread_id: p.thread.id.clone(),
                        plan_id: plan.id.clone(),
                    });
                }
                sent = Some((self.state.draft_key(), original));
                if new_thread {
                    let child = ThreadId::new(id("thread")).unwrap();
                    launched = Some(child.clone());
                    let title = markdown
                        .lines()
                        .find_map(|line| {
                            line.trim()
                                .strip_prefix('#')
                                .map(|line| line.trim_start_matches('#').trim())
                        })
                        .filter(|s| !s.is_empty())
                        .map_or("Implement plan".into(), |s| format!("Implement {s}"));
                    Some(Call::LaunchThread(Box::new(rpc::LaunchThread {
                        create: command(
                            child,
                            CommandBody::ThreadCreate {
                                created_by: CreatedBy::User,
                                creation_source: self.source,
                                project_id: p.thread.project_id,
                                title,
                                model_selection: draft.selection().map_err(invalid)?,
                                runtime_mode: crate::commands::runtime_mode(
                                    &self.state.default_draft.runtime_mode,
                                )
                                .map_err(invalid)?,
                                interaction_mode: mode,
                                branch: p.thread.branch,
                                worktree_path: p.thread.worktree_path,
                            },
                        ),
                        input,
                    })))
                } else {
                    self.job(
                        Call::DispatchCommand(command(
                            p.thread.id,
                            CommandBody::ThreadInteractionModeSet {
                                interaction_mode: mode,
                            },
                        )),
                        None,
                        None,
                        None,
                    )?;
                    if let Some(d) = self.state.drafts.get_mut(&self.state.draft_key()) {
                        d.interaction_mode = mode.as_str().into();
                    }
                    body = Some(CommandBody::MessageDispatch(input));
                    None
                }
            }
            Intent::Fork {
                source_thread_id,
                run_id,
            } => {
                let source = ThreadId::new(source_thread_id).map_err(invalid)?;
                if self.state.context_pending(&source) {
                    return Ok((None, None, None));
                }
                target = Some(source);
                let child =
                    ThreadId::new(format!("thread:{}", uuid::Uuid::new_v4())).map_err(invalid)?;
                body = Some(CommandBody::ThreadFork {
                    target_thread_id: child.clone(),
                    source_point: ForkPoint::Run {
                        run_id: RunId::new(run_id).map_err(invalid)?,
                    },
                    title: None,
                    created_by: CreatedBy::User,
                    creation_source: self.source,
                });
                sent = Some((self.state.draft_key(), self.state.current_draft()));
                launched = Some(child);
                None
            }
            Intent::MergeBack => {
                let projection = self
                    .state
                    .projection()
                    .ok_or_else(|| invalid("Thread not loaded"))?;
                if self.state.context_pending(&projection.thread.id) {
                    return Ok((None, None, None));
                }
                let source = orchestration::context::merge_back_run(&projection.runs)
                    .ok_or_else(|| invalid("Wait for the latest run to finish"))?
                    .id
                    .clone();
                let parent = projection
                    .thread
                    .lineage
                    .parent_thread_id
                    .clone()
                    .ok_or_else(|| invalid("Thread is not a fork"))?;
                sent = Some((self.state.draft_key(), self.state.current_draft()));
                launched = Some(parent.clone());
                body = Some(CommandBody::ThreadMergeBack {
                    target_thread_id: parent,
                    source_point: ForkPoint::Run { run_id: source },
                    created_by: CreatedBy::User,
                });
                None
            }
            Intent::Rollback {
                checkpoint_id,
                restore_files,
            } => {
                let projection = self
                    .state
                    .projection()
                    .ok_or_else(|| invalid("No thread selected"))?;
                let checkpoint = projection
                    .checkpoints
                    .iter()
                    .find(|c| c.id.as_str() == checkpoint_id)
                    .ok_or_else(|| invalid("Checkpoint unavailable"))?;
                body = Some(CommandBody::CheckpointRollback {
                    scope_id: checkpoint.scope_id.clone(),
                    checkpoint_id: checkpoint.id.clone(),
                    restore_files,
                });
                None
            }
            Intent::Stop => {
                body = Some(CommandBody::RunInterrupt {
                    run_id: self.active().ok_or_else(|| invalid("No active run"))?,
                    reason: Some("user".into()),
                    hold_queue: true,
                });
                None
            }
            Intent::Thread { thread_id, action } => {
                target = Some(ThreadId::new(thread_id).map_err(invalid)?);
                body = Some(crate::commands::thread_action(&action, &timestamp).map_err(invalid)?);
                None
            }
            Intent::Queue { action } => {
                match action {
                    QueueAction::Resume => body = Some(CommandBody::QueueResume),
                    QueueAction::Cancel { run_id } => {
                        body = Some(CommandBody::QueuedRunCancel {
                            run_id: RunId::new(run_id).map_err(invalid)?,
                        })
                    }
                    QueueAction::Steer { run_id } => {
                        body = Some(CommandBody::QueuedMessagePromoteToSteer {
                            queued_run_id: RunId::new(run_id).map_err(invalid)?,
                            target_run_id: self.active().ok_or_else(|| invalid("No active run"))?,
                        })
                    }
                    QueueAction::Edit { run_id } => {
                        let id = RunId::new(run_id).map_err(invalid)?;
                        let projection = self
                            .state
                            .projection()
                            .ok_or_else(|| invalid("Thread is loading"))?;
                        let run = projection
                            .runs
                            .iter()
                            .find(|r| r.id == id && r.status == RunStatus::Queued)
                            .ok_or_else(|| invalid("Queued run is unavailable"))?;
                        let text = projection
                            .messages
                            .iter()
                            .find(|m| m.id == run.user_message_id)
                            .map(|m| m.text.clone())
                            .unwrap_or_default();
                        let mut draft = self.state.current_draft();
                        draft.text = text;
                        self.state.editing_run = Some(id);
                        self.state.drafts.insert(self.state.draft_key(), draft);
                    }
                    QueueAction::SaveEdit => {
                        if self.state.draft_pending() {
                            return Ok((None, None, None));
                        }
                        let draft = self.state.current_draft();
                        body = Some(CommandBody::QueuedRunEdit {
                            run_id: self
                                .state
                                .editing_run
                                .clone()
                                .ok_or_else(|| invalid("No queued message is being edited"))?,
                            text: draft.text.clone(),
                            context: None,
                            attachments: None,
                        });
                        sent = Some((self.state.draft_key(), draft));
                    }
                    QueueAction::CancelEdit => {
                        self.state.drafts.remove(&self.state.draft_key());
                        self.state.editing_run = None;
                    }
                    QueueAction::Reorder { run_ids } => {
                        if run_ids.len()
                            + self.state.pending_commands.len()
                            + self.state.pending_launches.len()
                            > 2048
                        {
                            return Err(invalid("Wait for pending commands"));
                        }
                        let ordered = run_ids
                            .iter()
                            .map(|id| RunId::new(id.clone()).map_err(invalid))
                            .collect::<Result<Vec<_>, _>>()?;
                        for (index, run) in ordered.iter().enumerate().rev() {
                            self.job(
                                Call::DispatchCommand(command(
                                    self.selected()?,
                                    CommandBody::QueuedRunReorder {
                                        run_id: run.clone(),
                                        before_run_id: ordered.get(index + 1).cloned(),
                                    },
                                )),
                                None,
                                None,
                                None,
                            )?;
                        }
                    }
                }
                None
            }
            Intent::SetModel {
                instance_id,
                model,
                effort,
                service_tier,
            } => {
                let mut draft = self.state.current_draft();
                let switched = self.state.projection().map_or_else(
                    || {
                        self.state
                            .shell
                            .as_ref()
                            .and_then(|shell| {
                                shell.threads.iter().find(|s| {
                                    Some(&s.thread.id) == self.state.selected_thread.as_ref()
                                })
                            })
                            .map_or(draft.instance_id != instance_id, |s| {
                                s.thread.provider_instance_id.as_str() != instance_id
                            })
                    },
                    |p| p.thread.provider_instance_id.as_str() != instance_id,
                );
                draft.instance_id = instance_id;
                draft.model = model;
                draft.effort = effort;
                draft.service_tier = service_tier;
                let selection = draft.selection().map_err(invalid)?;
                self.state.default_draft = draft.clone();
                self.state.default_draft.text.clear();
                self.state.drafts.insert(self.state.draft_key(), draft);
                if target.is_some() {
                    body = Some(if switched {
                        CommandBody::ProviderSwitch {
                            model_selection: selection,
                        }
                    } else {
                        CommandBody::ThreadModelSelectionSet {
                            model_selection: selection,
                        }
                    });
                }
                None
            }
            Intent::SetRuntimeMode { mode } => {
                let runtime_mode = crate::commands::runtime_mode(&mode).map_err(invalid)?;
                let mut draft = self.state.current_draft();
                draft.runtime_mode = runtime_mode.as_str().into();
                self.state.drafts.insert(self.state.draft_key(), draft);
                if target.is_some() {
                    body = Some(CommandBody::ThreadRuntimeModeSet { runtime_mode });
                }
                None
            }
            Intent::SetInteractionMode { mode } => {
                let interaction_mode = crate::commands::interaction_mode(&mode).map_err(invalid)?;
                let mut draft = self.state.current_draft();
                draft.interaction_mode = interaction_mode.as_str().into();
                self.state.drafts.insert(self.state.draft_key(), draft);
                if target.is_some() {
                    body = Some(CommandBody::ThreadInteractionModeSet { interaction_mode });
                }
                None
            }
            Intent::RespondApproval {
                request_id,
                decision,
            } => {
                body = Some(CommandBody::RuntimeRequestRespond {
                    request_id: RuntimeRequestId::new(request_id).map_err(invalid)?,
                    decision: Some(crate::commands::approval_decision(&decision).map_err(invalid)?),
                    answers: None,
                });
                None
            }
            Intent::RespondQuestions {
                request_id,
                answers,
            } => {
                let row = self
                    .state
                    .projection()
                    .map(crate::presentation::timeline)
                    .unwrap_or_default()
                    .into_iter()
                    .find(|row| row.request_id.as_deref() == Some(&request_id))
                    .ok_or_else(|| invalid("Question is unavailable"))?;
                if let Some(error) =
                    crate::presentation::question_error(row.questions, answers.clone())
                {
                    return Err(invalid(error));
                }
                body = Some(CommandBody::RuntimeRequestRespond {
                    request_id: RuntimeRequestId::new(request_id).map_err(invalid)?,
                    decision: None,
                    answers: Some(crate::commands::question_answers(&answers)),
                });
                None
            }
            Intent::DismissInput { request_id } => {
                body = Some(CommandBody::ThreadUserInputDismiss {
                    request_id: RuntimeRequestId::new(request_id).map_err(invalid)?,
                });
                None
            }
            Intent::LoadHistory => {
                let id = self.selected()?;
                let cursor = self
                    .state
                    .threads
                    .get(&id)
                    .and_then(|c| c.history_cursor.clone())
                    .ok_or_else(|| invalid("No earlier history"))?;
                Some(Call::ReadThreadHistory(rpc::ReadThreadHistory {
                    thread_id: id,
                    cursor: Some(cursor),
                    limit: 200,
                }))
            }
            Intent::LoadItem { item_id } => Some(Call::GetTurnItem(rpc::GetTurnItem {
                thread_id: self.selected()?,
                item_id: TurnItemId::new(item_id).map_err(invalid)?,
            })),
            Intent::Refresh => {
                self.refresh();
                self.subscribe_thread();
                None
            }
            Intent::ListFiles { path } => {
                self.state.workspace.requested_directory = Some(path.clone());
                Some(Call::ListFiles(op::ListFiles { path }))
            }
            Intent::ReadFile {
                path,
                discard_draft,
            } => {
                self.state.workspace.requested_file = Some(path.clone());
                if discard_draft {
                    self.state.workspace.file_drafts.remove(&path);
                }
                Some(Call::ReadFile(op::ListFiles { path }))
            }
            Intent::EditFile { path, text } => {
                let file = self
                    .state
                    .workspace
                    .file
                    .as_ref()
                    .filter(|file| file.path == path)
                    .ok_or_else(|| invalid("Open the file before editing"))?;
                self.state
                    .workspace
                    .file_drafts
                    .entry(path)
                    .and_modify(|draft| Arc::make_mut(draft).text = text.clone())
                    .or_insert_with(|| {
                        Arc::new(crate::state::FileDraft {
                            text,
                            revision: file.revision.clone(),
                        })
                    });
                None
            }
            Intent::SaveFile { path } => {
                let draft = self
                    .state
                    .workspace
                    .file_drafts
                    .get(&path)
                    .cloned()
                    .ok_or_else(|| invalid("File has no edits"))?;
                Some(Call::WriteFile(op::WriteFile {
                    path,
                    revision: draft.revision.clone(),
                    text: draft.text.clone(),
                }))
            }
            Intent::ReviewWorkspace { cwd } => {
                self.state.workspace.diff_request = None;
                self.state.workspace.review = None;
                Some(Call::ReviewWorkspace(op::ReviewWorkspace { cwd }))
            }
            Intent::ReadTurnDiff {
                from_turn_count,
                to_turn_count,
                ignore_whitespace,
            } => {
                let request = rpc::GetTurnDiff {
                    thread_id: target.clone().ok_or_else(|| invalid("Select a thread"))?,
                    from_turn_count,
                    to_turn_count,
                    ignore_whitespace,
                };
                self.state.workspace.review = None;
                self.state.workspace.diff_request = Some(request.clone());
                Some(Call::GetTurnDiff(request))
            }
            Intent::LoadWorktreeSettings => Some(Call::ReadWorktreeSettings(m::Empty {})),
            Intent::SaveWorktreeSettings { settings } => {
                Some(Call::UpdateWorktreeSettings(settings))
            }
            Intent::ListWorktrees => Some(Call::ListWorktrees(m::Empty {})),
            Intent::RemoveWorktree { path } => {
                Some(Call::RemoveWorktree(op::RemoveWorktree { path }))
            }
            Intent::StartTerminal {
                handle,
                cwd,
                cols,
                rows,
            } => {
                let size = op::TerminalSize { cols, rows };
                let previous = self.state.terminals.get(&handle);
                let output = previous.map(|t| t.output.clone()).unwrap_or_default();
                let sequence = previous.map_or(0, |t| t.sequence);
                self.state.terminals.insert(
                    handle.clone(),
                    Terminal {
                        cwd: cwd.clone(),
                        size,
                        phase: TerminalPhase::Starting,
                        output,
                        sequence,
                    },
                );
                Some(Call::StartTerminal(op::StartTerminal { handle, cwd, size }))
            }
            Intent::ResizeTerminal { handle, cols, rows } => {
                Some(Call::ResizeTerminal(op::ResizeTerminal {
                    handle,
                    size: op::TerminalSize { cols, rows },
                }))
            }
            Intent::WriteTerminal { handle, data } => {
                Some(Call::WriteTerminal(op::TerminalWrite {
                    process_handle: handle,
                    data,
                }))
            }
            Intent::DetachTerminal { handle } => {
                Some(Call::DetachTerminal(op::DetachTerminal { handle }))
            }
            Intent::KillTerminal { handle } => Some(Call::KillTerminal(op::TerminalKill {
                process_handle: handle,
            })),
            Intent::LoadAccounts => Some(Call::ListAccounts(m::Empty {})),
            Intent::SelectAccount { provider, id } => {
                Some(Call::SelectAccount(op::SelectAccount { provider, id }))
            }
            Intent::StartLogin { provider } => {
                Some(Call::StartAccountLogin(op::StartAccountLogin { provider }))
            }
            Intent::CompleteLogin { provider, id, code } => {
                Some(Call::SubmitAccountLogin(op::SubmitAccountLogin {
                    provider,
                    id,
                    code,
                }))
            }
            Intent::CancelLogin { provider, id } => {
                Some(Call::CancelAccountLogin(op::CancelAccountLogin {
                    provider,
                    id,
                }))
            }
            Intent::DeleteAccount { provider, id } => {
                Some(Call::LogoutAccount(op::LogoutAccount { provider, id }))
            }
            Intent::LoadHostStatus => Some(Call::HostStatus(m::Empty {})),
            Intent::LoadRemoteHosts => Some(Call::ListRemotes(m::Empty {})),
            Intent::LoadHostManagement => {
                self.job(Call::HostStatus(m::Empty {}), None, None, None)?;
                Some(Call::ListRemotes(m::Empty {}))
            }
            Intent::RemoveRemoteHost { id } => {
                Some(Call::RemoveRemote(op::RemoveRemoteHost { id }))
            }
            Intent::PairRemoteHost { .. } => {
                unreachable!("pairing is executed by the connection owner")
            }
            Intent::CreateInvitation => Some(Call::Invite(m::Empty {})),
            Intent::RevokeDevice { id } => Some(Call::Revoke(op::RevokeDevice { id })),
            Intent::RegisterProject { path } => {
                Some(Call::AddProject(op::AddProject { cwd: path }))
            }
        };
        Ok((
            call.or_else(|| {
                body.map(|body| {
                    Call::DispatchCommand(command(target.expect("thread command target"), body))
                })
            }),
            sent,
            launched,
        ))
    }
    fn finished(&mut self, result: JobResult) {
        let JobResult {
            call,
            result,
            complete,
            sent,
            launched,
        } = result;
        let mutation = mutation_thread(&call);
        let should_navigate = sent
            .as_ref()
            .is_some_and(|(key, _)| self.state.draft_key() == *key);
        let paired = match &result {
            Ok(Reply::Remote(host)) => Some(host.id.clone()),
            _ => None,
        };
        let cancelled = if let Call::Transcribe(params) = &call {
            params
                .preparation
                .as_ref()
                .and_then(|id| self.dictations.remove(id))
                .is_some_and(|token| token.is_cancelled())
        } else {
            false
        };
        let result = if cancelled {
            Err(invalid("Dictation cancelled"))
        } else {
            result
        };
        let outcome = match result {
            Err(error) => {
                if !cancelled
                    && (complete.is_some()
                        || matches!(call, Call::StartTerminal(_) | Call::Transcribe(_)))
                {
                    self.state.error = Some(crate::presentation::error::error_message(
                        &error.to_string(),
                    ));
                }
                if let Some(id) = match &call {
                    Call::DispatchCommand(c) => Some(&c.command_id),
                    Call::LaunchThread(l) => Some(&l.create.command_id),
                    _ => None,
                } {
                    if !matches!(
                        error,
                        PeerError::Remote {
                            delivery: agent_protocol::error::Delivery::NotSent,
                            ..
                        }
                    ) {
                        self.state.uncertain_commands.insert(id.clone());
                    } else {
                        self.state.uncertain_commands.remove(id);
                    }
                }
                if matches!(
                    error,
                    PeerError::Remote {
                        delivery: agent_protocol::error::Delivery::NotSent,
                        ..
                    }
                ) {
                    match &call {
                        Call::DispatchCommand(command) => {
                            self.state
                                .pending_commands
                                .retain(|old| old.command_id != command.command_id);
                        }
                        Call::LaunchThread(launch) => {
                            self.state
                                .pending_launches
                                .retain(|old| old.create.command_id != launch.create.command_id);
                        }
                        _ => {}
                    }
                }
                if let Call::StartTerminal(params) = &call
                    && let Some(terminal) = self.state.terminals.get_mut(&params.handle)
                {
                    terminal.phase = TerminalPhase::Failed(
                        crate::presentation::error::error_message(&error.to_string()),
                    );
                }
                Err(error)
            }
            Ok(reply) => {
                if complete.is_some() {
                    self.state.error = None;
                }
                match reply {
                    Reply::Receipt(receipt) => {
                        if let Some(id) = match &call {
                            Call::DispatchCommand(c) => Some(&c.command_id),
                            Call::LaunchThread(l) => Some(&l.create.command_id),
                            _ => None,
                        } {
                            self.state.uncertain_commands.remove(id);
                        }
                        if let Call::DispatchCommand(command) = &call {
                            if matches!(command.body, CommandBody::CheckpointRollback { .. }) {
                                self.rollback_receipts
                                    .insert(command.command_id.clone(), receipt.sequence);
                            } else {
                                self.state
                                    .pending_commands
                                    .retain(|old| old.command_id != command.command_id);
                            }
                        }
                        if let Call::LaunchThread(launch) = &call {
                            self.state
                                .pending_launches
                                .retain(|old| old.create.command_id != launch.create.command_id);
                        }
                        if sent.is_none()
                            && let Call::DispatchCommand(Command {
                                thread_id,
                                body: CommandBody::MessageDispatch(input),
                                ..
                            }) = &call
                            && let Some(draft) = self.state.drafts.get_mut(thread_id.as_str())
                            && draft.text == input.text
                        {
                            draft.text.clear();
                        }
                        if let Some((key, draft)) = sent {
                            if !matches!(
                                &call,
                                Call::DispatchCommand(Command {
                                    body: CommandBody::ThreadFork { .. }
                                        | CommandBody::ThreadMergeBack { .. },
                                    ..
                                })
                            ) && self
                                .state
                                .drafts
                                .get(&key)
                                .is_none_or(|current| current.text == draft.text)
                            {
                                if matches!(
                                    &call,
                                    Call::DispatchCommand(Command {
                                        body: CommandBody::QueuedRunEdit { .. },
                                        ..
                                    })
                                ) {
                                    self.state.drafts.remove(&key);
                                } else {
                                    self.state.drafts.entry(key).or_insert(draft).text.clear();
                                }
                            }
                            if let Call::DispatchCommand(Command {
                                body: CommandBody::QueuedRunEdit { run_id, .. },
                                ..
                            }) = &call
                                && self.state.editing_run.as_ref() == Some(run_id)
                            {
                                self.state.editing_run = None;
                            }
                        }
                        if should_navigate && let Some(id) = &launched {
                            let key = self.state.draft_key();
                            let transfers_context = matches!(
                                &call,
                                Call::DispatchCommand(Command {
                                    body: CommandBody::ThreadFork { .. }
                                        | CommandBody::ThreadMergeBack { .. },
                                    ..
                                })
                            );
                            if !transfers_context
                                && let Some(draft) = self.state.drafts.get(&key).cloned()
                            {
                                self.state.drafts.insert(id.to_string(), draft);
                                if let Some(source) = self.state.drafts.get_mut(&key) {
                                    source.text.clear();
                                }
                            }
                            self.state.selected_thread = Some(id.clone());
                            self.visited.remove(id);
                            self.subscribe_thread();
                            self.visit_selected();
                        }
                        let _ = receipt;
                    }
                    Reply::History(id, page) => {
                        if let Some(cache) = self.state.threads.get_mut(&id)
                            && matches!(&call, Call::ReadThreadHistory(params) if params.cursor == cache.history_cursor)
                        {
                            *cache = crate::sync::history(cache, page);
                        }
                    }
                    Reply::Item(id, item) => {
                        if let Some(item) = item
                            && let Some(cache) = self.state.threads.get_mut(&id)
                        {
                            let p = Arc::make_mut(&mut cache.projection);
                            if let Some(old) = p.turn_items.iter_mut().find(|old| old.id == item.id)
                            {
                                *old = *item;
                            } else if item.thread_id == id {
                                p.turn_items.push(*item);
                            } else if let Some(row) = p.visible_turn_items.iter_mut().find(|row| {
                                row.source_thread_id == item.thread_id
                                    && row.source_item_id == item.id
                            }) {
                                row.item = *item;
                            }
                        }
                    }
                    Reply::Models(page) => {
                        self.state.models = page.data;
                        self.state.model_errors = page
                            .provider_errors
                            .unwrap_or_default()
                            .into_iter()
                            .map(|(k, v)| {
                                (
                                    k,
                                    v.as_str()
                                        .map(str::to_owned)
                                        .unwrap_or_else(|| v.to_string()),
                                )
                            })
                            .collect();
                        if self.state.default_draft.model.is_empty()
                            && let Some(model) = self
                                .state
                                .models
                                .iter()
                                .find(|m| m.is_default == Some(true))
                                .or(self.state.models.first())
                        {
                            self.state.default_draft = Draft {
                                instance_id: match model.model.provider {
                                    crate::provider::ProviderKind::Codex => "codex",
                                    crate::provider::ProviderKind::Claude => "claude",
                                }
                                .into(),
                                model: model.id.clone(),
                                runtime_mode: "full-access".into(),
                                interaction_mode: "default".into(),
                                ..Draft::default()
                            };
                        }
                    }
                    Reply::Search(matches) => {
                        if let Call::SearchThreads(params) = &call
                            && params.query == self.state.search.trim()
                        {
                            self.state.search_matches = matches;
                        }
                    }
                    Reply::ProjectAdded(id) => {
                        self.state.selected_project = Some(id);
                        self.refresh();
                    }
                    Reply::Projects(projects) => self.state.projects = projects,
                    Reply::Files(files) => {
                        if matches!(&call, Call::ListFiles(request) if self.state.workspace.requested_directory.as_ref() == Some(&request.path))
                        {
                            self.state.workspace.directory = Some(files);
                        }
                    }
                    Reply::File(file) => {
                        if let Call::WriteFile(written) = &call
                            && let Some(draft) =
                                self.state.workspace.file_drafts.get_mut(&file.path)
                            && draft.revision == written.revision
                        {
                            if draft.text == written.text {
                                self.state.workspace.file_drafts.remove(&file.path);
                            } else {
                                Arc::make_mut(draft).revision = file.revision.clone();
                            }
                        }
                        let requested = match &call {
                            Call::ReadFile(request) => {
                                self.state.workspace.requested_file.as_ref().map_or_else(
                                    || {
                                        self.state
                                            .workspace
                                            .file
                                            .as_ref()
                                            .is_some_and(|current| current.path == file.path)
                                    },
                                    |path| path == &request.path,
                                )
                            }
                            Call::WriteFile(_) => self
                                .state
                                .workspace
                                .file
                                .as_ref()
                                .is_some_and(|current| current.path == file.path),
                            _ => false,
                        };
                        if requested {
                            self.state.workspace.requested_file = Some(file.path.clone());
                            self.state.workspace.file = Some(Arc::new(file));
                        }
                    }
                    Reply::Review(review) => {
                        if let Call::ReviewWorkspace(request) = &call
                            && request.cwd == self.state.cwd()
                            && self.state.workspace.diff_request.is_none()
                        {
                            self.state.workspace.review_generation += 1;
                            self.state.workspace.review = Some(Arc::new(review));
                        }
                    }
                    Reply::TurnDiff(diff) => {
                        if let Call::GetTurnDiff(request) = &call
                            && self.state.workspace.diff_request.as_ref() == Some(request)
                            && self.state.selected_thread.as_ref() == Some(&diff.thread_id)
                            && request.thread_id == diff.thread_id
                            && request.from_turn_count == diff.from_turn_count
                            && request.to_turn_count == diff.to_turn_count
                        {
                            self.state.workspace.review_generation += 1;
                            self.state.workspace.review =
                                Some(Arc::new(crate::presentation::diff::turn_review(diff)));
                        }
                    }
                    Reply::WorktreeSettings(settings) => {
                        self.state.workspace.worktree_settings = Some(settings)
                    }
                    Reply::Worktrees(worktrees) => self.state.workspace.worktrees = worktrees,
                    Reply::Accounts(accounts) => self.state.accounts = Some(accounts),
                    Reply::Login(login) => self.state.account_login = Some(login),
                    Reply::HostStatus(status) => self.state.host_status = Some(status),
                    Reply::Remotes(remotes) => self.state.remote_hosts = remotes,
                    Reply::Remote(host) => {
                        self.state.remote_hosts.retain(|old| old.id != host.id);
                        self.state.remote_hosts.push(host);
                    }
                    Reply::Invitation(invitation) => self.state.invitation = Some(invitation),
                    Reply::Transcription(text) => {
                        if let Some((key, original)) = sent {
                            let draft = self.state.drafts.entry(key).or_insert(original);
                            if !draft.text.is_empty() && !text.is_empty() {
                                draft.text.push('\n');
                            }
                            draft.text.push_str(&text);
                        }
                    }
                    Reply::Done => {}
                }
                match &call {
                    Call::RemoveRemote(params) => {
                        self.state.remote_hosts.retain(|host| host.id != params.id)
                    }
                    Call::Revoke(_) => {
                        let _ = self.job(Call::HostStatus(m::Empty {}), None, None, None);
                    }
                    Call::StartTerminal(params) => {
                        if let Some(t) = self.state.terminals.get_mut(&params.handle)
                            && t.phase == TerminalPhase::Starting
                        {
                            t.phase = TerminalPhase::Running;
                        }
                    }
                    Call::ResizeTerminal(params) => {
                        if let Some(t) = self.state.terminals.get_mut(&params.handle) {
                            t.size = params.size;
                        }
                    }
                    Call::DetachTerminal(params) => {
                        if let Some(t) = self.state.terminals.get_mut(&params.handle) {
                            t.phase = TerminalPhase::Detached;
                        }
                    }
                    Call::SelectAccount(_)
                    | Call::SubmitAccountLogin(_)
                    | Call::CancelAccountLogin(_)
                    | Call::LogoutAccount(_) => self.refresh(),
                    _ => {}
                }
                Ok(if let Some(id) = paired {
                    Outcome::RemoteHostPaired { id }
                } else {
                    launched
                        .map(|id| Outcome::StartedThread { id: id.to_string() })
                        .unwrap_or_default()
                })
            }
        };
        if let Some(thread) = mutation
            && let Some(network) = self.network.as_mut()
        {
            network.running_mutations.remove(&thread);
            let next = network
                .mutations
                .get_mut(&thread)
                .and_then(VecDeque::pop_front);
            if network
                .mutations
                .get(&thread)
                .is_some_and(VecDeque::is_empty)
            {
                network.mutations.remove(&thread);
            }
            if let Some(job) = next {
                let _ = self.job(job.call, job.complete, job.sent, job.launched);
            }
        }
        self.reconcile_rollbacks();
        self.publish();
        if let Some(complete) = complete {
            let _ = complete.send(outcome);
        }
    }
    fn notification(&mut self, notification: protocol::Notification) {
        match notification {
            protocol::Notification::Output { handle, data } => {
                self.terminal_output(&handle, data, None)
            }
            protocol::Notification::TerminalRestored {
                handle,
                data,
                cols,
                rows,
            } => {
                self.terminal_output(&handle, data, Some(op::TerminalSize { cols, rows }));
                if let Some(t) = self.state.terminals.get_mut(&handle) {
                    t.phase = TerminalPhase::Running;
                }
            }
            protocol::Notification::Exited { handle, code } => {
                if let Some(t) = self.state.terminals.get_mut(&handle) {
                    t.phase = TerminalPhase::Exited(code);
                }
            }
            protocol::Notification::TerminalFailed { handle, reason } => {
                if let Some(t) = self.state.terminals.get_mut(&handle) {
                    t.phase = TerminalPhase::Failed(reason);
                }
            }
            _ => {}
        }
    }
    fn terminal_output(
        &mut self,
        handle: &str,
        data: Vec<u8>,
        reset_size: Option<op::TerminalSize>,
    ) {
        if let Some(t) = self.state.terminals.get_mut(handle) {
            t.sequence += 1;
            if reset_size.is_some() {
                t.output.clear();
            }
            t.output.push(Arc::new(TerminalOutput {
                sequence: t.sequence,
                data,
                reset_size,
            }));
            let mut bytes = t.output.iter().map(|o| o.data.len()).sum::<usize>();
            while bytes > 8 * 1024 * 1024 && t.output.len() > 1 {
                bytes -= t.output.remove(0).data.len();
            }
        }
    }
}

async fn execute(peer: &Client, call: &Call) -> Result<Reply, PeerError> {
    Ok(match call {
        Call::DispatchCommand(_) | Call::LaunchThread(_) => {
            Reply::Receipt(peer.request(call).await?)
        }
        Call::ReadThreadHistory(p) => {
            Reply::History(p.thread_id.clone(), peer.request(call).await?)
        }
        Call::GetTurnItem(p) => Reply::Item(
            p.thread_id.clone(),
            peer.request::<Option<TurnItem>>(call).await?.map(Box::new),
        ),
        Call::SearchThreads(_) => Reply::Search(peer.request(call).await?),
        Call::AddProject(_) => Reply::ProjectAdded(peer.request(call).await?),
        Call::ListModels(_) => Reply::Models(crate::client::models(peer).await?),
        Call::ListProjects(_) => Reply::Projects(peer.request(call).await?),
        Call::ListFiles(_) => Reply::Files(peer.request(call).await?),
        Call::ReadFile(_) | Call::WriteFile(_) => Reply::File(peer.request(call).await?),
        Call::ReviewWorkspace(_) => Reply::Review(peer.request(call).await?),
        Call::GetTurnDiff(_) => Reply::TurnDiff(peer.request(call).await?),
        Call::ReadWorktreeSettings(_) | Call::UpdateWorktreeSettings(_) => {
            Reply::WorktreeSettings(peer.request(call).await?)
        }
        Call::ListWorktrees(_) => Reply::Worktrees(peer.request(call).await?),
        Call::ListAccounts(_) => Reply::Accounts(peer.request(call).await?),
        Call::StartAccountLogin(_) => Reply::Login(peer.request(call).await?),
        Call::HostStatus(_) => Reply::HostStatus(peer.request(call).await?),
        Call::ListRemotes(_) => Reply::Remotes(peer.request(call).await?),
        Call::RegisterRemote(_) => Reply::Remote(peer.request(call).await?),
        Call::Invite(_) => Reply::Invitation(peer.request(call).await?),
        Call::Transcribe(_) => {
            Reply::Transcription(peer.request::<op::Transcription>(call).await?.text)
        }
        Call::SelectAccount(_) => {
            let _: op::AccountSelection = peer.request(call).await?;
            Reply::Done
        }
        Call::RemoveWorktree(_) => {
            let _: () = peer.request(call).await?;
            Reply::Done
        }
        _ => {
            let _: m::Empty = peer.request(call).await?;
            Reply::Done
        }
    })
}
async fn notifications(mut events: Updates, epoch: u64, sender: mpsc::Sender<OwnerEvent>) {
    loop {
        match events.read::<protocol::Notification>().await {
            Ok(Some(notification)) => {
                if sender
                    .send(OwnerEvent::Notification(epoch, notification))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => {
                let _ = sender
                    .send(OwnerEvent::Disconnected(
                        epoch,
                        "Host connection closed".into(),
                    ))
                    .await;
                return;
            }
            Err(error) => {
                let _ = sender
                    .send(OwnerEvent::Disconnected(epoch, error.to_string()))
                    .await;
                return;
            }
        }
    }
}
async fn shell_stream(
    peer: Arc<Client>,
    epoch: u64,
    snapshots: watch::Receiver<Arc<Snapshot>>,
    sender: mpsc::Sender<OwnerEvent>,
) {
    loop {
        let after = snapshots
            .borrow()
            .shell
            .as_ref()
            .map(|s| s.snapshot_sequence);
        let stream = peer
            .request_stream::<ShellStreamItem>(&Call::SubscribeShell(rpc::SubscribeShell {
                after_sequence: after,
            }))
            .await;
        if let Ok((first, mut stream)) = stream {
            if sender.send(OwnerEvent::Shell(epoch, first)).await.is_err() {
                return;
            }
            while let Ok(Some(item)) = stream.read::<ShellStreamItem>().await {
                if sender.send(OwnerEvent::Shell(epoch, item)).await.is_err() {
                    return;
                }
            }
        }
        if peer.is_closed() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
async fn thread_stream(
    peer: Arc<Client>,
    epoch: u64,
    id: ThreadId,
    snapshots: watch::Receiver<Arc<Snapshot>>,
    sender: mpsc::Sender<OwnerEvent>,
) {
    loop {
        let after = snapshots.borrow().threads.get(&id).map(|c| c.sequence);
        let stream = peer
            .request_stream::<ThreadStreamItem>(&Call::SubscribeThread(rpc::SubscribeThread {
                thread_id: id.clone(),
                after_sequence: after,
            }))
            .await;
        if let Ok((first, mut stream)) = stream {
            if sender
                .send(OwnerEvent::Thread(epoch, id.clone(), first))
                .await
                .is_err()
            {
                return;
            }
            while let Ok(Some(item)) = stream.read::<ThreadStreamItem>().await {
                if sender
                    .send(OwnerEvent::Thread(epoch, id.clone(), item))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        if peer.is_closed() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn healthy_resume_reuses_connection_and_timed_out_mutations_keep_id_and_order() {
        tokio::time::timeout(Duration::from_secs(10), async {
            use transport::{Endpoint, Identity, IncomingRequest, Relays, Trust};
            let host = Endpoint::bind(Identity::generate(), Relays::Loopback).await.unwrap();
            let endpoint = Endpoint::bind(Identity::generate(), Relays::Loopback).await.unwrap();
            let ticket = host.local_ticket();
            let (outgoing, incoming) = tokio::join!(endpoint.connect(&ticket), host.accept());
            let session = outgoing.unwrap();
            let incoming = incoming.unwrap().unwrap().authorize(&Trust { allowed: BTreeSet::from([endpoint.node_id()]), ..Default::default() }).unwrap();
            let (client, _events) = session.open_peer(Duration::from_millis(150), 8).await.unwrap();
            let _host_events = incoming.accept_peer().await.unwrap();
            let client = Arc::new(client);
            let mut owner = owner(Snapshot { connected: true, ..Default::default() });
            owner.network = Some(Network { peer: client.clone(), session: session.clone(), ticket: ticket.clone(), epoch: 0, tasks: vec![], thread: None, mutations: BTreeMap::new(), running_mutations: BTreeSet::new() });
            let (sender, mut receiver) = mpsc::channel(8);
            owner.sender = sender;
            let (complete, answer) = oneshot::channel();
            owner.handle(OwnerEvent::Resume { endpoint: endpoint.clone(), ticket, complete }).await;
            let IncomingRequest::Call(mut probe) = incoming.accept_request().await.unwrap() else { panic!("health probe") };
            assert!(matches!(probe.call, Call::HostStatus(_)));
            agent_transport::framing::write(&mut probe.send, protocol::Response::Success { result: m::HostStatus { name: "fixture".into(), node_id: "node".into(), devices: vec![], provider_errors: None } }).await.unwrap();
            probe.send.finish().unwrap();
            let reused = answer.await.unwrap().unwrap();
            assert!(reused.reused);
            assert_eq!(reused.connection_id, client.diagnostic_id);
            let thread = ThreadId::new("thread").unwrap();
            let first = command(thread.clone(), CommandBody::ThreadPin { order_key: None });
            let second = command(thread.clone(), CommandBody::ThreadUnpin);
            let expected = vec![first.clone(), first.clone(), second.clone()];
            let server = tokio::spawn(async move {
                let mut held = vec![];
                for (index, expected) in expected.into_iter().enumerate() {
                    let IncomingRequest::Call(mut request) = incoming.accept_request().await.unwrap() else { panic!("mutation request"); };
                    assert!(matches!(&request.call, Call::DispatchCommand(actual) if actual == &expected));
                    if index == 0 {
                        // Keep the response stream open until the client times out.
                        held.push(request.send);
                    } else {
                        agent_transport::framing::write(&mut request.send, protocol::Response::Success { result: rpc::DispatchReceipt { thread_id: expected.thread_id, sequence: index as u64, replayed: index == 1 } }).await.unwrap();
                        request.send.finish().unwrap();
                    }
                }
            });
            owner.job(Call::DispatchCommand(first), None, None, None).unwrap();
            owner.job(Call::DispatchCommand(second), None, None, None).unwrap();
            for _ in 0..2 {
                let event = receiver.recv().await.unwrap();
                assert!(matches!(&event, OwnerEvent::Finished(_, result) if result.result.is_ok()));
                owner.handle(event).await;
            }
            server.await.unwrap();
            assert!(owner.state.pending_commands.is_empty());
            assert!(owner.network.as_ref().unwrap().running_mutations.is_empty());
            drop(owner);
            session.close();
            endpoint.close().await;
            host.close().await;
        }).await.unwrap();
    }
    fn owner(state: Snapshot) -> Owner {
        let (sender, _) = mpsc::channel(64);
        let (snapshots, _) = watch::channel(Arc::new(state.clone()));
        Owner {
            state,
            snapshots,
            sender,
            network: None,
            epoch: 0,
            source: CreationSource::Desktop,
            visited: BTreeMap::new(),
            rollback_receipts: BTreeMap::new(),
            dictations: BTreeMap::new(),
        }
    }
    fn queued_state() -> Snapshot {
        let mut p = crate::test_support::projection();
        let caps = TurnCapabilities {
            exposes_native_turn_id: true,
            emits_turn_started: true,
            emits_turn_completed: true,
            supports_interrupt: true,
            supports_active_steering: true,
            supports_steering_by_interrupt_restart: true,
            supports_queued_messages: true,
            terminal_status_quality: Strength::Strong,
        };
        for (key, mode) in [
            (
                "preparing",
                DispatchMode::DeferStart {
                    workspace_strategy: None,
                },
            ),
            ("queued", DispatchMode::QueueAfterActive),
        ] {
            let draft = Draft {
                text: key.into(),
                instance_id: "codex".into(),
                model: "model".into(),
                runtime_mode: "full-access".into(),
                interaction_mode: "default".into(),
                ..Default::default()
            };
            let command = command(
                p.thread.id.clone(),
                CommandBody::MessageDispatch(
                    crate::commands::message(
                        &draft,
                        MessageId::new(key).unwrap(),
                        mode,
                        CreationSource::Desktop,
                    )
                    .unwrap(),
                ),
            );
            let decision = orchestration::decider::decide(
                &command,
                Some(&p),
                &crate::test_support::now(),
                &caps,
                Driver::Codex,
            )
            .unwrap();
            for event in decision.events {
                p = orchestration::projector::apply(Some(&p), &event, Default::default()).unwrap();
            }
        }
        let id = p.thread.id.clone();
        let shell = orchestration::projector::shell(&p);
        let mut state = Snapshot {
            selected_thread: Some(id.clone()),
            shell: Some(Arc::new(ShellSnapshot {
                schema_version: 2,
                snapshot_sequence: 1,
                threads: vec![shell],
                archived_threads: vec![],
            })),
            ..Default::default()
        };
        crate::sync::thread(
            &mut state,
            &id,
            ThreadStreamItem::Snapshot {
                snapshot_sequence: 1,
                projection: Box::new(p),
                history_cursor: None,
                has_more_history: false,
                latest_local_turn_ordinal: None,
            },
        );
        state
    }
    #[test]
    fn search_respects_server_limits_and_clear_remains_local() {
        let mut owner = owner(Snapshot::default());
        owner.state.connected = true;
        let (call, _, _) = owner
            .prepare(Intent::Search {
                query: "bug".into(),
            })
            .unwrap();
        assert!(matches!(
            call,
            Some(Call::SearchThreads(rpc::SearchThreads { limit: 50, .. }))
        ));
        for query in ["", "a"] {
            assert!(
                owner
                    .prepare(Intent::Search {
                        query: query.into()
                    })
                    .unwrap()
                    .0
                    .is_none()
            );
        }
    }
    #[test]
    fn queue_edit_cancel_and_receipt_preserve_the_main_composer_draft() {
        let mut owner = owner(queued_state());
        let mut draft = owner.state.current_draft();
        draft.text = "Unsent main draft".into();
        owner
            .state
            .drafts
            .insert(owner.state.draft_key(), draft.clone());
        let run = owner
            .state
            .projection()
            .unwrap()
            .runs
            .iter()
            .find(|r| r.status == RunStatus::Queued)
            .unwrap()
            .id
            .clone();
        owner
            .prepare(Intent::Queue {
                action: QueueAction::Edit {
                    run_id: run.to_string(),
                },
            })
            .unwrap();
        assert_eq!(owner.state.current_draft().text, "queued");
        owner
            .prepare(Intent::EditDraft {
                text: "cancelled edits".into(),
                base_text: None,
            })
            .unwrap();
        owner
            .prepare(Intent::Queue {
                action: QueueAction::CancelEdit,
            })
            .unwrap();
        assert_eq!(owner.state.current_draft(), draft);
        assert!(
            !owner
                .state
                .drafts
                .keys()
                .any(|key| key.starts_with("queue:"))
        );
        owner
            .prepare(Intent::Queue {
                action: QueueAction::Edit {
                    run_id: run.to_string(),
                },
            })
            .unwrap();
        assert_eq!(owner.state.current_draft().text, "queued");
        let (call, sent, _) = owner
            .prepare(Intent::Queue {
                action: QueueAction::SaveEdit,
            })
            .unwrap();
        owner.finished(JobResult {
            call: call.unwrap(),
            result: Ok(Reply::Receipt(rpc::DispatchReceipt {
                thread_id: owner.state.selected_thread.clone().unwrap(),
                sequence: 2,
                replayed: false,
            })),
            complete: None,
            sent,
            launched: None,
        });
        assert_eq!(owner.state.current_draft(), draft);
    }
    #[test]
    fn awaiting_message_cannot_be_submitted_twice() {
        let mut owner = owner(queued_state());
        let mut draft = owner.state.current_draft();
        draft.text = "Send me once".into();
        owner.state.drafts.insert(owner.state.draft_key(), draft);
        let (call, _, _) = owner
            .prepare(Intent::Send {
                behavior: SendBehavior::Default,
            })
            .unwrap();
        let Some(Call::DispatchCommand(command)) = call else {
            panic!("message command")
        };
        owner.state.pending_commands.push(command);
        assert!(
            owner
                .prepare(Intent::Send {
                    behavior: SendBehavior::Default
                })
                .unwrap()
                .0
                .is_none()
        );
        assert!(
            !crate::presentation::conversation(&owner.state, &now())
                .composer
                .enabled
        );
    }
    #[test]
    fn text_edits_preserve_newer_model_and_mode_choices() {
        let mut owner = owner(queued_state());
        let draft = Draft {
            model: "chosen model".into(),
            runtime_mode: "approval-required".into(),
            text: "before".into(),
            ..owner.state.current_draft()
        };
        owner
            .state
            .drafts
            .insert(owner.state.draft_key(), draft.clone());
        owner
            .prepare(Intent::EditDraft {
                text: "after".into(),
                base_text: Some("before".into()),
            })
            .unwrap();
        assert_eq!(
            owner.state.current_draft(),
            Draft {
                text: "after".into(),
                ..draft
            }
        );
    }
    #[test]
    fn context_receipts_preserve_both_drafts_and_pending_context_blocks_duplicates() {
        for merge in [false, true] {
            let mut owner = owner(queued_state());
            let source = owner.state.selected_thread.clone().unwrap();
            let parent = ThreadId::new("parent").unwrap();
            let p = Arc::make_mut(&mut owner.state.threads.get_mut(&source).unwrap().projection);
            p.thread.lineage.parent_thread_id = Some(parent.clone());
            p.thread.lineage.relationship_to_parent = Some(Relationship::Fork);
            for run in &mut p.runs {
                run.status = RunStatus::Completed;
            }
            let run = p.runs.last().unwrap().id.to_string();
            let source_draft = Draft {
                text: "source unsent".into(),
                ..owner.state.current_draft()
            };
            let parent_draft = Draft {
                text: "parent unsent".into(),
                ..source_draft.clone()
            };
            owner
                .state
                .drafts
                .insert(source.to_string(), source_draft.clone());
            owner
                .state
                .drafts
                .insert(parent.to_string(), parent_draft.clone());
            let intent = if merge {
                Intent::MergeBack
            } else {
                Intent::Fork {
                    source_thread_id: source.to_string(),
                    run_id: run,
                }
            };
            let (call, sent, launched) = owner.prepare(intent.clone()).unwrap();
            let Call::DispatchCommand(command) = call.clone().unwrap() else {
                panic!("context command")
            };
            if merge {
                assert!(matches!(
                    command.body,
                    CommandBody::ThreadMergeBack {
                        source_point: ForkPoint::Run { .. },
                        ..
                    }
                ));
            }
            owner.state.pending_commands.push(command);
            assert!(owner.prepare(intent).unwrap().0.is_none());
            assert!(!crate::presentation::conversation(&owner.state, &now()).can_merge_back);
            owner.finished(JobResult {
                call: call.unwrap(),
                result: Ok(Reply::Receipt(rpc::DispatchReceipt {
                    thread_id: source.clone(),
                    sequence: 2,
                    replayed: false,
                })),
                complete: None,
                sent,
                launched,
            });
            assert_eq!(owner.state.drafts[&source.to_string()], source_draft);
            assert_eq!(owner.state.drafts[&parent.to_string()], parent_draft);
        }
    }
    #[test]
    fn implement_follow_up_pending_blocks_same_thread_and_new_thread_submissions() {
        for new_thread in [false, true] {
            let mut owner = owner(queued_state());
            owner.state.connected = true;
            let id = owner.state.selected_thread.clone().unwrap();
            let p = Arc::make_mut(&mut owner.state.threads.get_mut(&id).unwrap().projection);
            for run in &mut p.runs {
                run.status = RunStatus::Completed;
            }
            p.thread.interaction_mode = InteractionMode::Plan;
            p.plans.push(PlanArtifact {
                id: PlanId::new("plan").unwrap(),
                thread_id: id.clone(),
                run_id: None,
                node_id: NodeId::new("root").unwrap(),
                status: PlanStatus::Active,
                detail_in_turn_item: false,
                body: PlanBody::ProposedPlan {
                    markdown: "# Build it".into(),
                },
            });
            let call = if new_thread {
                owner
                    .prepare(Intent::PlanFollowUp { new_thread })
                    .unwrap()
                    .0
                    .unwrap()
            } else {
                let mut input = crate::commands::message(
                    &Draft {
                        text: "implement".into(),
                        ..owner.state.current_draft()
                    },
                    MessageId::new("implement").unwrap(),
                    DispatchMode::StartImmediately,
                    CreationSource::Desktop,
                )
                .unwrap();
                input.text = "PLEASE IMPLEMENT THIS PLAN: Build it".into();
                input.source_plan_ref = Some(SourcePlanRef {
                    thread_id: id,
                    plan_id: PlanId::new("plan").unwrap(),
                });
                Call::DispatchCommand(command(
                    owner.state.selected_thread.clone().unwrap(),
                    CommandBody::MessageDispatch(input),
                ))
            };
            match call {
                Call::DispatchCommand(command) => owner.state.pending_commands.push(command),
                Call::LaunchThread(launch) => owner.state.pending_launches.push(*launch),
                _ => panic!("follow-up"),
            }
            assert!(owner.state.draft_pending());
            for new_thread in [false, true] {
                assert!(
                    owner
                        .prepare(Intent::PlanFollowUp { new_thread })
                        .unwrap()
                        .0
                        .is_none()
                );
            }
            assert!(
                !crate::presentation::conversation(&owner.state, &now())
                    .composer
                    .plan_follow_up
            );
        }
    }
    #[test]
    fn rollback_restores_text_only_after_success_and_appends_to_current_draft() {
        for failed in [false, true] {
            let mut owner = owner(queued_state());
            let id = owner.state.selected_thread.clone().unwrap();
            let p = Arc::make_mut(&mut owner.state.threads.get_mut(&id).unwrap().projection);
            let run = p.runs[0].clone();
            let scope = CheckpointScopeId::new("scope").unwrap();
            let checkpoint = orchestration::checkpoint::before_run_id(&scope, &run.id);
            p.checkpoints.push(Checkpoint {
                id: checkpoint.clone(),
                thread_id: id.clone(),
                scope_id: scope,
                run_id: Some(run.id.clone()),
                node_id: NodeId::new("root").unwrap(),
                parent_checkpoint_id: None,
                ordinal_within_scope: 0,
                app_run_ordinal: Some(0),
                reference: CheckpointRef::new("ref").unwrap(),
                status: CheckpointStatus::Ready,
                files: vec![],
                captured_at: now(),
            });
            let draft = Draft {
                text: "unsent".into(),
                ..owner.state.current_draft()
            };
            owner.state.drafts.insert(id.to_string(), draft);
            let (call, sent, launched) = owner
                .prepare(Intent::Rollback {
                    checkpoint_id: checkpoint.to_string(),
                    restore_files: false,
                })
                .unwrap();
            assert_eq!(owner.state.current_draft().text, "unsent");
            let Call::DispatchCommand(command) = call.clone().unwrap() else {
                panic!("rollback command")
            };
            owner.state.pending_commands.push(command.clone());
            owner.finished(JobResult {
                call: call.unwrap(),
                result: Ok(Reply::Receipt(rpc::DispatchReceipt {
                    thread_id: id.clone(),
                    sequence: 10,
                    replayed: false,
                })),
                complete: None,
                sent,
                launched,
            });
            assert_eq!(owner.state.current_draft().text, "unsent");
            owner
                .prepare(Intent::EditDraft {
                    text: "typed during rollback".into(),
                    base_text: None,
                })
                .unwrap();
            let cache = owner.state.threads.get_mut(&id).unwrap();
            cache.sequence = 10;
            Arc::make_mut(&mut cache.projection)
                .thread
                .rollback_request_id = Some(command.command_id);
            owner.reconcile_rollbacks();
            assert_eq!(owner.state.current_draft().text, "typed during rollback");
            let cache = owner.state.threads.get_mut(&id).unwrap();
            cache.sequence = 11;
            let p = Arc::make_mut(&mut cache.projection);
            p.thread.rollback_request_id = None;
            p.thread.rollback_failure = failed.then(|| "restore failed".into());
            p.runs[0].status = RunStatus::RolledBack;
            owner.reconcile_rollbacks();
            assert_eq!(
                owner.state.current_draft().text,
                if failed {
                    "typed during rollback"
                } else {
                    "typed during rollback\n\npreparing"
                }
            );
            assert!(owner.state.pending_commands.is_empty());
            owner.reconcile_rollbacks();
            assert_eq!(
                owner
                    .state
                    .current_draft()
                    .text
                    .matches("preparing")
                    .count(),
                usize::from(!failed)
            );
        }
    }
    #[test]
    fn launch_title_uses_the_first_nonempty_trimmed_line() {
        let mut owner = owner(Snapshot {
            default_draft: Draft {
                text: "  \n  Fix the bug \nMore details".into(),
                instance_id: "codex".into(),
                model: "model".into(),
                runtime_mode: "full-access".into(),
                interaction_mode: "default".into(),
                ..Default::default()
            },
            ..Default::default()
        });
        let (call, _, _) = owner
            .prepare(Intent::Send {
                behavior: SendBehavior::Default,
            })
            .unwrap();
        assert!(
            matches!(call, Some(Call::LaunchThread(launch)) if matches!(&launch.create.body, CommandBody::ThreadCreate { title, .. } if title == "Fix the bug"))
        );
    }
    #[test]
    fn failed_terminal_start_is_terminal_and_transport_failures_keep_pending_commands() {
        let mut owner = owner(queued_state());
        let (call, _, _) = owner
            .prepare(Intent::StartTerminal {
                handle: "terminal".into(),
                cwd: "/tmp".into(),
                cols: 80,
                rows: 24,
            })
            .unwrap();
        owner.finished(JobResult {
            call: call.unwrap(),
            result: Err(PeerError::ConnectionClosed("lost".into())),
            complete: None,
            sent: None,
            launched: None,
        });
        assert!(matches!(
            owner.state.terminals["terminal"].phase,
            TerminalPhase::Failed(_)
        ));
        let command = command(
            owner.state.selected_thread.clone().unwrap(),
            CommandBody::ThreadMarkUnread,
        );
        owner.state.pending_commands.push(command.clone());
        owner.finished(JobResult {
            call: Call::DispatchCommand(command),
            result: Err(PeerError::InvalidMessage("truncated reply".into())),
            complete: None,
            sent: None,
            launched: None,
        });
        assert_eq!(owner.state.pending_commands.len(), 1);
        owner.finished(JobResult {
            call: Call::ListProjects(m::Empty {}),
            result: Ok(Reply::Projects(vec![])),
            complete: None,
            sent: None,
            launched: None,
        });
        assert!(owner.state.error.is_some());
    }
    #[tokio::test]
    async fn snapshot_revisions_are_ordered_within_each_store_only() {
        let first = Store::offline(Snapshot {
            revision: 4000,
            ..Default::default()
        });
        let second = Store::offline(Snapshot::default());
        assert!(second.snapshot().accepts_after(&first.snapshot()));
        let mut old = (*second.snapshot()).clone();
        old.revision = 4;
        let mut next = old.clone();
        next.revision = 5;
        assert!(!old.accepts_after(&next));
        assert!(next.accepts_after(&old));
        first.close().await.unwrap();
        second.close().await.unwrap();
    }
    #[test]
    fn unknown_delivery_can_stop_retrying_without_erasing_the_draft() {
        let mut owner = owner(queued_state());
        owner.state.drafts.insert(
            owner.state.draft_key(),
            Draft {
                text: "unsent".into(),
                ..owner.state.current_draft()
            },
        );
        let command = command(
            owner.state.selected_thread.clone().unwrap(),
            CommandBody::ThreadMarkUnread,
        );
        let id = command.command_id.to_string();
        owner.state.pending_commands.push(command.clone());
        owner.finished(JobResult {
            call: Call::DispatchCommand(command),
            result: Err(PeerError::InvalidMessage("receipt decode failed".into())),
            complete: None,
            sent: None,
            launched: None,
        });
        assert_eq!(
            crate::presentation::conversation(&owner.state, &now())
                .composer
                .pending_deliveries
                .len(),
            1
        );
        owner
            .prepare(Intent::DiscardPending { command_id: id })
            .unwrap();
        assert!(owner.state.pending_commands.is_empty());
        assert!(owner.state.uncertain_commands.is_empty());
        assert_eq!(owner.state.current_draft().text, "unsent");
    }
    #[test]
    fn failed_background_visits_do_not_replace_user_notice() {
        let mut owner = owner(queued_state());
        owner.state.error = Some("user notice".into());
        owner.finished(JobResult {
            call: Call::DispatchCommand(command(
                owner.state.selected_thread.clone().unwrap(),
                CommandBody::ThreadVisit { visited_at: now() },
            )),
            result: Err(invalid("background failure")),
            complete: None,
            sent: None,
            launched: None,
        });
        assert_eq!(owner.state.error.as_deref(), Some("user notice"));
    }
    #[tokio::test]
    async fn mobile_cold_start_keeps_drafts_without_visiting_saved_selection() {
        let mut state = queued_state();
        state.drafts.insert(
            "thread".into(),
            Draft {
                text: "keep me".into(),
                ..state.current_draft()
            },
        );
        let store = Store::offline_for(state, CreationSource::Mobile);
        assert!(store.snapshot().selected_thread.is_none());
        assert_eq!(store.snapshot().drafts["thread"].text, "keep me");
        store.close().await.unwrap();
    }
    #[test]
    fn cancelled_dictation_cannot_append_a_late_transcript() {
        let mut owner = owner(queued_state());
        let key = owner.state.draft_key();
        owner.state.drafts.insert(
            key.clone(),
            Draft {
                text: "keep".into(),
                ..owner.state.current_draft()
            },
        );
        let token = CancellationToken::new();
        owner.dictations.insert("recording".into(), token.clone());
        token.cancel();
        owner.finished(JobResult {
            call: Call::Transcribe(op::Transcribe {
                preparation: Some("recording".into()),
                audio: vec![],
            }),
            result: Ok(Reply::Transcription("must not append".into())),
            complete: None,
            sent: Some((key, owner.state.current_draft())),
            launched: None,
        });
        assert_eq!(owner.state.current_draft().text, "keep");
        assert!(owner.state.error.is_none());
        assert!(owner.dictations.is_empty());
    }
    #[test]
    fn model_switch_is_compared_with_the_host_thread_selection() {
        let mut owner = owner(queued_state());
        let mut draft = owner.state.current_draft();
        draft.instance_id = "claude".into();
        owner.state.drafts.insert(owner.state.draft_key(), draft);
        let (call, _, _) = owner
            .prepare(Intent::SetModel {
                instance_id: "claude".into(),
                model: "claude-model".into(),
                effort: None,
                service_tier: None,
            })
            .unwrap();
        assert!(matches!(
            call,
            Some(Call::DispatchCommand(Command {
                body: CommandBody::ProviderSwitch { .. },
                ..
            }))
        ));
    }
    #[tokio::test]
    async fn a_burst_of_input_over_the_stream_channel_capacity_keeps_every_edit() {
        let store = Store::offline(Snapshot::default());
        let mut receipts = vec![];
        for i in 0..200 {
            receipts.push(store.dispatch(Intent::EditDraft {
                base_text: None,
                text: i.to_string(),
            }));
        }
        for receipt in receipts {
            receipt.await.unwrap().unwrap();
        }
        assert_eq!(store.snapshot().current_draft().text, "199");
        store.close().await.unwrap();
    }
    #[test]
    fn late_turn_diff_receipts_cannot_replace_another_range_or_thread() {
        let mut owner = owner(Snapshot {
            selected_thread: Some(ThreadId::new("thread").unwrap()),
            ..Default::default()
        });
        let intent = |from, to| Intent::ReadTurnDiff {
            from_turn_count: from,
            to_turn_count: to,
            ignore_whitespace: false,
        };
        let first = owner.prepare(intent(0, 1)).unwrap().0.unwrap();
        let second = owner.prepare(intent(1, 2)).unwrap().0.unwrap();
        let finish = |call, from, to| JobResult {
            call,
            result: Ok(Reply::TurnDiff(rpc::TurnDiff {
                thread_id: ThreadId::new("thread").unwrap(),
                from_turn_count: from,
                to_turn_count: to,
                diff: String::new(),
            })),
            complete: None,
            sent: None,
            launched: None,
        };
        owner.finished(finish(first, 0, 1));
        assert!(owner.state.workspace.review.is_none());
        owner.finished(finish(second.clone(), 1, 2));
        assert_eq!(
            owner.state.workspace.review.as_ref().unwrap().branch,
            "Turns 1–2"
        );
        owner
            .prepare(Intent::NewThread { project_id: None })
            .unwrap();
        owner.finished(finish(second, 1, 2));
        assert!(owner.state.workspace.review.is_none());
        assert!(owner.state.workspace.diff_request.is_none());
    }
    #[test]
    fn file_reload_and_save_receipts_preserve_edits_and_their_base_revision() {
        let file = m::FileContent {
            path: "/file".into(),
            revision: "v1".into(),
            text: "original".into(),
            size: 8,
        };
        let mut owner = owner(Snapshot {
            workspace: crate::state::Workspace {
                file: Some(Arc::new(file.clone())),
                ..Default::default()
            },
            ..Default::default()
        });
        owner
            .prepare(Intent::EditFile {
                path: file.path.clone(),
                text: "edit".into(),
            })
            .unwrap();
        let mut updated = file.clone();
        updated.revision = "external".into();
        owner.finished(JobResult {
            call: Call::ReadFile(op::ListFiles {
                path: file.path.clone(),
            }),
            result: Ok(Reply::File(updated)),
            complete: None,
            sent: None,
            launched: None,
        });
        let (call, _, _) = owner
            .prepare(Intent::SaveFile {
                path: file.path.clone(),
            })
            .unwrap();
        let Some(Call::WriteFile(written)) = call else {
            panic!("expected file write")
        };
        assert_eq!(written.revision, "v1");
        owner
            .prepare(Intent::EditFile {
                path: file.path.clone(),
                text: "typed during save".into(),
            })
            .unwrap();
        let mut saved = file.clone();
        saved.revision = "v2".into();
        saved.text = written.text.clone();
        owner.finished(JobResult {
            call: Call::WriteFile(written),
            result: Ok(Reply::File(saved)),
            complete: None,
            sent: None,
            launched: None,
        });
        let draft = &owner.state.workspace.file_drafts[&file.path];
        assert_eq!(draft.text, "typed during save");
        assert_eq!(draft.revision, "v2");
    }
    #[test]
    fn late_file_reads_do_not_switch_the_editor_and_canonical_paths_are_accepted() {
        let mut owner = owner(queued_state());
        let read = |path: &str| Intent::ReadFile {
            path: path.into(),
            discard_draft: false,
        };
        let first = owner.prepare(read("/old")).unwrap().0.unwrap();
        let second = owner.prepare(read("/symlink/new")).unwrap().0.unwrap();
        let finish = |call, path: &str| JobResult {
            call,
            result: Ok(Reply::File(m::FileContent {
                path: path.into(),
                revision: "v1".into(),
                text: "file".into(),
                size: 4,
            })),
            complete: None,
            sent: None,
            launched: None,
        };
        owner.finished(finish(first, "/old"));
        assert!(owner.state.workspace.file.is_none());
        owner.finished(finish(second, "/canonical/new"));
        assert_eq!(
            owner.state.workspace.file.as_ref().unwrap().path,
            "/canonical/new"
        );
        let pending = owner.prepare(read("/canonical/new")).unwrap().0.unwrap();
        owner
            .prepare(Intent::NewThread { project_id: None })
            .unwrap();
        owner.finished(finish(pending, "/canonical/new"));
        assert!(owner.state.workspace.file.is_none());
    }
    #[test]
    fn a_queued_edit_ends_when_its_run_starts_and_restores_the_main_draft() {
        let mut owner = owner(queued_state());
        let draft = Draft {
            text: "keep my draft".into(),
            ..owner.state.current_draft()
        };
        owner
            .state
            .drafts
            .insert(owner.state.draft_key(), draft.clone());
        let run = owner
            .state
            .projection()
            .unwrap()
            .runs
            .iter()
            .find(|r| r.status == RunStatus::Queued)
            .unwrap()
            .clone();
        owner
            .prepare(Intent::Queue {
                action: QueueAction::Edit {
                    run_id: run.id.to_string(),
                },
            })
            .unwrap();
        let mut started = run;
        started.status = RunStatus::Starting;
        let id = owner.state.selected_thread.clone().unwrap();
        crate::sync::thread(
            &mut owner.state,
            &id,
            ThreadStreamItem::Event(Box::new(StoredEvent {
                sequence: 2,
                command_id: None,
                event: crate::test_support::event("started", EventPayload::RunUpdated(started)),
            })),
        );
        assert!(owner.state.editing_run.is_none());
        assert_eq!(owner.state.current_draft(), draft);
    }
    #[test]
    fn delayed_submission_receipt_preserves_new_text_and_model_changes() {
        let thread = ThreadId::new("thread").unwrap();
        let mut state = Snapshot {
            selected_thread: Some(thread.clone()),
            ..Snapshot::default()
        };
        let submitted = Draft {
            text: "Original".into(),
            model: "model".into(),
            instance_id: "codex".into(),
            ..Draft::default()
        };
        let mut current = submitted.clone();
        current.text = "Next message".into();
        current.model = "other".into();
        state.drafts.insert("thread".into(), current.clone());
        let mut owner = owner(state);
        owner.finished(JobResult {
            call: Call::DispatchCommand(command(thread.clone(), CommandBody::ThreadMarkUnread)),
            result: Ok(Reply::Receipt(rpc::DispatchReceipt {
                thread_id: thread,
                sequence: 1,
                replayed: false,
            })),
            complete: None,
            sent: Some(("thread".into(), submitted)),
            launched: None,
        });
        assert_eq!(owner.state.current_draft(), current);
    }
    #[test]
    fn late_launch_receipt_does_not_navigate_away_from_another_thread() {
        let selected = ThreadId::new("selected").unwrap();
        let launched = ThreadId::new("launched").unwrap();
        let mut owner = owner(Snapshot {
            selected_thread: Some(selected.clone()),
            ..Snapshot::default()
        });
        owner.finished(JobResult {
            call: Call::DispatchCommand(command(launched.clone(), CommandBody::ThreadMarkUnread)),
            result: Ok(Reply::Receipt(rpc::DispatchReceipt {
                thread_id: launched.clone(),
                sequence: 1,
                replayed: false,
            })),
            complete: None,
            sent: Some(("new:bex:chats".into(), Draft::default())),
            launched: Some(launched),
        });
        assert_eq!(owner.state.selected_thread, Some(selected));
    }
    #[test]
    fn text_typed_during_launch_becomes_the_new_threads_followup() {
        let launched = ThreadId::new("launched").unwrap();
        let mut state = Snapshot::default();
        state.drafts.insert(
            "new:bex:chats".into(),
            Draft {
                text: "Next message".into(),
                ..Draft::default()
            },
        );
        let mut owner = owner(state);
        owner.finished(JobResult {
            call: Call::DispatchCommand(command(launched.clone(), CommandBody::ThreadMarkUnread)),
            result: Ok(Reply::Receipt(rpc::DispatchReceipt {
                thread_id: launched.clone(),
                sequence: 1,
                replayed: false,
            })),
            complete: None,
            sent: Some((
                "new:bex:chats".into(),
                Draft {
                    text: "Original".into(),
                    ..Draft::default()
                },
            )),
            launched: Some(launched.clone()),
        });
        assert_eq!(owner.state.selected_thread, Some(launched));
        assert_eq!(owner.state.current_draft().text, "Next message");
        assert!(owner.state.drafts["new:bex:chats"].text.is_empty());
    }
    #[tokio::test]
    async fn shutdown_closes_snapshot_waiters_and_preserves_local_edits() {
        let store = Store::offline(Snapshot::default());
        store
            .dispatch(Intent::EditDraft {
                base_text: None,
                text: "Unsent".into(),
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(store.snapshot().current_draft().text, "Unsent");
        let mut snapshots = store.subscribe();
        store.close().await.unwrap();
        while snapshots.changed().await.is_ok() {}
        assert!(!snapshots.borrow().connected);
    }
    #[test]
    fn remote_pairing_receipt_observes_the_registered_host() {
        let mut owner = owner(Snapshot::default());
        let (complete, mut receipt) = oneshot::channel();
        owner.finished(JobResult {
            call: Call::RegisterRemote(op::RegisterRemoteHost {
                ticket: "ticket".into(),
                name: "Host".into(),
            }),
            result: Ok(Reply::Remote(m::RemoteHost {
                id: "remote".into(),
                ticket: "ticket".into(),
                name: "Host".into(),
            })),
            complete: Some(complete),
            sent: None,
            launched: None,
        });
        assert_eq!(
            receipt.try_recv().unwrap().unwrap(),
            Outcome::RemoteHostPaired {
                id: "remote".into()
            }
        );
        assert_eq!(owner.snapshots.borrow().remote_hosts[0].id, "remote");
    }
    #[test]
    fn transcription_appends_to_its_original_draft_without_replacing_new_text() {
        let mut state = Snapshot::default();
        state.drafts.insert(
            "thread".into(),
            Draft {
                text: "Typed while recording".into(),
                ..Draft::default()
            },
        );
        let mut owner = owner(state);
        owner.finished(JobResult {
            call: Call::Transcribe(op::Transcribe {
                audio: vec![],
                preparation: None,
            }),
            result: Ok(Reply::Transcription("Dictated words".into())),
            complete: None,
            sent: Some(("thread".into(), Draft::default())),
            launched: None,
        });
        assert_eq!(
            owner.state.drafts["thread"].text,
            "Typed while recording\nDictated words"
        );
        assert!(owner.state.selected_thread.is_none());
    }
}

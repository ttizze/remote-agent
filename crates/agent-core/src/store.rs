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
    pub fn offline_for(snapshot: Snapshot, source: CreationSource) -> Self {
        let (sender, mut receiver) = mpsc::channel(64);
        let (snapshots, updates) = watch::channel(Arc::new(snapshot.clone()));
        let stop = CancellationToken::new();
        let mut owner = Owner {
            state: snapshot,
            snapshots,
            sender: sender.clone(),
            network: None,
            epoch: 0,
            source,
        };
        let stopped = stop.clone();
        tokio::spawn(async move {
            loop {
                let event = tokio::select! {biased;_=stopped.cancelled()=>break,event=receiver.recv()=>match event{Some(e)=>e,None=>break}};
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
            .sender
            .try_send(OwnerEvent::Dispatch(intent, sender))
        {
            let reason = invalid(&error);
            if let OwnerEvent::Dispatch(_, complete) = error.into_inner() {
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
            .sender
            .try_send(OwnerEvent::Dictation(recording.clone(), cancel.clone()));
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
    fn publish(&mut self) {
        self.state.revision += 1;
        self.snapshots.send_replace(Arc::new(self.state.clone()));
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
        let task = tokio::spawn(async move {
            let result = execute(&peer, &call).await;
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
            if let Err(error) = self.job(call, None, None, None) {
                self.state.error = Some(error.to_string());
            }
        }
    }
    async fn handle(&mut self, event: OwnerEvent) -> bool {
        match event {
            OwnerEvent::Attach {
                peer,
                host_name,
                session,
                events,
                complete,
            } => {
                if let Some(network) = self.network.take() {
                    network.peer.close().await;
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
                    self.state.error = Some(error.to_string());
                    self.publish();
                    let _ = complete.send(Err(error));
                }
                return false;
            }
            OwnerEvent::Shell(epoch, item) if epoch == self.epoch => {
                crate::sync::shell(&mut self.state, item, &now())
            }
            OwnerEvent::Thread(epoch, id, item) if epoch == self.epoch => {
                crate::sync::thread(&mut self.state, &id, item)
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
            OwnerEvent::Close(complete) => {
                if let Some(network) = self.network.take() {
                    network.peer.close().await;
                }
                self.state.connected = false;
                self.publish();
                let _ = complete.send(());
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
                self.subscribe_thread();
                target = Some(id);
                body = Some(CommandBody::ThreadVisit {
                    visited_at: timestamp,
                });
                None
            }
            Intent::NewThread { project_id } => {
                self.state.selected_thread = None;
                self.state.selected_project = project_id;
                self.state.editing_run = None;
                self.subscribe_thread();
                None
            }
            Intent::FilterProject { project_id } => {
                self.state.selected_project = project_id;
                None
            }
            Intent::Search { query } => {
                self.state.search = query.clone();
                Some(Call::SearchThreads(rpc::SearchThreads {
                    query,
                    limit: 100,
                }))
            }
            Intent::EditDraft { draft } => {
                self.state.drafts.insert(self.state.draft_key(), draft);
                None
            }
            Intent::Send { behavior } => {
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
                        .next()
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
                        self.state.drafts.insert(self.state.draft_key(), draft);
                        self.state.editing_run = Some(id);
                    }
                    QueueAction::SaveEdit => {
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
                        self.state.editing_run = None;
                        let mut draft = self.state.current_draft();
                        draft.text.clear();
                        self.state.drafts.insert(self.state.draft_key(), draft);
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
                let switched = draft.instance_id != instance_id;
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
            Intent::ListFiles { path } => Some(Call::ListFiles(op::ListFiles { path })),
            Intent::ReadFile {
                path,
                discard_draft,
            } => {
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
                    .and_modify(|draft| draft.text = text.clone())
                    .or_insert(crate::state::FileDraft {
                        text,
                        revision: file.revision.clone(),
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
                    revision: draft.revision,
                    text: draft.text,
                }))
            }
            Intent::ReviewWorkspace { cwd } => {
                Some(Call::ReviewWorkspace(op::ReviewWorkspace { cwd }))
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
        let outcome = match result {
            Err(error) => {
                self.state.error = Some(error.to_string());
                if matches!(
                    error,
                    PeerError::Remote { .. }
                        | PeerError::InvalidMessage(_)
                        | PeerError::InvalidResponse { .. }
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
                Err(error)
            }
            Ok(reply) => {
                self.state.error = None;
                match reply {
                    Reply::Receipt(receipt) => {
                        if let Call::DispatchCommand(command) = &call {
                            self.state
                                .pending_commands
                                .retain(|old| old.command_id != command.command_id);
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
                            if self
                                .state
                                .drafts
                                .get(&key)
                                .is_none_or(|current| current.text == draft.text)
                            {
                                self.state.drafts.entry(key).or_insert(draft).text.clear();
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
                            if let Some(draft) = self.state.drafts.get(&key).cloned() {
                                self.state.drafts.insert(id.to_string(), draft);
                                if let Some(source) = self.state.drafts.get_mut(&key) {
                                    source.text.clear();
                                }
                            }
                            self.state.selected_thread = Some(id.clone());
                            self.subscribe_thread();
                        }
                        let _ = receipt;
                    }
                    Reply::History(id, page) => {
                        if let Some(cache) = self.state.threads.get_mut(&id) {
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
                            } else {
                                p.turn_items.push(*item);
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
                                    crate::session::ProviderKind::Codex => "codex",
                                    crate::session::ProviderKind::Claude => "claude",
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
                            && params.query == self.state.search
                        {
                            self.state.search_matches = matches;
                        }
                    }
                    Reply::ProjectAdded(id) => {
                        self.state.selected_project = Some(id);
                        self.refresh();
                    }
                    Reply::Projects(projects) => self.state.projects = projects,
                    Reply::Files(files) => self.state.workspace.directory = Some(files),
                    Reply::File(file) => {
                        if let Call::WriteFile(written) = &call
                            && let Some(draft) =
                                self.state.workspace.file_drafts.get_mut(&file.path)
                            && draft.revision == written.revision
                        {
                            if draft.text == written.text {
                                self.state.workspace.file_drafts.remove(&file.path);
                            } else {
                                draft.revision = file.revision.clone();
                            }
                        }
                        self.state.workspace.file = Some(file);
                    }
                    Reply::Review(review) => self.state.workspace.review = Some(review),
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
            t.output.push(TerminalOutput {
                sequence: t.sequence,
                data,
                reset_size,
            });
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
        }
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
                file: Some(file.clone()),
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
                draft: Draft {
                    text: "Unsent".into(),
                    ..Draft::default()
                },
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

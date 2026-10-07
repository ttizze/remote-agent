//! The single owner of device state. Every change arrives as an event and
//! leaves a new published snapshot.
use super::{Outcome, Peer, calls::JobResult, invalid, streams::Payload};
use crate::{
    commands::outbox::Delivered,
    peer::PeerError,
    protocol::{self, Call},
    state::*,
    sync::{
        CachedThread, DiskCache, ShellCache, ShellCacheEntry, THREAD_SNAPSHOT_IDLE_TTL_MS,
        ThreadCacheEntry,
    },
    transport,
};
use agent_domain::{Attachment, CommandId, ThreadId, Timestamp, TurnItemId};
use agent_protocol::conversation::{HistoryPage, HistoryRow, ShellLocation};
use agent_protocol::models as m;
use agent_transport::client::Updates;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};

pub type Waiter = oneshot::Sender<Result<Outcome, PeerError>>;

#[derive(Debug, Clone)]
pub struct StoreOptions {
    /// Recorded as the creation source of threads and messages.
    pub creation_source: String,
    /// The disk cache of one Host's shell and threads.
    pub cache_directory: Option<PathBuf>,
    /// Mobile starts on the thread list, so a restored selection is not reopened.
    pub start_on_list: bool,
}
impl Default for StoreOptions {
    fn default() -> Self {
        let mobile = cfg!(any(target_os = "ios", target_os = "android"));
        Self {
            creation_source: if mobile { "mobile" } else { "desktop" }.into(),
            cache_directory: None,
            start_on_list: mobile,
        }
    }
}

pub(super) enum FileTransfer {
    AttachmentDownload {
        id: String,
        destination: String,
    },
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum StreamKey {
    Shell,
    Archive,
    Thread(ThreadId),
    Setup(ThreadId),
    TerminalMetadata,
}
impl StreamKey {
    pub fn location(&self) -> Option<ShellLocation> {
        match self {
            Self::Shell => Some(ShellLocation::Active),
            Self::Archive => Some(ShellLocation::Archived),
            _ => None,
        }
    }
}

pub(super) enum CacheWrite {
    Shell(u64),
    Thread(ThreadId, CachedThread),
}

pub(super) enum Event {
    Intent(Intent, Waiter),
    AppActive,
    Attach {
        peer: Peer,
        host_name: String,
        ticket: transport::Ticket,
        session: transport::Session,
        events: Box<Updates>,
        complete: oneshot::Sender<()>,
    },
    Stream {
        epoch: u64,
        key: StreamKey,
        generation: u64,
        payload: Box<Payload>,
    },
    Resubscribe {
        epoch: u64,
        key: StreamKey,
        generation: u64,
    },
    Delivered {
        epoch: u64,
        id: CommandId,
        delivered: Delivered,
    },
    RetryDelivery {
        epoch: u64,
        id: CommandId,
    },
    History {
        epoch: u64,
        thread: ThreadId,
        cursor: String,
        result: Result<HistoryPage, PeerError>,
    },
    Detail {
        epoch: u64,
        thread: ThreadId,
        item: TurnItemId,
        result: Box<Result<Option<HistoryRow>, PeerError>>,
    },
    CacheWritten(CacheWrite, bool),
    Notification(u64, protocol::Notification),
    Disconnected(u64, String),
    Finished(u64, Box<JobResult>),
    Imported {
        epoch: u64,
        steps: Vec<super::projects::ImportStep>,
        complete: Waiter,
    },
    AttachmentFinished(u64, String, String, Result<Attachment, PeerError>),
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

pub(super) struct Stream {
    pub generation: u64,
    /// Consecutive refused subscriptions; the first healthy item resets it.
    pub failures: u32,
    pub task: Option<AbortOnDropHandle<()>>,
}

pub(super) struct Network {
    pub peer: Peer,
    pub session: transport::Session,
    pub ticket: transport::Ticket,
    pub epoch: u64,
    pub tasks: Vec<AbortOnDropHandle<()>>,
    pub streams: BTreeMap<StreamKey, Stream>,
    /// Cancels the retry loop of a delivery the user stopped.
    pub deliveries: BTreeMap<CommandId, CancellationToken>,
}
impl Network {
    pub fn spawn(&mut self, task: impl Future<Output = ()> + Send + 'static) {
        self.tasks.retain(|task| !task.is_finished());
        self.tasks.push(AbortOnDropHandle::new(tokio::spawn(task)));
    }
}

pub(super) struct Owner {
    pub state: Snapshot,
    snapshots: watch::Sender<Arc<Snapshot>>,
    pub sender: mpsc::Sender<Event>,
    pub network: Option<Network>,
    pub epoch: u64,
    pub generation: u64,
    pub options: StoreOptions,
    pub cache: Option<DiskCache>,
    pub shell_cache: ShellCacheEntry,
    pub thread_caches: BTreeMap<ThreadId, ThreadCacheEntry>,
    /// When each retained thread was last shown.
    pub last_used: BTreeMap<ThreadId, u64>,
    pub visited: BTreeMap<ThreadId, Timestamp>,
    pub waiters: BTreeMap<CommandId, Waiter>,
    pub dictations: BTreeMap<String, CancellationToken>,
    /// The shell, outbox and Working preference the list holds last saw.
    pub observed_list: Option<ObservedList>,
    /// The thread whose setup "Work locally" is cancelling.
    pub work_locally: Option<ThreadId>,
}

pub(super) type ObservedList = (Arc<ShellCache>, Arc<crate::commands::outbox::Outbox>, bool);

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub(super) fn timestamp(ms: u64) -> Timestamp {
    Timestamp::from_millis(ms as i64).expect("current timestamp")
}
pub(super) fn new_id(prefix: &str) -> String {
    format!("{prefix}:{}", uuid::Uuid::new_v4())
}

impl Owner {
    pub fn new(
        mut snapshot: Snapshot,
        options: StoreOptions,
        sender: mpsc::Sender<Event>,
    ) -> (Self, watch::Receiver<Arc<Snapshot>>) {
        snapshot.store_id = uuid::Uuid::new_v4().to_string();
        if options.start_on_list {
            snapshot.selected_thread = None;
            snapshot.editing_run = None;
        }
        let cache = options.cache_directory.clone().map(DiskCache::new);
        if let Some(shell) = cache.as_ref().and_then(DiskCache::load_shell) {
            snapshot.shell = Arc::new(ShellCache::from_cache(shell));
        }
        let (snapshots, updates) = watch::channel(Arc::new(snapshot.clone()));
        let mut owner = Self {
            state: snapshot,
            snapshots,
            sender,
            network: None,
            epoch: 0,
            generation: 0,
            options,
            cache,
            shell_cache: ShellCacheEntry::default(),
            thread_caches: BTreeMap::new(),
            last_used: BTreeMap::new(),
            visited: BTreeMap::new(),
            waiters: BTreeMap::new(),
            dictations: BTreeMap::new(),
            observed_list: None,
            work_locally: None,
        };
        if let Some(thread) = owner.state.selected_thread.clone() {
            owner.open_thread(&thread);
        }
        owner.publish();
        (owner, updates)
    }

    pub async fn run(
        mut self,
        mut input: mpsc::UnboundedReceiver<Event>,
        mut receiver: mpsc::Receiver<Event>,
        stop: CancellationToken,
    ) {
        loop {
            let deadline = self.next_deadline();
            let wake = async move {
                match deadline {
                    Some(at) => {
                        tokio::time::sleep(Duration::from_millis(at.saturating_sub(now_ms()))).await
                    }
                    None => std::future::pending().await,
                }
            };
            let event = tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                event = input.recv() => match event { Some(event) => event, None => break },
                event = receiver.recv() => match event { Some(event) => event, None => break },
                _ = wake => {
                    self.tick();
                    self.publish();
                    continue;
                }
            };
            if self.handle(event).await {
                break;
            }
        }
        self.teardown();
        if let Some(network) = self.network.take() {
            network.peer.close().await;
        }
    }

    pub fn publish(&mut self) {
        self.observe_list();
        self.state.revision += 1;
        self.snapshots.send_replace(Arc::new(self.state.clone()));
    }

    pub fn connected(&self) -> bool {
        self.state.connected && self.network.is_some()
    }

    pub fn network(&mut self) -> Result<&mut Network, PeerError> {
        self.network
            .as_mut()
            .filter(|_| self.state.connected)
            .ok_or_else(|| invalid("Connect to the Host"))
    }

    fn next_deadline(&self) -> Option<u64> {
        let retention = self
            .last_used
            .iter()
            .filter(|(id, _)| self.state.selected_thread.as_ref() != Some(*id))
            .map(|(_, used)| used + THREAD_SNAPSHOT_IDLE_TTL_MS);
        self.shell_cache
            .next_due()
            .into_iter()
            .chain(
                self.thread_caches
                    .values()
                    .filter_map(ThreadCacheEntry::next_due),
            )
            .chain(retention)
            .chain(self.sources_deadline())
            .min()
    }

    /// Writes due cache entries and forgets threads idle past the retention.
    pub fn tick(&mut self) {
        let now = now_ms();
        self.sources_tick(now);
        if let Some((revision, shell)) = self.shell_cache.due(now) {
            self.write_cache(CacheWrite::Shell(revision), move |cache| {
                cache.save_shell(&shell)
            });
        }
        let due: Vec<_> = self
            .thread_caches
            .iter_mut()
            .filter_map(|(id, entry)| entry.due(now).map(|cached| (id.clone(), cached)))
            .collect();
        for (id, cached) in due {
            let (thread, value) = (id.clone(), cached.clone());
            self.write_cache(CacheWrite::Thread(id, cached), move |cache| {
                cache.save_thread(&thread, &value)
            });
        }
        let expired: Vec<_> = self
            .last_used
            .iter()
            .filter(|(id, used)| {
                self.state.selected_thread.as_ref() != Some(*id)
                    && **used + THREAD_SNAPSHOT_IDLE_TTL_MS <= now
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            self.last_used.remove(&id);
            self.store_thread_now(&id);
            self.thread_caches.remove(&id);
            self.state.threads.remove(&id);
        }
    }

    fn write_cache(
        &mut self,
        write: CacheWrite,
        save: impl FnOnce(&DiskCache) -> std::io::Result<()> + Send + 'static,
    ) {
        let Some(cache) = self.cache.clone() else {
            return;
        };
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let stored = tokio::task::spawn_blocking(move || save(&cache).is_ok())
                .await
                .unwrap_or(false);
            let _ = sender.send(Event::CacheWritten(write, stored)).await;
        });
    }

    pub fn cache_written(&mut self, write: CacheWrite, stored: bool) {
        let now = now_ms();
        match write {
            CacheWrite::Shell(revision) => self.shell_cache.written(revision, stored, now),
            CacheWrite::Thread(id, cached) => {
                if let Some(entry) = self.thread_caches.get_mut(&id) {
                    entry.written(cached, stored, now);
                }
            }
        }
    }

    /// Stores a thread leaving memory: the latest settled state not yet saved.
    pub fn store_thread_now(&mut self, id: &ThreadId) {
        let (Some(entry), Some(sync)) =
            (self.thread_caches.get_mut(id), self.state.threads.get(id))
        else {
            return;
        };
        if let Some(cached) = entry.teardown(sync) {
            let thread = id.clone();
            self.write_cache(
                CacheWrite::Thread(id.clone(), cached.clone()),
                move |cache| cache.save_thread(&thread, &cached),
            );
        }
    }

    pub fn flush_shell(&mut self) {
        if let Some((revision, shell)) = self.shell_cache.flush() {
            self.write_cache(CacheWrite::Shell(revision), move |cache| {
                cache.save_shell(&shell)
            });
        }
    }

    fn teardown(&mut self) {
        let Some(cache) = self.cache.clone() else {
            return;
        };
        if let Some((_, shell)) = self.shell_cache.flush() {
            let _ = cache.save_shell(&shell);
        }
        for (id, entry) in &mut self.thread_caches {
            if let Some(sync) = self.state.threads.get(id)
                && let Some(cached) = entry.teardown(sync)
            {
                let _ = cache.save_thread(id, &cached);
            }
        }
    }

    pub async fn handle(&mut self, event: Event) -> bool {
        match event {
            Event::Intent(intent, complete) => self.intent(intent, complete),
            Event::AppActive => self.app_became_active(),
            Event::Attach {
                peer,
                host_name,
                ticket,
                session,
                events,
                complete,
            } => {
                self.attach(peer, host_name, ticket, session, *events);
                let _ = complete.send(());
            }
            Event::Stream {
                epoch,
                key,
                generation,
                payload,
            } if epoch == self.epoch => self.stream_item(key, generation, *payload),
            Event::Resubscribe {
                epoch,
                key,
                generation,
            } if epoch == self.epoch => self.resubscribe(key, generation),
            Event::Delivered {
                epoch,
                id,
                delivered,
            } if epoch == self.epoch => self.delivered(id, delivered),
            Event::RetryDelivery { epoch, id } if epoch == self.epoch => {
                self.state_outbox().retry(&id);
                self.drain();
            }
            Event::History {
                epoch,
                thread,
                cursor,
                result,
            } if epoch == self.epoch => self.history_result(&thread, &cursor, result),
            Event::Detail {
                epoch,
                thread,
                item,
                result,
            } if epoch == self.epoch => self.detail_result(&thread, &item, *result),
            Event::CacheWritten(write, stored) => self.cache_written(write, stored),
            Event::Notification(epoch, notification) if epoch == self.epoch => {
                self.notification(notification)
            }
            Event::Disconnected(epoch, error) if epoch == self.epoch => self.disconnected(error),
            Event::Finished(epoch, result) if epoch == self.epoch => self.finished(*result),
            Event::Imported {
                epoch,
                steps,
                complete,
            } if epoch == self.epoch => self.imported(steps, complete),
            Event::Imported { complete, .. } => {
                self.state.session_import.importing = false;
                let _ = complete.send(Err(invalid("Host connection closed")));
            }
            Event::AttachmentFinished(epoch, key, id, result) if epoch == self.epoch => {
                self.attachment_finished(key, id, result)
            }
            Event::Resume {
                endpoint,
                ticket,
                complete,
            } => self.resume(endpoint, ticket, complete),
            Event::Close(complete) => {
                self.interrupt_uploads();
                self.flush_shell();
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
            Event::Browser(request, complete) => self.browser(request, complete),
            Event::Dictation(id, cancel) => self.dictation(id, cancel),
            Event::Transfer(transfer, complete) => self.transfer(transfer, complete),
            Event::Performance(performance) => {
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

    pub fn state_outbox(&mut self) -> &mut crate::commands::outbox::Outbox {
        Arc::make_mut(&mut self.state.outbox)
    }

    fn attach(
        &mut self,
        peer: Peer,
        host_name: String,
        ticket: transport::Ticket,
        session: transport::Session,
        events: Updates,
    ) {
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
        let epoch = self.epoch;
        let mut network = Network {
            peer,
            session,
            ticket,
            epoch,
            tasks: vec![],
            streams: BTreeMap::new(),
            deliveries: BTreeMap::new(),
        };
        network.spawn(super::notifications(events, epoch, self.sender.clone()));
        self.network = Some(network);
        Arc::make_mut(&mut self.state.shell).connecting();
        if let Some(archived) = self.state.archived.as_mut() {
            Arc::make_mut(archived).connecting();
        }
        for sync in self.state.threads.values_mut() {
            Arc::make_mut(sync).connecting();
        }
        self.subscribe_shell(ShellLocation::Active);
        if self.state.archived.is_some() {
            self.subscribe_shell(ShellLocation::Archived);
        }
        if let Some(thread) = self.state.selected_thread.clone() {
            self.subscribe_thread(&thread);
        }
        self.subscribe_terminal_metadata();
        self.refresh();
        self.state_outbox().reconnected();
        self.drain();
    }

    fn disconnected(&mut self, error: String) {
        self.interrupt_uploads();
        self.state.connected = false;
        self.state.error = Some(error);
        Arc::make_mut(&mut self.state.shell).disconnected();
        if let Some(archived) = self.state.archived.as_mut() {
            Arc::make_mut(archived).disconnected();
        }
        for sync in self.state.threads.values_mut() {
            Arc::make_mut(sync).disconnected();
        }
        if let Some(network) = self.network.as_mut() {
            network.streams.clear();
        }
        self.flush_shell();
        for terminal in self.state.terminals.values_mut() {
            if matches!(
                terminal.phase,
                TerminalPhase::Starting | TerminalPhase::Running
            ) {
                terminal.phase = TerminalPhase::Suspended;
            }
        }
    }

    fn interrupt_uploads(&mut self) {
        for draft in self.state.drafts.values_mut() {
            for attachment in &mut draft.attachments {
                if attachment.status == "uploading" {
                    attachment.status = "failed".into();
                    attachment.error = Some("Upload interrupted. Retry to continue.".into());
                }
            }
        }
    }

    fn resume(
        &mut self,
        endpoint: transport::Endpoint,
        ticket: transport::Ticket,
        complete: oneshot::Sender<Option<crate::diagnostics::ConnectionPerformance>>,
    ) {
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
                self.terminal_output(
                    &handle,
                    data,
                    Some(agent_protocol::operations::TerminalSize { cols, rows }),
                );
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
            protocol::Notification::TerminalDetached { .. } => {}
            protocol::Notification::TerminalClosed { handle } => self.terminal_closed(&handle),
        }
    }

    pub fn terminal_output(
        &mut self,
        handle: &str,
        data: Vec<u8>,
        reset_size: Option<agent_protocol::operations::TerminalSize>,
    ) {
        if let Some(t) = self.state.terminals.get_mut(handle) {
            t.sequence += 1;
            if reset_size.is_some() {
                t.clear_output();
            }
            t.output_bytes += data.len();
            t.output.push_back(Arc::new(TerminalOutput {
                sequence: t.sequence,
                data,
                reset_size,
            }));
            while t.output_bytes > 8 * 1024 * 1024 && t.output.len() > 1 {
                t.output_bytes -= t.output.pop_front().expect("nonempty output").data.len();
            }
        }
    }
}

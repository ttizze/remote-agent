//! The single owner of device state. Every change arrives as an event and
//! leaves a new published snapshot.
use super::{Outcome, Peer, calls::JobResult, disk::Disk, invalid, streams::Payload};
use crate::{
    commands::outbox::Delivered,
    peer::PeerError,
    persistence::StateWriter,
    protocol::{self, Call},
    state::*,
    sync::{
        CachedThread, DiskCache, ShellCache, ShellCacheEntry, THREAD_SNAPSHOT_IDLE_TTL_MS,
        ThreadCacheEntry,
    },
    transport,
};
use agent_domain::{Attachment, CommandId, ThreadId, Timestamp, TurnItemId};
use agent_protocol::conversation::{HistoryPage, ShellLocation};
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
    /// The device state file of one Host, which the store keeps written.
    pub state_file: Option<PathBuf>,
    /// Mobile starts on the thread list, so a restored selection is not reopened.
    pub start_on_list: bool,
}
impl Default for StoreOptions {
    fn default() -> Self {
        let mobile = cfg!(any(target_os = "ios", target_os = "android"));
        Self {
            creation_source: if mobile { "mobile" } else { "desktop" }.into(),
            cache_directory: None,
            state_file: None,
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
    Preview(ThreadId),
    TerminalMetadata,
    Keybindings,
    VcsStatus(String),
    GitAction(String),
    ScheduledTasks,
    Awareness,
    Background,
    Device(ThreadId),
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

/// A file write the owner asked for.
pub(super) enum Written {
    Shell(u64),
    Thread(ThreadId, CachedThread),
    /// The device state.
    Device,
}

pub(super) enum Event {
    Intent(Intent, Waiter),
    ReportHostPower(agent_protocol::background::HostPowerSnapshot, Waiter),
    AppActive,
    Attach {
        peer: Peer,
        host_name: String,
        environment: m::EnvironmentDescriptor,
        awareness_registration: m::AwarenessRegistration,
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
        result: Box<Result<Option<(agent_domain::Item, Option<agent_domain::Task>)>, PeerError>>,
    },
    Written(Written, bool),
    /// Resolves once the latest device state is written.
    Flush(oneshot::Sender<()>),
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

/// The device state file and its pending writes.
pub(super) struct DeviceFile {
    path: PathBuf,
    pub writer: StateWriter,
    flushes: Vec<oneshot::Sender<()>>,
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
    pub device: Option<DeviceFile>,
    /// Started with the first file write.
    disk: Option<Disk>,
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
    /// A selected thread's folded stream has changed but its configured
    /// publication boundary has not arrived yet.
    pub(super) stream_publish_pending: bool,
    pub(super) stream_publish_deferred: bool,
    /// The last attempted account/quota refresh, retained across connection epochs.
    pub(super) usage_refresh_last_attempt_ms: Option<u64>,
    /// Any account request currently running on this connection epoch.
    pub(super) accounts_refresh_in_flight_epoch: Option<u64>,
    /// Prevents repeated capacity RPCs while one automatic draft is waiting.
    pub(super) load_balancing_resources_in_flight: bool,
}

pub(super) type ObservedList = (Arc<ShellCache>, Arc<crate::commands::outbox::Outbox>, bool);

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub(super) const USAGE_REFRESH_INTERVAL_MS: u64 = 5 * 60 * 1_000;

fn usage_refresh_due(last_attempt_ms: Option<u64>, in_flight: bool, now: u64) -> bool {
    !in_flight
        && last_attempt_ms.map_or(true, |last| {
            now.saturating_sub(last) >= USAGE_REFRESH_INTERVAL_MS
        })
}

fn usage_refresh_deadline(last_attempt_ms: Option<u64>, in_flight: bool, now: u64) -> Option<u64> {
    (!in_flight).then(|| {
        last_attempt_ms
            .map(|last| last.saturating_add(USAGE_REFRESH_INTERVAL_MS))
            .unwrap_or(now)
    })
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
        let device = options.state_file.clone().map(|path| DeviceFile {
            path,
            writer: StateWriter::restored(&snapshot),
            flushes: vec![],
        });
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
            device,
            disk: None,
            shell_cache: ShellCacheEntry::default(),
            thread_caches: BTreeMap::new(),
            last_used: BTreeMap::new(),
            visited: BTreeMap::new(),
            waiters: BTreeMap::new(),
            dictations: BTreeMap::new(),
            observed_list: None,
            work_locally: None,
            stream_publish_pending: false,
            stream_publish_deferred: false,
            usage_refresh_last_attempt_ms: None,
            accounts_refresh_in_flight_epoch: None,
            load_balancing_resources_in_flight: false,
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
        let closing = loop {
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
                _ = stop.cancelled() => break None,
                event = input.recv() => match event { Some(event) => event, None => break None },
                event = receiver.recv() => match event { Some(event) => event, None => break None },
                _ = wake => {
                    self.tick();
                    self.publish();
                    continue;
                }
            };
            if let Event::Close(complete) = event {
                break Some(complete);
            }
            self.handle(event).await;
        };
        // File writes no longer report back.
        drop((input, receiver));
        self.interrupt_uploads();
        self.state.connected = false;
        self.publish();
        self.teardown().await;
        if let Some(network) = self.network.take() {
            network.peer.close().await;
        }
        if let Some(complete) = closing {
            let _ = complete.send(());
        }
    }

    pub fn publish(&mut self) {
        self.retry_pending_diff();
        self.observe_list();
        if let Some(device) = &mut self.device {
            device.writer.observe(&self.state, now_ms());
        }
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
        let usage_refresh = if self.connected() {
            usage_refresh_deadline(
                self.usage_refresh_last_attempt_ms,
                self.accounts_refresh_in_flight_epoch == Some(self.epoch),
                now_ms(),
            )
        } else {
            None
        };
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
            .chain(usage_refresh)
            .chain(
                self.device
                    .as_ref()
                    .and_then(|device| device.writer.next_due()),
            )
            .min()
    }

    /// Writes due cache entries and forgets threads idle past the retention.
    pub fn tick(&mut self) {
        let now = now_ms();
        self.refresh_accounts_if_due(now);
        self.sources_tick(now);
        self.write_device_state(now);
        if let Some((revision, shell)) = self.shell_cache.due(now) {
            self.write_cache(Written::Shell(revision), move |cache| {
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
            self.write_cache(Written::Thread(id, cached), move |cache| {
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

    /// Runs a file job after every one asked before it.
    pub fn write_file(&mut self, job: impl FnOnce() -> Option<Event> + Send + 'static) {
        let sender = self.sender.clone();
        self.disk.get_or_insert_with(|| Disk::new(sender)).run(job);
    }

    fn write_cache(
        &mut self,
        write: Written,
        save: impl FnOnce(&DiskCache) -> std::io::Result<()> + Send + 'static,
    ) {
        let Some(cache) = self.cache.clone() else {
            return;
        };
        self.write_file(move || Some(Event::Written(write, save(&cache).is_ok())));
    }

    /// Starts the due write of the device state.
    fn write_device_state(&mut self, now: u64) {
        let Some(device) = &mut self.device else {
            return;
        };
        if !device.writer.due(&self.state, now) {
            return;
        }
        let (path, snapshot) = (device.path.clone(), self.state.clone());
        self.write_file(move || {
            let stored = crate::persistence::save(&path, &snapshot).is_ok();
            Some(Event::Written(Written::Device, stored))
        });
    }

    pub fn written(&mut self, write: Written, stored: bool) {
        let now = now_ms();
        match write {
            Written::Shell(revision) => self.shell_cache.written(revision, stored, now),
            Written::Thread(id, cached) => {
                if let Some(entry) = self.thread_caches.get_mut(&id) {
                    entry.written(cached, stored, now);
                }
            }
            Written::Device => {
                let Some(device) = &mut self.device else {
                    return;
                };
                device.writer.written(stored, now);
                if !stored || !device.writer.changed() {
                    for flush in device.flushes.drain(..) {
                        let _ = flush.send(());
                    }
                }
                if !stored {
                    self.state.error = Some("Could not save this device's state.".into());
                }
                self.drain();
            }
        }
    }

    /// Writes the device state now; `complete` resolves once it is written.
    fn flush(&mut self, complete: oneshot::Sender<()>) {
        let Some(device) = &mut self.device else {
            let _ = complete.send(());
            return;
        };
        if !device.writer.changed() && !device.writer.writing() {
            let _ = complete.send(());
            return;
        }
        device.writer.hurry(now_ms());
        device.flushes.push(complete);
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
            self.write_cache(Written::Thread(id.clone(), cached.clone()), move |cache| {
                cache.save_thread(&thread, &cached)
            });
        }
    }

    pub fn flush_shell(&mut self) {
        if let Some((revision, shell)) = self.shell_cache.flush() {
            self.write_cache(Written::Shell(revision), move |cache| {
                cache.save_shell(&shell)
            });
        }
    }

    /// Writes what is not saved yet after the writes already asked, and waits
    /// for all of them.
    pub(super) async fn teardown(&mut self) {
        self.flush_shell();
        let threads: Vec<_> = self.thread_caches.keys().cloned().collect();
        for thread in threads {
            self.store_thread_now(&thread);
        }
        if let Some(device) = &self.device
            && device.writer.changed()
        {
            let (path, snapshot) = (device.path.clone(), self.state.clone());
            self.write_file(move || {
                let _ = crate::persistence::save(&path, &snapshot);
                None
            });
        }
        if let Some(disk) = self.disk.take() {
            disk.finish().await;
        }
    }

    pub async fn handle(&mut self, event: Event) {
        self.stream_publish_deferred = false;
        match event {
            Event::Intent(intent, complete) => self.intent(intent, complete),
            Event::ReportHostPower(snapshot, complete) => {
                self.job(Call::ReportHostPowerState(snapshot), Some(complete), None)
            }
            Event::AppActive => self.app_became_active(),
            Event::Attach {
                peer,
                host_name,
                environment,
                awareness_registration,
                ticket,
                session,
                events,
                complete,
            } => {
                self.attach(
                    peer,
                    host_name,
                    environment,
                    awareness_registration,
                    ticket,
                    session,
                    *events,
                );
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
            Event::Written(write, stored) => self.written(write, stored),
            Event::Flush(complete) => self.flush(complete),
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
        if self.stream_publish_deferred && self.stream_publish_pending {
            return;
        }
        self.stream_publish_pending = false;
        self.publish();
    }

    pub fn state_outbox(&mut self) -> &mut crate::commands::outbox::Outbox {
        Arc::make_mut(&mut self.state.outbox)
    }

    /// Gives new-thread composer edits a real draft identity before they are
    /// written. The initial composer can be visible before an explicit
    /// `NewThread` intent, so its first edit must still become an independent
    /// pending draft.
    pub(super) fn ensure_new_thread_draft(&mut self) {
        if self.state.selected_thread.is_some() {
            return;
        }
        let key = self.state.new_thread_draft_key();
        if self.state.open_new_thread_draft.is_some()
            && self
                .state
                .drafts
                .get(&key)
                .is_some_and(|draft| draft.project_id.is_some())
        {
            return;
        }
        let project = self.state.selected_project.clone();
        self.state
            .begin_new_thread_draft(new_id("new"), project, now_ms() as i64);
    }

    pub(super) fn accounts_refresh_in_flight(&self) -> bool {
        self.accounts_refresh_in_flight_epoch == Some(self.epoch)
    }

    pub(super) fn refresh_load_balancing_resources(&mut self) {
        if !self.state.connected || self.load_balancing_resources_in_flight {
            return;
        }
        self.load_balancing_resources_in_flight = true;
        self.job(
            Call::ReadHostResources(agent_protocol::background::ReadHostResources {}),
            None,
            None,
        );
    }

    pub(super) fn refresh_accounts_if_due(&mut self, now: u64) {
        if !self.connected()
            || !usage_refresh_due(
                self.usage_refresh_last_attempt_ms,
                self.accounts_refresh_in_flight(),
                now,
            )
        {
            return;
        }
        // Record the attempt before spawning the request. A failed request keeps
        // the same five-minute throttle, matching the widget refresher contract.
        self.usage_refresh_last_attempt_ms = Some(now);
        self.job(Call::ListAccounts(m::Empty {}), None, None);
    }

    pub(super) fn account_refresh_finished(&mut self) {
        self.accounts_refresh_in_flight_epoch = None;
        self.refresh_accounts_if_due(now_ms());
    }

    fn attach(
        &mut self,
        peer: Peer,
        host_name: String,
        environment: m::EnvironmentDescriptor,
        awareness_registration: m::AwarenessRegistration,
        ticket: transport::Ticket,
        session: transport::Session,
        events: Updates,
    ) {
        if let Some(network) = self.network.take() {
            self.abandon_requests();
            tokio::spawn(async move {
                let peer = network.peer.clone();
                drop(network);
                peer.close().await;
            });
        }
        self.epoch += 1;
        // A task from the replaced Network can never complete this epoch.
        self.accounts_refresh_in_flight_epoch = None;
        self.load_balancing_resources_in_flight = false;
        self.state.host_resources_received_at_ms = None;
        self.state.connected = true;
        self.state.host_name = Some(host_name);
        self.state.environment = Some(environment);
        self.state.awareness = None;
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
        self.subscribe_git_statuses();
        self.subscribe_terminal_metadata();
        self.subscribe_keybindings();
        self.subscribe_scheduled_tasks();
        if self
            .state
            .environment
            .as_ref()
            .is_some_and(|environment| environment.capabilities.agent_activity_publishing)
        {
            self.subscribe_awareness();
            self.job(Call::RegisterAwareness(awareness_registration), None, None);
        }
        self.subscribe_background();
        self.refresh_after_attach();
        self.sources_tick(now_ms());
        if let Some(request) = self
            .state
            .sources
            .diff_preview
            .as_ref()
            .filter(|entry| entry.result.is_none())
            .map(|entry| entry.request.clone())
        {
            self.job(Call::DiffPreview(request), None, None);
        }
        self.read_diff_files();
        self.state_outbox().reconnected();
        self.drain();
    }

    /// A replaced connection's requests never answer: nothing waits for them
    /// any more, and a provider command scan is asked again.
    pub(super) fn abandon_requests(&mut self) {
        let now = now_ms();
        if let Some(request) = self.state.search_request.as_mut() {
            request.due_at_ms = Some(now);
        }
        if self.state.sources.entries.wanted.is_some() {
            self.state.sources.entries.due_at_ms = Some(now);
        }
        self.state.sources.diff_generation = self.state.sources.diff_generation.wrapping_add(1);
        let sources = &mut self.state.sources;
        for entry in sources.provider_commands.values_mut() {
            if entry.in_flight {
                entry.in_flight = false;
                entry.retry_at_ms = Some(now);
            }
        }
        for entry in sources.refs.values_mut() {
            entry.in_flight = false;
        }
        if let Some(entry) = sources.diff_files.as_mut() {
            let mut retry = entry.superseded.clone();
            for (path, patch) in &mut entry.patches {
                if patch.in_flight {
                    patch.in_flight = false;
                    retry.insert(path.clone());
                }
            }
            entry.superseded.clear();
            for path in retry {
                if !entry.queue.contains(&path) {
                    entry.queue.push(path);
                }
            }
            entry.revision += 1;
        }
        for icon in self.state.project_icons.values_mut() {
            if icon.in_flight {
                icon.in_flight = false;
                icon.version.clear();
            }
        }
        let import = &mut self.state.session_import;
        import.scan_pending = false;
        import.importing = false;
        for sync in self.state.threads.values_mut() {
            if sync.has_pending_requests() {
                Arc::make_mut(sync).requests_abandoned();
            }
        }
    }

    fn disconnected(&mut self, error: String) {
        self.interrupt_uploads();
        self.abandon_requests();
        self.accounts_refresh_in_flight_epoch = None;
        self.load_balancing_resources_in_flight = false;
        self.state.host_resources_received_at_ms = None;
        self.state.connected = false;
        self.state.awareness = None;
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

#[cfg(test)]
mod tests {
    use super::{USAGE_REFRESH_INTERVAL_MS, usage_refresh_deadline, usage_refresh_due};

    #[test]
    fn usage_refresh_matches_attempt_throttle_and_clock_rollback() {
        assert!(usage_refresh_due(None, false, 0));
        assert!(!usage_refresh_due(
            Some(1_000),
            false,
            1_000 + USAGE_REFRESH_INTERVAL_MS - 1
        ));
        assert!(usage_refresh_due(
            Some(1_000),
            false,
            1_000 + USAGE_REFRESH_INTERVAL_MS
        ));
        assert!(!usage_refresh_due(Some(1_000), false, 999));
        assert!(!usage_refresh_due(
            Some(1_000),
            true,
            1_000 + USAGE_REFRESH_INTERVAL_MS
        ));
    }

    #[test]
    fn usage_refresh_deadline_is_suppressed_while_a_request_is_in_flight() {
        assert_eq!(usage_refresh_deadline(None, false, 50), Some(50));
        assert_eq!(
            usage_refresh_deadline(Some(100), false, 100),
            Some(100 + USAGE_REFRESH_INTERVAL_MS)
        );
        assert_eq!(
            usage_refresh_deadline(Some(100), true, 100 + USAGE_REFRESH_INTERVAL_MS),
            None
        );
    }
}

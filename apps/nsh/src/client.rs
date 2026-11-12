//! UI intents and publications cross this boundary; ClientSessions owns Host state.
use crate::l10n::tr;
use crate::storage::{self, HandedKeys, Host, Storage, error};
use agent_core::{
    client::{
        AgentError,
        persistence::DevicePreferences,
        session::{ClientBuild, ClientSession, ClientSessions},
    },
    presentation::view::Presentation,
    state::Intent,
    store::{BrowserSession, BrowserUpdate},
};
use agent_local_host::{LocalHostState, launch::HostScope};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

mod panes;
#[cfg(desktop)]
mod ssh;
mod transfers;
pub(crate) use transfers::{AttachmentSource, LocalFile};

pub(crate) enum Update {
    Hosts(Vec<Host>),
    AttachmentsFinished(uuid::Uuid),
    Presentation {
        host: String,
        value: Arc<Presentation>,
    },
    Error(String),
    Paired(Result<String, String>),
    PanePresentation {
        id: u64,
        value: Arc<Presentation>,
    },
    SideChatReady {
        id: u64,
        result: Result<Arc<Presentation>, String>,
    },
}

pub(crate) struct Client {
    runtime: tokio::runtime::Handle,
    jobs: tokio_util::task::TaskTracker,
    /// Cancelled once the client shuts down: its work ends.
    closing: CancellationToken,
    sessions: Arc<ClientSessions>,
    /// The device's preferences every Host shares: models, editor and language.
    pub device: Arc<DevicePreferences>,
    root: PathBuf,
    hosts: tokio::sync::Mutex<Vec<Host>>,
    /// This machine's Host, started from the app bundle when it is stopped.
    local: Option<HostScope>,
    local_startup: tokio::sync::Mutex<()>,
    /// The keys WSL distros' Hosts handed over, which their sessions connect with.
    handed: Arc<HandedKeys>,
    updates: async_channel::Sender<Update>,
    panes: std::sync::Mutex<std::collections::BTreeMap<u64, Arc<panes::Pane>>>,
    owns_sessions: bool,
}

pub(crate) struct BorrowedSession {
    pub host: String,
    pub session: Arc<ClientSession>,
    pub pane: Option<Arc<agent_core::client::native::AgentPane>>,
}

type Started = (
    tokio::runtime::Runtime,
    Arc<Client>,
    async_channel::Receiver<Update>,
);

pub(crate) const LOCAL: &str = "local";

impl Client {
    #[cfg(test)]
    pub fn new(root: PathBuf, local: Option<HostScope>) -> Result<Started, AgentError> {
        Self::start(root, local, None)
    }

    pub(super) fn start(
        root: PathBuf,
        local: Option<HostScope>,
        borrowed: Option<BorrowedSession>,
    ) -> Result<Started, AgentError> {
        let runtime = tokio::runtime::Runtime::new().map_err(error)?;
        let (updates, incoming) = async_channel::unbounded();
        let device = DevicePreferences::new(Arc::new(crate::storage::DeviceStorage {
            root: root.clone(),
        }));
        let client = Arc::new(Self {
            runtime: runtime.handle().clone(),
            jobs: Default::default(),
            closing: CancellationToken::new(),
            // Chats lists only standalone chats, as the desktop client's sidebar.
            sessions: ClientSessions::new(
                false,
                ClientBuild {
                    number: agent_core::updates::build_number(),
                    revision: option_env!("AGENT_BUILD_REVISION")
                        .unwrap_or("development")
                        .into(),
                },
                "Bex NSH".into(),
                device.clone(),
            ),
            device,
            root,
            hosts: Default::default(),
            local,
            local_startup: Default::default(),
            handed: Default::default(),
            updates,
            panes: Default::default(),
            owns_sessions: borrowed.is_none(),
        });
        let owner = client.clone();
        if let Some(borrowed) = borrowed {
            client
                .sessions
                .borrow(borrowed.host.clone(), borrowed.session.clone());
            if let Some(pane) = &borrowed.pane {
                client.attach_pane(borrowed.host.clone(), pane.clone());
            }
            client.spawn(async move {
                let ready = tokio::select! {
                    _ = owner.closing.cancelled() => return,
                    ready = borrowed.session.clone().ready() => ready,
                };
                if let Err(reason) = ready {
                    return owner.report(reason);
                }
                owner.observe_surface(borrowed.host, borrowed.session, borrowed.pane);
            });
        } else {
            client.spawn(async move {
                if let Err(reason) = owner.restore().await {
                    owner.report(reason);
                }
            });
        }
        Ok((runtime, client, incoming))
    }

    fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        self.jobs.spawn_on(work, &self.runtime);
    }

    fn report(&self, reason: impl std::fmt::Display) {
        let _ = self.updates.try_send(Update::Error(reason.to_string()));
    }

    async fn restore(self: &Arc<Self>) -> Result<(), AgentError> {
        let path = self.root.join("hosts.json");
        let bytes = tokio::task::spawn_blocking(move || storage::read(&path))
            .await
            .map_err(error)??;
        let mut hosts: Vec<Host> = if bytes.is_empty() {
            Vec::new()
        } else {
            serde_json::from_slice(&bytes).map_err(error)?
        };
        hosts.retain(|host| saved(&host.id));
        if let Some(scope) = &self.local {
            hosts.insert(0, local_profile(scope).await?);
            // WSL is optional: saved Hosts restore without it.
            match agent_local_host::wsl::distributions().await {
                Ok(distros) => hosts.extend(distros.iter().map(|distro| Host {
                    id: agent_local_host::wsl::host_id(distro),
                    name: agent_local_host::wsl::host_name(distro),
                    ticket: String::new(),
                    local_directory: None,
                })),
                Err(reason) => self.report(reason),
            }
        }
        let mut registry = self.hosts.lock().await;
        if self.closing.is_cancelled() {
            return Ok(());
        }
        *registry = hosts.clone();
        self.updates
            .send(Update::Hosts(hosts.clone()))
            .await
            .map_err(error)?;
        for host in hosts {
            self.open(host);
        }
        Ok(())
    }

    fn open(self: &Arc<Self>, host: Host) {
        let storage = Arc::new(Storage {
            root: self.root.clone(),
            host: host.clone(),
            handed: self.handed.clone(),
        });
        let session = self
            .sessions
            .open(host.id.clone(), storage.clone(), storage);
        let owner = self.clone();
        self.spawn(async move {
            match session.clone().ready().await {
                Ok(_) => {
                    owner.observe(host.id.clone(), session.clone());
                    let distro = agent_local_host::wsl::host_distro(&host.id).is_some();
                    if distro || host.id == LOCAL {
                        owner.recover(session.clone(), host.clone());
                    }
                    // A distro's Host starts only once it is chosen: starting
                    // WSL is not free.
                    if !distro {
                        owner.resume(session, host).await;
                    }
                }
                Err(reason) => {
                    // Removing or replacing this Host cancels restoration too.
                    if owner
                        .sessions
                        .session(host.id)
                        .is_some_and(|current| Arc::ptr_eq(&current, &session))
                    {
                        owner.report(reason);
                    }
                }
            }
        });
    }

    async fn resume(&self, session: Arc<ClientSession>, host: Host) {
        if let Some(failure) = self.reach(session, host).await {
            self.report(failure);
        }
    }

    /// Starts and reaches `host`; the failure Core did not record, if any.
    async fn reach(&self, session: Arc<ClientSession>, host: Host) -> Option<String> {
        // A disconnected Host is neither started nor reached.
        if !session.snapshot().connection_enabled() {
            return None;
        }
        let ticket = match self.ticket(host).await {
            Ok(ticket) => ticket,
            Err(reason) => return Some(reason.to_string()),
        };
        let message = session.clone().resume(ticket).await.err()?.to_string();
        // Identity and ticket preparation can fail before Core records an attempt.
        (!session.store().is_some_and(|store| {
            store.snapshot().connection.connection_failure.as_deref() == Some(&message)
        }))
        .then_some(message)
    }

    /// Core reconnects to a Host that runs; this machine's own Host and its
    /// distros' stop with it or with WSL, and only this client starts them
    /// again. Once reached, a Host lost while it is to be connected is
    /// started and reached again, less often while that keeps failing.
    fn recover(self: &Arc<Self>, session: Arc<ClientSession>, host: Host) {
        const CHECK: Duration = Duration::from_secs(1);
        const LONGEST: Duration = Duration::from_secs(8);
        let owner = self.clone();
        self.spawn(async move {
            let (mut reached, mut wait, mut last) = (false, CHECK, None);
            loop {
                tokio::select! {
                    _ = owner.closing.cancelled() => return,
                    _ = tokio::time::sleep(wait) => {}
                }
                let snapshot = session.snapshot();
                let connected = snapshot.connection.connected;
                reached |= connected;
                last = last.filter(|_| !connected);
                if connected || !reached || !snapshot.connection_enabled() {
                    wait = CHECK;
                    continue;
                }
                // A failure shows once, not again on each later attempt.
                let failure = owner.reach(session.clone(), host.clone()).await;
                if let Some(reason) = failure.as_ref().filter(|_| failure != last) {
                    owner.report(reason);
                }
                last = failure;
                wait = (wait * 2).min(LONGEST);
            }
        });
    }

    /// A paired Host's saved ticket, or this machine's Host's current one,
    /// starting it when it is stopped.
    async fn ticket(&self, host: Host) -> Result<String, AgentError> {
        if let Some((distro, scope)) =
            agent_local_host::wsl::host_distro(&host.id).zip(self.local.as_ref())
        {
            // Only this instance's first start may replace an older WSL Host.
            let executable = agent_local_host::launch::host_executable().map_err(error)?;
            let access = agent_local_host::wsl::bootstrap(
                distro,
                &executable,
                scope,
                !self.handed.has(&host.id),
            )
            .await
            .map_err(error)?;
            self.handed.hand(&host.id, access.secret);
            return Ok(access.ticket.to_string());
        }
        let Some(scope) = self.local.as_ref().filter(|_| host.id == LOCAL) else {
            return Ok(host.ticket);
        };
        let _startup = self.local_startup.lock().await;
        let executable = agent_local_host::launch::host_executable().map_err(error)?;
        let (directory, ticket) =
            agent_local_host::launch::connect(scope, &executable, |directory, ticket| async {
                Ok((directory, ticket))
            })
            .await
            .map_err(error)?;
        // The session reads its identity from the directory it was opened with.
        let same = |path: &std::path::Path| std::fs::canonicalize(path).ok();
        if host.local_directory.as_deref().and_then(same) != same(&directory) {
            return Err(error(tr(
                "このPCのホストが変わりました。Bexを再起動してください。",
            )));
        }
        Ok(ticket.to_string())
    }

    fn observe(self: &Arc<Self>, host: String, session: Arc<ClientSession>) {
        self.observe_surface(host, session, None);
    }

    fn observe_surface(
        self: &Arc<Self>,
        host: String,
        session: Arc<ClientSession>,
        pane: Option<Arc<agent_core::client::native::AgentPane>>,
    ) {
        let owner = self.clone();
        self.spawn(async move {
            let Some(store) = session.store() else {
                return;
            };
            let mut value = match &pane {
                Some(pane) => match pane.presentation() {
                    Ok(value) => value,
                    Err(reason) => return owner.report(reason),
                },
                None => store.presentation(),
            };
            loop {
                if owner
                    .updates
                    .send(Update::Presentation {
                        host: host.clone(),
                        value: value.clone(),
                    })
                    .await
                    .is_err()
                {
                    break;
                }
                let next = async {
                    match &pane {
                        Some(pane) => pane.next_presentation(value).await,
                        None => store.next_presentation(value).await,
                    }
                };
                match tokio::select! {
                    _ = owner.closing.cancelled() => break,
                    result = next => result,
                } {
                    Ok(next) => value = next,
                    Err(_) => break,
                }
            }
        });
    }

    pub fn dispatch(&self, host: &str, pane: Option<u64>, intent: Intent) {
        // Core enqueues effects synchronously and publishes their errors.
        // A receipt is needed only when its result or resource lifetime is needed.
        if let Err(reason) = self.execute(host, pane, intent) {
            self.report(reason);
        }
    }

    fn execute(
        &self,
        host: &str,
        pane: Option<u64>,
        intent: Intent,
    ) -> Result<Arc<agent_core::client::native::Receipt>, AgentError> {
        #[cfg(desktop)]
        if let Intent::ReadThread(read) = &intent
            && read.open
        {
            tracing::info!(target: "agent", operation = "task.open", session = ?read.thread_id, "Task selected");
        }
        if let Some(id) = pane {
            return self.pane(host, id)?.dispatch(intent);
        }
        self.sessions
            .session(host.into())
            .ok_or_else(|| error(tr("接続先が選択されていません")))?
            .dispatch(intent)
    }

    pub fn request(
        &self,
        host: &str,
        pane: Option<u64>,
        intent: Intent,
    ) -> async_channel::Receiver<Result<agent_core::store::Outcome, AgentError>> {
        let receipt = self.execute(host, pane, intent);
        if let Err(reason) = &receipt {
            self.report(reason);
        }
        let (send, receive) = async_channel::bounded(1);
        let closing = self.closing.clone();
        self.spawn(async move {
            let result = match receipt {
                Ok(receipt) => tokio::select! {
                    _ = closing.cancelled() => return,
                    result = receipt.wait() => result,
                },
                Err(reason) => Err(reason),
            };
            let _ = send.send(result).await;
        });
        receive
    }

    pub fn worktree_settings(
        &self,
        host: &str,
    ) -> Option<agent_protocol::models::WorktreeSettings> {
        self.snapshot(host)?
            .files
            .workspace
            .settings
            .as_deref()
            .cloned()
    }

    pub fn reconnect(self: &Arc<Self>, host: String) {
        self.connect_then(host, None);
    }

    /// Reaches `host`, then dispatches `intent` to it: a project picked in a
    /// distro's folder is added once its Host runs.
    pub fn connect_then(self: &Arc<Self>, host: String, intent: Option<Intent>) {
        let owner = self.clone();
        self.spawn(async move {
            let profile = owner
                .hosts
                .lock()
                .await
                .iter()
                .find(|profile| profile.id == host)
                .cloned();
            if let Some(profile) = profile
                && let Some(session) = owner.sessions.session(host.clone())
            {
                owner.resume(session, profile).await;
                if let Some(intent) = intent {
                    owner.dispatch(&host, None, intent);
                }
            }
        });
    }

    /// The view of `thread`'s tab in `host`'s browser, once its Host is ready.
    pub fn browser_session(
        &self,
        host: String,
        thread: agent_protocol::session::SessionRef,
    ) -> async_channel::Receiver<Result<Arc<BrowserSession>, AgentError>> {
        let session = self.sessions.session(host);
        let closing = self.closing.is_cancelled();
        let (send, receive) = async_channel::bounded(1);
        self.spawn(async move {
            let result = async {
                if closing {
                    return Err(error("client is closing"));
                }
                let store = session
                    .ok_or_else(|| error(tr("接続先が選択されていません")))?
                    .ready()
                    .await?;
                Ok(store.browser_session(thread))
            }
            .await;
            let _ = send.send(result).await;
        });
        receive
    }

    /// The browser's next update, which Core reads on this client's runtime;
    /// none once the view is hidden.
    pub fn browser_next(
        &self,
        browser: Arc<BrowserSession>,
    ) -> async_channel::Receiver<Option<BrowserUpdate>> {
        let (send, receive) = async_channel::bounded(1);
        self.spawn(async move {
            let _ = send.send(browser.next().await).await;
        });
        receive
    }

    pub fn pair(self: &Arc<Self>, invitation: agent_protocol::models::Invitation) {
        let owner = self.clone();
        self.spawn(async move {
            let result = owner
                .pair_host(invitation)
                .await
                .map_err(|reason| reason.to_string());
            let _ = owner.updates.send(Update::Paired(result)).await;
        });
    }

    async fn pair_host(
        self: &Arc<Self>,
        invitation: agent_protocol::models::Invitation,
    ) -> Result<String, AgentError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(error)?
            .as_secs();
        agent_core::client::native::validate_invitation(invitation.clone(), now)?;
        let id = agent_core::client::native::ticket_identity(invitation.endpoint.clone())?;
        let host = Host {
            id: id.clone(),
            name: invitation.host_name.clone(),
            ticket: invitation.endpoint.clone(),
            local_directory: None,
        };
        let storage = Arc::new(Storage {
            root: self.root.clone(),
            host: host.clone(),
            handed: self.handed.clone(),
        });
        let session = self
            .sessions
            .pair(storage.clone(), storage, invitation)
            .await?;
        let mut hosts = self.hosts.lock().await;
        if self.closing.is_cancelled() {
            session.shutdown().await?;
            return Err(error("client is closing"));
        }
        let mut next = hosts.clone();
        next.retain(|previous| previous.id != id);
        next.push(host);
        let path = self.root.join("hosts.json");
        let saved = next
            .iter()
            .filter(|host| saved(&host.id))
            .cloned()
            .collect::<Vec<_>>();
        if let Err(reason) = tokio::task::spawn_blocking(move || {
            agent_local_host::save_private_json(&path, &saved).map_err(error)
        })
        .await
        .map_err(error)?
        {
            let _ = session.shutdown().await;
            return Err(reason);
        }
        *hosts = next.clone();
        self.sessions.adopt(id.clone(), session.clone());
        self.observe(id.clone(), session);
        self.updates
            .send(Update::Hosts(next))
            .await
            .map_err(error)?;
        Ok(id)
    }

    /// Forgets a paired Host: it leaves the saved list and its session closes,
    /// then its files and device key on this device are deleted.
    pub fn remove(self: &Arc<Self>, host: String) {
        let owner = self.clone();
        self.spawn(async move {
            if let Err(reason) = owner.remove_host(host).await {
                owner.report(reason);
            }
        });
    }

    async fn remove_host(&self, id: String) -> Result<(), AgentError> {
        let mut hosts = self.hosts.lock().await;
        let Some(profile) = hosts
            .iter()
            .find(|host| host.id == id && saved(&host.id))
            .cloned()
        else {
            return Ok(());
        };
        if self.closing.is_cancelled() {
            return Err(error("client is closing"));
        }
        let next = hosts
            .iter()
            .filter(|host| host.id != id)
            .cloned()
            .collect::<Vec<_>>();
        let path = self.root.join("hosts.json");
        let kept = next
            .iter()
            .filter(|host| saved(&host.id))
            .cloned()
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || agent_local_host::save_private_json(&path, &kept))
            .await
            .map_err(error)?
            .map_err(error)?;
        *hosts = next.clone();
        // Its last publication precedes the list without it, which drops it.
        let closed = match self.sessions.remove(id) {
            Some(session) => session.shutdown().await,
            None => Ok(()),
        };
        self.updates
            .send(Update::Hosts(next))
            .await
            .map_err(error)?;
        drop(hosts);
        // A session that may still save keeps its files.
        closed?;
        let storage = Storage {
            root: self.root.clone(),
            host: profile,
            handed: self.handed.clone(),
        };
        tokio::task::spawn_blocking(move || storage.forget())
            .await
            .map_err(error)?
    }

    /// A server's invitation over the user's SSH, for this device to confirm.
    #[cfg(desktop)]
    pub fn ssh_invitation(
        &self,
        destination: String,
    ) -> async_channel::Receiver<Result<agent_protocol::models::Invitation, String>> {
        let (send, receive) = async_channel::bounded(1);
        self.spawn(async move {
            let _ = send.send(ssh::invitation(&destination).await).await;
        });
        receive
    }

    /// The Host's latest state, which its publication follows.
    pub fn snapshot(&self, host: &str) -> Option<Arc<agent_core::state::Snapshot>> {
        Some(self.sessions.session(host.into())?.snapshot())
    }

    pub fn scene(&self, active: bool) {
        self.sessions.record_scene(if active { 1 } else { 0 });
        if !active {
            let sessions = self.sessions.clone();
            let updates = self.updates.clone();
            self.spawn(async move {
                if let Err(reason) = sessions.flush().await {
                    let _ = updates.send(Update::Error(reason.to_string())).await;
                }
            });
        }
    }

    #[cfg(dictation)]
    pub fn prepare_dictation(
        &self,
        host: &str,
        pane: Option<u64>,
    ) -> Result<Arc<agent_core::client::DictationPreparation>, AgentError> {
        if let Some(id) = pane {
            return Ok(self.pane(host, id)?.prepare_dictation());
        }
        self.sessions
            .session(host.into())
            .and_then(|session| session.store())
            .map(|store| store.prepare_dictation())
            .ok_or_else(|| error(tr("Host 未接続")))
    }

    #[cfg(dictation)]
    pub fn dictate(
        self: &Arc<Self>,
        host: String,
        pane: Option<u64>,
        draft_key: agent_core::state::DraftKey,
        preparation: Arc<agent_core::client::DictationPreparation>,
        audio: Vec<u8>,
        send: bool,
    ) {
        let intent = Intent::Transcribe(agent_core::state::operations::Dictate {
            draft_key,
            preparation: Some(preparation.id()),
            audio,
            send,
            client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
        });
        match self.execute(&host, pane, intent) {
            Ok(receipt) => {
                self.spawn(async move {
                    let _ = receipt.wait().await;
                    drop(preparation);
                });
            }
            Err(reason) => self.report(reason),
        }
    }

    pub async fn shutdown(&self) -> Result<(), AgentError> {
        self.closing.cancel();
        // A borrowed session and its pane stay open for their native owner.
        if self.owns_sessions {
            self.close_panes();
        }
        let hosts = self.hosts.lock().await;
        if self.owns_sessions {
            self.sessions.retire();
        }
        drop(hosts);
        self.jobs.close();
        self.jobs.wait().await;
        if self.owns_sessions {
            self.sessions.shutdown().await
        } else {
            self.sessions.release();
            Ok(())
        }
    }
}

/// Paired Hosts are saved, and can be removed; this PC's and its WSL distros'
/// are found again.
pub(crate) fn saved(host: &str) -> bool {
    host != LOCAL && agent_local_host::wsl::host_distro(host).is_none()
}

/// This machine's Host where it runs or will start, before it is ready.
async fn local_profile(scope: &HostScope) -> Result<Host, AgentError> {
    let local = scope.discover().await.map_err(error)?;
    Ok(Host {
        id: LOCAL.into(),
        name: "This PC".into(),
        ticket: match local.state {
            LocalHostState::Ready(ticket) => ticket.to_string(),
            LocalHostState::Starting | LocalHostState::Stopped => String::new(),
        },
        local_directory: Some(local.directory),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::{client::persistence::ClientStorage, state::Snapshot};
    use std::time::Duration;

    pub(super) fn saved_host(root: &std::path::Path) -> Host {
        let host = Host {
            id: "test-host".into(),
            name: "Saved PC".into(),
            ticket: "offline".into(),
            local_directory: None,
        };
        let storage = Storage {
            root: root.into(),
            host: host.clone(),
            handed: Default::default(),
        };
        let mut snapshot = Snapshot::default();
        snapshot.connection.connection_disabled = true;
        storage
            .save(agent_core::persistence::encode(&snapshot).unwrap())
            .unwrap();
        agent_local_host::save_private_json(&root.join("hosts.json"), &vec![host.clone()]).unwrap();
        host
    }

    pub(super) async fn publication(
        incoming: &async_channel::Receiver<Update>,
        text: &str,
    ) -> Arc<Presentation> {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Update::Presentation { value, .. } = incoming.recv().await.unwrap()
                    && value.composer.draft.text == text
                {
                    return value;
                }
            }
        })
        .await
        .expect("client publication stalled")
    }

    #[test]
    fn embedded_conversations_share_updates_and_leave_the_native_owners_sessions_open() {
        for side_chat in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let host = saved_host(directory.path());
            let (owner_runtime, owner, incoming) =
                Client::new(directory.path().into(), None).unwrap();
            owner_runtime.block_on(publication(&incoming, ""));
            let session = owner.sessions.session(host.id.clone()).unwrap();
            let pane = side_chat.then(|| {
                owner_runtime
                    .block_on(session.store().unwrap().open_pane("/borrowed".into()))
                    .unwrap()
            });
            let (runtime, embedded, incoming) = Client::start(
                directory.path().into(),
                None,
                Some(BorrowedSession {
                    host: host.id.clone(),
                    session: session.clone(),
                    pane: pane.clone(),
                }),
            )
            .unwrap();
            runtime.block_on(async {
                let initial = publication(&incoming, "").await;
                embedded.dispatch(
                    &host.id,
                    side_chat.then_some(1),
                    Intent::SetDraftText {
                        thread_id: initial.composer.controls.navigation.draft_key.clone(),
                        text: "shared native input".into(),
                    },
                );
                publication(&incoming, "shared native input").await;
                tokio::time::timeout(Duration::from_secs(5), embedded.shutdown())
                    .await
                    .unwrap()
                    .unwrap();
                session.clone().ready().await.unwrap();
                let retained = match &pane {
                    Some(pane) => pane.presentation().unwrap(),
                    None => session.presentation(),
                };
                assert_eq!(retained.composer.draft.text, "shared native input");
            });
            let view_directory = tempfile::tempdir().unwrap();
            let selection = view_directory.path().join("connection-selection.json");
            std::fs::write(&selection, b"standalone selection remains untouched").unwrap();
            let (view_runtime, application) = crate::application::Application::embedded(
                view_directory.path().into(),
                BorrowedSession {
                    host: host.id.clone(),
                    session: session.clone(),
                    pane: pane.clone(),
                },
            )
            .unwrap();
            view_runtime.block_on(application.shutdown()).unwrap();
            assert_eq!(
                std::fs::read(selection).unwrap(),
                b"standalone selection remains untouched"
            );
            if let Some(pane) = pane {
                pane.retire();
            }
            owner_runtime.block_on(owner.shutdown()).unwrap();
        }
    }

    #[test]
    fn closing_an_embedded_view_does_not_wait_for_native_startup_or_pending_receipts() {
        struct BlockedPlatform {
            entered: std::sync::mpsc::SyncSender<()>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl agent_core::client::session::ClientPlatform for BlockedPlatform {
            fn identity(&self) -> Result<Vec<u8>, AgentError> {
                Ok(vec![1; 32])
            }
            fn diagnostics_directory(&self) -> Result<Option<String>, AgentError> {
                self.entered.send(()).unwrap();
                self.release.lock().unwrap().recv().unwrap();
                Ok(None)
            }
            fn conversation_directory(&self) -> Result<Option<String>, AgentError> {
                Ok(None)
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let host = saved_host(directory.path());
        let storage = Arc::new(Storage {
            root: directory.path().into(),
            host: host.clone(),
            handed: Default::default(),
        });
        let sessions = ClientSessions::new(
            false,
            ClientBuild {
                number: 1,
                revision: "test".into(),
            },
            "native".into(),
            DevicePreferences::new(Arc::new(crate::storage::DeviceStorage {
                root: directory.path().into(),
            })),
        );
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, blocked) = std::sync::mpsc::channel();
        let session = sessions.open(
            host.id.clone(),
            storage,
            Arc::new(BlockedPlatform {
                entered,
                release: std::sync::Mutex::new(blocked),
            }),
        );
        let owner_runtime = tokio::runtime::Runtime::new().unwrap();
        let startup = owner_runtime.spawn(session.clone().ready());
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        let (runtime, embedded, _) = Client::start(
            directory.path().into(),
            None,
            Some(BorrowedSession {
                host: host.id.clone(),
                session: session.clone(),
                pane: None,
            }),
        )
        .unwrap();
        let pending = embedded.request(
            &host.id,
            None,
            Intent::SetDraftText {
                thread_id: session
                    .presentation()
                    .composer
                    .controls
                    .navigation
                    .draft_key
                    .clone(),
                text: "native queued input".into(),
            },
        );
        let download = embedded.download(host.id.clone(), "/pending-image.png".into());
        let closed = runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), embedded.shutdown()).await
        });
        // Unblock the owner's worker even if the bounded shutdown assertion fails.
        release.send(()).unwrap();
        closed.unwrap().unwrap();
        assert!(runtime.block_on(pending.recv()).is_err());
        assert!(runtime.block_on(download.recv()).is_err());
        let store = owner_runtime.block_on(startup).unwrap().unwrap();
        assert_eq!(
            store.presentation().composer.draft.text,
            "native queued input"
        );
        owner_runtime.block_on(sessions.shutdown()).unwrap();
    }

    #[test]
    fn local_host_facts_are_explicit_and_saved_profiles_cannot_replace_them() {
        let directory = tempfile::tempdir().unwrap();
        let remote = saved_host(directory.path());
        let scope = HostScope {
            directory: directory.path().join("host"),
            isolated: true,
        };
        let local = Host {
            id: LOCAL.into(),
            name: "This PC".into(),
            ticket: String::new(),
            local_directory: Some(scope.directory.clone()),
        };
        let mut snapshot = Snapshot::default();
        snapshot.connection.connection_disabled = true;
        Storage {
            root: directory.path().into(),
            host: local.clone(),
            handed: Default::default(),
        }
        .save(agent_core::persistence::encode(&snapshot).unwrap())
        .unwrap();
        let mut stale = local.clone();
        stale.name = "Old derived local".into();
        agent_local_host::save_private_json(
            &directory.path().join("hosts.json"),
            &vec![stale, remote.clone()],
        )
        .unwrap();
        let (runtime, client, incoming) =
            Client::new(directory.path().into(), Some(scope)).unwrap();
        runtime.block_on(async {
            let Update::Hosts(hosts) = incoming.recv().await.unwrap() else {
                panic!("profiles must be published first");
            };
            assert_eq!(hosts.len(), 2);
            assert!(hosts[0] == local);
            assert!(hosts[1] == remote);
            client.shutdown().await.unwrap();
        });
        drop(client);
        drop(runtime);
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        runtime.block_on(async {
            let Update::Hosts(hosts) = incoming.recv().await.unwrap() else {
                panic!("profiles must be published first");
            };
            assert_eq!(hosts.len(), 1);
            assert!(hosts[0] == remote);
            client.shutdown().await.unwrap();
        });
    }

    #[test]
    fn removing_a_host_before_its_store_starts_does_not_report_startup_cancellation() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        runtime.block_on(async {
            assert!(
                matches!(incoming.recv().await.unwrap(), Update::Hosts(hosts) if hosts.is_empty())
            );
        });
        client.jobs.close();
        runtime.block_on(client.jobs.wait());
        client.jobs.reopen();

        // Park every worker so restore can register a Host while its startup
        // remains queued. Dropping a release sender also unblocks it on panic.
        let workers = runtime.metrics().num_workers();
        let (entered, parked) = std::sync::mpsc::channel();
        let mut releases = Vec::new();
        for _ in 0..workers {
            let (release, wait) = std::sync::mpsc::channel();
            releases.push(release);
            let entered = entered.clone();
            drop(runtime.spawn(async move {
                let _ = entered.send(());
                let _ = wait.recv();
            }));
        }
        for _ in 0..workers {
            parked.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        let remote = saved_host(directory.path());
        runtime.block_on(async {
            client.restore().await.unwrap();
            assert!(client.snapshot(&remote.id).is_some());
            client.remove_host(remote.id.clone()).await.unwrap();
        });
        for release in releases {
            let _ = release.send(());
        }
        client.jobs.close();
        runtime.block_on(async {
            client.jobs.wait().await;
            let mut hosts = None;
            while let Ok(update) = incoming.try_recv() {
                match update {
                    Update::Hosts(next) => hosts = Some(next),
                    Update::Error(reason) => panic!("retired startup reported an error: {reason}"),
                    Update::Presentation { .. } => panic!("retired startup published its UI"),
                    _ => {}
                }
            }
            assert!(hosts.unwrap().is_empty());
            assert!(client.snapshot(&remote.id).is_none());
            client.shutdown().await.unwrap();
        });
    }

    #[test]
    fn a_removed_host_leaves_the_saved_list_and_its_files_while_this_pc_stays() {
        let directory = tempfile::tempdir().unwrap();
        let remote = saved_host(directory.path());
        let hosts_files = directory.path().join("hosts");
        let files = || std::fs::read_dir(&hosts_files).unwrap().count();
        assert_eq!(files(), 1);
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        runtime.block_on(async {
            let Update::Hosts(hosts) = incoming.recv().await.unwrap() else {
                panic!("profiles must be published first");
            };
            assert!(hosts == [remote.clone()]);
            // This PC's Host is found again on each launch: nothing removes it.
            client.remove(LOCAL.into());
            client.remove(remote.id.clone());
            let remaining = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match incoming.recv().await.unwrap() {
                        Update::Hosts(hosts) => return hosts,
                        Update::Error(reason) => panic!("removal failed: {reason}"),
                        _ => {}
                    }
                }
            })
            .await
            .expect("the removal was not published");
            assert!(remaining.is_empty());
            assert!(client.snapshot(&remote.id).is_none());
            tokio::time::timeout(Duration::from_secs(10), async {
                while files() > 0 {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("the removed Host's files remain");
            client.shutdown().await.unwrap();
        });
        drop(client);
        drop(runtime);
        // The next launch restores no removed Host.
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        runtime.block_on(async {
            let Update::Hosts(hosts) = incoming.recv().await.unwrap() else {
                panic!("profiles must be published first");
            };
            assert!(hosts.is_empty());
            assert!(client.snapshot(&remote.id).is_none());
            client.shutdown().await.unwrap();
        });
    }

    #[test]
    fn an_unavailable_request_reports_once_and_a_valid_host_can_recover() {
        let directory = tempfile::tempdir().unwrap();
        let host = saved_host(directory.path());
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        let initial = runtime.block_on(publication(&incoming, ""));
        let draft = initial.composer.controls.navigation.draft_key.clone();
        let edit = |text: &str| Intent::SetDraftText {
            thread_id: draft.clone(),
            text: text.into(),
        };
        let failed = client.request("missing-host", None, edit("must not be applied"));
        runtime.block_on(async {
            let reason = failed.recv().await.unwrap().unwrap_err().to_string();
            client
                .request(&host.id, None, edit("recovered 日本語 👋"))
                .recv()
                .await
                .unwrap()
                .unwrap();
            let errors = tokio::time::timeout(Duration::from_secs(10), async {
                let mut errors = Vec::new();
                loop {
                    match incoming.recv().await.unwrap() {
                        Update::Error(reason) => errors.push(reason),
                        Update::Presentation { value, .. }
                            if value.composer.draft.text == "recovered 日本語 👋" =>
                        {
                            break errors;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(errors, [reason]);
            client.shutdown().await.unwrap();
        });
    }

    #[test]
    fn failed_attachment_batches_finish_once_and_release_only_their_own_staging_files() {
        let directory = tempfile::tempdir().unwrap();
        saved_host(directory.path());
        let selected = directory.path().join("selected.txt");
        std::fs::write(&selected, "保持する日本語 👋").unwrap();
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        let initial = runtime.block_on(publication(&incoming, ""));
        let id = uuid::Uuid::new_v4();
        client.attach_batch(
            id,
            "missing-host".into(),
            None,
            initial.composer.controls.navigation.draft_key.clone(),
            vec![
                AttachmentSource::Image {
                    name: "first.png".into(),
                    bytes: b"first fixture".to_vec(),
                },
                AttachmentSource::File {
                    path: selected.clone(),
                    is_image: false,
                },
                AttachmentSource::Image {
                    name: "last.png".into(),
                    bytes: b"last fixture".to_vec(),
                },
            ],
        );
        runtime.block_on(async {
            let errors = tokio::time::timeout(Duration::from_secs(10), async {
                let mut errors = Vec::new();
                loop {
                    match incoming.recv().await.unwrap() {
                        Update::AttachmentsFinished(finished) => {
                            assert_eq!(finished, id);
                            break errors;
                        }
                        Update::Error(reason) => errors.push(reason),
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(
                errors.len(),
                1,
                "a failed selection stops the batch and retains the error for retry"
            );
            assert!(errors.iter().all(|reason| reason.contains("接続先")));
            assert_eq!(
                std::fs::read_dir(directory.path().join("uploads"))
                    .unwrap()
                    .count(),
                0
            );
            assert_eq!(
                std::fs::read_to_string(&selected).unwrap(),
                "保持する日本語 👋"
            );
            let retry = uuid::Uuid::new_v4();
            client.attach_batch(
                retry,
                "missing-host".into(),
                None,
                initial.composer.controls.navigation.draft_key.clone(),
                vec![AttachmentSource::File {
                    path: selected.clone(),
                    is_image: false,
                }],
            );
            tokio::time::timeout(Duration::from_secs(10), async {
                let mut errors = 0;
                loop {
                    match incoming.recv().await.unwrap() {
                        Update::Error(_) => errors += 1,
                        Update::AttachmentsFinished(finished) => {
                            assert_eq!(finished, retry);
                            assert_eq!(errors, 1);
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(selected).unwrap(),
                "保持する日本語 👋"
            );
            client.shutdown().await.unwrap();
            while let Ok(update) = incoming.try_recv() {
                assert!(!matches!(update, Update::AttachmentsFinished(_)));
            }
        });
    }

    #[test]
    fn failed_core_upload_retains_its_notice_without_staging_later_selections() {
        let directory = tempfile::tempdir().unwrap();
        let host = saved_host(directory.path());
        let selected = directory.path().join("selected.txt");
        std::fs::write(&selected, "keep").unwrap();
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        let initial = runtime.block_on(publication(&incoming, ""));
        let draft_key = initial.composer.controls.navigation.draft_key.clone();
        let id = uuid::Uuid::new_v4();
        client.attach_batch(
            id,
            host.id.clone(),
            None,
            draft_key.clone(),
            vec![
                AttachmentSource::File {
                    path: selected.clone(),
                    is_image: false,
                },
                AttachmentSource::Image {
                    name: "not-started.png".into(),
                    bytes: b"fixture".to_vec(),
                },
            ],
        );
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match incoming.recv().await.unwrap() {
                        Update::Error(reason) => panic!("duplicate core error: {reason}"),
                        Update::AttachmentsFinished(finished) => {
                            assert_eq!(finished, id);
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            let snapshot = client.sessions.session(host.id).unwrap().snapshot();
            assert!(
                snapshot
                    .operation_error(agent_core::state::operations::OperationKey::Attachment {
                        draft_key
                    })
                    .is_some()
            );
            assert!(
                !directory.path().join("uploads").exists(),
                "later selections must not replace the failed upload's notice"
            );
            assert_eq!(std::fs::read_to_string(selected).unwrap(), "keep");
            client.shutdown().await.unwrap();
        });
    }

    #[test]
    fn shutdown_flushes_accepted_input_and_a_new_client_restores_it() {
        let directory = tempfile::tempdir().unwrap();
        let host = saved_host(directory.path());
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        let initial = runtime.block_on(publication(&incoming, ""));
        let key = initial.pane().navigation.draft_key.clone();
        client.dispatch(
            &host.id,
            None,
            Intent::SetDraftText {
                thread_id: key.clone(),
                text: "終了直前の日本語 👋".into(),
            },
        );
        runtime.block_on(async {
            publication(&incoming, "終了直前の日本語 👋").await;
            tokio::time::timeout(Duration::from_secs(10), client.shutdown())
                .await
                .unwrap()
                .unwrap();
        });
        drop(client);
        drop(runtime);
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        let restored = runtime.block_on(publication(&incoming, "終了直前の日本語 👋"));
        assert_eq!(restored.pane().navigation.draft_key, key);
        runtime.block_on(client.shutdown()).unwrap();
    }

    #[test]
    fn failed_pairing_preserves_existing_connections_and_drafts() {
        let directory = tempfile::tempdir().unwrap();
        let host = saved_host(directory.path());
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        let initial = runtime.block_on(publication(&incoming, ""));
        client.dispatch(
            &host.id,
            None,
            Intent::SetDraftText {
                thread_id: initial.pane().navigation.draft_key.clone(),
                text: "保持する入力".into(),
            },
        );
        runtime.block_on(publication(&incoming, "保持する入力"));
        client.pair(agent_protocol::models::Invitation {
            endpoint: "invalid".into(),
            invitation: uuid::Uuid::new_v4(),
            expires_at: 0,
            host_name: "Expired PC".into(),
            ai_recipients: Vec::new(),
            transcription_recipient: None,
        });
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if let Update::Paired(Err(reason)) = incoming.recv().await.unwrap() {
                        assert!(!reason.is_empty());
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(client.hosts.lock().await.len(), 1);
            assert_eq!(
                client
                    .sessions
                    .session(host.id)
                    .unwrap()
                    .presentation()
                    .composer
                    .draft
                    .text,
                "保持する入力"
            );
            client.shutdown().await.unwrap();
        });
    }

    #[test]
    fn closing_during_restore_cannot_open_a_late_session() {
        let directory = tempfile::tempdir().unwrap();
        saved_host(directory.path());
        let (runtime, client, _) = Client::new(directory.path().into(), None).unwrap();
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), client.shutdown())
                .await
                .unwrap()
                .unwrap();
            if let Some(session) = client.sessions.session("test-host".into()) {
                assert!(session.clone().ready().await.is_err());
            }
        });
    }

    #[test]
    fn a_failed_operation_has_one_dismissible_core_notice() {
        use agent_core::{presentation::error::ErrorScope, state::operations as op};
        let directory = tempfile::tempdir().unwrap();
        let host = saved_host(directory.path());
        let (runtime, client, incoming) = Client::new(directory.path().into(), None).unwrap();
        runtime.block_on(publication(&incoming, ""));
        client.dispatch(
            &host.id,
            None,
            Intent::ReadFile(op::ReadFile {
                path: "/missing.txt".into(),
                discard_draft: false,
            }),
        );
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match incoming.recv().await.unwrap() {
                        Update::Error(reason) => panic!("duplicate client error: {reason}"),
                        Update::Presentation { value, .. }
                            if value
                                .snapshot
                                .operation_error(op::OperationKey::File {
                                    pane: value.pane_id.clone(),
                                })
                                .is_some() =>
                        {
                            let notices = value
                                .snapshot
                                .error_notices(&value.pane_id, ErrorScope::Workspace);
                            assert_eq!(notices.len(), 1);
                            client.dispatch(
                                &host.id,
                                None,
                                Intent::DismissError {
                                    id: notices[0].id.clone(),
                                },
                            );
                            break;
                        }
                        _ => {}
                    }
                }
                loop {
                    match incoming.recv().await.unwrap() {
                        Update::Error(reason) => panic!("undismissed client error: {reason}"),
                        Update::Presentation { value, .. }
                            if value
                                .snapshot
                                .error_notices(&value.pane_id, ErrorScope::Workspace)
                                .is_empty() =>
                        {
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
            client.shutdown().await.unwrap();
            while let Ok(update) = incoming.try_recv() {
                if let Update::Error(reason) = update {
                    panic!("late client error: {reason}");
                }
            }
        });
    }

    #[test]
    fn a_preview_retains_its_file_until_its_last_owner_closes() {
        let directory = tempfile::tempdir().unwrap();
        saved_host(directory.path());
        let (runtime, client, _) = Client::new(directory.path().into(), None).unwrap();
        let file = runtime
            .block_on(client.cache_image(vec![1, 2, 3], "png".into()).recv())
            .unwrap()
            .unwrap();
        let path = file.path.clone();
        let preview = file.clone();
        drop(file);
        assert_eq!(std::fs::read(&path).unwrap(), [1, 2, 3]);
        let unrelated = path.parent().unwrap().parent().unwrap().join("keep.txt");
        std::fs::write(&unrelated, "keep").unwrap();
        drop(preview);
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(unrelated).unwrap(), "keep");
        runtime.block_on(client.shutdown()).unwrap();
    }

    #[test]
    fn closing_an_image_view_before_transfer_finishes_leaves_no_file() {
        let directory = tempfile::tempdir().unwrap();
        saved_host(directory.path());
        let (runtime, client, _) = Client::new(directory.path().into(), None).unwrap();
        drop(client.cache_image(vec![1, 2, 3], "png".into()));
        runtime.block_on(client.shutdown()).unwrap();
        assert_eq!(
            std::fs::read_dir(directory.path().join("downloads"))
                .unwrap()
                .count(),
            0
        );
    }
}

//! Host lifetimes belong to the client, independently of the displayed Host.
use super::*;
use crate::client::persistence::DevicePreferences;
use std::collections::BTreeMap;

struct Hosts {
    sessions: BTreeMap<String, Arc<ClientSession>>,
    active: bool,
}

#[derive(uniffi::Object)]
pub struct ClientSessions {
    hosts: Mutex<Hosts>,
    device: Arc<DevicePreferences>,
    recent_chats: bool,
    client_build: ClientBuild,
    device_name: String,
}

#[uniffi::export(async_runtime = "tokio")]
impl ClientSessions {
    #[uniffi::constructor]
    pub fn new(
        recent_chats: bool,
        client_build: ClientBuild,
        device_name: String,
        device: Arc<DevicePreferences>,
    ) -> Arc<Self> {
        Arc::new(Self {
            hosts: Mutex::new(Hosts {
                sessions: BTreeMap::new(),
                active: true,
            }),
            device,
            recent_chats,
            client_build,
            device_name,
        })
    }

    /// Opening another Host neither retires nor waits for an existing session.
    pub fn open(
        &self,
        host: String,
        storage: Arc<dyn ClientStorage>,
        platform: Arc<dyn ClientPlatform>,
    ) -> Arc<ClientSession> {
        let mut hosts = self.hosts.lock().unwrap();
        let active = hosts.active;
        hosts
            .sessions
            .entry(host)
            .or_insert_with(|| {
                let session = self.create(storage, platform);
                session.set_active(active);
                session
            })
            .clone()
    }

    pub fn session(&self, host: String) -> Option<Arc<ClientSession>> {
        self.hosts.lock().unwrap().sessions.get(&host).cloned()
    }

    pub fn thread_list(
        &self,
        host: Option<String>,
    ) -> Option<crate::presentation::list::ThreadList> {
        let sessions: Vec<_> = self
            .hosts
            .lock()
            .unwrap()
            .sessions
            .iter()
            .filter(|(id, _)| host.as_ref().is_none_or(|host| host == *id))
            .map(|(id, session)| (id.clone(), session.clone()))
            .collect();
        crate::presentation::connection_list::combine_connection_lists(
            sessions.into_iter().filter_map(|(id, session)| {
                let list = session.presentation().thread_list();
                list.map(|list| (id, list))
            }),
        )
    }

    /// Remains provisional until the platform saves the profile and adopts it.
    pub async fn pair(
        &self,
        storage: Arc<dyn ClientStorage>,
        platform: Arc<dyn ClientPlatform>,
        invitation: Invitation,
    ) -> Result<Arc<ClientSession>, AgentError> {
        // Include pending preference edits without making another Host's disk
        // failure prevent pairing. Each session retains its own save error.
        let _ = self.flush().await;
        let session = self.create(storage, platform);
        let store = session.restore(true).await?;
        store
            .reconnect(
                session
                    .connection(invitation.endpoint, Some(invitation.invitation.to_string()))
                    .await?,
            )
            .await?;
        session.install(store, false)?;
        session.dispatch(Intent::LoadHostName(
            crate::state::operations::LoadHostName {},
        ))?;
        Ok(session)
    }

    pub fn adopt(&self, host: String, session: Arc<ClientSession>) {
        let mut hosts = self.hosts.lock().unwrap();
        if let Some(previous) = hosts.sessions.get(&host) {
            previous.retire();
            *session.replacing.lock().unwrap() = previous.closing.lock().unwrap().clone();
        }
        session.set_active(hosts.active);
        hosts.sessions.insert(host, session);
    }

    /// Detach synchronously so pending native work cannot reopen a removed Host.
    /// Await the returned session's shutdown before deleting platform files.
    pub fn remove(&self, host: String) -> Option<Arc<ClientSession>> {
        let session = self.hosts.lock().unwrap().sessions.remove(&host)?;
        session.retire();
        Some(session)
    }

    pub fn record_scene(&self, value: u64) {
        let mut hosts = self.hosts.lock().unwrap();
        hosts.active = value == 1;
        for session in hosts.sessions.values() {
            session.set_active(hosts.active);
            if let Some(store) = session.store() {
                store.record_connection_event(ConnectionPhase::AppScene, value);
            }
        }
    }

    pub async fn network_changed(&self) {
        futures_util::future::join_all(
            self.all()
                .into_iter()
                .filter_map(|session| session.store())
                .map(|store| async move { store.network_changed().await }),
        )
        .await;
    }

    pub async fn flush(&self) -> Result<(), AgentError> {
        let results =
            futures_util::future::join_all(self.all().into_iter().map(|session| session.flush()))
                .await;
        results.into_iter().collect()
    }

    pub fn retire(&self) {
        for session in self.all() {
            session.retire();
        }
    }

    pub async fn shutdown(&self) -> Result<(), AgentError> {
        self.retire();
        let results = futures_util::future::join_all(
            self.all().into_iter().map(|session| session.shutdown()),
        )
        .await;
        results.into_iter().collect()
    }
}

impl ClientSessions {
    fn all(&self) -> Vec<Arc<ClientSession>> {
        self.hosts
            .lock()
            .unwrap()
            .sessions
            .values()
            .cloned()
            .collect()
    }

    fn create(
        &self,
        storage: Arc<dyn ClientStorage>,
        platform: Arc<dyn ClientPlatform>,
    ) -> Arc<ClientSession> {
        ClientSession::create(
            Persistence::new(storage, self.device.clone()),
            platform,
            self.recent_chats,
            self.client_build.clone(),
            self.device_name.clone(),
        )
    }
}

impl ClientSessions {
    /// Shows a session another owner keeps: its activity and lifetime stay theirs.
    pub fn borrow(&self, host: String, session: Arc<ClientSession>) {
        self.hosts.lock().unwrap().sessions.insert(host, session);
    }

    /// Forgets borrowed sessions without closing them, before this manager ends.
    pub fn release(&self) {
        self.hosts.lock().unwrap().sessions.clear();
    }
}

impl Drop for ClientSessions {
    fn drop(&mut self) {
        self.retire();
    }
}

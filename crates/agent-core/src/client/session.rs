//! Each mobile Host session owns restoration, queued intents and ordered saves.
//! Platforms supply private storage; no UI task decides when state is durable.
mod hosts;
pub use hosts::ClientSessions;

use super::native::{AgentStore, Connection, Receipt};
use super::persistence::ClientStorage;
use super::persistence::{Completion, Persistence};
use super::{AgentError, error};
use crate::{
    diagnostics::ConnectionPhase,
    state::{Intent, Snapshot},
    store::Outcome,
};
use agent_protocol::models::Invitation;
use futures_util::FutureExt;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// OS-owned connection identity and private directories, independent of state payloads.
#[uniffi::export(rust, foreign)]
pub trait ClientPlatform: Send + Sync {
    fn identity(&self) -> Result<Vec<u8>, AgentError>;
    fn diagnostics_directory(&self) -> Result<Option<String>, AgentError>;
    fn conversation_directory(&self) -> Result<Option<String>, AgentError>;
}

type Pending = (Intent, oneshot::Sender<Result<Outcome, AgentError>>);

/// Native apps supply build metadata without recompiling the shared library.
#[derive(Clone, uniffi::Record)]
pub struct ClientBuild {
    pub number: u64,
    pub revision: String,
}

enum Phase {
    Loading(Vec<Pending>),
    Ready {
        store: Arc<AgentStore>,
        runtime: tokio::runtime::Handle,
        observing: bool,
    },
    Failed(AgentError),
}

#[derive(uniffi::Object)]
pub struct ClientSession {
    persistence: Arc<Persistence>,
    platform: Arc<dyn ClientPlatform>,
    active: AtomicBool,
    replacing: Mutex<Option<Completion<()>>>,
    closing: Mutex<Option<Completion<()>>>,
    recent_chats: bool,
    client_build: ClientBuild,
    device_name: String,
    phase: Mutex<Phase>,
    initialization: tokio::sync::Mutex<()>,
    operations: Mutex<Vec<Completion<Outcome>>>,
    stop: CancellationToken,
    empty: Arc<Snapshot>,
}

#[uniffi::export(async_runtime = "tokio")]
impl ClientSession {
    pub fn store(&self) -> Option<Arc<AgentStore>> {
        match &*self.phase.lock().unwrap() {
            Phase::Ready { store, .. } => Some(store.clone()),
            _ => None,
        }
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.store()
            .map_or_else(|| self.pending_snapshot(), |store| store.snapshot())
    }

    pub fn presentation(&self) -> Arc<crate::presentation::view::Presentation> {
        self.store().map_or_else(
            || {
                crate::presentation::view::Presentation::pending(self.pending_snapshot(), "")
                    .expect("primary pane always exists")
            },
            |store| store.presentation(),
        )
    }

    pub fn persistence_error(&self) -> Option<String> {
        self.persistence.failure()
    }

    pub async fn ready(self: Arc<Self>) -> Result<Arc<AgentStore>, AgentError> {
        let _initializing = self.initialization.lock().await;
        if self.stop.is_cancelled() {
            return Err(error("session closed"));
        }
        // Re-pairing the same Host shares its disk location. Its old final
        // write must finish before this session starts autosaving there.
        let replacing = self.replacing.lock().unwrap().clone();
        if let Some(replacing) = replacing {
            if let Err(reason) = replacing.await {
                self.persistence.report_failure(reason.to_string());
            }
            self.replacing.lock().unwrap().take();
        }
        let status = match &mut *self.phase.lock().unwrap() {
            Phase::Loading(_) => None,
            Phase::Ready {
                store, observing, ..
            } => {
                let start = !*observing;
                *observing = true;
                Some(Ok((store.clone(), start)))
            }
            Phase::Failed(reason) => Some(Err(reason.clone())),
        };
        let (store, observe) = match status {
            Some(result) => result?,
            None => {
                let result = tokio::select! {
                    biased;
                    _ = self.stop.cancelled() => Err(error("session closed")),
                    result = self.restore(false) => result,
                };
                match result {
                    Ok(store) => {
                        if let Err(reason) = self.install(store.clone(), true) {
                            let _ = store.shutdown().await;
                            return Err(reason);
                        }
                        (store, true)
                    }
                    Err(reason) => {
                        self.fail(reason.clone());
                        return Err(reason);
                    }
                }
            }
        };
        if observe {
            tokio::spawn(
                self.persistence
                    .clone()
                    .observe(store.store.subscribe(), self.stop.clone()),
            );
        }
        Ok(store)
    }

    /// The queue and ready transition share one lock, preserving input order.
    pub fn dispatch(&self, intent: Intent) -> Result<Arc<Receipt>, AgentError> {
        let (complete, result) = oneshot::channel();
        let completion = async move { result.await.map_err(error)? }.boxed().shared();
        let ready = {
            let mut phase = self.phase.lock().unwrap();
            let ready = match &mut *phase {
                Phase::Loading(pending) => {
                    pending.push((intent, complete));
                    None
                }
                Phase::Ready { store, runtime, .. } => {
                    Some((store.clone(), runtime.clone(), intent, complete))
                }
                Phase::Failed(reason) => return Err(reason.clone()),
            };
            let mut operations = self.operations.lock().unwrap();
            operations.retain(|operation| operation.peek().is_none());
            operations.push(completion.clone());
            ready
        };
        // Publication may synchronously resume native observers that read this
        // session or flush accepted work. Track it first, then release the locks.
        if let Some((store, runtime, intent, complete)) = ready {
            Self::dispatch_to(&store, &runtime, intent, complete);
        }
        Ok(Arc::new(Receipt {
            result: Mutex::new(Some(Box::pin(completion))),
        }))
    }

    pub async fn resume(self: Arc<Self>, ticket: String) -> Result<bool, AgentError> {
        let store = self.clone().ready().await?;
        if !store.snapshot().connection_enabled() {
            return Ok(false);
        }
        let reused = tokio::select! {
            biased;
            _ = self.stop.cancelled() => return Err(error("session closed")),
            result = async { store.resume(self.connection(ticket, None).await?).await } => result?,
        };
        if store.snapshot().connection.host_name.is_none() {
            self.dispatch(Intent::LoadHostName(
                crate::state::operations::LoadHostName {},
            ))?;
        }
        Ok(reused)
    }

    /// Wait for intents accepted before this flush, then capture and enqueue
    /// under the session lock to preserve order with autosaves and retirement.
    pub async fn flush(self: Arc<Self>) -> Result<(), AgentError> {
        self.clone().ready().await?;
        let operations = self.operations.lock().unwrap().clone();
        for operation in operations {
            let _ = operation.await;
        }
        self.operations
            .lock()
            .unwrap()
            .retain(|operation| operation.peek().is_none());
        self.save()?.await
    }

    /// Start closing synchronously. Native cancellation cannot discard the final save.
    pub fn retire(&self) {
        let mut closing = self.closing.lock().unwrap();
        if closing.is_some() {
            return;
        }
        self.stop.cancel();
        if let Phase::Ready {
            store,
            runtime,
            observing,
        } = self.fail(error("session closed"))
        {
            let persistence = self.persistence.clone();
            let (complete, result) = oneshot::channel();
            runtime.spawn(async move {
                if observing {
                    persistence.flush_copies().await;
                }
                let closed = store.shutdown().await;
                let saved = if observing {
                    match persistence.finish(store.snapshot()) {
                        Ok(saved) => saved.await,
                        Err(reason) => Err(reason),
                    }
                } else {
                    Ok(())
                };
                let _ = complete.send(closed.and(saved));
            });
            *closing = Some(async move { result.await.map_err(error)? }.boxed().shared());
        } else {
            *closing = Some(async { Ok(()) }.boxed().shared());
        }
    }

    pub async fn shutdown(self: Arc<Self>) -> Result<(), AgentError> {
        self.retire();
        let closing = self.closing.lock().unwrap().as_ref().unwrap().clone();
        closing.await
    }
}

impl ClientSession {
    fn pending_snapshot(&self) -> Arc<Snapshot> {
        let language = self.persistence.device.language();
        if language == self.empty.catalog.ui_language {
            return self.empty.clone();
        }
        let mut snapshot = self.empty.as_ref().clone();
        snapshot.catalog.ui_language = language;
        Arc::new(snapshot)
    }
    fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::SeqCst);
        if let Some(store) = self.store() {
            store.set_connection_active(active);
        }
    }

    fn create(
        persistence: Arc<Persistence>,
        platform: Arc<dyn ClientPlatform>,
        recent_chats: bool,
        client_build: ClientBuild,
        device_name: String,
    ) -> Arc<Self> {
        let mut empty = Snapshot::default();
        empty.catalog.ui_language = persistence.device.language();
        Arc::new(Self {
            persistence,
            platform,
            active: AtomicBool::new(true),
            replacing: Default::default(),
            closing: Default::default(),
            recent_chats,
            client_build,
            device_name,
            phase: Mutex::new(Phase::Loading(Vec::new())),
            initialization: Default::default(),
            operations: Default::default(),
            stop: CancellationToken::new(),
            empty: Arc::new(empty),
        })
    }
    async fn restore(&self, new_pairing: bool) -> Result<Arc<AgentStore>, AgentError> {
        let started = std::time::Instant::now();
        let platform = self.platform.clone();
        let (directory, conversations) = tokio::task::spawn_blocking(move || {
            Ok::<_, AgentError>((
                platform.diagnostics_directory()?,
                platform
                    .conversation_directory()?
                    .map(std::path::PathBuf::from),
            ))
        })
        .await
        .map_err(error)??;
        let snapshot = self
            .persistence
            .restore(new_pairing, conversations)
            .await
            .map_err(error)?;
        let read = started.elapsed().as_micros() as u64;
        let store = AgentStore::restored(
            snapshot,
            directory,
            self.client_build.revision.clone(),
            Some(self.persistence.device.clone()),
        )?;
        store.record_connection_event(ConnectionPhase::SnapshotRead, read);
        store.record_connection_event(
            ConnectionPhase::AppPreparation,
            started.elapsed().as_micros() as u64,
        );
        store.record_connection_event(ConnectionPhase::ClientBuild, self.client_build.number);
        Ok(store)
    }

    async fn connection(
        &self,
        ticket: String,
        invitation: Option<String>,
    ) -> Result<Connection, AgentError> {
        let started = std::time::Instant::now();
        let platform = self.platform.clone();
        let identity = tokio::task::spawn_blocking(move || platform.identity())
            .await
            .map_err(error)??;
        if let Some(store) = self.store() {
            store.record_connection_event(
                ConnectionPhase::IdentityRead,
                started.elapsed().as_micros() as u64,
            );
        }
        Ok(Connection {
            ticket,
            identity,
            invitation,
            use_relays: true,
            device_name: self.device_name.clone(),
        })
    }

    fn install(&self, store: Arc<AgentStore>, observing: bool) -> Result<(), AgentError> {
        let runtime = tokio::runtime::Handle::current();
        let mut phase = self.phase.lock().unwrap();
        let Phase::Loading(pending) = &mut *phase else {
            return Err(error("session closed"));
        };
        if let Some(task) = self.persistence.start(store.store.subscribe()) {
            runtime.spawn(task);
        }
        store.set_connection_active(self.active.load(Ordering::SeqCst));
        // Navigation comes before queued UI intents, just as on both platforms.
        let _ = store.dispatch(Intent::ShowThreadList);
        if self.recent_chats {
            let _ = store.dispatch(Intent::ShowRecentChats);
        }
        for (intent, complete) in pending.drain(..) {
            Self::dispatch_to(&store, &runtime, intent, complete);
        }
        *phase = Phase::Ready {
            store,
            runtime,
            observing,
        };
        Ok(())
    }

    fn dispatch_to(
        store: &AgentStore,
        runtime: &tokio::runtime::Handle,
        intent: Intent,
        complete: oneshot::Sender<Result<Outcome, AgentError>>,
    ) {
        let receipt = store.dispatch(intent);
        runtime.spawn(async move {
            let result = match receipt {
                Ok(receipt) => receipt.wait().await,
                Err(reason) => Err(reason),
            };
            let _ = complete.send(result);
        });
    }

    fn fail(&self, reason: AgentError) -> Phase {
        let previous = std::mem::replace(
            &mut *self.phase.lock().unwrap(),
            Phase::Failed(reason.clone()),
        );
        match previous {
            Phase::Loading(pending) => {
                for (_, complete) in pending {
                    let _ = complete.send(Err(reason.clone()));
                }
                Phase::Failed(reason)
            }
            other => other,
        }
    }

    fn save(&self) -> Result<Completion<()>, AgentError> {
        let phase = self.phase.lock().unwrap();
        match &*phase {
            Phase::Ready { store, .. } => self.persistence.flush(store.snapshot()),
            _ => Err(error("session closed")),
        }
    }
}

impl Drop for ClientSession {
    fn drop(&mut self) {
        self.retire();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::persistence::{DevicePreferences, PreferenceKey, PreferenceStorage};
    use std::sync::{
        Condvar,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::sync::Notify;

    #[tokio::test]
    async fn a_borrowing_manager_leaves_the_owners_session_open_when_it_ends() {
        let storage = Arc::new(Storage::default());
        let session = open(storage.clone());
        let clients = ClientSessions::new(
            false,
            client_build(42),
            "Fixture device".into(),
            DevicePreferences::new(storage),
        );
        clients.borrow("host".into(), session.clone());
        clients.release();
        drop(clients);
        session.clone().ready().await.unwrap();
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pending_presentations_use_device_language_before_host_storage_is_read() {
        use crate::localization::UiLanguage;
        let storage = Arc::new(Storage::default());
        *storage.language.lock().unwrap() = br#""en""#.to_vec();
        let device = DevicePreferences::new(storage.clone());
        let session = ClientSession::create(
            Persistence::new(storage.clone(), device.clone()),
            storage.clone(),
            false,
            client_build(42),
            "Fixture device".into(),
        );
        let first = session.snapshot();
        assert_eq!(
            session.presentation().settings.connection_status_label(),
            "Disconnected"
        );
        assert_eq!(first.catalog.ui_language, UiLanguage::English);
        assert!(Arc::ptr_eq(&first, &session.snapshot()));
        device.select_language(UiLanguage::Japanese).unwrap();
        assert_eq!(session.snapshot().catalog.ui_language, UiLanguage::Japanese);
        assert_eq!(
            session.presentation().settings.connection_status_label(),
            "未接続"
        );
        assert_eq!(storage.loads.load(Ordering::SeqCst), 0);
        assert!(storage.writes.lock().unwrap().is_empty());
        session.clone().ready().await.unwrap();
        assert_eq!(session.snapshot().catalog.ui_language, UiLanguage::Japanese);
        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn switching_a_connection_off_survives_restore_without_losing_the_draft() {
        let storage = Arc::new(Storage::default());
        let session = open(storage.clone());
        session.clone().ready().await.unwrap();
        session
            .dispatch(Intent::SetDraftText {
                thread_id: Default::default(),
                text: "Keep my work".into(),
            })
            .unwrap()
            .wait()
            .await
            .unwrap();
        session
            .dispatch(Intent::SetConnectionEnabled { enabled: false })
            .unwrap()
            .wait()
            .await
            .unwrap();
        session.clone().flush().await.unwrap();
        session.shutdown().await.unwrap();

        let restored = open(storage.clone());
        restored.clone().ready().await.unwrap();
        assert!(!restored.snapshot().connection_enabled());
        assert_eq!(
            restored.snapshot().draft(Default::default()).text,
            "Keep my work"
        );
        // An off connection must not even parse a ticket or load an identity.
        assert!(
            !restored
                .clone()
                .resume("not a ticket".into())
                .await
                .unwrap()
        );
        restored
            .dispatch(Intent::SetConnectionEnabled { enabled: true })
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert!(restored.snapshot().connection_enabled());
        assert!(
            restored
                .clone()
                .resume("not a ticket".into())
                .await
                .is_err()
        );

        storage.fail_save.store(true, Ordering::SeqCst);
        restored
            .dispatch(Intent::SetConnectionEnabled { enabled: false })
            .unwrap()
            .wait()
            .await
            .unwrap();
        assert!(restored.clone().flush().await.is_err());
        assert!(!restored.snapshot().connection_enabled());
        storage.fail_save.store(false, Ordering::SeqCst);
        restored.clone().flush().await.unwrap();
        let saved = crate::persistence::decode(&storage.bytes.lock().unwrap()).unwrap();
        assert!(!saved.connection_enabled());
        assert_eq!(saved.draft(Default::default()).text, "Keep my work");
        restored.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn publication_allows_reentrant_session_reads_and_flush() {
        struct NativeRead {
            session: Arc<ClientSession>,
            observed: Mutex<Option<(String, bool)>>,
        }
        impl futures_util::task::ArcWake for NativeRead {
            fn wake_by_ref(this: &Arc<Self>) {
                let readable = this.session.phase.try_lock().is_ok();
                if !readable {
                    *this.observed.lock().unwrap() = Some(("session lock held".into(), false));
                    return;
                }
                let cwd = this
                    .session
                    .snapshot()
                    .panes
                    .primary()
                    .navigation
                    .cwd
                    .clone();
                let flush_pending = this.session.clone().flush().now_or_never().is_none();
                *this.observed.lock().unwrap() = Some((cwd, flush_pending));
            }
        }
        let session = open(Arc::new(Storage::default()));
        let store = session.clone().ready().await.unwrap();
        let probe = Arc::new(NativeRead {
            session: session.clone(),
            observed: Mutex::new(None),
        });
        let waker = futures_util::task::waker(probe.clone());
        let mut context = std::task::Context::from_waker(&waker);
        let mut snapshots = store.store.subscribe();
        snapshots.borrow_and_update();
        let mut waiting = Box::pin(snapshots.changed());
        assert!(std::future::Future::poll(waiting.as_mut(), &mut context).is_pending());
        let receipt = session
            .dispatch(Intent::NewChat {
                cwd: "/reentrant".into(),
            })
            .unwrap();
        assert_eq!(
            *probe.observed.lock().unwrap(),
            Some(("/reentrant".into(), true))
        );
        drop(waiting);
        receipt.wait().await.unwrap();
        session.shutdown().await.unwrap();
    }

    #[derive(Default)]
    struct Gate {
        entered: Notify,
        released: Mutex<bool>,
        wake: Condvar,
    }
    impl Gate {
        fn block(&self) {
            self.entered.notify_one();
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
        }
        fn release(&self) {
            *self.released.lock().unwrap() = true;
            self.wake.notify_all();
        }
    }

    #[derive(Default)]
    struct Storage {
        bytes: Mutex<Vec<u8>>,
        model_preferences: Mutex<Vec<u8>>,
        editor_preferences: Mutex<Vec<u8>>,
        language: Mutex<Vec<u8>>,
        writes: Mutex<Vec<Vec<u8>>>,
        loads: AtomicUsize,
        read_gate: Mutex<Option<Arc<Gate>>>,
        write_gate: Mutex<Option<Arc<Gate>>>,
        fail_save: AtomicBool,
        saved: Notify,
        diagnostics_directory: Option<String>,
    }
    impl ClientStorage for Storage {
        fn load(&self) -> Result<Vec<u8>, AgentError> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            let gate = self.read_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.block();
            }
            Ok(self.bytes.lock().unwrap().clone())
        }
        fn save(&self, bytes: Vec<u8>) -> Result<(), AgentError> {
            let gate = self.write_gate.lock().unwrap().take();
            if let Some(gate) = gate {
                gate.block();
            }
            if self.fail_save.load(Ordering::SeqCst) {
                return Err(error("disk unavailable"));
            }
            crate::persistence::decode(&bytes).unwrap();
            *self.bytes.lock().unwrap() = bytes.clone();
            self.writes.lock().unwrap().push(bytes);
            self.saved.notify_one();
            Ok(())
        }
    }
    impl PreferenceStorage for Storage {
        fn load(&self, key: PreferenceKey) -> Result<Vec<u8>, AgentError> {
            Ok(match key {
                PreferenceKey::Models => self.model_preferences.lock().unwrap().clone(),
                PreferenceKey::Editor => self.editor_preferences.lock().unwrap().clone(),
                PreferenceKey::Language => self.language.lock().unwrap().clone(),
                PreferenceKey::Composer => Vec::new(),
            })
        }
        fn save(&self, key: PreferenceKey, bytes: Vec<u8>) -> Result<(), AgentError> {
            match key {
                PreferenceKey::Models => {
                    serde_json::from_slice::<crate::state::ModelPreferences>(&bytes).unwrap();
                    *self.model_preferences.lock().unwrap() = bytes;
                }
                PreferenceKey::Editor => {
                    serde_json::from_slice::<crate::persistence::EditorPreferences>(&bytes)
                        .unwrap();
                    *self.editor_preferences.lock().unwrap() = bytes;
                }
                PreferenceKey::Composer => {
                    serde_json::from_slice::<crate::state::ComposerPreferences>(&bytes).unwrap();
                }
                PreferenceKey::Language => {
                    serde_json::from_slice::<crate::localization::UiLanguage>(&bytes).unwrap();
                    *self.language.lock().unwrap() = bytes;
                }
            }
            Ok(())
        }
    }
    impl ClientPlatform for Storage {
        fn identity(&self) -> Result<Vec<u8>, AgentError> {
            Ok(vec![1; 32])
        }
        fn diagnostics_directory(&self) -> Result<Option<String>, AgentError> {
            Ok(self.diagnostics_directory.clone())
        }
        fn conversation_directory(&self) -> Result<Option<String>, AgentError> {
            Ok(None)
        }
    }
    fn edit(text: &str) -> Intent {
        Intent::SetDraftText {
            thread_id: "draft".into(),
            text: text.into(),
        }
    }
    fn text(bytes: &[u8]) -> String {
        crate::persistence::decode(bytes).unwrap().input.drafts[&"draft".into()]
            .text
            .clone()
    }
    fn client_build(number: u64) -> ClientBuild {
        ClientBuild {
            number,
            revision: "test-revision".into(),
        }
    }

    fn invalid_invitation() -> Invitation {
        Invitation {
            endpoint: "invalid ticket".into(),
            invitation: uuid::Uuid::nil(),
            expires_at: u64::MAX,
            host_name: "Fixture Host".into(),
            ai_recipients: Vec::new(),
            transcription_recipient: None,
        }
    }

    fn clients() -> Arc<ClientSessions> {
        ClientSessions::new(
            false,
            client_build(42),
            "Fixture device".into(),
            DevicePreferences::new(Arc::new(Storage::default())),
        )
    }

    fn open(storage: Arc<Storage>) -> Arc<ClientSession> {
        ClientSession::create(
            Persistence::new(storage.clone(), DevicePreferences::new(storage.clone())),
            storage,
            false,
            client_build(42),
            "Fixture device".into(),
        )
    }

    #[allow(dead_code)]
    mod host_fixture {
        include!("../../tests/support/host.rs");
    }

    async fn incoming(
        host: &crate::transport::Endpoint,
    ) -> (
        host_fixture::Session,
        host_fixture::Reader,
        host_fixture::Writer,
    ) {
        let identity = crate::transport::Identity::from_bytes([1; 32]);
        let trust = crate::transport::Trust {
            allowed: [identity.node_id()].into(),
            ..Default::default()
        };
        let session = host
            .accept()
            .await
            .unwrap()
            .unwrap()
            .authorize(&trust)
            .unwrap();
        let (session, mut reader, writer) = host_fixture::accept(session).await;
        let identify = reader.read_request().await.unwrap().unwrap();
        assert_eq!(identify["method"], "host/device/identify");
        writer
            .reply(
                &identify,
                serde_json::json!({"result":{"storageScope":"fixture-storage","ticket":""}}),
            )
            .await
            .unwrap();
        (session, reader, writer)
    }

    async fn connected(store: &AgentStore, expected: bool) {
        let mut snapshots = store.store.subscribe();
        while snapshots.borrow_and_update().connection.connected != expected {
            snapshots.changed().await.unwrap();
        }
    }

    async fn read_name(
        session: &ClientSession,
        reader: &mut host_fixture::Reader,
        writer: &host_fixture::Writer,
        name: &str,
    ) {
        let receipt = session
            .dispatch(Intent::LoadHostName(
                crate::state::operations::LoadHostName {},
            ))
            .unwrap();
        // Host facts remain valid when the user navigates before the reply.
        session
            .dispatch(Intent::ShowThreadList)
            .unwrap()
            .wait()
            .await
            .unwrap();
        loop {
            let request = reader.read_request().await.unwrap().unwrap();
            if request["method"] == "host/name" {
                writer
                    .reply(&request, serde_json::json!({"result": name}))
                    .await
                    .unwrap();
                break;
            }
        }
        receipt.wait().await.unwrap();
        assert_eq!(
            session.snapshot().connection.host_name.as_deref(),
            Some(name)
        );
    }

    #[tokio::test]
    async fn two_live_hosts_survive_switching_and_recover_together_after_backgrounding() {
        use crate::transport::{Endpoint, Identity, Relays};
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let clients = clients();
            let mac_storage = Arc::new(Storage::default());
            let windows_storage = Arc::new(Storage::default());
            let mac = clients.open("mac".into(), mac_storage.clone(), mac_storage.clone());
            let mac_store = mac.clone().ready().await.unwrap();
            let mac_host = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let windows_host = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let connect = |host: &Endpoint| Connection {
                ticket: host.ticket().to_string(),
                identity: vec![1; 32],
                invitation: None,
                use_relays: false,
                device_name: "Phone".into(),
            };
            let (result, (mac_peer, mut mac_reader, mac_writer)) =
                tokio::join!(mac_store.reconnect(connect(&mac_host)), incoming(&mac_host));
            result.unwrap();
            clients.record_scene(3);
            // Hosts opened while inactive must inherit the same lifecycle.
            let windows = clients.open(
                "windows".into(),
                windows_storage.clone(),
                windows_storage.clone(),
            );
            let windows_store = windows.clone().ready().await.unwrap();
            let (result, (windows_peer, mut windows_reader, windows_writer)) = tokio::join!(
                windows_store.reconnect(connect(&windows_host)),
                incoming(&windows_host)
            );
            result.unwrap();
            for _ in 0..3 {
                let selected = clients.open("mac".into(), mac_storage.clone(), mac_storage.clone());
                read_name(&selected, &mut mac_reader, &mac_writer, "Mac").await;
                let selected = clients.open(
                    "windows".into(),
                    windows_storage.clone(),
                    windows_storage.clone(),
                );
                read_name(&selected, &mut windows_reader, &windows_writer, "Windows").await;
                assert!(mac.snapshot().connection.connected);
                assert!(windows.snapshot().connection.connected);
            }
            mac_peer.close();
            windows_peer.close();
            tokio::join!(
                connected(&mac_store, false),
                connected(&windows_store, false)
            );
            let pause = std::time::Duration::from_millis(2200);
            let (mac_retry, windows_retry) = tokio::join!(
                tokio::time::timeout(pause, mac_host.accept()),
                tokio::time::timeout(pause, windows_host.accept())
            );
            assert!(mac_retry.is_err(), "inactive Mac connection retried");
            assert!(
                windows_retry.is_err(),
                "inactive Windows connection retried"
            );
            clients.record_scene(1);
            let (
                (_mac_peer, mut mac_reader, mac_writer),
                (_windows_peer, mut windows_reader, windows_writer),
            ) = tokio::join!(incoming(&mac_host), incoming(&windows_host));
            tokio::join!(connected(&mac_store, true), connected(&windows_store, true));
            read_name(&mac, &mut mac_reader, &mac_writer, "Mac recovered").await;
            read_name(
                &windows,
                &mut windows_reader,
                &windows_writer,
                "Windows recovered",
            )
            .await;
            mac.dispatch(Intent::SetConnectionEnabled { enabled: false })
                .unwrap()
                .wait()
                .await
                .unwrap();
            connected(&mac_store, false).await;
            assert!(mac_store.reconnect(connect(&mac_host)).await.is_err());
            clients.record_scene(3);
            clients.record_scene(1);
            assert!(
                tokio::time::timeout(pause, mac_host.accept())
                    .await
                    .is_err(),
                "an off connection retried after a foreground wakeup"
            );
            read_name(
                &windows,
                &mut windows_reader,
                &windows_writer,
                "Windows stays connected",
            )
            .await;
            mac.dispatch(Intent::SetConnectionEnabled { enabled: true })
                .unwrap()
                .wait()
                .await
                .unwrap();
            let (_mac_peer, mut mac_reader, mac_writer) = incoming(&mac_host).await;
            connected(&mac_store, true).await;
            read_name(&mac, &mut mac_reader, &mac_writer, "Mac enabled again").await;
            clients
                .remove("windows".into())
                .unwrap()
                .shutdown()
                .await
                .unwrap();
            read_name(&mac, &mut mac_reader, &mac_writer, "Mac still connected").await;
            assert!(clients.session("windows".into()).is_none());
            clients.shutdown().await.unwrap();
            mac_host.close().await;
            windows_host.close().await;
        })
        .await
        .expect("multi-Host connections or foreground recovery stalled");
    }

    #[tokio::test]
    async fn a_failed_host_save_does_not_block_other_hosts_or_their_queued_work() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let clients = clients();
            let old_storage = Arc::new(Storage::default());
            let old = clients.open("mac".into(), old_storage.clone(), old_storage.clone());
            old.dispatch(edit("saved old Host work")).unwrap();
            old.clone().flush().await.unwrap();
            old.dispatch(edit("unsaved old Host edit")).unwrap();
            old_storage.fail_save.store(true, Ordering::SeqCst);
            assert!(old.clone().flush().await.is_err());

            let storage = Arc::new(Storage::default());
            let next = clients.open("windows".into(), storage.clone(), storage.clone());
            let queued = next.dispatch(edit("new Host work")).unwrap();
            next.clone().ready().await.unwrap();
            queued.wait().await.unwrap();
            assert!(clients.flush().await.is_err());
            assert!(old.persistence_error().is_some());
            assert!(next.persistence_error().is_none());
            assert_eq!(text(&storage.bytes.lock().unwrap()), "new Host work");
            assert_eq!(
                text(&old_storage.bytes.lock().unwrap()),
                "saved old Host work"
            );
            old_storage.fail_save.store(false, Ordering::SeqCst);
            clients.flush().await.unwrap();
            assert!(old.persistence_error().is_none());
            assert_eq!(
                text(&old_storage.bytes.lock().unwrap()),
                "unsaved old Host edit"
            );
            clients.shutdown().await.unwrap();
        })
        .await
        .expect("one Host's failed save blocked another Host");
    }

    #[tokio::test]
    async fn pairing_reads_device_preferences_after_pending_saves_and_keeps_the_current_host_on_failure()
     {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let current_storage = Arc::new(Storage::default());
            let clients = clients();
            let current = clients.open(
                "mac".into(),
                current_storage.clone(),
                current_storage.clone(),
            );
            current.clone().ready().await.unwrap();
            current.dispatch(edit("unsaved current Host")).unwrap();
            let gate = Arc::new(Gate::default());
            let _release = scopeguard::guard(gate.clone(), |gate| gate.release());
            *current_storage.write_gate.lock().unwrap() = Some(gate.clone());
            let storage = Arc::new(Storage::default());
            let pairing_clients = clients.clone();
            let pairing_storage = storage.clone();
            let pairing = tokio::spawn(async move {
                pairing_clients
                    .pair(
                        pairing_storage.clone(),
                        pairing_storage,
                        invalid_invitation(),
                    )
                    .await
            });
            gate.entered.notified().await;
            assert_eq!(storage.loads.load(Ordering::SeqCst), 0);
            gate.release();
            assert!(pairing.await.unwrap().is_err());
            assert_eq!(
                text(&current_storage.bytes.lock().unwrap()),
                "unsaved current Host"
            );
            current.dispatch(edit("still usable")).unwrap();
            current.shutdown().await.unwrap();
            assert_eq!(text(&current_storage.bytes.lock().unwrap()), "still usable");
        })
        .await
        .expect("pairing did not flush device preferences");
    }

    #[tokio::test]
    async fn adopting_a_provisional_pair_enables_autosave_while_discarding_it_keeps_saved_work() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let storage = Arc::new(Storage::default());
            *storage.bytes.lock().unwrap() = b"previous pairing".to_vec();
            let provisional = open(storage.clone());
            provisional
                .install(
                    AgentStore::offline(Vec::new(), None, "test-revision".into())
                        .await
                        .unwrap(),
                    false,
                )
                .unwrap();
            provisional.shutdown().await.unwrap();
            assert_eq!(*storage.bytes.lock().unwrap(), b"previous pairing");
            assert!(storage.writes.lock().unwrap().is_empty());

            let adopted = open(storage.clone());
            adopted
                .install(
                    AgentStore::offline(Vec::new(), None, "test-revision".into())
                        .await
                        .unwrap(),
                    false,
                )
                .unwrap();
            adopted.clone().ready().await.unwrap();
            adopted.dispatch(edit("paired Host work")).unwrap();
            storage.saved.notified().await;
            assert_eq!(text(&storage.bytes.lock().unwrap()), "paired Host work");
            adopted.shutdown().await.unwrap();
        })
        .await
        .expect("pair adoption did not start persistence");
    }

    #[tokio::test]
    async fn restoration_applies_queued_intents_in_order_and_flushes_without_native_waiters() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let directory = tempfile::tempdir().unwrap();
            let storage = Arc::new(Storage {
                diagnostics_directory: Some(directory.path().to_string_lossy().into_owned()),
                ..Default::default()
            });
            let original = crate::store::Store::offline(Snapshot::default(), None);
            original.dispatch(edit("cached")).await.unwrap();
            *storage.bytes.lock().unwrap() =
                crate::persistence::encode(&original.snapshot()).unwrap();
            original.close().await.unwrap();
            let session = open(storage.clone());
            let first = session.dispatch(edit("first edit")).unwrap();
            let second = session.dispatch(edit("last edit")).unwrap();
            assert!(session.store().is_none());
            session.clone().ready().await.unwrap();
            assert!(session.store().is_some());
            assert_eq!(
                session.snapshot().input.drafts[&"draft".into()].text,
                "last edit"
            );
            session.clone().flush().await.unwrap();
            assert_eq!(text(&storage.bytes.lock().unwrap()), "last edit");
            first.wait().await.unwrap();
            second.wait().await.unwrap();
            let store = session.clone().ready().await.unwrap();
            assert!(Arc::ptr_eq(&store, &session.clone().ready().await.unwrap()));
            assert_eq!(storage.loads.load(Ordering::SeqCst), 1);
            // Once ready, dispatch remains synchronous even on a native UI thread.
            session.dispatch(edit("synchronous edit")).unwrap();
            assert_eq!(
                session.snapshot().input.drafts[&"draft".into()].text,
                "synchronous edit"
            );
            store.record_connection_event(ConnectionPhase::AppScene, 1);
            session.shutdown().await.unwrap();
            assert_eq!(text(&storage.bytes.lock().unwrap()), "synchronous edit");
            let trace = crate::diagnostics::connection::Trace::new("next-revision".into());
            trace.persist(directory.path().into()).unwrap();
            // Each trace has its own disk worker; reading the next trace does
            // not wait for the previous trace's checkpoint to reach disk.
            let reports = loop {
                let reports = trace.pending_reports().await;
                if !reports.is_empty() {
                    break reports;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            };
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].client_revision, "test-revision");
            assert!(
                reports[0].timeline.events.iter().any(|event| {
                    event.phase == ConnectionPhase::ClientBuild && event.value == 42
                })
            );
        })
        .await
        .expect("session lifecycle stalled");
    }

    #[tokio::test]
    async fn damaged_state_fails_every_queued_receipt_without_overwriting_user_work() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let storage = Arc::new(Storage::default());
            *storage.bytes.lock().unwrap() = b"damaged state".to_vec();
            let session = open(storage.clone());
            let receipt = session
                .dispatch(edit("must not replace saved work"))
                .unwrap();
            assert!(session.clone().ready().await.is_err());
            assert!(receipt.wait().await.is_err());
            assert!(session.dispatch(edit("rejected")).is_err());
            assert!(session.clone().flush().await.is_err());
            session.shutdown().await.unwrap();
            assert_eq!(*storage.bytes.lock().unwrap(), b"damaged state");
            assert!(storage.writes.lock().unwrap().is_empty());
        })
        .await
        .expect("session lifecycle stalled");
    }

    #[tokio::test]
    async fn closing_cancels_all_queued_receipts_without_loading() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let storage = Arc::new(Storage::default());
            let session = open(storage.clone());
            let receipts: Vec<_> = (0..256)
                .map(|_| session.dispatch(edit("queued")).unwrap())
                .collect();
            session.retire();
            for receipt in receipts {
                assert!(receipt.wait().await.is_err());
            }
            assert!(session.clone().ready().await.is_err());
            assert!(session.dispatch(edit("closed")).is_err());
            session.shutdown().await.unwrap();
            assert_eq!(storage.loads.load(Ordering::SeqCst), 0);
        })
        .await
        .expect("session lifecycle stalled");
    }

    #[tokio::test]
    async fn closing_during_a_blocked_read_never_installs_the_obsolete_store() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let storage = Arc::new(Storage::default());
            let gate = Arc::new(Gate::default());
            let _release = scopeguard::guard(gate.clone(), |gate| gate.release());
            *storage.read_gate.lock().unwrap() = Some(gate.clone());
            let session = open(storage.clone());
            let receipt = session.dispatch(edit("old Host")).unwrap();
            let loading = tokio::spawn(session.clone().ready());
            gate.entered.notified().await;
            session.retire();
            assert!(loading.await.unwrap().is_err());
            assert!(receipt.wait().await.is_err());
            gate.release();
            session.clone().shutdown().await.unwrap();
            assert!(session.store().is_none());
            assert!(storage.writes.lock().unwrap().is_empty());
        })
        .await
        .expect("session lifecycle stalled");
    }

    #[tokio::test]
    async fn cancelling_a_flush_cannot_release_its_write_lock_before_io_finishes() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let storage = Arc::new(Storage::default());
            let session = open(storage.clone());
            session.clone().ready().await.unwrap();
            session.dispatch(edit("old edit")).unwrap();
            let gate = Arc::new(Gate::default());
            let _release = scopeguard::guard(gate.clone(), |gate| gate.release());
            *storage.write_gate.lock().unwrap() = Some(gate.clone());
            let writing = tokio::spawn(session.clone().flush());
            gate.entered.notified().await;
            writing.abort();
            assert!(writing.await.unwrap_err().is_cancelled());
            session.dispatch(edit("latest edit")).unwrap();
            let final_write = tokio::spawn(session.clone().flush());
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_millis(30),
                    storage.saved.notified()
                )
                .await
                .is_err()
            );
            gate.release();
            final_write.await.unwrap().unwrap();
            session.shutdown().await.unwrap();
            assert_eq!(text(&storage.bytes.lock().unwrap()), "latest edit");
        })
        .await
        .expect("session lifecycle stalled");
    }

    #[tokio::test]
    async fn connection_lists_filter_hosts_keep_offline_rows_and_forget_removed_hosts() {
        let clients = clients();
        for host in ["mac", "server"] {
            let storage = Arc::new(Storage::default());
            let session = clients.open(host.into(), storage.clone(), storage);
            let snapshot = Snapshot {
                lists: crate::state::Lists {
                    threads: Some(Arc::new(serde_json::from_value(serde_json::json!({
                        "data":[{"id":{"provider":"codex","id":"same-session"},"name":host,"projectId":null}],
                        "projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":10
                    })).unwrap())),
                    ..Default::default()
                },
                ..Default::default()
            };
            let store = AgentStore::restored(snapshot, None, "fixture".into(), None).unwrap();
            let mut presentations = store.store.subscribe_presentation();
            session.install(store, false).unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                presentations.wait_for(|value| value.list.is_some()),
            )
            .await
            .unwrap()
            .unwrap();
        }
        let all = clients.thread_list(None).unwrap();
        assert_eq!(
            all.threads
                .iter()
                .map(|row| row.host_id.as_str())
                .collect::<Vec<_>>(),
            ["mac", "server"]
        );
        let selected = clients.thread_list(Some("server".into())).unwrap();
        assert_eq!(selected.threads.len(), 1);
        assert_eq!(selected.threads[0].title, "server");
        assert_eq!(selected.threads[0].host_id, "server");
        assert!(clients.thread_list(Some("missing".into())).is_none());
        clients
            .remove("server".into())
            .unwrap()
            .shutdown()
            .await
            .unwrap();
        assert_eq!(clients.thread_list(None).unwrap().threads.len(), 1);
        clients.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn switching_hosts_reuses_sessions_and_never_waits_for_another_hosts_io() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let clients = clients();
            let mac_storage = Arc::new(Storage::default());
            let mac = clients.open("mac".into(), mac_storage.clone(), mac_storage.clone());
            mac.clone().ready().await.unwrap();
            let gate = Arc::new(Gate::default());
            let _release = scopeguard::guard(gate.clone(), |gate| gate.release());
            *mac_storage.write_gate.lock().unwrap() = Some(gate.clone());
            mac.dispatch(edit("Mac draft")).unwrap();
            let saving = tokio::spawn(mac.clone().flush());
            gate.entered.notified().await;
            let windows_storage = Arc::new(Storage::default());
            let windows = clients.open(
                "windows".into(),
                windows_storage.clone(),
                windows_storage.clone(),
            );
            windows.dispatch(edit("Windows draft")).unwrap();
            windows.clone().flush().await.unwrap();
            for _ in 0..3 {
                assert!(Arc::ptr_eq(
                    &mac,
                    &clients.open("mac".into(), mac_storage.clone(), mac_storage.clone())
                ));
                assert!(Arc::ptr_eq(
                    &windows,
                    &clients.open(
                        "windows".into(),
                        windows_storage.clone(),
                        windows_storage.clone()
                    )
                ));
            }
            mac.dispatch(edit("Mac keeps working")).unwrap();
            assert_eq!(
                mac.snapshot().input.drafts[&"draft".into()].text,
                "Mac keeps working"
            );
            assert_eq!(
                windows.snapshot().input.drafts[&"draft".into()].text,
                "Windows draft"
            );
            assert_eq!(mac_storage.loads.load(Ordering::SeqCst), 1);
            assert_eq!(windows_storage.loads.load(Ordering::SeqCst), 1);
            gate.release();
            saving.await.unwrap().unwrap();
            let removed = clients.remove("windows".into()).unwrap();
            assert!(clients.session("windows".into()).is_none());
            assert!(removed.dispatch(edit("removed Host")).is_err());
            removed.shutdown().await.unwrap();
            mac.dispatch(edit("Mac survives removal")).unwrap();
            clients.shutdown().await.unwrap();
            assert_eq!(
                text(&mac_storage.bytes.lock().unwrap()),
                "Mac survives removal"
            );
            assert_eq!(
                text(&windows_storage.bytes.lock().unwrap()),
                "Windows draft"
            );
        })
        .await
        .expect("switching Hosts waited for an unrelated save or lost a session");
    }

    #[tokio::test]
    async fn repairing_one_host_orders_its_final_save_without_retiring_another_host() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            for fail_final_save in [false, true] {
                let clients = clients();
                let storage = Arc::new(Storage::default());
                let old = clients.open("windows".into(), storage.clone(), storage.clone());
                old.clone().ready().await.unwrap();
                old.dispatch(edit("old Windows work")).unwrap();
                let mac_storage = Arc::new(Storage::default());
                let mac = clients.open("mac".into(), mac_storage.clone(), mac_storage.clone());
                mac.clone().ready().await.unwrap();
                let paired = open(storage.clone());
                paired
                    .install(
                        AgentStore::offline(Vec::new(), None, "test-revision".into())
                            .await
                            .unwrap(),
                        false,
                    )
                    .unwrap();
                paired.dispatch(edit("paired Windows work")).unwrap();
                let gate = Arc::new(Gate::default());
                let _release = scopeguard::guard(gate.clone(), |gate| gate.release());
                *storage.write_gate.lock().unwrap() = Some(gate.clone());
                storage.fail_save.store(fail_final_save, Ordering::SeqCst);
                clients.adopt("windows".into(), paired.clone());
                gate.entered.notified().await;
                assert!(old.dispatch(edit("retired Windows work")).is_err());
                let cancelled = tokio::spawn(paired.clone().ready());
                cancelled.abort();
                let _ = cancelled.await;
                mac.dispatch(edit("Mac keeps working during re-pairing"))
                    .unwrap();
                mac.clone().flush().await.unwrap();
                gate.release();
                paired.clone().ready().await.unwrap();
                assert_eq!(paired.persistence_error().is_some(), fail_final_save);
                storage.fail_save.store(false, Ordering::SeqCst);
                paired.clone().flush().await.unwrap();
                assert!(paired.persistence_error().is_none());
                assert_eq!(text(&storage.bytes.lock().unwrap()), "paired Windows work");
                assert!(Arc::ptr_eq(
                    &paired,
                    &clients.session("windows".into()).unwrap()
                ));
                assert_eq!(
                    text(&mac_storage.bytes.lock().unwrap()),
                    "Mac keeps working during re-pairing"
                );
                clients.shutdown().await.unwrap();
            }
        })
        .await
        .expect("re-pairing lost the final save or blocked another Host");
    }

    #[tokio::test]
    async fn saving_recovers_after_storage_returns_and_dropping_a_session_flushes_its_last_edit() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let storage = Arc::new(Storage::default());
            let session = open(storage.clone());
            session.clone().ready().await.unwrap();
            session.dispatch(edit("unsaved work")).unwrap();
            storage.fail_save.store(true, Ordering::SeqCst);
            assert!(session.clone().flush().await.is_err());
            assert_eq!(
                session.persistence_error().as_deref(),
                Some("エラーが発生しました。\ndisk unavailable")
            );
            storage.fail_save.store(false, Ordering::SeqCst);
            session.clone().flush().await.unwrap();
            assert!(session.persistence_error().is_none());
            assert_eq!(text(&storage.bytes.lock().unwrap()), "unsaved work");
            // Drain the notification from the explicit flush.
            storage.saved.notified().await;
            session.dispatch(edit("last edit before dropping")).unwrap();
            drop(session);
            tokio::time::timeout(std::time::Duration::from_secs(2), storage.saved.notified())
                .await
                .unwrap();
            assert_eq!(
                text(&storage.bytes.lock().unwrap()),
                "last edit before dropping"
            );
        })
        .await
        .expect("session lifecycle stalled");
    }

    #[tokio::test]
    async fn a_failed_pairing_does_not_replace_a_previously_saved_host_snapshot() {
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            let storage = Arc::new(Storage::default());
            *storage.bytes.lock().unwrap() = b"saved work must remain".to_vec();
            assert!(
                clients()
                    .pair(storage.clone(), storage.clone(), invalid_invitation())
                    .await
                    .is_err()
            );
            assert_eq!(*storage.bytes.lock().unwrap(), b"saved work must remain");
            assert!(storage.writes.lock().unwrap().is_empty());
        })
        .await
        .expect("session lifecycle stalled");
    }
}

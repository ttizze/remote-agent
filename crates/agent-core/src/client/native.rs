//! Native connection ownership and transport recovery shared by mobile clients.
mod pane;
pub use pane::AgentPane;
mod recovery;

use super::{AgentError, error};
use crate::{
    diagnostics::{
        ConnectionPhase,
        connection::{Trace, identifier},
    },
    models::Invitation,
    state::{Intent, Snapshot},
    store::Outcome,
    transport::{Endpoint, Identity, Relays, Ticket},
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

#[derive(uniffi::Record)]
pub struct Connection {
    pub ticket: String,
    pub identity: Vec<u8>,
    pub invitation: Option<String>,
    pub use_relays: bool,
    pub device_name: String,
}

#[uniffi::export]
pub fn generate_identity() -> Vec<u8> {
    Identity::generate().to_bytes().to_vec()
}

#[uniffi::export]
pub fn ticket_identity(ticket: String) -> Result<String, AgentError> {
    Ok(ticket
        .parse::<Ticket>()
        .map_err(error)?
        .node_id()
        .to_string())
}

#[uniffi::export]
pub fn parse_invitation(contents: String, now: u64) -> Result<Invitation, AgentError> {
    let invitation: crate::models::Invitation = serde_json::from_str(&contents).map_err(|_| {
        error("接続情報を読み取れませんでした。PCで新しいQRコードを表示してください。")
    })?;
    validate_invitation(invitation.clone(), now)?;
    Ok(invitation)
}

#[uniffi::export]
pub fn validate_invitation(invitation: Invitation, now: u64) -> Result<String, AgentError> {
    let ticket = invitation
        .endpoint
        .parse::<Ticket>()
        .map_err(|_| error("接続情報が無効です。PCで新しいQRコードを表示してください。"))?;
    if now >= invitation.expires_at {
        return Err(error(
            "接続情報の有効期限が切れています。PCで新しいQRコードを表示してください。",
        ));
    }
    if invitation.host_name.trim().is_empty()
        || invitation
            .ai_recipients
            .iter()
            .any(|name| name.trim().is_empty())
        || invitation
            .transcription_recipient
            .as_ref()
            .is_some_and(|name| name.trim().is_empty())
    {
        return Err(error(
            "送信先の情報を確認できませんでした。PCで新しいQRコードを表示してください。",
        ));
    }
    Ok(ticket.node_id().to_string())
}

#[derive(uniffi::Object)]
pub struct AgentStore {
    pub(super) store: Arc<crate::store::Store>,
    recovery: Mutex<Option<recovery::Recovery>>,
    connection_active: tokio::sync::watch::Sender<bool>,
    connection_work: Arc<tokio::sync::Mutex<()>>,
    endpoint: tokio::sync::Mutex<Option<NativeEndpoint>>,
    trace: Arc<Trace>,
}
struct NativeEndpoint {
    endpoint: Endpoint,
    use_relays: bool,
}

impl AgentStore {
    async fn connect_recording(
        &self,
        connection: Connection,
        reuse: bool,
    ) -> Result<bool, AgentError> {
        self.recovery.lock().unwrap().take();
        let _attempt = self.connection_work.lock().await;
        let started = std::time::Instant::now();
        self.resume_connection(started, async {
            let Connection {
                ticket,
                identity,
                invitation,
                use_relays,
                device_name,
            } = connection;
            let (endpoint, ticket, invitation) = self
                .connection_endpoint(ticket, identity, invitation, use_relays)
                .await?;
            let endpoint_ms = started.elapsed().as_millis() as u64;
            let result = if reuse && invitation.is_none() {
                self.store
                    .resume(&endpoint, &ticket, &device_name)
                    .await
                    .map_err(error)
            } else {
                self.store
                    .reconnect(&endpoint, &ticket, invitation, &device_name)
                    .await
                    .map_err(error)
            };
            if invitation.is_none() || result.is_ok() {
                *self.recovery.lock().unwrap() = Some(recovery::Recovery::start(
                    self.store.clone(),
                    endpoint,
                    ticket,
                    device_name,
                    self.trace.client_revision().into(),
                    self.connection_work.clone(),
                    self.connection_active.subscribe(),
                ));
            }
            let mut performance = result?;
            performance.endpoint_ms = endpoint_ms;
            performance.total_ms = started.elapsed().as_millis() as u64;
            Ok::<_, AgentError>(performance.with_client(
                self.trace.client_revision(),
                crate::diagnostics::ClientPlatform::current(),
            ))
        })
        .await
    }

    async fn resume_connection(
        &self,
        started: std::time::Instant,
        connection: impl Future<Output = Result<crate::diagnostics::ConnectionPerformance, AgentError>>,
    ) -> Result<bool, AgentError> {
        let attempt = identifier();
        self.trace.activate();
        self.trace
            .record(ConnectionPhase::ResumeStart, attempt, 0, 0);
        let cancelled = scopeguard::guard((), |_| {
            self.trace
                .record(ConnectionPhase::ResumeCancelled, attempt, 0, 0)
        });
        let result = connection.await;
        scopeguard::ScopeGuard::into_inner(cancelled);
        self.trace.record(
            if result.is_ok() {
                ConnectionPhase::ResumeReady
            } else {
                ConnectionPhase::ResumeFailed
            },
            attempt,
            0,
            started.elapsed().as_micros() as u64,
        );
        let mut performance = result?;
        performance.attempt_id = attempt;
        self.trace.record(
            ConnectionPhase::ResumeConnection,
            attempt,
            performance.connection_id,
            u64::from(performance.reused),
        );
        let reused = performance.reused;
        self.store.record_connection_performance(performance);
        self.trace.activate();
        Ok(reused)
    }

    async fn connection_endpoint(
        &self,
        ticket: String,
        identity: Vec<u8>,
        invitation: Option<String>,
        use_relays: bool,
    ) -> Result<(Endpoint, Ticket, Option<uuid::Uuid>), AgentError> {
        let secret = Zeroizing::new(identity);
        let bytes = Zeroizing::new(
            <[u8; 32]>::try_from(secret.as_slice())
                .map_err(|_| error("identity must contain 32 bytes"))?,
        );
        let identity = Identity::from_bytes(*bytes);
        let ticket = ticket.parse::<Ticket>().map_err(error)?;
        let invitation = invitation
            .map(|value| value.parse())
            .transpose()
            .map_err(error)?;
        let mut cached = self.endpoint.lock().await;
        if let Some(cached) = cached.as_ref()
            && cached.endpoint.node_id() == identity.node_id()
            && cached.use_relays == use_relays
        {
            return Ok((cached.endpoint.clone(), ticket, invitation));
        }
        let endpoint = Endpoint::bind_recording(
            identity,
            if use_relays {
                Relays::Default
            } else {
                Relays::Disabled
            },
            self.trace.clone(),
        )
        .await
        .map_err(error)?;
        *cached = Some(NativeEndpoint {
            endpoint: endpoint.clone(),
            use_relays,
        });
        Ok((endpoint, ticket, invitation))
    }
}

impl AgentStore {
    pub(crate) fn set_connection_active(&self, active: bool) {
        self.connection_active.send_replace(active);
    }
    /// Platform network monitors supply changes; transport recovery stays in Core.
    pub(crate) async fn network_changed(&self) {
        let endpoint = self
            .endpoint
            .lock()
            .await
            .as_ref()
            .map(|cached| cached.endpoint.clone());
        if let Some(endpoint) = endpoint {
            endpoint.network_change().await;
        }
        if let Some(recovery) = self.recovery.lock().unwrap().as_ref() {
            recovery.wake();
        }
    }
    /// A store for restored device state and its connection diagnostics.
    pub(crate) fn restored(
        snapshot: Snapshot,
        diagnostics_directory: Option<String>,
        client_revision: String,
        preferences: Option<Arc<super::persistence::DevicePreferences>>,
    ) -> Result<Arc<Self>, AgentError> {
        let started = std::time::Instant::now();
        let trace = Trace::new(client_revision);
        if let Some(directory) = diagnostics_directory {
            trace.persist(directory.into()).map_err(error)?;
        }
        trace.activate();
        let store = crate::store::Store::offline(snapshot, preferences);
        trace.record(
            ConnectionPhase::StoreRestored,
            0,
            0,
            started.elapsed().as_micros() as u64,
        );
        Ok(Arc::new(Self {
            store: Arc::new(store),
            recovery: Mutex::new(None),
            connection_active: tokio::sync::watch::channel(true).0,
            connection_work: Default::default(),
            endpoint: Default::default(),
            trace,
        }))
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl AgentStore {
    #[uniffi::constructor]
    pub async fn offline(
        persisted: Vec<u8>,
        diagnostics_directory: Option<String>,
        client_revision: String,
    ) -> Result<Arc<Self>, AgentError> {
        Self::restored(
            crate::persistence::decode(&persisted).map_err(error)?,
            diagnostics_directory,
            client_revision,
            None,
        )
    }

    pub async fn reconnect(&self, connection: Connection) -> Result<(), AgentError> {
        self.connect_recording(connection, false).await.map(|_| ())
    }

    /// Foreground recovery reuses a responsive session and the endpoint identity.
    pub async fn resume(&self, connection: Connection) -> Result<bool, AgentError> {
        self.connect_recording(connection, true).await
    }
    /// Platform lifecycle boundaries only; shared transport measurements stay in Core.
    pub fn record_connection_event(&self, phase: ConnectionPhase, value: u64) {
        if matches!(
            phase,
            ConnectionPhase::AppPreparation
                | ConnectionPhase::SnapshotRead
                | ConnectionPhase::ClientBuild
                | ConnectionPhase::IdentityRead
                | ConnectionPhase::UiConnectStart
                | ConnectionPhase::UiConnectReady
                | ConnectionPhase::UiConnectFailed
                | ConnectionPhase::UiConnectCancelled
                | ConnectionPhase::ListPublished
                | ConnectionPhase::ListViewUpdated
                | ConnectionPhase::AppScene
        ) {
            if matches!(phase, ConnectionPhase::UiConnectStart) {
                self.trace.activate();
            }
            self.trace.record(phase, 0, 0, value);
        }
    }

    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.store.snapshot()
    }

    pub async fn open_pane(&self, cwd: String) -> Result<Arc<AgentPane>, AgentError> {
        Ok(Arc::new(AgentPane {
            pane: self.store.open_pane(cwd).map_err(error)?,
        }))
    }

    pub fn presentation(&self) -> Arc<crate::presentation::view::Presentation> {
        self.store.presentation()
    }

    pub async fn next_presentation(
        &self,
        previous: Arc<crate::presentation::view::Presentation>,
    ) -> Result<Arc<crate::presentation::view::Presentation>, AgentError> {
        let mut updates = self.store.subscribe_presentation();
        loop {
            updates.borrow_and_update();
            let current = self.store.presentation();
            if current.changes(previous.clone()).changed {
                return Ok(current);
            }
            updates.changed().await.map_err(error)?;
        }
    }

    /// A native view of one Host browser tab for `thread`.
    pub fn browser_session(
        &self,
        thread: crate::session::SessionRef,
    ) -> Arc<crate::store::BrowserSession> {
        crate::store::BrowserSession::new(self.store.clone(), thread)
    }

    /// Prepare network access without capturing or sending microphone audio.
    pub fn prepare_dictation(&self) -> Arc<crate::client::DictationPreparation> {
        Arc::new(self.store.prepare_dictation())
    }

    /// Enqueue synchronously; native task scheduling cannot reorder UI intents.
    pub fn dispatch(&self, intent: Intent) -> Result<Arc<Receipt>, AgentError> {
        let receipt = self.store.dispatch(intent);
        Ok(Arc::new(Receipt {
            result: Mutex::new(Some(Box::pin(async move { receipt.await.map_err(error) }))),
        }))
    }

    pub async fn shutdown(&self) -> Result<(), AgentError> {
        self.recovery.lock().unwrap().take();
        let result = self.store.close().await.map_err(error);
        if let Some(cached) = self.endpoint.lock().await.take() {
            cached.endpoint.close().await;
        }
        self.trace.close().await;
        result
    }
}

type Completion = Pin<Box<dyn Future<Output = Result<Outcome, AgentError>> + Send>>;
#[derive(uniffi::Object)]
pub struct Receipt {
    pub(crate) result: Mutex<Option<Completion>>,
}
#[uniffi::export(async_runtime = "tokio")]
impl Receipt {
    pub async fn wait(&self) -> Result<Outcome, AgentError> {
        let result = self
            .result
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| error("receipt was already awaited"))?;
        result.await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_transfer_errors_show_localized_host_reasons_without_wrappers() {
        use crate::localization::UiLanguage;
        use crate::presentation::error::localized_error_message;
        for (code, japanese, english) in [
            (
                "file_not_found",
                "ファイルまたはフォルダが見つかりません。",
                "The file or folder could not be found.",
            ),
            (
                "file_access_denied",
                "ファイルまたはフォルダにアクセスする権限がありません。",
                "You do not have permission to access this file or folder.",
            ),
            (
                "file_operation_failed",
                "ホストで操作に失敗しました。\nHost supplied reason",
                "The operation failed on the Host.\nHost supplied reason",
            ),
        ] {
            let peer = crate::peer::PeerError::Remote {
                error: serde_json::json!({"code":code,"message":"Host supplied reason"})
                    .to_string(),
                delivery: agent_protocol::error::Delivery::NotSent,
                sequence: Some(1),
            };
            let error = super::error(crate::peer::PeerError::from(
                crate::transfers::TransferError::from(peer),
            ));
            assert_eq!(error.to_string(), japanese);
            assert_eq!(
                localized_error_message(UiLanguage::Japanese, &error.to_string()),
                japanese
            );
            assert_eq!(
                localized_error_message(UiLanguage::English, &error.to_string()),
                english
            );
        }
    }
    fn item_text(item: &agent_protocol::items::Item) -> Option<&str> {
        match item.body() {
            agent_protocol::items::ItemBody::AssistantText { text, .. } => Some(text),
            agent_protocol::items::ItemBody::UserMessage { text, .. } => text.as_deref(),
            agent_protocol::items::ItemBody::Reasoning { content, .. } => {
                content.first().map(String::as_str)
            }
            _ => None,
        }
    }

    #[allow(dead_code)]
    mod host_fixture {
        include!("../../tests/support/host.rs");
    }
    use super::*;
    use std::time::Duration;
    #[tokio::test]
    async fn invitation_preview_preserves_recipients_and_rechecks_expiry() {
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let invitation = Invitation {
            endpoint: host.ticket().to_string(),
            invitation: uuid::Uuid::new_v4(),
            expires_at: 200,
            host_name: "My PC".into(),
            ai_recipients: vec!["Future AI service".into()],
            transcription_recipient: None,
        };
        let contents = serde_json::to_string(&invitation).unwrap();
        let parsed = parse_invitation(contents, 100).unwrap();
        assert_eq!(parsed, invitation);
        assert_eq!(
            validate_invitation(parsed.clone(), 100).unwrap(),
            host.ticket().node_id().to_string()
        );
        assert!(validate_invitation(parsed.clone(), 200).is_err());
        let mut missing_name = parsed;
        missing_name.host_name.clear();
        assert!(validate_invitation(missing_name, 100).is_err());
        host.close().await;
    }

    async fn scoped_incoming(
        host: &Endpoint,
        trust: &crate::transport::Trust,
    ) -> (
        host_fixture::Session,
        host_fixture::Reader,
        host_fixture::Writer,
    ) {
        let session = host
            .accept()
            .await
            .unwrap()
            .unwrap()
            .authorize(trust)
            .unwrap();
        let (session, mut reader, writer) = host_fixture::accept(session).await;
        let request = reader.read_request().await.unwrap().unwrap();
        assert_eq!(request["method"], "host/device/identify");
        writer
            .reply(
                &request,
                serde_json::json!({"result":{"storageScope":"fixture-storage","ticket":""}}),
            )
            .await
            .unwrap();
        (session, reader, writer)
    }

    #[tokio::test]
    async fn automatic_recovery_pauses_in_background_and_reconnects_once_when_active() {
        tokio::time::timeout(Duration::from_secs(15), async {
            let identity = Identity::generate();
            let trust = crate::transport::Trust {
                allowed: [identity.node_id()].into(),
                ..Default::default()
            };
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
                .await
                .unwrap();
            let (incoming, store) = tokio::join!(scoped_incoming(&host, &trust), async {
                let store = AgentStore::offline(Vec::new(), None, "test-revision".into()).await?;
                store
                    .reconnect(Connection {
                        ticket: host.ticket().to_string(),
                        identity: identity.to_bytes().to_vec(),
                        invitation: None,
                        use_relays: false,
                        device_name: "Fixture device".into(),
                    })
                    .await?;
                Ok::<_, AgentError>(store)
            });
            let store = store.unwrap();
            let (first, _reader, _writer) = incoming;
            store.set_connection_active(false);
            first.close();
            let mut snapshots = store.store.subscribe();
            while snapshots.borrow_and_update().connection.connected {
                snapshots.changed().await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(2200), host.accept())
                    .await
                    .is_err(),
                "background recovery must not open a connection"
            );
            store.set_connection_active(true);
            let (second, _reader, _writer) = scoped_incoming(&host, &trust).await;
            while !snapshots.borrow_and_update().connection.connected {
                snapshots.changed().await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(200), host.accept())
                    .await
                    .is_err(),
                "a ready connection must have no competing retry owner"
            );
            store.shutdown().await.unwrap();
            second.close();
            host.close().await;
        })
        .await
        .expect("automatic recovery did not settle");
    }

    #[tokio::test]
    async fn shutdown_finishes_diagnostics_before_the_hosts_files_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("diagnostics");
        let store = AgentStore::offline(
            Vec::new(),
            Some(directory.to_string_lossy().into_owned()),
            "test-revision".into(),
        )
        .await
        .unwrap();
        store.shutdown().await.unwrap();
        assert!(directory.exists());
        std::fs::remove_dir_all(&directory).unwrap();
        // A retained native handle may finish publishing after its Host is gone.
        store.record_connection_event(ConnectionPhase::AppScene, 1);
        assert!(store.trace.pending_reports().await.is_empty());
        drop(store);
        assert!(!directory.exists());
    }

    #[tokio::test]
    async fn shutdown_releases_a_native_presentation_waiter() {
        let store = AgentStore::offline(Vec::new(), None, "test-revision".into())
            .await
            .unwrap();
        let previous = store.presentation();
        let weak = Arc::downgrade(&store);
        let waiting = store.clone();
        let task = tokio::spawn(async move {
            let mut snapshot = previous;
            while let Ok(next) = waiting.next_presentation(snapshot).await {
                snapshot = next;
            }
        });
        store.shutdown().await.unwrap();
        drop(store);
        tokio::time::timeout(Duration::from_millis(200), task)
            .await
            .expect("shutdown must release a native waiter without task cancellation")
            .unwrap();
        assert!(weak.upgrade().is_none());
    }
    #[tokio::test]
    async fn reconnect_reloads_history_and_catalogs_while_accounts_are_pending() {
        use crate::transport::Trust;
        use serde_json::json;
        for recovery in ["silent", "disconnected", "closed"] {
            tokio::time::timeout(Duration::from_secs(40), async {
            let identity = Identity::generate();
            let trust = Trust { allowed: [identity.node_id()].into(), ..Default::default() };
            let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
            let connection = || Connection {
                ticket: host.ticket().to_string(), identity: identity.to_bytes().to_vec(),
                invitation: None, use_relays: false,
                device_name: "Fixture device".into(),
            };
            let cached = crate::state::Snapshot {
                lists: crate::state::Lists {
                    list_query: Arc::new(crate::models::ListQuery { limit: 60, project_limit: 5, search_term: "retained search".into(), ..Default::default() }),
                    ..Default::default()
                },
 panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(crate::state::PaneState { navigation: Arc::new(crate::state::Navigation { thread_id: Some(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }), draft_key: agent_protocol::session::SessionRef {provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into()}.into(), ..Default::default() }), ..Default::default() })),
                ..Default::default()
            };
            let mut saved: serde_json::Value = serde_json::from_slice(&cached.serialize_local_state().unwrap()).unwrap();
            assert!(saved.get("list_query").is_none());
            saved["list_query"] = serde_json::to_value(&cached.lists.list_query).unwrap();
            let restored = AgentStore::offline(serde_json::to_vec(&saved).unwrap(), None, "test-revision".into()).await.unwrap();
            assert_eq!(*restored.snapshot().lists.list_query, crate::models::ListQuery::default());
            restored.shutdown().await.unwrap();
            let store = Arc::new(AgentStore { store: Arc::new(crate::store::Store::offline(cached, None)), recovery: Mutex::new(None), connection_active: tokio::sync::watch::channel(true).0, connection_work: Default::default(), endpoint: Default::default(), trace: Trace::new("development".into()) });
            let (connected, (session, reader, writer)) = tokio::join!(store.reconnect(connection()), scoped_incoming(&host, &trust));
            connected.unwrap();
            let first = session;
            let mut old = reader;
            let _first_writer = writer;
            assert!(store.snapshot().connected());
            store.dispatch(Intent::SetDraftText { thread_id: agent_protocol::session::SessionRef {provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into()}.into(), text: "preserved".into() })
                .unwrap().wait().await.unwrap();
            // Leave every automatic read pending on the old transport.
            let mut methods = std::collections::BTreeSet::new();
            for _ in 0..5 {
                let request = tokio::time::timeout(Duration::from_secs(2), old.read_request()).await
                    .expect("Connected must reload without native intents").unwrap().unwrap();
                methods.insert(request["method"].as_str().unwrap().to_owned());
            }
            assert_eq!(methods, ["host/session/list", "host/session/open", "host/model/list", "host/account/list", "host/taskActivity/read"].map(str::to_owned).into());
            let (release_accounts, accounts_released) = tokio::sync::oneshot::channel();
            let server = async {
                assert!(!matches!(old.read_request().await, Ok(Some(_))),
                    "reconnect must close the old stream without probing it with list/history reads");
                let (next, mut reader, writer) = scoped_incoming(&host, &trust).await;
                let mut requests = std::collections::BTreeMap::new();
                for _ in 0..5 {
                    let request = reader.read_request().await.unwrap().unwrap();
                    assert!(requests.insert(request["method"].as_str().unwrap().to_owned(), request).is_none());
                }
                writer.reply(&requests["host/taskActivity/read"], json!({"result":{"revision":0,"statuses":[],"waiting":[],"unread":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}})).await.unwrap();
                let list = &requests["host/session/list"];
                assert_eq!(list["params"]["limit"], 60);
                assert_eq!(list["params"]["searchTerm"], "retained search");
                let open = &requests["host/session/open"];
                assert_eq!(open["params"]["session"]["id"], "thread");
                // Finish the conversation before the lists; no reload invalidates another.
                writer.reply(open, json!({"result":{"session":{"provider":"codex","id":"thread"},"subscriptionId":uuid::Uuid::new_v4(),"revision":0,"response":{"thread":{"id":{"provider":"codex","id":"thread"},"turns":[{"id":"turn","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"after reconnect","phase":"unknown"}}}}}],"status":"unknown"}]}}}})).await.unwrap();
                writer.reply(&requests["host/model/list"], json!({ "result":{"data":[{"id":"fresh-model","model":{"provider": "codex", "id": "fresh-model"},"displayName":"Fresh", "settings": {"defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}}],"nextCursor":null}})).await.unwrap();
                writer.reply(list, json!({ "result":{"data":[{"id":{"provider":"codex","id":"thread"},"name":"reloaded"}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}})).await.unwrap();
                accounts_released.await.unwrap();
                writer.reply(&requests["host/account/list"], json!({"result":{"accounts":[],"selected":{}}})).await.unwrap();
                assert!(!matches!(reader.read_request().await, Ok(Some(_))));
                next.close();
            };
            let client = async {
                if recovery == "disconnected" { store.store.disconnect().await.unwrap(); }
                if recovery == "closed" { first.close(); }
                tokio::time::timeout(Duration::from_secs(5), store.reconnect(connection()))
                    .await.expect("reconnect must not wait for the old 30-second RPC deadline").unwrap();
                let mut updates = store.store.subscribe();
                loop {
                    let ready = {
                        let snapshot = updates.borrow_and_update();
                        snapshot.lists.threads.is_some() && !snapshot.catalog.models.is_empty() && snapshot.sessions.conversations.contains_key(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() })
                    };
                    if ready { break; }
                    updates.changed().await.unwrap();
                }
                let snapshot = store.store.snapshot();
                assert!(snapshot.connected());
                assert!(snapshot.panes.primary().error.is_none());
                assert_eq!(snapshot.panes.primary().navigation.thread_id.as_ref().map(|session| session.id.as_str()), Some("thread"));
                assert!(snapshot.lists.threads.as_ref().unwrap().data.iter().any(|thread| thread.id.as_ref().map(|session| session.id.as_str()) == Some("thread")));
                assert_eq!(item_text(&(snapshot.sessions.conversations[&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0])), Some("after reconnect"));
                assert_eq!(snapshot.catalog.models[0].model, agent_protocol::models::ModelRef {provider:agent_protocol::session::ProviderKind::Codex,id:"fresh-model".into()});
                assert_eq!(snapshot.input.drafts[&crate::state::DraftKey::from(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() })].text, "preserved");
                assert!(snapshot.catalog.account.accounts.is_none(), "account discovery must not gate connection, history or catalogs");
                release_accounts.send(()).unwrap();
                while store.snapshot().catalog.account.accounts.is_none() {
                    updates.changed().await.unwrap();
                }
                store.shutdown().await.unwrap();
            };
            tokio::join!(server, client);
            first.close(); host.close().await;
        }).await.unwrap();
        }
    }
    #[rstest::rstest]
    #[case(false, "live")]
    #[case(true, "live")]
    #[case(false, "slow")]
    #[case(true, "slow")]
    #[case(true, "silent")]
    #[case(true, "replacement-error")]
    #[case(true, "error")]
    #[tokio::test]
    async fn foreground_reuses_live_session_and_replaces_silent_session(
        #[case] selected: bool,
        #[case] mode: &str,
    ) {
        use crate::transport::Trust;
        use serde_json::{Value, json};
        let silent = mode == "silent";
        tokio::time::timeout(Duration::from_secs(10), async {
                    let identity = Identity::generate();
                    let trust = Trust { allowed: [identity.node_id()].into(), ..Default::default() };
                    let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
                    let connection = || Connection {
                        ticket: host.local_ticket().to_string(), identity: identity.to_bytes().to_vec(),
                        invitation: None, use_relays: false,
                        device_name: "Fixture device".into(),
                    };
                    let snapshot = Snapshot {
 panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(crate::state::PaneState { navigation: Arc::new(crate::state::Navigation {
                            thread_id: selected.then(|| agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }), draft_key: agent_protocol::session::SessionRef {provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into()}.into(),
                            ..Default::default()
                        }), ..Default::default() })), ..Default::default()
                    };
                    let store = AgentStore::offline(crate::persistence::encode(&snapshot).unwrap(), None, "test-revision".into()).await.unwrap();
                    let (connected, (first, mut reader, writer)) = tokio::join!(store.reconnect(connection()), scoped_incoming(&host, &trust));
                    connected.unwrap();
                    let old_identity = store.endpoint.lock().await.as_ref().unwrap().endpoint.node_id();
                    let response = |request: &Value, text: &str| {
                        let result = match request["method"].as_str().unwrap() {
                            "host/device/identify" => json!({"storageScope":"fixture-storage","ticket":""}),
                            "host/diagnostics/connection" => {
                                assert_eq!(request["params"]["client_revision"], "test-revision");
                                json!({})
                            },
                            "host/account/list" => json!({"accounts":[],"selected":{}}),
                            "host/taskActivity/read" => json!({"revision":u64::from(text == "after"),"statuses":[],"waiting":[],"unread":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
                            "host/session/list" => json!({"data":[{"id":{"provider":"codex","id":"thread"},"name":text}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}),
                            "host/session/open" => json!({"session":{"provider":"codex","id":"thread"},"subscriptionId":uuid::Uuid::new_v4(),"revision":0,"response":{"thread":{"id":{"provider":"codex","id":"thread"},"turns":[{"id":"turn","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":text,"phase":"unknown"}}}}}],"status":"unknown"}]}}}),
                            "host/model/list" => json!({"data":[],"nextCursor":null}),
                            method => panic!("unexpected request: {method}"),
                        };
                        json!({"result":result})
                    };
                    let server = async {
                        for _ in 0..(4 + usize::from(selected)) {
                            let request = reader.read_request().await.unwrap().unwrap();
                            writer.reply(&request, response(&request, "before")).await.unwrap();
                        }
                        let request = reader.read_request().await.unwrap().unwrap();
                        assert_eq!(request["method"], "host/device/identify");
                        if silent {
                            // Do not answer the Host check. Recovery must replace this
                            // transport without waiting for the normal 30-second deadline.
                            let (next, mut next_reader, next_writer) = scoped_incoming(&host, &trust).await;
                            let mut reads = 0;
                            while reads < 2 + usize::from(selected) {
                                let request = next_reader.read_request().await.unwrap().unwrap();
                                reads += usize::from(request["method"] != "host/diagnostics/connection");
                                next_writer.reply(&request, response(&request, "after")).await.unwrap();
                            }
                            assert!(!matches!(reader.read_request().await, Ok(Some(_))));
                            while let Ok(Some(_)) = next_reader.read_request().await {}
                            next.close();
                        } else {
                            if mode == "replacement-error" {
                                let incoming = host.accept().await.unwrap().unwrap().authorize(&trust).unwrap();
                                let (candidate, mut candidate_reader, candidate_writer) = host_fixture::accept(incoming).await;
                                let check = candidate_reader.read_request().await.unwrap().unwrap();
                                assert_eq!(check["method"], "host/device/identify");
                                candidate_writer.reply(&check, json!({"error":{"code":"request_failed","message":"candidate rejected"}})).await.unwrap();
                                assert!(!matches!(candidate_reader.read_request().await, Ok(Some(_))), "failed replacement must close before old connection succeeds");
                                candidate.close();
                            }
                            let reply = if mode == "error" {
                                json!({"error":{"code":"request_failed","message":"history unavailable"}})
                            } else { response(&request, "after") };
                            writer.reply(&request, reply).await.unwrap();
                            if mode == "slow" { tokio::time::sleep(Duration::from_millis(1200)).await; }
                            while let Ok(Some(request)) = reader.read_request().await {
                                let _ = writer.reply(&request, response(&request, "after")).await;
                            }
                        }
                    };
                    let client = async {
                        let mut updates = store.store.subscribe();
                        loop {
                            let ready = { let snapshot = updates.borrow_and_update(); snapshot.catalog.account.accounts.is_some() && snapshot.lists.threads.is_some() && (!selected || snapshot.sessions.conversations.contains_key(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() })) };
                            if ready { break; }
                            updates.changed().await.unwrap();
                        }
                        store.dispatch(Intent::SetDraftText { thread_id: agent_protocol::session::SessionRef {provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into()}.into(), text: "keep draft".into() }).unwrap().wait().await.unwrap();
                        let started = std::time::Instant::now();
                        let resumed = tokio::time::timeout(Duration::from_millis(500), store.resume(connection())).await
                            .expect("foreground recovery must not wait for provider reads or shutdown deadlines");
                        eprintln!("foreground mode={mode} selected={selected} elapsed_ms={}", started.elapsed().as_millis());
                        if mode == "error" { assert!(resumed.is_err()); } else {
                            assert_eq!(resumed.unwrap(), !silent);
                        }
                        if mode != "error" { loop {
                            let ready = {
                                let snapshot = updates.borrow_and_update();
                                snapshot.lists.task_activity.as_ref().is_some_and(|state| state.revision == 1)
                                    && (!silent || !selected || item_text(&(snapshot.sessions.conversations[&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0])) == Some("after"))
                            };
                            if ready { break; }
                            updates.changed().await.unwrap();
                        } }
                        assert!(store.snapshot().connected());
                        assert_eq!(store.snapshot().input.drafts[&crate::state::DraftKey::from(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() })].text, "keep draft");
                        assert_eq!(store.endpoint.lock().await.as_ref().unwrap().endpoint.node_id(), old_identity, "recovery retains the endpoint identity; discovered addresses can change");
                        store.shutdown().await.unwrap();
                    };
                    tokio::join!(server, client);
                    first.close(); host.close().await;
                }).await.unwrap();
    }

    #[tokio::test]
    async fn failed_and_cancelled_attempts_survive_store_recreation() {
        let directory = tempfile::tempdir().unwrap();
        let path = Some(directory.path().to_string_lossy().into_owned());
        let store = AgentStore::offline(Vec::new(), path.clone(), "test-revision".into())
            .await
            .unwrap();
        assert!(
            store
                .resume_connection(std::time::Instant::now(), async {
                    Err(error("private failure detail must never enter the journal"))
                })
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(1),
                store.resume_connection(std::time::Instant::now(), std::future::pending())
            )
            .await
            .is_err()
        );
        let reports = store.trace.pending_reports().await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].client_revision, "test-revision");
        let id = reports[0].timeline.id;
        assert!(
            !serde_json::to_string(&reports)
                .unwrap()
                .contains("private failure")
        );
        store.shutdown().await.unwrap();
        drop(store);
        let restored = AgentStore::offline(Vec::new(), path, "next-revision".into())
            .await
            .unwrap();
        let reports = restored.trace.pending_reports().await;
        let previous = reports
            .iter()
            .find(|report| report.timeline.id == id)
            .unwrap();
        assert_eq!(previous.client_revision, "test-revision");
        restored.record_connection_event(ConnectionPhase::AppScene, 1);
        let reports = restored.trace.pending_reports().await;
        assert_eq!(
            reports
                .iter()
                .find(|report| report.timeline.id != id)
                .unwrap()
                .client_revision,
            "next-revision"
        );
        assert!(
            previous
                .timeline
                .events
                .iter()
                .any(|event| event.phase == ConnectionPhase::ResumeFailed)
        );
        assert!(
            previous
                .timeline
                .events
                .iter()
                .any(|event| event.phase == ConnectionPhase::ResumeCancelled)
        );
        restored.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn resume_queues_diagnostics_without_waiting_for_the_receiver() {
        use crate::diagnostics::{ConnectionPerformance, ConnectionRoute};
        let (store, report) = crate::store::Store::mock_connection();
        let store = AgentStore {
            store: Arc::new(store),
            recovery: Mutex::new(None),
            connection_active: tokio::sync::watch::channel(true).0,
            connection_work: Default::default(),
            endpoint: Default::default(),
            trace: Trace::new("development".into()),
        };
        let (connected, connection) = tokio::sync::oneshot::channel();
        let mut resume = Box::pin(store.resume_connection(std::time::Instant::now(), async {
            connection.await.expect("mock connection was cancelled")
        }));
        assert!(futures_util::poll!(&mut resume).is_pending());
        connected
            .send(Ok(ConnectionPerformance {
                connection_id: 42,
                route: ConnectionRoute::Direct,
                endpoint_ms: 7,
                total_ms: 12,
                ..Default::default()
            }))
            .unwrap();
        // The diagnostic receiver has never been polled. Connection readiness
        // must still complete synchronously after the connection becomes ready.
        assert!(matches!(
            futures_util::poll!(&mut resume),
            std::task::Poll::Ready(Ok(false))
        ));
        assert!(store.snapshot().connected());
        drop(resume);
        // An unread diagnostic report must not prevent the real Store from closing.
        tokio::time::timeout(Duration::from_secs(10), store.shutdown())
            .await
            .expect("shutdown waited for diagnostic consumption")
            .unwrap();
        let mut report = Box::pin(report);
        let std::task::Poll::Ready(performance) = futures_util::poll!(&mut report) else {
            panic!("resume completed without queuing its diagnostic report");
        };
        assert_eq!(performance.connection_id, 42);
        assert!(matches!(performance.route, ConnectionRoute::Direct));
        assert_eq!(performance.endpoint_ms, 7);
        assert_eq!(performance.total_ms, 12);
    }

    #[tokio::test]
    async fn foreground_replacement_and_cancellation_respect_connection_ownership() {
        let identity = Identity::generate();
        let trust = crate::transport::Trust {
            allowed: [identity.node_id()].into(),
            ..Default::default()
        };
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        // Measure replacement over loopback without depending on LAN routing.
        let ticket = host.local_ticket();
        let first = Endpoint::bind(Identity::from_bytes(identity.to_bytes()), Relays::Disabled)
            .await
            .unwrap();
        let replacement = Endpoint::bind(identity, Relays::Disabled).await.unwrap();
        let (store, (old, old_reader, old_writer)) = tokio::join!(
            crate::store::Store::connect(
                &first,
                &ticket,
                Snapshot::default(),
                None,
                "Fixture device",
                None
            ),
            scoped_incoming(&host, &trust),
        );
        let store = Arc::new(store.unwrap());
        let (resumed, (next, mut reader, _writer)) =
            tokio::time::timeout(Duration::from_millis(500), async {
                tokio::join!(
                    store.resume(&replacement, &ticket, "Fixture device"),
                    scoped_incoming(&host, &trust)
                )
            })
            .await
            .expect("a changed endpoint must replace the session without probing the old one");
        resumed.unwrap();
        // Leave automatic list/model/account/task reads pending, then cancel the foreground read.
        let mut methods = std::collections::BTreeSet::new();
        for _ in 0..4 {
            let request = reader.read_request().await.unwrap().unwrap();
            methods.insert(request["method"].as_str().unwrap().to_owned());
        }
        assert_eq!(
            methods,
            [
                "host/session/list",
                "host/model/list",
                "host/account/list",
                "host/taskActivity/read"
            ]
            .map(str::to_owned)
            .into()
        );
        let recovering = store.clone();
        let endpoint = replacement.clone();
        let resume = tokio::spawn(async move {
            recovering
                .resume(&endpoint, &ticket, "Fixture device")
                .await
        });
        let request = tokio::time::timeout(Duration::from_secs(1), reader.read_request())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(request["method"], "host/device/identify");
        let candidate = host
            .accept()
            .await
            .unwrap()
            .unwrap()
            .authorize(&trust)
            .unwrap();
        let (candidate, mut candidate_reader, _candidate_writer) =
            host_fixture::accept(candidate).await;
        let check =
            tokio::time::timeout(Duration::from_millis(500), candidate_reader.read_request())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        assert_eq!(check["method"], "host/device/identify");
        tokio::time::timeout(Duration::from_millis(500), store.disconnect())
            .await
            .unwrap()
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(500), resume)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(!store.snapshot().connection.connected);
        assert!(
            !matches!(
                tokio::time::timeout(Duration::from_millis(500), candidate_reader.read_request())
                    .await
                    .unwrap(),
                Ok(Some(_))
            ),
            "cancellation must close the speculative session"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(1100), host.accept())
                .await
                .is_err(),
            "cancelled recovery must not start another connection"
        );
        candidate.close();
        store.close().await.unwrap();
        drop((old_reader, old_writer));
        old.close();
        next.close();
        first.close().await;
        replacement.close().await;
        host.close().await;
    }

    #[tokio::test]
    async fn unresponsive_setup_expires_and_the_same_endpoint_can_retry() {
        let identity = Identity::generate();
        let trust = crate::transport::Trust {
            allowed: [identity.node_id()].into(),
            ..Default::default()
        };
        let host = Endpoint::bind(Identity::generate(), Relays::Disabled)
            .await
            .unwrap();
        let endpoint = Endpoint::bind(identity, Relays::Disabled).await.unwrap();
        let ticket = host.local_ticket();
        let store = Arc::new(crate::store::Store::offline(Snapshot::default(), None));
        let connecting = tokio::spawn({
            let store = store.clone();
            let endpoint = endpoint.clone();
            let ticket = ticket.clone();
            async move {
                store
                    .reconnect(&endpoint, &ticket, None, "Fixture device")
                    .await
            }
        });
        let incoming = host
            .accept()
            .await
            .unwrap()
            .unwrap()
            .authorize(&trust)
            .unwrap();
        let (incoming, mut reader, _writer) = host_fixture::accept(incoming).await;
        let scope = reader.read_request().await.unwrap().unwrap();
        assert_eq!(scope["method"], "host/device/identify");
        let result = tokio::time::timeout(Duration::from_secs(12), connecting)
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(
                result,
                Err(crate::transport::TransportError::Peer(
                    crate::peer::PeerError::RequestTimeout { .. }
                ))
            ),
            "unexpected setup result: {result:?}"
        );
        assert!(!store.snapshot().connection.connected);
        assert!(
            !matches!(reader.read_request().await, Ok(Some(_))),
            "deadline closes the pending session"
        );
        let (retried, (next, next_reader, next_writer)) =
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(
                    store.resume(&endpoint, &ticket, "Fixture device"),
                    scoped_incoming(&host, &trust)
                )
            })
            .await
            .unwrap();
        retried.unwrap();
        assert!(store.snapshot().connection.connected);
        store.close().await.unwrap();
        drop((next_reader, next_writer));
        next.close();
        incoming.close();
        endpoint.close().await;
        host.close().await;
    }

    #[tokio::test]
    async fn active_reads_retain_transport_navigation_and_draft_after_error() {
        use crate::transport::Trust;
        use serde_json::json;
        for selected in [false, true] {
            tokio::time::timeout(Duration::from_secs(40), async {
                let identity = Identity::generate();
                let trust = Trust { allowed: [identity.node_id()].into(), ..Default::default() };
                let host = Endpoint::bind(Identity::generate(), Relays::Disabled).await.unwrap();
                let connection = || Connection { ticket: host.ticket().to_string(), identity: identity.to_bytes().to_vec(), invitation: None, use_relays: false, device_name: "Fixture device".into() };
                let cached = Snapshot {
 panes: crate::state::PaneStates::with_primary(std::sync::Arc::new(crate::state::PaneState { navigation: Arc::new(crate::state::Navigation { thread_id: selected.then(|| agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }), draft_key: agent_protocol::session::SessionRef {provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into()}.into(), ..Default::default() }), ..Default::default() })),
                    ..Default::default()
                };
                let store = AgentStore::offline(crate::persistence::encode(&cached).unwrap(), None, "test-revision".into()).await.unwrap();
                let (connected, (session, mut reader, writer)) = tokio::join!(store.reconnect(connection()), scoped_incoming(&host, &trust));
                connected.unwrap();
                let (withheld, waiting) = tokio::sync::oneshot::channel();
                let server = async {
                    let mut pending = Vec::new();
                    let mut withheld = Some(withheld);
                    for round in 0..(3 + usize::from(selected)) {
                        let mut requests = Vec::new();
                        while requests.len() < (2 + usize::from(selected) + 2 * usize::from(round == 0)) {
                            let request = reader.read_request().await.unwrap().expect("refresh must retain the existing stream");
                            requests.push(request);
                        }
                        for request in requests {
                            let result = match request["method"].as_str().unwrap() {
                                "host/session/list" => json!({"data":[{"id":{"provider":"codex","id":"thread"},"name":format!("round {round}")}],"projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}),
                                "host/session/open" => json!({"session":{"provider":"codex","id":"thread"},"subscriptionId":uuid::Uuid::new_v4(),"revision":0,"response":{"thread":{"id":{"provider":"codex","id":"thread"},"turns":[]}}}),
                                "host/account/list" if round == 0 => json!({"accounts":[],"selected":{}}),
                                "host/model/list" if round == 0 => json!({"data":[],"nextCursor":null}),
                                "host/taskActivity/read" => json!({"revision":0,"statuses":[],"waiting":[],"unread":[],"display":agent_protocol::live_activity::TaskActivitySummary::default().display()}),
                                method => panic!("unexpected refresh request {method}"),
                            };
                            if round == 2 && selected && request["method"] == "host/session/open" { pending.push(request); continue; }
                            let response = if round == 1 && request["method"] == "host/session/list" {
                                json!({"error":{"code":"request_failed","message":"temporary read error"}})
                            } else { json!({"result":result}) };
                            writer.reply(&request, response).await.unwrap();
                        }
                        if round == 2 && selected { withheld.take().unwrap().send(()).unwrap(); }
                    }
                    assert!(reader.read_request().await.unwrap().is_none());
                };
                let client = async {
                    let mut updates = store.store.subscribe();
                    loop {
                        let ready = { let snapshot = updates.borrow_and_update(); snapshot.catalog.account.accounts.is_some() && snapshot.lists.threads.is_some() && (!selected || snapshot.sessions.conversations.contains_key(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() })) };
                        if ready { break; }
                        updates.changed().await.unwrap();
                    }
                    store.dispatch(Intent::SetDraftText { thread_id: agent_protocol::session::SessionRef {provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into()}.into(), text: "preserved".into() }).unwrap().wait().await.unwrap();
                    let refresh = || async {
                        use crate::state::operations as op;
                        let list = store.store.dispatch(Intent::ListSessions(op::ListSessions::new(
                            (*store.snapshot().lists.list_query).clone(),
                        )));
                        let history = async {
                            if selected {
                                store.store.dispatch(Intent::ReadThread(op::ReadThread::new(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }))).await?;
                            }
                            Ok(Outcome::Applied)
                        };
                        let (list, history) = tokio::join!(list, history);
                        list.and(history)
                    };
                    let before = store.snapshot();
                    assert!(refresh().await.is_err());
                    assert!(store.snapshot().connected());
                    assert!(store.snapshot().panes.primary().error.is_none());
                    assert!(store.snapshot().operations.values().any(|state| matches!(&state.phase,
                        crate::state::operations::OperationPhase::Failed { message } if message.contains("temporary read error"))));
                    if selected {
                        let refresh = refresh();
                        tokio::pin!(refresh);
                        tokio::select! {
                            result = &mut refresh => panic!("withheld history must remain pending: {result:?}"),
                            result = waiting => result.unwrap(),
                        }
                        // The list reply is adopted and only the withheld history read remains.
                        while store.snapshot().lists.threads.as_ref().unwrap().data[0].name.as_deref() != Some("round 2") {
                            updates.changed().await.unwrap();
                        }
                        tokio::time::pause();
                        tokio::time::advance(Duration::from_secs(30)).await;
                        tokio::time::resume();
                        assert!(refresh.await.unwrap_err().to_string().contains("timed out"));
                        assert!(store.snapshot().connected());
                    }
                    refresh().await.unwrap();
                    let after = store.snapshot();
                    assert!(after.connected());
                    assert!(after.panes.primary().error.is_none());
                    assert_eq!(after.connection.epoch, before.connection.epoch);
                    assert_eq!(after.panes.primary().navigation, before.panes.primary().navigation);
                    assert_eq!(after.input.drafts[&crate::state::DraftKey::from(agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() })].text, "preserved");
                    assert_eq!(after.lists.threads.as_ref().unwrap().data[0].name.as_deref(), Some(if selected { "round 3" } else { "round 2" }));
                    store.shutdown().await.unwrap();
                };
                tokio::join!(server, client);
                session.close();
                host.close().await;
            }).await.unwrap();
        }
    }
}

use crate::Runtime;
use agent_core::{connection::Store, state::Snapshot};
use gpui_kit::Context;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tokio::{sync::watch, task::JoinHandle};

#[derive(Clone)]
pub(crate) struct ClientPreferences {
    bytes: Arc<Mutex<Vec<u8>>>,
    pending_selected: Arc<Mutex<PendingSelectedSync>>,
}

struct PendingSelectedSync {
    next_token: u64,
    pending: Option<SelectedSync>,
}

struct SelectedSync {
    owner: usize,
    token: u64,
    minimum_revision: u64,
    bytes: Vec<u8>,
}

impl ClientPreferences {
    pub(crate) fn from_disk() -> Self {
        let bytes = crate::platform::state_dir()
            .ok()
            .map(|directory| {
                std::fs::read(directory.join("model-preferences.json")).unwrap_or_default()
            })
            .unwrap_or_default();
        Self {
            bytes: Arc::new(Mutex::new(bytes)),
            pending_selected: Arc::new(Mutex::new(PendingSelectedSync {
                next_token: 0,
                pending: None,
            })),
        }
    }

    #[cfg(test)]
    fn from_path(path: PathBuf) -> Self {
        Self {
            bytes: Arc::new(Mutex::new(std::fs::read(path).unwrap_or_default())),
            pending_selected: Arc::new(Mutex::new(PendingSelectedSync {
                next_token: 0,
                pending: None,
            })),
        }
    }

    pub(crate) fn current(&self) -> Vec<u8> {
        self.bytes
            .lock()
            .map(|bytes| bytes.clone())
            .unwrap_or_default()
    }

    pub(crate) fn replace(&self, bytes: Vec<u8>) -> bool {
        let Ok(mut current) = self.bytes.lock() else {
            return false;
        };
        if *current == bytes {
            return false;
        }
        *current = bytes;
        true
    }

    fn owner_id(store: &Arc<Store>) -> usize {
        Arc::as_ptr(store) as usize
    }

    pub(crate) fn selected_sync_blocks(&self, owner: &Arc<Store>, bytes: &[u8]) -> bool {
        let owner = Self::owner_id(owner);
        self.pending_selected
            .lock()
            .map(|state| {
                state.pending.as_ref().is_some_and(|pending| {
                    pending.owner == owner && pending.bytes.as_slice() != bytes
                })
            })
            .unwrap_or(false)
    }

    pub(crate) fn observe_selected_sync(
        &self,
        owner: &Arc<Store>,
        revision: u64,
        bytes: &[u8],
    ) -> bool {
        let owner = Self::owner_id(owner);
        let Ok(mut state) = self.pending_selected.lock() else {
            return false;
        };
        if state.pending.as_ref().is_some_and(|pending| {
            pending.owner == owner
                && revision >= pending.minimum_revision
                && pending.bytes.as_slice() == bytes
        }) {
            state.pending = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn mark_selected_sync(&self, owner: &Arc<Store>, bytes: &[u8]) -> u64 {
        let minimum_revision = owner.snapshot().revision.saturating_add(1);
        let owner = Self::owner_id(owner);
        let Ok(mut state) = self.pending_selected.lock() else {
            return 0;
        };
        state.next_token = state.next_token.wrapping_add(1);
        let token = state.next_token;
        state.pending = Some(SelectedSync {
            owner,
            token,
            minimum_revision,
            bytes: bytes.to_vec(),
        });
        token
    }

    pub(crate) fn selected_sync_matches(
        &self,
        owner: &Arc<Store>,
        token: u64,
        bytes: &[u8],
    ) -> bool {
        let owner = Self::owner_id(owner);
        self.pending_selected
            .lock()
            .map(|state| {
                state.pending.as_ref().is_some_and(|pending| {
                    pending.owner == owner
                        && pending.token == token
                        && pending.bytes.as_slice() == bytes
                })
            })
            .unwrap_or(false)
    }

    pub(crate) fn clear_selected_sync(&self) {
        if let Ok(mut state) = self.pending_selected.lock() {
            state.pending = None;
        }
    }

    /// Seeds a newly exposed Host store with the latest client-global values.
    /// A replacement arriving while the receipt is in flight is applied again
    /// before the store is handed to a native owner.
    pub(crate) async fn apply_to(&self, store: &Store) -> anyhow::Result<()> {
        loop {
            let bytes = self.current();
            if bytes.is_empty() {
                return Ok(());
            }
            let payload = bytes.clone();
            store
                .apply_client_preferences(payload)
                .await
                .map_err(|error| anyhow::anyhow!("client preferences receipt dropped: {error}"))?
                .map_err(|error| anyhow::anyhow!("client preferences rejected: {error}"))?;
            if self.current() == bytes {
                return Ok(());
            }
        }
    }

    async fn write_current(&self, path: &Path, bytes: &[u8]) -> anyhow::Result<bool> {
        let shared = self.bytes.clone();
        let path = path.to_path_buf();
        let bytes = bytes.to_vec();
        tokio::task::spawn_blocking(move || {
            let current = shared
                .lock()
                .map_err(|_| anyhow::anyhow!("client preferences lock was poisoned"))?;
            if *current != bytes {
                return Ok(false);
            }
            host_daemon::platform::save_private_bytes(&path, &bytes)?;
            Ok(true)
        })
        .await
        .map_err(anyhow::Error::from)?
    }
}

/// Owns a view's connection, snapshot subscription and final persistence flush.
/// Dropping a replaced or undelivered session also closes its Store.
pub(crate) struct StoreSession {
    pub(crate) store: Arc<Store>,
    runtime: Runtime,
    closed: bool,
    persistence: Option<watch::Sender<Arc<Snapshot>>>,
    persistence_task: Option<JoinHandle<()>>,
}
impl StoreSession {
    pub(crate) fn on_app_quit<V: 'static>(
        cx: &mut Context<V>,
        session: fn(&mut V) -> &mut Option<Self>,
    ) {
        cx.on_app_quit(move |view, _| {
            if let Some(mut session) = session(view).take()
                && let Some(close) = session.close()
            {
                // GPUI's asynchronous quit deadline is only 200 ms. Complete
                // the tracked Store close and final disk flush synchronously.
                let _ = session.runtime.handle.block_on(close);
            }
            async {}
        })
        .detach();
    }

    pub(crate) async fn publish<E: Send>(
        result: anyhow::Result<Arc<Store>>,
        runtime: Runtime,
        updates: async_channel::Sender<E>,
        connected: impl FnOnce(Result<Self, String>) -> E,
        snapshot: impl Fn(Arc<Snapshot>) -> E,
        client_preferences: Option<ClientPreferences>,
    ) {
        let store = match result {
            Ok(store) => store,
            Err(error) => {
                let _ = updates.send(connected(Err(format!("{error:#}")))).await;
                return;
            }
        };
        if let Some(client_preferences) = client_preferences
            && let Err(error) = client_preferences.apply_to(&store).await
        {
            let _ = updates.send(connected(Err(format!("{error:#}")))).await;
            return;
        }
        let mut snapshots = store.subscribe();
        let session = Self {
            store,
            runtime,
            closed: false,
            persistence: None,
            persistence_task: None,
        };
        if updates.send(connected(Ok(session))).await.is_err() {
            return;
        }
        loop {
            let current = snapshots.borrow_and_update().clone();
            if updates.send(snapshot(current)).await.is_err() || snapshots.changed().await.is_err()
            {
                break;
            }
        }
    }

    /// Keeps the selected client's shared preferences saved at `path`; the
    /// Store writes its own Host-scoped device state.
    pub(crate) fn persist<E: Send + 'static>(
        &mut self,
        path: PathBuf,
        client_preferences: ClientPreferences,
        updates: async_channel::Sender<E>,
        failure: impl Fn(String) -> E + Send + 'static,
    ) {
        let (send, mut receive) = watch::channel(self.store.snapshot());
        let mut saved = std::fs::read(&path).unwrap_or_default();
        self.persistence = Some(send);
        self.persistence_task = Some(self.runtime.closing.spawn_on(
            async move {
                let current = client_preferences.current();
                if !current.is_empty() && current != saved {
                    let result = client_preferences.write_current(&path, &current).await;
                    match result {
                        Ok(true) => saved = current,
                        Ok(false) => {}
                        Err(error) => {
                            let _ = updates.send(failure(format!("{error:#}"))).await;
                        }
                    }
                }
                while receive.changed().await.is_ok() {
                    let snapshot = receive.borrow_and_update().clone();
                    let preferences = agent_core::persistence::encode_model_preferences(&snapshot)
                        .unwrap_or_default();
                    if preferences == saved || preferences != client_preferences.current() {
                        continue;
                    }
                    let result = client_preferences.write_current(&path, &preferences).await;
                    match result {
                        Ok(true) => saved = preferences,
                        Ok(false) => {}
                        Err(error) => {
                            let _ = updates.send(failure(format!("{error:#}"))).await;
                        }
                    }
                }
            },
            &self.runtime.handle,
        ));
    }

    pub(crate) fn save(&self, snapshot: Arc<Snapshot>) {
        if let Some(persistence) = &self.persistence {
            persistence.send_replace(snapshot);
        }
    }

    fn close(&mut self) -> Option<JoinHandle<()>> {
        if std::mem::replace(&mut self.closed, true) {
            return None;
        }
        let store = self.store.clone();
        let persistence = self.persistence.take();
        let persistence_task = self.persistence_task.take();
        Some(self.runtime.closing.spawn_on(
            async move {
                let _ = store.close().await;
                if let Some(persistence) = persistence {
                    persistence.send_replace(store.snapshot());
                }
                if let Some(task) = persistence_task {
                    let _ = task.await;
                }
            },
            &self.runtime.handle,
        ))
    }
}
impl Drop for StoreSession {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::state::Intent;
    use std::time::Duration;

    enum Update {
        Connected(Result<StoreSession, String>),
        Snapshot,
        Error,
    }
    fn runtime() -> Runtime {
        Runtime {
            handle: tokio::runtime::Handle::current(),
            connections: Arc::default(),
            closing: tokio_util::task::TaskTracker::new(),
            logging_error: None,
        }
    }

    #[tokio::test]
    async fn selected_preference_sync_blocks_a_stale_snapshot_until_receipt_completes() {
        let preferences = ClientPreferences {
            bytes: Arc::new(Mutex::new(Vec::new())),
            pending_selected: Arc::new(Mutex::new(PendingSelectedSync {
                next_token: 0,
                pending: None,
            })),
        };
        let current = b"current";
        let stale = b"stale";
        let store = Arc::new(Store::offline(Snapshot::default(), Default::default()));
        let token = preferences.mark_selected_sync(&store, current);
        assert!(preferences.selected_sync_blocks(&store, stale));
        assert!(!preferences.selected_sync_blocks(&store, current));
        assert!(preferences.selected_sync_matches(&store, token, current));
        assert!(!preferences.observe_selected_sync(&store, 0, current));
        let receipt_revision = store.snapshot().revision.saturating_add(1);
        assert!(!preferences.observe_selected_sync(&store, receipt_revision - 1, current));
        assert!(preferences.observe_selected_sync(&store, receipt_revision, current));
        assert!(!preferences.selected_sync_blocks(&store, stale));
        let replacement = Arc::new(Store::offline(Snapshot::default(), Default::default()));
        let replacement_token = preferences.mark_selected_sync(&replacement, current);
        assert!(!preferences.selected_sync_matches(&store, replacement_token, current));
        assert!(!preferences.observe_selected_sync(&replacement, 0, current));
        let replacement_receipt_revision = replacement.snapshot().revision.saturating_add(1);
        assert!(!preferences.observe_selected_sync(
            &replacement,
            replacement_receipt_revision - 1,
            current,
        ));
        assert!(preferences.observe_selected_sync(
            &replacement,
            replacement_receipt_revision,
            current,
        ));
        store.close().await.unwrap();
        replacement.close().await.unwrap();
    }

    #[tokio::test]
    async fn closing_writes_the_latest_draft_and_model_preferences_and_closes_the_store() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let directory = tempfile::tempdir().unwrap();
            let state_file = directory.path().join("device.json");
            let path = directory.path().join("model-preferences.json");
            let runtime = runtime();
            let store = Arc::new(Store::offline(
                Snapshot::default(),
                agent_core::connection::StoreOptions {
                    state_file: Some(state_file.clone()),
                    ..Default::default()
                },
            ));
            let (updates, incoming) = async_channel::unbounded();
            let publish = tokio::spawn(StoreSession::publish(
                Ok(store.clone()),
                runtime.clone(),
                updates.clone(),
                Update::Connected,
                |_| Update::Snapshot,
                None,
            ));
            let Update::Connected(Ok(mut session)) = incoming.recv().await.unwrap() else {
                panic!("missing session")
            };
            let client_preferences = ClientPreferences::from_path(path.clone());
            session.persist(path.clone(), client_preferences.clone(), updates, |_| {
                Update::Error
            });
            store
                .dispatch(Intent::SetRuntimeMode {
                    mode: agent_domain::RuntimeMode::Auto,
                })
                .await
                .unwrap()
                .unwrap();
            store
                .dispatch(Intent::EditDraft {
                    text: "last edit before close".into(),
                    base_text: None,
                })
                .await
                .unwrap()
                .unwrap();
            client_preferences.replace(
                agent_core::persistence::encode_model_preferences(&store.snapshot()).unwrap(),
            );
            // No UI snapshot/save notification is needed for the final flush.
            drop(session);
            runtime.closing.close();
            runtime.closing.wait().await;
            publish.await.unwrap();
            let preferences = std::fs::read(&path).unwrap();
            let restored = agent_core::persistence::load(&state_file, &preferences);
            assert_eq!(restored.current_draft().text, "last edit before close");
            assert!(store.dispatch(Intent::LeaveThread).await.unwrap().is_err());
            let other =
                agent_core::persistence::load(&directory.path().join("other.json"), &preferences);
            assert_eq!(other.default_draft, restored.default_draft);
            assert!(other.drafts.is_empty());
        })
        .await
        .expect("session close stalled");
    }

    #[tokio::test]
    async fn a_model_preferences_failure_is_reported_and_a_later_save_recovers() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("model-preferences.json");
            std::fs::create_dir(&path).unwrap();
            let client_preferences = ClientPreferences {
                bytes: Arc::new(Mutex::new(
                    agent_core::persistence::encode_model_preferences(&Snapshot::default())
                        .unwrap(),
                )),
                pending_selected: Arc::new(Mutex::new(PendingSelectedSync {
                    next_token: 0,
                    pending: None,
                })),
            };
            let runtime = runtime();
            let store = Arc::new(Store::offline(Snapshot::default(), Default::default()));
            let (updates, incoming) = async_channel::unbounded();
            let publish = tokio::spawn(StoreSession::publish(
                Ok(store.clone()),
                runtime.clone(),
                updates.clone(),
                Update::Connected,
                |_| Update::Snapshot,
                None,
            ));
            let Update::Connected(Ok(mut session)) = incoming.recv().await.unwrap() else {
                panic!("missing session")
            };
            session.persist(path.clone(), client_preferences.clone(), updates, |_| {
                Update::Error
            });
            store
                .dispatch(Intent::SetRuntimeMode {
                    mode: agent_domain::RuntimeMode::Auto,
                })
                .await
                .unwrap()
                .unwrap();
            session.save(store.snapshot());
            while !matches!(incoming.recv().await.unwrap(), Update::Error) {}
            std::fs::remove_dir(&path).unwrap();
            store
                .dispatch(Intent::SetRuntimeMode {
                    mode: agent_domain::RuntimeMode::FullAccess,
                })
                .await
                .unwrap()
                .unwrap();
            client_preferences.replace(
                agent_core::persistence::encode_model_preferences(&store.snapshot()).unwrap(),
            );
            session.save(store.snapshot());
            drop(session);
            runtime.closing.close();
            runtime.closing.wait().await;
            publish.await.unwrap();
            let restored = agent_core::persistence::load(
                &directory.path().join("device.json"),
                &std::fs::read(&path).unwrap(),
            );
            assert_eq!(
                restored.default_draft.runtime_mode,
                agent_domain::RuntimeMode::FullAccess
            );
        })
        .await
        .expect("persistence recovery stalled");
    }

    #[tokio::test]
    async fn a_replaced_session_cannot_overwrite_newer_client_preferences() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let directory = tempfile::tempdir().unwrap();
            let state_file = directory.path().join("device.json");
            let path = directory.path().join("model-preferences.json");
            let mut old = Snapshot::default();
            old.default_draft.model = "old-model".into();
            let old_bytes = agent_core::persistence::encode_model_preferences(&old).unwrap();
            std::fs::write(&path, &old_bytes).unwrap();
            let client_preferences = ClientPreferences::from_path(path.clone());
            let mut current = old.clone();
            current.default_draft.model = "new-model".into();
            let current_bytes =
                agent_core::persistence::encode_model_preferences(&current).unwrap();
            assert!(client_preferences.replace(current_bytes));

            let runtime = runtime();
            let store = Arc::new(Store::offline(
                old,
                agent_core::connection::StoreOptions {
                    state_file: Some(state_file.clone()),
                    ..Default::default()
                },
            ));
            let (updates, incoming) = async_channel::unbounded();
            let publish = tokio::spawn(StoreSession::publish(
                Ok(store.clone()),
                runtime.clone(),
                updates.clone(),
                Update::Connected,
                |_| Update::Snapshot,
                None,
            ));
            let Update::Connected(Ok(mut session)) = incoming.recv().await.unwrap() else {
                panic!("missing session")
            };
            session.persist(path.clone(), client_preferences, updates, |_| Update::Error);
            session.save(store.snapshot());
            drop(session);
            runtime.closing.close();
            runtime.closing.wait().await;
            publish.await.unwrap();

            let persisted = std::fs::read(&path).unwrap();
            let restored = agent_core::persistence::load(&state_file, &persisted);
            assert_eq!(restored.default_draft.model, "new-model");
        })
        .await
        .expect("session close stalled");
    }

    #[tokio::test]
    async fn a_new_session_exposes_the_current_client_preferences_before_connected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("model-preferences.json");
        let mut current = Snapshot::default();
        current.default_draft.model = "current-model".into();
        let client_preferences = ClientPreferences::from_path(path);
        client_preferences
            .replace(agent_core::persistence::encode_model_preferences(&current).unwrap());
        let runtime = runtime();
        let store = Arc::new(Store::offline(Snapshot::default(), Default::default()));
        let (updates, incoming) = async_channel::unbounded();
        let publish = tokio::spawn(StoreSession::publish(
            Ok(store.clone()),
            runtime.clone(),
            updates,
            Update::Connected,
            |_| Update::Snapshot,
            Some(client_preferences),
        ));
        let Update::Connected(Ok(session)) = incoming.recv().await.unwrap() else {
            panic!("missing session")
        };
        assert_eq!(
            session.store.snapshot().default_draft.model,
            "current-model"
        );
        drop(session);
        runtime.closing.close();
        runtime.closing.wait().await;
        publish.await.unwrap();
    }

    #[tokio::test]
    async fn failed_connection_is_delivered_and_undelivered_session_closes_its_store() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let runtime = runtime();
            let store = Arc::new(Store::offline(Snapshot::default(), Default::default()));
            let (updates, incoming) = async_channel::unbounded();
            StoreSession::publish(
                Err(anyhow::anyhow!("connection failed").context("cannot open session")), runtime.clone(), updates.clone(),
                Update::Connected, |_| Update::Snapshot, None,
            ).await;
            assert!(matches!(incoming.recv().await.unwrap(), Update::Connected(Err(error)) if error == "cannot open session: connection failed"));
            assert!(incoming.try_recv().is_err());
            drop(incoming);
            StoreSession::publish(
                Ok(store.clone()),
                runtime.clone(),
                updates,
                Update::Connected,
                |_| Update::Snapshot,
                None,
            )
            .await;
            runtime.closing.close();
            runtime.closing.wait().await;
            assert!(store.dispatch(Intent::LeaveThread).await.unwrap().is_err());
        })
        .await
        .expect("undelivered session remained open");
    }
}

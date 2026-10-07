use crate::Runtime;
use agent_core::{connection::Store, state::Snapshot};
use gpui_kit::Context;
use std::{path::PathBuf, sync::Arc};
use tokio::{sync::watch, task::JoinHandle};

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
    ) {
        let store = match result {
            Ok(store) => store,
            Err(error) => {
                let _ = updates.send(connected(Err(format!("{error:#}")))).await;
                return;
            }
        };
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

    /// Keeps the model preferences every Host shares saved at `path`; the
    /// Store writes its own device state.
    pub(crate) fn persist<E: Send + 'static>(
        &mut self,
        path: PathBuf,
        updates: async_channel::Sender<E>,
        failure: impl Fn(String) -> E + Send + 'static,
    ) {
        let (send, mut receive) = watch::channel(self.store.snapshot());
        let mut saved = std::fs::read(&path).unwrap_or_default();
        self.persistence = Some(send);
        self.persistence_task = Some(self.runtime.closing.spawn_on(
            async move {
                while receive.changed().await.is_ok() {
                    let snapshot = receive.borrow_and_update().clone();
                    let preferences =
                        agent_core::persistence::encode_model_preferences(&snapshot)
                            .unwrap_or_default();
                    if preferences == saved {
                        continue;
                    }
                    let (path, bytes) = (path.clone(), preferences.clone());
                    let result = tokio::task::spawn_blocking(move || {
                        host_daemon::platform::save_private_bytes(&path, &bytes)
                    })
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result);
                    match result {
                        Ok(()) => saved = preferences,
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
            ));
            let Update::Connected(Ok(mut session)) = incoming.recv().await.unwrap() else {
                panic!("missing session")
            };
            session.persist(path.clone(), updates, |_| Update::Error);
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
            // No UI snapshot/save notification is needed for the final flush.
            drop(session);
            runtime.closing.close();
            runtime.closing.wait().await;
            publish.await.unwrap();
            let preferences = std::fs::read(&path).unwrap();
            let restored = agent_core::persistence::load(&state_file, &preferences);
            assert_eq!(restored.current_draft().text, "last edit before close");
            assert!(store.dispatch(Intent::LeaveThread).await.unwrap().is_err());
            let other = agent_core::persistence::load(&directory.path().join("other.json"), &preferences);
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
            let runtime = runtime();
            let store = Arc::new(Store::offline(Snapshot::default(), Default::default()));
            let (updates, incoming) = async_channel::unbounded();
            let publish = tokio::spawn(StoreSession::publish(
                Ok(store.clone()),
                runtime.clone(),
                updates.clone(),
                Update::Connected,
                |_| Update::Snapshot,
            ));
            let Update::Connected(Ok(mut session)) = incoming.recv().await.unwrap() else {
                panic!("missing session")
            };
            session.persist(path.clone(), updates, |_| Update::Error);
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
    async fn failed_connection_is_delivered_and_undelivered_session_closes_its_store() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let runtime = runtime();
            let store = Arc::new(Store::offline(Snapshot::default(), Default::default()));
            let (updates, incoming) = async_channel::unbounded();
            StoreSession::publish(
                Err(anyhow::anyhow!("connection failed").context("cannot open session")), runtime.clone(), updates.clone(),
                Update::Connected, |_| Update::Snapshot,
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

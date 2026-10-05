use crate::Runtime;
use agent_core::{state::Snapshot, store::Store};
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

    pub(crate) fn persist<E: Send + 'static>(
        &mut self,
        path: PathBuf,
        updates: async_channel::Sender<E>,
        failure: impl Fn(String) -> E + Send + 'static,
    ) {
        let (send, mut receive) = watch::channel(self.store.snapshot());
        let mut preferences =
            agent_core::persistence::encode_model_preferences(&self.store.snapshot())
                .unwrap_or_default();
        self.persistence = Some(send);
        self.persistence_task = Some(self.runtime.closing.spawn_on(
            async move {
                while receive.changed().await.is_ok() {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                    let snapshot = receive.borrow_and_update().clone();
                    let path = path.clone();
                    let next_preferences =
                        agent_core::persistence::encode_model_preferences(&snapshot)
                            .unwrap_or_default();
                    let preferences_changed = preferences != next_preferences;
                    let saved_preferences = next_preferences.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        if preferences_changed {
                            host_daemon::platform::save_private_bytes(
                                &path.with_file_name("orchestration-model-preferences.json"),
                                &saved_preferences,
                            )?;
                        }
                        host_daemon::platform::save_private_bytes(
                            &path,
                            &agent_core::persistence::encode(&snapshot)?,
                        )
                    })
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result);
                    if result.is_ok() {
                        preferences = next_preferences;
                    }
                    if let Err(error) = result {
                        let _ = updates.send(failure(format!("{error:#}"))).await;
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

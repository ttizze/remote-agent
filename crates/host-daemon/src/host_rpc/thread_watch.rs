use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::json;

use super::routing::{SessionId, SessionRouter};

/// One visible persisted conversation per authenticated connection. Native
/// turn/item events cover work owned by our Codex process; another process's
/// rollout must be observed without trying to take its active writer lock.
#[derive(Clone, Default)]
pub(super) struct ThreadWatches {
    slots: Arc<Mutex<HashMap<SessionId, Slot>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::{fs, io::Write, time::Duration};

    fn append(path: &std::path::Path) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "persisted reply").unwrap();
        file.sync_all().unwrap();
    }

    async fn changed(session: &mut super::super::routing::HostSession, revision: u64) -> Value {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let value: Value = serde_json::from_str(&session.recv().await.unwrap()).unwrap();
                if value["params"]["watchId"] == revision {
                    return value;
                }
            }
        })
        .await
        .expect("a changed rollout must notify its visible conversation")
    }

    #[tokio::test]
    async fn an_external_append_notifies_only_the_subscribed_connection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        fs::write(&path, "initial\n").unwrap();
        let router = SessionRouter::new();
        let mut owner = router.open_session(16);
        let mut other = router.open_session(16);
        let watches = ThreadWatches::default();
        watches
            .request(
                owner.id(),
                router.clone(),
                WatchRequest::Watch(WatchParams {
                    watch_id: 1,
                    thread_id: "open".into(),
                    path: path.clone(),
                }),
            )
            .await
            .unwrap();
        append(&path);
        let event = changed(&mut owner, 1).await;
        assert_eq!(event["method"], "host/thread/changed");
        assert_eq!(event["params"]["threadId"], "open");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), other.recv())
                .await
                .is_err()
        );
        watches.clear_session(owner.id());
    }

    #[tokio::test]
    async fn stale_registration_and_cancellation_cannot_replace_the_newer_watch() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.jsonl");
        let second = directory.path().join("second.jsonl");
        fs::write(&first, "first\n").unwrap();
        fs::write(&second, "second\n").unwrap();
        let router = SessionRouter::new();
        let mut session = router.open_session(16);
        let watches = ThreadWatches::default();
        let register = |watch_id, path: &PathBuf| {
            WatchRequest::Watch(WatchParams {
                watch_id,
                thread_id: "open".into(),
                path: path.clone(),
            })
        };
        watches
            .request(session.id(), router.clone(), register(2, &second))
            .await
            .unwrap();
        watches
            .request(
                session.id(),
                router.clone(),
                WatchRequest::Unwatch(UnwatchParams { watch_id: 1 }),
            )
            .await
            .unwrap();
        watches
            .request(session.id(), router.clone(), register(1, &first))
            .await
            .unwrap();
        append(&second);
        assert_eq!(
            changed(&mut session, 2).await["method"],
            "host/thread/changed"
        );
        watches
            .request(
                session.id(),
                router.clone(),
                WatchRequest::Unwatch(UnwatchParams { watch_id: 2 }),
            )
            .await
            .unwrap();
        watches
            .request(session.id(), router.clone(), register(2, &second))
            .await
            .unwrap();
        while matches!(
            tokio::time::timeout(Duration::from_millis(300), session.recv()).await,
            Ok(Some(_))
        ) {}
        append(&second);
        assert!(
            tokio::time::timeout(Duration::from_secs(2), session.recv())
                .await
                .is_err(),
            "a cancelled revision cannot be resurrected by a delayed registration"
        );
    }
}

#[derive(Default)]
struct Slot {
    revision: u64,
    enabled: bool,
    watcher: Option<RecommendedWatcher>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WatchParams {
    watch_id: u64,
    thread_id: String,
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UnwatchParams {
    watch_id: u64,
}

#[derive(Deserialize)]
#[serde(tag = "method", content = "params")]
pub(super) enum WatchRequest {
    #[serde(rename = "host/thread/watch")]
    Watch(WatchParams),
    #[serde(rename = "host/thread/unwatch")]
    Unwatch(UnwatchParams),
}

impl ThreadWatches {
    pub(super) async fn request(
        &self,
        session: SessionId,
        router: SessionRouter,
        request: WatchRequest,
    ) -> Result<agent_core::models::Empty, String> {
        let watches = self.clone();
        tokio::task::spawn_blocking(move || {
            router.ensure_session(session)?;
            match request {
                WatchRequest::Unwatch(params) => watches.unwatch(session, params.watch_id),
                WatchRequest::Watch(params) => watches.watch(session, router, params)?,
            }
            Ok(agent_core::models::Empty {})
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn watch(
        &self,
        session: SessionId,
        router: SessionRouter,
        params: WatchParams,
    ) -> Result<(), String> {
        if params.watch_id == 0 || params.thread_id.is_empty() || !params.path.is_absolute() {
            return Err("watchId, threadId and an absolute rollout path are required".into());
        }
        let previous = {
            let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
            let slot = slots.entry(session).or_default();
            if params.watch_id <= slot.revision {
                return Ok(());
            }
            slot.revision = params.watch_id;
            slot.enabled = true;
            slot.watcher.take()
        };
        drop(previous);

        // Watch the parent so atomic replacement of the rollout keeps working.
        let parent = params
            .path
            .parent()
            .ok_or("rollout parent is missing")?
            .canonicalize()
            .map_err(|error| error.to_string())?;
        let path = parent.join(
            params
                .path
                .file_name()
                .ok_or("rollout filename is missing")?,
        );
        let event_router = router.clone();
        let watch_id = params.watch_id;
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let message = match event {
                Ok(event) if !matches!(event.kind, EventKind::Access(_)) &&
                    (event.need_rescan() || event.paths.iter().any(|changed| changed == &path)) =>
                    json!({"method":"host/thread/changed","params":{"watchId":watch_id,"threadId":params.thread_id}}),
                Err(_) => json!({"method":"host/thread/watchFailed","params":{"watchId":watch_id,"threadId":params.thread_id}}),
                _ => return,
            };
            let _ = event_router.send_line(session, message.to_string());
        }).map_err(|error| error.to_string())?;
        watcher
            .watch(&parent, RecursiveMode::NonRecursive)
            .map_err(|error| error.to_string())?;

        if router.ensure_session(session).is_err() {
            self.clear_session(session);
            return Err("conversation connection closed".into());
        }
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(slot) = slots
            .get_mut(&session)
            .filter(|slot| slot.revision == watch_id && slot.enabled)
        {
            slot.watcher = Some(watcher);
        }
        Ok(())
    }

    fn unwatch(&self, session: SessionId, revision: u64) {
        let previous = {
            let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
            let slot = slots.entry(session).or_default();
            if revision < slot.revision {
                return;
            }
            slot.revision = revision;
            slot.enabled = false;
            slot.watcher.take()
        };
        // Keep the revision after cancellation: a slower, older registration
        // must not replace the current watch or resurrect a cancelled one.
        drop(previous);
    }

    pub(super) fn clear_session(&self, session: SessionId) {
        let previous = self
            .slots
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&session);
        drop(previous);
    }
}

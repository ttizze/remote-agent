use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::{Value, json};

use super::routing::{self, SessionId, SessionQueues};

/// Independently watched conversations on each authenticated connection. Native
/// turn/item events cover work owned by our Codex process; another process's
/// rollout must be observed without trying to take its active writer lock.
#[derive(Clone, Default)]
pub(super) struct ThreadWatches {
    slots: Arc<Mutex<HashMap<SessionId, HashMap<u64, Slot>>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write, time::Duration};

    fn append(path: &std::path::Path) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(file, "persisted reply").unwrap();
        file.sync_all().unwrap();
    }

    async fn changed(session: &mut super::super::routing::CodexSession, revision: u64) -> Value {
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
        let router = routing::session_queues();
        let mut owner = routing::open_session(&router, 16);
        let mut other = routing::open_session(&router, 16);
        let watches = ThreadWatches::default();
        watches
            .request(
                owner.id(),
                router.clone(),
                "host/thread/watch".into(),
                json!({"watchKey":1,"watchId":1,"threadId":"open","path":path}),
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
    async fn two_views_on_one_connection_watch_and_cancel_independently() {
        let directory = tempfile::tempdir().unwrap();
        let paths = [
            directory.path().join("main.jsonl"),
            directory.path().join("side.jsonl"),
        ];
        let router = routing::session_queues();
        let mut session = routing::open_session(&router, 16);
        let watches = ThreadWatches::default();
        for (key, path) in paths.iter().enumerate() {
            fs::write(path, "initial\n").unwrap();
            watches.request(session.id(), router.clone(), "host/thread/watch".into(),
                json!({"watchKey":key,"watchId":1,"threadId":format!("thread-{key}"),"path":path}))
                .await.unwrap();
        }
        for path in &paths {
            append(path);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut seen = [false; 2];
            while !seen.into_iter().all(|value| value) {
                let event = changed(&mut session, 1).await;
                let key = event["params"]["watchKey"].as_u64().unwrap() as usize;
                assert_eq!(event["params"]["threadId"], format!("thread-{key}"));
                seen[key] = true;
            }
        })
        .await
        .unwrap();
        watches
            .request(
                session.id(),
                router.clone(),
                "host/thread/unwatch".into(),
                json!({"watchKey":0,"watchId":1}),
            )
            .await
            .unwrap();
        // Drain events already delivered by the OS before cancellation.
        while matches!(
            tokio::time::timeout(Duration::from_millis(300), session.recv()).await,
            Ok(Some(_))
        ) {}
        for path in &paths {
            append(path);
        }
        let event = changed(&mut session, 1).await;
        assert_eq!(event["params"]["watchKey"], 1);
        assert_eq!(event["params"]["threadId"], "thread-1");
        // A reopened Main reuses its key with a newer revision. A late close
        // from the old view must not stop either currently visible view.
        watches
            .request(
                session.id(),
                router.clone(),
                "host/thread/watch".into(),
                json!({"watchKey":0,"watchId":2,"threadId":"reopened","path":paths[0]}),
            )
            .await
            .unwrap();
        watches
            .request(
                session.id(),
                router.clone(),
                "host/thread/unwatch".into(),
                json!({"watchKey":0,"watchId":1}),
            )
            .await
            .unwrap();
        append(&paths[0]);
        assert_eq!(
            changed(&mut session, 2).await["params"]["threadId"],
            "reopened"
        );
        watches.clear_session(session.id());
        while matches!(
            tokio::time::timeout(Duration::from_millis(300), session.recv()).await,
            Ok(Some(_))
        ) {}
        for path in &paths {
            append(path);
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(2), session.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stale_registration_and_cancellation_cannot_replace_the_newer_watch() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.jsonl");
        let second = directory.path().join("second.jsonl");
        fs::write(&first, "first\n").unwrap();
        fs::write(&second, "second\n").unwrap();
        let router = routing::session_queues();
        let mut session = routing::open_session(&router, 16);
        let watches = ThreadWatches::default();
        let register =
            |revision, path| json!({"watchKey":1,"watchId":revision,"threadId":"open","path":path});
        watches
            .request(
                session.id(),
                router.clone(),
                "host/thread/watch".into(),
                register(2, &second),
            )
            .await
            .unwrap();
        watches
            .request(
                session.id(),
                router.clone(),
                "host/thread/unwatch".into(),
                json!({"watchKey":1,"watchId":1}),
            )
            .await
            .unwrap();
        watches
            .request(
                session.id(),
                router.clone(),
                "host/thread/watch".into(),
                register(1, &first),
            )
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
                "host/thread/unwatch".into(),
                json!({"watchKey":1,"watchId":2}),
            )
            .await
            .unwrap();
        watches
            .request(
                session.id(),
                router.clone(),
                "host/thread/watch".into(),
                register(2, &second),
            )
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
struct WatchParams {
    watch_key: u64,
    watch_id: u64,
    thread_id: String,
    path: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UnwatchParams {
    watch_key: u64,
    watch_id: u64,
}

impl ThreadWatches {
    pub(super) async fn request(
        &self,
        session: SessionId,
        router: Arc<Mutex<SessionQueues>>,
        method: String,
        params: Value,
    ) -> Result<Value, String> {
        let watches = self.clone();
        tokio::task::spawn_blocking(move || {
            routing::ensure_session(&router, session).map_err(|error| error.to_string())?;
            if method == "host/thread/unwatch" {
                let params: UnwatchParams =
                    serde_json::from_value(params).map_err(|error| error.to_string())?;
                watches.unwatch(session, params.watch_key, params.watch_id);
                return Ok(json!({}));
            }
            let params: WatchParams =
                serde_json::from_value(params).map_err(|error| error.to_string())?;
            watches.watch(session, router, params)?;
            Ok(json!({}))
        })
        .await
        .map_err(|error| error.to_string())?
    }

    fn watch(
        &self,
        session: SessionId,
        router: Arc<Mutex<SessionQueues>>,
        params: WatchParams,
    ) -> Result<(), String> {
        if params.watch_id == 0 || params.thread_id.is_empty() || !params.path.is_absolute() {
            return Err("watchId, threadId and an absolute rollout path are required".into());
        }
        let previous = {
            let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
            let slot = slots
                .entry(session)
                .or_default()
                .entry(params.watch_key)
                .or_default();
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
        let watch_key = params.watch_key;
        let watch_id = params.watch_id;
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let message = match event {
                Ok(event) if !matches!(event.kind, EventKind::Access(_)) &&
                    (event.need_rescan() || event.paths.iter().any(|changed| changed == &path)) =>
                    json!({"method":"host/thread/changed","params":{"watchKey":watch_key,"watchId":watch_id,"threadId":params.thread_id}}),
                Err(_) => json!({"method":"host/thread/watchFailed","params":{"watchKey":watch_key,"watchId":watch_id,"threadId":params.thread_id}}),
                _ => return,
            };
            let _ = routing::send_line(&event_router, session, message.to_string());
        }).map_err(|error| error.to_string())?;
        watcher
            .watch(&parent, RecursiveMode::NonRecursive)
            .map_err(|error| error.to_string())?;

        if routing::ensure_session(&router, session).is_err() {
            self.clear_session(session);
            return Err("conversation connection closed".into());
        }
        let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(slot) = slots
            .get_mut(&session)
            .and_then(|slots| slots.get_mut(&watch_key))
            .filter(|slot| slot.revision == watch_id && slot.enabled)
        {
            slot.watcher = Some(watcher);
        }
        Ok(())
    }

    fn unwatch(&self, session: SessionId, key: u64, revision: u64) {
        let previous = {
            let mut slots = self.slots.lock().unwrap_or_else(|error| error.into_inner());
            let slot = slots.entry(session).or_default().entry(key).or_default();
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

    pub(super) fn clear_all(&self) {
        let previous =
            std::mem::take(&mut *self.slots.lock().unwrap_or_else(|error| error.into_inner()));
        drop(previous);
    }
}

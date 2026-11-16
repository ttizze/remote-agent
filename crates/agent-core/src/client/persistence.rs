//! Shared restoration and ordered persistence. Platform adapters only read and
//! atomically replace opaque payloads; device preferences have one shared owner.
use super::{AgentError, error};
use crate::state::{Same, Snapshot};
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

mod device;
pub use device::{DevicePreferences, PreferenceKey, PreferenceStorage};
pub(crate) use device::{ModelChoice, Preferences};

/// A Host/window state file. Device settings use their own storage adapter.
#[cfg_attr(feature = "bindings", uniffi::export(rust, foreign))]
pub trait ClientStorage: Send + Sync {
    fn load(&self) -> Result<Vec<u8>, AgentError>;
    fn save(&self, bytes: Vec<u8>) -> Result<(), AgentError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("{0}")]
    InvalidState(serde_json::Error),
    #[error(transparent)]
    Storage(#[from] AgentError),
}

// A repeated failure is a new failed attempt. Successful writes only notify
// when they clear an error; idle flushes must not republish an empty state.
fn publish_failure(failures: &watch::Sender<Option<String>>, reason: Option<String>) {
    failures.send_if_modified(|current| {
        let changed = current.is_some() || reason.is_some();
        *current = reason;
        changed
    });
}

pub(crate) type Completion<T> = Shared<BoxFuture<'static, Result<T, AgentError>>>;
struct Save {
    snapshot: Arc<Snapshot>,
    complete: oneshot::Sender<Result<(), AgentError>>,
}

pub struct Persistence {
    storage: Arc<dyn ClientStorage>,
    pub(crate) device: Arc<DevicePreferences>,
    conversations: Mutex<Option<PathBuf>>,
    queue: Mutex<Option<mpsc::UnboundedSender<Save>>>,
    /// Asks the conversation copies to write what is pending now.
    copies: Mutex<Option<mpsc::UnboundedSender<crate::conversation_cache::Flush>>>,
    failure: watch::Sender<Option<String>>,
}

/// How far a save reaches.
#[derive(Clone, Copy, PartialEq)]
enum Reach {
    /// The state, as it changes.
    State,
    /// The state and the conversation copies' pending writes, before the app
    /// may stop: an iPhone app leaving the foreground.
    Copies,
    /// The state, and nothing is saved after it: an app quitting, which has
    /// flushed the copies while still connected.
    Last,
}
impl Persistence {
    pub fn new(storage: Arc<dyn ClientStorage>, device: Arc<DevicePreferences>) -> Arc<Self> {
        Arc::new(Self {
            storage,
            device,
            conversations: Mutex::default(),
            queue: Mutex::default(),
            copies: Mutex::default(),
            failure: watch::channel(None).0,
        })
    }
    pub fn failure(&self) -> Option<String> {
        self.failure
            .borrow()
            .clone()
            .or_else(|| self.device.failure())
    }
    #[cfg(feature = "bindings")]
    pub(crate) fn report_failure(&self, reason: String) {
        publish_failure(&self.failure, Some(reason));
    }
    pub fn failures(&self) -> watch::Receiver<Option<String>> {
        self.failure.subscribe()
    }
    pub async fn restore(
        self: &Arc<Self>,
        new_pairing: bool,
        conversations: Option<PathBuf>,
    ) -> Result<Snapshot, RestoreError> {
        let persistence = self.clone();
        tokio::task::spawn_blocking(move || {
            let storage = persistence.storage.as_ref();
            let bytes = if new_pairing {
                Vec::new()
            } else {
                storage.load()?
            };
            let mut snapshot =
                crate::persistence::decode(&bytes).map_err(RestoreError::InvalidState)?;
            let preferences = persistence.device.restore()?;
            snapshot.catalog.model_preferences = preferences.models.clone();
            snapshot.files.preferred_editor = preferences.editor.preferred;
            snapshot.catalog.ui_language = preferences.language;
            snapshot.catalog.web_link_target = preferences.web_link_target;
            if let Some(directory) = &conversations {
                if new_pairing {
                    crate::conversation_cache::clear(directory);
                }
                crate::conversation_cache::restore(directory, &mut snapshot);
            }
            *persistence.conversations.lock().unwrap() = conversations;
            Ok::<_, RestoreError>(snapshot)
        })
        .await
        .map_err(error)?
    }
    /// Prepare the ordered writer and conversation copies without entering a
    /// runtime. The caller must spawn the returned task on its own executor.
    pub fn start(
        &self,
        snapshots: watch::Receiver<Arc<Snapshot>>,
    ) -> Option<impl Future<Output = ()> + Send + use<>> {
        let mut queue = self.queue.lock().unwrap();
        if queue.is_some() {
            return None;
        }
        let (send, mut receive) = mpsc::unbounded_channel::<Save>();
        *queue = Some(send);
        let conversations = self.conversations.lock().unwrap().take();
        let (flush, flushes) = mpsc::unbounded_channel();
        if conversations.is_some() {
            *self.copies.lock().unwrap() = Some(flush);
        }
        let storage = self.storage.clone();
        let device = self.device.clone();
        let failure = self.failure.clone();
        let saving = async move {
            while let Some(Save { snapshot, complete }) = receive.recv().await {
                if let Err(reason) = device.flush().await {
                    let _ = complete.send(Err(reason));
                    continue;
                }
                let storage = storage.clone();
                let result = match tokio::task::spawn_blocking(move || {
                    storage.save(crate::persistence::encode(&snapshot).map_err(error)?)
                })
                .await
                {
                    Ok(result) => result,
                    Err(reason) => {
                        let reason = error(reason);
                        publish_failure(&failure, Some(reason.to_string()));
                        let _ = complete.send(Err(reason));
                        break;
                    }
                };
                publish_failure(&failure, result.as_ref().err().map(ToString::to_string));
                let _ = complete.send(result);
            }
        };
        Some(async move {
            let copies = async move {
                if let Some(directory) = conversations {
                    crate::conversation_cache::run(directory, snapshots, flushes).await;
                }
            };
            tokio::join!(saving, copies);
        })
    }
    pub fn enqueue(&self, snapshot: Arc<Snapshot>) -> Result<Completion<()>, AgentError> {
        self.send(snapshot, Reach::State)
    }
    /// Save the state and write the conversation copies' pending changes
    /// before the app may be stopped, which their quiet period would lose.
    pub fn flush(&self, snapshot: Arc<Snapshot>) -> Result<Completion<()>, AgentError> {
        self.send(snapshot, Reach::Copies)
    }
    /// Seal the queue before publishing the final snapshot. An autosave racing
    /// shutdown can no longer enqueue an older snapshot behind this write. The
    /// copies stop; [`Self::flush_copies`] writes theirs first.
    pub fn finish(&self, snapshot: Arc<Snapshot>) -> Result<Completion<()>, AgentError> {
        self.send(snapshot, Reach::Last)
    }
    /// Write the conversation copies' pending changes now, which their quiet
    /// period would lose if the app stopped. The list copy is written only while
    /// connected, so a quitting app flushes before it disconnects.
    pub fn flush_copies(&self) -> impl Future<Output = ()> + Send + 'static {
        let copies = self.copies.lock().unwrap().clone();
        async move {
            // Copies are best effort: a stopped copier has nothing pending.
            if let Some(copies) = copies {
                let (written, done) = oneshot::channel();
                if copies.send(written).is_ok() {
                    let _ = done.await;
                }
            }
        }
    }
    fn send(&self, snapshot: Arc<Snapshot>, reach: Reach) -> Result<Completion<()>, AgentError> {
        let (complete, result) = oneshot::channel();
        let mut queue = self.queue.lock().unwrap();
        queue
            .as_ref()
            .ok_or_else(|| error("session is not ready"))?
            .send(Save { snapshot, complete })
            .map_err(error)?;
        if reach == Reach::Last {
            queue.take();
        }
        let copies = (reach == Reach::Copies).then(|| self.flush_copies());
        if reach == Reach::Last {
            self.copies.lock().unwrap().take();
        }
        Ok(async move {
            result.await.map_err(error)??;
            if let Some(copies) = copies {
                copies.await;
            }
            Ok(())
        }
        .boxed()
        .shared())
    }
    pub async fn observe(
        self: Arc<Self>,
        mut snapshots: watch::Receiver<Arc<Snapshot>>,
        stop: CancellationToken,
    ) {
        let mut previous = snapshots.borrow_and_update().clone();
        let mut dirty = true;
        let delay = std::time::Duration::from_millis(250);
        let deadline = tokio::time::sleep(delay);
        tokio::pin!(deadline);
        loop {
            let next = tokio::select! {
                biased;
                _ = stop.cancelled() => return,
                result = snapshots.changed() => match result { Ok(()) => Some(snapshots.borrow_and_update().clone()), Err(_) => return },
                _ = &mut deadline, if dirty => None,
            };
            if let Some(next) = next {
                let changed = !crate::persistence::PersistedState::capture(&next)
                    .same(&crate::persistence::PersistedState::capture(&previous));
                let immediate = !next.list_source().equivalent(&previous.list_source())
                    || !next.input.stashes.same(&previous.input.stashes);
                previous = next;
                if !changed {
                    continue;
                }
                dirty = true;
                deadline.as_mut().reset(tokio::time::Instant::now() + delay);
                if !immediate {
                    continue;
                }
            }
            if let Ok(saved) = self.enqueue(previous.clone()) {
                let _ = saved.await;
            }
            dirty = false;
        }
    }
}

#[cfg(test)]
mod tests;

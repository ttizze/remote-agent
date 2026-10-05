use super::{ActorContext, ActorHandle, CommandOrigin, Committed};
use crate::{EffectSettlement, KeyedSerial, RuntimeError};
use agent_domain::{Command, CommandId, EffectResult, State, ThreadId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

pub const IDLE_EVICTION: Duration = Duration::from_secs(5 * 60);

/// Loaded actors by thread. Loading and eviction of one thread are serialized,
/// so at most one actor writes a thread at any time.
pub struct ActorRegistry {
    context: ActorContext,
    actors: Mutex<HashMap<ThreadId, ActorHandle>>,
    keys: KeyedSerial<ThreadId>,
}

impl ActorRegistry {
    pub fn new(context: ActorContext) -> Arc<Self> {
        Arc::new(Self {
            context,
            actors: Mutex::new(HashMap::new()),
            keys: KeyedSerial::default(),
        })
    }

    pub fn context(&self) -> &ActorContext {
        &self.context
    }

    pub fn loaded(&self) -> Vec<ThreadId> {
        self.actors
            .lock()
            .expect("actor map")
            .iter()
            .filter(|(_, handle)| !handle.is_closed())
            .map(|(thread, _)| thread.clone())
            .collect()
    }

    fn live(&self, thread: &ThreadId) -> Option<ActorHandle> {
        let mut actors = self.actors.lock().expect("actor map");
        match actors.get(thread) {
            Some(handle) if !handle.is_closed() => Some(handle.clone()),
            Some(_) => {
                actors.remove(thread);
                None
            }
            None => None,
        }
    }

    pub async fn get_or_load(&self, thread: &ThreadId) -> Result<ActorHandle, RuntimeError> {
        if let Some(handle) = self.live(thread) {
            return Ok(handle);
        }
        self.keys
            .with_lock(thread.clone(), async {
                if let Some(handle) = self.live(thread) {
                    return Ok(handle);
                }
                let handle = ActorHandle::spawn(self.context.clone(), thread.clone()).await?;
                self.actors
                    .lock()
                    .expect("actor map")
                    .insert(thread.clone(), handle.clone());
                Ok(handle)
            })
            .await
    }

    /// Retries once when the actor stopped between lookup and delivery; receipts make
    /// a retried command return the original result.
    pub async fn dispatch(
        &self,
        thread: &ThreadId,
        id: CommandId,
        command: Command,
        origin: CommandOrigin,
    ) -> Result<Committed, RuntimeError> {
        let handle = self.get_or_load(thread).await?;
        match handle.dispatch(id.clone(), command.clone(), origin).await {
            Err(RuntimeError::ActorStopped) => {
                self.get_or_load(thread)
                    .await?
                    .dispatch(id, command, origin)
                    .await
            }
            result => result,
        }
    }

    /// The thread's committed state, loading its actor if needed.
    pub async fn state(&self, thread: &ThreadId) -> Result<Arc<State>, RuntimeError> {
        match self.get_or_load(thread).await?.view().await {
            Err(RuntimeError::ActorStopped) => {
                Ok(self.get_or_load(thread).await?.view().await?.state)
            }
            result => Ok(result?.state),
        }
    }

    /// Feeds an effect result to its thread and settles the claimed outbox row in the same commit.
    pub async fn settle_effect(
        &self,
        thread: &ThreadId,
        settle: EffectSettlement,
        result: EffectResult,
    ) -> Result<Committed, RuntimeError> {
        let handle = self.get_or_load(thread).await?;
        match handle.settle_effect(settle.clone(), result.clone()).await {
            Err(RuntimeError::ActorStopped) => {
                self.get_or_load(thread)
                    .await?
                    .settle_effect(settle, result)
                    .await
            }
            result => result,
        }
    }

    /// Stops actors with no subscribers, pending effects, timer, residency pin or
    /// activity within `idle_for`. Returns the evicted threads.
    pub async fn evict_idle(&self, idle_for: Duration) -> Vec<ThreadId> {
        let candidates: Vec<ThreadId> = self
            .actors
            .lock()
            .expect("actor map")
            .keys()
            .cloned()
            .collect();
        let mut evicted = Vec::new();
        for thread in candidates {
            let stopped = self
                .keys
                .with_lock(thread.clone(), async {
                    let handle = self.actors.lock().expect("actor map").get(&thread).cloned();
                    let stopped = match handle {
                        Some(handle) => handle.evict(idle_for).await,
                        None => false,
                    };
                    if stopped {
                        self.actors.lock().expect("actor map").remove(&thread);
                    }
                    stopped
                })
                .await;
            if stopped {
                evicted.push(thread);
            }
        }
        evicted
    }

    pub fn spawn_eviction(
        self: &Arc<Self>,
        every: Duration,
        idle_for: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let registry: Weak<Self> = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(every);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticks.tick().await;
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                let evicted = registry.evict_idle(idle_for).await;
                if !evicted.is_empty() {
                    tracing::debug!(count = evicted.len(), "evicted idle thread actors");
                }
            }
        })
    }
}

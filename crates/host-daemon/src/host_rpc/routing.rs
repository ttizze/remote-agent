//! Authenticated connections and connection-wide delivery. Conversation
//! subscriptions and execution belong to `owners` and transactional storage.
use agent_protocol::protocol::{self, Notification};
use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed},
    },
};
use tokio::sync::mpsc;
mod activity;
mod connections;
pub use connections::HostSession;
pub(super) use connections::Outbound;
pub(in crate::host_rpc) use connections::{MAX_QUEUED_BYTES, frame_cost};
pub type SessionId = u64;
pub enum HostSubscription {
    List(super::runtime::list::Subscription),
    Conversation(super::owners::Subscription),
    Output(super::runtime::output::Subscription),
}
impl HostSubscription {
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        match self {
            Self::List(stream) => stream.recv().await,
            Self::Conversation(stream) => stream.recv().await,
            Self::Output(stream) => stream.recv().await,
        }
    }
}
pub struct HostReply {
    pub initial: Vec<u8>,
    pub updates: Option<HostSubscription>,
}
impl From<protocol::Response> for HostReply {
    fn from(response: protocol::Response) -> Self {
        Self {
            initial: protocol::response_frame(response).expect("response encodes"),
            updates: None,
        }
    }
}

#[derive(Default)]
struct State {
    next_session_id: SessionId,
    sessions: HashMap<SessionId, Outbound>,
}
#[derive(Clone)]
pub(crate) struct SessionRouter {
    state: Arc<Mutex<State>>,
    activity: Arc<Mutex<activity::ActivityState>>,
}
fn lock_state<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    state.lock().unwrap_or_else(|error| error.into_inner())
}
impl SessionRouter {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                next_session_id: 1,
                ..Default::default()
            })),
            activity: Default::default(),
        }
    }
}

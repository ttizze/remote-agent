//! Provider implementations enter the Host through an opaque backend.
pub mod codex;
pub(crate) mod requests;
pub use crate::claude::Claude;

use crate::{
    dictation::Dictation,
    host_rpc::{agent::Agent, service::Failure},
};
use agent_protocol::session::ProviderKind;
use std::sync::Arc;

pub struct Backend {
    pub(crate) provider: ProviderKind,
    pub(crate) agent: Result<Arc<dyn Agent>, Failure>,
    pub(crate) dictation: Option<Dictation>,
}

impl Backend {
    pub fn unavailable(provider: ProviderKind, error: impl std::fmt::Display) -> Self {
        Self {
            provider,
            agent: Err(Failure::new("provider_unavailable", error)),
            dictation: None,
        }
    }
}

impl From<Claude> for Backend {
    fn from(agent: Claude) -> Self {
        Self {
            provider: ProviderKind::Claude,
            agent: Ok(Arc::new(agent)),
            dictation: None,
        }
    }
}

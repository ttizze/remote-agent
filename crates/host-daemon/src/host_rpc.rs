//! Host-owned routing for authenticated clients and independent agent backends.
//! Backend adapters translate their protocols into the shared conversation API.

pub(crate) mod agent;
mod codex;
mod codex_history;
mod codex_home;
mod composer;
mod conversations;
pub(crate) mod native;
pub(crate) mod permissions;
pub(crate) mod requests;
pub(crate) mod routing;
pub(crate) mod service;
mod session_actor;
pub(crate) mod submission;

pub use routing::{HostSession, SessionId};
pub use service::HostRpcService;

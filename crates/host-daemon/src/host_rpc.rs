//! Host-owned routing for authenticated clients and independent agent backends.
//! Backend adapters translate their protocols into the shared conversation API.

mod codex;
mod composer;
pub(crate) mod native;
mod permissions;
pub(crate) mod requests;
pub(crate) mod routing;
mod service;
mod session_actor;
mod submission;

pub use routing::{HostSession, SessionId};
pub use service::HostRpcService;

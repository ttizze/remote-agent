//! Host-owned routing for authenticated clients and independent agent backends.
//! Backend adapters translate their protocols into the shared conversation API.

pub(crate) mod routing;
mod service;
mod thread_watch;

pub use routing::{HostSession, SessionId};
pub use service::HostRpcService;

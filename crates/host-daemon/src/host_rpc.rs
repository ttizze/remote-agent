//! Host RPCs and the authenticated connections that carry them.
mod commands;
pub(crate) mod connections;
pub(crate) mod identity;
pub(crate) mod permissions;
mod resources;
pub(crate) mod service;
pub use connections::{HostSession, SessionId};
pub use service::HostRpcService;

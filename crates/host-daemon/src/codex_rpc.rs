//! Small in-process router for one shared Codex App Server.
//!
//! The wire protocol is Codex's JSONL protocol. The router only needs the
//! top-level id to proxy Codex-originated requests to more than one phone;
//! it never models Codex params, results, errors, or extension members.

mod routing;
mod service;
mod thread_watch;

pub use routing::{CodexSession, ResponseDisposition, SessionId};
pub use service::{CodexRpcService, DispatchError};

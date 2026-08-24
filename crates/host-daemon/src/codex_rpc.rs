//! Lossless gateway between authenticated mobile sessions and Codex App Server.
//!
//! The daemon owns the transport and device authentication boundary. Once a
//! session is authenticated, Codex owns the RPC vocabulary: this module does
//! not maintain a second allow-list or a second model of Codex's objects. A
//! few small pieces of state are still required at the seam: mobile session
//! queues, proxy ids for Codex-originated requests, and response arbitration
//! when more than one authenticated session is connected.

mod routing;
mod service;

pub use routing::{CodexSession, ResponseDisposition, SessionId};
pub use service::{CodexRpcService, DispatchError};

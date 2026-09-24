//! Shared connection and provider-process I/O; independent of client state and FFI.
use agent_protocol::{models, protocol};
pub mod client;
pub mod diagnostics;
pub mod framing;
pub mod peer;
pub mod transfers;
pub mod transport;

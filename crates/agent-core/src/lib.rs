//! Client state, workflows, presentation, and optional native bindings.
use agent_protocol::{browser, models, protocol, session};
use agent_transport::{diagnostics, peer, transport};
#[cfg(test)]
extern crate self as agent_core;

pub mod client;
pub mod commands;
pub mod persistence;
pub mod presentation;
pub mod privacy;
pub mod state;
pub mod store;
#[path = "state/sync.rs"]
mod sync;
#[cfg(test)]
mod test_support;

#[cfg(feature = "bindings")]
pub mod bindings;
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

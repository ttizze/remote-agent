//! Client state, workflows, presentation, and optional native bindings.
use agent_protocol::{browser, models, protocol, session};
use agent_transport::{diagnostics, peer, transfers, transport};
#[cfg(test)]
extern crate self as agent_core;

pub mod client;
pub mod composer;
pub mod persistence;
pub mod presentation;
pub mod privacy;
pub mod state;
pub mod store;

#[cfg(feature = "bindings")]
pub mod bindings;
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

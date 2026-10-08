//! Client state, sync, commands, presentation, and optional native bindings.
use agent_protocol::{browser, models, protocol, provider};
use agent_transport::{diagnostics, peer, transport};
#[cfg(test)]
extern crate self as agent_core;

pub mod client;
pub mod commands;
pub mod connection;
pub mod environment;
mod js_text;
mod ordering;
pub mod persistence;
pub mod presentation;
pub mod privacy;
pub mod state;
pub mod sync;
pub mod view;

#[cfg(feature = "bindings")]
pub mod bindings;
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

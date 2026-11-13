//! Client state, workflows, presentation, and optional native bindings.
use agent_protocol::{browser, models, protocol, session};
use agent_transport::{diagnostics, peer, transfers, transport};
#[cfg(test)]
extern crate self as agent_core;

pub mod browser_view;
pub mod client;
pub mod composer;
pub mod conversation_cache;
pub mod localization;
pub mod panels;
pub mod persistence;
pub mod presentation;
pub mod privacy;
pub mod questions;
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub mod recording;
pub mod state;
pub mod store;
pub mod updates;

#[cfg(feature = "bindings")]
pub mod bindings;
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

#[cfg(test)]
#[path = "../tests/support/model_catalog.rs"]
mod test_models;

#[cfg(test)]
#[path = "../tests/support/conversation_fixtures.rs"]
mod test_conversations;

// Shared with integration suites; each test binary uses only part of the Host.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../tests/support/host.rs"]
mod test_host;

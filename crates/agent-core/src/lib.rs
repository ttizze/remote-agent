pub mod browser;
#[cfg(test)]
extern crate self as agent_core;

pub mod client;
pub mod composer;
pub mod diagnostics;
pub mod models;
pub mod peer;
pub mod privacy;
pub mod protocol;
pub mod session;
pub mod state;
pub mod store;
pub mod transfers;
pub mod transport;

#[cfg(feature = "bindings")]
pub mod bindings;
pub mod presentation;
#[cfg(feature = "bindings")]
uniffi::setup_scaffolding!();

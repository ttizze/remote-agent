//! Shared wire contracts and deterministic value operations. No client state or I/O.
pub mod browser;
pub mod browser_import;
pub mod browser_native;
pub mod composer;
pub mod diagnostics;
pub mod dsh;
pub mod editors;
pub mod error;
pub mod execution;
pub mod file_metadata;
pub mod file_tree;
pub mod handoff;
pub mod ids;
pub mod items;
pub mod live_activity;
pub mod message;
pub mod models;
pub mod operations;
pub mod paths;
pub mod permissions;
pub mod project_import;
pub mod protocol;
pub mod providers;
pub mod requests;
pub mod session;
pub mod tool_output;

#[cfg(test)]
mod sync_model;

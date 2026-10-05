//! Native presentation of T3 orchestration, with shared text and diff rendering.
mod conversation;
pub use conversation::*;
pub mod connections;
pub mod diff;
pub mod error;
pub mod markdown;
pub mod theme;

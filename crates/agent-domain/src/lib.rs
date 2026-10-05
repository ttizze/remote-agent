//! Conversation decisions and projections. This crate has no process, filesystem,
//! database, async runtime, global clock, or random ID source.
mod answers;
mod context;
mod fact;
mod ids;
mod machine;
mod model;
mod notification;
pub use answers::*;
pub use context::*;
pub use fact::*;
pub use ids::*;
pub use machine::*;
pub use model::*;
pub use notification::*;

#[cfg(test)]
mod tests;

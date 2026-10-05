//! Conversation decisions and projections. This crate has no process, filesystem,
//! database, async runtime, global clock, or random ID source.
mod fact;
mod ids;
mod machine;
mod model;
pub use fact::*;
pub use ids::*;
pub use machine::*;
pub use model::*;

#[cfg(test)]
mod tests;

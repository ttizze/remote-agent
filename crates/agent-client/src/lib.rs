//! Agent behavior shared by PC and mobile clients.
//! Native adapters own transport setup and UI; this crate owns agent operations
//! and state, independent of GPUI, Kotlin, Swift, and Android.

pub mod commands;
pub mod conversation;
pub mod operations;

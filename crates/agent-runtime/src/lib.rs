//! Host conversation runtime: one writer actor per thread over a SQLite fact log.
mod actor;
mod clock;
mod error;
mod keyed;
mod query;
mod shell;
mod store;
mod sync;

pub use actor::*;
pub use clock::*;
pub use error::*;
pub use keyed::*;
pub use query::*;
pub use shell::*;
pub use store::*;
pub use sync::*;

/// Envelope keys seed fact, attempt and effect IDs, so they must be unique across threads.
pub fn envelope_key(thread: &agent_domain::ThreadId, input_seq: u64) -> String {
    format!("{thread}#{input_seq}")
}

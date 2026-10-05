//! Host conversation runtime: one writer actor per thread over a SQLite fact log.
mod actor;
mod clock;
mod error;
mod import;
mod keyed;
mod outbox;
mod query;
mod session;
mod shell;
mod store;
mod sync;
mod title;

pub use actor::*;
pub use clock::*;
pub use error::*;
pub use import::*;
pub use keyed::*;
pub use outbox::*;
pub use query::*;
pub use session::*;
pub use shell::*;
pub use store::*;
pub use sync::*;
pub use title::*;

/// Envelope keys seed fact, attempt and effect IDs, so they must be unique across threads.
pub fn envelope_key(thread: &agent_domain::ThreadId, input_seq: u64) -> String {
    format!("{thread}#{input_seq}")
}

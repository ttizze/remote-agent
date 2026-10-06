//! Host conversation runtime: one writer actor per thread over a SQLite fact log.
mod actor;
mod clock;
mod error;
mod executor;
mod import;
mod keyed;
mod launch;
mod ops;
mod outbox;
mod query;
mod runtime;
mod session;
mod setup;
mod shell;
mod store;
mod sweep;
mod sync;
mod title;

pub use actor::*;
pub use clock::*;
pub use error::*;
pub use executor::*;
pub use import::*;
pub use keyed::*;
pub use launch::*;
pub use ops::*;
pub use outbox::*;
pub use query::*;
pub use runtime::*;
pub use session::*;
pub use setup::*;
pub use shell::*;
pub use store::*;
pub use sweep::*;
pub use sync::*;
pub use title::*;

/// Envelope keys seed fact, attempt and effect IDs, so they must be unique across threads.
pub fn envelope_key(thread: &agent_domain::ThreadId, input_seq: u64) -> String {
    format!("{thread}#{input_seq}")
}

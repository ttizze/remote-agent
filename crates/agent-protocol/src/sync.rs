//! Subscription addresses and ephemeral command output. Canonical thread and
//! shell snapshots/events use orchestration::projection and orchestration::shell.
use serde::{Deserialize, Serialize};

mod output;
pub use output::{Output, OutputUpdate, TAIL_CHARS};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope<C> {
    Conversation(C),
    List,
}

#[cfg(test)]
mod tests;

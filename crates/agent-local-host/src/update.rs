//! The private handover journal is shared by the Host and the detached updater.
use agent_protocol::{ids::TurnId, operations::Submission, session::SessionRef};
use serde::{Deserialize, Serialize};

pub const RECOVERY_FILE: &str = "host-update-recovery.json";

#[derive(Serialize, Deserialize)]
pub struct Recovery {
    pub tasks: Vec<InterruptedTask>,
}

#[derive(Serialize, Deserialize)]
// The detached worker is copied from the installed app before replacement.
// These fields remain its wire contract through the first canonical-store update.
pub struct InterruptedTask {
    pub native_session: SessionRef,
    pub submission: Submission,
    pub cwd: Option<String>,
    pub last_turn: Option<TurnId>,
    pub delivery: ContinuationDelivery,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum ContinuationDelivery {
    Pending,
    /// The previous Host recorded this before sending. Never resend it automatically.
    Sending,
    /// The saved turn completed while the previous Host was stopping.
    Completed,
    Accepted(TurnId),
    Failed(String),
}

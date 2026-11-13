//! The private handover journal is shared by the Host and the detached updater.
use agent_protocol::{
    ids::{ClientInputId, TurnId},
    session::SessionRef,
};
use serde::{Deserialize, Serialize};

pub const RECOVERY_FILE: &str = "host-update-recovery.json";

#[derive(Serialize, Deserialize)]
pub struct Recovery {
    pub tasks: Vec<InterruptedTask>,
}

#[derive(Serialize, Deserialize)]
pub struct InterruptedTask {
    pub session: SessionRef,
    pub input: ClientInputId,
    pub last_turn: Option<TurnId>,
    pub delivery: ContinuationDelivery,
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
pub enum ContinuationDelivery {
    Pending,
    /// The saved turn completed while the previous Host was stopping.
    Completed,
    Accepted(TurnId),
    Failed(String),
}

//! Conversation input and RPC commands. Durable execution is owned by orchestration.
use crate::{
    ids::{ClientInputId, RequestId, TurnId},
    models::ModelRef,
    operations::{FollowUp, Input as InputPart},
    requests::Answer,
};
use serde::{Deserialize, Serialize};
pub mod records;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputContent {
    /// T3 message.dispatch freshContext: a new provider conversation.
    #[serde(default)]
    pub fresh_context: bool,
    pub parts: Vec<InputPart>,
    pub model: Option<ModelRef>,
    pub effort: Option<String>,
    pub service_tier: Option<String>,
    pub follow_up: FollowUp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    Submit {
        id: ClientInputId,
        content: InputContent,
    },
    ResumeQueue,
    RemoveInput(ClientInputId),
    EditInput {
        id: ClientInputId,
        parts: Vec<InputPart>,
    },
    MoveInput {
        id: ClientInputId,
        before: Option<ClientInputId>,
    },
    SteerInput(ClientInputId),
    ResolveUnknown {
        id: ClientInputId,
        retry: bool,
    },
    RetryInput {
        id: ClientInputId,
        work_locally: bool,
    },
    Interrupt {
        turn: Option<TurnId>,
    },
    Answer {
        id: RequestId,
        answer: Answer,
    },
    Rename(String),
    Rewind(TurnId),
}

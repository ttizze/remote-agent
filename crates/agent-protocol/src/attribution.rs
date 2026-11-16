//! The sender of a message other than the local user, from T3 createdBy and
//! scheduledTaskId. Never inferred from message text or prompts.
use crate::session::SessionRef;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Author {
    /// A Host automation (T3 scheduled task); clients name it from their list.
    Automation {
        id: uuid::Uuid,
    },
    /// The agent of the conversation that sent this prompt.
    Agent {
        session: SessionRef,
    },
    System,
}

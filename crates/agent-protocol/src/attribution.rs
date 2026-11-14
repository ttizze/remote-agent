//! Authenticated origin metadata. Never inferred from message text or prompts.
use crate::session::SessionRef;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Author {
    Device {
        id: String,
    },
    Automation {
        id: uuid::Uuid,
        name: String,
    },
    Agent {
        session: SessionRef,
        name: Option<String>,
    },
    System {
        label: String,
    },
}

use crate::shared::Shared;
use agent_core::presentation::conversation::RenderedItem;
use agent_protocol::ids::TurnId;

#[derive(Clone, PartialEq)]
pub(crate) struct Item {
    pub source: Shared<RenderedItem>,
    pub turn: TurnId,
    /// Its turn's live row (`RenderedTurn::live_activity`), which shimmers.
    pub live: bool,
    /// What its row reads (`TurnRow::Work`, `TurnRow::Live`).
    pub label: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Status {
    /// The turn works with nothing else live.
    Thinking,
    /// The provider's stream dropped and is retried.
    Retrying,
    /// The turn waits for an answer: an approval (its body names what for)
    /// or input.
    Requested {
        approval: bool,
    },
    SettingUp,
}

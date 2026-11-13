use crate::shared::Shared;
use agent_core::presentation::conversation::RenderedItem;
use agent_protocol::ids::TurnId;
use nsh_core::prelude::*;

#[derive(Clone, PartialEq)]
pub(crate) struct Item {
    pub source: Shared<RenderedItem>,
    pub turn: TurnId,
    /// Its turn's live row (`RenderedTurn::live_activity`), which shimmers.
    pub live: bool,
    /// What its row reads (`TurnRow::Work`, `TurnRow::Live`).
    pub label: String,
}

/// T3 Code's work log line: `icon` in its box, then the label.
pub(crate) fn line(icon: impl IntoElement, label: impl IntoElement) -> View {
    use crate::conversation::look;
    view()
        .flex_1()
        .min_w_0()
        .flex_row()
        .items_center()
        .gap(px(look::ICON_GAP))
        .child(
            view()
                .w(px(look::ICON_BOX))
                .h(px(look::ICON_BOX))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .child(icon),
        )
        .child(label)
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

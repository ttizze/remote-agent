//! The conversation timeline: `ThreadView.rows` in a virtual list, with the
//! banners over it and the history control above it.
use super::{Desktop, Views};
use agent_core::view::thread::TimelineDisclosure;
use gpui_kit::*;

pub(crate) struct TimelineState {
    disclosure: TimelineDisclosure,
}
impl TimelineState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        Self {
            disclosure: TimelineDisclosure::default(),
        }
    }
    /// What the user expanded, which the rows are derived with.
    pub(crate) fn disclosure(&self) -> TimelineDisclosure {
        self.disclosure.clone()
    }
    /// Forgets the rows of the previous connection.
    pub(crate) fn reset(&mut self) {}
}

impl Desktop {
    pub(crate) fn render_timeline(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().flex_1().into_any_element()
    }

    /// Brings the list from `previous` rows to the current ones.
    pub(crate) fn timeline_views_changed(
        &mut self,
        previous: &Views,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let _ = previous;
    }
}

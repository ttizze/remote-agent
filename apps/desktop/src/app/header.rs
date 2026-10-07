//! The chat header: project crumb, thread title with its action menu and
//! inline rename, and the panel toggles.
use super::Desktop;
use gpui_kit::*;

pub(crate) struct HeaderState {}
impl HeaderState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        Self {}
    }
}

impl Desktop {
    pub(crate) fn render_header(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().into_any_element()
    }
}

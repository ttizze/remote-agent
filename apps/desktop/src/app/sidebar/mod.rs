//! The thread list: drafts, shelves of thread rows, search results and the
//! project scope, drawn from `SidebarView`.
use super::Desktop;
use agent_core::view::sidebar::SidebarOptions;
use gpui_kit::*;

pub(crate) struct SidebarState {
    options: SidebarOptions,
}
impl SidebarState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        Self {
            options: SidebarOptions::default(),
        }
    }
    /// The shelves the user expanded and the settled pages shown.
    pub(crate) fn options(&self) -> SidebarOptions {
        self.options.clone()
    }
}

impl Desktop {
    pub(crate) fn render_sidebar(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().into_any_element()
    }

    /// Opens the previous or next thread in sidebar order.
    pub(crate) fn select_adjacent_thread(&mut self, next: bool, cx: &mut Context<Self>) {
        let _ = (next, cx);
    }
}

//! The thread action menu the sidebar rows and the header share, and the
//! dialogs its items open (custom snooze, rename).
use super::Desktop;
use gpui_kit::*;

pub(crate) struct MenuState {}
impl MenuState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        Self {}
    }
}

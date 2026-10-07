//! The right panel (Diff, Terminal, Files, Browser), the thread terminal
//! drawer and the thread details panel.
use super::Desktop;
use agent_core::{connection::Outcome, view::header::HeaderPanelState};
use gpui_kit::*;

/// The right panel's tabs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelTab {
    Diff,
    Terminal,
    Files,
    Browser,
}

pub(crate) struct PanelState {}
impl PanelState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        Self {}
    }
    /// The panels the header shows as open.
    pub(crate) fn header_panels(&self) -> HeaderPanelState {
        HeaderPanelState::default()
    }
    /// Closes what belonged to the previous connection.
    pub(crate) fn reset(&mut self, _: &mut Window, _: &mut Context<Desktop>) {}
}

impl Desktop {
    pub(crate) fn render_right_panel(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }

    pub(crate) fn render_terminal_drawer(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }

    pub(crate) fn render_thread_details(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<AnyElement> {
        None
    }

    /// Follows the snapshot: the diff source, file editor and terminals.
    pub(crate) fn sync_panels(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    pub(crate) fn panel_outcome(
        &mut self,
        outcome: &Outcome,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let _ = outcome;
    }

    pub(crate) fn open_right_panel(
        &mut self,
        tab: PanelTab,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let _ = tab;
    }

    pub(crate) fn toggle_right_panel(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    pub(crate) fn toggle_terminal_drawer(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    pub(crate) fn toggle_thread_details(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    /// Opens the Diff tab at a turn (the latest when `None`) and file.
    pub(crate) fn open_diff(
        &mut self,
        run_id: Option<String>,
        file_path: Option<String>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let _ = (run_id, file_path);
    }

    /// Opens one of the thread's terminals in the drawer.
    pub(crate) fn open_thread_terminal(
        &mut self,
        thread_id: String,
        terminal_id: String,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let _ = (thread_id, terminal_id);
    }
}

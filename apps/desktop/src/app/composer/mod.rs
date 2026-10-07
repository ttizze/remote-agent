//! The composer: banners above it, the editor surface and its controls, and
//! the new-thread hero.
use super::Desktop;
use agent_core::{connection::Outcome, view::composer::view::ComposerOptions};
use gpui_kit::*;

pub(crate) struct ComposerState {}
impl ComposerState {
    pub(crate) fn new(
        _: &mut Window,
        _: &mut Context<Desktop>,
        _subscriptions: &mut Vec<Subscription>,
    ) -> Self {
        Self {}
    }
    /// Labels and the held modifier the composer view is derived with.
    pub(crate) fn options(&self) -> ComposerOptions {
        ComposerOptions::default()
    }
}

impl Desktop {
    /// The banner stack and composer under the open thread's timeline.
    pub(crate) fn render_composer(&mut self, _: &mut Window, _: &mut Context<Self>) -> AnyElement {
        div().into_any_element()
    }

    /// The new-thread draft: the hero headline over a centred composer.
    pub(crate) fn render_new_thread(
        &mut self,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> AnyElement {
        div().flex_1().into_any_element()
    }

    /// Brings the editor in line with the draft after a snapshot or views change.
    pub(crate) fn sync_composer(&mut self, _: &mut Window, _: &mut Context<Self>) {}

    pub(crate) fn composer_outcome(
        &mut self,
        outcome: &Outcome,
        _: &mut Window,
        _: &mut Context<Self>,
    ) {
        let _ = outcome;
    }

    pub(crate) fn focus_composer(&mut self, _: &mut Window, _: &mut Context<Self>) {}
}

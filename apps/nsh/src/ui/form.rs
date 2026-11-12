//! Shared presentation and request lifetime for workspace forms.
use super::{Enabled, colors};
use crate::{Connection, l10n::tr};
use agent_core::{presentation::conversation::ActionTone, state::Intent};
use nsh_core::prelude::*;

#[derive(Clone, Copy)]
pub(crate) struct Submission {
    pub busy: Signal<bool>,
    pub failure: Signal<String>,
    pub owner: Owner,
}

impl Submission {
    pub fn new() -> Self {
        Self {
            busy: signal(false),
            failure: signal(String::new()),
            owner: Owner::current(),
        }
    }

    pub fn submit(
        self,
        connection: Connection,
        host: &str,
        intent: Intent,
        complete: impl FnOnce(Result<(), String>) + 'static,
    ) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.failure.set(String::new());
        let response = connection
            .client
            .with(|client| client.request(host, connection.pane, intent));
        self.owner.run(move || {
            spawn(async move {
                let result = response
                    .recv()
                    .await
                    .map_err(|reason| reason.to_string())
                    .and_then(|result| result.map(|_| ()).map_err(|reason| reason.to_string()));
                self.busy.set(false);
                complete(result);
            })
        });
    }

    pub fn dialog(self, cancel: impl Fn() + 'static) -> nsh_core::element::Modal {
        nsh_base::dialog()
            .presentation(ModalPresentation::FormSheet)
            .close_on_escape(move || !self.busy.get())
            .close_on_backdrop_press(move || !self.busy.get())
            .on_cancel(cancel)
            .flex_1()
            .items_center()
            .justify_center()
    }

    pub fn field(
        self,
        value: Signal<String>,
        submit: impl Fn() + 'static,
    ) -> nsh_core::element::Input {
        super::field()
            .value(value)
            .on_change(move |text: &str| value.set(text.into()))
            .on_submit(move |_: &str| submit())
            .disabled(self.busy)
    }

    pub fn error(self) -> impl IntoElement {
        when(
            move || !self.failure.get().is_empty(),
            move || {
                text(move || self.failure.get())
                    .text_color(crate::conversation::look::DESTRUCTIVE)
                    .selectable(true)
            },
        )
    }

    pub fn buttons(
        self,
        title: &'static str,
        valid: impl Fn() -> bool + Copy + 'static,
        cancel: impl Fn() + 'static,
        save: impl Fn() + 'static,
    ) -> View {
        view()
            .flex_row()
            .justify_end()
            .gap_2()
            .child(super::text_button(tr("キャンセル"), cancel).enabled(move || !self.busy.get()))
            .child(
                super::action_button(title, ActionTone::Primary, save)
                    .enabled(move || !self.busy.get() && valid()),
            )
    }
}

pub(crate) fn panel(title: &'static str, width: f32, radius: f32) -> View {
    view()
        .p_5()
        .gap_3()
        .w_full()
        .max_w(px(width))
        .rounded(px(radius))
        .bg(colors::SURFACE)
        .child(
            text(title)
                .text_size(px(18.0))
                .font_weight(FontWeight::SEMIBOLD),
        )
}

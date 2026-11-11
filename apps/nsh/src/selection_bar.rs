//! The bar of actions on a message's selected text, as T3 Code's
//! (`AssistantSelectionToolbar`) and the desktop client's: a pill just above
//! the selection, kept inside the window, that goes once the selection does or
//! the conversation scrolls.
use crate::l10n::tr;
use crate::ui::{self, Icon, colors};
use nsh_core::{
    overlay::{Align, Placement},
    prelude::*,
};

/// Text selected in a message.
#[derive(Clone, PartialEq)]
pub(crate) struct Selected {
    pub text: String,
    /// The message's target (`rich_text::Target` as JSON), which its actions
    /// act for.
    pub target: String,
    /// The box around the selection in the window's content.
    pub bounds: Bounds<Points>,
    /// Whether it can be asked about in a side chat: not from inside one.
    pub side_chat: bool,
}

/// One conversation's bar.
#[derive(Clone, Copy)]
pub(crate) struct SelectionBar(Signal<Option<Selected>>);

impl SelectionBar {
    pub(crate) fn new() -> Self {
        Self(signal(None))
    }

    /// A message's selection changed: `None` once nothing is selected.
    pub(crate) fn select(self, selected: Option<Selected>) {
        if self.0.with_untracked(|shown| *shown != selected) {
            self.0.set(selected);
        }
    }

    /// The conversation scrolled, or the reader pressed elsewhere.
    pub(crate) fn dismiss(self) {
        self.select(None);
    }

    /// The bar while something is selected; `act` does what the reader chose
    /// (`"quote"`, `"explain"` or `"sideChat"`) with the selection.
    pub(crate) fn view(
        self,
        act: impl Fn(&'static str, Selected) + Copy + 'static,
    ) -> impl IntoElement {
        let bar = self.0;
        when(
            move || bar.with(Option::is_some),
            move || {
                let side_chat = bar.with_untracked(|s| s.as_ref().is_some_and(|s| s.side_chat));
                let action = move |name: &'static str| {
                    move || {
                        if let Some(selected) = bar.get_untracked() {
                            bar.set(None);
                            act(name, selected);
                        }
                    }
                };
                overlay()
                    .anchor_bounds(move || {
                        bar.with(|s| s.as_ref().map(|s| s.bounds).unwrap_or_default())
                    })
                    .placement(Placement::Top)
                    .align(Align::Start)
                    .offset(px(4.0))
                    .margin(px(8.0))
                    .on_cancel(move || self.dismiss())
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .p(px(2.0))
                            .rounded_full()
                            .border_1()
                            .border_color(colors::border())
                            .bg(colors::RAISED)
                            .shadow_sm()
                            .label(SharedString::from(tr("選択範囲の操作")))
                            .child(button(Icon::QUOTE, tr("チャットに追加"), action("quote")))
                            .child(button(Icon::SPARKLES, tr("詳細を表示"), action("explain")))
                            .child(when(
                                move || side_chat,
                                move || {
                                    button(
                                        Icon::QUESTION,
                                        tr("サイドチャットで質問"),
                                        action("sideChat"),
                                    )
                                },
                            )),
                    )
            },
        )
    }
}

/// A small pill button, as T3 Code's `xs` glass buttons: its icon and label.
fn button(glyph: Icon, label: &'static str, press: impl Fn() + 'static) -> impl IntoElement {
    nsh_base::button()
        .accessibility_label(Some(SharedString::from(label)))
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .h(px(24.0))
        .px(px(10.0))
        .rounded_full()
        .text_color(colors::foreground())
        .hover(|s| s.bg(colors::GHOST_HOVER))
        .active(|s| s.bg(colors::SELECTED))
        .on_press(press)
        .child(ui::icon(glyph, 14.0))
        .child(text(label).text_size(px(12.0)).number_of_lines(1))
}

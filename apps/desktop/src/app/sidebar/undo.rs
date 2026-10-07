//! The sidebar's Undo notice after settling, snoozing, unpinning, archiving
//! or discarding a draft.
use super::super::{
    Desktop,
    ui::{self, color},
};
use agent_core::state::Intent;
use gpui_kit::{component::h_flex, *};
use std::time::Duration;

impl Desktop {
    /// Restores the actions the notice shows; false when nothing is left to undo.
    pub(crate) fn undo_thread_action(&mut self) -> bool {
        if self.snapshot.thread_undo_notice(ui::now_ms()).is_none() {
            return false;
        }
        self.perform(Intent::UndoThreadAction);
        true
    }

    /// "Settled 2 threads, ⌘Z to undo", until the Undo expires.
    pub(super) fn render_undo_notice(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notice = self.snapshot.thread_undo_notice(ui::now_ms())?;
        if self.sidebar.undo_expiry.as_ref().map(|(at, _)| *at) != Some(notice.expires_at_ms) {
            let wait = (notice.expires_at_ms - ui::now_ms()).max(0) as u64;
            let timer = cx.spawn(async move |view, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(wait))
                    .await;
                let _ = view.update(cx, |_, cx| cx.notify());
            });
            self.sidebar.undo_expiry = Some((notice.expires_at_ms, timer));
        }
        let action = match self.keymap.shortcut_label("thread.undo") {
            Some(shortcut) => format!("{shortcut} to undo"),
            None => "Undo".into(),
        };
        Some(
            ui::text_2xs(div())
                .mx_2()
                .mt_2()
                .mb_1()
                .flex_shrink_0()
                .rounded(px(8.))
                .border_1()
                .border_color(color("sidebarBorder"))
                .bg(color("sidebarControlSurface"))
                .px_2()
                .py_1p5()
                .text_color(color("sidebarMutedForeground"))
                .child(
                    h_flex()
                        .flex_wrap()
                        .child(format!("{}, ", notice.label))
                        .child(
                            div()
                                .id("thread-undo")
                                .cursor_pointer()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(color("text"))
                                .hover(|button| button.underline())
                                .child(action)
                                .on_click(cx.listener(|view, _, _, _| {
                                    view.undo_thread_action();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

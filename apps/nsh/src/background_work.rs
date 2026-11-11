//! The work an answered conversation still waits on, above its composer as
//! T3 Code's strip: a dot that pulses while the work will wake the agent, the
//! work's title and names, and Stop, the only stop while the composer sends.
use crate::{
    Connection,
    l10n::{language, tr},
    ui::{self, Enabled, colors},
};
use agent_core::state::{Intent, operations as op};
use nsh_core::prelude::*;

pub(crate) fn strip() -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let composer = connection.composer;
    let work = computed(move || composer.with(|c| c.controls.background.clone()));
    let waiting = move || work.with(|work| work.as_ref().is_some_and(|work| work.waiting));
    let title = move || {
        work.with(|work| work.as_ref().map(|work| work.title(language())))
            .unwrap_or_default()
    };
    let description = computed(move || {
        work.with(|work| work.as_ref().and_then(|work| work.description(language())))
    });
    let stopping =
        move || composer.with(|c| c.controls.interrupt_phase == Some(op::OperationPhase::Running));
    let stop = move || {
        if composer.with_untracked(|c| c.can_interrupt())
            && let Some((thread_id, turn_id)) = composer.with_untracked(|c| {
                c.controls
                    .navigation
                    .thread_id
                    .clone()
                    .zip(c.controls.running_turn.clone())
            })
        {
            connection.dispatch(Intent::Interrupt(op::Interrupt { thread_id, turn_id }));
        }
    };
    when(
        move || work.with(Option::is_some),
        move || {
            view()
                .w_full()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(px(12.0))
                .border_1()
                .border_color(colors::border())
                .bg(colors::RAISED)
                .text_color(colors::foreground())
                .child(
                    // A dev server can run for hours after the agent is done,
                    // so only work that will wake the agent pulses.
                    view()
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded_full()
                        .bg(colors::foreground())
                        .style_when(waiting, |s| s.animate_pulse()),
                )
                .child(
                    view()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(text(title).text_size(px(14.0)).number_of_lines(1))
                        .child(when(
                            move || description.with(Option::is_some),
                            move || {
                                text(move || description.get().unwrap_or_default())
                                    .text_size(px(12.0))
                                    .text_color(colors::muted())
                                    .number_of_lines(2)
                            },
                        )),
                )
                .child(
                    ui::text_button(
                        move || {
                            SharedString::from(if stopping() {
                                tr("停止中")
                            } else {
                                tr("停止")
                            })
                        },
                        stop,
                    )
                    .enabled(move || composer.with(|c| c.can_interrupt())),
                )
        },
    )
}

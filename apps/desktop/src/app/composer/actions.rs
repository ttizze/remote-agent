//! The composer footer's right side: attaching files, dictation, the
//! context-window meter and the primary action.
use super::{Desktop, on_click};
use crate::app::ui::{color, icon, text_2xs, tint};
use agent_core::{
    state::Intent,
    view::composer::{
        actions::{ComposerPrimaryAction, STOP_TOOLTIP, SendButton, SendIcon},
        context_meter::ContextWindowMeter,
        view::ComposerView,
    },
};
use gpui_kit::{
    component::{
        Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        popover::Popover,
        spinner::Spinner,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::f32::consts::PI;

/// A circular path of `fraction` of a turn, clockwise from twelve o'clock.
fn arc(
    center: Point<Pixels>,
    radius: Pixels,
    width: Pixels,
    fraction: f32,
) -> Option<Path<Pixels>> {
    let fraction = fraction.clamp(0., 1.);
    if fraction <= 0. {
        return None;
    }
    let steps = (96. * fraction).ceil().max(2.) as usize;
    let mut builder = PathBuilder::stroke(width);
    for step in 0..=steps {
        let angle = -PI / 2. + 2. * PI * fraction * step as f32 / steps as f32;
        let point = point(
            center.x + radius * angle.cos(),
            center.y + radius * angle.sin(),
        );
        if step == 0 {
            builder.move_to(point);
        } else {
            builder.line_to(point);
        }
    }
    builder.build().ok()
}

/// The 20px usage ring: a faint track under the used share.
fn ring(progress: f64, overloaded: bool) -> impl IntoElement {
    let track = color("textMuted").opacity(0.24);
    let usage = if overloaded {
        color("error")
    } else {
        color("textMuted").opacity(0.72)
    };
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let size = bounds.size.width.min(bounds.size.height);
            let (radius, width) = (size * (9.75 / 24.), size * (3. / 24.));
            let center = bounds.center();
            if let Some(path) = arc(center, radius, width, 1.) {
                window.paint_path(path, track);
            }
            if let Some(path) = arc(center, radius, width, progress as f32 / 100.) {
                window.paint_path(path, usage);
            }
        },
    )
    .size(px(20.))
}

/// A message-action pill (Submit, Refine, Implement).
fn pill(id: impl Into<ElementId>, label: String, height: f32, disabled: bool) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(height))
        .px_4()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(color("messageAction"))
        .text_color(color("messageActionForeground"))
        .text_sm()
        .font_weight(FontWeight::MEDIUM)
        .when(disabled, |pill| pill.opacity(0.64))
        .when(!disabled, |pill| {
            pill.cursor_pointer()
                .hover(|pill| pill.bg(color("messageActionHover")))
        })
        .child(label)
}

impl Desktop {
    pub(super) fn composer_actions(
        &mut self,
        composer: &ComposerView,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .flex_none()
            .items_center()
            .gap_2()
            .child(
                Button::new("composer-attach")
                    .icon(icon("paperclip").size(px(16.)))
                    .ghost()
                    .small()
                    .size(px(28.))
                    .tooltip("Attach files")
                    .accessibility_label("Attach files")
                    .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                        if let Some(key) = view.composer_attachment_target() {
                            view.pick_attachments(key);
                        }
                    })),
            )
            .child(self.composer_dictation(cx))
            .children(
                composer
                    .context_meter
                    .as_ref()
                    .map(|meter| self.composer_context_meter(meter)),
            )
            .child(self.composer_primary_action(composer, cx))
            .into_any_element()
    }

    /// The microphone, or the recording's state with its finish and cancel.
    fn composer_dictation(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(dictation) = &self.dictation else {
            return Button::new("composer-dictation")
                .icon(icon("mic").size(px(16.)))
                .ghost()
                .small()
                .size(px(28.))
                .disabled(!self.snapshot.connected)
                .tooltip("Start dictation")
                .accessibility_label("Start dictation")
                .on_click(
                    cx.listener(|view, _: &ClickEvent, window, cx| {
                        view.start_dictation(window, cx)
                    }),
                )
                .into_any_element();
        };
        let level = dictation.level;
        h_flex()
            .flex_none()
            .items_center()
            .gap_1()
            .child(if dictation.recording {
                h_flex()
                    .size(px(16.))
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(4.))
                            .h(px(4. + 12. * level))
                            .rounded_full()
                            .bg(color("error")),
                    )
                    .into_any_element()
            } else {
                Spinner::new().with_size(px(14.)).into_any_element()
            })
            .child(
                div()
                    .text_xs()
                    .text_color(color("textMuted"))
                    .child(dictation.label),
            )
            .when(dictation.recording, |row| {
                row.child(
                    Button::new("composer-dictation-finish")
                        .icon(icon("check").size(px(14.)))
                        .ghost()
                        .xsmall()
                        .tooltip("Finish dictation")
                        .accessibility_label("Finish dictation")
                        .on_click(cx.listener(|view, _: &ClickEvent, window, cx| {
                            view.finish_dictation(window, cx)
                        })),
                )
            })
            .child(
                Button::new("composer-dictation-cancel")
                    .icon(icon("x").size(px(14.)))
                    .ghost()
                    .xsmall()
                    .tooltip("Cancel dictation")
                    .accessibility_label("Cancel dictation")
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| view.cancel_dictation(cx))),
            )
            .into_any_element()
    }

    /// The ring and, on click, the context window's numbers.
    fn composer_context_meter(&self, meter: &ContextWindowMeter) -> AnyElement {
        let details = meter.clone();
        let trigger = Button::new("context-meter")
            .ghost()
            .small()
            .size(px(28.))
            .accessibility_label(meter.accessibility_label.clone())
            .child(ring(meter.ring_progress, meter.overloaded));
        Popover::new("context-meter-popover")
            .anchor(Anchor::BottomRight)
            .trigger(trigger)
            .content(move |_, _, _| context_meter_details(&details))
            .into_any_element()
    }

    fn composer_primary_action(
        &mut self,
        composer: &ComposerView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &composer.primary_action {
            ComposerPrimaryAction::Send(button) => self.composer_send_button(button, cx),
            ComposerPrimaryAction::Stop { tooltip, .. } => {
                stop_button(tooltip.clone(), cx).into_any_element()
            }
            ComposerPrimaryAction::Answer {
                show_stop,
                previous,
                submit_label,
                submit_disabled,
            } => {
                let disabled = *submit_disabled;
                h_flex()
                    .gap_2()
                    .when(*show_stop, |row| {
                        row.child(stop_button(STOP_TOOLTIP.into(), cx))
                    })
                    .when_some(previous.clone(), |row, previous| {
                        let button = Button::new("previous-question")
                            .outline()
                            .small()
                            .disabled(previous.disabled)
                            .accessibility_label(previous.label.clone())
                            .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                                view.previous_answer_question()
                            }));
                        row.child(if previous.icon_only {
                            button.icon(icon("chevron-left"))
                        } else {
                            button.label(previous.label)
                        })
                    })
                    .child(
                        pill("submit-answer", submit_label.clone(), 28., disabled).when(
                            !disabled,
                            |pill| {
                                pill.on_click(cx.listener(|view, _: &ClickEvent, window, cx| {
                                    view.advance_answer(window, cx)
                                }))
                            },
                        ),
                    )
                    .into_any_element()
            }
            ComposerPrimaryAction::Refine { label, disabled } => {
                let disabled = *disabled;
                pill("refine-plan", label.clone(), 32., disabled)
                    .when(!disabled, |pill| {
                        pill.on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                            view.perform(Intent::Send { alternate: false })
                        }))
                    })
                    .into_any_element()
            }
            ComposerPrimaryAction::Implement {
                label,
                disabled,
                new_thread_label,
            } => {
                let disabled = *disabled;
                let view = cx.entity().downgrade();
                let new_thread_label = new_thread_label.clone();
                h_flex()
                    .child(
                        pill("implement-plan", label.clone(), 32., disabled)
                            .rounded_r_none()
                            .when(!disabled, |pill| {
                                pill.on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                                    view.perform(Intent::PlanFollowUp { new_thread: false })
                                }))
                            }),
                    )
                    .child(
                        Button::new("implement-actions")
                            .icon(icon("chevron-down").size(px(14.)))
                            .h(px(32.))
                            .px_2()
                            .rounded_l_none()
                            .rounded_r_full()
                            .border_l_1()
                            .border_color(color("messageActionForeground").opacity(0.2))
                            .primary()
                            .disabled(disabled)
                            .accessibility_label("Implementation actions")
                            .dropdown_menu_with_anchor(Anchor::BottomRight, move |menu, _, _| {
                                menu.item(PopupMenuItem::new(new_thread_label.clone()).on_click(
                                    on_click(&view, |view, _, _| {
                                        view.perform(Intent::PlanFollowUp { new_thread: true })
                                    }),
                                ))
                            }),
                    )
                    .into_any_element()
            }
        }
    }

    /// The round send button; Mod+click sends the other follow-up.
    fn composer_send_button(&mut self, button: &SendButton, cx: &mut Context<Self>) -> AnyElement {
        let glyph = match button.icon {
            SendIcon::Spinner => Spinner::new()
                .with_size(px(16.))
                .color(color("messageActionForeground"))
                .into_any_element(),
            icon_kind => icon(match icon_kind {
                SendIcon::Resume => "play",
                SendIcon::Check => "check",
                SendIcon::Queue => "list-plus",
                SendIcon::Steer => "corner-up-right",
                _ => "arrow-up",
            })
            .size(px(16.))
            .text_color(color("messageActionForeground"))
            .into_any_element(),
        };
        let tooltip = button.tooltip.clone();
        let (disabled, resumes) = (button.disabled, button.resumes);
        div()
            .id("composer-send")
            .size(px(32.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(color("messageAction"))
            .shadow_xs()
            .when(disabled, |send| send.opacity(0.64))
            .when(!disabled, |send| {
                send.cursor_pointer()
                    .hover(|send| send.bg(color("messageActionHover")))
                    .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                        if resumes {
                            view.resume_composer_thread();
                        } else {
                            view.submit_composer(event.modifiers().secondary(), window, cx);
                        }
                    }))
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(glyph)
            .into_any_element()
    }
}

/// The round stop button: a small square on the error color.
fn stop_button(tooltip: String, cx: &mut Context<Desktop>) -> Stateful<Div> {
    div()
        .id("composer-stop")
        .size(px(32.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(color("error").opacity(0.9))
        .shadow_xs()
        .cursor_pointer()
        .hover(|stop| stop.bg(color("error")))
        .on_click(cx.listener(|view, _: &ClickEvent, _, _| view.perform(Intent::Stop)))
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .child(div().size(px(8.)).rounded(px(1.5)).bg(hsla(0., 0., 1., 1.)))
}

fn context_meter_details(meter: &ContextWindowMeter) -> Div {
    let usage = if meter.overloaded {
        color("error")
    } else {
        color("textMuted").opacity(0.72)
    };
    v_flex()
        .w(px(240.))
        .gap_2()
        .child(
            h_flex()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color("textMuted"))
                        .child("Context Window"),
                )
                .child(
                    text_2xs(h_flex())
                        .text_color(color("textMuted"))
                        .when_some(meter.used_percentage_label.clone(), |row, percent| {
                            row.child(percent).child(div().mx_1().child("·"))
                        })
                        .child(meter.tokens_label.clone()),
                ),
        )
        .when_some(meter.progress, |column, progress| {
            column.child(
                div()
                    .h(px(6.))
                    .w_full()
                    .overflow_hidden()
                    .rounded_full()
                    .bg(tint("muted", 0.6))
                    .child(
                        div()
                            .h_full()
                            .w(relative(progress as f32 / 100.))
                            .rounded_full()
                            .bg(usage),
                    ),
            )
        })
        .when_some(meter.total_processed_label.clone(), |column, total| {
            column.child(
                text_2xs(h_flex())
                    .justify_between()
                    .gap_3()
                    .text_color(color("textMuted"))
                    .child("Total processed")
                    .child(div().font_weight(FontWeight::MEDIUM).child(total)),
            )
        })
        .when_some(meter.compaction_message.clone(), |column, message| {
            column.child(
                text_2xs(div())
                    .mt_1()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color("textMuted"))
                    .child(message),
            )
        })
        .when_some(meter.compact.clone(), |column, compact| {
            // The Host takes no compaction request yet, so the button stays
            // disabled; agent-core does not offer it either.
            column
                .child(
                    Button::new("compact-context")
                        .outline()
                        .xsmall()
                        .w_full()
                        .mt_1()
                        .icon(icon("minimize-2"))
                        .label(compact.label)
                        .disabled(true),
                )
                .when_some(compact.disabled_reason, |column, reason| {
                    column.child(text_2xs(div()).text_color(color("textMuted")).child(reason))
                })
        })
}

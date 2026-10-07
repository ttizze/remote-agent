//! The work log: tool calls and their detail, group headers, the live and
//! thinking rows, the working timer and turn folds.
use super::{
    Desktop, cards::render_setup_card, chevron, color, icon, local_time, markdown::chat_markdown,
    mono, shimmer_label, tint,
};
use agent_core::state::Intent;
use agent_core::view::thread::ThreadView;
use agent_core::view::{
    time::upcoming_timestamp,
    timeline::{
        rows::{FoldKind, FoldRow, TimelineRow, WorkToggleRow},
        timing::working_row_label,
        work_row::{
            ProviderFailureRow, WorkActivityDetail, WorkActivityRow, WorkIcon, WorkIconTone,
            WorkLabelTone, WorkLogRow, WorkRowIcon, WorkRowRole,
        },
    },
    work_log::ToolIcon,
    working_status::FloatingWorkingStatus,
};
use chrono::Local;
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        popover::Popover,
        spinner::Spinner,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::sync::Arc;

/// The lucide glyph of a work-log icon.
fn work_icon_name(icon: &WorkRowIcon) -> &'static str {
    match icon {
        WorkRowIcon::Brain => "brain",
        WorkRowIcon::Logo(_) => "message-square",
        WorkRowIcon::Feed(icon) => match icon {
            WorkIcon::Agent => "bot",
            WorkIcon::Alert => "circle-alert",
            WorkIcon::Browser | WorkIcon::Globe => "globe",
            WorkIcon::Computer => "monitor",
            WorkIcon::Check => "check",
            WorkIcon::Command => "terminal",
            WorkIcon::Edit => "square-pen",
            WorkIcon::Eye => "eye",
            WorkIcon::Search => "search",
            WorkIcon::Hammer => "hammer",
            WorkIcon::Lock => "lock",
            WorkIcon::Message => "message-circle",
            WorkIcon::Warning => "triangle-alert",
            WorkIcon::Wrench => "wrench",
            WorkIcon::Zap => "zap",
        },
    }
}

/// A 16px work-log icon: the tool's own picture when it has one.
pub(super) fn work_icon(
    icon_kind: &WorkRowIcon,
    tool: Option<&ToolIcon>,
    tint_color: Hsla,
) -> AnyElement {
    let dark = super::super::ui::is_dark();
    let picture = tool.and_then(|tool| match tool {
        ToolIcon::Website {
            favicon_url,
            favicon_url_dark,
            ..
        } => if dark { favicon_url_dark.clone() } else { None }.or_else(|| favicon_url.clone()),
        ToolIcon::ThemedLogo {
            logo_url,
            logo_url_dark,
        } => Some(
            if dark { logo_url_dark.clone() } else { None }.unwrap_or_else(|| logo_url.clone()),
        ),
        ToolIcon::NativeApp(_) => None,
    });
    match picture {
        Some(url) => img(SharedString::from(url))
            .size(px(16.))
            .rounded(px(3.))
            .into_any_element(),
        None => icon(work_icon_name(icon_kind))
            .size(px(16.))
            .text_color(tint_color)
            .into_any_element(),
    }
}

fn icon_tone(tone: WorkIconTone) -> Hsla {
    match tone {
        WorkIconTone::Default => color("iconMuted"),
        WorkIconTone::Warning => color("warning"),
        WorkIconTone::Destructive => color("error"),
        WorkIconTone::Failed => tint("error", 0.4),
    }
}

/// One work-log line: a 24px icon slot, the label and trailing controls.
fn work_line() -> Div {
    h_flex()
        .min_h(px(24.))
        .min_w_0()
        .gap(px(6.))
        .text_sm()
        .line_height(px(22.75))
}

fn icon_slot(child: impl IntoElement) -> Div {
    h_flex()
        .flex_none()
        .size(px(24.))
        .justify_center()
        .child(child)
}

/// The indented, scrolling panel under an opened call.
fn render_detail(id: &str, detail: &WorkActivityDetail, cx: &mut Context<Desktop>) -> AnyElement {
    let mono = mono(cx);
    let pre = |text: String, max: f32, tone: Hsla, suffix: &str| {
        div()
            .id(SharedString::from(format!("{id}-{suffix}")))
            .max_h(px(max))
            .overflow_y_scroll()
            .font_family(mono.clone())
            .text_size(px(super::super::ui::metrics().code_size))
            .line_height(px(20.))
            .text_color(tone)
            .child(text)
    };
    if let Some(reasoning) = &detail.reasoning {
        return div()
            .id(SharedString::from(format!("{id}-reasoning")))
            .ml(px(28.))
            .max_h(px(384.))
            .overflow_y_scroll()
            .px(px(2.))
            .py_1()
            .text_sm()
            .text_color(color("text"))
            .child(chat_markdown(
                SharedString::from(format!("{id}-reasoning-text")),
                reasoning.clone(),
                Arc::default(),
                cx,
            ))
            .into_any_element();
    }
    v_flex()
        .ml(px(28.))
        .mt(px(2.))
        .mb(px(6.))
        .gap_2()
        .text_xs()
        .when_some(detail.call.as_ref(), |panel, call| {
            panel.child(
                v_flex()
                    .gap(px(6.))
                    .font_family(mono.clone())
                    .text_color(tint("text", 0.85))
                    .when_some(call.command.clone(), |lines, command| lines.child(command))
                    .when_some(call.args.clone(), |lines, args| {
                        lines.children(args.into_iter().map(|arg| {
                            h_flex()
                                .gap_1()
                                .child(div().text_color(color("textMuted")).child(arg.key))
                                .child(arg.value)
                        }))
                    })
                    .when_some(call.args_text.clone(), |lines, text| lines.child(text))
                    .when_some(detail.failed_exit_code, |lines, code| {
                        lines.child(
                            div()
                                .text_color(color("error"))
                                .child(format!("exit {code}")),
                        )
                    }),
            )
        })
        .when_some(detail.full_detail.clone(), |panel, text| {
            panel.child(pre(text, 256., color("secondaryLabel"), "detail"))
        })
        .when_some(detail.output.clone(), |panel, text| {
            panel.child(pre(text, 320., color("textMuted"), "output"))
        })
        .when_some(detail.viewed_image_path.clone(), |panel, path| {
            panel.child(div().text_color(color("textMuted")).child(path))
        })
        .into_any_element()
}

/// A failure that ended the turn: red, or amber for a usage limit.
fn render_failure(
    id: &str,
    failure: &ProviderFailureRow,
    format: agent_core::view::time::TimestampFormat,
    timestamp: Option<AnyElement>,
    cx: &mut Context<Desktop>,
) -> AnyElement {
    let retry = failure.retry_preparation.as_ref().map(|run| {
        let run_id = run.to_string();
        div().ml(px(28.)).pb_1().child(
            Button::new(SharedString::from(format!("retry-preparation-{id}")))
                .outline()
                .xsmall()
                .icon(icon("rotate-ccw"))
                .label("Retry")
                .on_click(cx.listener(move |view, _, _, _| {
                    view.perform(Intent::RetryPreparation {
                        run_id: run_id.clone(),
                    })
                })),
        )
    });
    let tone = if failure.warning {
        color("warning")
    } else {
        color("error")
    };
    let reset = failure
        .reset_at
        .as_ref()
        .and_then(local_time)
        .map(|reset| upcoming_timestamp(&reset, &Local::now(), format));
    v_flex()
        .id(SharedString::from(format!("failure-{id}")))
        .w_full()
        .px(px(2.))
        .py(px(2.))
        .child(
            work_line()
                .child(icon_slot(
                    icon("circle-alert").size(px(16.)).text_color(tone),
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tone)
                        .child(failure.label(reset.as_deref())),
                )
                .children(timestamp),
        )
        .when(!failure.warning, |row| {
            row.child(
                div()
                    .ml(px(28.))
                    .py_1()
                    .text_sm()
                    .line_height(px(22.75))
                    .text_color(tint("text", 0.8))
                    .child(failure.message.clone()),
            )
        })
        .children(retry)
        .into_any_element()
}

/// The 2.2 s sweep over a live row.
fn live_label(id: &str, label: &str, active: bool) -> AnyElement {
    shimmer_label(
        SharedString::from(format!("{id}-shine")),
        label.to_owned(),
        active,
    )
}

impl Desktop {
    fn render_activity(
        &mut self,
        row: &TimelineRow,
        activity: &WorkActivityRow,
        grouped: bool,
        timestamped: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = activity.id.clone();
        let group = SharedString::from(format!("work-{id}"));
        let (label_color, weight) = match activity.label_tone {
            WorkLabelTone::Default => (color("secondaryLabel"), FontWeight::NORMAL),
            WorkLabelTone::Warning => (color("warning"), FontWeight::MEDIUM),
            WorkLabelTone::Danger => (color("error"), FontWeight::MEDIUM),
        };
        let open = activity
            .opens_thread
            .as_ref()
            .zip(activity.open_label.clone())
            .map(|(thread, label)| {
                let thread = thread.to_string();
                div()
                    .id(SharedString::from(format!("{id}-open")))
                    .flex_none()
                    .cursor_pointer()
                    .text_color(color("accent"))
                    .hover(|style| style.underline())
                    .child(label)
                    .on_click(cx.listener(move |view, _, _, cx| {
                        cx.stop_propagation();
                        view.open_thread(thread.clone(), cx);
                    }))
            });
        let timestamp = timestamped
            .then(|| {
                self.render_timestamp(
                    SharedString::from(format!("{id}-time")),
                    row.created_at.as_ref(),
                    Some(group.clone()),
                )
            })
            .flatten();
        let toggle_id = id.clone();
        let link = (activity.role == WorkRowRole::Link)
            .then(|| activity.opens_thread.as_ref().map(ToString::to_string))
            .flatten();
        let line = work_line()
            .id(SharedString::from(format!("{id}-line")))
            .when(activity.can_expand || link.is_some(), |line| {
                line.cursor_pointer()
            })
            .child(icon_slot(work_icon(
                &activity.icon,
                activity.tool_icon.as_ref(),
                icon_tone(activity.icon_tone),
            )))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.))
                    .child(
                        div()
                            .min_w_0()
                            .when(activity.answer_preview.is_none(), |label| label.flex_1())
                            .truncate()
                            .font_weight(weight)
                            .text_color(label_color)
                            .child(live_label(&id, &activity.label, activity.shimmer)),
                    )
                    .when_some(activity.answer_preview.clone(), |label, answer| {
                        label.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(if activity.answer_highlighted {
                                    color("text")
                                } else {
                                    color("textMuted")
                                })
                                .child(answer),
                        )
                    }),
            )
            .children(open)
            .when(activity.failure_mark, |line| {
                line.child(icon("x").size(px(12.)).text_color(tint("error", 0.4)))
            })
            .children(timestamp)
            .child(
                h_flex()
                    .flex_none()
                    .size(px(16.))
                    .justify_center()
                    .opacity(if activity.can_expand { 0.7 } else { 0. })
                    .child(chevron(activity.expanded)),
            )
            .when(activity.can_expand || link.is_some(), |line| {
                line.on_click(cx.listener(move |view, _, _, cx| match &link {
                    Some(thread) => view.open_thread(thread.clone(), cx),
                    None => view.toggle_disclosure(|d| &mut d.expanded_entries, &toggle_id, cx),
                }))
            });
        v_flex()
            .group(group)
            .w_full()
            .min_w_0()
            .rounded(px(8.))
            .px(px(2.))
            .when(!grouped, |row| row.py(px(2.)))
            .child(line)
            .when_some(activity.detail.as_ref(), |row, detail| {
                row.child(render_detail(&id, detail, cx))
            })
            .into_any_element()
    }

    fn render_work_entry(
        &mut self,
        row: &TimelineRow,
        entry: &WorkLogRow,
        grouped: bool,
        timestamped: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match entry {
            WorkLogRow::Activity(activity) => {
                self.render_activity(row, activity, grouped, timestamped, cx)
            }
            WorkLogRow::ProviderFailure(failure) => {
                let timestamp = self.render_timestamp(
                    SharedString::from(format!("{}-failure-time", row.id)),
                    Some(&failure.created_at),
                    None,
                );
                render_failure(&row.id, failure, self.timestamp_format(), timestamp, cx)
            }
        }
    }

    pub(super) fn render_work(
        &mut self,
        row: &TimelineRow,
        entries: &[WorkLogRow],
        expanded_group: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let timestamped = entries.len() == 1;
        let items: Vec<AnyElement> = entries
            .iter()
            .map(|entry| self.render_work_entry(row, entry, expanded_group, timestamped, cx))
            .collect();
        if !expanded_group {
            return v_flex().w_full().children(items).into_any_element();
        }
        let opened = entries
            .iter()
            .any(|entry| matches!(entry, WorkLogRow::Activity(activity) if activity.expanded));
        div()
            .id(SharedString::from(format!("{}-group", row.id)))
            .w_full()
            .when(!opened, |group| group.max_h(px(288.)))
            .overflow_y_scroll()
            .rounded(px(8.))
            .child(v_flex().w_full().children(items))
            .into_any_element()
    }

    /// The active turn's latest call; opens the calls behind it.
    pub(super) fn render_live_work(
        &mut self,
        row: &TimelineRow,
        label: &str,
        entry: &WorkLogRow,
        group_id: &str,
        active: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (icon_element, failed, mark) = match entry {
            WorkLogRow::Activity(activity) => (
                work_icon(
                    &activity.icon,
                    activity.tool_icon.as_ref(),
                    if activity.failed {
                        tint("error", 0.4)
                    } else if active {
                        color("text")
                    } else {
                        color("iconMuted")
                    },
                ),
                activity.failed,
                activity.failure_mark,
            ),
            WorkLogRow::ProviderFailure(failure) => (
                icon("circle-alert")
                    .size(px(16.))
                    .text_color(if failure.warning {
                        color("warning")
                    } else {
                        color("error")
                    })
                    .into_any_element(),
                true,
                false,
            ),
        };
        let group = group_id.to_owned();
        work_line()
            .id(SharedString::from(format!("{}-live", row.id)))
            .w_full()
            .rounded(px(8.))
            .cursor_pointer()
            .child(icon_slot(icon_element))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(color("secondaryLabel"))
                    .child(live_label(&row.id, label, active && !failed)),
            )
            .when(mark, |line| {
                line.child(icon("x").size(px(12.)).text_color(tint("error", 0.4)))
            })
            .on_click(cx.listener(move |view, _, _, cx| {
                view.toggle_disclosure(|d| &mut d.expanded_work_groups, &group, cx)
            }))
            .into_any_element()
    }

    /// The header of a group of calls: its summary, opening the calls.
    pub(super) fn render_work_toggle(
        &mut self,
        row: &TimelineRow,
        toggle: &WorkToggleRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group = SharedString::from(format!("toggle-{}", row.id));
        let group_id = toggle.group_id.clone();
        work_line()
            .id(SharedString::from(format!("{}-toggle", row.id)))
            .group(group.clone())
            .w_full()
            .rounded(px(8.))
            .px(px(2.))
            .py(px(2.))
            .cursor_pointer()
            .child(icon_slot(work_icon(
                &toggle.icon,
                toggle.tool_icon.as_ref(),
                color("iconMuted"),
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(color("secondaryLabel"))
                    .child(live_label(
                        &row.id,
                        &toggle.summary,
                        toggle.live && !toggle.has_failure,
                    )),
            )
            .children(self.render_timestamp(
                SharedString::from(format!("{}-time", row.id)),
                row.created_at.as_ref(),
                Some(group),
            ))
            .on_click(cx.listener(move |view, _, _, cx| {
                view.toggle_disclosure(|d| &mut d.expanded_work_groups, &group_id, cx)
            }))
            .into_any_element()
    }

    pub(super) fn render_thinking(
        &mut self,
        thread: &ThreadView,
        row: &TimelineRow,
        group_id: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if thread.setup.preparing_worktree || compacting(thread) {
            return work_line().into_any_element();
        }
        let line = work_line()
            .id(SharedString::from(format!("{}-thinking", row.id)))
            .w_full()
            .rounded(px(8.))
            .child(icon_slot(
                icon("brain").size(px(16.)).text_color(color("iconMuted")),
            ))
            .child(
                div()
                    .text_color(color("secondaryLabel"))
                    .child(live_label(&row.id, "Thinking", true)),
            );
        match group_id {
            Some(group_id) => {
                let group_id = group_id.to_owned();
                line.cursor_pointer()
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.toggle_disclosure(|d| &mut d.expanded_work_groups, &group_id, cx)
                    }))
                    .into_any_element()
            }
            None => line.into_any_element(),
        }
    }

    /// "Working for 12s", with a chip while the setup script still runs.
    pub(super) fn render_working(
        &mut self,
        thread: &ThreadView,
        row: &TimelineRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let preparing = thread.setup.preparing_worktree;
        let compacting = compacting(thread);
        let label = working_row_label(
            row.created_at.as_ref(),
            super::super::ui::now_ms(),
            preparing,
            compacting,
        );
        let background = thread
            .setup
            .card
            .clone()
            .filter(|card| card.background_script.is_some());
        let owner = cx.entity().downgrade();
        let mono = mono(cx);
        div()
            .border_b_1()
            .border_color(tint("border", 0.6))
            .pb_2()
            .pt_1()
            .child(
                h_flex()
                    .h(px(24.))
                    .min_w_0()
                    .gap_2()
                    .px_1()
                    .text_sm()
                    .text_color(color("textMuted"))
                    .child(
                        h_flex()
                            .flex_none()
                            .gap(px(6.))
                            .when(compacting && !preparing, |label| {
                                label.child(icon("minimize-2").size(px(12.)))
                            })
                            .child(live_label(&row.id, &label, preparing || compacting)),
                    )
                    .when_some(background, |header, card| {
                        let script = card.background_script.clone().unwrap_or_default();
                        let details = self.timeline.setup_details;
                        header.child(
                            div().ml_auto().child(
                                Popover::new("setup-background")
                                    .trigger(
                                        Button::new("setup-background-chip")
                                            .ghost()
                                            .xsmall()
                                            .child(
                                                h_flex()
                                                    .gap_1()
                                                    .child(Spinner::new().xsmall())
                                                    .child(div().truncate().child(script.clone())),
                                            )
                                            .accessibility_label(format!(
                                                "{script} is still running. Show setup progress."
                                            )),
                                    )
                                    .content(move |_, _, _| {
                                        div().w(px(480.)).child(render_setup_card(
                                            &card,
                                            true,
                                            details,
                                            owner.clone(),
                                            mono.clone(),
                                        ))
                                    }),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    pub(super) fn render_fold(
        &mut self,
        row: &TimelineRow,
        fold: &FoldRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match fold.kind {
            FoldKind::Turn => {
                let group = SharedString::from(format!("fold-{}", row.id));
                let run = fold.run.to_string();
                h_flex()
                    .group(group.clone())
                    .gap_1()
                    .border_b_1()
                    .border_color(tint("border", 0.6))
                    .pb_2()
                    .pr(px(2.))
                    .pt_1()
                    .child(
                        h_flex()
                            .id(SharedString::from(format!("{}-fold", row.id)))
                            .gap_1()
                            .rounded(px(8.))
                            .px_1()
                            .text_sm()
                            .line_height(px(22.75))
                            .text_color(color("textMuted"))
                            .cursor_pointer()
                            .hover(|style| style.text_color(color("text")))
                            .child(fold.label.clone())
                            .child(
                                icon(if fold.expanded {
                                    "chevron-down"
                                } else {
                                    "chevron-right"
                                })
                                .size(px(14.)),
                            )
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.toggle_disclosure(|d| &mut d.expanded_runs, &run, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .children(self.render_timestamp(
                        SharedString::from(format!("{}-time", row.id)),
                        row.created_at.as_ref(),
                        Some(group),
                    ))
                    .into_any_element()
            }
            FoldKind::Attempt => {
                let attempt = fold
                    .attempt
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                h_flex()
                    .id(SharedString::from(format!("{}-attempt", row.id)))
                    .w_full()
                    .gap_2()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(tint("border", 0.6))
                    .bg(tint("muted", 0.2))
                    .px(px(10.))
                    .py_2()
                    .cursor_pointer()
                    .hover(|style| style.bg(tint("muted", 0.35)))
                    .child(
                        icon(if fold.expanded {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        })
                        .size(px(14.))
                        .text_color(color("textMuted")),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(tint("text", 0.8))
                            .child(fold.label.clone()),
                    )
                    .child(super::text_2xs(
                        div()
                            .text_color(color("textMuted"))
                            .child("Partial output retained"),
                    ))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.toggle_disclosure(|d| &mut d.expanded_attempts, &attempt, cx)
                    }))
                    .into_any_element()
            }
        }
    }
}

/// The thread is compacting its context, as the working control reports.
fn compacting(thread: &ThreadView) -> bool {
    thread
        .working
        .as_ref()
        .is_some_and(|working| working.status == Some(FloatingWorkingStatus::Compacting))
}

/// A line across the column naming a compaction, shining while it runs.
pub(super) fn render_compaction(id: &str, label: &str, active: bool) -> AnyElement {
    let line = || div().h(px(1.)).flex_1().bg(tint("border", 0.7));
    h_flex()
        .w_full()
        .gap_3()
        .py_1()
        .text_xs()
        .text_color(color("textMuted"))
        .child(line())
        .child(
            h_flex()
                .flex_none()
                .gap(px(6.))
                .child(icon("minimize-2").size(px(12.)))
                .child(live_label(id, label, active)),
        )
        .child(line())
        .into_any_element()
}

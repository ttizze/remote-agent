//! Lifecycle dividers, subagent cards, context handoffs, proposed plans and
//! the worktree setup card.
use super::{
    Desktop, color, icon, local_time, markdown::chat_markdown, mono, shimmer_label, text_2xs,
    text_3xs, tint,
};
use agent_core::{
    state::Intent,
    view::{
        models::catalog,
        setup_card::{
            CardContext, SetupCardLayout, SetupCardView, SetupPhase, SetupStageStatus, SetupTone,
            setup_card,
        },
        thread::ThreadView,
        time::time_of_day,
        timeline::{
            lifecycle::{
                CreatedThreadLayout, CreatedThreadRow, DividerIcon, DividerTone, HandoffDivider,
                HandoffEndpoint, LifecycleRow, SubagentDot, SubagentLink, SystemDivider,
                present_handoff_endpoint, subagent_status_visual,
            },
            mobile_work_log::{
                SubagentGroupCard, SubagentGroupTone, SubagentTiming, subagent_card_elapsed,
            },
            plan_card::PlanCard,
            rows::TimelineRow,
        },
    },
};
use agent_domain::{Driver, ItemStatus, WorktreeSetupSnapshot};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        spinner::Spinner,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::sync::Arc;

/// A boxed click listener.
type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

fn divider_icon(icon_kind: DividerIcon) -> &'static str {
    match icon_kind {
        DividerIcon::Stop => "x",
        DividerIcon::Compaction => "minus",
        DividerIcon::Handoff => "arrow-right-left",
        DividerIcon::Fork => "git-fork",
    }
}

/// A centered label between two rules; with an action it is a pill button.
fn system_divider(
    id: &str,
    label: &str,
    icon_kind: DividerIcon,
    tone: DividerTone,
    detail: Option<AnyElement>,
    action: Option<(String, ClickHandler)>,
) -> AnyElement {
    let rule = || div().h(px(1.)).flex_1().bg(tint("border", 0.7));
    let content = |pill: Stateful<Div>| {
        pill.gap(px(6.))
            .child(icon(divider_icon(icon_kind)).size(px(12.)))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(label.to_owned()),
            )
            .children(detail)
    };
    let pill = h_flex().id(SharedString::from(format!("divider-{id}")));
    let pill = match action {
        Some((label, on_click)) => {
            let tooltip = SharedString::from(label);
            content(pill)
                .rounded_full()
                .border_1()
                .border_color(tint("border", 0.7))
                .bg(color("canvas"))
                .px(px(10.))
                .py_1()
                .cursor_pointer()
                .hover(|style| style.bg(color("muted")))
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .on_click(on_click)
        }
        None => content(pill)
            .flex_wrap()
            .justify_center()
            .rounded_full()
            .px_2()
            .py_1(),
    };
    text_2xs(
        h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .py_2()
            .text_color(match tone {
                DividerTone::Neutral => color("textMuted"),
                DividerTone::Danger => color("error"),
            }),
    )
    .child(rule())
    .child(pill)
    .child(rule())
    .into_any_element()
}

/// The text after a divider's label: "· detail", at most 320px wide.
fn divider_detail(detail: String) -> AnyElement {
    div()
        .max_w(px(320.))
        .min_w_0()
        .truncate()
        .opacity(0.7)
        .child(format!("·\u{a0}{detail}"))
        .into_any_element()
}

fn dot_color(dot: SubagentDot) -> Hsla {
    match dot {
        SubagentDot::Info => color("update"),
        SubagentDot::Success => color("successForeground"),
        SubagentDot::Destructive => color("error"),
        SubagentDot::Muted => tint("textMuted", 0.6),
    }
}

/// A round provider tile; the status dot sits on its corner.
fn subagent_avatar(driver: Option<Driver>, status: Option<ItemStatus>) -> Div {
    div()
        .relative()
        .flex_none()
        .size(px(24.))
        .rounded_full()
        .border_1()
        .border_color(tint("border", 0.7))
        .bg(color("muted"))
        .flex()
        .items_center()
        .justify_center()
        .child(match driver {
            Some(driver) => super::super::ui::driver_icon(driver).size(px(14.)),
            None => icon("bot").size(px(14.)).text_color(color("textMuted")),
        })
        .when_some(status, |avatar, status| {
            avatar.child(
                div()
                    .absolute()
                    .right(px(-1.))
                    .bottom(px(-1.))
                    .size(px(8.))
                    .rounded_full()
                    .border_2()
                    .border_color(color("canvas"))
                    .bg(dot_color(subagent_status_visual(status).dot)),
            )
        })
}

/// One worktree setup stage's glyph.
fn stage_icon(status: SetupStageStatus) -> AnyElement {
    let name = match status {
        SetupStageStatus::Running => return Spinner::new().small().into_any_element(),
        SetupStageStatus::Done => "check",
        SetupStageStatus::Failed => "x",
        SetupStageStatus::Warning => "circle-alert",
        SetupStageStatus::Skipped => "minus",
        SetupStageStatus::Pending => "circle",
    };
    icon(name)
        .size(px(16.))
        .text_color(color("iconMuted"))
        .into_any_element()
}

fn stage_tone(status: SetupStageStatus) -> Hsla {
    match status {
        SetupStageStatus::Failed => color("errorForeground"),
        SetupStageStatus::Warning => color("warningForeground"),
        _ => color("secondaryLabel"),
    }
}

/// One setup line in the work-log geometry.
fn setup_line(status: SetupStageStatus, label: AnyElement) -> Div {
    h_flex()
        .min_h(px(24.))
        .min_w_0()
        .gap(px(6.))
        .px(px(2.))
        .text_sm()
        .child(
            h_flex()
                .flex_none()
                .size(px(24.))
                .justify_center()
                .when(status == SetupStageStatus::Pending, |slot| {
                    slot.opacity(0.4)
                })
                .child(stage_icon(status)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(stage_tone(status))
                .when(status == SetupStageStatus::Pending, |label| {
                    label.opacity(0.4)
                })
                .child(label),
        )
}

fn elapsed_label(elapsed: Option<String>) -> Option<Div> {
    elapsed.map(|elapsed| {
        div()
            .flex_none()
            .text_xs()
            .text_color(color("textMuted"))
            .child(elapsed)
    })
}

/// The worktree setup: stages with the script's output, details and the
/// actions the setup still allows.
pub(super) fn render_setup_card(
    card: &SetupCardView,
    embedded: bool,
    details_open: bool,
    owner: WeakEntity<Desktop>,
    mono: SharedString,
) -> AnyElement {
    let running = card.phase == SetupPhase::Running;
    let header = card.show_header.then(|| {
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
                    .text_color(match card.tone {
                        SetupTone::Destructive => color("errorForeground"),
                        SetupTone::Warning => color("warningForeground"),
                        SetupTone::Muted => color("textMuted"),
                    })
                    .child(div().min_w_0().truncate().child(shimmer_label(
                        "setup-title",
                        card.title.clone(),
                        running,
                    )))
                    .children(elapsed_label(card.elapsed.clone()).map(|elapsed| elapsed.ml_auto())),
            )
    });
    let stages = card.show_stages.then(|| {
        v_flex()
            .when(card.show_header, |stages| stages.pt(px(6.)))
            .children(card.stages.iter().enumerate().map(|(index, stage)| {
                let label = shimmer_label(
                    SharedString::from(format!("setup-stage-{index}")),
                    stage.label.clone(),
                    stage.status == SetupStageStatus::Running,
                );
                v_flex()
                    .child(
                        setup_line(stage.status, label)
                            .when_some(stage.trailing.clone(), |line, trailing| {
                                line.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_xs()
                                        .text_color(color("textMuted"))
                                        .child(trailing),
                                )
                            })
                            .children(elapsed_label(stage.elapsed.clone())),
                    )
                    .when_some(stage.output.clone(), |stage, output| {
                        stage.child(
                            text_2xs(
                                v_flex()
                                    .mb_1()
                                    .ml(px(32.))
                                    .overflow_hidden()
                                    .rounded(px(8.))
                                    .border_1()
                                    .px(px(10.))
                                    .py(px(6.))
                                    .font_family(mono.clone())
                                    .line_height(px(18.)),
                            )
                            .when(output.failed, |tail| {
                                tail.border_color(tint("error", 0.2))
                                    .bg(color("errorSurface"))
                                    .text_color(color("errorForeground"))
                            })
                            .when(!output.failed, |tail| {
                                tail.border_color(color("border"))
                                    .bg(color("codeBackground"))
                                    .text_color(color("textMuted"))
                            })
                            .children(output.lines.into_iter().map(|line| {
                                div()
                                    .truncate()
                                    .whitespace_nowrap()
                                    .child(if line.is_empty() {
                                        "\u{a0}".to_owned()
                                    } else {
                                        line
                                    })
                            })),
                        )
                    })
            }))
    });
    let summary = card.summary.as_ref().map(|summary| {
        setup_line(
            summary.status,
            div().child(summary.label.clone()).into_any_element(),
        )
        .children(elapsed_label(summary.elapsed.clone()))
    });
    let details = details_open.then(|| {
        v_flex()
            .mt_1()
            .mb(px(6.))
            .ml(px(32.))
            .gap(px(2.))
            .text_xs()
            .text_color(color("textMuted"))
            .children(card.details.iter().map(|detail| {
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .w(px(48.))
                            .flex_none()
                            .text_color(tint("text", 0.8))
                            .child(detail.label.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(mono.clone())
                            .child(detail.value.clone()),
                    )
            }))
    });
    let toggle_details = {
        let owner = owner.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = owner.update(cx, |view, cx| {
                view.timeline.setup_details = !view.timeline.setup_details;
                cx.notify();
            });
        }
    };
    let terminal = card.open_terminal_id.clone().map(|terminal| {
        let owner = owner.clone();
        let thread = card.thread_id.clone();
        Button::new("setup-open-terminal")
            .icon(icon("terminal"))
            .label("Open terminal")
            .ghost()
            .xsmall()
            .on_click(move |_, window, cx| {
                let _ = owner.update(cx, |view, cx| {
                    view.open_thread_terminal(thread.clone(), terminal.clone(), window, cx)
                });
            })
    });
    let work_locally = (card.can_work_locally && !embedded).then(|| {
        let owner = owner.clone();
        Button::new("setup-work-locally")
            .icon(icon("laptop"))
            .label("Work locally")
            .ghost()
            .xsmall()
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |view, _| view.perform(Intent::WorkLocally));
            })
    });
    let cancel = (card.can_cancel && !embedded).then(|| {
        Button::new("setup-cancel")
            .icon(icon("x"))
            .label("Cancel")
            .ghost()
            .xsmall()
            .on_click(move |_, _, cx| {
                let _ = owner.update(cx, |view, _| view.perform(Intent::CancelSetup));
            })
    });
    v_flex()
        .w_full()
        .children(header)
        .children(summary)
        .children(stages)
        .when_some(card.error.clone(), |setup, error| {
            setup.child(
                div()
                    .mt_1()
                    .ml(px(32.))
                    .text_xs()
                    .text_color(color("textMuted"))
                    .child(error),
            )
        })
        .children(details)
        .child(
            h_flex()
                .mt(px(2.))
                .ml(px(19.))
                .flex_wrap()
                .gap(px(2.))
                .child(
                    Button::new("setup-details")
                        .icon(icon(if details_open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        }))
                        .label("Details")
                        .ghost()
                        .xsmall()
                        .on_click(toggle_details),
                )
                .children(terminal)
                .children(work_locally)
                .children(cancel),
        )
        .into_any_element()
}

impl Desktop {
    pub(super) fn render_lifecycle(
        &mut self,
        row: &TimelineRow,
        lifecycle: &LifecycleRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match lifecycle {
            LifecycleRow::InterruptRequest(request) => {
                let time = local_time(&request.created_at)
                    .map(|time| time_of_day(&time, self.timestamp_format()));
                h_flex()
                    .justify_end()
                    .px_1()
                    .py_1()
                    .child(
                        h_flex()
                            .max_w(relative(0.8))
                            .gap_2()
                            .text_xs()
                            .text_color(color("error"))
                            .child(div().font_family(mono(cx)).child("■"))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(request.label.clone()),
                            )
                            .child(div().opacity(0.5).child("·"))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(request.message.clone()),
                            )
                            .when_some(time, |line, time| {
                                line.child(text_3xs(
                                    div().text_color(color("textMuted")).child(time),
                                ))
                            }),
                    )
                    .into_any_element()
            }
            LifecycleRow::Divider(divider) => self.render_system_divider(&row.id, divider, cx),
            LifecycleRow::CreatedThread(created) => {
                self.render_created_thread(&row.id, created, cx)
            }
            LifecycleRow::Subagent(link) => {
                let elapsed = self.subagent_elapsed(link);
                self.render_subagent_link(&row.id, link, elapsed, cx)
            }
        }
    }

    fn render_system_divider(
        &mut self,
        id: &str,
        divider: &SystemDivider,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let action = divider.action.clone().map(|action| {
            let thread = action.thread.to_string();
            let on_click: ClickHandler =
                Box::new(cx.listener(move |view, _, _, cx| view.open_thread(thread.clone(), cx)));
            (action.label, on_click)
        });
        system_divider(
            id,
            &divider.label,
            divider.icon,
            divider.tone,
            divider.detail.clone().map(divider_detail),
            action,
        )
    }

    fn render_created_thread(
        &mut self,
        id: &str,
        created: &CreatedThreadRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let thread = created.thread.to_string();
        let open = cx.listener(move |view, _, _, cx| view.open_thread(thread.clone(), cx));
        match created.layout {
            CreatedThreadLayout::ResourceCard => h_flex()
                .min_w_0()
                .gap_3()
                .rounded(px(10.))
                .border_1()
                .border_color(tint("border", 0.6))
                .p_3()
                .child(
                    h_flex()
                        .flex_none()
                        .size(px(36.))
                        .justify_center()
                        .rounded(px(10.))
                        .bg(tint("muted", 0.6))
                        .child(
                            icon("message-square")
                                .size(px(16.))
                                .text_color(color("secondaryLabel")),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(created.label.clone()),
                )
                .child(
                    Button::new(SharedString::from(format!("created-{id}")))
                        .label(created.action_label.clone())
                        .outline()
                        .xsmall()
                        .accessibility_label(created.accessibility_label.clone())
                        .on_click(open),
                )
                .into_any_element(),
            CreatedThreadLayout::WorkLogRow => h_flex()
                .min_h(px(24.))
                .min_w_0()
                .gap(px(6.))
                .px(px(2.))
                .py(px(2.))
                .text_sm()
                .child(
                    h_flex().flex_none().size(px(24.)).justify_center().child(
                        icon("message-square")
                            .size(px(16.))
                            .text_color(color("iconMuted")),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(color("secondaryLabel"))
                        .child(created.label.clone()),
                )
                .child(
                    div()
                        .id(SharedString::from(format!("created-{id}")))
                        .flex_none()
                        .cursor_pointer()
                        .text_color(color("textMuted"))
                        .hover(|style| style.text_color(color("text")).underline())
                        .child(created.action_label.clone())
                        .on_click(open),
                )
                .into_any_element(),
        }
    }

    /// When a subagent reported, or how long it has worked.
    fn subagent_elapsed(&self, link: &SubagentLink) -> Option<String> {
        if let Some(event) = &link.event_at {
            return local_time(event).map(|time| time_of_day(&time, self.timestamp_format()));
        }
        subagent_card_elapsed(
            &[SubagentTiming {
                status: link.live_status,
                started_at: link.started_at.clone(),
                completed_at: link.completed_at.clone(),
            }],
            super::super::ui::now_ms(),
        )
    }

    fn render_subagent_link(
        &mut self,
        key: &str,
        link: &SubagentLink,
        elapsed: Option<String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail_color = if link.failed {
            color("error")
        } else {
            color("textMuted")
        };
        let entry = h_flex()
            .id(SharedString::from(format!("subagent-{key}")))
            .w_full()
            .min_w_0()
            .gap(px(10.))
            .rounded(px(8.))
            .px_2()
            .py(px(6.))
            .child(subagent_avatar(link.driver, link.status))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(link.title.clone()),
                            )
                            .when(link.status_label_beside_title, |title| {
                                title.child(text_3xs(
                                    div()
                                        .flex_none()
                                        .text_color(detail_color)
                                        .child(link.status_label.clone()),
                                ))
                            }),
                    )
                    .child(text_2xs(
                        div()
                            .truncate()
                            .line_height(px(18.))
                            .text_color(detail_color)
                            .child(
                                link.detail
                                    .clone()
                                    .unwrap_or_else(|| link.status_label.clone()),
                            ),
                    )),
            )
            .children(elapsed.map(|elapsed| {
                text_3xs(
                    div()
                        .flex_none()
                        .font_family(mono(cx))
                        .text_color(tint("textMuted", 0.8))
                        .child(elapsed),
                )
            }));
        match link.thread.as_ref().map(ToString::to_string) {
            Some(thread) => entry
                .cursor_pointer()
                .hover(|style| style.bg(tint("accentSurface", 0.2)))
                .child(
                    icon("chevron-right")
                        .size(px(14.))
                        .text_color(tint("textMuted", 0.6)),
                )
                .on_click(cx.listener(move |view, _, _, cx| view.open_thread(thread.clone(), cx)))
                .into_any_element(),
            None => entry.into_any_element(),
        }
    }

    pub(super) fn render_subagents(
        &mut self,
        row: &TimelineRow,
        card: &SubagentGroupCard,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut members: Vec<AnyElement> = vec![];
        if card.shows_members {
            for (index, member) in card.members.iter().enumerate() {
                members.push(self.render_subagent_link(
                    &format!("{}-{index}", row.id),
                    &member.link,
                    member.elapsed.clone(),
                    cx,
                ));
            }
        }
        if !card.grouped {
            return v_flex().children(members).into_any_element();
        }
        let group_id = card.group_id.clone();
        let lit = card.expanded || card.tone == SubagentGroupTone::Active;
        v_flex()
            .child(
                h_flex()
                    .id(SharedString::from(format!("{}-subagents", row.id)))
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .py_2()
                    .cursor_pointer()
                    .text_color(if lit {
                        color("text")
                    } else {
                        color("textMuted")
                    })
                    .when(!lit, |header| header.opacity(0.55))
                    .hover(|style| style.opacity(1.))
                    .child(
                        h_flex()
                            .flex_none()
                            .children(card.members.iter().take(3).enumerate().map(
                                |(index, member)| {
                                    subagent_avatar(member.link.driver, None)
                                        .when(index > 0, |avatar| avatar.ml(px(-6.)))
                                },
                            ))
                            .when(card.overflow > 0, |stack| {
                                stack.child(text_3xs(
                                    div()
                                        .ml(px(-6.))
                                        .size(px(24.))
                                        .rounded_full()
                                        .bg(color("muted"))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(color("textMuted"))
                                        .child(format!("+{}", card.overflow)),
                                ))
                            }),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(card.label.clone()),
                            )
                            .child(text_3xs(
                                div()
                                    .truncate()
                                    .text_color(match card.tone {
                                        SubagentGroupTone::Active => color("update"),
                                        SubagentGroupTone::Failed => color("error"),
                                        SubagentGroupTone::Default => color("textMuted"),
                                    })
                                    .child(card.summary.clone()),
                            )),
                    )
                    .children(card.elapsed.clone().map(|elapsed| {
                        text_3xs(
                            div()
                                .flex_none()
                                .font_family(mono(cx))
                                .text_color(color("textMuted"))
                                .child(elapsed),
                        )
                    }))
                    .child(
                        icon(if card.expanded {
                            "chevron-up"
                        } else {
                            "chevron-down"
                        })
                        .size(px(14.))
                        .text_color(color("textMuted")),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.toggle_disclosure(|d| &mut d.expanded_work_groups, &group_id, cx)
                    })),
            )
            .when(card.shows_members, |group| {
                group.child(
                    v_flex()
                        .mt_1()
                        .mb_1()
                        .rounded(px(10.))
                        .border_1()
                        .border_color(tint("border", 0.6))
                        .bg(tint("surface", 0.3))
                        .p_1()
                        .children(members),
                )
            })
            .into_any_element()
    }

    /// "Context handoff" with the providers and models it moved between.
    pub(super) fn render_handoff(&mut self, divider: &HandoffDivider) -> AnyElement {
        let catalog = catalog(&self.snapshot);
        let endpoint = |endpoint: &HandoffEndpoint| {
            let instance = catalog.instance(&endpoint.instance);
            let model = endpoint.model.as_deref().and_then(|slug| {
                catalog
                    .models_of(&endpoint.instance)
                    .find(|model| model.slug == slug.trim())
                    .map(|model| model.name.as_str())
            });
            let presentation = present_handoff_endpoint(
                endpoint,
                model,
                instance.map(|instance| instance.display_name.as_str()),
            );
            let tooltip = SharedString::from(presentation.tooltip);
            h_flex()
                .id(SharedString::from(format!(
                    "handoff-{}-{}",
                    divider.run, endpoint.instance
                )))
                .min_w_0()
                .gap_1()
                .when_some(instance.map(|instance| instance.driver), |label, driver| {
                    label.child(super::super::ui::driver_icon(driver).size(px(12.)))
                })
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(presentation.label),
                )
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        };
        let from = &divider.endpoints.from;
        let detail = h_flex()
            .min_w_0()
            .flex_wrap()
            .justify_center()
            .gap(px(6.))
            .children(from.iter().enumerate().map(|(index, from)| {
                h_flex()
                    .when(index > 0, |endpoint| {
                        endpoint.child(div().ml(px(-4.)).child(","))
                    })
                    .child(endpoint(from))
            }))
            .when(!from.is_empty(), |detail| {
                detail.child(icon("arrow-right").size(px(12.)))
            })
            .child(endpoint(&divider.endpoints.to))
            .into_any_element();
        system_divider(
            divider.run.as_str(),
            &divider.label,
            divider.icon,
            divider.tone,
            Some(detail),
            None,
        )
    }

    pub(super) fn render_plan(
        &mut self,
        row: &TimelineRow,
        plan: &PlanCard,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = plan.plan.to_string();
        let expanded = self.timeline.expanded_plans.contains(&id);
        let collapsed = plan.collapsed_preview.is_some() && !expanded;
        let copy_key = format!("plan-{id}");
        let copied = self.timeline.copied(&copy_key);
        let owner = cx.entity().downgrade();
        let (markdown, filename, export) = (
            plan.export_markdown.clone(),
            plan.filename.clone(),
            plan.export_markdown.clone(),
        );
        let menu = Button::new(SharedString::from(format!("plan-menu-{id}")))
            .icon(icon("ellipsis"))
            .outline()
            .xsmall()
            .accessibility_label("Plan actions")
            .dropdown_menu(move |menu, _, _| {
                let copy = {
                    let (owner, markdown, key) =
                        (owner.clone(), markdown.clone(), copy_key.clone());
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        cx.write_to_clipboard(ClipboardItem::new_string(markdown.clone()));
                        let _ = owner.update(cx, |view, cx| {
                            view.timeline.copied = Some((key.clone(), std::time::Instant::now()));
                            cx.notify();
                        });
                    }
                };
                let download = {
                    let (owner, filename, export) =
                        (owner.clone(), filename.clone(), export.clone());
                    move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                        let _ = owner.update(cx, |view, _| {
                            view.download_plan(filename.clone(), export.clone())
                        });
                    }
                };
                menu.item(
                    PopupMenuItem::new(if copied {
                        "Copied!"
                    } else {
                        "Copy to clipboard"
                    })
                    .on_click(copy),
                )
                .item(PopupMenuItem::new("Download as markdown").on_click(download))
                .item(PopupMenuItem::new("Save to workspace").disabled(true))
            });
        let text = plan
            .collapsed_preview
            .clone()
            .filter(|_| collapsed)
            .unwrap_or_else(|| plan.displayed_markdown.clone());
        let fade = color("surface").opacity(0.7);
        div()
            .min_w_0()
            .px_1()
            .py(px(2.))
            .child(
                v_flex()
                    .rounded(px(22.))
                    .border_1()
                    .border_color(tint("border", 0.8))
                    .bg(tint("surface", 0.7))
                    .p_4()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_between()
                            .gap_3()
                            .child(
                                h_flex()
                                    .min_w_0()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex_none()
                                            .rounded(px(6.))
                                            .bg(color("secondary"))
                                            .border_1()
                                            .border_color(color("border"))
                                            .px(px(6.))
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Plan"),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(plan.title.clone()),
                                    ),
                            )
                            .child(menu),
                    )
                    .child(
                        div()
                            .mt_4()
                            .relative()
                            .text_sm()
                            .line_height(px(22.75))
                            .text_color(tint("text", 0.8))
                            .when(collapsed, |body| body.max_h(px(416.)).overflow_hidden())
                            .child(chat_markdown(
                                SharedString::from(format!("plan-{}-{}", row.id, collapsed)),
                                text,
                                Arc::default(),
                                cx,
                            ))
                            .when(collapsed, |body| {
                                body.child(
                                    div()
                                        .absolute()
                                        .bottom_0()
                                        .left_0()
                                        .right_0()
                                        .h(px(96.))
                                        .bg(linear_gradient(
                                            180.,
                                            linear_color_stop(fade.opacity(0.), 0.),
                                            linear_color_stop(color("surface").opacity(0.95), 1.),
                                        )),
                                )
                            }),
                    )
                    .when(plan.collapsed_preview.is_some(), |card| {
                        card.child(
                            h_flex().mt_4().justify_center().child(
                                Button::new(SharedString::from(format!("plan-expand-{id}")))
                                    .label(if expanded {
                                        "Collapse plan"
                                    } else {
                                        "Expand plan"
                                    })
                                    .outline()
                                    .small()
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        let plans = &mut view.timeline.expanded_plans;
                                        if !plans.remove(&id) {
                                            plans.insert(id.clone());
                                        }
                                        cx.notify();
                                    })),
                            ),
                        )
                    }),
            )
            .into_any_element()
    }

    /// Saves a plan where the user chooses.
    fn download_plan(&self, filename: String, contents: String) {
        self.spawn_task(
            async move {
                tokio::task::spawn_blocking(move || {
                    match rfd::FileDialog::new().set_file_name(filename).save_file() {
                        Some(path) => std::fs::write(path, contents).map_err(|e| e.to_string()),
                        None => Ok(()),
                    }
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result)
            },
            |view, result, window, cx| {
                if let Err(error) = result {
                    view.show_error(&error, window, cx);
                }
            },
        );
    }

    pub(super) fn render_setup_row(
        &mut self,
        thread: &ThreadView,
        snapshot: &WorktreeSetupSnapshot,
        embedded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let card = thread
            .setup
            .card
            .clone()
            .filter(|card| card.sequence == snapshot.sequence)
            .unwrap_or_else(|| {
                setup_card(
                    snapshot,
                    &CardContext {
                        layout: SetupCardLayout::Desktop,
                        turn_started: embedded,
                        working_since_ms: None,
                        now_ms: super::super::ui::now_ms(),
                    },
                )
            });
        if !card.show_in_timeline {
            return div().into_any_element();
        }
        render_setup_card(
            &card,
            embedded,
            self.timeline.setup_details,
            cx.entity().downgrade(),
            mono(cx),
        )
    }
}

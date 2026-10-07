//! The thread details panel: the thread's workspace and its Lineage.
use crate::app::{
    Desktop,
    ui::{color, driver_icon, icon, text_2xs, tint},
};
use agent_core::{
    state::Intent,
    view::{
        agents::StatusTone,
        relationships::{
            LineageGroup, LineageGroupKind, LineageIcon, LineagePanel, LineageRow,
            lineage_group_title, lineage_window,
        },
    },
};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::collections::HashMap;

const PANEL_WIDTH: f32 = 280.;

fn group_key(kind: LineageGroupKind) -> u8 {
    match kind {
        LineageGroupKind::Related => 0,
        LineageGroupKind::Active => 1,
        LineageGroupKind::Previous => 2,
    }
}

/// The row hover tint of the panel's controls.
fn row_hover() -> Hsla {
    if super::super::ui::is_dark() {
        hsla(0., 0., 1., 0.075)
    } else {
        hsla(0., 0., 0., 0.055)
    }
}

/// What the user expanded and paged in each Lineage group, by thread.
#[derive(Default)]
pub(super) struct DetailsState {
    groups: HashMap<(String, u8), (bool, Option<u32>)>,
}

impl Desktop {
    pub(super) fn render_details(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let thread = self.views.thread.as_ref();
        let cwd = thread
            .and_then(|thread| thread.header.as_ref())
            .and_then(|header| header.cwd.clone());
        let scripts = thread
            .and_then(|thread| thread.scripts.clone())
            .map(|scripts| scripts.rows)
            .unwrap_or_default();
        let lineage = thread.and_then(|thread| {
            thread
                .lineage
                .clone()
                .map(|lineage| (thread.thread_id.clone(), lineage))
        });
        let thread_id = thread.map(|thread| thread.thread_id.clone());
        let workspace = v_flex()
            .px_2()
            .pt_2()
            .pb_2p5()
            .when_some(cwd, |section, cwd| {
                section.child(
                    h_flex()
                        .h_8()
                        .w_full()
                        .gap_2p5()
                        .px_2p5()
                        .rounded(px(10.))
                        .text_size(px(13.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(tint("text", 0.8))
                        .child(icon("folder").size_4().text_color(color("textMuted")))
                        .child(div().flex_1().min_w_0().truncate().child(cwd)),
                )
            })
            .children(scripts.into_iter().map(|row| {
                let script_id = row.script.id.clone();
                let thread_id = thread_id.clone();
                h_flex()
                    .id(SharedString::from(format!(
                        "details-script-{}",
                        row.script.id
                    )))
                    .h_8()
                    .w_full()
                    .gap_2p5()
                    .px_2p5()
                    .rounded(px(10.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tint("text", 0.8))
                    .cursor_pointer()
                    .hover(|row| row.bg(row_hover()))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        let Some(thread_id) = thread_id.clone() else {
                            return;
                        };
                        let size = view.terminal_size(cx);
                        view.perform(Intent::RunProjectScript {
                            thread_id,
                            script_id: script_id.clone(),
                            cols: size.cols,
                            rows: size.rows,
                        });
                    }))
                    .child(icon("play").size_4().text_color(color("textMuted")))
                    .child(div().flex_1().min_w_0().truncate().child(row.run_label))
            }));
        div()
            .w(px(PANEL_WIDTH + 24.))
            .flex_shrink_0()
            .h_full()
            .p_3()
            .child(
                v_flex()
                    .id("thread-details")
                    .max_h_full()
                    .w(px(PANEL_WIDTH))
                    .overflow_y_scroll()
                    .rounded(px(22.))
                    .border_1()
                    .border_color(color("border"))
                    .bg(color("surfaceOverlay"))
                    .shadow_lg()
                    .child(workspace)
                    .when_some(lineage, |card, (thread, lineage)| {
                        card.child(self.render_lineage(thread, lineage, cx))
                    }),
            )
            .into_any_element()
    }

    fn render_lineage(
        &self,
        thread: String,
        lineage: LineagePanel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.entity().downgrade();
        v_flex()
            .px_2()
            .pt_2()
            .pb_2p5()
            .border_t_1()
            .border_color(tint("border", 0.65))
            .child(
                h_flex()
                    .mb_1()
                    .min_h_8()
                    .px_1p5()
                    .justify_between()
                    .gap_2()
                    .child(
                        text_2xs(div())
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color("textMuted"))
                            .child(lineage.title.clone()),
                    )
                    .when(lineage.can_detach, |header| {
                        header.child(
                            Button::new("lineage-actions")
                                .icon(icon("ellipsis"))
                                .ghost()
                                .xsmall()
                                .accessibility_label("More thread actions")
                                .dropdown_menu(move |menu, _, _| {
                                    let owner = owner.clone();
                                    menu.item(
                                        PopupMenuItem::new("Disconnect agent session")
                                            .icon(icon("unplug"))
                                            .on_click(move |_, _, cx| {
                                                let _ = owner.update(cx, |view, _| {
                                                    view.perform(Intent::StopSessions)
                                                });
                                            }),
                                    )
                                }),
                        )
                    }),
            )
            .children(
                lineage
                    .groups
                    .into_iter()
                    .map(|group| self.render_lineage_group(&thread, group, cx))
                    .collect::<Vec<_>>(),
            )
    }

    fn render_lineage_group(
        &self,
        thread: &str,
        group: LineageGroup,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = (thread.to_owned(), group_key(group.kind));
        let (expanded, visible) = self
            .panels
            .details
            .groups
            .get(&key)
            .copied()
            .unwrap_or((group.expanded_by_default, None));
        let total = group.rows.len() as u32;
        let window = lineage_window(total, visible);
        let header = group.label.clone().map(|label| {
            let key = key.clone();
            h_flex()
                .id(SharedString::from(format!("lineage-group-{}", key.1)))
                .h_8()
                .w_full()
                .gap_2()
                .px_2()
                .rounded_md()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(tint("sidebarMutedForeground", 0.6))
                .cursor_pointer()
                .on_click(cx.listener(move |view, _, _, cx| {
                    let entry = view
                        .panels
                        .details
                        .groups
                        .entry(key.clone())
                        .or_insert((group.expanded_by_default, None));
                    entry.0 = !expanded;
                    cx.notify();
                }))
                .child(
                    div()
                        .flex_shrink_0()
                        .child(lineage_group_title(&label, total, expanded)),
                )
                .child(
                    div()
                        .h(px(1.))
                        .min_w_2()
                        .flex_1()
                        .bg(tint("sidebarBorder", 0.6)),
                )
                .when_some(group.failed_label.clone(), |header, failed| {
                    header.child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(10.))
                            .text_color(color("errorForeground"))
                            .child(failed),
                    )
                })
                .child(
                    icon(if expanded {
                        "chevron-up"
                    } else {
                        "chevron-down"
                    })
                    .size_3(),
                )
        });
        let show_more = window.show_more_label.clone().map(|label| {
            let key = key.clone();
            let next = window.next_visible_count;
            h_flex()
                .id(SharedString::from(format!("lineage-more-{}", key.1)))
                .h_8()
                .w_full()
                .gap_2p5()
                .px_2p5()
                .rounded(px(10.))
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(tint("textMuted", 0.7))
                .cursor_pointer()
                .hover(|row| row.bg(row_hover()).text_color(tint("text", 0.8)))
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.panels
                        .details
                        .groups
                        .insert(key.clone(), (true, Some(next)));
                    cx.notify();
                }))
                .child(icon("plus").size_4())
                .child(label)
        });
        v_flex()
            .children(header)
            .when(expanded, |group_column| {
                group_column
                    .child(
                        v_flex()
                            .id(SharedString::from(format!("lineage-rows-{}", key.1)))
                            .max_h(px(216.))
                            .overflow_y_scroll()
                            .children(
                                group
                                    .rows
                                    .into_iter()
                                    .take(window.visible_count as usize)
                                    .map(|row| self.render_lineage_row(row, cx)),
                            ),
                    )
                    .children(show_more)
            })
            .into_any_element()
    }

    fn render_lineage_row(&self, row: LineageRow, cx: &mut Context<Self>) -> AnyElement {
        let tooltip: SharedString = match &row.agent {
            Some(agent) => [
                Some(row.title.clone()),
                (!agent.model_label.is_empty())
                    .then(|| format!("{} · {}", agent.model_label, agent.status_label)),
                agent.elapsed.clone(),
                agent.preview.clone(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
            None => row.hint.clone(),
        }
        .into();
        let status_dot = match row.status_tone {
            StatusTone::Working => color("updateForeground"),
            StatusTone::Completed => color("successForeground"),
            StatusTone::Failed => color("error"),
            StatusTone::Inactive => tint("textMuted", 0.45),
        };
        let glyph = match (row.driver, row.icon) {
            (Some(driver), _) => driver_icon(driver),
            (None, LineageIcon::Parent) => icon("corner-left-up"),
            (None, LineageIcon::Agent) => icon("bot"),
            (None, LineageIcon::Fork) => icon("git-fork"),
        };
        let merge = row.merge_back.clone();
        let thread_id = row.thread_id.clone();
        let missing = row.missing;
        let content = h_flex()
            .id(SharedString::from(format!("lineage-row-{}", row.thread_id)))
            .group("lineage-row")
            .h_8()
            .flex_1()
            .min_w_0()
            .gap_2p5()
            .px_2p5()
            .rounded(px(10.))
            .text_size(px(13.))
            .when(merge.is_some(), |content| content.rounded_r_none())
            .map(|content| {
                if missing {
                    content.opacity(0.64)
                } else {
                    content
                        .cursor_pointer()
                        .hover(|content| content.bg(row_hover()))
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.open_thread(thread_id.clone(), cx)
                        }))
                }
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(
                div()
                    .relative()
                    .size_4()
                    .flex_shrink_0()
                    .child(glyph.size_4().text_color(color("textMuted")))
                    .child(
                        div()
                            .absolute()
                            .right(px(-4.))
                            .bottom(px(-4.))
                            .size_2()
                            .rounded_full()
                            .border_2()
                            .border_color(color("surfaceOverlay"))
                            .bg(status_dot),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .line_height(px(16.))
                    .text_color(tint("text", 0.85))
                    .child(row.title.clone()),
            )
            .map(
                |content| match row.agent.as_ref().and_then(|agent| agent.elapsed.clone()) {
                    Some(elapsed) => content.child(
                        text_2xs(div())
                            .flex_shrink_0()
                            .text_color(color("textMuted"))
                            .child(elapsed),
                    ),
                    None if row.agent.is_none() => content.child(
                        div()
                            .flex_shrink_0()
                            .invisible()
                            .group_hover("lineage-row", |arrow| arrow.visible())
                            .child(icon("arrow-right").size_3().text_color(color("textMuted"))),
                    ),
                    None => content,
                },
            )
            .when(merge.is_none(), |content| {
                content.child(
                    text_2xs(div())
                        .flex_shrink_0()
                        .text_color(color("textMuted"))
                        .child(row.status_label.clone()),
                )
            });
        let Some(merge) = merge else {
            return content.into_any_element();
        };
        let target = row.thread_id.clone();
        let merge_tooltip: SharedString = merge.tooltip.clone().into();
        h_flex()
            .h_8()
            .w_full()
            .items_center()
            .rounded(px(10.))
            .child(content)
            .child(
                div()
                    .h_4()
                    .w(px(1.))
                    .flex_shrink_0()
                    .bg(tint("border", 0.65)),
            )
            .child(
                h_flex()
                    .id("lineage-merge-back")
                    .h_8()
                    .w_8()
                    .flex_shrink_0()
                    .justify_center()
                    .map(|button| {
                        if merge.enabled {
                            button
                                .cursor_pointer()
                                .hover(|button| button.bg(row_hover()))
                                .on_click(cx.listener(move |view, _, _, _| {
                                    let target = target.clone();
                                    view.perform_then(
                                        Intent::MergeBack,
                                        move |view, result, window, cx| match result {
                                            Ok(_) => view.open_thread(target, cx),
                                            Err(error) => view.show_error(error, window, cx),
                                        },
                                    );
                                }))
                        } else {
                            button.opacity(0.64)
                        }
                    })
                    .tooltip(move |window, cx| {
                        Tooltip::new(merge_tooltip.clone()).build(window, cx)
                    })
                    .child(
                        icon(if merge.busy {
                            "loader-circle"
                        } else {
                            "git-merge"
                        })
                        .size_3(),
                    ),
            )
            .child(
                text_2xs(div())
                    .flex_shrink_0()
                    .pl_1()
                    .pr_2p5()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color("textMuted"))
                    .child(row.status_label),
            )
            .into_any_element()
    }
}

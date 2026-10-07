//! The thread details card: the thread's workspace and its Lineage, pinned
//! at the chat area's top right while a readable chat lane fits beside it,
//! else a popover under the header.
use crate::app::{
    Desktop,
    settings::SettingsPage,
    ui::{color, driver_icon, icon, text_2xs, tint},
};
use agent_core::{
    state::Intent,
    view::{
        agents::StatusTone,
        header::WorkspaceRow,
        projects::scripts::ProjectScriptsView,
        relationships::{
            LineageGroup, LineageGroupKind, LineageIcon, LineagePanel, LineageRow,
            lineage_group_title, lineage_window,
        },
    },
};
use gpui_kit::{
    component::{
        Sizable, WindowExt,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        notification::Notification,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

const PANEL_WIDTH: f32 = 280.;
const CARD_GAP: f32 = 12.;
/// The room kept between the chat lane and the card.
const CARD_CLEARANCE: f32 = 32.;
const LANE_PADDING: f32 = 20.;
const LANE_MIN_WIDTH: f32 = 640.;

/// Where the card sits in a chat area of `width` x `height`: its left, top,
/// width and height; `None` when the chat lane would get too narrow, and the
/// card opens as a popover instead.
fn details_card_layout(width: f32, height: f32) -> Option<(f32, f32, f32, f32)> {
    let x = width - PANEL_WIDTH - CARD_GAP;
    if x - CARD_CLEARANCE - LANE_PADDING < LANE_MIN_WIDTH {
        return None;
    }
    let card_height = height - CARD_GAP * 2.;
    (card_height >= 160.).then_some((x, CARD_GAP, PANEL_WIDTH, card_height))
}

/// How far the chat lane's right edge moves in from the area's so the lane
/// clears an open card: the lane stays centred while it fits, then moves left.
fn chat_lane_inset(width: f32, max_width: f32, card_left: Option<f32>) -> f32 {
    let Some(card_left) = card_left else {
        return 0.;
    };
    let centered = max_width.min(width - LANE_PADDING * 2.).max(0.);
    let lane_right = card_left - CARD_CLEARANCE;
    let lane_width = centered.min(lane_right - LANE_PADDING).max(0.);
    let left = LANE_PADDING.max(((width - lane_width) / 2.).min(lane_right - lane_width));
    (width - left * 2. - lane_width).max(0.)
}

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

/// What the user expanded and paged in each Lineage group, and whether the
/// card is open inline and as a popover, by thread.
#[derive(Default)]
pub(super) struct DetailsState {
    groups: HashMap<(String, u8), (bool, Option<u32>)>,
    /// Threads whose inline card the user closed; it starts open.
    inline_closed: HashSet<String>,
    popover_open: HashSet<String>,
    /// When a click outside last closed the popover; the toggle under that
    /// click must not open it again.
    dismissed_at: Option<std::time::Instant>,
    /// The chat area, measured as it was last laid out.
    area: Rc<Cell<Bounds<Pixels>>>,
}

impl Desktop {
    /// Where the card sits now; `None` shows it as a popover.
    fn details_layout(&self) -> Option<(f32, f32, f32, f32)> {
        let area = self.panels.details.area.get();
        details_card_layout(area.size.width.as_f32(), area.size.height.as_f32())
    }

    /// The card is open in the mode it has now.
    pub(crate) fn details_open(&self) -> bool {
        let Some(thread) = self.thread_id() else {
            return false;
        };
        let details = &self.panels.details;
        match self.details_layout() {
            Some(_) => !details.inline_closed.contains(&thread),
            None => details.popover_open.contains(&thread),
        }
    }

    /// A click outside closed the popover just now.
    pub(super) fn details_just_dismissed(&self) -> bool {
        self.panels
            .details
            .dismissed_at
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_millis(300))
    }

    pub(super) fn set_details_open(&mut self, open: bool) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        let inline = self.details_layout().is_some();
        let details = &mut self.panels.details;
        if inline {
            details.popover_open.remove(&thread);
            if open {
                details.inline_closed.remove(&thread);
            } else {
                details.inline_closed.insert(thread);
            }
        } else if open {
            details.popover_open.insert(thread);
        } else {
            details.popover_open.remove(&thread);
        }
    }

    /// The chat area with the thread details card over its top right, the
    /// chat lane moved left to clear the card.
    pub(crate) fn details_area(
        &mut self,
        body: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let area = self.panels.details.area.clone();
        let bounds = area.get();
        let shown = self.snapshot.selected_thread.is_some() && self.details_open();
        let layout = self.details_layout();
        let inset = chat_lane_inset(
            bounds.size.width.as_f32(),
            super::super::ui::metrics().chat_max_width,
            layout.filter(|_| shown).map(|(x, ..)| x),
        );
        let card = shown.then(|| self.render_details(window, cx));
        let card = card.map(|card| match layout {
            Some((x, y, width, height)) => div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(width))
                .max_h(px(height))
                .flex()
                .flex_col()
                .child(card)
                .into_any_element(),
            None => {
                let width = PANEL_WIDTH.min(bounds.size.width.as_f32() - 16.);
                deferred(
                    anchored()
                        .position(point(
                            bounds.right() - px(width) - px(8.),
                            bounds.top() + px(8.),
                        ))
                        .child(
                            div()
                                .id("thread-details-popover")
                                .occlude()
                                .w(px(width))
                                .max_h(bounds.size.height - px(16.))
                                .flex()
                                .flex_col()
                                .on_mouse_down_out(cx.listener(|view, _, window, cx| {
                                    view.set_details_open(false);
                                    view.panels.details.dismissed_at =
                                        Some(std::time::Instant::now());
                                    view.panels_changed(window, cx);
                                }))
                                .child(card),
                        ),
                )
                .into_any_element()
            }
        });
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                canvas(move |bounds, _, _| area.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .pr(px(inset))
                    .child(body),
            )
            .children(card)
            .into_any_element()
    }

    fn render_details(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let thread = self.views.thread.as_ref();
        let workspace_row = thread
            .and_then(|thread| thread.header.as_ref())
            .and_then(|header| header.workspace.clone());
        let scripts = thread.and_then(|thread| thread.scripts.clone());
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
            .when_some(workspace_row, |section, row| {
                section.child(self.render_workspace_row(row, cx))
            })
            .when_some(
                scripts.filter(|_| thread_id.is_some()),
                |section, scripts| section.child(self.render_details_scripts(scripts, cx)),
            );
        v_flex()
            .id("thread-details")
            .max_h_full()
            .w_full()
            .overflow_y_scroll()
            .rounded(px(24.))
            .border_1()
            .border_color(color("border"))
            .bg(color("surfaceOverlay"))
            .shadow_lg()
            .child(workspace)
            .when_some(lineage, |card, (thread, lineage)| {
                card.child(self.render_lineage(thread, lineage, cx))
            })
            .into_any_element()
    }

    /// The thread's folder: its name, "Worktree" beside a worktree, the full
    /// path on hover and "Copy full path" on right click.
    fn render_workspace_row(&self, row: WorkspaceRow, cx: &mut Context<Self>) -> AnyElement {
        let tooltip: SharedString = row.path.clone().unwrap_or_else(|| row.label.clone()).into();
        let path = row.path.clone();
        h_flex()
            .id("details-workspace")
            .h_8()
            .w_full()
            .gap_2p5()
            .px_2p5()
            .rounded(px(10.))
            .text_size(px(13.))
            .text_color(tint("textMuted", 0.7))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .when_some(path, |row, path| {
                row.on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                        let path = path.clone();
                        view.open_menu(event.position, window, cx, move |menu, _, _| {
                            let path = path.clone();
                            menu.item(
                                PopupMenuItem::new("Copy full path")
                                    .icon(icon("copy"))
                                    .on_click(move |_, window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            path.clone(),
                                        ));
                                        window.push_notification(
                                            Notification::success(path.clone())
                                                .title("Path copied"),
                                            cx,
                                        );
                                    }),
                            )
                        });
                    }),
                )
            })
            .child(
                icon(if row.in_worktree {
                    "folder-git"
                } else {
                    "folder"
                })
                .size_4()
                .text_color(color("textMuted")),
            )
            .child(div().min_w_0().truncate().child(row.label))
            .children(row.kind.map(|kind| {
                div()
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(tint("textMuted", 0.7))
                    .child(kind)
            }))
            .into_any_element()
    }

    /// The project's scripts: the primary one runs from the row, the rest
    /// and "Add project script" from the menu beside it.
    fn render_details_scripts(
        &self,
        scripts: ProjectScriptsView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let project_id = scripts.project_id.clone();
        let primary = scripts
            .primary_script_id
            .as_ref()
            .and_then(|id| scripts.rows.iter().find(|row| &row.script.id == id))
            .or_else(|| scripts.rows.first())
            .cloned();
        let Some(primary) = primary else {
            return h_flex()
                .id("details-add-script")
                .h_8()
                .w_full()
                .gap_2p5()
                .px_2p5()
                .rounded(px(10.))
                .text_size(px(13.))
                .text_color(tint("text", 0.8))
                .cursor_pointer()
                .hover(|row| row.bg(row_hover()))
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.open_settings(
                        SettingsPage::Projects {
                            project_id: Some(project_id.clone()),
                        },
                        window,
                        cx,
                    )
                }))
                .child(icon("plus").size_4().text_color(color("textMuted")))
                .child("Add project script")
                .into_any_element();
        };
        let primary_id = primary.script.id.clone();
        let tooltip: SharedString = primary.run_label.clone().into();
        let owner = cx.entity().downgrade();
        let rows = scripts.rows.clone();
        h_flex()
            .w_full()
            .h_8()
            .rounded(px(10.))
            .child(
                h_flex()
                    .id("details-script-primary")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .gap_2p5()
                    .px_2p5()
                    .rounded_l(px(10.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(tint("text", 0.8))
                    .cursor_pointer()
                    .hover(|row| row.bg(row_hover()))
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.run_project_script(primary_id.clone(), cx)
                    }))
                    .child(icon("play").size_4().text_color(color("textMuted")))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(primary.script.name.clone()),
                    ),
            )
            .child(div().w(px(1.)).h_4().bg(tint("border", 0.65)))
            .child(
                Button::new("details-scripts-menu")
                    .icon(icon("chevron-down"))
                    .ghost()
                    .xsmall()
                    .accessibility_label("Script actions")
                    .dropdown_menu(move |mut menu, _, _| {
                        for row in &rows {
                            let (owner, id) = (owner.clone(), row.script.id.clone());
                            menu = menu.item(
                                PopupMenuItem::new(row.label.clone())
                                    .icon(icon("play"))
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            view.run_project_script(id.clone(), cx)
                                        });
                                    }),
                            );
                        }
                        let (owner, project_id) = (owner.clone(), project_id.clone());
                        menu.item(
                            PopupMenuItem::new("Add project script")
                                .icon(icon("plus"))
                                .on_click(move |_, window, cx| {
                                    let project_id = project_id.clone();
                                    let _ = owner.update(cx, |view, cx| {
                                        view.open_settings(
                                            SettingsPage::Projects {
                                                project_id: Some(project_id),
                                            },
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        )
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

#[cfg(test)]
mod tests {
    use super::{chat_lane_inset, details_card_layout};
    use core::prelude::v1::test;

    #[test]
    fn the_card_pins_top_right_only_beside_a_readable_chat_lane() {
        assert_eq!(
            details_card_layout(1200., 800.),
            Some((908., 12., 280., 776.))
        );
        assert_eq!(details_card_layout(983., 800.), None);
        assert_eq!(
            details_card_layout(984., 800.),
            Some((692., 12., 280., 776.))
        );
        assert_eq!(details_card_layout(1200., 183.), None);
    }

    #[test]
    fn the_chat_lane_stays_centred_until_the_card_needs_its_room() {
        assert_eq!(chat_lane_inset(1600., 736., None), 0.);
        assert_eq!(chat_lane_inset(1600., 736., Some(1308.)), 0.);
        let inset = chat_lane_inset(1100., 736., Some(808.));
        let left = (1100. - inset - 736.) / 2.;
        assert_eq!(left + 736., 776.);
    }
}

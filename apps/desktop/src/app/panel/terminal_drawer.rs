//! The thread's terminals: the drawer under the chat column, which the right
//! panel's Terminal tab shows instead while it is open.
use super::{PanelTab, tab_close_button, with_shortcut};
use crate::{
    app::{
        Desktop,
        ui::{color, icon, tint},
    },
    terminal::Terminal,
};
use agent_core::{state::Intent, view::terminals::TerminalTab};
use agent_protocol::operations::TerminalSize;
use gpui_kit::{
    component::{Sizable, button::Button, h_flex, tooltip::Tooltip, v_flex},
    prelude::FluentBuilder,
    *,
};
use std::collections::{HashMap, HashSet};

const DRAWER_DEFAULT_HEIGHT: f32 = 280.;
const DRAWER_MIN_HEIGHT: f32 = 180.;
const DRAWER_MAX_FRACTION: f32 = 0.75;
const MAX_TERMINALS_PER_GROUP: usize = 4;

/// The drawer's height, kept between its minimum and three quarters of the
/// window.
fn clamp_drawer_height(height: f32, viewport: f32) -> f32 {
    height.clamp(
        DRAWER_MIN_HEIGHT,
        (viewport * DRAWER_MAX_FRACTION).max(DRAWER_MIN_HEIGHT),
    )
}

/// Dragged while resizing the drawer by its top edge.
#[derive(Clone)]
struct DrawerResize;
impl Render for DrawerResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// The tabs split side by side, by their group, in tab order.
fn groups(tabs: &[TerminalTab]) -> Vec<(&str, Vec<&TerminalTab>)> {
    let mut groups: Vec<(&str, Vec<&TerminalTab>)> = vec![];
    for tab in tabs {
        match groups.iter_mut().find(|(group, _)| *group == tab.group) {
            Some((_, members)) => members.push(tab),
            None => groups.push((&tab.group, vec![tab])),
        }
    }
    groups
}

pub(super) struct TerminalState {
    views: HashMap<(String, String), Entity<Terminal>>,
    /// The selected terminal of each thread.
    active: HashMap<String, String>,
    /// Threads whose drawer is open.
    open: HashSet<String>,
    /// Groups split top to bottom, by thread.
    stacked: HashSet<(String, String)>,
    /// The thread whose terminals are shown.
    thread: Option<String>,
    height: f32,
    focus_request: Option<(String, String)>,
    creating: bool,
}
impl Default for TerminalState {
    fn default() -> Self {
        Self {
            views: HashMap::new(),
            active: HashMap::new(),
            open: HashSet::new(),
            stacked: HashSet::new(),
            thread: None,
            height: DRAWER_DEFAULT_HEIGHT,
            focus_request: None,
            creating: false,
        }
    }
}
impl TerminalState {
    pub(super) fn drawer_open(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| self.open.contains(thread))
    }
    pub(super) fn set_drawer_open(&mut self, thread: &str, open: bool) {
        if open {
            self.open.insert(thread.to_owned());
        } else {
            self.open.remove(thread);
        }
    }
    fn active_id<'a>(&self, thread: &str, tabs: &'a [TerminalTab]) -> Option<&'a str> {
        let selected = self.active.get(thread);
        tabs.iter()
            .find(|tab| Some(&tab.terminal_id) == selected)
            .or(tabs.first())
            .map(|tab| tab.terminal_id.as_str())
    }
}

impl Desktop {
    /// Keeps a view for each terminal of the shown thread and feeds them the
    /// snapshot; views of hidden terminals drop and detach.
    pub(super) fn sync_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.thread_id();
        if thread != self.panels.terminals.thread {
            self.panels.terminals.thread = thread.clone();
            self.refresh_views(cx);
        }
        let shown = self.panels.terminals.drawer_open() || self.panels.shows(PanelTab::Terminal);
        let (Some(thread), Some(store), true) = (thread, self.store(), shown) else {
            self.panels.terminals.views.clear();
            return;
        };
        let tabs = self.snapshot.terminals(thread.clone());
        let state = &mut self.panels.terminals;
        state.views.retain(|(owner, id), _| {
            *owner == thread && tabs.iter().any(|tab| &tab.terminal_id == id)
        });
        for tab in &tabs {
            let key = (thread.clone(), tab.terminal_id.clone());
            if let std::collections::hash_map::Entry::Vacant(entry) = state.views.entry(key) {
                entry.insert(Terminal::new(
                    store.clone(),
                    self.snapshot.clone(),
                    thread.clone(),
                    tab.terminal_id.clone(),
                    window,
                    cx,
                ));
            }
        }
        for view in state.views.values() {
            let snapshot = self.snapshot.clone();
            view.update(cx, |view, cx| view.set_snapshot(snapshot, cx));
        }
        if let Some(request) = state.focus_request.take() {
            match state.views.get(&request) {
                Some(view) => {
                    let focus = view.read(cx).focus_handle();
                    focus.focus(window, cx);
                }
                None if request.0 == thread => state.focus_request = Some(request),
                None => {}
            }
        }
    }

    /// Selects a terminal of the thread and shows it, in the drawer unless
    /// the right panel's Terminal tab is showing.
    pub(super) fn show_terminal(
        &mut self,
        thread: String,
        terminal_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.panels.terminals;
        state.active.insert(thread.clone(), terminal_id.clone());
        state.focus_request = Some((thread.clone(), terminal_id));
        if !self.panels.shows(PanelTab::Terminal) {
            self.panels.terminals.set_drawer_open(&thread, true);
        }
        self.panels_changed(window, cx);
    }

    /// Focuses the thread's selected terminal, starting its first terminal
    /// when it has none.
    pub(super) fn ensure_terminal(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        let tabs = self.snapshot.terminals(thread.clone());
        let state = &mut self.panels.terminals;
        if let Some(active) = state.active_id(&thread, &tabs) {
            state.focus_request = Some((thread, active.to_owned()));
            return;
        }
        if state.creating {
            return;
        }
        self.panels.terminals.creating = true;
        let size = self.terminal_size(cx);
        self.perform_then(
            Intent::NewTerminal {
                thread_id: thread,
                cols: size.cols,
                rows: size.rows,
            },
            |view, result, window, cx| {
                view.panels.terminals.creating = false;
                if let Err(error) = result {
                    view.show_error(error, window, cx);
                }
            },
        );
    }

    /// The size new terminals start at: the selected terminal's.
    pub(super) fn terminal_size(&self, cx: &App) -> TerminalSize {
        let state = &self.panels.terminals;
        state
            .thread
            .as_ref()
            .and_then(|thread| {
                let id = state.active.get(thread)?;
                state.views.get(&(thread.clone(), id.clone()))
            })
            .or_else(|| state.views.values().next())
            .map_or(TerminalSize { cols: 80, rows: 24 }, |view| {
                view.read(cx).size()
            })
    }

    pub(super) fn terminal_focused(&self, window: &Window, cx: &App) -> bool {
        self.panels
            .terminals
            .views
            .values()
            .any(|view| view.read(cx).is_focused(window))
    }

    /// The drawer's keys while a terminal is focused.
    pub(super) fn terminal_key(
        &mut self,
        key: &str,
        shift: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match (key, shift) {
            ("d", vertical) => self.split_terminal(vertical, cx),
            ("n", false) => self.new_terminal(cx),
            ("w", false) => {
                if let Some(thread) = self.thread_id() {
                    let tabs = self.snapshot.terminals(thread.clone());
                    if let Some(tab) = self
                        .panels
                        .terminals
                        .active_id(&thread, &tabs)
                        .and_then(|id| tabs.iter().find(|tab| tab.terminal_id == id))
                    {
                        self.close_terminal(tab.clone(), window, cx);
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn split_terminal(&mut self, vertical: bool, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        let tabs = self.snapshot.terminals(thread.clone());
        let Some(active) = self.panels.terminals.active_id(&thread, &tabs) else {
            return;
        };
        let Some((group, members)) = groups(&tabs)
            .into_iter()
            .find(|(_, members)| members.iter().any(|tab| tab.terminal_id == active))
        else {
            return;
        };
        if members.len() >= MAX_TERMINALS_PER_GROUP {
            return;
        }
        let key = (thread.clone(), group.to_owned());
        if vertical {
            self.panels.terminals.stacked.insert(key);
        } else {
            self.panels.terminals.stacked.remove(&key);
        }
        let size = self.terminal_size(cx);
        self.perform(Intent::SplitTerminal {
            thread_id: thread,
            terminal_id: active.to_owned(),
            cols: size.cols,
            rows: size.rows,
        });
    }

    fn new_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        let size = self.terminal_size(cx);
        self.perform(Intent::NewTerminal {
            thread_id: thread,
            cols: size.cols,
            rows: size.rows,
        });
    }

    /// Asks, then stops the terminal's process; the last one closes the drawer.
    fn close_terminal(&mut self, tab: TerminalTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        self.confirm(
            crate::app::dialogs::Confirm {
                title: Some(format!("Close terminal \"{}\"?", tab.label)),
                message: "This stops the running process and clears its history.".into(),
                action: "Close".into(),
                destructive: true,
            },
            window,
            cx,
            move |view, _, _| {
                let thread = thread.clone();
                view.perform_then(
                    Intent::CloseTerminal {
                        thread_id: thread.clone(),
                        terminal_id: tab.terminal_id.clone(),
                    },
                    move |view, result, window, cx| {
                        if let Err(error) = result {
                            view.show_error(error, window, cx);
                        } else if view.snapshot.terminals(thread.clone()).is_empty() {
                            view.panels.terminals.set_drawer_open(&thread, false);
                            if view.panels.shows(PanelTab::Terminal) {
                                view.close_surface(PanelTab::Terminal, window, cx);
                            }
                            view.panels_changed(window, cx);
                        }
                    },
                );
            },
        );
    }

    fn select_terminal(
        &mut self,
        terminal_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        if let Some(view) = self
            .panels
            .terminals
            .views
            .get(&(thread.clone(), terminal_id.clone()))
        {
            let focus = view.read(cx).focus_handle();
            focus.focus(window, cx);
        }
        self.panels.terminals.active.insert(thread, terminal_id);
        cx.notify();
    }

    /// The thread's terminals: in the drawer, or filling the right panel.
    pub(super) fn render_terminals(
        &mut self,
        in_panel: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let thread = self.thread_id().unwrap_or_default();
        let tabs = self
            .views
            .thread
            .as_ref()
            .filter(|view| view.thread_id == thread)
            .map(|view| view.terminals.clone())
            .unwrap_or_else(|| self.snapshot.terminals(thread.clone()));
        let viewport = window.viewport_size().height.as_f32();
        let height = clamp_drawer_height(self.panels.terminals.height, viewport);
        let container = v_flex()
            .id(if in_panel {
                "panel-terminals"
            } else {
                "terminal-drawer"
            })
            .relative()
            .min_w_0()
            .overflow_hidden()
            .bg(color("canvas"))
            .map(|container| {
                if in_panel {
                    container.flex_1().h_full()
                } else {
                    container
                        .h(px(height))
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(tint("border", 0.8))
                        .on_drag_move(cx.listener(
                            |view, event: &DragMoveEvent<DrawerResize>, window, cx| {
                                let viewport = window.viewport_size().height.as_f32();
                                let bottom = event.bounds.bottom().as_f32();
                                view.panels.terminals.height = clamp_drawer_height(
                                    bottom - event.event.position.y.as_f32(),
                                    viewport,
                                );
                                cx.notify();
                            },
                        ))
                }
            });
        // Last, so it sits above the terminals it overlaps.
        let resize_handle = (!in_panel).then(|| {
            div()
                .id("terminal-drawer-resize")
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(px(6.))
                .cursor_row_resize()
                .on_drag(DrawerResize, |drag, _, _, cx| {
                    cx.stop_propagation();
                    cx.new(|_| drag.clone())
                })
        });
        let new_label = with_shortcut("New Terminal", "N");
        if tabs.is_empty() {
            return container
                .child(
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .items_center()
                        .justify_center()
                        .gap_3()
                        .px_4()
                        .py_6()
                        .text_sm()
                        .text_color(color("textMuted"))
                        .child("No terminal sessions for this thread yet.")
                        .child(
                            Button::new("new-terminal-empty")
                                .label(new_label)
                                .outline()
                                .xsmall()
                                .on_click(cx.listener(|view, _, _, cx| view.new_terminal(cx))),
                        ),
                )
                .children(resize_handle)
                .into_any_element();
        }
        let state = &self.panels.terminals;
        let active = state
            .active_id(&thread, &tabs)
            .unwrap_or_default()
            .to_owned();
        let groups = groups(&tabs);
        let (active_group, visible) = groups
            .iter()
            .find(|(_, members)| members.iter().any(|tab| tab.terminal_id == active))
            .map(|(group, members)| (group.to_string(), members.clone()))
            .unwrap_or_default();
        let stacked = state.stacked.contains(&(thread.clone(), active_group));
        let limit = visible.len() >= MAX_TERMINALS_PER_GROUP;
        let labels = ActionLabels {
            split: if limit {
                format!("Split Terminal Horizontally (max {MAX_TERMINALS_PER_GROUP} per group)")
            } else {
                with_shortcut("Split Terminal Horizontally", "D")
            },
            split_vertical: if limit {
                format!("Split Terminal Vertically (max {MAX_TERMINALS_PER_GROUP} per group)")
            } else {
                with_shortcut("Split Terminal Vertically", "⇧D")
            },
            new: new_label,
            close: with_shortcut("Close Terminal", "W"),
            limit,
        };
        let active_tab = tabs.iter().find(|tab| tab.terminal_id == active).cloned();
        let views: Vec<(String, Entity<Terminal>)> = visible
            .iter()
            .filter_map(|tab| {
                state
                    .views
                    .get(&(thread.clone(), tab.terminal_id.clone()))
                    .map(|view| (tab.terminal_id.clone(), view.clone()))
            })
            .collect();
        let split = views.len() > 1;
        let screens = views.into_iter().enumerate().map(|(index, (id, view))| {
            let selected = id == active;
            div()
                .id(SharedString::from(format!("terminal-pane-{id}")))
                .flex_1()
                .min_w_0()
                .min_h_0()
                .when(split && index > 0, |pane| {
                    let border = if selected {
                        color("border")
                    } else {
                        tint("border", 0.7)
                    };
                    if stacked {
                        pane.border_t_1().border_color(border)
                    } else {
                        pane.border_l_1().border_color(border)
                    }
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |view, _, _, cx| {
                        if let Some(thread) = view.thread_id() {
                            view.panels.terminals.active.insert(thread, id.clone());
                            cx.notify();
                        }
                    }),
                )
                .child(view)
        });
        let has_sidebar = tabs.len() > 1;
        let body = div().flex_1().min_w_0().h_full().child(
            if stacked {
                v_flex().size_full()
            } else {
                h_flex().size_full()
            }
            .children(screens),
        );
        container
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .bg(color("terminalBackground"))
                    .when(has_sidebar, |row| row.gap_1p5())
                    .child(body)
                    .when(has_sidebar, |row| {
                        row.child(self.render_terminal_sidebar(
                            &thread,
                            &groups,
                            &active,
                            &labels,
                            active_tab.clone(),
                            cx,
                        ))
                    }),
            )
            .when(!has_sidebar, |container| {
                container.child(
                    div().absolute().right_2().top_2().child(
                        h_flex()
                            .id("terminal-actions")
                            .occlude()
                            .items_center()
                            .overflow_hidden()
                            .rounded_md()
                            .border_1()
                            .border_color(tint("border", 0.8))
                            .bg(color("canvas"))
                            .shadow_xs()
                            .children(
                                self.terminal_actions(&labels, active_tab.clone(), false, cx)
                                    .into_iter()
                                    .enumerate()
                                    .flat_map(|(index, button)| {
                                        let separator = (index > 0).then(|| {
                                            div()
                                                .h_4()
                                                .w(px(1.))
                                                .bg(tint("border", 0.8))
                                                .into_any_element()
                                        });
                                        separator.into_iter().chain([button])
                                    }),
                            ),
                    ),
                )
            })
            .children(resize_handle)
            .into_any_element()
    }

    /// Split, split vertically, new and close, as compact icon buttons.
    fn terminal_actions(
        &self,
        labels: &ActionLabels,
        active: Option<TerminalTab>,
        bordered: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let button = |id: &'static str, icon_name: &'static str, label: &str, disabled: bool| {
            let tooltip: SharedString = label.to_owned().into();
            div()
                .id(id)
                .flex()
                .items_center()
                .h_full()
                .p_1()
                .when(bordered, |button| {
                    button
                        .px_1()
                        .py_0()
                        .border_l_1()
                        .border_color(tint("border", 0.7))
                })
                .text_color(tint("text", 0.9))
                .map(|button| {
                    if disabled {
                        button.opacity(0.64).cursor_not_allowed()
                    } else {
                        button
                            .cursor_pointer()
                            .hover(|button| button.bg(color("accentSurface")))
                    }
                })
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .child(icon(icon_name).size(px(13.)))
        };
        vec![
            button(
                "terminal-split",
                "square-split-horizontal",
                &labels.split,
                labels.limit,
            )
            .when(bordered, |button| button.border_l_0())
            .on_click(cx.listener(|view, _, _, cx| view.split_terminal(false, cx)))
            .into_any_element(),
            button(
                "terminal-split-vertical",
                "square-split-vertical",
                &labels.split_vertical,
                labels.limit,
            )
            .on_click(cx.listener(|view, _, _, cx| view.split_terminal(true, cx)))
            .into_any_element(),
            button("terminal-new", "plus", &labels.new, false)
                .on_click(cx.listener(|view, _, _, cx| view.new_terminal(cx)))
                .into_any_element(),
            button("terminal-close", "trash-2", &labels.close, false)
                .on_click(cx.listener(move |view, _, window, cx| {
                    if let Some(tab) = active.clone() {
                        view.close_terminal(tab, window, cx);
                    }
                }))
                .into_any_element(),
        ]
    }

    /// The list of the thread's terminals beside them, once there are two.
    fn render_terminal_sidebar(
        &self,
        thread: &str,
        groups: &[(&str, Vec<&TerminalTab>)],
        active: &str,
        labels: &ActionLabels,
        active_tab: Option<TerminalTab>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let actions = self.terminal_actions(labels, active_tab, true, cx);
        let show_headers = groups.len() > 1 || groups.iter().any(|(_, members)| members.len() > 1);
        let entries = groups.iter().map(|(group, members)| {
            let group_active = members.iter().any(|tab| tab.terminal_id == active);
            let first = members
                .first()
                .map(|tab| tab.terminal_id.clone())
                .unwrap_or_default();
            let stacked = self
                .panels
                .terminals
                .stacked
                .contains(&(thread.to_owned(), group.to_string()));
            let (group_icon, group_label) = match (members.len() > 1, stacked) {
                (false, _) => ("square", "Single"),
                (true, true) => ("square-split-vertical", "Stacked"),
                (true, false) => ("square-split-horizontal", "Side by side"),
            };
            let count = members.len();
            v_flex()
                .pb_0p5()
                .when(show_headers, |entry| {
                    entry.child(
                        h_flex()
                            .id(SharedString::from(format!("terminal-group-{group}")))
                            .h(px(22.))
                            .w_full()
                            .gap_1()
                            .px_1p5()
                            .rounded(px(4.))
                            .text_size(px(11.))
                            .cursor_pointer()
                            .map(|header| {
                                if group_active {
                                    header
                                        .bg(tint("accentSurface", 0.5))
                                        .text_color(color("text"))
                                } else {
                                    header.text_color(color("textMuted")).hover(|header| {
                                        header
                                            .bg(tint("accentSurface", 0.4))
                                            .text_color(color("text"))
                                    })
                                }
                            })
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if !group_active {
                                    view.select_terminal(first.clone(), window, cx);
                                }
                            }))
                            .child(icon(group_icon).size_3())
                            .child(div().flex_1().min_w_0().truncate().child(group_label))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(tint("textMuted", 0.7))
                                    .child(count.to_string()),
                            ),
                    )
                })
                .child(v_flex().gap_0p5().children(members.iter().map(|tab| {
                    let selected = tab.terminal_id == active;
                    let id = tab.terminal_id.clone();
                    let close_tab = (*tab).clone();
                    let close_label = if selected {
                        format!("Close {} ({})", tab.label, super::shortcut("W"))
                    } else {
                        format!("Close {}", tab.label)
                    };
                    h_flex()
                        .id(SharedString::from(format!("terminal-tab-{id}")))
                        .group("terminal-tab")
                        .h_6()
                        .w_full()
                        .gap_0p5()
                        .pl_1p5()
                        .pr_2()
                        .rounded_md()
                        .text_xs()
                        .cursor_pointer()
                        .map(|row| {
                            if selected {
                                row.bg(color("accentSurface")).text_color(color("text"))
                            } else {
                                row.text_color(color("textMuted")).hover(|row| {
                                    row.bg(tint("accentSurface", 0.6)).text_color(color("text"))
                                })
                            }
                        })
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.select_terminal(id.clone(), window, cx)
                        }))
                        .child(tab_close_button(
                            SharedString::from(format!("terminal-tab-close-{}", tab.terminal_id)),
                            "terminal-tab",
                            "square-terminal",
                            close_label,
                            cx.listener(move |view, _, window, cx| {
                                cx.stop_propagation();
                                view.close_terminal(close_tab.clone(), window, cx);
                            }),
                        ))
                        .child(div().flex_1().min_w_0().truncate().child(tab.label.clone()))
                })))
        });
        v_flex()
            .w(px(144.))
            .flex_shrink_0()
            .h_full()
            .border_1()
            .border_color(tint("border", 0.7))
            .bg(tint("muted", 0.1))
            .child(
                h_flex()
                    .h(px(22.))
                    .flex_shrink_0()
                    .justify_end()
                    .border_b_1()
                    .border_color(tint("border", 0.7))
                    .children(actions),
            )
            .child(
                v_flex()
                    .id("terminal-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_1()
                    .py_1()
                    .children(entries),
            )
    }
}

struct ActionLabels {
    split: String,
    split_vertical: String,
    new: String,
    close: String,
    limit: bool,
}

#[cfg(test)]
mod tests {
    use super::{DRAWER_MIN_HEIGHT, clamp_drawer_height, groups};
    use agent_core::view::terminals::TerminalTab;
    use core::prelude::v1::test;

    fn tab(id: &str, group: &str) -> TerminalTab {
        TerminalTab {
            terminal_id: id.into(),
            group: group.into(),
            label: id.into(),
            status: "Running".into(),
            running: true,
            running_process: false,
            menu_status: "Ready".into(),
        }
    }

    #[test]
    fn drawer_height_stays_between_its_minimum_and_three_quarters_of_the_window() {
        assert_eq!(clamp_drawer_height(280., 960.), 280.);
        assert_eq!(clamp_drawer_height(100., 960.), DRAWER_MIN_HEIGHT);
        assert_eq!(clamp_drawer_height(900., 960.), 720.);
        assert_eq!(clamp_drawer_height(300., 200.), DRAWER_MIN_HEIGHT);
    }

    #[test]
    fn groups_keep_tab_order_and_collect_split_terminals() {
        let tabs = [
            tab("setup-x", "setup-x"),
            tab("term-1", "term-1"),
            tab("term-2", "term-1"),
            tab("term-3", "term-3"),
        ];
        let groups: Vec<(&str, Vec<&str>)> = groups(&tabs)
            .into_iter()
            .map(|(group, members)| {
                (
                    group,
                    members.iter().map(|tab| tab.terminal_id.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(
            groups,
            [
                ("setup-x", vec!["setup-x"]),
                ("term-1", vec!["term-1", "term-2"]),
                ("term-3", vec!["term-3"]),
            ]
        );
    }
}

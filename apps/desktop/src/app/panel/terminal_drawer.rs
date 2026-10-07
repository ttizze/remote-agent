//! The thread's terminals: the drawer under the chat column and the right
//! panel's terminal surfaces. Each terminal is held by one of them; the
//! drawer shows the thread's terminals no surface holds.
use super::{Surface, tab_close_button, with_shortcut};
use crate::{
    app::{
        Desktop,
        ui::{color, icon, tint},
    },
    terminal::{SelectionEvent, Terminal},
};
use agent_core::{
    connection::Outcome,
    state::{Intent, TerminalPhase},
    view::{composer::terminal_context::TerminalContextSelection, terminals::TerminalTab},
};
use agent_protocol::operations::{TerminalSize, terminal_label, thread_terminal_handle_for};
use gpui_kit::{
    component::{Sizable, button::Button, h_flex, menu::PopupMenuItem, tooltip::Tooltip, v_flex},
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

/// "Close terminal "a"?" or "Close 2 terminals?", and what closing does.
fn close_confirmation(labels: &[String]) -> (String, String) {
    match labels {
        [label] => (
            format!("Close terminal \"{label}\"?"),
            "This stops the running process and clears its history.".into(),
        ),
        labels => (
            format!("Close {} terminals?", labels.len()),
            format!(
                "This stops their running processes and clears their histories: {}.",
                labels
                    .iter()
                    .map(|label| format!("\"{label}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
    }
}

/// One thread's drawer.
#[derive(Default)]
struct Drawer {
    open: bool,
    active: Option<String>,
    /// Groups split top to bottom.
    stacked: HashSet<String>,
}

pub(super) struct TerminalState {
    views: HashMap<(String, String), Entity<Terminal>>,
    drawers: HashMap<String, Drawer>,
    /// The thread whose terminals are shown.
    thread: Option<String>,
    height: f32,
    focus_request: Option<(String, String)>,
    creating: bool,
    /// Exited terminals already being closed.
    exited: HashSet<(String, String)>,
}
impl Default for TerminalState {
    fn default() -> Self {
        Self {
            views: HashMap::new(),
            drawers: HashMap::new(),
            thread: None,
            height: DRAWER_DEFAULT_HEIGHT,
            focus_request: None,
            creating: false,
            exited: HashSet::new(),
        }
    }
}
impl TerminalState {
    pub(super) fn drawer_open(&self) -> bool {
        self.thread
            .as_ref()
            .and_then(|thread| self.drawers.get(thread))
            .is_some_and(|drawer| drawer.open)
    }
    fn drawer(&mut self, thread: &str) -> &mut Drawer {
        self.drawers.entry(thread.to_owned()).or_default()
    }
    pub(super) fn request_focus(&mut self, thread: String, terminal_id: String) {
        self.focus_request = Some((thread, terminal_id));
    }
}

/// Where a terminal action applies: the drawer or one panel surface.
#[derive(Clone, PartialEq)]
enum Place {
    Drawer,
    Surface(String),
}

impl Desktop {
    /// The thread's terminals the drawer shows: those no panel surface holds.
    fn drawer_tabs(&self, thread: &str) -> Vec<TerminalTab> {
        let held: HashSet<&String> = self.right().terminal_ids().collect();
        self.snapshot
            .terminals(thread.to_owned())
            .into_iter()
            .filter(|tab| !held.contains(&tab.terminal_id))
            .collect()
    }

    /// A panel surface's terminals, in its order; one not opened yet shows
    /// under its default name.
    fn surface_tabs(&self, thread: &str, terminal_ids: &[String]) -> Vec<TerminalTab> {
        let tabs = self.snapshot.terminals(thread.to_owned());
        let group = terminal_ids.first().cloned().unwrap_or_default();
        terminal_ids
            .iter()
            .map(|id| {
                let tab = tabs.iter().find(|tab| &tab.terminal_id == id);
                TerminalTab {
                    terminal_id: id.clone(),
                    group: group.clone(),
                    label: tab.map_or_else(|| terminal_label(id), |tab| tab.label.clone()),
                    status: tab.map(|tab| tab.status.clone()).unwrap_or_default(),
                    running: tab.is_some_and(|tab| tab.running),
                    running_process: tab.is_some_and(|tab| tab.running_process),
                    menu_status: tab.map(|tab| tab.menu_status.clone()).unwrap_or_default(),
                    exited: tab.is_some_and(|tab| tab.exited),
                    cwd: tab.map(|tab| tab.cwd.clone()).unwrap_or_default(),
                }
            })
            .collect()
    }

    /// The panel surface holding `terminal_id`.
    fn surface_of(&self, terminal_id: &str) -> Option<String> {
        self.right()
            .surfaces
            .iter()
            .find(|surface| {
                matches!(surface, Surface::Terminal { terminal_ids, .. }
                    if terminal_ids.iter().any(|id| id == terminal_id))
            })
            .map(Surface::id)
    }

    /// Keeps a view for each terminal on screen and feeds them the snapshot;
    /// views of hidden terminals drop and detach. A shown terminal whose
    /// process exited closes.
    pub(super) fn sync_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let thread = self.thread_id();
        if thread != self.panels.terminals.thread {
            self.panels.terminals.thread = thread.clone();
            self.refresh_views(cx);
        }
        let (Some(thread), Some(store)) = (thread, self.store()) else {
            self.panels.terminals.views.clear();
            return;
        };
        let mut shown: Vec<String> = vec![];
        if self.panels.terminals.drawer_open() {
            shown.extend(
                self.drawer_tabs(&thread)
                    .into_iter()
                    .map(|tab| tab.terminal_id),
            );
        }
        if self.right().open
            && let Some(Surface::Terminal { terminal_ids, .. }) = self.right().active_surface()
        {
            shown.extend(terminal_ids.iter().cloned());
        }
        let exited: Vec<String> = shown
            .iter()
            .filter(|id| {
                self.snapshot
                    .terminals
                    .get(&thread_terminal_handle_for(&thread, id))
                    .is_some_and(|terminal| matches!(terminal.phase, TerminalPhase::Exited(_)))
                    && !self
                        .panels
                        .terminals
                        .exited
                        .contains(&(thread.clone(), (*id).clone()))
            })
            .cloned()
            .collect();
        let state = &mut self.panels.terminals;
        state
            .views
            .retain(|(owner, id), _| *owner == thread && shown.contains(id));
        for id in &shown {
            let key = (thread.clone(), id.clone());
            if let std::collections::hash_map::Entry::Vacant(entry) = state.views.entry(key) {
                let view = Terminal::new(
                    store.clone(),
                    self.snapshot.clone(),
                    thread.clone(),
                    id.clone(),
                    window,
                    cx,
                );
                let terminal_id = id.clone();
                cx.subscribe_in(&view, window, move |desktop, view, event, window, cx| {
                    desktop.terminal_selection(&terminal_id, view, event, window, cx)
                })
                .detach();
                entry.insert(view);
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
        for id in exited {
            self.panels
                .terminals
                .exited
                .insert((thread.clone(), id.clone()));
            self.close_terminals(thread.clone(), vec![id], window, cx);
        }
    }

    /// A finished selection offers "Add to chat" and "Copy" where it ended.
    fn terminal_selection(
        &mut self,
        terminal_id: &str,
        view: &Entity<Terminal>,
        event: &SelectionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SelectionEvent::Released { position } => {
                let can_add = self.composer_caret(cx).is_some();
                let desktop = cx.entity().downgrade();
                let (add, copy) = (view.downgrade(), view.downgrade());
                let terminal_id = terminal_id.to_owned();
                self.open_menu(*position, window, cx, move |menu, _, _| {
                    let menu = if can_add {
                        menu.item(PopupMenuItem::new("Add to chat").on_click(
                            move |_, window, cx| {
                                if let Some(view) = add.upgrade() {
                                    let _ = desktop.update(cx, |desktop, cx| {
                                        desktop.add_terminal_selection(
                                            &terminal_id,
                                            &view,
                                            window,
                                            cx,
                                        )
                                    });
                                }
                            },
                        ))
                    } else {
                        menu
                    };
                    menu.item(PopupMenuItem::new("Copy").on_click(move |_, window, cx| {
                        let _ = copy.update(cx, |view, cx| {
                            view.copy_selection(cx);
                            view.focus_handle().focus(window, cx);
                        });
                    }))
                });
            }
            SelectionEvent::AddToChat => self.add_terminal_selection(terminal_id, view, window, cx),
        }
    }

    /// Places the terminal's selected lines in the composer at its caret.
    fn add_terminal_selection(
        &mut self,
        terminal_id: &str,
        view: &Entity<Terminal>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(thread), Some((text, cursor)), Some(lines)) = (
            self.thread_id(),
            self.composer_caret(cx),
            view.read(cx).selected_lines(),
        ) else {
            return;
        };
        let terminal_label = self
            .snapshot
            .terminals(thread)
            .into_iter()
            .find(|tab| tab.terminal_id == terminal_id)
            .map_or_else(|| terminal_label(terminal_id), |tab| tab.label);
        self.perform(Intent::AddTerminalContext {
            text,
            cursor,
            selection: TerminalContextSelection {
                terminal_id: terminal_id.to_owned(),
                terminal_label,
                line_start: lines.line_start,
                line_end: lines.line_end,
                text: lines.text,
            },
        });
        view.update(cx, |view, cx| view.clear_selection(cx));
    }

    /// Selects a terminal of the thread and shows it where it is held.
    pub(super) fn show_terminal(
        &mut self,
        thread: String,
        terminal_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panels
            .terminals
            .request_focus(thread.clone(), terminal_id.clone());
        match self.surface_of(&terminal_id) {
            Some(surface) => {
                let right = self.right_mut();
                right.open = true;
                right.active = Some(surface);
                if let Some(Surface::Terminal { active, .. }) = right
                    .surfaces
                    .iter_mut()
                    .find(|open| Some(open.id()) == right.active)
                {
                    *active = terminal_id;
                }
            }
            None => {
                let drawer = self.panels.terminals.drawer(&thread);
                drawer.open = true;
                drawer.active = Some(terminal_id);
            }
        }
        self.panels_changed(window, cx);
    }

    /// Shows a terminal the thread just opened in the drawer.
    fn show_opened(
        &mut self,
        result: &Result<Outcome, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(Outcome::TerminalOpened { terminal_id }) => {
                if let Some(thread) = self.thread_id() {
                    self.show_terminal(thread, terminal_id.clone(), window, cx);
                }
            }
            Ok(_) => {}
            Err(error) => self.show_error(error, window, cx),
        }
    }

    /// Runs a project script in a new drawer terminal.
    pub(super) fn run_project_script(&mut self, script_id: String, cx: &mut Context<Self>) {
        let Some(thread_id) = self.thread_id() else {
            return;
        };
        let size = self.terminal_size(cx);
        self.perform_then(
            Intent::RunProjectScript {
                thread_id,
                script_id,
                cols: size.cols,
                rows: size.rows,
            },
            |view, result, window, cx| view.show_opened(result, window, cx),
        );
    }

    pub(crate) fn toggle_terminal_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        if !self.snapshot.terminal_available() {
            return;
        }
        let drawer = self.panels.terminals.drawer(&thread);
        drawer.open = !drawer.open;
        if drawer.open {
            self.ensure_drawer_terminal(cx);
        }
        self.panels_changed(window, cx);
    }

    /// Focuses the drawer's selected terminal, starting one when the drawer
    /// has none.
    fn ensure_drawer_terminal(&mut self, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        let tabs = self.drawer_tabs(&thread);
        if let Some(active) = self.drawer_active(&thread, &tabs) {
            self.panels.terminals.request_focus(thread, active);
            return;
        }
        if self.panels.terminals.creating {
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
                view.show_opened(result, window, cx);
            },
        );
    }

    fn drawer_active(&self, thread: &str, tabs: &[TerminalTab]) -> Option<String> {
        let selected = self
            .panels
            .terminals
            .drawers
            .get(thread)
            .and_then(|drawer| drawer.active.as_ref());
        tabs.iter()
            .find(|tab| Some(&tab.terminal_id) == selected)
            .or(tabs.first())
            .map(|tab| tab.terminal_id.clone())
    }

    /// Opens a new terminal in its own panel surface.
    pub(super) fn add_terminal_surface(&mut self) -> bool {
        let Some(thread) = self.thread_id() else {
            return false;
        };
        let size = TerminalSize { cols: 80, rows: 24 };
        self.perform_then(
            Intent::NewTerminal {
                thread_id: thread.clone(),
                cols: size.cols,
                rows: size.rows,
            },
            move |view, result, window, cx| match result {
                Ok(Outcome::TerminalOpened { terminal_id }) => {
                    if view.thread_id().as_deref() == Some(thread.as_str()) {
                        view.right_mut()
                            .upsert(Surface::terminal(terminal_id.clone()));
                        view.panels
                            .terminals
                            .request_focus(thread.clone(), terminal_id.clone());
                        view.panels_changed(window, cx);
                    }
                }
                Ok(_) => {}
                Err(error) => view.show_error(error, window, cx),
            },
        );
        true
    }

    /// The size new terminals start at: the selected terminal's.
    pub(super) fn terminal_size(&self, cx: &App) -> TerminalSize {
        let state = &self.panels.terminals;
        let focused = state.thread.as_ref().and_then(|thread| {
            let id = state.drawers.get(thread)?.active.clone()?;
            state.views.get(&(thread.clone(), id))
        });
        focused
            .or_else(|| state.views.values().next())
            .map_or(TerminalSize { cols: 80, rows: 24 }, |view| {
                view.read(cx).size()
            })
    }

    /// The focused terminal's id.
    fn focused_terminal(&self, window: &Window, cx: &App) -> Option<String> {
        self.panels
            .terminals
            .views
            .iter()
            .find(|(_, view)| view.read(cx).is_focused(window))
            .map(|((_, id), _)| id.clone())
    }

    pub(crate) fn terminal_focused(&self, window: &Window, cx: &App) -> bool {
        self.focused_terminal(window, cx).is_some()
    }

    /// A terminal command for the focused terminal, where it is held.
    pub(crate) fn terminal_command(
        &mut self,
        command: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(focused) = self.focused_terminal(window, cx) else {
            return false;
        };
        let place = match self.surface_of(&focused) {
            Some(surface) => Place::Surface(surface),
            None => Place::Drawer,
        };
        match command {
            "terminal.split" => self.split_terminal(place, false, cx),
            "terminal.splitVertical" => self.split_terminal(place, true, cx),
            "terminal.new" => self.new_terminal(place, window, cx),
            "terminal.close" => self.confirm_close_terminals(vec![focused], window, cx),
            _ => return false,
        }
        true
    }

    fn split_terminal(&mut self, place: Place, vertical: bool, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        let size = self.terminal_size(cx);
        match place {
            Place::Drawer => {
                let tabs = self.drawer_tabs(&thread);
                let Some(active) = self.drawer_active(&thread, &tabs) else {
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
                let group = group.to_owned();
                let drawer = self.panels.terminals.drawer(&thread);
                drawer.open = true;
                if vertical {
                    drawer.stacked.insert(group);
                } else {
                    drawer.stacked.remove(&group);
                }
                self.perform_then(
                    Intent::SplitTerminal {
                        thread_id: thread,
                        terminal_id: active,
                        cols: size.cols,
                        rows: size.rows,
                    },
                    |view, result, window, cx| view.show_opened(result, window, cx),
                );
            }
            Place::Surface(surface) => {
                let Some(Surface::Terminal {
                    terminal_ids,
                    active,
                    ..
                }) = self
                    .right()
                    .surfaces
                    .iter()
                    .find(|open| open.id() == surface)
                    .cloned()
                else {
                    return;
                };
                if terminal_ids.len() >= MAX_TERMINALS_PER_GROUP {
                    return;
                }
                let thread_id = thread.clone();
                self.perform_then(
                    Intent::SplitTerminal {
                        thread_id,
                        terminal_id: active.clone(),
                        cols: size.cols,
                        rows: size.rows,
                    },
                    move |view, result, window, cx| match result {
                        Ok(Outcome::TerminalOpened { terminal_id }) => {
                            if view.thread_id().as_deref() != Some(thread.as_str()) {
                                return;
                            }
                            let right = view.right_mut();
                            if let Some(Surface::Terminal {
                                terminal_ids,
                                active: selected,
                                stacked,
                                ..
                            }) = right.surfaces.iter_mut().find(|open| open.id() == surface)
                            {
                                let at = terminal_ids
                                    .iter()
                                    .position(|id| *id == active)
                                    .map_or(terminal_ids.len(), |index| index + 1);
                                terminal_ids.insert(at, terminal_id.clone());
                                *selected = terminal_id.clone();
                                *stacked = vertical;
                            }
                            view.panels
                                .terminals
                                .request_focus(thread.clone(), terminal_id.clone());
                            view.panels_changed(window, cx);
                        }
                        Ok(_) => {}
                        Err(error) => view.show_error(error, window, cx),
                    },
                );
            }
        }
    }

    /// A new terminal: a new drawer group, or a new panel surface.
    fn new_terminal(&mut self, place: Place, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        if let Place::Surface(_) = place {
            self.add_terminal_surface();
            return;
        }
        self.panels.terminals.drawer(&thread).open = true;
        let size = self.terminal_size(cx);
        self.perform_then(
            Intent::NewTerminal {
                thread_id: thread,
                cols: size.cols,
                rows: size.rows,
            },
            |view, result, window, cx| view.show_opened(result, window, cx),
        );
        self.panels_changed(window, cx);
    }

    /// Asks, then stops the terminals' processes and clears their history.
    pub(super) fn confirm_close_terminals(
        &mut self,
        terminal_ids: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        let tabs = self.snapshot.terminals(thread.clone());
        let labels: Vec<String> = terminal_ids
            .iter()
            .map(|id| {
                tabs.iter()
                    .find(|tab| &tab.terminal_id == id)
                    .map_or_else(|| terminal_label(id), |tab| tab.label.clone())
            })
            .collect();
        let (title, message) = close_confirmation(&labels);
        self.confirm(
            crate::app::dialogs::Confirm {
                title: Some(title),
                message,
                action: "Close".into(),
                destructive: true,
            },
            window,
            cx,
            move |view, window, cx| {
                view.close_terminals(thread.clone(), terminal_ids.clone(), window, cx)
            },
        );
    }

    /// Stops the terminals and takes them out of their surface or the
    /// drawer; the drawer closes with its last terminal.
    pub(super) fn close_terminals(
        &mut self,
        thread: String,
        terminal_ids: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.thread_id().as_deref() == Some(thread.as_str()) {
            let right = self.right_mut();
            for id in &terminal_ids {
                right.remove_terminal(id);
            }
        }
        for terminal_id in terminal_ids {
            let thread = thread.clone();
            self.perform_then(
                Intent::CloseTerminal {
                    thread_id: thread.clone(),
                    terminal_id,
                },
                move |view, result, window, cx| {
                    if let Err(error) = result {
                        view.show_error(error, window, cx);
                    } else if view.drawer_tabs(&thread).is_empty() {
                        view.panels.terminals.drawer(&thread).open = false;
                        view.panels_changed(window, cx);
                    }
                },
            );
        }
        self.panels_changed(window, cx);
    }

    fn select_terminal(
        &mut self,
        place: &Place,
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
        match place {
            Place::Drawer => self.panels.terminals.drawer(&thread).active = Some(terminal_id),
            Place::Surface(surface) => {
                if let Some(Surface::Terminal { active, .. }) = self
                    .right_mut()
                    .surfaces
                    .iter_mut()
                    .find(|open| &open.id() == surface)
                {
                    *active = terminal_id;
                }
            }
        }
        cx.notify();
    }

    /// The thread's terminals: the drawer's (`surface` is `None`), or one
    /// panel surface's filling the panel.
    pub(super) fn render_terminals(
        &mut self,
        surface: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let thread = self.thread_id().unwrap_or_default();
        let in_panel = surface.is_some();
        let place = match &surface {
            Some(surface) => Place::Surface(surface.clone()),
            None => Place::Drawer,
        };
        let (tabs, active, panel_stacked) = match &surface {
            Some(surface) => match self
                .right()
                .surfaces
                .iter()
                .find(|open| &open.id() == surface)
            {
                Some(Surface::Terminal {
                    terminal_ids,
                    active,
                    stacked,
                    ..
                }) => (
                    self.surface_tabs(&thread, terminal_ids),
                    active.clone(),
                    *stacked,
                ),
                _ => (vec![], String::new(), false),
            },
            None => {
                let tabs = self.drawer_tabs(&thread);
                let active = self.drawer_active(&thread, &tabs).unwrap_or_default();
                (tabs, active, false)
            }
        };
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
            let place = place.clone();
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
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.new_terminal(place.clone(), window, cx)
                                })),
                        ),
                )
                .children(resize_handle)
                .into_any_element();
        }
        let groups = groups(&tabs);
        let (active_group, visible) = groups
            .iter()
            .find(|(_, members)| members.iter().any(|tab| tab.terminal_id == active))
            .map(|(group, members)| (group.to_string(), members.clone()))
            .unwrap_or_default();
        let stacked = if in_panel {
            panel_stacked
        } else {
            self.panels
                .terminals
                .drawers
                .get(&thread)
                .is_some_and(|drawer| drawer.stacked.contains(&active_group))
        };
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
        let state = &self.panels.terminals;
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
            let place = place.clone();
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
                    cx.listener(move |view, _, window, cx| {
                        view.select_terminal(&place, id.clone(), window, cx)
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
        let active_id = active.clone();
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
                            &place, &thread, &groups, &active, &labels, stacked, cx,
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
                                self.terminal_actions(&place, &labels, active_id, false, cx)
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
        place: &Place,
        labels: &ActionLabels,
        active: String,
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
        let (split, split_vertical, new) = (place.clone(), place.clone(), place.clone());
        vec![
            button(
                "terminal-split",
                "square-split-horizontal",
                &labels.split,
                labels.limit,
            )
            .when(bordered, |button| button.border_l_0())
            .on_click(
                cx.listener(move |view, _, _, cx| view.split_terminal(split.clone(), false, cx)),
            )
            .into_any_element(),
            button(
                "terminal-split-vertical",
                "square-split-vertical",
                &labels.split_vertical,
                labels.limit,
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                view.split_terminal(split_vertical.clone(), true, cx)
            }))
            .into_any_element(),
            button("terminal-new", "plus", &labels.new, false)
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.new_terminal(new.clone(), window, cx)
                }))
                .into_any_element(),
            button("terminal-close", "trash-2", &labels.close, false)
                .on_click(cx.listener(move |view, _, window, cx| {
                    if !active.is_empty() {
                        view.confirm_close_terminals(vec![active.clone()], window, cx);
                    }
                }))
                .into_any_element(),
        ]
    }

    /// The list of the terminals beside them, once there are two.
    #[allow(clippy::too_many_arguments)]
    fn render_terminal_sidebar(
        &self,
        place: &Place,
        thread: &str,
        groups: &[(&str, Vec<&TerminalTab>)],
        active: &str,
        labels: &ActionLabels,
        panel_stacked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let actions = self.terminal_actions(place, labels, active.to_owned(), true, cx);
        let show_headers = groups.len() > 1 || groups.iter().any(|(_, members)| members.len() > 1);
        let entries = groups.iter().map(|(group, members)| {
            let group_active = members.iter().any(|tab| tab.terminal_id == active);
            let first = members
                .first()
                .map(|tab| tab.terminal_id.clone())
                .unwrap_or_default();
            let stacked = match place {
                Place::Surface(_) => panel_stacked,
                Place::Drawer => self
                    .panels
                    .terminals
                    .drawers
                    .get(thread)
                    .is_some_and(|drawer| drawer.stacked.contains(*group)),
            };
            let (group_icon, group_label) = match (members.len() > 1, stacked) {
                (false, _) => ("square", "Single"),
                (true, true) => ("square-split-vertical", "Stacked"),
                (true, false) => ("square-split-horizontal", "Side by side"),
            };
            let count = members.len();
            let group_place = place.clone();
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
                                    view.select_terminal(&group_place, first.clone(), window, cx);
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
                    let close_id = tab.terminal_id.clone();
                    let close_label = if selected {
                        format!("Close {} ({})", tab.label, super::shortcut("W"))
                    } else {
                        format!("Close {}", tab.label)
                    };
                    let place = place.clone();
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
                            view.select_terminal(&place, id.clone(), window, cx)
                        }))
                        .child(tab_close_button(
                            SharedString::from(format!("terminal-tab-close-{}", tab.terminal_id)),
                            "terminal-tab",
                            "square-terminal",
                            close_label,
                            cx.listener(move |view, _, window, cx| {
                                cx.stop_propagation();
                                view.confirm_close_terminals(vec![close_id.clone()], window, cx);
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
    use super::{DRAWER_MIN_HEIGHT, clamp_drawer_height, close_confirmation, groups};
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
            exited: false,
            cwd: String::new(),
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

    #[test]
    fn closing_asks_with_every_label() {
        assert_eq!(
            close_confirmation(&["vite".into()]),
            (
                "Close terminal \"vite\"?".into(),
                "This stops the running process and clears its history.".into()
            )
        );
        assert_eq!(
            close_confirmation(&["a".into(), "b".into()]),
            (
                "Close 2 terminals?".into(),
                "This stops their running processes and clears their histories: \"a\", \"b\"."
                    .into()
            )
        );
    }
}

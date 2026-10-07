//! The right panel (Browser, Terminal, Files, Diff), the thread terminal
//! drawer and the thread details panel.
mod details;
mod diff;
mod files;
mod terminal_drawer;

use super::{
    Desktop, Route,
    ui::{color, icon, shortcut, tint},
};
use agent_core::{connection::Outcome, view::header::HeaderPanelState};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

const PANEL_MIN_WIDTH: f32 = 360.;
const PANEL_MAX_FRACTION: f32 = 0.7;
/// The chat column keeps this much beside the panel.
const SIBLING_MIN_WIDTH: f32 = 360.;

/// The right panel's tabs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelTab {
    Diff,
    Terminal,
    Files,
    Browser,
}
impl PanelTab {
    /// The order the surface menus list them in.
    const ALL: [PanelTab; 4] = [
        PanelTab::Browser,
        PanelTab::Terminal,
        PanelTab::Files,
        PanelTab::Diff,
    ];
    fn label(self) -> &'static str {
        match self {
            PanelTab::Browser => "Browser",
            PanelTab::Terminal => "Terminal",
            PanelTab::Files => "Files",
            PanelTab::Diff => "Diff",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            PanelTab::Browser => "globe",
            PanelTab::Terminal => "square-terminal",
            PanelTab::Files => "files",
            PanelTab::Diff => "file-diff",
        }
    }
    fn key(self) -> &'static str {
        match self {
            PanelTab::Browser => "b",
            PanelTab::Terminal => "t",
            PanelTab::Files => "f",
            PanelTab::Diff => "d",
        }
    }
    fn unavailable_hint(self) -> &'static str {
        match self {
            PanelTab::Browser => "Only available in the desktop app.",
            PanelTab::Terminal | PanelTab::Files => "Available when a project is open.",
            PanelTab::Diff => "Available for Git repositories.",
        }
    }
}

/// The right panel's width, kept between its minimum and what leaves the chat
/// column usable.
fn clamp_panel_width(width: f32, viewport: f32) -> f32 {
    let max = (viewport * PANEL_MAX_FRACTION)
        .min(viewport - SIBLING_MIN_WIDTH)
        .max(PANEL_MIN_WIDTH);
    width.clamp(PANEL_MIN_WIDTH, max)
}

/// Dragged while resizing the right panel by its left edge.
#[derive(Clone)]
struct PanelResize;
impl Render for PanelResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

pub(crate) struct PanelState {
    right_open: bool,
    /// The open surfaces, in tab order.
    surfaces: Vec<PanelTab>,
    active: Option<PanelTab>,
    width: f32,
    launcher_focus: FocusHandle,
    details_open: bool,
    browser: Option<Entity<crate::browser::Browser>>,
    diff: diff::DiffState,
    files: files::FilesState,
    terminals: terminal_drawer::TerminalState,
    details: details::DetailsState,
    _subscriptions: Vec<Subscription>,
}
impl PanelState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let mut subscriptions = vec![];
        Self {
            right_open: false,
            surfaces: vec![],
            active: None,
            width: super::ui::metrics().panel_width,
            launcher_focus: cx.focus_handle(),
            details_open: false,
            browser: None,
            diff: diff::DiffState::new(cx),
            files: files::FilesState::new(window, cx, &mut subscriptions),
            terminals: terminal_drawer::TerminalState::default(),
            details: details::DetailsState::default(),
            _subscriptions: subscriptions,
        }
    }
    /// The panels the header shows as open.
    pub(crate) fn header_panels(&self) -> HeaderPanelState {
        HeaderPanelState {
            thread_panel_open: self.details_open,
            terminal_open: self.terminals.drawer_open(),
            right_panel_open: self.right_open,
            files_open: self.right_open && self.active == Some(PanelTab::Files),
        }
    }
    /// Closes what belonged to the previous connection.
    pub(crate) fn reset(&mut self, _: &mut Window, cx: &mut Context<Desktop>) {
        self.terminals = terminal_drawer::TerminalState::default();
        self.diff.reset();
        self.files.reset();
        self.details = details::DetailsState::default();
        if let Some(browser) = self.browser.take() {
            browser.update(cx, |browser, cx| browser.set_visible(false, cx));
        }
        self.surfaces.retain(|tab| *tab != PanelTab::Browser);
        if self.active == Some(PanelTab::Browser) {
            self.active = self.surfaces.first().copied();
        }
    }
    fn shows(&self, tab: PanelTab) -> bool {
        self.right_open && self.active == Some(tab)
    }
}

impl Desktop {
    pub(crate) fn render_right_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.panels.right_open {
            return None;
        }
        let viewport = window.viewport_size().width.as_f32();
        let width = clamp_panel_width(self.panels.width, viewport);
        let body = match self.panels.active {
            None => self.render_launcher(cx),
            Some(PanelTab::Browser) => match &self.panels.browser {
                Some(browser) => div()
                    .flex_1()
                    .min_h_0()
                    .child(browser.clone())
                    .into_any_element(),
                None => div().flex_1().into_any_element(),
            },
            Some(PanelTab::Terminal) => self.render_terminals(true, window, cx),
            Some(PanelTab::Files) => self.render_files(window, cx),
            Some(PanelTab::Diff) => self.render_diff(window, cx),
        };
        Some(
            v_flex()
                .id("right-panel")
                .relative()
                .w(px(width))
                .flex_shrink_0()
                .h_full()
                .bg(color("canvas"))
                .border_l_1()
                .border_color(color("border"))
                .on_drag_move(cx.listener(
                    |view, event: &DragMoveEvent<PanelResize>, window, cx| {
                        let viewport = window.viewport_size().width.as_f32();
                        let right = event.bounds.right().as_f32();
                        view.panels.width =
                            clamp_panel_width(right - event.event.position.x.as_f32(), viewport);
                        cx.notify();
                    },
                ))
                .child(self.render_tab_bar(cx))
                .child(v_flex().flex_1().min_h_0().child(body))
                .child(
                    div()
                        .id("right-panel-resize")
                        .absolute()
                        .left(px(-3.))
                        .top_0()
                        .bottom_0()
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(PanelResize, |drag, _, _, cx| {
                            cx.stop_propagation();
                            cx.new(|_| drag.clone())
                        })
                        .on_click(cx.listener(|view, event: &ClickEvent, _, cx| {
                            if event.click_count() == 2 {
                                view.panels.width = super::ui::metrics().panel_width;
                                cx.notify();
                            }
                        })),
                )
                .into_any_element(),
        )
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = super::ui::metrics();
        let tabs = self.panels.surfaces.iter().map(|tab| {
            let tab = *tab;
            let active = self.panels.active == Some(tab);
            h_flex()
                .id(SharedString::from(format!("panel-tab-{}", tab.label())))
                .group("panel-tab")
                .h_6()
                .max_w(px(144.))
                .flex_shrink_0()
                .gap_0p5()
                .pl_1p5()
                .pr_2()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .map(|row| {
                    if active {
                        row.bg(color("accentSurface")).text_color(color("text"))
                    } else {
                        row.text_color(color("textMuted")).hover(|row| {
                            row.bg(tint("accentSurface", 0.6)).text_color(color("text"))
                        })
                    }
                })
                .on_click(
                    cx.listener(move |view, _, window, cx| view.open_right_panel(tab, window, cx)),
                )
                .child(tab_close_button(
                    SharedString::from(format!("panel-tab-close-{}", tab.label())),
                    "panel-tab",
                    tab.icon(),
                    format!("Close {}", tab.label()),
                    cx.listener(move |view, _, window, cx| {
                        cx.stop_propagation();
                        view.close_surface(tab, window, cx);
                    }),
                ))
                .child(div().min_w_0().truncate().child(tab.label()))
        });
        let owner = cx.entity().downgrade();
        let available = self.surface_availability();
        h_flex()
            .h(px(metrics.header_height))
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .pl_2()
            .pr_3()
            .children(tabs)
            .when(!self.panels.surfaces.is_empty(), |bar| {
                bar.child(
                    Button::new("add-panel-surface")
                        .icon(icon("plus"))
                        .ghost()
                        .xsmall()
                        .accessibility_label("Add panel surface")
                        .dropdown_menu(move |mut menu, _, _| {
                            for tab in PanelTab::ALL {
                                let owner = owner.clone();
                                let enabled = available(tab);
                                menu = menu.item(
                                    PopupMenuItem::new(tab.label())
                                        .icon(icon(tab.icon()))
                                        .disabled(!enabled)
                                        .on_click(move |_, window, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.open_right_panel(tab, window, cx)
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                )
            })
    }

    /// Which surfaces can open now.
    fn surface_availability(&self) -> impl Fn(PanelTab) -> bool + 'static {
        let has_folder = !self.snapshot.cwd().is_empty();
        let terminal = self.snapshot.terminal_available();
        let thread = self.snapshot.selected_thread.is_some();
        move |tab| match tab {
            PanelTab::Browser => true,
            PanelTab::Terminal => terminal,
            PanelTab::Files => has_folder,
            PanelTab::Diff => thread,
        }
    }

    /// The list shown while the right panel has no surfaces.
    fn render_launcher(&self, cx: &mut Context<Self>) -> AnyElement {
        let available = self.surface_availability();
        let rows = PanelTab::ALL.into_iter().map(|tab| {
            let enabled = available(tab);
            h_flex()
                .id(SharedString::from(format!("open-surface-{}", tab.label())))
                .h_8()
                .w_full()
                .gap_2p5()
                .px_2p5()
                .rounded(px(8.))
                .text_sm()
                .map(|row| {
                    if enabled {
                        row.cursor_pointer()
                            .hover(|row| row.bg(tint("accentSurface", 0.6)))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                view.open_right_panel(tab, window, cx)
                            }))
                    } else {
                        row.text_color(color("textMuted")).opacity(0.64)
                    }
                })
                .child(icon(tab.icon()).size_4())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().truncate().child(tab.label()))
                        .when(!enabled, |label| {
                            label.child(
                                super::ui::text_2xs(div())
                                    .text_color(color("textMuted"))
                                    .child(tab.unavailable_hint()),
                            )
                        }),
                )
                .child(kbd(tab.key().to_uppercase()))
        });
        let available = self.surface_availability();
        div()
            .id("surface-launcher")
            .track_focus(&self.panels.launcher_focus)
            .flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .px_6()
            .pb(px(super::ui::metrics().header_height))
            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                let keystroke = &event.keystroke;
                if keystroke.modifiers.modified() {
                    return;
                }
                if let Some(tab) = PanelTab::ALL
                    .into_iter()
                    .find(|tab| tab.key() == keystroke.key && available(*tab))
                {
                    cx.stop_propagation();
                    view.open_right_panel(tab, window, cx);
                }
            }))
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(320.))
                    .py_6()
                    .child(
                        div()
                            .mb_3()
                            .text_center()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Open a surface"),
                    )
                    .child(v_flex().gap_0p5().children(rows)),
            )
            .into_any_element()
    }

    pub(crate) fn render_terminal_drawer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.panels.terminals.drawer_open() || self.panels.shows(PanelTab::Terminal) {
            return None;
        }
        Some(self.render_terminals(false, window, cx))
    }

    pub(crate) fn render_thread_details(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.panels.details_open || self.snapshot.selected_thread.is_none() {
            return None;
        }
        Some(self.render_details(window, cx))
    }

    /// Follows the snapshot: the diff source, file editor and terminals.
    pub(crate) fn sync_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_terminals(window, cx);
        self.sync_diff(cx);
        self.sync_files(window, cx);
        self.sync_browser(cx);
    }

    /// Shows the browser's native view only while its tab is on screen.
    pub(crate) fn sync_browser(&mut self, cx: &mut Context<Self>) {
        let visible = self.route == Route::Chat && self.panels.shows(PanelTab::Browser);
        if let Some(browser) = &self.panels.browser {
            browser.update(cx, |browser, cx| browser.set_visible(visible, cx));
        }
    }

    pub(crate) fn panel_outcome(
        &mut self,
        outcome: &Outcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Outcome::TerminalOpened { terminal_id } = outcome
            && let Some(thread) = self.thread_id()
        {
            self.show_terminal(thread, terminal_id.clone(), window, cx);
        }
    }

    pub(crate) fn open_right_panel(
        &mut self,
        tab: PanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.surface_availability()(tab) {
            return;
        }
        if tab == PanelTab::Browser && self.panels.browser.is_none() {
            match crate::browser::Browser::new(
                wry::WebViewBuilder::new(),
                #[cfg(target_os = "macos")]
                crate::browser::ChromeProfileSource::default(),
                window,
                cx,
            ) {
                Ok(browser) => self.panels.browser = Some(browser),
                Err(error) => {
                    self.show_error(&error, window, cx);
                    return;
                }
            }
        }
        if !self.panels.surfaces.contains(&tab) {
            self.panels.surfaces.push(tab);
        }
        self.panels.right_open = true;
        self.panels.active = Some(tab);
        if tab == PanelTab::Terminal {
            // The thread's terminals move from the drawer into the panel.
            if let Some(thread) = self.thread_id() {
                self.panels.terminals.set_drawer_open(&thread, false);
            }
            self.ensure_terminal(window, cx);
        }
        self.panels_changed(window, cx);
    }

    pub(crate) fn toggle_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panels.right_open = !self.panels.right_open;
        if self.panels.right_open && self.panels.active.is_none() {
            self.panels.launcher_focus.focus(window, cx);
        }
        self.panels_changed(window, cx);
    }

    fn close_surface(&mut self, tab: PanelTab, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.panels.surfaces.iter().position(|open| *open == tab) else {
            return;
        };
        self.panels.surfaces.remove(index);
        if self.panels.active == Some(tab) {
            self.panels.active = self
                .panels
                .surfaces
                .get(index.min(self.panels.surfaces.len().saturating_sub(1)))
                .copied();
        }
        if self.panels.active.is_none() {
            self.panels.launcher_focus.focus(window, cx);
        }
        self.panels_changed(window, cx);
    }

    pub(crate) fn toggle_terminal_drawer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.thread_id() else {
            return;
        };
        if !self.snapshot.terminal_available() {
            return;
        }
        let open = !self.panels.terminals.drawer_open();
        if open && self.panels.shows(PanelTab::Terminal) {
            self.close_surface(PanelTab::Terminal, window, cx);
        }
        self.panels.terminals.set_drawer_open(&thread, open);
        if open {
            self.ensure_terminal(window, cx);
        }
        self.panels_changed(window, cx);
    }

    pub(crate) fn toggle_thread_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.panels.details_open = !self.panels.details_open;
        self.panels_changed(window, cx);
    }

    /// Opens the Diff tab at a turn (the latest when `None`) and file.
    pub(crate) fn open_diff(
        &mut self,
        run_id: Option<String>,
        file_path: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.snapshot.selected_thread.is_none() {
            return;
        }
        // Selecting first keeps the panel from loading the previous selection.
        self.select_diff_turn(run_id, file_path);
        self.open_right_panel(PanelTab::Diff, window, cx);
    }

    /// Opens one of the thread's terminals in the drawer.
    pub(crate) fn open_thread_terminal(
        &mut self,
        thread_id: String,
        terminal_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.thread_id().as_deref() != Some(thread_id.as_str()) {
            self.open_thread(thread_id.clone(), cx);
        }
        self.show_terminal(thread_id, terminal_id, window, cx);
    }

    /// Panel keys: the terminal's own while one is focused, else the Diff
    /// toggle and closing the right panel.
    pub(crate) fn panel_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if !modifiers.secondary() || modifiers.alt {
            return false;
        }
        if self.terminal_focused(window, cx) {
            return self.terminal_key(keystroke.key.as_str(), modifiers.shift, window, cx);
        }
        match (keystroke.key.as_str(), modifiers.shift) {
            ("d", false) => {
                if self.panels.shows(PanelTab::Diff) {
                    self.panels.right_open = false;
                    self.panels_changed(window, cx);
                } else {
                    self.open_right_panel(PanelTab::Diff, window, cx);
                }
                true
            }
            ("w", false) if self.panels.right_open => {
                self.panels.right_open = false;
                self.panels_changed(window, cx);
                true
            }
            _ => false,
        }
    }

    /// Derives the views again after a panel opened or closed.
    fn panels_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_panels(window, cx);
        self.refresh_views(cx);
        cx.notify();
    }
}

/// A 16 px slot showing `icon_name` that turns into a close button while its
/// row (`group`) is hovered.
fn tab_close_button(
    id: SharedString,
    group: &'static str,
    icon_name: &str,
    label: String,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let tooltip: SharedString = label.into();
    div()
        .id(id)
        .size_4()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_sm()
        .cursor_pointer()
        .hover(|button| button.bg(color("muted")))
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
        })
        .on_click(on_click)
        .child(
            div()
                .group_hover(group, |slot| slot.invisible())
                .child(icon(icon_name).size_3()),
        )
        .child(
            div()
                .absolute()
                .invisible()
                .group_hover(group, |close| close.visible())
                .child(icon("x").size_3()),
        )
}

/// A key cap such as `B`.
fn kbd(key: String) -> impl IntoElement {
    h_flex()
        .h_5()
        .min_w_5()
        .px_1()
        .justify_center()
        .rounded(px(4.))
        .bg(color("muted"))
        .text_color(color("textMuted"))
        .text_xs()
        .font_weight(FontWeight::MEDIUM)
        .child(key)
}

/// `Name (⌘K)`.
fn with_shortcut(label: &str, key: &str) -> String {
    format!("{label} ({})", shortcut(key))
}

#[cfg(test)]
mod tests {
    use super::{PANEL_MIN_WIDTH, clamp_panel_width};
    use core::prelude::v1::test;

    #[test]
    fn panel_width_leaves_the_chat_column_room_and_never_drops_below_its_minimum() {
        assert_eq!(clamp_panel_width(540., 1440.), 540.);
        assert_eq!(clamp_panel_width(2000., 1440.), 1440. * 0.7);
        assert_eq!(clamp_panel_width(100., 1440.), PANEL_MIN_WIDTH);
        assert_eq!(clamp_panel_width(600., 1000.), 640.0f32.min(600.));
        assert_eq!(clamp_panel_width(900., 800.), 440.);
        assert_eq!(clamp_panel_width(900., 500.), PANEL_MIN_WIDTH);
    }
}

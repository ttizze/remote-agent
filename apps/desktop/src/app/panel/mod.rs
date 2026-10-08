//! The right panel's surfaces (browsers, terminals, Files, Diff), the thread
//! terminal drawer and the thread details card.
mod details;
mod device;
mod diff;
mod files;
mod pull_requests;
mod terminal_drawer;

use super::{
    Desktop, Route,
    ui::{color, icon, shortcut, tint},
};
use agent_core::{state::Intent, view::header::HeaderPanelState};
use gpui_kit::{
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::collections::HashMap;

const PANEL_MIN_WIDTH: f32 = 360.;
const PANEL_MAX_FRACTION: f32 = 0.7;
/// The chat column keeps this much beside the panel.
const SIBLING_MIN_WIDTH: f32 = 360.;

/// The kinds of surface the right panel opens.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PanelTab {
    Diff,
    Terminal,
    Files,
    Browser,
    PullRequests,
    Device,
}
impl PanelTab {
    /// The order the surface menus list them in.
    const ALL: [PanelTab; 6] = [
        PanelTab::Browser,
        PanelTab::Terminal,
        PanelTab::Files,
        PanelTab::Diff,
        PanelTab::PullRequests,
        PanelTab::Device,
    ];
    fn label(self) -> &'static str {
        match self {
            PanelTab::Browser => "Browser",
            PanelTab::Terminal => "Terminal",
            PanelTab::Files => "Files",
            PanelTab::Diff => "Diff",
            PanelTab::PullRequests => "Pull requests",
            PanelTab::Device => "Device",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            PanelTab::Browser => "globe",
            PanelTab::Terminal => "square-terminal",
            PanelTab::Files => "files",
            PanelTab::Diff => "file-diff",
            PanelTab::PullRequests => "git-pull-request",
            PanelTab::Device => "smartphone",
        }
    }
    fn key(self) -> &'static str {
        match self {
            PanelTab::Browser => "b",
            PanelTab::Terminal => "t",
            PanelTab::Files => "f",
            PanelTab::Diff => "d",
            PanelTab::PullRequests => "p",
            PanelTab::Device => "e",
        }
    }
    fn unavailable_hint(self) -> &'static str {
        match self {
            PanelTab::Browser => "Only available in the desktop app.",
            PanelTab::Terminal | PanelTab::Files => "Available when a project is open.",
            PanelTab::Diff => "Available for Git repositories.",
            PanelTab::PullRequests => "Available for GitHub repositories.",
            PanelTab::Device => "Available when a thread is open.",
        }
    }
}

/// One open surface. Browsers and terminals may be open several times; a
/// terminal surface holds up to four split terminals.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Surface {
    Diff,
    PullRequests,
    Files,
    Browser {
        id: u64,
    },
    Terminal {
        /// The first terminal's id, which names the surface for good.
        key: String,
        terminal_ids: Vec<String>,
        active: String,
        stacked: bool,
    },
    Device,
}
impl Surface {
    fn id(&self) -> String {
        match self {
            Surface::Diff => "diff".into(),
            Surface::PullRequests => "pull-requests".into(),
            Surface::Files => "files".into(),
            Surface::Browser { id } => format!("browser:{id}"),
            Surface::Terminal { key, .. } => format!("terminal:{key}"),
            Surface::Device => "device".into(),
        }
    }
    fn kind(&self) -> PanelTab {
        match self {
            Surface::Diff => PanelTab::Diff,
            Surface::PullRequests => PanelTab::PullRequests,
            Surface::Files => PanelTab::Files,
            Surface::Browser { .. } => PanelTab::Browser,
            Surface::Terminal { .. } => PanelTab::Terminal,
            Surface::Device => PanelTab::Device,
        }
    }
    fn terminal(terminal_id: String) -> Self {
        Surface::Terminal {
            key: terminal_id.clone(),
            terminal_ids: vec![terminal_id.clone()],
            active: terminal_id,
            stacked: false,
        }
    }
}

static NO_PANEL: ThreadPanel = ThreadPanel::EMPTY;

/// One thread's right panel: whether it is open, its surfaces in tab order
/// and the active one.
#[derive(Clone, Default, PartialEq, Debug)]
pub(crate) struct ThreadPanel {
    open: bool,
    surfaces: Vec<Surface>,
    active: Option<String>,
}
impl ThreadPanel {
    const EMPTY: ThreadPanel = ThreadPanel {
        open: false,
        surfaces: Vec::new(),
        active: None,
    };
    fn active_surface(&self) -> Option<&Surface> {
        let active = self.active.as_ref()?;
        self.surfaces.iter().find(|surface| &surface.id() == active)
    }
    fn active_kind(&self) -> Option<PanelTab> {
        self.active_surface().map(Surface::kind)
    }
    /// Adds the surface, or keeps the one already open with its id, and
    /// makes it active.
    fn upsert(&mut self, surface: Surface) {
        let id = surface.id();
        if !self.surfaces.iter().any(|open| open.id() == id) {
            self.surfaces.push(surface);
        }
        self.active = Some(id);
        self.open = true;
    }
    /// Removes a surface; its neighbour becomes active, and the panel closes
    /// once nothing is left.
    fn close(&mut self, id: &str) {
        let Some(index) = self.surfaces.iter().position(|open| open.id() == id) else {
            return;
        };
        self.surfaces.remove(index);
        if self.active.as_deref() == Some(id) {
            self.active = self
                .surfaces
                .get(index.min(self.surfaces.len().saturating_sub(1)))
                .map(Surface::id);
        }
        if self.surfaces.is_empty() {
            self.open = false;
        }
    }
    /// The terminal ids every terminal surface holds.
    fn terminal_ids(&self) -> impl Iterator<Item = &String> {
        self.surfaces.iter().flat_map(|surface| match surface {
            Surface::Terminal { terminal_ids, .. } => terminal_ids.as_slice(),
            _ => &[],
        })
    }
    /// Takes a terminal out of the surface holding it; a surface left empty
    /// closes.
    fn remove_terminal(&mut self, terminal_id: &str) {
        let Some(surface) = self.surfaces.iter_mut().find(|surface| {
            matches!(surface, Surface::Terminal { terminal_ids, .. } if terminal_ids.iter().any(|id| id == terminal_id))
        }) else {
            return;
        };
        let id = surface.id();
        let Surface::Terminal {
            terminal_ids,
            active,
            ..
        } = surface
        else {
            return;
        };
        terminal_ids.retain(|id| id != terminal_id);
        if active == terminal_id {
            *active = terminal_ids.last().cloned().unwrap_or_default();
        }
        if terminal_ids.is_empty() {
            self.close(&id);
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
    /// Each thread's right panel, by thread id (`""` for the new-thread draft).
    threads: HashMap<String, ThreadPanel>,
    width: f32,
    launcher_focus: FocusHandle,
    browsers: HashMap<u64, Entity<crate::browser::Browser>>,
    preview_browsers: HashMap<u64, Entity<crate::browser::HostBrowser>>,
    next_browser: u64,
    diff: diff::DiffState,
    files: files::FilesState,
    terminals: terminal_drawer::TerminalState,
    device: device::DeviceState,
    details: details::DetailsState,
    pull_requests: pull_requests::PullRequestsState,
    _subscriptions: Vec<Subscription>,
}
impl PanelState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let mut subscriptions = vec![];
        Self {
            threads: HashMap::new(),
            width: super::ui::metrics().panel_width,
            launcher_focus: cx.focus_handle(),
            browsers: HashMap::new(),
            preview_browsers: HashMap::new(),
            next_browser: 0,
            diff: diff::DiffState::new(window, cx, &mut subscriptions),
            files: files::FilesState::new(window, cx, &mut subscriptions),
            terminals: terminal_drawer::TerminalState::default(),
            device: device::DeviceState::new(window, cx),
            details: details::DetailsState::default(),
            pull_requests: pull_requests::PullRequestsState::default(),
            _subscriptions: subscriptions,
        }
    }
    /// Closes what belonged to the previous connection.
    pub(crate) fn reset(&mut self, _: &mut Window, cx: &mut Context<Desktop>) {
        self.terminals = terminal_drawer::TerminalState::default();
        self.device.reset();
        self.diff.reset();
        self.files.reset();
        self.details = details::DetailsState::default();
        self.pull_requests = pull_requests::PullRequestsState::default();
        for browser in self.browsers.values() {
            browser.update(cx, |browser, cx| browser.set_visible(false, cx));
        }
        for browser in self.preview_browsers.values() {
            browser.update(cx, |browser, cx| {
                browser.close();
                browser.set_visible(false, cx);
            });
        }
        self.browsers.clear();
        self.preview_browsers.clear();
        self.threads.clear();
    }
}

impl Desktop {
    /// The key of the shown thread's panels.
    fn panel_thread(&self) -> String {
        self.thread_id().unwrap_or_default()
    }
    fn right(&self) -> &ThreadPanel {
        self.panels
            .threads
            .get(&self.panel_thread())
            .unwrap_or(&NO_PANEL)
    }
    fn right_mut(&mut self) -> &mut ThreadPanel {
        let key = self.panel_thread();
        self.panels.threads.entry(key).or_default()
    }
    /// The right panel is open on a surface of this kind.
    pub(crate) fn panel_shows(&self, tab: PanelTab) -> bool {
        let right = self.right();
        right.open && right.active_kind() == Some(tab)
    }

    /// The panels the header shows as open.
    pub(crate) fn header_panels(&self) -> HeaderPanelState {
        HeaderPanelState {
            thread_panel_open: self.details_open(),
            terminal_open: self.panels.terminals.drawer_open(),
            right_panel_open: self.right().open,
            files_open: self.panel_shows(PanelTab::Files),
        }
    }

    pub(crate) fn render_right_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let panel_key = self.panel_thread();
        let right = self.right().clone();
        let duration = Desktop::panel_animation_duration();
        let animation = self.right_panel_animation.prepare(
            &panel_key,
            if right.open { 1. } else { 0. },
            duration,
        );
        if !right.open && !self.panels.threads.contains_key(&panel_key) {
            return None;
        }
        let viewport = window.viewport_size().width.as_f32();
        let width = clamp_panel_width(self.panels.width, viewport);
        let open = right.open;
        let body = match right.active_surface().cloned() {
            None => self.render_launcher(cx),
            Some(Surface::Browser { id }) => {
                if let Some(browser) = self.panels.preview_browsers.get(&id) {
                    div()
                        .flex_1()
                        .min_h_0()
                        .child(browser.clone())
                        .into_any_element()
                } else {
                    match self.panels.browsers.get(&id) {
                        Some(browser) => div()
                            .flex_1()
                            .min_h_0()
                            .child(browser.clone())
                            .into_any_element(),
                        None => div().flex_1().into_any_element(),
                    }
                }
            }
            Some(surface @ Surface::Terminal { .. }) => {
                self.render_terminals(Some(surface.id()), window, cx)
            }
            Some(Surface::Files) => self.render_files(window, cx),
            Some(Surface::Diff) => self.render_diff(window, cx),
            Some(Surface::PullRequests) => self.render_pull_requests(window, cx),
            Some(Surface::Device) => self.render_device(cx),
        };
        let panel = v_flex()
            .id("right-panel")
            .relative()
            .w(px(if open { width } else { 0. }))
            .flex_shrink_0()
            .h_full()
            .overflow_hidden()
            .bg(color("canvas"))
            .border_l_1()
            .border_color(color("border"))
            .on_drag_move(
                cx.listener(|view, event: &DragMoveEvent<PanelResize>, window, cx| {
                    let viewport = window.viewport_size().width.as_f32();
                    let right = event.bounds.right().as_f32();
                    view.panels.width =
                        clamp_panel_width(right - event.event.position.x.as_f32(), viewport);
                    cx.notify();
                }),
            )
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
            );
        Some(match animation {
            Some(animation) => panel
                .with_animation(
                    ("desktop-right-panel-animation", animation.run),
                    Animation::new(animation.duration).with_easing(super::panel_ease_out),
                    move |panel, delta| {
                        let progress = animation.from + (animation.target - animation.from) * delta;
                        panel.w(px(width * progress))
                    },
                )
                .into_any_element(),
            None => panel.into_any_element(),
        })
    }

    /// Keeps the active Host Preview visible as a small player while the full
    /// panel is closed. The same entity remains the source of frames and input.
    pub(crate) fn render_preview_mini_player(
        &self,
        cx: &mut Context<Desktop>,
    ) -> Option<AnyElement> {
        let right = self.right();
        if right.open {
            return None;
        }
        let Surface::Browser { id } = right.active_surface()? else {
            return None;
        };
        let browser = self.panels.preview_browsers.get(id)?.clone();
        Some(
            div()
                .id("preview-mini-player")
                .absolute()
                .right_4()
                .bottom_4()
                .w(px(320.))
                .h(px(220.))
                .rounded_md()
                .border_1()
                .border_color(color("border"))
                .bg(color("canvas"))
                .shadow_lg()
                .child(browser)
                .into_any_element(),
        )
    }

    /// A surface's tab title.
    fn surface_title(&self, surface: &Surface, cx: &App) -> String {
        match surface {
            Surface::Browser { id } => self
                .panels
                .preview_browsers
                .get(id)
                .map_or_else(
                    || {
                        self.panels
                            .browsers
                            .get(id)
                            .map_or_else(|| "Browser".into(), |browser| browser.read(cx).title(cx))
                    },
                    |browser| browser.read(cx).title(cx),
                ),
            Surface::Terminal { active, .. } => {
                let thread = self.panel_thread();
                self.snapshot
                    .terminals(thread)
                    .into_iter()
                    .find(|tab| &tab.terminal_id == active)
                    .map_or_else(
                        || agent_protocol::operations::terminal_label(active),
                        |tab| tab.label,
                    )
            }
            other => other.kind().label().into(),
        }
    }

    fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = super::ui::metrics();
        let right = self.right();
        let ids: Vec<String> = right.surfaces.iter().map(Surface::id).collect();
        let owner = cx.entity().downgrade();
        let tabs: Vec<_> = right
            .surfaces
            .iter()
            .enumerate()
            .map(|(index, surface)| {
                let id = surface.id();
                let active = right.active.as_ref() == Some(&id);
                let title = self.surface_title(surface, cx);
                let (select, close) = (id.clone(), id.clone());
                let (owner, ids) = (owner.clone(), ids.clone());
                h_flex()
                    .id(SharedString::from(format!("panel-tab-{id}")))
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
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.select_surface(select.clone(), window, cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener({
                            let id = id.clone();
                            move |view, _, window, cx| view.request_close_surface(&id, window, cx)
                        }),
                    )
                    .child(tab_close_button(
                        SharedString::from(format!("panel-tab-close-{id}")),
                        "panel-tab",
                        surface.kind().icon(),
                        format!("Close {title}"),
                        cx.listener(move |view, _, window, cx| {
                            cx.stop_propagation();
                            view.request_close_surface(&close, window, cx);
                        }),
                    ))
                    .child(div().min_w_0().truncate().child(title))
                    .context_menu(move |menu, _, _| {
                        let others: Vec<String> = ids
                            .iter()
                            .filter(|other| **other != ids[index])
                            .cloned()
                            .collect();
                        let right: Vec<String> = ids[index + 1..].to_vec();
                        let item = |label: &'static str, close: Vec<String>, confirm: bool| {
                            let owner = owner.clone();
                            let disabled = close.is_empty();
                            PopupMenuItem::new(label).disabled(disabled).on_click(
                                move |_, window, cx| {
                                    let _ = owner.update(cx, |view, cx| {
                                        view.close_surfaces(close.clone(), confirm, window, cx)
                                    });
                                },
                            )
                        };
                        menu.item(item("Close", vec![ids[index].clone()], true))
                            .item(item("Close others", others, false))
                            .item(item("Close to the right", right, false))
                            .item(item("Close all", ids.clone(), false))
                    })
            })
            .collect();
        let owner = cx.entity().downgrade();
        let available = self.surface_availability();
        h_flex()
            .h(px(metrics.header_height))
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .pl_2()
            .pr_3()
            .child(
                h_flex()
                    .id("panel-tabs")
                    .min_w_0()
                    .gap_1()
                    .overflow_x_scroll()
                    .children(tabs),
            )
            .when(!right.surfaces.is_empty(), |bar| {
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
                                let tab_key = tab.key().to_uppercase();
                                menu = menu.item(
                                    PopupMenuItem::element(move |_, _| {
                                        h_flex()
                                            .w_full()
                                            .gap_2()
                                            .child(tab.label())
                                            .child(div().ml_auto().child(kbd(tab_key.clone())))
                                    })
                                    .icon(icon(tab.icon()))
                                    .disabled(!enabled)
                                    .on_click(
                                        move |_, window, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.add_surface(tab, window, cx)
                                            });
                                        },
                                    ),
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
        let pull_requests = self.snapshot.selected_project.is_some() && self.snapshot.connected;
        move |tab| match tab {
            PanelTab::Browser => true,
            PanelTab::Terminal => terminal,
            PanelTab::Files => has_folder,
            PanelTab::Diff => thread,
            PanelTab::PullRequests => pull_requests,
            PanelTab::Device => thread,
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
                                view.add_surface(tab, window, cx)
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
                    view.add_surface(tab, window, cx);
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
        let open = self.panels.terminals.drawer_open();
        let thread_key = self.thread_id().unwrap_or_default();
        let duration = Desktop::panel_animation_duration();
        let animation = self.terminal_drawer_animation.prepare(
            &thread_key,
            if open { 1. } else { 0. },
            duration,
        );
        if !open && !self.panels.terminals.drawer_present() {
            return None;
        }
        let height = self
            .panels
            .terminals
            .drawer_height(window.viewport_size().height.as_f32());
        let drawer = v_flex()
            .id("terminal-drawer-animation")
            .h(px(if open { height } else { 0. }))
            .flex_shrink_0()
            .overflow_hidden()
            .child(self.render_terminals(None, window, cx));
        Some(match animation {
            Some(animation) => drawer
                .with_animation(
                    ("desktop-terminal-drawer-animation", animation.run),
                    Animation::new(animation.duration).with_easing(super::panel_ease_out),
                    move |drawer, delta| {
                        let progress = animation.from + (animation.target - animation.from) * delta;
                        drawer.h(px(height * progress))
                    },
                )
                .into_any_element(),
            None => drawer.into_any_element(),
        })
    }

    /// Follows the snapshot: the diff source, file editor and terminals.
    pub(crate) fn sync_panels(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_terminals(window, cx);
        self.sync_diff(cx);
        self.sync_files(window, cx);
        self.sync_pull_requests();
        self.sync_browser(cx);
        self.sync_device(cx);
    }

    /// Shows a browser's native view only while its tab is on screen.
    pub(crate) fn sync_browser(&mut self, cx: &mut Context<Self>) {
        let shown = match (self.route, self.right()) {
            (Route::Chat, right) if right.open => match right.active_surface() {
                Some(Surface::Browser { id }) => Some(*id),
                _ => None,
            },
            _ => None,
        };
        for (id, browser) in &self.panels.browsers {
            let visible = shown == Some(*id);
            browser.update(cx, |browser, cx| browser.set_visible(visible, cx));
        }
        for (id, browser) in &self.panels.preview_browsers {
            let visible = shown == Some(*id);
            browser.update(cx, |browser, cx| browser.set_visible(visible, cx));
        }
    }

    /// Opens a surface of `tab`: Diff and Files once, a browser or terminal
    /// as a new tab each time.
    pub(crate) fn add_surface(
        &mut self,
        tab: PanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.surface_availability()(tab) {
            return;
        }
        match tab {
            PanelTab::Diff => self.right_mut().upsert(Surface::Diff),
            PanelTab::Files => self.right_mut().upsert(Surface::Files),
            PanelTab::PullRequests => self.right_mut().upsert(Surface::PullRequests),
            PanelTab::Browser => {
                self.panels.next_browser += 1;
                let id = self.panels.next_browser;
                if let (Some(store), Some(thread)) = (self.store(), self.thread_id()) {
                    let resolved_browser = self.snapshot.preferences.browser.resolved();
                    let browser = crate::browser::HostBrowser::new(
                        store,
                        thread,
                        crate::browser::PreviewDefaults {
                            viewport: resolved_browser.viewport,
                            appearance: resolved_browser.appearance,
                            zoom: resolved_browser.zoom,
                            profile_id: Some(resolved_browser.profile_id),
                            recording_options:
                                agent_protocol::preview::PreviewRecordingOptions {
                                    frame_rate: resolved_browser.recording_frame_rate as u8,
                                    show_key_presses: resolved_browser.recording_show_key_presses,
                                    show_mouse_presses: resolved_browser.recording_show_mouse_presses,
                                },
                        },
                        window,
                        cx,
                    );
                    self.panels.preview_browsers.insert(id, browser);
                } else {
                    match crate::browser::Browser::new(
                        wry::WebViewBuilder::new(),
                        #[cfg(target_os = "macos")]
                        crate::browser::ChromeProfileSource::default(),
                        window,
                        cx,
                    ) {
                        Ok(browser) => {
                            self.panels.browsers.insert(id, browser);
                        }
                        Err(error) => {
                            self.show_error(&error, window, cx);
                            return;
                        }
                    }
                }
                self.right_mut().upsert(Surface::Browser { id });
            }
            PanelTab::Terminal => {
                if !self.add_terminal_surface() {
                    return;
                }
            }
            PanelTab::Device => self.right_mut().upsert(Surface::Device),
        }
        self.panels_changed(window, cx);
    }

    /// Opens the right panel on the surface of `tab` already open, else a new one.
    pub(crate) fn open_right_panel(
        &mut self,
        tab: PanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self
            .right()
            .surfaces
            .iter()
            .find(|surface| surface.kind() == tab)
            .map(Surface::id);
        match existing {
            Some(id) => {
                let right = self.right_mut();
                right.open = true;
                right.active = Some(id);
                self.panels_changed(window, cx);
            }
            None => self.add_surface(tab, window, cx),
        }
    }

    fn select_surface(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let focus_terminal = match self
            .right()
            .surfaces
            .iter()
            .find(|surface| surface.id() == id)
        {
            Some(Surface::Terminal { active, .. }) => Some(active.clone()),
            _ => None,
        };
        self.right_mut().active = Some(id);
        if let (Some(terminal), Some(thread)) = (focus_terminal, self.thread_id()) {
            self.panels.terminals.request_focus(thread, terminal);
        }
        self.panels_changed(window, cx);
    }

    pub(crate) fn toggle_right_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let right = self.right_mut();
        right.open = !right.open;
        if right.open && right.surfaces.is_empty() {
            self.panels.launcher_focus.focus(window, cx);
        }
        self.panels_changed(window, cx);
    }

    /// Closes a surface; a terminal surface asks first, as it stops its
    /// processes.
    fn request_close_surface(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let surface = self
            .right()
            .surfaces
            .iter()
            .find(|surface| surface.id() == id)
            .cloned();
        match surface {
            Some(Surface::Terminal { terminal_ids, .. }) => {
                self.confirm_close_terminals(terminal_ids, window, cx)
            }
            Some(_) => self.close_surface(id, window, cx),
            None => {}
        }
    }

    /// Closes several surfaces; only a single close asks before stopping
    /// terminals.
    fn close_surfaces(
        &mut self,
        ids: Vec<String>,
        confirm: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if confirm && let [id] = ids.as_slice() {
            self.request_close_surface(id, window, cx);
            return;
        }
        for id in ids {
            let surface = self
                .right()
                .surfaces
                .iter()
                .find(|surface| surface.id() == id)
                .cloned();
            match (surface, self.thread_id()) {
                (Some(Surface::Terminal { terminal_ids, .. }), Some(thread)) => {
                    self.close_terminals(thread, terminal_ids, window, cx)
                }
                (Some(_), _) => self.close_surface(&id, window, cx),
                (None, _) => {}
            }
        }
    }

    fn close_surface(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let closing_device = self
            .right()
            .surfaces
            .iter()
            .find(|surface| surface.id() == id)
            .is_some_and(|surface| matches!(surface, Surface::Device));
        if let Some(Surface::Browser { id: browser }) = self
            .right()
            .surfaces
            .iter()
            .find(|surface| surface.id() == id)
            .cloned()
            && let Some(browser) = self.panels.browsers.remove(&browser)
        {
            browser.update(cx, |browser, cx| browser.set_visible(false, cx));
        }
        if let Some(Surface::Browser { id: browser }) = self
            .right()
            .surfaces
            .iter()
            .find(|surface| surface.id() == id)
            .cloned()
            && let Some(browser) = self.panels.preview_browsers.remove(&browser)
        {
            browser.update(cx, |browser, cx| {
                browser.close();
                browser.set_visible(false, cx);
            });
        }
        let empty = {
            let right = self.right_mut();
            right.close(id);
            right.surfaces.is_empty()
        };
        if closing_device {
            self.panels.device.subscribed = false;
            self.perform(Intent::UnsubscribeDevice);
        }
        if empty {
            self.panels.launcher_focus.focus(window, cx);
        }
        self.panels_changed(window, cx);
    }

    pub(crate) fn toggle_thread_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.details_just_dismissed() {
            return;
        }
        let open = !self.details_open();
        self.set_details_open(open);
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

    /// Opens one of the thread's terminals where it is held: its panel
    /// surface, else the drawer.
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

    /// Opens the Diff surface, or closes the panel while it shows.
    pub(crate) fn toggle_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel_shows(PanelTab::Diff) {
            self.right_mut().open = false;
            self.panels_changed(window, cx);
        } else {
            self.open_right_panel(PanelTab::Diff, window, cx);
        }
    }

    /// Closes the active surface of an open panel.
    pub(crate) fn close_active_surface(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.right().open {
            return false;
        }
        match self.right().active.clone() {
            Some(id) => self.request_close_surface(&id, window, cx),
            None => {
                self.right_mut().open = false;
                self.panels_changed(window, cx);
            }
        }
        true
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
    use super::{PANEL_MIN_WIDTH, Surface, ThreadPanel, clamp_panel_width};
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

    #[test]
    fn browsers_and_terminals_open_as_many_tabs_as_asked_while_diff_stays_single() {
        let mut panel = ThreadPanel::default();
        panel.upsert(Surface::Diff);
        panel.upsert(Surface::Browser { id: 1 });
        panel.upsert(Surface::Browser { id: 2 });
        panel.upsert(Surface::terminal("term-1".into()));
        panel.upsert(Surface::terminal("term-2".into()));
        panel.upsert(Surface::Diff);
        let ids: Vec<String> = panel.surfaces.iter().map(Surface::id).collect();
        assert_eq!(
            ids,
            [
                "diff",
                "browser:1",
                "browser:2",
                "terminal:term-1",
                "terminal:term-2"
            ]
        );
        assert_eq!(panel.active.as_deref(), Some("diff"));
        assert!(panel.open);
    }

    #[test]
    fn closing_moves_to_the_neighbour_and_the_last_close_shuts_the_panel() {
        let mut panel = ThreadPanel::default();
        panel.upsert(Surface::Diff);
        panel.upsert(Surface::Files);
        panel.upsert(Surface::Browser { id: 1 });
        panel.active = Some("files".into());
        panel.close("files");
        assert_eq!(panel.active.as_deref(), Some("browser:1"));
        panel.close("browser:1");
        assert_eq!(panel.active.as_deref(), Some("diff"));
        panel.close("diff");
        assert!(!panel.open);
        assert_eq!(panel.active, None);
    }

    #[test]
    fn a_terminal_surface_closes_with_its_last_terminal() {
        let mut panel = ThreadPanel::default();
        panel.upsert(Surface::terminal("term-1".into()));
        if let Some(Surface::Terminal {
            terminal_ids,
            active,
            ..
        }) = panel.surfaces.first_mut()
        {
            terminal_ids.push("term-3".into());
            *active = "term-3".into();
        }
        panel.remove_terminal("term-3");
        assert_eq!(
            panel.surfaces,
            [Surface::Terminal {
                key: "term-1".into(),
                terminal_ids: vec!["term-1".into()],
                active: "term-1".into(),
                stacked: false,
            }]
        );
        assert_eq!(
            panel.terminal_ids().cloned().collect::<Vec<_>>(),
            ["term-1"]
        );
        panel.remove_terminal("term-1");
        assert!(panel.surfaces.is_empty());
        assert!(!panel.open);
    }
}

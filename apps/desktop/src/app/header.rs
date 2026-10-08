//! The chat header: project crumb, thread title with its action menu and
//! inline rename, and the panel toggles.
use super::{
    Desktop,
    menus::MenuSurface,
    settings::SettingsPage,
    sidebar::window_drag,
    ui::{self, color, icon},
};
use agent_core::view::header::{
    HeaderInput, PanelToggle, ThreadHeaderView, snapshot_thread_header, thread_header,
};
use gpui_kit::{
    component::{h_flex, menu::PopupMenuItem, tooltip::Tooltip},
    prelude::FluentBuilder,
    *,
};
use std::{cell::Cell, rc::Rc};

/// The title a new-thread draft shows.
const DRAFT_TITLE: &str = "New thread";

pub(crate) struct HeaderState {
    /// Where the title button was laid out; its menu opens under it.
    title_bounds: Rc<Cell<Bounds<Pixels>>>,
}
impl HeaderState {
    pub(crate) fn new(_: &mut Window, _: &mut Context<Desktop>) -> Self {
        Self {
            title_bounds: Rc::default(),
        }
    }
}

#[derive(Clone, Copy)]
enum Panel {
    ThreadDetails,
    Terminal,
    Right,
}

fn stop_mouse_down<E: InteractiveElement>(element: E) -> E {
    element.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
}

impl Desktop {
    /// The open thread's header, or the draft's while none is open.
    fn header_view(&self) -> ThreadHeaderView {
        let panels = self.header_panels();
        if let Some(thread) = &self.snapshot.selected_thread
            && let Some(header) = snapshot_thread_header(&self.snapshot, thread, &panels)
        {
            return header;
        }
        let project = self
            .views
            .new_thread
            .as_ref()
            .and_then(|draft| draft.project_id.as_deref())
            .filter(|_| self.snapshot.selected_thread.is_none())
            .and_then(|id| {
                self.snapshot
                    .shell_projects()
                    .iter()
                    .find(|project| project.id == id)
            });
        thread_header(
            &HeaderInput {
                title: if self.snapshot.selected_thread.is_some() {
                    ""
                } else {
                    DRAFT_TITLE
                },
                project,
                workspace: None,
                is_server_thread: false,
                environment_label: self.snapshot.environment_display_label(),
                environment_unavailable: !self.snapshot.connected,
                merge_back_available: false,
            },
            &panels,
        )
    }

    fn render_panel_toggle(
        &self,
        id: &'static str,
        glyph: &'static str,
        toggle: &PanelToggle,
        shortcut: Option<String>,
        panel: Panel,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let tooltip: SharedString = match (&shortcut, toggle.available) {
            (Some(shortcut), true) => format!("{} ({shortcut})", toggle.tooltip).into(),
            _ => toggle.tooltip.clone().into(),
        };
        let available = toggle.available;
        stop_mouse_down(
            div()
                .id(id)
                .relative()
                .size_7()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.))
                .text_color(if available {
                    color("text")
                } else {
                    color("textMuted")
                })
                .when(toggle.open, |button| {
                    button
                        .bg(color("accentSurface"))
                        .text_color(color("accentSurfaceForeground"))
                })
                .when(available, |button| {
                    button
                        .cursor_pointer()
                        .hover(|button| button.bg(color("accentSurface")))
                })
                .child(icon(glyph).size_4().opacity(0.8))
                .when(toggle.attention, |button| {
                    button.child(
                        div()
                            .absolute()
                            .right(px(3.))
                            .top(px(3.))
                            .size(px(10.))
                            .rounded_full()
                            .border_2()
                            .border_color(color("canvas"))
                            .bg(color("warning")),
                    )
                })
                .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                .on_click(cx.listener(move |view, _, window, cx| {
                    if !available {
                        return;
                    }
                    match panel {
                        Panel::ThreadDetails => view.toggle_thread_details(window, cx),
                        Panel::Terminal => view.toggle_terminal_drawer(window, cx),
                        Panel::Right => view.toggle_right_panel(window, cx),
                    }
                })),
        )
    }

    fn render_title(&mut self, header: &ThreadHeaderView, cx: &mut Context<Self>) -> AnyElement {
        let thread_id = self.thread_id();
        let renaming = self.menus.rename.as_ref().is_some_and(|rename| {
            rename.surface == MenuSurface::Header && Some(&rename.thread_id) == thread_id.as_ref()
        });
        if renaming && let Some(input) = self.render_rename_input(cx) {
            return input;
        }
        let title: SharedString = header.title.clone().into();
        let tip = title.clone();
        let text = div()
            .min_w_0()
            .truncate()
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .child(title);
        let Some(thread_id) = thread_id.filter(|_| header.is_server_thread) else {
            return div()
                .id("thread-title")
                .min_w_0()
                .flex_1()
                .child(text)
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .into_any_element();
        };
        let bounds = self.header.title_bounds.clone();
        stop_mouse_down(
            h_flex()
                .id("thread-title")
                .group("thread-title")
                .min_w_0()
                .max_w_full()
                .gap_1()
                .rounded(px(6.))
                .cursor_pointer()
                .child(text)
                .child(
                    div()
                        .flex_shrink_0()
                        .opacity(0.)
                        .group_hover("thread-title", |chevron| chevron.opacity(1.))
                        .child(
                            icon("chevron-down")
                                .size_3p5()
                                .text_color(color("textMuted")),
                        ),
                )
                .child(
                    canvas(move |layout, _, _| bounds.set(layout), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(move |view, event: &ClickEvent, window, cx| {
                    if event.click_count() >= 2 {
                        view.close_menu(cx);
                        view.start_rename(thread_id.clone(), MenuSurface::Header, window, cx);
                        return;
                    }
                    let bounds = view.header.title_bounds.get();
                    let position = point(bounds.left(), bounds.bottom() + px(4.));
                    view.open_thread_menu(
                        thread_id.clone(),
                        MenuSurface::Header,
                        position,
                        window,
                        cx,
                    );
                })),
        )
        .into_any_element()
    }

    pub(crate) fn render_header(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let header = self.header_view();
        let crumb = header.project.clone().map(|project| {
            let tip: SharedString = project.new_thread_label.clone().into();
            let id = project.id.clone();
            stop_mouse_down(
                h_flex()
                    .id("project-crumb")
                    .min_w_0()
                    .flex_shrink(1.)
                    .gap_1p5()
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_color(color("textMuted"))
                    .hover(|crumb| crumb.text_color(color("text")))
                    .child(self.project_icon(&project.id, &project.name, 14.))
                    .child(div().max_w(px(160.)).truncate().child(project.name.clone()))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(
                        cx.listener(move |view, _, _, cx| view.new_thread(Some(id.clone()), cx)),
                    ),
            )
        });
        let server_thread = header.is_server_thread;
        let draft_project = header
            .project
            .as_ref()
            .filter(|_| !server_thread)
            .map(|project| project.id.clone());
        let breadcrumb = h_flex()
            .min_w_0()
            .flex_shrink(1.)
            .gap_3()
            .text_sm()
            .when_some(crumb, |row, crumb| {
                row.child(crumb)
                    .child(div().text_color(color("textMuted")).child("/"))
            })
            .child(self.render_title(&header, cx));
        let controls = h_flex()
            .flex_shrink_0()
            .gap_1()
            .child(self.render_panel_toggle(
                "toggle-thread-details",
                "square-menu",
                &header.thread_panel,
                None,
                Panel::ThreadDetails,
                cx,
            ))
            .child(self.render_panel_toggle(
                "toggle-terminal",
                "panel-bottom",
                &header.terminal,
                Some(ui::shortcut("J")),
                Panel::Terminal,
                cx,
            ))
            .child(self.render_panel_toggle(
                "toggle-right-panel",
                "panel-right",
                &header.right_panel,
                Some(ui::shortcut("⌥B")),
                Panel::Right,
                cx,
            ));
        let menu_layer = self.render_menu_layer();
        let bar = window_drag(
            h_flex()
                .h(px(ui::metrics().header_height))
                .flex_shrink_0()
                .w_full()
                .gap_3()
                .px_5()
                .when(self.sidebar_hidden && cfg!(target_os = "macos"), |bar| {
                    bar.pl(px(90.))
                })
                .bg(color("canvas"))
                .border_b_1()
                .border_color(color("toolbarBorder")),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |view, event: &MouseDownEvent, window, cx| {
                if view.menus.rename.is_some() {
                    return;
                }
                if let Some(thread_id) = view.thread_id().filter(|_| server_thread) {
                    view.open_thread_menu(
                        thread_id,
                        MenuSurface::Header,
                        event.position,
                        window,
                        cx,
                    );
                } else if let Some(project_id) = draft_project.clone() {
                    let desktop = cx.entity().downgrade();
                    view.open_menu(event.position, window, cx, move |menu, _, _| {
                        menu.item(
                            PopupMenuItem::new("Project settings")
                                .icon(icon("settings"))
                                .on_click(move |_, window, cx| {
                                    let project_id = project_id.clone();
                                    let _ = desktop.update(cx, |view, cx| {
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
                    });
                }
            }),
        )
        .child(breadcrumb)
        .child(div().flex_1().h_full())
        .child(controls);
        // The open menu sits outside the bar so its clicks never move the window.
        div()
            .flex_shrink_0()
            .child(bar)
            .children(menu_layer)
            .into_any_element()
    }
}

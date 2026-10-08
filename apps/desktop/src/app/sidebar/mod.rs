//! The thread list: drafts, shelves of thread rows, search results and the
//! project scope, drawn from `SidebarView`.
mod drag;
mod row;
mod scope;
mod sweep;
mod undo;
pub(crate) use drag::ThreadDrag;
pub(crate) use sweep::SweepDrag;

use super::{
    Desktop, Route,
    menus::MenuSurface,
    settings::SettingsPage,
    ui::{self, color, icon, tint},
};
use agent_core::view::sidebar::SidebarOptions;
use agent_core::{
    environment::{EnvironmentSidebarSection, EnvironmentSidebarView, EnvironmentThreadRow},
    state::Intent,
    view::{
        sidebar::{
            SidebarEmptyState, SidebarItem, SidebarSelection, TraversalDirection, project_identity,
            resolve_adjacent_thread_id, should_create_new_thread_in_current_project,
        },
        thread_menu::{ThreadMenuItemId, ThreadMenuOptions, ThreadMenuSurface},
    },
};
use drag::DragState;
use gpui_kit::{
    component::{
        h_flex,
        input::{Input, InputEvent, InputState},
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use scope::ScopeMenu;
use std::{sync::Arc, time::Duration};

/// How long the secondary modifier is held before the jump hints show.
const JUMP_HINT_DELAY: Duration = Duration::from_millis(200);

pub(crate) struct SidebarState {
    options: SidebarOptions,
    selection: SidebarSelection,
    search: Entity<InputState>,
    /// The highlighted search result.
    search_index: usize,
    hovered: Option<String>,
    drag: Option<DragState>,
    sweep: Option<sweep::SweepState>,
    /// Redraws once the shown Undo expires, at that time.
    undo_expiry: Option<(i64, Task<()>)>,
    scope: Option<ScopeMenu>,
    jump_hints: bool,
    jump_timer: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl SidebarState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let subscriptions = vec![cx.subscribe_in(
            &search,
            window,
            |view, search, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    view.sidebar.search_index = 0;
                    view.perform(Intent::Search {
                        query: search.read(cx).value().to_string(),
                    });
                }
                InputEvent::PressEnter { .. } => view.open_search_result(window, cx),
                _ => {}
            },
        )];
        Self {
            options: SidebarOptions::default(),
            selection: SidebarSelection::default(),
            search,
            search_index: 0,
            hovered: None,
            drag: None,
            sweep: None,
            undo_expiry: None,
            scope: None,
            jump_hints: false,
            jump_timer: None,
            _subscriptions: subscriptions,
        }
    }
    /// The shelves the user expanded and the settled pages shown.
    pub(crate) fn options(&self) -> SidebarOptions {
        SidebarOptions {
            selection: self.selection.selected.clone(),
            ..self.options.clone()
        }
    }
}

/// A project's generated badge: its monogram on a tint of its color.
pub(crate) fn project_badge(name: &str, size: f32) -> Div {
    let identity = project_identity(name);
    let hex = identity.color.hex(ui::is_dark());
    let tone: Hsla = rgb(u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0)).into();
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size / 4.))
        .bg(tone.opacity(0.14))
        .text_color(tone)
        .text_size(px(size * 0.5))
        .line_height(px(size))
        .font_weight(FontWeight::BOLD)
        .font_family("Menlo")
        .child(identity.monogram)
}

/// A project's icon at `size`: the Host's image, else its generated badge.
pub(crate) fn project_mark(image: Option<Arc<Image>>, name: &str, size: f32) -> AnyElement {
    match image {
        Some(image) => img(image)
            .size(px(size))
            .flex_shrink_0()
            .rounded(px(size / 4.))
            .object_fit(ObjectFit::Contain)
            .into_any_element(),
        None => project_badge(name, size).into_any_element(),
    }
}

impl Desktop {
    /// The project's icon image, decoded once per icon.
    pub(crate) fn project_icon_image(&self, project_id: &str) -> Option<Arc<Image>> {
        let hash = self.snapshot.project_icon_hash(project_id.to_owned())?;
        if let Some(image) = self.project_icons.borrow().get(&hash) {
            return Some(image.clone());
        }
        let icon = self.snapshot.project_icon(project_id.to_owned())?;
        let format = ImageFormat::from_mime_type(&icon.mime_type)?;
        let image = Arc::new(Image::from_bytes(format, icon.data));
        self.project_icons
            .borrow_mut()
            .insert(icon.hash, image.clone());
        Some(image)
    }

    /// The project's icon at `size`.
    pub(crate) fn project_icon(&self, project_id: &str, name: &str, size: f32) -> AnyElement {
        project_mark(self.project_icon_image(project_id), name, size)
    }
}

/// A 28 px sidebar header button with a tooltip.
fn header_button(id: &'static str, glyph: impl IntoElement, label: SharedString) -> Stateful<Div> {
    div()
        .id(id)
        .size_7()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(8.))
        .cursor_pointer()
        .text_color(tint("sidebarMutedForeground", 0.6))
        .hover(|button| {
            button
                .bg(color("sidebarRowHover"))
                .text_color(color("sidebarForeground"))
        })
        .child(glyph)
        .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
}

/// Starts moving the window from an empty stretch of the title bar.
pub(crate) fn window_drag(element: Div) -> Div {
    element
        .window_control_area(WindowControlArea::Drag)
        .on_mouse_down(MouseButton::Left, |event, window, _| {
            if event.click_count == 2 {
                window.titlebar_double_click();
            } else {
                window.start_window_move();
            }
        })
}

impl Desktop {
    pub(crate) fn render_sidebar(&mut self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if !cx.has_active_drag() {
            self.sidebar.drag = None;
            self.sidebar.sweep = None;
        }
        let metrics = ui::metrics();
        v_flex()
            .id("sidebar")
            .relative()
            .w(px(metrics.sidebar_width))
            .h_full()
            .flex_shrink_0()
            .bg(color("sidebar"))
            .text_color(color("sidebarForeground"))
            .border_r_1()
            .border_color(color("sidebarBorder"))
            .child(self.render_brand_row(cx))
            .child(self.render_thread_header(cx))
            .child(self.render_thread_list(cx))
            .children(self.render_undo_notice(cx))
            .child(self.render_sidebar_footer(cx))
            .children(self.render_scope_menu(cx))
            .into_any_element()
    }

    fn render_brand_row(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        window_drag(
            h_flex()
                .h(px(ui::metrics().header_height))
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .pr_3()
                .pl(px(if cfg!(target_os = "macos") { 90. } else { 12. })),
        )
        .child(
            div()
                .id("brand")
                .h_7()
                .flex()
                .items_center()
                .rounded(px(8.))
                .cursor_pointer()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(color("text"))
                .child(h_flex().gap_1().child("Bex"))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|view, _, _, cx| {
                    view.route = Route::Chat;
                    view.perform(Intent::LeaveThread);
                    cx.notify();
                })),
        )
    }

    /// Search, then the project scope, Add project and New thread buttons.
    fn render_thread_header(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let searching = !self.snapshot.search.trim().is_empty();
        let projects = self.views.sidebar.project_scope.len().saturating_sub(1);
        let scoped = self
            .views
            .sidebar
            .project_scope
            .iter()
            .find(|item| item.selected && item.project_id.is_some())
            .map(|item| {
                (
                    item.project_id.clone().unwrap_or_default(),
                    item.label.clone(),
                )
            });
        let search = h_flex()
            .id("sidebar-search")
            .h_8()
            .flex_1()
            .min_w_0()
            .gap_2()
            .px_2()
            .rounded(px(8.))
            .text_sm()
            .font_weight(FontWeight::MEDIUM)
            .text_color(color("sidebarMutedForeground"))
            .hover(|field| field.bg(color("sidebarRowHover")))
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if view.search_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                icon("search")
                    .size_4()
                    .text_color(tint("sidebarMutedForeground", 0.6)),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.sidebar.search)
                        .appearance(false)
                        .aria_label("Search threads"),
                ),
            )
            .when(searching, |field| {
                field.child(
                    div()
                        .id("clear-search")
                        .size_4()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(|button| button.text_color(color("sidebarForeground")))
                        .child(icon("x").size_3())
                        .on_click(cx.listener(|view, _, window, cx| view.clear_search(window, cx))),
                )
            });
        let mut new_thread_tip = format!("New thread ({})", ui::shortcut("N"));
        if projects > 1 {
            new_thread_tip.push_str("\nNew thread in current project: Shift+click");
        }
        h_flex().p_2().gap_1().flex_shrink_0().child(search).child(
            h_flex()
                .flex_shrink_0()
                .when(projects > 0, |group| {
                    group
                        .child(
                            header_button(
                                "project-scope",
                                match &scoped {
                                    Some((id, name)) => self.project_icon(id, name, 16.),
                                    None => icon("folder").size_4().into_any_element(),
                                },
                                match &scoped {
                                    Some((_, name)) => {
                                        format!("Filter threads by project: {name}").into()
                                    }
                                    None => "Filter threads by project".into(),
                                },
                            )
                            .on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.toggle_scope_menu(window, cx)
                                }),
                            ),
                        )
                        .child(
                            header_button(
                                "add-project",
                                icon("folder-plus").size_4(),
                                "Add project".into(),
                            )
                            .on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.open_add_project(window, cx)
                                }),
                            ),
                        )
                })
                .child(
                    header_button(
                        "new-thread",
                        icon("square-pen").size_4(),
                        new_thread_tip.into(),
                    )
                    .when(projects == 0, |button| {
                        button.opacity(0.64).cursor_default()
                    })
                    .on_click(cx.listener(
                        move |view, event: &ClickEvent, window, cx| {
                            if projects == 0 {
                                return;
                            }
                            if should_create_new_thread_in_current_project(
                                event.modifiers().shift,
                                projects,
                            ) {
                                let project = view.current_project();
                                view.new_thread(project, cx);
                            } else {
                                view.open_new_thread_menu(event.position(), window, cx);
                            }
                        },
                    )),
                ),
        )
    }

    fn render_sidebar_footer(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex().flex_shrink_0().px_2().py_1().child(
            div()
                .id("sidebar-settings")
                .size_8()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(8.))
                .cursor_pointer()
                .text_color(tint("sidebarMutedForeground", 0.6))
                .hover(|button| {
                    button
                        .bg(color("sidebarRowHover"))
                        .text_color(color("sidebarForeground"))
                })
                .child(icon("settings").size_4())
                .tooltip(|window, cx| Tooltip::new("Settings").build(window, cx))
                .on_click(cx.listener(|view, _, window, cx| {
                    view.open_settings(SettingsPage::General, window, cx)
                })),
        )
    }

    fn render_thread_list(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let views = self.views.clone();
        if (views.environment_sidebar.sections.len() > 1
            || !views.environment_sidebar.activities.is_empty())
            && self.snapshot.search.trim().is_empty()
        {
            return self.render_environment_thread_list(views.environment_sidebar.clone(), cx);
        }
        let sidebar = &views.sidebar;
        let mut list = v_flex().min_h_full().p_2().gap_px();
        if let Some(search) = &sidebar.search {
            list = list.children(search.results.iter().enumerate().map(|(index, result)| {
                self.render_search_result(index, result, &search.query, cx)
            }));
            if let Some(label) = &search.empty_label {
                list = list.child(
                    div()
                        .px_2()
                        .py_6()
                        .text_center()
                        .text_xs()
                        .text_color(color("sidebarMutedForeground"))
                        .child(label.clone()),
                );
            }
        } else {
            if !sidebar.drafts.is_empty() {
                list = list
                    .children(
                        sidebar
                            .drafts
                            .iter()
                            .map(|draft| self.render_draft_row(draft, cx)),
                    )
                    .child(
                        div()
                            .mx(px(10.))
                            .my(px(6.))
                            .h_px()
                            .flex_shrink_0()
                            .bg(tint("sidebarBorder", 0.6)),
                    );
            }
            let catalog = agent_core::view::models::catalog(&self.snapshot);
            let mut jump = 0;
            for item in &sidebar.items {
                list = list.child(match item {
                    SidebarItem::Thread { row } => {
                        let label = (self.sidebar.jump_hints && jump < 9)
                            .then(|| ui::shortcut(&(jump + 1).to_string()));
                        jump += 1;
                        self.render_thread_row(row, label, &catalog, cx)
                    }
                    SidebarItem::Boundary { marker } => self.render_boundary(*marker, cx),
                    SidebarItem::Shelf { header } => self.render_shelf(header, cx),
                    SidebarItem::ShowMore { count } => self.render_show_more(*count, cx),
                });
            }
            if let Some(empty) = &sidebar.empty_state {
                list = list.child(self.render_empty_state(empty, cx));
            }
        }
        div()
            .id("sidebar-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .on_drag_move::<drag::ThreadDrag>(cx.listener(
                |view, event: &DragMoveEvent<drag::ThreadDrag>, _, cx| {
                    let x = event.event.position.x;
                    let outside = x < event.bounds.left() || x > event.bounds.right();
                    view.drag_across_list_edge(outside, cx);
                },
            ))
            .on_drag_move::<SweepDrag>(cx.listener(
                |view, event: &DragMoveEvent<SweepDrag>, _, cx| {
                    view.sweep_moved(event.event.position.y, event.bounds, cx);
                },
            ))
            .child(list)
    }

    fn render_environment_thread_list(
        &mut self,
        view: EnvironmentSidebarView,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut list = v_flex().min_h_full().p_2().gap_2();
        let inbox_count = self.views.environment_inbox.items.len();
        if inbox_count > 0 {
            list = list.child(
                div()
                    .px_2()
                    .pb_1()
                    .text_size(px(10.))
                    .text_color(color("sidebarMutedForeground"))
                    .child(format!("All environments · {inbox_count} active")),
            );
        }
        for section in view.sections {
            list = list.child(self.render_environment_section(section, cx));
        }
        if !view.activities.is_empty() {
            list = list.child(
                v_flex()
                    .gap_1()
                    .pt_2()
                    .child(
                        div()
                            .px_2()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color("sidebarMutedForeground"))
                            .child("Agent activity"),
                    )
                    .children(view.activities.into_iter().map(|activity| {
                        let thread = agent_core::environment::scoped_key(
                            &activity.environment.environment_id,
                            &activity.activity.thread_id,
                        );
                        let label = activity.environment.label.clone();
                        let title = activity.activity.thread_title.clone();
                        let headline = activity.activity.headline.clone();
                        let id = thread.unwrap_or_default();
                        h_flex()
                            .id(ElementId::Name(format!("activity-{id}").into()))
                            .w_full()
                            .gap_2()
                            .px_2()
                            .py_1p5()
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(|row| row.bg(color("sidebarRowHover")))
                            .child(icon("bot").size_3().text_color(color("updateForeground")))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(div().truncate().text_xs().child(title))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(px(10.))
                                            .text_color(color("sidebarMutedForeground"))
                                            .child(format!("{label} · {headline}")),
                                    ),
                            )
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if !id.is_empty() {
                                    view.open_sidebar_thread(id.clone(), cx);
                                }
                            }))
                    })),
            );
        }
        div()
            .id("environment-sidebar-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(list)
    }

    fn render_environment_section(
        &mut self,
        section: EnvironmentSidebarSection,
        cx: &mut Context<Self>,
    ) -> Div {
        let title = section.summary.descriptor.label.clone();
        let environment_id = section.summary.descriptor.environment_id.clone();
        let status = match section.summary.connection {
            agent_core::environment::EnvironmentConnectionState::Connected => "Connected",
            agent_core::environment::EnvironmentConnectionState::Connecting => "Connecting…",
            agent_core::environment::EnvironmentConnectionState::Disconnected => "Offline",
        };
        let status = section.summary.reconnect_reason.as_deref().map_or_else(
            || status.to_owned(),
            |reason| format!("{status} · {reason}"),
        );
        v_flex()
            .gap_1()
            .child(
                h_flex()
                    .px_2()
                    .gap_2()
                    .child(
                        icon("monitor")
                            .size_3()
                            .text_color(color("sidebarMutedForeground")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(color("sidebarMutedForeground"))
                            .child(status),
                    ),
            )
            .children(
                section
                    .drafts
                    .into_iter()
                    .map(|draft| self.render_environment_draft(&environment_id, draft, cx)),
            )
            .children(
                section
                    .rows
                    .into_iter()
                    .map(|item| self.render_environment_row(item, cx)),
            )
    }

    fn render_environment_draft(
        &mut self,
        environment_id: &str,
        draft: agent_core::view::sidebar::SidebarDraftRow,
        cx: &mut Context<Desktop>,
    ) -> Div {
        let project = draft.project_name.clone().unwrap_or_default();
        let preview = draft.preview.clone();
        let environment_id = environment_id.to_owned();
        let project_id = agent_core::environment::parse_scoped_project_key(&draft.project_id)
            .map(|reference| reference.project_id);
        h_flex()
            .id(ElementId::Name(
                format!("environment-draft-{}", draft.draft_key).into(),
            ))
            .w_full()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|row| row.bg(color("sidebarRowHover")))
            .child(
                icon("square-pen")
                    .size_3()
                    .text_color(color("warningForeground")),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .child(
                        div()
                            .truncate()
                            .text_xs()
                            .child(format!("{environment_id} · {project}")),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(color("sidebarMutedForeground"))
                            .child(preview),
                    ),
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                view.pending_new_thread = Some((environment_id.clone(), project_id.clone()));
                view.apply_pending_open(window, cx);
                cx.notify();
            }))
    }

    fn render_environment_row(
        &mut self,
        item: EnvironmentThreadRow,
        cx: &mut Context<Self>,
    ) -> Div {
        let id = item.row.id.clone();
        let project = item.row.project_name.clone().unwrap_or_default();
        let title = item.row.title.clone();
        let environment = item.environment_label;
        h_flex()
            .id(ElementId::Name(format!("environment-thread-{id}").into()))
            .w_full()
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|row| row.bg(color("sidebarRowHover")))
            .child(
                icon("message-square")
                    .size_3()
                    .text_color(color("sidebarMutedForeground")),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .child(div().truncate().text_sm().child(title))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(10.))
                            .text_color(color("sidebarMutedForeground"))
                            .child(format!("{environment} · {project}")),
                    ),
            )
            .on_click(cx.listener(move |view, _, _, cx| {
                view.open_sidebar_thread(id.clone(), cx);
            }))
    }

    fn render_empty_state(&self, empty: &SidebarEmptyState, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_6()
            .text_center()
            .text_xs()
            .text_color(tint("textMuted", 0.6))
            .child(empty.label())
            .when(matches!(empty, SidebarEmptyState::NoProjects), |state| {
                state.child(
                    ui::text_2xs(
                        h_flex()
                            .id("empty-add-project")
                            .gap_1p5()
                            .px_2p5()
                            .py_1()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(color("sidebarBorder"))
                            .cursor_pointer()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color("sidebarMutedForeground"))
                            .hover(|button| {
                                button
                                    .bg(color("sidebarRowHover"))
                                    .text_color(color("sidebarForeground"))
                            }),
                    )
                    .child(icon("plus").size_3())
                    .child("Add project")
                    .on_click(cx.listener(|view, _, window, cx| view.open_add_project(window, cx))),
                )
            })
            .into_any_element()
    }

    /// The open thread's project, else the scoped one, else the first.
    fn current_project(&self) -> Option<String> {
        self.snapshot
            .selected_thread
            .as_ref()
            .and_then(|thread| self.snapshot.thread_project(thread))
            .map(str::to_owned)
            .or_else(|| self.snapshot.selected_project.clone())
            .or_else(|| {
                self.views
                    .sidebar
                    .project_scope
                    .iter()
                    .find_map(|item| item.project_id.clone())
            })
    }

    /// Up, Down, Enter and Escape while the search field has focus.
    fn search_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(search) = &self.views.sidebar.search else {
            return false;
        };
        let count = search.results.len();
        match event.keystroke.key.as_str() {
            "escape" => self.clear_search(window, cx),
            "down" if count > 0 => {
                self.sidebar.search_index = (self.sidebar.search_index + 1) % count
            }
            "up" if count > 0 => {
                self.sidebar.search_index = (self.sidebar.search_index + count - 1) % count
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.search_index = 0;
        self.sidebar
            .search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.perform(Intent::Search {
            query: String::new(),
        });
        cx.notify();
    }

    fn open_search_result(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(result) = self
            .views
            .sidebar
            .search
            .as_ref()
            .and_then(|search| search.results.get(self.sidebar.search_index))
            .map(|result| result.id.clone())
        else {
            return;
        };
        self.clear_search(window, cx);
        self.open_sidebar_thread(result, cx);
    }

    /// Opens a thread from the list: the selection clears and the row anchors
    /// the next Shift+click range.
    fn open_sidebar_thread(&mut self, thread_id: String, cx: &mut Context<Self>) {
        let local_thread_id =
            if let Some(reference) = agent_core::environment::parse_scoped_thread_key(&thread_id) {
                if self.environment_registry.selected() != Some(reference.environment_id.as_str()) {
                    self.pending_open = Some((
                        reference.environment_id.clone(),
                        reference.thread_id.clone(),
                    ));
                    self.promote_environment(&reference.environment_id);
                    cx.notify();
                    return;
                }
                reference.thread_id
            } else {
                thread_id
            };
        self.sidebar.selection.open(&local_thread_id);
        self.open_thread(local_thread_id, cx);
        self.refresh_views(cx);
    }

    pub(crate) fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.selection.selected.is_empty() {
            return;
        }
        self.sidebar.selection.selected.clear();
        self.refresh_views(cx);
    }

    /// The project scope changed: the settled tail starts from its first page.
    pub(crate) fn sidebar_scope_changed(&mut self, cx: &mut Context<Self>) {
        self.sidebar.options.settled_pages = 0;
        self.refresh_views(cx);
    }

    /// Opens the previous or next thread in sidebar order.
    pub(crate) fn select_adjacent_thread(&mut self, next: bool, cx: &mut Context<Self>) {
        let ids = self.views.sidebar.thread_ids();
        let current = self.thread_id();
        let direction = if next {
            TraversalDirection::Next
        } else {
            TraversalDirection::Previous
        };
        if let Some(id) = resolve_adjacent_thread_id(&ids, current.as_deref(), direction) {
            let id = id.to_owned();
            self.open_sidebar_thread(id, cx);
        }
    }

    /// Opens the `index`th rendered row (⌘1…⌘9).
    pub(crate) fn jump_to_thread(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(id) = self.views.sidebar.thread_ids().into_iter().nth(index) {
            self.open_sidebar_thread(id, cx);
        }
    }

    /// Runs the open thread's menu item `ids.0`, or `ids.1` when that is the
    /// one offered: settle or un-settle (⌘⇧S), pin or unpin (⌘⇧P).
    pub(crate) fn toggle_open_thread(
        &mut self,
        ids: (ThreadMenuItemId, ThreadMenuItemId),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread_id) = self.thread_id() else {
            return;
        };
        let Some(item) = self
            .snapshot
            .thread_menu(
                thread_id.clone(),
                ui::now_ms(),
                ThreadMenuOptions {
                    surface: ThreadMenuSurface::Header,
                    ..ThreadMenuOptions::default()
                },
            )
            .and_then(|menu| {
                menu.items
                    .into_iter()
                    .find(|item| item.id == ids.0 || item.id == ids.1)
            })
        else {
            return;
        };
        if let Some(action) = item.action {
            self.run_thread_action(
                thread_id,
                MenuSurface::Header,
                action,
                item.confirmation,
                window,
                cx,
            );
        }
    }

    /// Shows the ⌘1…⌘9 hints while only the secondary modifier is held.
    pub(crate) fn sidebar_modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.shift {
            self.snapshot_shift_presses = 0;
        }
        let held = event.modifiers.secondary()
            && !event.modifiers.shift
            && !event.modifiers.alt
            && !(cfg!(target_os = "macos") && event.modifiers.control);
        if !held {
            self.sidebar.jump_timer = None;
            if self.sidebar.jump_hints {
                self.sidebar.jump_hints = false;
                cx.notify();
            }
            return;
        }
        if self.sidebar.jump_hints || self.sidebar.jump_timer.is_some() {
            return;
        }
        self.sidebar.jump_timer = Some(cx.spawn_in(window, async move |view, cx| {
            cx.background_executor().timer(JUMP_HINT_DELAY).await;
            let _ = view.update(cx, |view, cx| {
                view.sidebar.jump_timer = None;
                view.sidebar.jump_hints = true;
                cx.notify();
            });
        }));
    }
}

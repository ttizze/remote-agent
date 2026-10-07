//! The project scope picker under the search field: a query, "All
//! projects" and the projects in sidebar order.
use super::{
    super::{
        Desktop,
        settings::SettingsPage,
        ui::{color, icon},
    },
    project_badge,
};
use agent_core::view::sidebar::{
    SidebarProjectScopeItem, filter_sidebar_project_scope_items, project_scope_label_matches,
};
use gpui_kit::{
    component::{
        h_flex,
        input::{Input, InputEvent, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};

pub(super) struct ScopeMenu {
    query: Entity<InputState>,
    highlighted: usize,
    _events: Subscription,
}

impl Desktop {
    pub(super) fn toggle_scope_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sidebar.scope.take().is_some() {
            cx.notify();
            return;
        }
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search projects..."));
        query.update(cx, |query, cx| query.focus(window, cx));
        let events =
            cx.subscribe_in(
                &query,
                window,
                |view, _, event: &InputEvent, _, cx| match event {
                    InputEvent::Change => {
                        if let Some(scope) = view.sidebar.scope.as_mut() {
                            scope.highlighted = 0;
                        }
                        cx.notify();
                    }
                    InputEvent::PressEnter { .. } => {
                        let items = view.scope_items(cx);
                        if let Some(scope) = &view.sidebar.scope
                            && let Some(item) = items.get(scope.highlighted)
                        {
                            let project_id = item.project_id.clone();
                            view.choose_scope(project_id, cx);
                        }
                    }
                    _ => {}
                },
            );
        self.sidebar.scope = Some(ScopeMenu {
            query,
            highlighted: 0,
            _events: events,
        });
        cx.notify();
    }

    fn scope_items(&self, cx: &App) -> Vec<SidebarProjectScopeItem> {
        let query = self
            .sidebar
            .scope
            .as_ref()
            .map(|scope| scope.query.read(cx).value().to_string())
            .unwrap_or_default();
        filter_sidebar_project_scope_items(
            &self.views.sidebar.project_scope,
            &query,
            project_scope_label_matches,
        )
    }

    fn choose_scope(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        self.sidebar.scope = None;
        self.filter_project(project_id, cx);
        cx.notify();
    }

    fn scope_key(&mut self, event: &KeyDownEvent, count: usize, cx: &mut Context<Self>) -> bool {
        let Some(scope) = self.sidebar.scope.as_mut() else {
            return false;
        };
        match event.keystroke.key.as_str() {
            "escape" => self.sidebar.scope = None,
            "down" if count > 0 => scope.highlighted = (scope.highlighted + 1) % count,
            "up" if count > 0 => scope.highlighted = (scope.highlighted + count - 1) % count,
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn render_scope_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let scope = self.sidebar.scope.as_ref()?;
        let items = self.scope_items(cx);
        let highlighted = scope.highlighted;
        let count = items.len();
        let rows = items.into_iter().enumerate().map(|(index, item)| {
            let project_id = item.project_id.clone();
            let settings_id = item.project_id.clone();
            h_flex()
                .id(("scope-item", index))
                .h_8()
                .px_2()
                .gap_2()
                .rounded(px(6.))
                .cursor_pointer()
                .text_sm()
                .when(index == highlighted, |row| row.bg(color("accentSurface")))
                .on_hover(cx.listener(move |view, hovered: &bool, _, cx| {
                    if *hovered && let Some(scope) = view.sidebar.scope.as_mut() {
                        scope.highlighted = index;
                        cx.notify();
                    }
                }))
                .on_click(
                    cx.listener(move |view, _, _, cx| view.choose_scope(project_id.clone(), cx)),
                )
                .child(match &item.project_id {
                    Some(_) => project_badge(&item.label, 16.).into_any_element(),
                    None => icon("folder").size_4().into_any_element(),
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(item.label.clone()),
                )
                .when_some(settings_id, |row, project_id| {
                    row.child(
                        div()
                            .id(("scope-settings", index))
                            .size_6()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(6.))
                            .text_color(color("textMuted"))
                            .hover(|button| button.text_color(color("text")))
                            .child(icon("settings").size_3p5())
                            .on_click(cx.listener(move |view, _, window, cx| {
                                cx.stop_propagation();
                                view.sidebar.scope = None;
                                view.open_settings(
                                    SettingsPage::Projects {
                                        project_id: Some(project_id.clone()),
                                    },
                                    window,
                                    cx,
                                );
                            })),
                    )
                })
        });
        let popup = v_flex()
            .id("project-scope-menu")
            .min_w(px(152.))
            .max_w(px(288.))
            .bg(color("surfaceOverlay"))
            .border_1()
            .border_color(color("border"))
            .rounded(px(10.))
            .shadow_lg()
            .overflow_hidden()
            .text_color(color("text"))
            .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                view.sidebar.scope = None;
                cx.notify();
            }))
            .capture_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
                if view.scope_key(event, count, cx) {
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .px_1()
                    .border_b_1()
                    .border_color(color("border"))
                    .child(
                        Input::new(&scope.query)
                            .appearance(false)
                            .aria_label("Search projects"),
                    ),
            )
            .child(
                v_flex()
                    .id("project-scope-list")
                    .p_1()
                    .max_h(px(320.))
                    .overflow_y_scroll()
                    .children(rows)
                    .when(count == 0, |list| {
                        list.child(
                            div()
                                .px_2()
                                .py_2()
                                .text_sm()
                                .text_color(color("textMuted"))
                                .child("No matching projects."),
                        )
                    }),
            );
        Some(
            deferred(anchored().position(point(px(8.), px(96.))).child(popup))
                .with_priority(1)
                .into_any_element(),
        )
    }
}

use super::*;

fn source_scope(host: &str, current: &str) -> String {
    if host.is_empty() {
        current.into()
    } else {
        host.into()
    }
}

#[derive(Clone)]
enum SidebarDragKind {
    Project(String),
    Chat(agent_protocol::session::SessionRef),
}

#[derive(Clone)]
pub(super) struct SidebarDrag {
    scope: String,
    label: String,
    kind: SidebarDragKind,
}

impl Render for SidebarDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_2()
            .max_w(px(240.))
            .rounded_md()
            .bg(rgb(appearance::SIDEBAR))
            .border_1()
            .border_color(rgb(0x60a5fa))
            .text_sm()
            .text_ellipsis()
            .child(self.label.clone())
    }
}

#[derive(Clone)]
pub(super) struct SidebarEntry {
    row: SidebarRow,
    collapsed: bool,
}
impl From<SidebarRow> for SidebarEntry {
    fn from(row: SidebarRow) -> Self {
        Self {
            row,
            collapsed: false,
        }
    }
}

#[derive(Clone)]
pub(super) enum SidebarRow {
    Section {
        label: &'static str,
        add_project: Option<(WeakEntity<Desktop>, bool)>,
    },
    Item(Box<SidebarMenuItem>),
    Chat {
        item: Box<SidebarMenuItem>,
        drag: SidebarDrag,
    },
    Heading(String),
    Error(agent_core::presentation::error::ErrorText),
    Project {
        host: String,
        id: String,
        name: String,
        icon: Option<agent_core::presentation::list::ProjectIconImages>,
        monogram: String,
        color: u32,
        root: String,
        expanded: bool,
        connected: bool,
        scope: String,
        desktop: WeakEntity<Desktop>,
    },
}

impl From<SidebarMenuItem> for SidebarRow {
    fn from(item: SidebarMenuItem) -> Self {
        Self::Item(Box::new(item))
    }
}

impl SidebarRow {
    fn render(self, id: String, collapsed: bool, window: &mut Window, cx: &mut App) -> AnyElement {
        let language = crate::localization::language(cx);
        match self {
            Self::Section { label, add_project } => h_flex()
                .h_8()
                .mt_2()
                .flex_shrink_0()
                .pl_2()
                .text_xs()
                .text_color(cx.theme().sidebar_foreground)
                .child(div().flex_1().child(language.text(label)))
                .when_some(add_project, |header, (desktop, enabled)| {
                    header.child(
                        Button::new("choose-project")
                            .icon(Icon::default().path("icons/folder-plus.svg"))
                            .small()
                            .ghost()
                            .tooltip(language.text("プロジェクトを追加"))
                            .accessibility_label(language.text("プロジェクトを追加"))
                            .disabled(!enabled)
                            .on_click(move |_, _, cx| {
                                let _ = desktop.update(cx, |desktop, cx| {
                                    desktop.pick_folder();
                                    cx.notify();
                                });
                            }),
                    )
                })
                .when(collapsed, |header| header.hidden())
                .into_any_element(),
            Self::Error(error) => error_view(&error, language)
                .px_3()
                .text_xs()
                .when(collapsed, |row| row.hidden())
                .into_any_element(),
            Self::Heading(label) => div()
                .px_3()
                .pt_3()
                .pb_1()
                .text_xs()
                .text_color(rgb(appearance::MUTED))
                .when(!collapsed, |row| row.child(label))
                .into_any_element(),
            Self::Item(item) => item
                .collapsed(collapsed)
                .render(id, window, cx)
                .into_any_element(),
            Self::Chat { item, drag } => {
                let selector = format!(
                    "drag-chat-{}",
                    match &drag.kind {
                        SidebarDragKind::Chat(id) => id.id.as_str(),
                        _ => "",
                    }
                );
                div()
                    .debug_selector(move || selector.clone())
                    .id(format!("drag-{id}"))
                    .w_full()
                    .cursor_grab()
                    .on_drag(drag, |drag, _, _, cx| {
                        cx.stop_propagation();
                        cx.new(|_| drag.clone())
                    })
                    .child(item.collapsed(collapsed).render(id, window, cx))
                    .into_any_element()
            }
            Self::Project {
                host,
                id: project_id,
                name,
                icon,
                monogram,
                color,
                root,
                expanded,
                connected,
                scope,
                desktop,
            } => {
                let icon = if let Some(icon) = icon {
                    let png = if cx.theme().is_dark() {
                        icon.dark_png
                    } else {
                        icon.light_png
                    };
                    img(Arc::new(Image::from_bytes(ImageFormat::Png, png)))
                        .size_4()
                        .flex_shrink_0()
                        .object_fit(ObjectFit::Contain)
                        .rounded(px(4.))
                        .into_any_element()
                } else {
                    div()
                        .debug_selector(|| "project-monogram".into())
                        .size_4()
                        .flex_shrink_0()
                        .rounded(px(4.))
                        .bg(rgb(color).opacity(0.15))
                        .text_color(rgb(color))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(8.))
                        .font_bold()
                        .child(monogram)
                        .into_any_element()
                };
                let target = desktop.clone();
                let menu_id = project_id.clone();
                let menu_owner = desktop.clone();
                let selector = if host.is_empty() {
                    format!("project-{project_id}")
                } else {
                    format!("project-{host}-{project_id}")
                };
                let toggle_host = host.clone();
                let drop_host = host.clone();
                let new_host = host.clone();
                let drag = SidebarDrag {
                    scope: scope.clone(),
                    label: name.clone(),
                    kind: SidebarDragKind::Project(project_id.clone()),
                };
                let drop_owner = desktop.clone();
                let drop_id = project_id.clone();
                let drop_target = project_id.clone();
                let bounds = window
                    .use_keyed_state(format!("project-drop-{scope}-{project_id}"), cx, |_, _| {
                        std::rc::Rc::new(std::cell::Cell::new(None::<Bounds<Pixels>>))
                    })
                    .read(cx)
                    .clone();
                let move_bounds = bounds.clone();
                let style_bounds = bounds.clone();
                let group = SharedString::from(id.clone());
                h_flex()
                    .id(id)
                    .group(group.clone())
                    .w_full()
                    .gap_1()
                    .on_drag_move::<SidebarDrag>(move |event, _, _| {
                        move_bounds.set(Some(event.bounds));
                    })
                    .can_drop(move |value, _, _| value.downcast_ref::<SidebarDrag>().is_some_and(|drag| {
                        drag.scope == scope && !matches!(&drag.kind, SidebarDragKind::Project(id) if id == &drop_id)
                    }))
                    .drag_over::<SidebarDrag>(move |style, drag, window, _| {
                        if matches!(drag.kind, SidebarDragKind::Chat(_)) {
                            style.bg(rgb(0x60a5fa).opacity(0.2))
                        } else if style_bounds.get().is_some_and(|bounds| window.mouse_position().y > bounds.center().y) {
                            style.border_b_2().border_color(rgb(0x60a5fa))
                        } else {
                            style.border_t_2().border_color(rgb(0x60a5fa))
                        }
                    })
                    .on_drop(move |drag: &SidebarDrag, window, cx| {
                        cx.stop_propagation();
                        let after = bounds.get().is_some_and(|bounds| window.mouse_position().y > bounds.center().y);
                        let _ = drop_owner.update(cx, |desktop, cx| {
                            if drop_host.is_empty() && desktop.snapshot.connection.storage_scope != drag.scope { return; }
                            let intent = match &drag.kind {
                                SidebarDragKind::Project(id) => Intent::ReorderProject { project_id: id.clone(), target_id: drop_target.clone(), after },
                                SidebarDragKind::Chat(id) => Intent::MoveChatToProject { thread_id: id.clone(), project_id: drop_target.clone() },
                            };
                            desktop.dispatch_host(&drop_host, intent, cx);
                            cx.notify();
                        });
                    })
                    .child(
                        div().id("project-drag-source").flex_1().min_w_0().cursor_grab()
                            .on_drag(drag, |drag, _, _, cx| {
                                cx.stop_propagation();
                                cx.new(|_| drag.clone())
                            }).child(Button::new("heading")
                            .debug_selector(move || selector.clone())
                            .ghost()
                            .w_full()
                            .min_w_0()
                            .h(px(44.))
                            .px_2()
                            .gap_2()
                            .accessibility_label(name.clone())
                            .when(collapsed, |button| button.tooltip(name.clone()))
                            .child(icon)
                            .when(!collapsed, |button| {
                                button.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_left()
                                        .text_ellipsis()
                                        .child(name),
                                )
                            })
                            .on_click(move |_, _, cx| {
                                let _ = target.update(cx, |desktop, cx| {
                                    desktop.dispatch_host(&toggle_host, Intent::SetProjectExpanded {
                                        project_id: project_id.clone(),
                                        expanded: !expanded,
                                    }, cx);
                                    cx.notify();
                                });
                            })),
                    )
                    .when(!collapsed, |row| {
                        row.child(
                            h_flex()
                                .gap_1()
                                .invisible()
                                .group_hover(group, |style| style.visible())
                                .child(super::project_management::project_menu_button(
                                    host, menu_id, menu_owner,
                                ))
                                .child(
                                    Button::new("new-project-chat")
                                        .icon(new_chat_icon())
                                        .xsmall()
                                        .ghost()
                                        .tooltip(language.text("このプロジェクトで新しいチャット"))
                                        .accessibility_label(language.text("このプロジェクトで新しいチャット"))
                                        .disabled(!connected)
                                        .on_click(move |_, window, cx| {
                                            cx.stop_propagation();
                                            let _ = desktop.update(cx, |desktop, cx| {
                                                desktop.navigate_host(&new_host, Intent::NewChat { cwd: root.clone() }, window, cx);
                                                cx.notify();
                                            });
                                        }),
                                ),
                        )
                    })
                    .into_any_element()
            }
        }
    }
}

impl Collapsible for SidebarEntry {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }
    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}
impl SidebarItem for SidebarEntry {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        self.row
            .render(format!("{}", id.into()), self.collapsed, window, cx)
    }
}

/// Everything the sidebar shows. [`SidebarView`] renders only from this value,
/// so it re-renders exactly when the value changes.
pub(in crate::app) struct Conversations {
    list: Option<Arc<agent_core::presentation::list::ThreadList>>,
    selected: Option<SessionRef>,
    host: String,
    all: bool,
    connections: Vec<(String, String)>,
    connected_hosts: std::collections::BTreeSet<String>,
    storage_scope: String,
    connected: bool,
    /// Folders can only be picked on this computer.
    local: bool,
    loading_session_list: bool,
    errors: Vec<super::errors::ErrorRow>,
}

impl Conversations {
    fn is_selected(&self, id: &SessionRef, host: &str) -> bool {
        self.selected.as_ref() == Some(id) && (host.is_empty() || host == self.host)
    }
}

impl PartialEq for Conversations {
    fn eq(&self, other: &Self) -> bool {
        self.list.as_ref().map(Arc::as_ptr) == other.list.as_ref().map(Arc::as_ptr)
            && self.host == other.host
            && self.all == other.all
            && self.connections == other.connections
            && self.connected_hosts == other.connected_hosts
            && self.selected == other.selected
            && self.storage_scope == other.storage_scope
            && self.connected == other.connected
            && self.local == other.local
            && self.loading_session_list == other.loading_session_list
            && self.errors == other.errors
    }
}

impl Desktop {
    pub(in crate::app) fn sidebar_model(&self) -> Conversations {
        Conversations {
            list: self
                .connection_list
                .clone()
                .or_else(|| self.publication.list.clone()),
            selected: self.selected().cloned(),
            host: self.connection_id(),
            all: self.connection_selection.all,
            connections: self.connection_names.clone(),
            connected_hosts: self.connected_hosts.clone(),
            storage_scope: self.snapshot.connection.storage_scope.clone(),
            connected: self.snapshot.connection.connected,
            local: self.remote.is_none(),
            loading_session_list: self
                .snapshot
                .operation_running(op::OperationKey::SessionList),
            errors: self.error_rows(&ErrorScope::Projects),
        }
    }
}

/// The sidebar is its own cached view, so typing, scrolling and conversation
/// updates elsewhere in the window do not rebuild it.
pub(in crate::app) struct SidebarView {
    desktop: WeakEntity<Desktop>,
    model: Conversations,
    search: Entity<InputState>,
    app_updates: Option<Entity<crate::updates::Updates>>,
    pane: Option<Arc<agent_core::store::Pane>>,
    #[cfg(test)]
    pub(in crate::app) projections: usize,
    #[cfg(test)]
    pub(in crate::app) renders: usize,
}

impl SidebarView {
    pub(in crate::app) fn new(
        desktop: &Entity<Desktop>,
        model: Conversations,
        search: Entity<InputState>,
        app_updates: Option<Entity<crate::updates::Updates>>,
        pane: Option<Arc<agent_core::store::Pane>>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_global::<crate::localization::Language>(|_, cx| cx.notify())
            .detach();
        Self {
            desktop: desktop.downgrade(),
            model,
            search,
            app_updates,
            #[cfg(test)]
            projections: 0,
            #[cfg(test)]
            renders: 0,
            pane,
        }
    }
}

pub(in crate::app) enum SidebarAction {
    NewChat(String, String),
    OpenChat(String, SessionRef),
    Connection(Option<String>),
    List(Option<String>, Intent),
    Settings(SettingsPage),
    Refresh,
    DismissLocal(op::ErrorId),
}
impl EventEmitter<SidebarAction> for SidebarView {}
impl SidebarView {
    pub(in crate::app) fn sync(
        &mut self,
        model: Conversations,
        pane: Option<Arc<agent_core::store::Pane>>,
        cx: &mut Context<Self>,
    ) {
        #[cfg(test)]
        {
            self.projections += 1;
        }
        self.pane = pane;
        if self.model != model {
            self.model = model;
            cx.notify();
        }
    }
    fn dispatch(&self, intent: Intent) {
        if let Some(pane) = &self.pane {
            drop(pane.dispatch(intent));
        }
    }
    fn new_chat(&self, cwd: String, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(SidebarAction::NewChat(String::new(), cwd));
    }
    fn open_chat(&self, host: String, id: SessionRef, _: &mut Window, cx: &mut Context<Self>) {
        if host.is_empty() || host == self.model.host {
            self.dispatch(Intent::ReadThread(op::ReadThread::open(id)));
        } else {
            cx.emit(SidebarAction::OpenChat(host, id));
        }
    }
    fn dispatch_list(&self, host: Option<String>, intent: Intent, cx: &mut Context<Self>) {
        cx.emit(SidebarAction::List(host, intent));
    }
    pub(in crate::app::view) fn open_settings(&self, page: SettingsPage, cx: &mut Context<Self>) {
        cx.emit(SidebarAction::Settings(page));
    }
    fn refresh_threads(&self, cx: &mut Context<Self>) {
        cx.emit(SidebarAction::Refresh);
    }
}
impl Render for SidebarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        let sidebar = Self::conversation_sidebar(
            &self.model,
            &self.search,
            self.app_updates.clone(),
            &self.desktop,
            cx,
        );
        v_flex()
            .debug_selector(|| "desktop-sidebar-shell".into())
            .size_full()
            .bg(rgb(appearance::SIDEBAR))
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(sidebar_header(true, &self.desktop, cx))
            .child(
                sidebar
                    .w_full()
                    .h_auto()
                    .flex_1()
                    .min_h_0()
                    .border_r_0()
                    .bg(rgb(appearance::SIDEBAR)),
            )
    }
}

impl SidebarView {
    fn project_sidebar_rows(
        model: &Conversations,
        desktop: &WeakEntity<Desktop>,
        cx: &Context<Self>,
    ) -> Vec<SidebarRow> {
        let language = crate::localization::language(cx);
        let list = &model.list;
        let loading_session_list = model.loading_session_list;
        let mut projects = Vec::new();
        let mut last_section = Some(language.text("プロジェクト").to_string());
        let mut pinned_heading = false;
        for project in list
            .as_ref()
            .map(|page| page.projects.as_slice())
            .unwrap_or_default()
        {
            if project.pinned {
                last_section = None;
                if !pinned_heading {
                    projects.push(SidebarRow::Heading(language.text("ピン留め").into()));
                    pinned_heading = true;
                }
            } else {
                let section = project
                    .section
                    .clone()
                    .unwrap_or_else(|| language.text("プロジェクト").into());
                if last_section.as_ref() != Some(&section) {
                    projects.push(SidebarRow::Heading(section.clone()));
                    last_section = Some(section);
                }
            }
            let id = project.id.clone();
            let expanded = project.expanded;
            let host = project.host_id.clone();
            let new_root = project
                .roots
                .first()
                .map(|root| root.path.clone())
                .unwrap_or_default();
            projects.push(SidebarRow::Project {
                host: project.host_id.clone(),
                id: id.clone(),
                name: project.name.clone(),
                icon: project.icon.clone(),
                monogram: project.monogram.clone(),
                color: project.icon_color,
                root: new_root,
                expanded,
                connected: if project.host_id.is_empty() {
                    model.connected
                } else {
                    model.connected_hosts.contains(&project.host_id)
                },
                scope: source_scope(&project.host_id, &model.storage_scope),
                desktop: desktop.clone(),
            });
            if expanded {
                for thread in &project.threads {
                    let selected = model.is_selected(&thread.id, &thread.host_id);
                    projects.push(Self::draggable_chat(
                        &model.storage_scope,
                        thread,
                        Self::thread_button(thread, selected, cx).icon(Icon::empty().size_4()),
                    ));
                }
                if project.loading && !project.has_more {
                    projects.push(SidebarMenuItem::new(language.text("読み込み中…")).into());
                }
                if let Some(error) = &project.error {
                    projects.push(SidebarRow::Error(error.clone()));
                    let retry_id = id.clone();
                    projects.push(
                        SidebarMenuItem::new(language.text("再試行"))
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.dispatch_list(
                                    Some(host.clone()),
                                    Intent::RefreshProject {
                                        project_id: retry_id.clone(),
                                    },
                                    cx,
                                );
                                cx.notify();
                            }))
                            .into(),
                    );
                }
                if project.has_more {
                    let host = project.host_id.clone();
                    let mut more = SidebarMenuItem::new(language.text("もっと表示する"))
                        .icon(Icon::empty().size_4())
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.dispatch_list(
                                Some(host.clone()),
                                Intent::ExpandThreadList {
                                    project_id: Some(id.clone()),
                                },
                                cx,
                            );
                            cx.notify();
                        }));
                    if project.loading {
                        more = more.suffix(|_, _| crate::activity::indicator());
                    }
                    projects.push(more.disable(project.loading).into());
                }
            }
        }
        if list.as_ref().is_some_and(|page| page.has_more_projects) {
            let mut more = SidebarMenuItem::new(language.text("もっとプロジェクトを表示"))
                .on_click(cx.listener(|s, _, _, cx| {
                    s.dispatch_list(None, Intent::ExpandProjects, cx);
                    cx.notify();
                }));
            if list.as_ref().is_some_and(|page| page.loading_projects) {
                more = more.suffix(|_, _| crate::activity::indicator());
            }
            projects.push(
                more.disable(
                    loading_session_list || list.as_ref().is_some_and(|page| page.loading_projects),
                )
                .into(),
            );
        }
        if let Some(list) = &list {
            for project in &list.hidden_projects {
                let id = project.id.clone();
                let host = project.host_id.clone();
                projects.push(
                    SidebarMenuItem::new(agent_core::localization::localized_ui_format(
                        language,
                        "一覧に戻す: {0}".into(),
                        vec![project.name.clone()],
                    ))
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.dispatch_list(
                            Some(host.clone()),
                            Intent::SetProjectHidden {
                                project_id: id.clone(),
                                hidden: false,
                            },
                            cx,
                        );
                        cx.notify();
                    }))
                    .into(),
                );
            }
        }
        projects
    }

    fn conversation_sidebar(
        model: &Conversations,
        search: &Entity<InputState>,
        app_updates: Option<Entity<crate::updates::Updates>>,
        desktop: &WeakEntity<Desktop>,
        cx: &Context<Self>,
    ) -> Sidebar<SidebarEntry> {
        let language = crate::localization::language(cx);
        let list = &model.list;
        let loading_session_list = model.loading_session_list;
        let projects = Self::project_sidebar_rows(model, desktop, cx);
        let mut chats: Vec<SidebarRow> = list
            .as_ref()
            .map(|page| {
                page.threads
                    .iter()
                    .map(|thread| {
                        let selected = model.is_selected(&thread.id, &thread.host_id);
                        Self::draggable_chat(
                            &model.storage_scope,
                            thread,
                            Self::thread_button(thread, selected, cx),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        if list.as_ref().is_some_and(|page| page.has_more) {
            let mut more = SidebarMenuItem::new(language.text("もっと表示する")).on_click(
                cx.listener(|s, _, _, cx| {
                    s.dispatch_list(None, Intent::ExpandThreadList { project_id: None }, cx);
                    cx.notify();
                }),
            );
            if list.as_ref().is_some_and(|page| page.loading_chats) {
                more = more.suffix(|_, _| crate::activity::indicator());
            }
            chats.push(
                more.disable(
                    loading_session_list || list.as_ref().is_some_and(|page| page.loading_chats),
                )
                .into(),
            );
        }
        Sidebar::new("desktop-sidebar")
            .header(
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(Self::connection_menu(model, cx))
                    .child(Self::project_errors(&model.errors, cx))
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(search)
                                        .small()
                                        .prefix(IconName::Search)
                                        .appearance(false)
                                        .aria_label(language.text("会話を検索")),
                                ),
                            )
                            .child(
                                icon_button(
                                    "new-chat",
                                    new_chat_icon(),
                                    language.text("新しいチャット"),
                                    cx,
                                    |s, window, cx| s.new_chat(String::new(), window, cx),
                                )
                                .disabled(!model.connected),
                            ),
                    ),
            )
            .child(SidebarEntry::from(SidebarRow::Section {
                label: language.text("プロジェクト"),
                add_project: Some((desktop.clone(), model.connected && model.local)),
            }))
            .children(projects.into_iter().map(SidebarEntry::from))
            .child(SidebarEntry::from(SidebarRow::Section {
                label: language.text("チャット"),
                add_project: None,
            }))
            .children(chats.into_iter().map(SidebarEntry::from))
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .py_2()
                    .child(
                        icon_button(
                            "sidebar-settings",
                            IconName::Settings,
                            language.text("設定を開く"),
                            cx,
                            |s, _, cx| {
                                s.open_settings(SettingsPage::General, cx);
                            },
                        )
                        .debug_selector(|| "sidebar-settings".into()),
                    )
                    .child(if let Some(updates) = &app_updates {
                        updates.clone().into_any_element()
                    } else {
                        icon_button(
                            "refresh-threads",
                            IconName::RotateCw,
                            language.text("会話を更新"),
                            cx,
                            |s, _, cx| s.refresh_threads(cx),
                        )
                        .debug_selector(|| "refresh-threads".into())
                        .into_any_element()
                    }),
            )
    }
    fn connection_menu(model: &Conversations, cx: &Context<Self>) -> AnyElement {
        let owner = cx.entity().downgrade();
        let selected = model.host.clone();
        let all = model.all;
        let language = crate::localization::language(cx);
        let choices: Vec<_> = model
            .connections
            .iter()
            .map(|(id, name)| {
                let name = if id == "local" && name == "このPC" {
                    language.text(name).to_owned()
                } else {
                    name.clone()
                };
                (id.clone(), name)
            })
            .collect();
        let label = if all {
            language.text("すべての接続先").to_owned()
        } else {
            choices
                .iter()
                .find(|(id, _)| id == &selected)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| language.text("このPC").into())
        };
        Button::new("sidebar-connections")
            .label(label)
            .dropdown_caret(true)
            .ghost()
            .w_full()
            .debug_selector(|| "sidebar-connections".into())
            .dropdown_menu(move |mut menu, _, _| {
                let target = owner.clone();
                menu = menu
                    .item(
                        PopupMenuItem::new(language.text("すべての接続先"))
                            .checked(all)
                            .on_click(move |_, _, cx| {
                                let _ = target
                                    .update(cx, |_, cx| cx.emit(SidebarAction::Connection(None)));
                            }),
                    )
                    .separator();
                for (id, name) in &choices {
                    let target = owner.clone();
                    let id = id.clone();
                    menu = menu.item(
                        PopupMenuItem::new(name.clone())
                            .checked(!all && selected == id)
                            .on_click(move |_, _, cx| {
                                let _ = target.update(cx, |_, cx| {
                                    cx.emit(SidebarAction::Connection(Some(id.clone())))
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn project_errors(rows: &[super::errors::ErrorRow], cx: &Context<Self>) -> Div {
        errors::error_list(
            rows,
            ErrorScope::Projects,
            None,
            cx,
            |view, action, _, cx| match action {
                errors::NoticeAction::Dispatch(intent) => view.dispatch(intent),
                errors::NoticeAction::DismissLocal(_, id) => {
                    cx.emit(SidebarAction::DismissLocal(id))
                }
                errors::NoticeAction::Reconnect => cx.emit(SidebarAction::Refresh),
            },
        )
    }

    fn draggable_chat(
        scope: &str,
        thread: &agent_core::presentation::list::ThreadSummary,
        item: SidebarMenuItem,
    ) -> SidebarRow {
        SidebarRow::Chat {
            item: Box::new(item),
            drag: SidebarDrag {
                scope: source_scope(&thread.host_id, scope),
                label: thread.title.clone(),
                kind: SidebarDragKind::Chat(thread.id.clone()),
            },
        }
    }

    fn thread_button(
        thread: &agent_core::presentation::list::ThreadSummary,
        selected: bool,
        cx: &Context<Self>,
    ) -> SidebarMenuItem {
        let language = crate::localization::language(cx);
        let id = thread.id.clone();
        let host = thread.host_id.clone();
        let active = thread.active;
        let unread = thread.unread;
        let worktree = thread.worktree_status.map(|status| match status {
            agent_protocol::models::WorktreeStatus::Unmerged => (
                "icons/diff.svg",
                0xfb923c,
                language.text("main に未反映の変更あり"),
                format!("thread-unmerged-{}", thread.id),
            ),
            agent_protocol::models::WorktreeStatus::Merged => (
                "icons/merge.svg",
                0xa78bfa,
                language.text("main にマージ済み"),
                format!("thread-merged-{}", thread.id),
            ),
        });
        SidebarMenuItem::new(thread.title.clone())
            .active(selected)
            .suffix(move |_, _| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(active, |row| row.child(crate::activity::indicator()))
                    .when(!active && unread, |row| {
                        row.child(div().size(px(8.)).rounded_full().bg(rgb(0xffffff)))
                    })
                    .when_some(worktree.clone(), |row, (path, color, label, id)| {
                        row.child(
                            div()
                                .id(id)
                                .debug_selector(move || path.into())
                                .role(Role::Image)
                                .child(Icon::default().path(path).size_4())
                                .text_color(rgb(color))
                                .aria_label(label)
                                .tooltip(move |window, cx| {
                                    tooltip::Tooltip::new(label).build(window, cx)
                                }),
                        )
                    })
            })
            .on_click(cx.listener(move |view, _, window, cx| {
                view.open_chat(host.clone(), id.clone(), window, cx);
                cx.notify();
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Arc, Desktop, Draft, Mode, RemoteHost, Runtime, SettingsPage, Snapshot, StoreSession, Tab,
    };
    use agent_core::store::Store;
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, IntoElement, Modifiers, Render, TestAppContext, Window, px,
        size,
    };

    struct TaskNavigation(Entity<Desktop>);
    impl Render for TaskNavigation {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let sidebar = self.0.update(cx, |desktop, cx| {
                let entity = cx.entity();
                if let Some(sidebar) = &desktop.sidebar_view {
                    return sidebar.clone();
                }
                let sidebar = cx.new(|cx| {
                    super::SidebarView::new(
                        &entity,
                        desktop.sidebar_model(),
                        desktop.search.clone(),
                        desktop.app_updates.clone(),
                        desktop.session.as_ref().map(|s| s.pane.clone()),
                        cx,
                    )
                });
                desktop.sidebar_view = Some(sidebar.clone());
                sidebar
            });
            self.0.update(cx, |desktop, cx| {
                let model = desktop.sidebar_model();
                let pane = desktop.session.as_ref().map(|session| session.pane.clone());
                sidebar.update(cx, |view, cx| view.sync(model, pane, cx));
            });
            sidebar
        }
    }

    struct ProjectNavigation(Entity<Desktop>, Entity<super::SidebarView>);
    impl Render for ProjectNavigation {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            self.1.clone()
        }
    }

    #[gpui::test]
    fn sidebar_drag_reorders_projects_and_moves_a_chat(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let app_runtime = Runtime {
            handle: runtime.handle().clone(),
            sessions: Default::default(),
            connections: Arc::default(),
            closing: tokio_util::task::TaskTracker::new(),
            logging_error: None,
        };
        let snapshot = Snapshot {
            connection: agent_core::state::Connection {
                storage_scope: "drag-host".into(),
                ..Default::default()
            },
            lists: agent_core::state::Lists {
                threads: Some(Arc::new(serde_json::from_value(serde_json::json!({
                    "data":[{"id":{"provider":"codex","id":"moving"},"name":"Move this chat","projectId":null}],
                    "projects":[{"id":"a","name":"Alpha","roots":[]},{"id":"b","name":"Beta","roots":[]}],
                    "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
                })).unwrap())),
                ..Default::default()
            },
            ..Default::default()
        };
        let session = runtime.block_on(async {
            let (updates, receive) = async_channel::unbounded();
            tokio::spawn(StoreSession::publish(
                Ok(Arc::new(Store::offline(snapshot))),
                app_runtime.clone(),
                updates,
                Some,
                |_| None,
            ));
            receive.recv().await.unwrap().unwrap().unwrap()
        });
        let store = session.host.store.clone();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(app_runtime);
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::Fixture {
                        remote: Some(RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid-fixture-ticket".into(),
                        }),
                        cwd: String::new(),
                    },
                    window,
                    cx,
                )
            });
            let sidebar = desktop.update(cx, |desktop, cx| {
                desktop.snapshot = store.snapshot();
                desktop.project_fixture();
                desktop.session = Some(session);
                let entity = cx.entity();
                let sidebar = cx.new(|cx| {
                    super::SidebarView::new(
                        &entity,
                        desktop.sidebar_model(),
                        desktop.search.clone(),
                        desktop.app_updates.clone(),
                        desktop.session.as_ref().map(|s| s.pane.clone()),
                        cx,
                    )
                });
                desktop.sidebar_view = Some(sidebar.clone());
                sidebar
            });
            ProjectNavigation(desktop, sidebar)
        });
        window.simulate_resize(size(px(320.), px(700.)));
        window.run_until_parked();
        let from = window.debug_bounds("project-a").unwrap().center();
        let target = window.debug_bounds("project-b").unwrap();
        let to = gpui::point(target.center().x, target.bottom() - px(5.));
        window.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::default());
        window.simulate_mouse_move(
            from + gpui::point(px(12.), px(0.)),
            gpui::MouseButton::Left,
            Modifiers::default(),
        );
        window.run_until_parked();
        window.simulate_mouse_move(to, gpui::MouseButton::Left, Modifiers::default());
        window.run_until_parked();
        window.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::default());
        assert_eq!(store.snapshot().thread_list().unwrap().projects[0].id, "b");
        view.update(window, |view, cx| {
            view.0.update(cx, |desktop, cx| {
                desktop.snapshot = store.snapshot();
                desktop.project_fixture();
                cx.notify();
            })
        });
        window.run_until_parked();
        let from = window.debug_bounds("drag-chat-moving").unwrap().center();
        let to = window.debug_bounds("project-b").unwrap().center();
        window.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::default());
        window.simulate_mouse_move(
            from + gpui::point(px(12.), px(0.)),
            gpui::MouseButton::Left,
            Modifiers::default(),
        );
        window.run_until_parked();
        window.simulate_mouse_move(to, gpui::MouseButton::Left, Modifiers::default());
        window.run_until_parked();
        window.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::default());
        let page = store.snapshot().thread_list().unwrap();
        assert!(page.threads.is_empty());
        assert_eq!(page.projects[0].threads[0].id.id, "moving");
        // Move the project back above Alpha, then move its chat to Alpha.
        for (source, target, chat) in [
            ("project-b", "project-a", false),
            ("drag-chat-moving", "project-a", true),
        ] {
            view.update(window, |view, cx| {
                view.0.update(cx, |desktop, cx| {
                    desktop.snapshot = store.snapshot();
                    desktop.project_fixture();
                    cx.notify();
                })
            });
            window.run_until_parked();
            let from = window.debug_bounds(source).unwrap().center();
            let target = window.debug_bounds(target).unwrap();
            let to = gpui::point(target.center().x, target.top() + px(5.));
            window.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::default());
            window.simulate_mouse_move(
                from + gpui::point(px(12.), px(0.)),
                gpui::MouseButton::Left,
                Modifiers::default(),
            );
            window.run_until_parked();
            window.simulate_mouse_move(to, gpui::MouseButton::Left, Modifiers::default());
            window.run_until_parked();
            window.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::default());
            let page = store.snapshot().thread_list().unwrap();
            assert_eq!(page.projects[0].id, "b");
            if chat {
                assert!(page.projects[0].threads.is_empty());
                assert_eq!(page.projects[1].threads[0].id.id, "moving");
            }
        }
        assert!(
            store.snapshot().pane.navigation.thread_id.is_none(),
            "dragging must not open the chat"
        );
    }

    #[gpui::test]
    fn settings_overlay_keeps_the_sidebar_and_returns_to_the_same_conversation(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(Runtime {
                handle: runtime.handle().clone(),
                sessions: Default::default(),
                connections: Arc::default(),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let mut desktop = Desktop::new(Mode::Main, window, cx);
            desktop.onboarding = false;
            desktop.panels = desktop
                .panels
                .clone()
                .set_open(desktop.panel_scope(), false);
            let session = agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "fixture".into(),
            };
            let snapshot = Arc::make_mut(&mut desktop.snapshot);
            snapshot.lists.threads = Some(Arc::new(
                serde_json::from_value(serde_json::json!({
                    "data":[], "projects":[{"id":"brand", "name":"remote-agent", "roots":[]}],
                     "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5, }))
                .unwrap(),
            ));
            Arc::make_mut(&mut Arc::make_mut(&mut snapshot.pane).navigation).thread_id =
                Some(session.clone());
            Arc::make_mut(&mut Arc::make_mut(&mut snapshot.pane).navigation).draft_key =
                session.clone().into();
            Arc::make_mut(&mut snapshot.input.drafts).insert(
                session.into(),
                Arc::new(Draft {
                    text: "Keep my draft".into(),
                    ..Default::default()
                }),
            );
            let store =
                runtime.block_on(async { Arc::new(Store::offline((*desktop.snapshot).clone())) });
            let app_runtime = desktop.runtime.clone();
            desktop.session = Some(runtime.block_on(async {
                let (updates, receive) = async_channel::unbounded();
                tokio::spawn(StoreSession::publish(
                    Ok(store.clone()),
                    app_runtime,
                    updates,
                    Some,
                    |_| None,
                ));
                receive.recv().await.unwrap().unwrap().unwrap()
            }));
            desktop.snapshot = store.snapshot();
            desktop.project_fixture();
            cx.notify();
            desktop
        });
        window.simulate_resize(size(px(1000.), px(700.)));
        window.run_until_parked();
        let original = window.debug_bounds("desktop-sidebar-shell").unwrap();
        let settings_button = window.debug_bounds("sidebar-settings").unwrap();
        let refresh_button = window.debug_bounds("refresh-threads").unwrap();
        assert_eq!(refresh_button.size, settings_button.size);
        assert_eq!(refresh_button.center().y, settings_button.center().y);
        assert!(refresh_button.left() > settings_button.right());
        assert!(window.debug_bounds("app-update").is_none());
        let project = window.debug_bounds("project-brand").unwrap().center();
        let icon = window.debug_bounds("project-monogram").unwrap();
        assert_eq!(icon.size, size(px(16.), px(16.)));
        for expanded in [true, false] {
            window.simulate_click(project, Modifiers::default());
            view.update(window, |view, cx| {
                view.snapshot = view.session.as_ref().unwrap().host.store.snapshot();
                view.project_fixture();
                cx.notify();
            });
            window.run_until_parked();
            view.update(window, |view, _| {
                assert_eq!(
                    view.snapshot.lists.expanded_projects.contains_key("brand"),
                    expanded
                );
            });
            assert_eq!(window.debug_bounds("project-monogram").unwrap(), icon);
        }
        let settings = window.debug_bounds("sidebar-settings").unwrap().center();
        window.simulate_click(settings, Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("refresh-threads").is_some());
        assert!(window.debug_bounds("app-update").is_none());
        assert_eq!(
            window.debug_bounds("desktop-sidebar-shell").unwrap(),
            original
        );
        for page in [
            SettingsPage::General,
            SettingsPage::Providers,
            SettingsPage::Connections,
            SettingsPage::Worktrees,
        ] {
            view.update(window, |view, cx| {
                view.preferences.update(cx, |view, cx| {
                    view.settings_page = page;
                    cx.notify();
                });
                cx.notify();
            });
            window.run_until_parked();
            let general = page == SettingsPage::General;
            assert_eq!(window.debug_bounds("settings-language").is_some(), general);
            assert_eq!(window.debug_bounds("settings-scope").is_none(), general);
            let scope = window
                .debug_bounds(if general {
                    "settings-general-breadcrumb"
                } else {
                    "settings-scope"
                })
                .unwrap();
            assert!(scope.bottom() <= window.debug_bounds("settings-page-heading").unwrap().top());
            let modal = window.debug_bounds("settings-modal").unwrap();
            assert!(modal.origin.x > px(0.));
            assert!(modal.origin.y > px(0.));
            assert!(window.debug_bounds("settings-close").unwrap().top() < scope.bottom());
            if page == SettingsPage::Worktrees {
                let before = window.debug_bounds("settings-page-heading").unwrap();
                window.simulate_event(gpui::ScrollEvent {
                    position: gpui::point(modal.right() - px(30.), modal.center().y),
                    delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.), px(-500.))),
                    touch_phase: gpui::TouchPhase::Moved,
                    ..Default::default()
                });
                window.run_until_parked();
                let after = window.debug_bounds("settings-page-heading");
                assert!(after.is_none_or(|after| after.top() < before.top()));
                assert!(window.debug_bounds("settings-close").unwrap().top() < scope.bottom());
            }
        }
        let back = window.debug_bounds("settings-close").unwrap().center();
        window.simulate_click(back, Modifiers::default());
        window.run_until_parked();
        assert_eq!(
            window.debug_bounds("refresh-threads").unwrap(),
            refresh_button
        );
        assert_eq!(
            window.debug_bounds("desktop-sidebar-shell").unwrap(),
            original
        );
        view.update(window, |view, _| {
            assert!(view.tab == Tab::Chat);
            assert_eq!(view.selected().unwrap().id, "fixture");
            assert_eq!(view.draft().text, "Keep my draft");
        });
    }

    #[gpui::test]
    fn sidebar_renders_again_only_when_what_it_shows_changes(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(Runtime {
                handle: runtime.handle().clone(),
                sessions: Default::default(),
                connections: Arc::default(),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let session = agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "listed".into(),
        };
        let (view, window) = cx.add_window_view(|window, cx| {
            let mut desktop = Desktop::new(Mode::Main, window, cx);
            desktop.onboarding = false;
            Arc::make_mut(&mut desktop.snapshot).lists.threads = Some(Arc::new(
                serde_json::from_value(serde_json::json!({
                    "data":[{"id":{"provider":"codex","id":"listed"},"name":"Listed task"}],
                    "projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}))
                .unwrap(),
            ));
            desktop.project_fixture();
            cx.notify();
            desktop
        });
        window.run_until_parked();
        let renders = std::rc::Rc::new(std::cell::Cell::new(0));
        let _observation = view.update(window, |view, cx| {
            let renders = renders.clone();
            cx.observe(view.sidebar_view.as_ref().unwrap(), move |_, _, _| {
                renders.set(renders.get() + 1)
            })
        });
        view.update(window, |view, cx| {
            let snapshot = Arc::make_mut(&mut view.snapshot);
            let draft = snapshot.pane.navigation.draft_key.clone();
            Arc::make_mut(&mut snapshot.input.drafts).insert(
                draft,
                Arc::new(Draft {
                    text: "typing".into(),
                    ..Default::default()
                }),
            );
            cx.notify();
        });
        window.run_until_parked();
        assert_eq!(renders.get(), 0, "typing must not rebuild the sidebar");
        view.update(window, |view, cx| {
            let snapshot = Arc::make_mut(&mut view.snapshot);
            Arc::make_mut(&mut Arc::make_mut(&mut snapshot.pane).navigation).thread_id =
                Some(session);
            cx.notify();
        });
        window.run_until_parked();
        assert_eq!(renders.get(), 1, "selecting a task highlights it");
    }

    #[gpui::test]
    fn running_tasks_spin_without_rebuilding_the_window(cx: &mut TestAppContext) {
        struct Counted(Entity<Desktop>, std::rc::Rc<std::cell::Cell<usize>>);
        impl Render for Counted {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.1.set(self.1.get() + 1);
                self.0.clone()
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(Runtime {
                handle: runtime.handle().clone(),
                sessions: Default::default(),
                connections: Arc::default(),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let renders = std::rc::Rc::new(std::cell::Cell::new(0));
        let window = cx.open_window(size(px(1000.), px(700.)), |window, cx| {
            let desktop = cx.new(|cx| {
                let mut desktop = Desktop::new(Mode::Main, window, cx);
                desktop.onboarding = false;
                Arc::make_mut(&mut desktop.snapshot).lists.threads = Some(Arc::new(
                    serde_json::from_value(serde_json::json!({
                        "data":[{"id":{"provider":"codex","id":"running"},"name":"Running task","status":"running"}],
                        "projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}))
                    .unwrap(),
                ));
                desktop.project_fixture();
                cx.notify();
                desktop
            });
            let content = cx.new(|_| Counted(desktop, renders.clone()));
            crate::activity::ActivityWindow::new(content, window, cx)
        });
        cx.run_until_parked();
        let rendered = renders.get();
        for _ in 0..3 {
            let frames = window
                .update(cx, |_, window, cx| window.simulate_next_frame(cx))
                .unwrap();
            cx.run_until_parked();
            assert!(frames > 0, "the running task's indicator keeps spinning");
        }
        assert_eq!(
            renders.get(),
            rendered,
            "spinning must not rebuild the window content"
        );
    }

    #[gpui::test]
    fn pending_operations_do_not_block_task_navigation(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let app_runtime = Runtime {
            handle: runtime.handle().clone(),
            sessions: Default::default(),
            connections: Arc::default(),
            closing: tokio_util::task::TaskTracker::new(),
            logging_error: None,
        };
        let snapshot = Snapshot {
            lists: agent_core::state::Lists {
                threads: Some(Arc::new(
                    serde_json::from_value(serde_json::json!({
                        "data":[{"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"first"},"name":"Child task","status":"running"},
                                {"id":{"provider":"codex","id":"first"},"name":"First task","worktreeStatus":"unmerged"},
                                {"id":{"provider":"codex","id":"second"},"name":"Second task","worktreeStatus":"merged"}],
                        "projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}))
                    .unwrap(),
                )),
                ..Default::default()
            },
            ..Default::default()
        };
        let session = runtime.block_on(async {
            let store = Arc::new(Store::offline(snapshot));
            let (updates, receive) = async_channel::unbounded();
            tokio::spawn(StoreSession::publish(
                Ok(store),
                app_runtime.clone(),
                updates,
                Some,
                |_| None,
            ));
            receive.recv().await.unwrap().unwrap().unwrap()
        });
        let store = session.host.store.clone();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(app_runtime);
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::Fixture {
                        remote: Some(RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid-fixture-ticket".into(),
                        }),
                        cwd: String::new(),
                    },
                    window,
                    cx,
                )
            });
            desktop.update(cx, |desktop, _| {
                desktop.snapshot = store.snapshot();
                desktop.project_fixture();
                desktop.session = Some(session);
                desktop.busy = 1;
            });
            TaskNavigation(desktop)
        });
        window.run_until_parked();
        // Leave the Tokio executor parked so both task reads remain pending.
        assert!(window.debug_bounds("icons/diff.svg").is_some());
        assert!(window.debug_bounds("icons/merge.svg").is_some());
        for (id, selector) in [("first", "drag-chat-first"), ("second", "drag-chat-second")] {
            let bounds = window.debug_bounds(selector).unwrap();
            let button = gpui::point(bounds.left() + gpui::px(70.), bounds.top() + gpui::px(14.));
            window.simulate_click(button, Modifiers::default());
            window.run_until_parked();
            assert_eq!(
                store
                    .snapshot()
                    .pane
                    .navigation
                    .thread_id
                    .as_ref()
                    .unwrap()
                    .id,
                id
            );
            view.update(window, |view, cx| {
                assert_eq!(
                    view.0.read(cx).busy,
                    1,
                    "opening a task must not block later navigation"
                );
            });
        }
        assert!(
            window.debug_bounds("task-2").is_none(),
            "subagents are not left-sidebar conversations"
        );
    }
}

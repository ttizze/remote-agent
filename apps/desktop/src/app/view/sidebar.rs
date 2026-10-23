use super::*;

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
pub(super) struct SidebarSection {
    pub(super) label: &'static str,
    pub(super) menu: Vec<SidebarRow>,
    pub(super) collapsed: bool,
    pub(super) add_project: Option<(WeakEntity<Desktop>, bool)>,
}

#[derive(Clone)]
pub(super) enum SidebarRow {
    Item(Box<SidebarMenuItem>),
    Chat {
        item: Box<SidebarMenuItem>,
        drag: SidebarDrag,
    },
    Heading(String),
    Project {
        id: String,
        name: String,
        icon_png: Option<Vec<u8>>,
        monogram: String,
        color: u32,
        root: String,
        expanded: bool,
        connected: bool,
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
        match self {
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
                id: project_id,
                name,
                icon_png,
                monogram,
                color,
                root,
                expanded,
                connected,
                desktop,
            } => {
                let icon = if let Some(png) = icon_png {
                    img(Arc::new(Image::from_bytes(ImageFormat::Png, png)))
                        .size_4()
                        .flex_shrink_0()
                        .object_fit(ObjectFit::Contain)
                        .bg(rgb(0x777777))
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
                let selector = format!("project-{project_id}");
                let scope = desktop
                    .upgrade()
                    .map(|s| s.read(cx).snapshot.storage_scope.clone())
                    .unwrap_or_default();
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
                h_flex()
                    .id(id)
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
                            if desktop.snapshot.storage_scope != drag.scope { return; }
                            let intent = match &drag.kind {
                                SidebarDragKind::Project(id) => Intent::ReorderProject { project_id: id.clone(), target_id: drop_target.clone(), after },
                                SidebarDragKind::Chat(id) => Intent::MoveChatToProject { thread_id: id.clone(), project_id: drop_target.clone() },
                            };
                            desktop.dispatch(intent);
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
                            .w_full()
                            .min_w_0()
                            .h(px(44.))
                            .px_2()
                            .gap_2()
                            .accessibility_label(name.clone())
                            .when(collapsed, |button| button.tooltip(name.clone()))
                            .child(icon)
                            .when(!collapsed, |button| {
                                button
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_sm()
                                            .text_left()
                                            .text_ellipsis()
                                            .child(name),
                                    )
                                    .child(
                                        Icon::new(if expanded {
                                            IconName::ChevronDown
                                        } else {
                                            IconName::ChevronRight
                                        })
                                        .size(px(12.)),
                                    )
                            })
                            .on_click(move |_, _, cx| {
                                let _ = target.update(cx, |desktop, cx| {
                                    desktop.dispatch(Intent::SetProjectExpanded {
                                        project_id: project_id.clone(),
                                        expanded: !expanded,
                                    });
                                    cx.notify();
                                });
                            })),
                    )
                    .when(!collapsed, |row| {
                        row.child(super::project_management::project_menu_button(
                            menu_id, menu_owner,
                        ))
                        .child(
                            Button::new("new-project-chat")
                                .icon(new_chat_icon())
                                .xsmall()
                                .tooltip("このプロジェクトで新しいチャット")
                                .accessibility_label("このプロジェクトで新しいチャット")
                                .disabled(!connected)
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    let _ = desktop.update(cx, |desktop, cx| {
                                        desktop.new_chat(root.clone(), window, cx);
                                        cx.notify();
                                    });
                                }),
                        )
                    })
                    .into_any_element()
            }
        }
    }
}

impl Collapsible for SidebarSection {
    fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }
}

impl SidebarItem for SidebarSection {
    fn render(
        self,
        id: impl Into<ElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let id = id.into();
        v_flex()
            .when(!self.is_collapsed(), |section| {
                section.child(
                    h_flex()
                        .h_8()
                        .flex_shrink_0()
                        .pl_2()
                        .text_xs()
                        .text_color(cx.theme().sidebar_foreground)
                        .child(div().flex_1().child(self.label))
                        .when_some(self.add_project, |header, (desktop, enabled)| {
                            header.child(
                                Button::new("choose-project")
                                    .icon(Icon::default().path("bex/folder-plus.svg"))
                                    .small()
                                    .tooltip("プロジェクトを追加")
                                    .accessibility_label("プロジェクトを追加")
                                    .disabled(!enabled)
                                    .on_click(move |_, _, cx| {
                                        let _ = desktop.update(cx, |desktop, cx| {
                                            desktop.pick_folder();
                                            cx.notify();
                                        });
                                    }),
                            )
                        }),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .children(self.menu.into_iter().enumerate().map(|(index, row)| {
                        row.render(format!("{id}-{index}"), self.collapsed, window, cx)
                    })),
            )
    }
}

impl Desktop {
    pub(super) fn sidebar(&self, cx: &Context<Self>) -> AnyElement {
        let sidebar = match self.tab {
            Tab::Chat => self.conversation_sidebar(cx),
            Tab::Settings => self.settings_sidebar(cx),
        }
        .w_full()
        .h_auto()
        .flex_1()
        .min_h_0()
        .border_r_0()
        .bg(rgb(appearance::SIDEBAR));
        v_flex()
            .debug_selector(|| "desktop-sidebar-shell".into())
            .w(px(272.))
            .h_full()
            .flex_shrink_0()
            .bg(rgb(appearance::SIDEBAR))
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(sidebar_header(true, cx))
            .child(sidebar)
            .into_any_element()
    }

    fn conversation_sidebar(&self, cx: &Context<Self>) -> Sidebar<SidebarSection> {
        let list = self.snapshot.thread_list();
        let loading_session_list = self
            .snapshot
            .operation_running(op::OperationKey::SessionList);
        let mut projects = Vec::new();
        let mut last_section = Some("プロジェクト".to_string());
        let mut pinned_heading = false;
        for project in list
            .as_ref()
            .map(|page| page.projects.as_slice())
            .unwrap_or_default()
        {
            if project.pinned {
                last_section = None;
                if !pinned_heading {
                    projects.push(SidebarRow::Heading("ピン留め".into()));
                    pinned_heading = true;
                }
            } else {
                let section = project
                    .section
                    .clone()
                    .unwrap_or_else(|| "プロジェクト".into());
                if last_section.as_ref() != Some(&section) {
                    projects.push(SidebarRow::Heading(section.clone()));
                    last_section = Some(section);
                }
            }
            let id = project.id.clone();
            let expanded = project.expanded;
            let new_root = project
                .roots
                .first()
                .map(|root| root.path.clone())
                .unwrap_or_default();
            projects.push(SidebarRow::Project {
                id: id.clone(),
                name: project.name.clone(),
                icon_png: project.icon_png.clone(),
                monogram: project.monogram.clone(),
                color: project.icon_color,
                root: new_root,
                expanded,
                connected: self.snapshot.connected,
                desktop: cx.entity().downgrade(),
            });
            if expanded {
                for thread in &project.threads {
                    projects.push(self.draggable_chat(
                        thread,
                        self.thread_button(thread, cx).icon(Icon::empty().size_4()),
                    ));
                }
                if project.loading && !project.has_more {
                    projects.push(SidebarMenuItem::new("読み込み中…").into());
                }
                if let Some(error) = &project.error {
                    projects.push(SidebarMenuItem::new(error.clone()).into());
                    let retry_id = id.clone();
                    projects.push(
                        SidebarMenuItem::new("再試行")
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.dispatch(Intent::RefreshProject {
                                    project_id: retry_id.clone(),
                                });
                                cx.notify();
                            }))
                            .into(),
                    );
                }
                if project.has_more {
                    let mut more = SidebarMenuItem::new("もっと表示する")
                        .icon(Icon::empty().size_4())
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.dispatch(Intent::ExpandThreadList {
                                project_id: Some(id.clone()),
                            });
                            cx.notify();
                        }));
                    if project.loading {
                        more = more.suffix(|_, _| spinner::Spinner::new().small());
                    }
                    projects.push(more.disable(project.loading).into());
                }
            }
        }
        if list.as_ref().is_some_and(|page| page.has_more_projects) {
            let mut more = SidebarMenuItem::new("もっとプロジェクトを表示").on_click(cx.listener(
                |s, _, _, cx| {
                    s.dispatch(Intent::ExpandProjects);
                    cx.notify();
                },
            ));
            if list.as_ref().is_some_and(|page| page.loading_projects) {
                more = more.suffix(|_, _| spinner::Spinner::new().small());
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
                projects.push(
                    SidebarMenuItem::new(format!("一覧に戻す: {}", project.name))
                        .on_click(cx.listener(move |s, _, _, cx| {
                            s.dispatch(Intent::SetProjectHidden {
                                project_id: id.clone(),
                                hidden: false,
                            });
                            cx.notify();
                        }))
                        .into(),
                );
            }
        }
        let mut chats: Vec<SidebarRow> = list
            .as_ref()
            .map(|page| {
                page.threads
                    .iter()
                    .map(|thread| self.draggable_chat(thread, self.thread_button(thread, cx)))
                    .collect()
            })
            .unwrap_or_default();
        if list.as_ref().is_some_and(|page| page.has_more) {
            let mut more =
                SidebarMenuItem::new("もっと表示する").on_click(cx.listener(|s, _, _, cx| {
                    s.dispatch(Intent::ExpandThreadList { project_id: None });
                    cx.notify();
                }));
            if list.as_ref().is_some_and(|page| page.loading_chats) {
                more = more.suffix(|_, _| spinner::Spinner::new().small());
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
                    .when_some(
                        list.as_ref().and_then(|list| list.notice.clone()),
                        |this, notice| this.child(div().px_3().py_2().text_sm().child(notice)),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .child(
                                div().flex_1().min_w_0().child(
                                    Input::new(&self.search)
                                        .small()
                                        .prefix(IconName::Search)
                                        .appearance(false)
                                        .aria_label("会話を検索"),
                                ),
                            )
                            .child(
                                Self::icon_button(
                                    "new-chat",
                                    new_chat_icon(),
                                    "新しいチャット",
                                    cx,
                                    |s, window, cx| s.new_chat(String::new(), window, cx),
                                )
                                .disabled(!self.snapshot.connected),
                            ),
                    ),
            )
            .child(SidebarSection {
                label: "プロジェクト",
                menu: projects,
                collapsed: false,
                add_project: Some((
                    cx.entity().downgrade(),
                    self.snapshot.connected && self.remote.is_none(),
                )),
            })
            .child(SidebarSection {
                label: "チャット",
                menu: chats,
                collapsed: false,
                add_project: None,
            })
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .py_2()
                    .child(
                        Self::icon_button(
                            "sidebar-settings",
                            IconName::Settings,
                            "設定を開く",
                            cx,
                            |s, _, cx| {
                                s.open_settings();
                                if let Some(hosts) = &s.hosts {
                                    hosts.update(cx, |hosts, _| hosts.refresh());
                                }
                            },
                        )
                        .debug_selector(|| "sidebar-settings".into()),
                    )
                    .child(Self::icon_button(
                        "refresh-threads",
                        IconName::RotateCw,
                        "会話を更新",
                        cx,
                        |s, _, _| s.refresh_threads(),
                    )),
            )
    }
    fn draggable_chat(
        &self,
        thread: &agent_core::presentation::list::ThreadSummary,
        item: SidebarMenuItem,
    ) -> SidebarRow {
        SidebarRow::Chat {
            item: Box::new(item),
            drag: SidebarDrag {
                scope: self.snapshot.storage_scope.clone(),
                label: thread.title.clone(),
                kind: SidebarDragKind::Chat(thread.id.clone()),
            },
        }
    }

    fn thread_button(
        &self,
        thread: &agent_core::presentation::list::ThreadSummary,
        cx: &Context<Self>,
    ) -> SidebarMenuItem {
        let id = thread.id.clone();
        let active = thread.active;
        let unread = thread.unread;
        let worktree = thread.worktree_status.map(|status| match status {
            agent_protocol::models::WorktreeStatus::Unmerged => (
                "bex/diff.svg",
                0xfb923c,
                "main に未反映の変更あり",
                format!("thread-unmerged-{}", thread.id),
            ),
            agent_protocol::models::WorktreeStatus::Merged => (
                "bex/merge.svg",
                0xa78bfa,
                "main にマージ済み",
                format!("thread-merged-{}", thread.id),
            ),
        });
        SidebarMenuItem::new(thread.title.clone())
            .active(self.selected() == Some(&id) && self.tab != Tab::Settings)
            .suffix(move |_, _| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(active, |row| row.child(spinner::Spinner::new().small()))
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
                view.open_chat(id.clone(), window, cx);
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
        AppContext, Context, Entity, InteractiveElement, IntoElement, Modifiers, ParentElement,
        Render, Styled, TestAppContext, Window,
        component::{sidebar::SidebarItem, v_flex},
        div, px, size,
    };

    struct TaskNavigation(Entity<Desktop>);
    impl Render for TaskNavigation {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |desktop, cx| {
                let page = desktop.snapshot.thread_list().unwrap();
                v_flex()
                    .w(px(272.))
                    .children(page.threads.iter().enumerate().map(|(index, thread)| {
                        let button = desktop.thread_button(thread, cx);
                        div()
                            .debug_selector(move || format!("task-{index}"))
                            .child(button.render(format!("task-button-{index}"), window, cx))
                    }))
            })
        }
    }

    struct ProjectNavigation(Entity<Desktop>);
    impl Render for ProjectNavigation {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0
                .update(cx, |desktop, cx| desktop.conversation_sidebar(cx))
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
            connections: Arc::default(),
            closing: tokio_util::task::TaskTracker::new(),
            logging_error: None,
        };
        let snapshot = Snapshot {
            storage_scope: "drag-host".into(),
            threads: Some(Arc::new(serde_json::from_value(serde_json::json!({
                "data":[{"id":{"provider":"codex","id":"moving"},"name":"Move this chat","projectId":null}],
                "projects":[{"id":"a","name":"Alpha","roots":[]},{"id":"b","name":"Beta","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
            })).unwrap())), ..Default::default()
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
        let store = session.store.clone();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(app_runtime);
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::SideChat {
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
                desktop.session = Some(session);
            });
            ProjectNavigation(desktop)
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
            view.0
                .update(cx, |desktop, _| desktop.snapshot = store.snapshot());
            cx.notify();
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
                view.0
                    .update(cx, |desktop, _| desktop.snapshot = store.snapshot());
                cx.notify();
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
            store.snapshot().navigation.thread_id.is_none(),
            "dragging must not open the chat"
        );
    }

    #[gpui::test]
    fn settings_share_sidebar_and_return_to_the_same_conversation(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::default(),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let mut desktop = Desktop::new(Mode::Main, window, cx);
            desktop.onboarding = false;
            desktop.panel_open = false;
            let session = agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "fixture".into(),
            };
            let snapshot = Arc::make_mut(&mut desktop.snapshot);
            snapshot.threads = Some(Arc::new(
                serde_json::from_value(serde_json::json!({
                    "data":[], "projects":[{"id":"brand", "name":"remote-agent", "roots":[]}],
                     "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5, }))
                .unwrap(),
            ));
            Arc::make_mut(&mut snapshot.navigation).thread_id = Some(session.clone());
            Arc::make_mut(&mut snapshot.navigation).draft_key = session.clone().into();
            Arc::make_mut(&mut snapshot.drafts).insert(
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
            desktop
        });
        window.simulate_resize(size(px(1000.), px(700.)));
        window.run_until_parked();
        let original = window.debug_bounds("desktop-sidebar-shell").unwrap();
        let project = window.debug_bounds("project-brand").unwrap().center();
        let icon = window.debug_bounds("project-monogram").unwrap();
        assert_eq!(icon.size, size(px(16.), px(16.)));
        for expanded in [true, false] {
            window.simulate_click(project, Modifiers::default());
            view.update(window, |view, cx| {
                view.snapshot = view.session.as_ref().unwrap().store.snapshot();
                cx.notify();
            });
            window.run_until_parked();
            view.update(window, |view, _| {
                assert_eq!(
                    view.snapshot.expanded_projects.contains_key("brand"),
                    expanded
                );
            });
            assert_eq!(window.debug_bounds("project-monogram").unwrap(), icon);
        }
        let settings = window.debug_bounds("sidebar-settings").unwrap().center();
        window.simulate_click(settings, Modifiers::default());
        window.run_until_parked();
        assert_eq!(
            window.debug_bounds("desktop-sidebar-shell").unwrap(),
            original
        );
        for page in [
            SettingsPage::Models,
            SettingsPage::Agents,
            SettingsPage::Connections,
            SettingsPage::Worktrees,
        ] {
            view.update(window, |view, cx| {
                view.settings_page = page;
                cx.notify();
            });
            window.run_until_parked();
            let scope = window.debug_bounds("settings-scope").unwrap();
            assert!(scope.bottom() <= window.debug_bounds("settings-page-heading").unwrap().top());
            assert!(window.debug_bounds("settings-back").unwrap().bottom() >= px(640.));
        }
        let collapse = window.debug_bounds("collapse-sidebar").unwrap().center();
        window.simulate_click(collapse, Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("desktop-sidebar-shell").is_none());
        let expand = window.debug_bounds("expand-sidebar").unwrap().center();
        window.simulate_click(expand, Modifiers::default());
        window.run_until_parked();
        let back = window.debug_bounds("settings-back").unwrap().center();
        window.simulate_click(back, Modifiers::default());
        window.run_until_parked();
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
    fn pending_operations_do_not_block_task_navigation(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let app_runtime = Runtime {
            handle: runtime.handle().clone(),
            connections: Arc::default(),
            closing: tokio_util::task::TaskTracker::new(),
            logging_error: None,
        };
        let snapshot = Snapshot {
            threads: Some(Arc::new(
                serde_json::from_value(serde_json::json!({
                    "data":[{"id":{"provider":"codex","id":"child"},"parentId":{"provider":"codex","id":"first"},"name":"Child task","status":"running"},
                            {"id":{"provider":"codex","id":"first"},"name":"First task","worktreeStatus":"unmerged"},
                            {"id":{"provider":"codex","id":"second"},"name":"Second task","worktreeStatus":"merged"}],
                    "projects":[],"hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5,}))
                .unwrap(),
            )),
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
        let store = session.store.clone();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(app_runtime);
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::SideChat {
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
                desktop.session = Some(session);
                desktop.busy = 1;
            });
            TaskNavigation(desktop)
        });
        window.run_until_parked();
        // Leave the Tokio executor parked so both task reads remain pending.
        assert!(window.debug_bounds("bex/diff.svg").is_some());
        assert!(window.debug_bounds("bex/merge.svg").is_some());
        for (id, selector) in [("first", "task-0"), ("second", "task-1")] {
            let bounds = window.debug_bounds(selector).unwrap();
            let button = gpui::point(bounds.left() + gpui::px(70.), bounds.top() + gpui::px(14.));
            window.simulate_click(button, Modifiers::default());
            window.run_until_parked();
            assert_eq!(
                store.snapshot().navigation.thread_id.as_ref().unwrap().id,
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

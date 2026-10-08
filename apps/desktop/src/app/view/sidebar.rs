use super::*;

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
            Self::Item(item) => item
                .collapsed(collapsed)
                .render(id, window, cx)
                .into_any_element(),
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
                let selector = format!("project-{project_id}");
                h_flex()
                    .id(id)
                    .w_full()
                    .gap_1()
                    .child(
                        Button::new("heading")
                            .debug_selector(move || selector.clone())
                            .ghost()
                            .flex_1()
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
                                    toggle_set(&mut desktop.expanded_projects, &project_id);
                                    cx.notify();
                                });
                            }),
                    )
                    .when(!collapsed, |row| {
                        row.child(
                            Button::new("new-project-chat")
                                .icon(new_chat_icon())
                                .xsmall()
                                .ghost()
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
                                    .ghost()
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
        let mut projects = Vec::new();
        for project in list
            .as_ref()
            .map(|page| page.projects.as_slice())
            .unwrap_or_default()
        {
            let id = project.id.clone();
            let expanded = self.expanded_projects.contains(&id)
                || !self.snapshot.list_query.search_term.is_empty();
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
                for button in self.thread_buttons(
                    list.as_ref()
                        .map(|page| page.threads.as_slice())
                        .unwrap_or_default(),
                    Some(&project.id),
                    cx,
                ) {
                    projects.push(button.into());
                }
                if list
                    .as_ref()
                    .is_some_and(|page| page.more_project_ids.contains(&project.id))
                {
                    projects.push(
                        SidebarMenuItem::new("もっと表示する")
                            .icon(Icon::empty().size_4())
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.dispatch(Intent::ExpandThreadList {
                                    project_id: Some(id.clone()),
                                    projects: false,
                                });
                                cx.notify();
                            }))
                            .into(),
                    );
                }
            }
        }
        if list.as_ref().is_some_and(|page| page.has_more_projects) {
            projects.push(
                SidebarMenuItem::new("もっとプロジェクトを表示")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.dispatch(Intent::ExpandThreadList {
                            project_id: None,
                            projects: true,
                        });
                        cx.notify();
                    }))
                    .into(),
            );
        }
        let mut chats = Vec::new();
        for button in self.thread_buttons(
            list.as_ref()
                .map(|page| page.threads.as_slice())
                .unwrap_or_default(),
            None,
            cx,
        ) {
            chats.push(button.into());
        }
        if list.as_ref().is_some_and(|page| page.has_more_chats) {
            chats.push(
                SidebarMenuItem::new("もっと表示する")
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.dispatch(Intent::ExpandThreadList {
                            project_id: None,
                            projects: false,
                        });
                        cx.notify();
                    }))
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
    fn thread_buttons(
        &self,
        threads: &[agent_core::presentation::list::ThreadSummary],
        project_id: Option<&str>,
        cx: &Context<Self>,
    ) -> Vec<SidebarMenuItem> {
        threads
            .iter()
            .filter(|thread| thread.project_id.as_deref() == project_id)
            .map(|thread| {
                self.thread_button(thread, cx)
                    .when(project_id.is_some(), |button| {
                        button.icon(Icon::empty().size_4())
                    })
            })
            .collect()
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
                v_flex().w(px(272.)).children(
                    desktop
                        .thread_buttons(&page.threads, None, cx)
                        .into_iter()
                        .enumerate()
                        .map(|(index, button)| {
                            div()
                                .debug_selector(move || format!("task-{index}"))
                                .child(button.render(format!("task-button-{index}"), window, cx))
                        }),
                )
            })
        }
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
                    "moreProjectIds":[], "hasMoreChats":false, "hasMoreProjects":false
                }))
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
            window.run_until_parked();
            view.update(window, |view, _| {
                assert_eq!(view.expanded_projects.contains("brand"), expanded);
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
                    "projects":[],"moreProjectIds":[],"hasMoreChats":false,"hasMoreProjects":false
                }))
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

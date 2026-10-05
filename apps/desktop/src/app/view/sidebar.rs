use super::*;

#[derive(Clone)]
pub(super) struct SidebarSection {
    pub(super) label: &'static str,
    pub(super) menu: SidebarMenu,
    pub(super) add_project: Option<(WeakEntity<Desktop>, bool)>,
}

impl Collapsible for SidebarSection {
    fn is_collapsed(&self) -> bool {
        self.menu.is_collapsed()
    }

    fn collapsed(mut self, collapsed: bool) -> Self {
        self.menu = self.menu.collapsed(collapsed);
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
        v_flex()
            .when(!self.is_collapsed(), |section| {
                section.child(
                    h_flex()
                        .h_8()
                        .flex_shrink_0()
                        .px_2()
                        .text_xs()
                        .text_color(cx.theme().sidebar_foreground.opacity(0.7))
                        .child(div().flex_1().child(self.label))
                        .when_some(self.add_project, |header, (desktop, enabled)| {
                            header.child(
                                Button::new("choose-project")
                                    .icon(IconName::Plus)
                                    .xsmall()
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
            .child(self.menu.render(id, window, cx))
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
        .bg(cx.theme().sidebar);
        v_flex()
            .debug_selector(|| "desktop-sidebar-shell".into())
            .w(px(272.))
            .h_full()
            .flex_shrink_0()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(sidebar_header(true, cx))
            .child(sidebar)
            .into_any_element()
    }

    fn conversation_sidebar(&self, cx: &Context<Self>) -> Sidebar<SidebarSection> {
        let list = self.snapshot.thread_list();
        let mut projects = SidebarMenu::new().gap_1();
        for project in list
            .as_ref()
            .map(|page| page.projects.as_slice())
            .unwrap_or_default()
        {
            let id = project.id.clone();
            let expanded = self.expanded_projects.contains(&id)
                || !self.snapshot.list_query.search_term.is_empty();
            let toggle = id.clone();
            let new_root = project
                .roots
                .first()
                .map(|root| root.path.clone())
                .unwrap_or_default();
            let entity = cx.entity().downgrade();
            let connected = self.snapshot.connected;
            projects = projects.child(
                SidebarMenuItem::new(project.name.clone())
                    .icon(if expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::FolderClosed
                    })
                    .on_click(cx.listener(move |s, _, _, cx| {
                        toggle_set(&mut s.expanded_projects, &toggle);
                        cx.notify();
                    }))
                    .suffix(move |_, _| {
                        let entity = entity.clone();
                        let path = new_root.clone();
                        Button::new("new-project-chat")
                            .icon(new_chat_icon())
                            .xsmall()
                            .ghost()
                            .tooltip("このプロジェクトで新しいチャット")
                            .accessibility_label("このプロジェクトで新しいチャット")
                            .disabled(!connected)
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                let _ = entity.update(cx, |s, cx| {
                                    s.new_chat(path.clone(), window, cx);
                                    cx.notify();
                                });
                            })
                    }),
            );
            if expanded {
                for thread in list
                    .as_ref()
                    .map(|page| page.threads.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .filter(|thread| thread.project_id.as_deref() == Some(&project.id))
                {
                    projects =
                        projects.child(self.thread_button(thread, cx).icon(Icon::empty().size_4()));
                }
                if list
                    .as_ref()
                    .is_some_and(|page| page.more_project_ids.contains(&project.id))
                {
                    projects = projects.child(
                        SidebarMenuItem::new("もっと表示する")
                            .icon(Icon::empty().size_4())
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.dispatch(Intent::ExpandThreadList {
                                    project_id: Some(id.clone()),
                                    projects: false,
                                });
                                cx.notify();
                            })),
                    );
                }
            }
        }
        if list.as_ref().is_some_and(|page| page.has_more_projects) {
            projects = projects.child(SidebarMenuItem::new("もっとプロジェクトを表示").on_click(
                cx.listener(|s, _, _, cx| {
                    s.dispatch(Intent::ExpandThreadList {
                        project_id: None,
                        projects: true,
                    });
                    cx.notify();
                }),
            ));
        }
        let mut chats = SidebarMenu::new().gap_1();
        for thread in list
            .as_ref()
            .map(|page| page.threads.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|thread| thread.project_id.is_none())
        {
            chats = chats.child(self.thread_button(thread, cx));
        }
        if list.as_ref().is_some_and(|page| page.has_more_chats) {
            chats = chats.child(SidebarMenuItem::new("もっと表示する").on_click(cx.listener(
                |s, _, _, cx| {
                    s.dispatch(Intent::ExpandThreadList {
                        project_id: None,
                        projects: false,
                    });
                    cx.notify();
                },
            )));
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
                add_project: Some((
                    cx.entity().downgrade(),
                    self.snapshot.connected && self.remote.is_none(),
                )),
            })
            .child(SidebarSection {
                label: "チャット",
                menu: chats,
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
    pub(super) fn thread_button(
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
                        div().debug_selector(move || format!("task-{index}")).child(
                            desktop.thread_button(thread, cx).render(
                                format!("task-button-{index}"),
                                window,
                                cx,
                            ),
                        )
                    }))
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
            gpui_kit::init(cx);
            cx.set_global(Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::default(),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
                preferences: Arc::default(),
            });
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let mut desktop = Desktop::new(Mode::Main, window, cx);
            desktop.onboarding = false;
            desktop.panel_open = false;
            let session = agent_protocol::session::SessionRef {
                id: "fixture".into(),
            };
            let snapshot = Arc::make_mut(&mut desktop.snapshot);
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
            preferences: Arc::default(),
        };
        let snapshot = Snapshot {
            threads: Some(Arc::new(
                serde_json::from_value(serde_json::json!({
                    "data":[{"provider":"codex","id":{"id":"first"},"name":"First task","worktreeStatus":"unmerged"},
                            {"provider":"codex","id":{"id":"second"},"name":"Second task","worktreeStatus":"merged"}],
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
            gpui_kit::init(cx);
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
            let button = window.debug_bounds(selector).unwrap().center();
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
    }
}

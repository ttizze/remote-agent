use super::*;

#[derive(Clone)]
struct SidebarSection {
    label: &'static str,
    menu: SidebarMenu,
    add_project: Option<(WeakEntity<Desktop>, bool)>,
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
    pub(super) fn sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let list = self.snapshot.thread_list();
        let navigation = SidebarMenu::new().gap_1().child(
            SidebarMenuItem::new("新しいチャット")
                .icon(IconName::Plus)
                .disable(!self.snapshot.connected)
                .on_click(cx.listener(|s, _, _, cx| {
                    s.new_chat(String::new());
                    cx.notify();
                })),
        );
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
                            .icon(IconName::Plus)
                            .xsmall()
                            .ghost()
                            .tooltip("このプロジェクトで新しいチャット")
                            .accessibility_label("このプロジェクトで新しいチャット")
                            .disabled(!connected)
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                let _ = entity.update(cx, |s, cx| {
                                    s.new_chat(path.clone());
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
        let navigation = navigation
            .render("primary-navigation", window, cx)
            .into_any_element();
        Sidebar::new("desktop-sidebar")
            .w(px(272.))
            .bg(rgb(0x242424))
            .header(
                v_flex()
                    .w_full()
                    .gap_3()
                    .child(
                        h_flex()
                            .h_8()
                            .pl(px(72.))
                            .justify_end()
                            .child(self.icon_button(
                                "collapse-sidebar",
                                IconName::PanelLeftClose,
                                "サイドバーを閉じる",
                                cx,
                                |s, _, _| s.sidebar = false,
                            )),
                    )
                    .child(div().w_full().text_lg().font_semibold().child("Bex"))
                    .when_some(
                        list.as_ref().and_then(|list| list.notice.clone()),
                        |this, notice| this.child(div().px_3().py_2().text_sm().child(notice)),
                    )
                    .child(navigation)
                    .child(
                        Input::new(&self.search)
                            .small()
                            .prefix(IconName::Search)
                            .appearance(false)
                            .aria_label("会話を検索"),
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
                    .border_t_1()
                    .border_color(rgb(0x383838))
                    .py_2()
                    .child(
                        div()
                            .size_2()
                            .rounded_full()
                            .bg(rgb(if self.snapshot.connected {
                                0x37cf77
                            } else {
                                0x999999
                            })),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            self.button(
                                "sidebar-settings",
                                self.remote
                                    .as_ref()
                                    .map_or("この端末", |remote| remote.name.as_str())
                                    .to_owned(),
                                cx,
                                |s, _, cx| {
                                    s.open_settings();
                                    if let Some(hosts) = &s.hosts {
                                        hosts.update(cx, |hosts, _| hosts.refresh());
                                    }
                                },
                            )
                            .accessibility_label("設定を開く")
                            .tooltip("設定を開く")
                            .selected(self.tab == Tab::Settings),
                        ),
                    )
                    .child(self.icon_button(
                        "refresh-threads",
                        IconName::RotateCw,
                        "会話を更新",
                        cx,
                        |s, _, _| s.refresh_threads(),
                    )),
            )
            .into_any_element()
    }
    pub(super) fn thread_button(
        &self,
        thread: &agent_core::presentation::list::ThreadSummary,
        cx: &Context<Self>,
    ) -> SidebarMenuItem {
        let id = thread.id.clone();
        let active = thread.active;
        let unread = thread.unread;
        let merged = thread.worktree_merged;
        let merged_id = format!("thread-merged-{}", thread.id);
        SidebarMenuItem::new(thread.title.clone())
            .active(id == self.selected() && self.tab != Tab::Settings)
            .disable(self.busy > 0)
            .suffix(move |_, _| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(active, |row| row.child(spinner::Spinner::new().small()))
                    .when(!active && unread, |row| {
                        row.child(div().size(px(8.)).rounded_full().bg(rgb(0xffffff)))
                    })
                    .when(merged, |row| {
                        row.child(
                            div()
                                .id(merged_id.clone())
                                .role(Role::Image)
                                .child(Icon::default().path("bex/merge.svg").size_4())
                                .text_color(rgb(0xa78bfa))
                                .aria_label("main にマージ済み")
                                .tooltip(|window, cx| {
                                    tooltip::Tooltip::new("main にマージ済み").build(window, cx)
                                }),
                        )
                    })
            })
            .on_click(cx.listener(move |view, _, _, cx| {
                view.open_chat(id.clone());
                cx.notify();
            }))
    }
}

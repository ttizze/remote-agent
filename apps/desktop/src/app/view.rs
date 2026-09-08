use super::*;
use gpui_kit::component::{
    resizable::{h_resizable, resizable_panel},
    sidebar::{Sidebar, SidebarGroup, SidebarItem, SidebarMenu, SidebarMenuItem},
    tab::{Tab as UiTab, TabBar},
};
pub(super) fn button<V: 'static>(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    cx: &Context<V>,
    action: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> Button {
    let label = label.into();
    Button::new(id.into())
        .accessibility_label(label.clone())
        .child(div().flex_1().min_w_0().text_ellipsis().child(label))
        .small()
        .ghost()
        .on_click(cx.listener(move |s, _, w, cx| {
            action(s, w, cx);
            cx.notify();
        }))
}

pub(super) fn icon_button<V: 'static>(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    cx: &Context<V>,
    action: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> Button {
    Button::new(id)
        .icon(icon)
        .small()
        .ghost()
        .tooltip(label)
        .accessibility_label(label)
        .on_click(cx.listener(move |s, _, w, cx| {
            action(s, w, cx);
            cx.notify();
        }))
}

pub(super) fn diff<V: 'static>(
    diffs: &mut HashMap<String, Entity<DiffView>>,
    id: String,
    patch: &str,
    cx: &mut Context<V>,
) -> AnyElement {
    let view = diffs
        .entry(id.clone())
        .or_insert_with(|| {
            cx.new(|_| DiffView::new(patch.to_owned().into(), id == "workspace-patch"))
        })
        .clone();
    view.update(cx, |s, cx| s.set_source(patch, cx));
    view.into_any_element()
}

pub(super) fn host_menu<V: 'static>(
    id: &'static str,
    manager: &Entity<Management>,
    remote: &str,
    disabled: bool,
    cx: &Context<V>,
    select: fn(&mut V, String, &mut Window, &mut Context<V>),
) -> AnyElement {
    let mut hosts = vec![(String::new(), "この Mac".to_owned())];
    hosts.extend(
        manager
            .read(cx)
            .hosts
            .iter()
            .map(|h| (text(h, "id").to_owned(), text(h, "hostName").to_owned())),
    );
    let current = remote.to_owned();
    let entity = cx.entity().downgrade();
    let label = hosts
        .iter()
        .find(|(id, _)| id == &current)
        .map(|(_, name)| name.as_str())
        .unwrap_or("Host")
        .to_owned();
    Button::new(id)
        .label(label)
        .dropdown_caret(true)
        .small()
        .ghost()
        .disabled(disabled)
        .dropdown_menu(move |mut menu, _, _| {
            for (id, name) in &hosts {
                let entity = entity.clone();
                let id = id.clone();
                menu = menu.item(
                    PopupMenuItem::new(name.clone())
                        .checked(id == current)
                        .on_click(move |_, w, cx| {
                            let _ = entity.update(cx, |s, cx| {
                                select(s, id.clone(), w, cx);
                                cx.notify();
                            });
                        }),
                );
            }
            menu
        })
        .into_any_element()
}

impl Desktop {
    fn sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let navigation = SidebarMenu::new().gap_1().child(
            SidebarMenuItem::new("新しいチャット")
                .icon(IconName::Plus)
                .disable(!self.host.state.read(cx).connected)
                .on_click(cx.listener(|s, _, w, cx| {
                    s.new_thread(String::new(), w, cx);
                    cx.notify();
                })),
        );
        let mut projects = SidebarMenu::new().gap_1();
        for project in &self.host.state.read(cx).projects {
            let id = text(project, "id").to_owned();
            let expanded = self.expanded_projects.contains(&id)
                || !text(&self.host.state.read(cx).query, "searchTerm").is_empty();
            let toggle = id.clone();
            let new_root = project_root(project);
            let entity = cx.entity().downgrade();
            let connected = self.host.state.read(cx).connected;
            projects = projects.child(
                SidebarMenuItem::new(text(project, "name").to_owned())
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
                            .on_click(move |_, w, cx| {
                                cx.stop_propagation();
                                let _ = entity.update(cx, |s, cx| {
                                    s.new_thread(path.clone(), w, cx);
                                    cx.notify();
                                });
                            })
                    }),
            );
            if expanded {
                for thread in self
                    .host
                    .state
                    .read(cx)
                    .threads
                    .iter()
                    .filter(|t| t["projectId"] == project["id"])
                {
                    projects =
                        projects.child(self.thread_button(thread, cx).icon(Icon::empty().size_4()));
                }
                if array(&self.host.state.read(cx).more["moreProjectIds"])
                    .iter()
                    .any(|v| v == &project["id"])
                {
                    projects = projects.child(
                        SidebarMenuItem::new("もっと表示する")
                            .icon(Icon::empty().size_4())
                            .on_click(cx.listener(move |s, _, _, cx| {
                                host::send(
                                    &s.host,
                                    host::CatalogueInput::MoreProjectThreads(id.clone()),
                                );
                                cx.notify();
                            })),
                    );
                }
            }
        }
        if self.host.state.read(cx).more["hasMoreProjects"] == true {
            projects = projects.child(SidebarMenuItem::new("もっとプロジェクトを表示").on_click(
                cx.listener(|s, _, _, cx| {
                    host::send(&s.host, host::CatalogueInput::MoreProjects);
                    cx.notify();
                }),
            ));
        }
        let mut chats = SidebarMenu::new().gap_1();
        for thread in self
            .host
            .state
            .read(cx)
            .threads
            .iter()
            .filter(|t| t["projectId"].is_null())
        {
            chats = chats.child(self.thread_button(thread, cx));
        }
        if self.host.state.read(cx).more["hasMoreChats"] == true {
            chats = chats.child(SidebarMenuItem::new("もっと表示する").on_click(cx.listener(
                |s, _, _, cx| {
                    host::send(&s.host, host::CatalogueInput::MoreChats);
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
                    .child(h_flex().h_8().pl(px(72.)).justify_end().child(icon_button(
                        "collapse-sidebar",
                        IconName::PanelLeftClose,
                        "サイドバーを閉じる",
                        cx,
                        |s, _, _| s.sidebar = false,
                    )))
                    .child(
                        h_flex()
                            .w_full()
                            .child(div().flex_1().text_lg().font_semibold().child("Bex"))
                            .child(
                                icon_button(
                                    "choose-project",
                                    IconName::FolderOpen,
                                    "プロジェクトを追加",
                                    cx,
                                    |s, _, _| s.pick_folder(),
                                )
                                .disabled(!self.remote.is_empty()),
                            ),
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
            .child(SidebarGroup::new("プロジェクト").child(projects))
            .child(SidebarGroup::new("チャット").child(chats))
            .footer(
                h_flex()
                    .w_full()
                    .gap_2()
                    .border_t_1()
                    .border_color(rgb(0x383838))
                    .py_2()
                    .child(div().size_2().rounded_full().bg(rgb(
                        if self.host.state.read(cx).connected {
                            0x37cf77
                        } else {
                            0x999999
                        },
                    )))
                    .child(
                        div().flex_1().min_w_0().child(
                            button(
                                "sidebar-settings",
                                if self.remote.is_empty() {
                                    "この Mac".to_owned()
                                } else {
                                    self.manager_session
                                        .read(cx)
                                        .hosts
                                        .iter()
                                        .find(|host| text(host, "id") == self.remote)
                                        .map(|host| text(host, "hostName"))
                                        .unwrap_or("Host")
                                        .to_owned()
                                },
                                cx,
                                |s, _, cx| {
                                    s.set_tab(Tab::Settings, cx);
                                    s.refresh_manager(cx);
                                    s.refresh_worktree_settings(cx);
                                },
                            )
                            .accessibility_label("設定を開く")
                            .tooltip("設定を開く")
                            .selected(self.tab == Tab::Settings),
                        ),
                    )
                    .child(icon_button(
                        "refresh-threads",
                        IconName::RotateCw,
                        "会話を更新",
                        cx,
                        |s, _, cx| s.refresh_threads(cx),
                    )),
            )
            .into_any_element()
    }

    fn thread_button(&self, thread: &Value, cx: &Context<Self>) -> SidebarMenuItem {
        let id = text(thread, "id").to_owned();
        let active = self.host.state.read(cx).indicators.is_active(thread);
        SidebarMenuItem::new(
            thread["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("新しいチャット")
                .to_owned(),
        )
        .active(id == self.chat.read(cx).session.selected && self.tab != Tab::Settings)
        .disable(self.busy > 0)
        .when(active, |item| {
            item.suffix(|_, _| spinner::Spinner::new().small())
        })
        .when(
            !active && self.host.state.read(cx).indicators.unread.contains(&id),
            |item| item.suffix(|_, _| div().size(px(8.)).rounded_full().bg(rgb(0xffffff))),
        )
        .on_click(cx.listener(move |s, _, w, cx| {
            s.open_thread(id.clone(), w, cx);
            cx.notify();
        }))
    }

    fn files(&self, cx: &Context<Self>) -> AnyElement {
        let mut entries = v_flex().gap_1();
        for (i, entry) in self.entries.iter().enumerate() {
            let path = entry.path.to_string_lossy().into_owned();
            let download = path.clone();
            let directory = entry.directory;
            entries = entries.child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        button(
                            format!("file-{i}"),
                            entry.name.clone(),
                            cx,
                            move |s, _, _| {
                                if directory {
                                    s.browse(path.clone());
                                } else {
                                    s.edit(path.clone(), false);
                                }
                            },
                        )
                        .icon(if directory {
                            IconName::Folder
                        } else {
                            IconName::FileText
                        })
                        .flex_1(),
                    )
                    .when(!directory, |row| {
                        row.child(
                            Button::new(SharedString::from(format!("download-{i}")))
                                .icon(IconName::ArrowDown)
                                .small()
                                .ghost()
                                .tooltip("ダウンロード")
                                .accessibility_label("ダウンロード")
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.download(download.clone());
                                    cx.notify();
                                })),
                        )
                    }),
            );
        }
        let mut editor = v_flex().flex_1().min_h_0().min_w_0().gap_2();
        if let Some(file) = &self.editor {
            editor = editor
                .child(
                    h_flex().gap_2().child(IconName::FileText).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .child(file.path.to_string_lossy().into_owned()),
                    ),
                )
                .child(
                    div().flex_1().min_h_0().child(
                        Editor::new(&self.editor_input)
                            .h_full()
                            .aria_label("ファイル編集"),
                    ),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            button("save-file", "保存", cx, |s, _, cx| s.save_file(cx))
                                .primary()
                                .disabled(self.busy > 0),
                        )
                        .child(icon_button(
                            "reload-file",
                            IconName::RotateCw,
                            "再読み込み・下書きを破棄",
                            cx,
                            |s, _, _| {
                                if let Some(file) = &s.editor {
                                    s.edit(file.path.to_string_lossy().into_owned(), true);
                                }
                            },
                        ))
                        .child(
                            button("ai-edit", "AI に編集を依頼", cx, |s, w, cx| {
                                let Some(file) = &s.editor else {
                                    return;
                                };
                                let key = s.chat.read(cx).draft_key();
                                let message = json!(format!(
                                    "ファイル {} を編集してください。\n\n",
                                    file.path.display()
                                ));
                                let scope = drafts::Scope::Main;
                                drafts::update(&s.drafts, scope, cx, |mut cache| {
                                    cache["messages"][key] = message;
                                    cache
                                });
                                s.chat.update(cx, |chat, cx| chat.restore_draft(w, cx));
                                s.tab = Tab::Chat;
                            })
                            .icon(IconName::Bot),
                        ),
                );
        } else {
            editor = editor
                .items_center()
                .justify_center()
                .gap_3()
                .child(
                    Icon::new(IconName::FileText)
                        .size_8()
                        .text_color(rgb(0x777777)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x999999))
                        .child("ファイルを選択して編集"),
                );
        }
        v_flex()
            .size_full()
            .min_h_0()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(icon_button(
                        "parent-directory",
                        IconName::ArrowUp,
                        "親フォルダ",
                        cx,
                        |s, _, cx| {
                            let path = s.path.read(cx).value();
                            s.browse(
                                Path::new(path.as_ref())
                                    .parent()
                                    .unwrap_or(Path::new("/"))
                                    .to_string_lossy()
                                    .into_owned(),
                            );
                        },
                    ))
                    .child(Input::new(&self.path).small().aria_label("フォルダのパス"))
                    .child(icon_button(
                        "browse",
                        IconName::ArrowRight,
                        "フォルダを開く",
                        cx,
                        |s, _, cx| s.browse(s.path.read(cx).value().to_string()),
                    )),
            )
            .child(
                div()
                    .id("file-list")
                    .h(px(160.))
                    .flex_shrink_0()
                    .overflow_y_scroll()
                    .child(entries),
            )
            .child(div().h(px(1.)).bg(rgb(0x303030)))
            .child(editor)
            .into_any_element()
    }

    fn settings(&self, cx: &Context<Self>) -> AnyElement {
        let mut body = v_flex()
            .gap_4()
            .p_7()
            .child(div().text_2xl().child("設定"))
            .child(host_menu(
                "settings-host",
                &self.manager_session,
                &self.remote,
                self.busy > 0,
                cx,
                |s, id, w, cx| s.switch_host(id, w, cx),
            ));
        body = body.child(
            v_flex().gap_3()
                .child(div().text_xl().child("ワークツリー"))
                .child("選択中の Host に保存し、Mac・iPhone からの新規セッションに適用します。")
                .child(switch::Switch::new("worktree-create")
                    .label("新規セッションをワークツリーで開始")
                    .checked(self.worktree_settings.as_ref().is_some_and(|s| s.create_on_new_session))
                    .disabled(!self.host.state.read(cx).connected || self.worktree_settings.is_none() || self.busy > 0 || self.worktree_saving)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.save_worktree_settings(Some(WorktreeToggle::Create(*checked)), cx);
                        cx.notify();
                    })))
                .child("ワークツリーの保存先")
                .child(Input::new(&self.worktree_directory).aria_label("ワークツリーの保存先")
                    .disabled(!self.host.state.read(cx).connected || self.worktree_settings.is_none() || self.busy > 0))
                .child("指定フォルダ内にセッションごとのフォルダを作ります。空欄ならリポジトリのGit管理領域に保存します。既存のワークツリーは移動しません。")
                .child(switch::Switch::new("worktree-copy")
                    .label("ワークツリー作成時にファイルをコピー")
                    .checked(self.worktree_settings.as_ref().is_some_and(|s| s.copy_on_create))
                    .disabled(!self.host.state.read(cx).connected || self.worktree_settings.is_none() || self.busy > 0 || self.worktree_saving)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.save_worktree_settings(Some(WorktreeToggle::Copy(*checked)), cx);
                        cx.notify();
                    })))
                .child("コピー対象（リポジトリからの相対パスを1行に1つ）")
                .child(Textarea::new(&self.worktree_copy_paths).aria_label("コピー対象")
                    .readonly(!self.host.state.read(cx).connected || self.worktree_settings.is_none() || self.busy > 0))
                .child("例: .env、.env.local、config/local。存在しないパスはスキップします。指定したファイルはコピー元の内容で置き換えます。シンボリックリンクはコピーできません。")
                .child("最初のメッセージ送信時に現在の HEAD から作成します。既存セッションを開き直しても作成・コピーしません。")
                .child(div().text_sm().text_color(rgb(0x999999)).child(if self.worktree_saved {
                    "保存しました"
                } else {
                    "スイッチは切り替え時、入力欄は入力を終えると自動保存します。"
                }))
        );
        if !self.manager_session.read(cx).connected {
            body=body.child("この Mac の Host を起動").child(Input::new(&self.relay_url)).child(Input::new(&self.relay_token)).child(Input::new(&self.runner)).child(button("start-host","接続して起動",cx,|s,_,cx|{let endpoint=json!({"relayUrl":s.relay_url.read(cx).value().as_ref(),"relayToken":s.relay_token.read(cx).value().as_ref(),"runnerId":s.runner.read(cx).value().as_ref()});s.work(true,move||platform::start_host(Some(endpoint)).map(|_|Value::Null),|_,_,_,_|{});}));
        } else {
            body = body
                .child(format!(
                    "{} · relay {}",
                    text(&self.manager_session.read(cx).status, "hostName"),
                    if self.manager_session.read(cx).status["relayConnected"] == true {
                        "接続中"
                    } else {
                        "再接続中"
                    }
                ))
                .child(button("invite", "iPhone・Mac を招待", cx, |s, _, _| {
                    s.invite()
                }));
            if let Some(qr) = &self.invitation_qr {
                body = body
                    .child(img(qr.clone()).w(px(320.)).h(px(320.)))
                    .child("1回限りの招待です。相手端末で読み取るか、招待を貼り付けてください。")
                    .child(Textarea::new(&self.invitation_input).readonly(true))
                    .child(button(
                        "copy-invite",
                        "招待をコピー",
                        cx,
                        |s, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(
                                s.invitation_input.read(cx).value().to_string(),
                            ))
                        },
                    ));
            }
            for (i, device) in array(&self.manager_session.read(cx).status["devices"])
                .iter()
                .enumerate()
            {
                let identity = device["identity"].clone();
                body = body.child(
                    h_flex()
                        .gap_3()
                        .child(text(device, "name").to_owned())
                        .child(button(
                            format!("revoke-{i}"),
                            "接続を解除",
                            cx,
                            move |s, _, _| {
                                s.request(
                                    true,
                                    "host/revoke",
                                    json!({"identity":identity}),
                                    true,
                                    |s, _, _, cx| s.refresh_manager(cx),
                                )
                            },
                        )),
                );
            }
            body =
                body.child("別の Mac に接続")
                    .child(Textarea::new(&self.pairing).aria_label("ペアリング招待"))
                    .child(button("pair", "ペアリング", cx, |s, _, cx| {
                        match serde_json::from_str::<Value>(s.pairing.read(cx).value().as_ref()) {
                            Ok(invitation) => s.request(
                                true,
                                "host/pairRemote",
                                json!({"invitation":invitation,"deviceName":"Mac"}),
                                true,
                                |s, v, w, cx| {
                                    s.pairing.update(cx, |i, cx| i.set_value("", w, cx));
                                    s.refresh_manager(cx);
                                    s.switch_host(text(&v, "id").into(), w, cx);
                                },
                            ),
                            Err(e) => s.error = e.to_string(),
                        }
                    }));
            for host in &self.manager_session.read(cx).hosts {
                let id = text(host, "id").to_owned();
                body = body.child(
                    h_flex()
                        .gap_3()
                        .child(text(host, "hostName").to_owned())
                        .child(button(
                            format!("remove-{id}"),
                            "保存した接続を削除",
                            cx,
                            move |s, _, _| {
                                let id = id.clone();
                                s.request(
                                    true,
                                    "host/removeRemote",
                                    json!({"id":id}),
                                    true,
                                    move |s, _, w, cx| {
                                        if s.remote == id {
                                            s.switch_host(String::new(), w, cx);
                                        }
                                        s.refresh_manager(cx);
                                    },
                                );
                            },
                        )),
                );
            }
        }
        div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(body)
            .into_any_element()
    }

    fn workspace_card(&self, cx: &Context<Self>) -> AnyElement {
        let review = self.chat.read(cx).review.clone();
        let review_error = self.chat.read(cx).review_error.clone();
        let enabled = self.host.state.read(cx).connected && !self.cwd.is_empty();
        let mut sources = v_flex().gap_1();
        for (i, path) in self.chat.read(cx).source_paths.iter().enumerate() {
            let path = path.clone();
            sources = sources.child(
                button(
                    format!("source-{i}"),
                    basename(&path),
                    cx,
                    move |s, _, _| s.download(path.clone()),
                )
                .icon(IconName::FileText)
                .w_full(),
            );
        }
        if self.chat.read(cx).source_paths.is_empty() {
            sources = sources.child(
                div()
                    .text_sm()
                    .text_color(rgb(0x909090))
                    .py_2()
                    .child("添付ファイルがここに表示されます"),
            );
        }
        v_flex()
            .id("workspace-card")
            .w_full()
            .h_full()
            .p_4()
            .gap_4()
            .overflow_y_scroll()
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .rounded(px(18.))
                    .bg(rgb(0x282828))
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(rgb(0xa3a3a3))
                                    .child("ワークスペース"),
                            )
                            .child(
                                icon_button(
                                    "refresh-context",
                                    IconName::RotateCw,
                                    "変更を更新",
                                    cx,
                                    |s, _, cx| s.chat.update(cx, |chat, _| chat.refresh_review()),
                                )
                                .disabled(!enabled),
                            ),
                    )
                    .child(
                        button(
                            "context-files",
                            if self.cwd.is_empty() {
                                "フォルダを選択".into()
                            } else {
                                basename(&self.cwd)
                            },
                            cx,
                            |s, _, _| {
                                if s.cwd.is_empty() {
                                    s.pick_folder();
                                } else {
                                    s.browse(s.cwd.clone());
                                }
                            },
                        )
                        .icon(IconName::FolderOpen)
                        .w_full(),
                    )
                    .when(
                        review
                            .as_ref()
                            .is_some_and(|review| !review.branch.is_empty()),
                        |body| {
                            body.child(
                                div().pl_2().text_sm().text_color(rgb(0x999999)).child(
                                    review
                                        .as_ref()
                                        .map(|review| review.branch.clone())
                                        .unwrap_or_default(),
                                ),
                            )
                        },
                    )
                    .child(
                        button(
                            "context-changes",
                            format!(
                                "変更  +{}  −{}",
                                review.as_ref().map_or(0, |review| review.additions),
                                review.as_ref().map_or(0, |review| review.deletions)
                            ),
                            cx,
                            |s, _, cx| s.show_review(&s.chat.clone(), cx),
                        )
                        .icon(IconName::Replace)
                        .w_full()
                        .disabled(!enabled),
                    )
                    .child(div().h(px(1.)).my_2().bg(rgb(0x3a3a3a)))
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(rgb(0xa3a3a3))
                                    .child("ソース"),
                            )
                            .child(
                                icon_button(
                                    "context-attach",
                                    IconName::Plus,
                                    "ソースを追加",
                                    cx,
                                    |s, _, cx| s.chat.update(cx, |chat, _| chat.attach()),
                                )
                                .disabled(!enabled),
                            ),
                    )
                    .child(sources)
                    .child(
                        button("all-files", "すべてのファイル", cx, |s, _, _| {
                            s.browse(s.cwd.clone())
                        })
                        .icon(IconName::Folder)
                        .w_full()
                        .disabled(!enabled),
                    ),
            )
            .when(!review_error.is_empty(), |body| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xff8e86))
                        .child(review_error.clone()),
                )
            })
            .into_any_element()
    }

    fn workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        const TOOLS: [(Panel, IconName, &str); 4] = [
            (Panel::Terminal, IconName::SquareTerminal, "ターミナル"),
            (Panel::SideChat, IconName::Bot, "サイドチャット"),
            (Panel::Browser, IconName::Globe, "ブラウザ"),
            (Panel::Files, IconName::Folder, "ファイラ"),
        ];
        let selected = TOOLS.iter().position(|(panel, _, _)| *panel == self.panel);
        let tabs = TabBar::new("workbench-tabs")
            .small()
            .underline()
            .when_some(selected, |bar, ix| bar.selected_index(ix))
            .children(
                TOOLS
                    .iter()
                    .map(|(_, icon, label)| UiTab::new().icon(icon.clone()).aria_label(*label)),
            )
            .on_click(cx.listener(|s, ix: &usize, w, cx| {
                let panel = TOOLS[*ix].0;
                if panel == Panel::Files {
                    s.browse(s.cwd.clone());
                } else {
                    s.open_panel(panel, w, cx);
                }
            }));
        let toolbar = h_flex()
            .h_10()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(rgb(0x303030))
            .child(icon_button(
                "panel-home",
                IconName::LayoutDashboard,
                "パネルのホーム",
                cx,
                |s, _, _| s.panel = Panel::Home,
            ))
            .child(tabs)
            .child(div().flex_1())
            .when(self.panel == Panel::Terminal, |bar| {
                bar.child(icon_button(
                    "new-terminal",
                    IconName::Plus,
                    "新しいターミナル",
                    cx,
                    |s, w, cx| {
                        s.terminal = None;
                        s.open_panel(Panel::Terminal, w, cx);
                    },
                ))
            })
            .when(self.panel == Panel::SideChat, |bar| {
                bar.child(icon_button(
                    "new-side-chat",
                    IconName::Plus,
                    "新しいサイドチャット",
                    cx,
                    |s, w, cx| {
                        if let Some(chat) = &s.side_chat {
                            chat.update(cx, |chat, cx| chat.new_thread(String::new(), w, cx));
                        }
                    },
                ))
            })
            .child(icon_button(
                "close-panel",
                IconName::Close,
                "右パネルを閉じる",
                cx,
                |s, _, _| s.panel_open = false,
            ));
        let body = match self.panel {
            Panel::Home => {
                let mut chooser = v_flex().w_full().max_w(px(400.)).gap_2();
                for (ix, (panel, icon, label)) in TOOLS.iter().enumerate() {
                    let panel = *panel;
                    chooser = chooser.child(
                        button(format!("tool-{ix}"), *label, cx, move |s, w, cx| {
                            if panel == Panel::Files {
                                s.browse(s.cwd.clone());
                            } else {
                                s.open_panel(panel, w, cx);
                            }
                        })
                        .icon(icon.clone())
                        .w_full()
                        .h_10()
                        .bg(rgb(0x232323))
                        .disabled(
                            panel != Panel::Browser
                                && (!self.host.state.read(cx).connected || self.cwd.is_empty()),
                        ),
                    );
                }
                v_flex()
                    .size_full()
                    .p_8()
                    .items_center()
                    .justify_center()
                    .child(chooser)
                    .into_any_element()
            }
            Panel::Files => self.files(cx),
            Panel::Diff => v_flex()
                .size_full()
                .min_h_0()
                .p_4()
                .gap_3()
                .child(
                    h_flex()
                        .child(div().flex_1().child(format!(
                            "変更  +{} −{}",
                            self.review.as_ref().map_or(0, |review| review.additions),
                            self.review.as_ref().map_or(0, |review| review.deletions)
                        )))
                        .child(icon_button(
                            "refresh-diff",
                            IconName::RotateCw,
                            "変更を更新",
                            cx,
                            |s, _, _| s.refresh_review(),
                        )),
                )
                .when(!self.review_error.is_empty(), |v| {
                    v.child(
                        div()
                            .text_sm()
                            .text_color(rgb(0xff8e86))
                            .child(self.review_error.clone()),
                    )
                })
                .child(
                    div().flex_1().min_h_0().child(diff(
                        &mut self.diffs,
                        "workspace-patch".into(),
                        self.review
                            .as_ref()
                            .map_or("", |review| review.diff.as_str()),
                        cx,
                    )),
                )
                .into_any_element(),
            Panel::SideChat => self
                .side_chat
                .as_ref()
                .map(|v| v.clone().into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_4()
                        .child("パネルを開けませんでした。ツールを選び直してください。")
                        .into_any_element()
                }),
            Panel::Terminal => self
                .terminal
                .as_ref()
                .map(|v| v.clone().into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_4()
                        .child("パネルを開けませんでした。ツールを選び直してください。")
                        .into_any_element()
                }),
            Panel::Browser => self
                .browser
                .as_ref()
                .map(|v| v.clone().into_any_element())
                .unwrap_or_else(|| {
                    div()
                        .p_4()
                        .child("パネルを開けませんでした。ツールを選び直してください。")
                        .into_any_element()
                }),
        };
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(toolbar)
            .child(div().flex_1().min_h_0().flex().child(body))
            .into_any_element()
    }
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = !self.chat.read(cx).gallery_open() && self.panel_open && self.tab == Tab::Chat;
        let composer_visible = !self.chat.read(cx).gallery_open()
            && self.tab == Tab::Chat
            && (!self.panel_open || window.viewport_size().width >= px(1080.));
        if !composer_visible {
            self.chat.update(cx, |chat, _| chat.cancel_recording());
        }
        if let Some(chat) = self
            .side_chat
            .as_ref()
            .filter(|_| !active || self.panel != Panel::SideChat)
        {
            chat.update(cx, |chat, _| chat.cancel_recording());
        }
        if let Some(view) = self.browser.clone() {
            view.update(cx, |v, cx| {
                v.set_visible(active && self.panel == Panel::Browser, cx)
            });
        }
        if let Some(view) = self.terminal.clone() {
            view.update(cx, |v, cx| {
                v.set_visible(active && self.panel == Panel::Terminal, cx)
            });
        }
        if self.chat.read(cx).gallery_open() {
            let gallery = self.chat.clone();
            return h_flex()
                .size_full()
                .bg(rgb(0x181818))
                .text_color(rgb(0xececec))
                .font_family("Hiragino Sans")
                .text_size(px(14.))
                .child(gallery);
        }
        let wide = window.viewport_size().width >= px(1080.);
        let title = if self.tab == Tab::Settings {
            "設定"
        } else {
            self.chat.read(cx).session.conversation.thread["name"]
                .as_str()
                .or_else(|| {
                    self.host
                        .state
                        .read(cx)
                        .threads
                        .iter()
                        .find(|t| t["id"] == self.chat.read(cx).session.selected)
                        .and_then(|t| t["name"].as_str())
                })
                .unwrap_or("新しいチャット")
        }
        .to_owned();
        let mut header = h_flex().h(px(48.)).flex_shrink_0().px_4().gap_2();
        if !self.sidebar {
            header = header.pl(px(88.)).child(icon_button(
                "expand-sidebar",
                IconName::PanelLeftOpen,
                "サイドバーを開く",
                cx,
                |s, _, _| s.sidebar = true,
            ));
        }
        header = header.child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_ellipsis()
                .child(title),
        );
        if self.tab == Tab::Settings {
            header = header.child(icon_button(
                "back-chat",
                IconName::ArrowLeft,
                "チャットに戻る",
                cx,
                |s, _, cx| s.set_tab(Tab::Chat, cx),
            ));
        }
        header = header.child(
            icon_button(
                "panel-toggle",
                if self.panel_open {
                    IconName::PanelRightClose
                } else {
                    IconName::PanelRightOpen
                },
                "右パネルを切り替え",
                cx,
                |s, _, _| s.panel_open = !s.panel_open,
            )
            .selected(self.panel_open),
        );
        let content = if self.tab == Tab::Settings {
            self.settings(cx)
        } else if self.panel_open {
            let right = self.workbench(cx);
            if wide {
                h_resizable("chat-workbench-split")
                    .child(
                        resizable_panel()
                            .size_range(px(360.)..px(2400.))
                            .child(self.chat.clone().into_any_element()),
                    )
                    .child(
                        resizable_panel()
                            .size(px(540.))
                            .size_range(px(320.)..px(1100.))
                            .child(right),
                    )
                    .into_any_element()
            } else {
                right
            }
        } else {
            h_flex()
                .size_full()
                .items_stretch()
                .child(self.chat.clone().into_any_element())
                .when(window.viewport_size().width >= px(1280.), |v| {
                    v.child(
                        div()
                            .w(px(300.))
                            .flex_shrink_0()
                            .child(self.workspace_card(cx)),
                    )
                })
                .into_any_element()
        };
        let main = v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .when(!self.error.is_empty(), |body| {
                body.child(
                    h_flex()
                        .p_3()
                        .gap_2()
                        .bg(rgb(0x352523))
                        .child(Icon::new(IconName::TriangleAlert).text_color(rgb(0xff8e86)))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(rgb(0xff8e86))
                                .child(self.error.clone()),
                        )
                        .child(icon_button(
                            "dismiss-error",
                            IconName::Close,
                            "エラーを閉じる",
                            cx,
                            |s, _, _| s.error.clear(),
                        )),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_stretch()
                    .child(content),
            );
        h_flex()
            .size_full()
            .items_stretch()
            .bg(rgb(0x181818))
            .text_color(rgb(0xececec))
            .text_size(px(14.))
            .font_family("Hiragino Sans")
            .font_weight(FontWeight::NORMAL)
            .when(self.sidebar, |body| body.child(self.sidebar(window, cx)))
            .child(main)
    }
}

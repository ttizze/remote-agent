use super::*;

impl Desktop {
    pub(super) fn open_review(&mut self, path: Option<&str>, cx: &mut Context<Self>) {
        self.panel = Panel::Diff;
        self.panel_open = true;
        self.tab = Tab::Chat;
        if let Some(path) = path {
            let source = self
                .snapshot
                .workspace
                .review
                .as_ref()
                .map_or("", |review| review.diff.as_str());
            Self::diff(&mut self.diffs, "workspace-patch".into(), source, cx);
            if let Some(diff) = self.diffs.get("workspace-patch") {
                diff.update(cx, |diff, cx| {
                    diff.reveal_path(path, cx);
                });
            }
        }
        self.refresh_review();
    }

    pub(super) fn review_card(&self, cx: &Context<Self>) -> AnyElement {
        let review = self.snapshot.workspace.review.as_deref();
        let files = review
            .map(|review| review.files.as_slice())
            .unwrap_or_default();
        let visible = if self.review_expanded {
            files.len()
        } else {
            files.len().min(3)
        };
        v_flex()
            .w_full()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(0x383838))
            .overflow_hidden()
            .bg(rgb(0x191919))
            .child(
                h_flex()
                    .id("review-summary")
                    .cursor_pointer()
                    .on_click(cx.listener(|s, _, _, cx| {
                        s.open_review(None, cx);
                        cx.notify();
                    }))
                    .gap_3()
                    .p_3()
                    .bg(rgb(0x232323))
                    .border_b_1()
                    .border_color(rgb(0x383838))
                    .child(
                        div()
                            .p_2()
                            .rounded(px(9.))
                            .bg(rgb(0x141414))
                            .child(Icon::new(IconName::Replace).size_4()),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_sm()
                                    .child(format!("{} 件のファイルを変更", files.len())),
                            )
                            .child(review_counts(
                                Some(review.map_or(0, |review| review.additions)),
                                Some(review.map_or(0, |review| review.deletions)),
                            )),
                    )
                    .child(
                        self.button("review-changes", "レビューする", cx, |s, _, cx| {
                            cx.stop_propagation();
                            s.open_review(None, cx);
                        })
                        .border_1()
                        .rounded(px(8.))
                        .disabled(!self.snapshot.connected),
                    ),
            )
            .child(
                v_flex()
                    .id("review-file-list")
                    .max_h(px(252.))
                    .overflow_y_scroll()
                    .py_1()
                    .children(files.iter().take(visible).map(|file| {
                        let path = file.path.clone();
                        h_flex()
                            .id(SharedString::from(format!("review-file-{}", file.path)))
                            .cursor_pointer()
                            .on_click(cx.listener(move |s, _, _, cx| {
                                s.open_review(Some(&path), cx);
                                cx.notify();
                            }))
                            .px_3()
                            .h(px(36.))
                            .flex_shrink_0()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_ellipsis()
                                    .child(file.path.clone()),
                            )
                            .child(review_counts(file.additions, file.deletions))
                    })),
            )
            .when(files.len() > 3, |card| {
                card.child(
                    div().px_2().py_1().bg(rgb(0x232323)).child(
                        self.button(
                            "expand-review-files",
                            if self.review_expanded {
                                "折りたたむ".to_owned()
                            } else {
                                format!("あと {} 個のファイルを表示", files.len() - 3)
                            },
                            cx,
                            |s, _, _| s.review_expanded = !s.review_expanded,
                        )
                        .icon(if self.review_expanded {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        }),
                    ),
                )
            })
            .into_any_element()
    }
    pub(super) fn files(&self, cx: &Context<Self>) -> AnyElement {
        let mut entries = v_flex().gap_1();
        for (i, entry) in self
            .snapshot
            .workspace
            .directory
            .as_ref()
            .map(|directory| directory.entries.as_slice())
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            let path = entry.path.clone();
            let download = path.clone();
            let directory = entry.directory;
            entries = entries.child(
                h_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        self.button(
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
        if let Some(file) = &self.snapshot.workspace.file {
            editor = editor
                .child(
                    h_flex().gap_2().child(IconName::FileText).child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .child(file.path.clone()),
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
                            self.button("save-file", "保存", cx, |s, _, _| s.save_file())
                                .primary()
                                .disabled(self.busy > 0),
                        )
                        .child(Self::icon_button(
                            "reload-file",
                            IconName::RotateCw,
                            "再読み込み・下書きを破棄",
                            cx,
                            |s, _, _| {
                                if let Some(file) = &s.snapshot.workspace.file {
                                    s.edit(file.path.clone(), true);
                                }
                            },
                        ))
                        .child(
                            self.button("ai-edit", "AI に編集を依頼", cx, |s, _, _| {
                                if let Some(file) = &s.snapshot.workspace.file {
                                    s.dispatch(Intent::SetDraftText {
                                        thread_id: s.draft_key().clone(),
                                        text: format!(
                                            "ファイル {} を編集してください。\n\n",
                                            file.path
                                        ),
                                    });
                                }
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
                    .child(Self::icon_button(
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
                    .child(Self::icon_button(
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
                    .debug_selector(|| "conversation-files".into())
                    .h(px(160.))
                    .flex_shrink_0()
                    .overflow_y_scroll()
                    .child(entries),
            )
            .child(div().h(px(1.)).bg(rgb(0x303030)))
            .child(editor)
            .into_any_element()
    }
    pub(super) fn workbench(&mut self, cx: &mut Context<Self>) -> AnyElement {
        const TOOLS: [(Panel, IconName, &str); 4] = [
            (Panel::Terminal, IconName::SquareTerminal, "ターミナル"),
            (Panel::SideChat, IconName::Bot, "サイドチャット"),
            (Panel::Browser, IconName::Globe, "ブラウザ"),
            (Panel::Files, IconName::Folder, "ファイラ"),
        ];
        let toolbar = if self.side_chat_mode {
            h_flex().h_10().px_2().child(
                self.button(
                    "back-side-chat",
                    "サイドチャットに戻る",
                    cx,
                    |view, _, _| {
                        view.panel_open = false;
                    },
                )
                .icon(IconName::ArrowLeft)
                .debug_selector(|| "back-side-chat".into()),
            )
        } else {
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
                        s.browse(s.snapshot.navigation.cwd.clone());
                    } else {
                        s.open_panel(panel, w, cx);
                    }
                }));
            h_flex()
                .h_10()
                .px_2()
                .gap_1()
                .border_b_1()
                .border_color(rgb(0x303030))
                .child(Self::icon_button(
                    "panel-home",
                    IconName::LayoutDashboard,
                    "パネルのホーム",
                    cx,
                    |s, _, _| s.panel = Panel::Home,
                ))
                .child(tabs)
                .child(div().flex_1())
                .when(self.panel == Panel::Terminal, |bar| {
                    bar.child(Self::icon_button(
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
                    bar.child(Self::icon_button(
                        "new-side-chat",
                        new_chat_icon(),
                        "新しいサイドチャット",
                        cx,
                        |s, window, cx| {
                            if let Some(chat) = &s.side_chat {
                                chat.update(cx, |chat, cx| {
                                    chat.new_chat(String::new(), window, cx);
                                });
                            }
                        },
                    ))
                })
                .child(Self::icon_button(
                    "close-panel",
                    IconName::Close,
                    "右パネルを閉じる",
                    cx,
                    |s, _, _| s.panel_open = false,
                ))
        };
        let body = match self.panel {
            Panel::Home => {
                let mut chooser = v_flex().w_full().max_w(px(400.)).gap_2();
                for (ix, (panel, icon, label)) in TOOLS.iter().enumerate() {
                    let panel = *panel;
                    chooser = chooser.child(
                        self.button(format!("tool-{ix}"), *label, cx, move |s, w, cx| {
                            if panel == Panel::Files {
                                s.browse(s.snapshot.navigation.cwd.clone());
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
                                && (!self.snapshot.connected
                                    || self.snapshot.navigation.cwd.is_empty()),
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
                                self.snapshot
                                    .workspace
                                    .review
                                    .as_ref()
                                    .map_or(0, |review| review.additions),
                                self.snapshot
                                    .workspace
                                    .review
                                    .as_ref()
                                    .map_or(0, |review| review.deletions)
                            )))
                        .child(Self::icon_button(
                            "refresh-diff",
                            IconName::RotateCw,
                            "変更を更新",
                            cx,
                            |s, _, _| s.refresh_review(),
                        )),
                )
                .child(self.diff_file_menu(cx))
                .child(
                    div().flex_1().min_h_0().child(Self::diff(
                        &mut self.diffs,
                        "workspace-patch".into(),
                        self.snapshot
                            .workspace
                            .review
                            .as_ref()
                            .map_or("", |review| review.diff.as_str()),
                        cx,
                    )),
                )
                .into_any_element(),
            Panel::SideChat => tool_view(self.side_chat.as_ref()),
            Panel::Terminal => tool_view(self.terminal.as_ref()),
            Panel::Browser => tool_view(self.browser.as_ref()),
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

impl Desktop {
    fn diff_file_menu(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        Button::new("diff-file-navigation")
            .debug_selector(|| "diff-file-navigation".into())
            .label(format!(
                "ファイルを選択 ({})",
                self.snapshot
                    .workspace
                    .review
                    .as_ref()
                    .map_or(0, |review| review.files.len())
            ))
            .accessibility_label("差分のファイルを選択")
            .dropdown_caret(true)
            .ghost()
            .dropdown_menu(move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade()
                    && let Some(review) = &owner.read(cx).snapshot.workspace.review
                {
                    for file in &review.files {
                        let path = file.path.clone();
                        let entity = entity.clone();
                        menu = menu.item(PopupMenuItem::new(path.clone()).on_click(
                            move |_, _, cx| {
                                let _ = entity.update(cx, |s, cx| s.open_review(Some(&path), cx));
                            },
                        ));
                    }
                }
                menu
            })
            .into_any_element()
    }
}

fn tool_view<V: Render>(view: Option<&Entity<V>>) -> AnyElement {
    view.map(|view| view.clone().into_any_element())
        .unwrap_or_else(|| {
            div()
                .p_4()
                .child("パネルを開けませんでした。ツールを選び直してください。")
                .into_any_element()
        })
}

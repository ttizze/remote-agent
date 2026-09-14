use super::*;

impl Desktop {
    pub(super) fn open_settings(&mut self) {
        self.tab = Tab::Settings;
        self.worktree_removal = None;
        self.dispatch(Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}));
        self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
    }

    pub(super) fn settings(&self, cx: &Context<Self>) -> AnyElement {
        let mut body = v_flex()
            .gap_4()
            .p_7()
            .child(div().text_2xl().child("設定"))
            .child(self.host_menu("settings-host", cx));
        if let Some(hosts) = &self.hosts {
            body = body.child(hosts.clone());
        }
        body = body.child(
            v_flex().gap_3()
                .child(div().text_xl().child("ワークツリー"))
                .child("選択中の Host に保存し、すべての端末 からの新規セッションに適用します。")
                .child(switch::Switch::new("worktree-create")
                    .label("新規セッションをワークツリーで開始")
                    .checked(self.snapshot.workspace.settings.as_ref().is_some_and(|settings| settings.create_on_new_session))
                    .disabled(!self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0 || self.worktree_saving)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.save_worktree_settings(Some((true, *checked)), cx);
                        cx.notify();
                    })))
                .child("ワークツリーの保存先")
                .child(Input::new(&self.worktree_directory).aria_label("ワークツリーの保存先")
                    .disabled(!self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0))
                .child("保存先に「セッション名/リポジトリ名」の構成で作ります。空欄なら元のリポジトリ内の .worktree に保存します。既存のワークツリーは移動しません。")
                .child(switch::Switch::new("worktree-copy")
                    .label("ワークツリー作成時にファイルをコピー")
                    .checked(self.snapshot.workspace.settings.as_ref().is_some_and(|settings| settings.copy_on_create))
                    .disabled(!self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0 || self.worktree_saving)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.save_worktree_settings(Some((false, *checked)), cx);
                        cx.notify();
                    })))
                .child("コピー対象（リポジトリからの相対パスを1行に1つ）")
                .child(Textarea::new(&self.worktree_copy_paths).aria_label("コピー対象")
                    .readonly(!self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0))
                .child("例: .env、.env.local、config/local。存在しないパスはスキップします。指定したファイルはコピー元の内容で置き換えます。シンボリックリンクはコピーできません。")
                .child("最初のメッセージ送信時に現在の HEAD から作成します。既存セッションを開き直しても作成・コピーしません。")
                .child(div().text_sm().text_color(rgb(0x999999)).child(if self.worktree_saved {
                    "保存しました"
                } else {
                    "スイッチは切り替え時、入力欄は入力を終えると自動保存します。"
                }))
        );
        body = body.child(self.managed_worktrees(cx));
        div()
            .id("settings-scroll")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(body)
            .into_any_element()
    }
    pub(super) fn workspace_card(&self, cx: &Context<Self>) -> AnyElement {
        let enabled = self.snapshot.connected && !self.snapshot.navigation.cwd.is_empty();
        let review_enabled = enabled && !self.snapshot.selected_directory().is_empty();
        let mut sources = v_flex().gap_1();
        for (i, path) in self.source_paths.iter().enumerate() {
            let path = path.clone();
            sources = sources.child(
                self.button(
                    format!("source-{i}"),
                    file_name(&path),
                    cx,
                    move |s, _, _| s.download(path.clone()),
                )
                .icon(IconName::FileText)
                .w_full(),
            );
        }
        if self.source_paths.is_empty() {
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
                                self.icon_button(
                                    "refresh-context",
                                    IconName::RotateCw,
                                    "変更を更新",
                                    cx,
                                    |s, _, _| s.refresh_review(),
                                )
                                .disabled(!review_enabled),
                            ),
                    )
                    .child(
                        self.button(
                            "context-files",
                            if self.snapshot.navigation.cwd.is_empty() {
                                "フォルダを選択".into()
                            } else if self.snapshot.selected_directory().is_empty() {
                                "チャット".into()
                            } else {
                                file_name(&self.snapshot.navigation.cwd)
                            },
                            cx,
                            |s, _, _| {
                                if s.snapshot.navigation.cwd.is_empty() {
                                    s.pick_folder();
                                } else {
                                    s.browse(s.snapshot.navigation.cwd.clone());
                                }
                            },
                        )
                        .icon(IconName::FolderOpen)
                        .w_full(),
                    )
                    .when(
                        self.snapshot
                            .workspace
                            .review
                            .as_ref()
                            .is_some_and(|review| !review.branch.is_empty()),
                        |body| {
                            body.child(
                                div().pl_2().text_sm().text_color(rgb(0x999999)).child(
                                    self.snapshot
                                        .workspace
                                        .review
                                        .as_ref()
                                        .map(|review| review.branch.clone())
                                        .unwrap_or_default(),
                                ),
                            )
                        },
                    )
                    .child(
                        self.button(
                            "context-changes",
                            format!(
                                "変更  +{}  −{}",
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
                            ),
                            cx,
                            |s, _, cx| s.open_review(None, cx),
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
                                self.icon_button(
                                    "context-attach",
                                    IconName::Plus,
                                    "ソースを追加",
                                    cx,
                                    |s, _, _| s.attach(),
                                )
                                .disabled(!enabled),
                            ),
                    )
                    .child(sources)
                    .child(
                        self.button("all-files", "すべてのファイル", cx, |s, _, _| {
                            s.browse(s.snapshot.navigation.cwd.clone())
                        })
                        .icon(IconName::Folder)
                        .w_full()
                        .disabled(!enabled),
                    ),
            )
            .into_any_element()
    }
}

impl Desktop {
    fn managed_worktrees(&self, cx: &Context<Self>) -> AnyElement {
        let mut body = v_flex().gap_3().child(
            h_flex()
                .justify_between()
                .child(div().text_xl().child("作成済みのワークツリー"))
                .child(
                    self.icon_button(
                        "refresh-worktrees",
                        IconName::RotateCw,
                        "ワークツリー一覧を更新",
                        cx,
                        |s, _, _| {
                            s.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
                        },
                    )
                    .disabled(!self.snapshot.connected || self.worktree_busy),
                ),
        );
        let Some(worktrees) = &self.snapshot.workspace.worktrees else {
            return body.child("一覧を読み込み中…").into_any_element();
        };
        if worktrees.is_empty() {
            body = body.child("Bexで作成したワークツリーはありません。");
        }
        for (index, worktree) in worktrees.iter().enumerate() {
            let path = worktree.path.clone();
            let current = Path::new(&self.snapshot.navigation.cwd).starts_with(&worktree.path);
            let reason = worktree.blocked_reason.as_deref().or(current.then_some(
                "現在開いている会話の作業場所です。別の会話に移動してから削除してください。",
            ));
            let confirming = self.worktree_removal.as_deref() == Some(path.as_str());
            let mut entry = v_flex()
                .p_4()
                .gap_2()
                .rounded(px(12.))
                .border_1()
                .border_color(rgb(0x383838))
                .child(
                    h_flex()
                        .justify_between()
                        .gap_3()
                        .child(div().flex_1().min_w_0().child(format!(
                            "{} · {}",
                            file_name(&worktree.project_path),
                            worktree.branch
                        )))
                        .child(
                            self.button(
                                format!("remove-worktree-{index}"),
                                "削除…",
                                cx,
                                move |s, _, _| {
                                    s.worktree_removal = Some(path.clone());
                                },
                            )
                            .disabled(
                                reason.is_some() || !self.snapshot.connected || self.worktree_busy,
                            ),
                        ),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xaaaaaa))
                        .child(worktree.path.clone()),
                );
            for (thread_index, thread) in worktree.threads.iter().enumerate() {
                let id = thread.id.clone();
                entry = entry.child(
                    self.button(
                        format!("worktree-thread-{index}-{thread_index}"),
                        format!(
                            "{}{}",
                            if thread.active { "実行中 · " } else { "" },
                            thread.name
                        ),
                        cx,
                        move |s, _, _| s.open_chat(id.clone()),
                    )
                    .icon(IconName::FileText)
                    .disabled(!self.snapshot.connected || self.worktree_busy),
                );
            }
            if let Some(reason) = reason {
                entry = entry.child(
                    div()
                        .text_sm()
                        .text_color(rgb(0xaaaaaa))
                        .child(reason.to_owned()),
                );
            }
            if confirming {
                let path = worktree.path.clone();
                entry = entry.child(div().text_sm().child("この作業ディレクトリを削除します。ブランチと会話履歴は残りますが、この場所での作業再開はできなくなります。"))
                    .child(h_flex().gap_2()
                        .child(self.button(format!("cancel-remove-worktree-{index}"), "取消", cx, |s, _, _| s.worktree_removal = None).disabled(self.worktree_busy))
                        .child(self.button(format!("confirm-remove-worktree-{index}"), "ワークツリーを削除", cx, move |s, _, _| s.remove_worktree(path.clone()))
                            .disabled(reason.is_some() || !self.snapshot.connected || self.worktree_busy)));
            }
            body = body.child(entry);
        }
        body.into_any_element()
    }

    fn remove_worktree(&mut self, path: String) {
        if self.worktree_busy || !self.snapshot.connected {
            return;
        }
        self.worktree_busy = true;
        self.perform(
            Intent::RemoveWorktree(op::RemoveWorktree { path }),
            OperationCompletion::RemoveWorktree,
        );
    }
}

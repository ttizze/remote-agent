use super::*;

impl Desktop {
    pub(super) fn settings(&self, cx: &Context<Self>) -> AnyElement {
        let mut body = v_flex()
            .gap_4()
            .p_7()
            .child(div().text_2xl().child("設定"))
            .child(self.host_menu("settings-host", cx));
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
                .child("指定フォルダ内にセッションごとのフォルダを作ります。空欄ならリポジトリのGit管理領域に保存します。既存のワークツリーは移動しません。")
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
        if let Some(hosts) = &self.hosts {
            body = body.child(hosts.clone());
        }
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
                            |s, _, _| {
                                s.panel = Panel::Diff;
                                s.panel_open = true;
                                s.tab = Tab::Chat;
                                s.refresh_review();
                            },
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

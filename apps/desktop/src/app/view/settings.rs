use super::sidebar::SidebarSection;
use super::*;

impl Desktop {
    pub(super) fn open_settings(&mut self) {
        self.tab = Tab::Settings;
        self.settings_page = SettingsPage::Agents;
        if self.snapshot.account.login.is_none() {
            self.model_provider = None;
        }
        self.account_sign_out = None;
        self.refresh_accounts_and_models();
        self.worktree_removal = None;
        self.dispatch(Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}));
        self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
    }

    pub(super) fn settings_sidebar(&self, cx: &Context<Self>) -> Sidebar<SidebarSection> {
        let mut navigation = Vec::new();
        for (label, icon, page) in [
            ("モデル", IconName::Settings, SettingsPage::Models),
            ("エージェント", IconName::User, SettingsPage::Agents),
            ("端末と接続", IconName::Network, SettingsPage::Connections),
            ("ワークツリー", IconName::Folder, SettingsPage::Worktrees),
        ] {
            navigation.push(
                SidebarMenuItem::new(label)
                    .icon(icon)
                    .active(self.settings_page == page)
                    .on_click(cx.listener(move |s, _, _, cx| {
                        s.settings_page = page;
                        cx.notify();
                    }))
                    .into(),
            );
        }
        Sidebar::new("settings-sidebar")
            .child(SidebarSection {
                label: "設定",
                menu: navigation,
                collapsed: false,
                add_project: None,
            })
            .footer(
                self.button("settings-back", "会話に戻る", cx, |s, _, _| {
                    s.tab = Tab::Chat
                })
                .icon(IconName::ArrowLeft)
                .debug_selector(|| "settings-back".into())
                .w_full()
                .h(px(40.)),
            )
    }

    pub(super) fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let (projects, environment) = match self.settings_page {
            SettingsPage::Models => (
                self.model_scope_menu(
                    "settings-scope-projects",
                    self.snapshot
                        .model_project_scope_choices(self.settings_model_scope.clone()),
                    cx,
                ),
                self.model_scope_menu(
                    "settings-scope-environment",
                    self.snapshot
                        .model_environment_scope_choices(self.settings_model_scope.clone()),
                    cx,
                ),
            ),
            page => (
                div().child("すべてのプロジェクト").into_any_element(),
                if page == SettingsPage::Connections {
                    div().child("このPC").into_any_element()
                } else if let Some(hosts) = &self.hosts {
                    Hosts::menu(
                        hosts,
                        "settings-scope-environment",
                        self.remote.as_ref().map(|host| host.id.as_str()),
                        self.remote
                            .as_ref()
                            .map_or("Local", |host| host.name.as_str()),
                        self.busy > 0
                            || self.worktree_dirty
                            || self.worktree_saving
                            || self.snapshot.account.login.is_some(),
                        cx,
                    )
                    .into_any_element()
                } else {
                    div().child("このPC").into_any_element()
                },
            ),
        };
        let (title, subtitle) = match self.settings_page {
            SettingsPage::Models => (
                "モデル",
                "プロバイダごとの初期値と、新規チャットのモデルを設定します。",
            ),
            SettingsPage::Agents => (
                "エージェント",
                "選択した環境のAIアカウントと使用量を管理します。",
            ),
            SettingsPage::Connections => (
                "端末と接続",
                "作業する環境と、このPCへ接続する端末を管理します。",
            ),
            SettingsPage::Worktrees => (
                "ワークツリー",
                "新しい会話の作業場所と、作成済みのワークツリーを管理します。",
            ),
        };
        let heading = v_flex()
            .debug_selector(|| "settings-page-heading".into())
            .gap_2()
            .child(div().text_size(px(26.)).font_semibold().child(title))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(appearance::MUTED))
                    .child(subtitle),
            );
        let mut body = v_flex().w_full().max_w(px(960.)).gap_7().child(
            heading
                .pb_6()
                .border_b_1()
                .border_color(rgb(appearance::BORDER)),
        );
        if !self.error.is_empty() {
            body = body.child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .text_color(rgb(0xff8e86))
                            .child(error_message(&self.error)),
                    )
                    .child(Self::icon_button(
                        "settings-dismiss-error",
                        IconName::Close,
                        "エラーを閉じる",
                        cx,
                        |s, _, _| s.error.clear(),
                    )),
            );
        }
        body =
            match self.settings_page {
                SettingsPage::Models => body.child(self.default_model_settings(cx)),
                SettingsPage::Agents => body.child(self.account_controls(true, cx)),
                SettingsPage::Connections => {
                    let setup = self.snapshot.connection_setup();
                    let agent_controls = self.connection_agent_controls(&setup.agents, cx);
                    let header = section_heading(
                        "作業するコンピューター",
                        "このPCや、自分のクラウドVMを選びます。",
                    );
                    body.children(self.hosts.as_ref().map(|hosts| {
                        hosts.update(cx, |view, cx| {
                            v_flex()
                                .gap_8()
                                .child(v_flex().gap_4().child(header).child(
                                    view.connection_choices(
                                        ConnectionLayout::Settings,
                                        self.remote.as_ref().map(|host| host.id.as_str()),
                                        self.snapshot.connected,
                                        agent_controls,
                                        cx,
                                    ),
                                ))
                                .child(
                                    div()
                                        .pt_7()
                                        .border_t_1()
                                        .border_color(rgb(appearance::BORDER))
                                        .child(hosts.clone()),
                                )
                        })
                    }))
                }
                SettingsPage::Worktrees => body.child(self.worktree_settings(cx)),
            };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                h_flex()
                    .id("settings-scope")
                    .debug_selector(|| "settings-scope".into())
                    .h(px(crate::WINDOW_HEADER_HEIGHT))
                    .flex_shrink_0()
                    .gap_2()
                    .when(self.sidebar, |header| header.px_8())
                    .when(!self.sidebar, |header| {
                        header.child(sidebar_header(false, cx)).pr_8()
                    })
                    .text_xs()
                    .child(
                        div()
                            .text_color(rgb(appearance::MUTED))
                            .child("設定の適用先"),
                    )
                    .child(environment)
                    .child(div().text_color(rgb(appearance::MUTED)).child("／"))
                    .child(projects)
                    .border_b_1()
                    .border_color(rgb(appearance::BORDER)),
            )
            .child(
                div()
                    .id(match self.settings_page {
                        SettingsPage::Models => "settings-models-scroll",
                        SettingsPage::Agents => "settings-agents-scroll",
                        SettingsPage::Connections => "settings-connections-scroll",
                        SettingsPage::Worktrees => "settings-worktrees-scroll",
                    })
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().pt_6().px_8().pb_8().child(body)),
            )
            .into_any_element()
    }

    fn model_scope_menu(
        &self,
        id: &'static str,
        choices: Vec<agent_core::presentation::model_settings::ModelScopeChoice>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let selected = self.settings_model_scope.clone();
        let label = choices
            .iter()
            .find(|choice| choice.scope == selected)
            .map_or("すべてのプロジェクト", |choice| {
                choice.label.as_str()
            })
            .to_owned();
        let owner = cx.entity().downgrade();
        Button::new(id)
            .label(label)
            .dropdown_caret(true)
            .small()
            .ghost()
            .debug_selector(move || id.into())
            .dropdown_menu(move |mut menu, _, _| {
                for choice in &choices {
                    let scope = choice.scope.clone();
                    let owner = owner.clone();
                    menu = menu.item(
                        PopupMenuItem::new(choice.label.clone())
                            .checked(scope == selected)
                            .on_click(move |_, _, cx| {
                                let _ = owner.update(cx, |view, cx| {
                                    view.settings_model_scope = scope.clone();
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn worktree_settings(&self, cx: &Context<Self>) -> AnyElement {
        let disabled =
            !self.snapshot.connected || self.snapshot.workspace.settings.is_none() || self.busy > 0;
        let settings = self.snapshot.workspace.settings.as_ref();
        v_flex().gap_7()
            .child(v_flex().gap_5()
                .child(div().text_lg().font_semibold().child("新しい会話の作業場所"))
                .child(h_flex().gap_5().items_center()
                    .child(v_flex().flex_1().min_w_0().gap_1()
                        .child("ワークツリーで開始")
                        .child(div().text_sm().text_color(rgb(appearance::MUTED)).child("元の作業フォルダと分けて、新しい会話を進めます。")))
                    .child(switch::Switch::new("worktree-create")
                        .accessibility_label("新規セッションをワークツリーで開始")
                        .checked(settings.is_some_and(|settings| settings.create_on_new_session))
                        .disabled(disabled || self.worktree_saving)
                        .on_click(cx.listener(|s, checked, _, cx| {
                            s.save_worktree_settings(Some(WorktreeToggle::Create(*checked)), cx);
                            cx.notify();
                        }))))
                .child(v_flex().gap_2()
                    .child(div().text_sm().child("保存先"))
                    .child(Input::new(&self.worktree_directory).aria_label("ワークツリーの保存先").disabled(disabled))
                    .child(div().text_xs().text_color(rgb(appearance::MUTED)).child("空欄の場合は、元のリポジトリ内の .worktree に保存します。指定先には「セッション名/リポジトリ名」の構成で作ります。"))))
            .child(v_flex().gap_5().pt_6().border_t_1().border_color(rgb(appearance::BORDER))
                .child(h_flex().gap_5().items_center()
                    .child(section_heading("ファイルの引き継ぎ", "作成時に、指定したファイルを元のリポジトリからコピーします。")
                        .flex_1().min_w_0())
                    .child(switch::Switch::new("worktree-copy")
                        .accessibility_label("ワークツリー作成時にファイルをコピー")
                        .checked(settings.is_some_and(|settings| settings.copy_on_create))
                        .disabled(disabled || self.worktree_saving)
                        .on_click(cx.listener(|s, checked, _, cx| {
                            s.save_worktree_settings(Some(WorktreeToggle::Copy(*checked)), cx);
                            cx.notify();
                        }))))
                .child(v_flex().gap_2()
                    .child(div().text_sm().child("コピーするファイル"))
                    .child(Textarea::new(&self.worktree_copy_paths).aria_label("コピー対象").readonly(disabled))
                    .child(div().text_xs().text_color(rgb(appearance::MUTED)).child("リポジトリからの相対パスを1行に1つ入力します。例: .env.local、config/local。存在しないパスはスキップし、シンボリックリンクはコピーしません。")))
                .child(div().text_xs().text_color(rgb(appearance::MUTED)).child("最初のメッセージ送信時に作成・コピーします。指定ファイルはコピー元の内容で置き換え、既存セッションでは再実行しません。")))
            .child(h_flex().gap_5().items_center().pt_6().border_t_1().border_color(rgb(appearance::BORDER))
                .child(section_heading("マージ済みを自動削除", "main に取り込まれた作業場所を毎分確認します。実行中・回答待ち・ターミナル使用中・ローカル変更ありの場合は保留し、後で再確認します。ブランチと会話履歴は残ります。")
                    .flex_1().min_w_0())
                .child(switch::Switch::new("worktree-delete-merged")
                    .accessibility_label("マージ済みワークツリーを自動削除")
                    .checked(settings.is_some_and(|settings| settings.delete_merged))
                    .disabled(disabled || self.worktree_saving)
                    .on_click(cx.listener(|s, checked, _, cx| {
                        s.save_worktree_settings(Some(WorktreeToggle::DeleteMerged(*checked)), cx);
                        cx.notify();
                    }))))
            .child(div().text_xs().text_color(rgb(appearance::MUTED)).child(if self.worktree_saved { "保存しました" } else { "変更は自動で保存されます。既存のワークツリーは移動しません。" }))
            .child(div().pt_6().border_t_1().border_color(rgb(appearance::BORDER)).child(self.managed_worktrees(cx)))
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
                    .text_color(rgb(appearance::MUTED))
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
                    .bg(rgb(appearance::RAISED))
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(rgb(appearance::MUTED))
                                    .child("ワークスペース"),
                            )
                            .child(
                                Self::icon_button(
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
                                div()
                                    .pl_2()
                                    .text_sm()
                                    .text_color(rgb(appearance::MUTED))
                                    .child(
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
                    .child(div().h(px(1.)).my_2().bg(rgb(appearance::BORDER)))
                    .child(
                        h_flex()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .text_color(rgb(appearance::MUTED))
                                    .child("ソース"),
                            )
                            .child(
                                Self::icon_button(
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
                .child(
                    div()
                        .text_lg()
                        .font_semibold()
                        .child("作成済みのワークツリー"),
                )
                .child(
                    Self::icon_button(
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
            body = body.child(
                div()
                    .p_4()
                    .rounded(px(8.))
                    .border_1()
                    .border_color(rgb(appearance::BORDER))
                    .text_sm()
                    .text_color(rgb(appearance::MUTED))
                    .child("Bexで作成したワークツリーはありません。"),
            );
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
                .border_color(rgb(appearance::BORDER))
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
                        .text_color(rgb(appearance::MUTED))
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
                        move |s, window, cx| s.open_chat(id.clone(), window, cx),
                    )
                    .icon(IconName::FileText)
                    .disabled(!self.snapshot.connected || self.worktree_busy),
                );
            }
            if let Some(reason) = reason {
                entry = entry.child(
                    div()
                        .text_sm()
                        .text_color(rgb(appearance::MUTED))
                        .child(reason.to_owned()),
                );
            }
            if confirming {
                let path = worktree.path.clone();
                entry = entry.child(div().text_sm().child("この作業ディレクトリを削除します。ブランチと会話履歴は残ります。次のメッセージ送信時に、元プロジェクトの main から同じ場所に作り直します。"))
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

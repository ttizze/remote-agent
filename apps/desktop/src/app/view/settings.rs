use super::*;

impl Desktop {
    pub(super) fn open_settings(&mut self) {
        self.tab = Tab::Settings;
        self.settings_page = SettingsPage::Accounts;
        self.dispatch(Intent::ListAccounts(op::ListAccounts {}));
        self.worktree_removal = None;
        self.dispatch(Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}));
        self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
    }

    pub(super) fn settings_sidebar(&self, cx: &Context<Self>) -> AnyElement {
        let mut navigation = v_flex().gap_2();
        for (id, label, icon, page) in [
            (
                "settings-accounts",
                "アカウント",
                IconName::User,
                SettingsPage::Accounts,
            ),
            (
                "settings-connections",
                "端末と接続",
                IconName::Network,
                SettingsPage::Connections,
            ),
            (
                "settings-worktrees",
                "ワークツリー",
                IconName::Folder,
                SettingsPage::Worktrees,
            ),
        ] {
            navigation = navigation.child(
                self.button(id, label, cx, move |s, _, _| s.settings_page = page)
                    .icon(icon)
                    .w_full()
                    .h(px(44.))
                    .px_4()
                    .selected(self.settings_page == page),
            );
        }
        v_flex()
            .w(px(240.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(0x212121))
            .pt(px(52.))
            .px_4()
            .gap_6()
            .child(
                self.button("settings-back", "チャットに戻る", cx, |s, _, _| {
                    s.tab = Tab::Chat
                })
                .icon(IconName::ArrowLeft)
                .h(px(36.)),
            )
            .child(
                div()
                    .px_3()
                    .text_2xl()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("設定"),
            )
            .child(navigation)
            .into_any_element()
    }

    pub(super) fn settings(&self, cx: &Context<Self>) -> AnyElement {
        let (title, subtitle) = match self.settings_page {
            SettingsPage::Accounts => (
                "アカウント",
                if self.remote.is_some() {
                    "接続先に保存した Codex・Claude アカウントを管理します。"
                } else {
                    "この端末に保存した Codex・Claude アカウントを管理します。"
                },
            ),
            SettingsPage::Connections => {
                ("端末と接続", "端末のペアリングと保存した接続を管理します。")
            }
            SettingsPage::Worktrees => (
                "ワークツリー",
                "作業場所の作成方法と作成済みのワークツリーを管理します。",
            ),
        };
        let mut body = v_flex().w_full().max_w(px(960.)).gap_6().child(
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_2xl()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(div().text_color(rgb(0xa3a3a3)).child(subtitle)),
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
                    .child(self.icon_button(
                        "settings-dismiss-error",
                        IconName::Close,
                        "エラーを閉じる",
                        cx,
                        |s, _, _| s.error.clear(),
                    )),
            );
        }
        body = match self.settings_page {
            SettingsPage::Accounts => body.child(self.account_settings(cx)),
            SettingsPage::Connections => body.children(self.hosts.clone()),
            SettingsPage::Worktrees => body.child(self.worktree_settings(cx)),
        };
        div()
            .id(match self.settings_page {
                SettingsPage::Accounts => "settings-accounts-scroll",
                SettingsPage::Connections => "settings-connections-scroll",
                SettingsPage::Worktrees => "settings-worktrees-scroll",
            })
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(div().pt(px(64.)).px_8().pb_8().child(body))
            .into_any_element()
    }

    fn worktree_settings(&self, cx: &Context<Self>) -> AnyElement {
        let body = v_flex().gap_6().child(
            v_flex().gap_3()
                .child(div().text_xl().child("新規チャットの作業場所"))
                .child("選択中の Host に保存し、すべての端末からの新規セッションに適用します。")
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
        let body = body.child(self.managed_worktrees(cx));
        body.into_any_element()
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

impl Desktop {
    pub(in crate::app) fn account_operation(&mut self, intent: Intent) {
        if self.account_busy || self.session.is_none() || !self.snapshot.connected {
            return;
        }
        self.account_busy = true;
        self.perform(intent, OperationCompletion::Account);
    }

    fn account_settings(&self, cx: &Context<Self>) -> AnyElement {
        let disabled = !self.snapshot.connected || self.account_busy || self.busy > 0;
        let mut body = v_flex().gap_4().child(
            self.button("accounts-refresh", "再読み込み", cx, |s, _, _| {
                s.dispatch(Intent::ListAccounts(op::ListAccounts {}));
            })
            .disabled(disabled || self.snapshot.account.login.is_some()),
        );
        if let Some(accounts) = &self.snapshot.account.accounts {
            let mut rows = v_flex()
                .border_1()
                .border_color(rgb(0x383838))
                .rounded(px(8.))
                .overflow_hidden();
            if accounts.accounts.is_empty() {
                rows =
                    rows.child(div().p_6().text_color(rgb(0xa3a3a3)).child(
                        "アカウントがありません。アカウントを追加してログインしてください。",
                    ));
            }
            for (index, account) in accounts.accounts.iter().enumerate() {
                let logout_id = account.id.clone();
                let email = account.email.clone().unwrap_or_else(|| account.id.clone());
                rows = rows.child(
                    h_flex()
                        .gap_4()
                        .p_5()
                        .when(index > 0, |row| {
                            row.border_t_1().border_color(rgb(0x383838))
                        })
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_3()
                                .child(div().child(format!(
                                    "{} · {email}",
                                    match account.provider {
                                        agent_protocol::session::ProviderKind::Codex => "Codex",
                                        agent_protocol::session::ProviderKind::Claude => "Claude",
                                    }
                                )))
                                .when(accounts.is_selected(account), |row| {
                                    row.child(
                                        div().text_xs().text_color(rgb(0x8acfac)).child("選択中"),
                                    )
                                })
                                .child(account_usage_view(account.usage.as_ref(), true)),
                        )
                        .child(
                            Button::new(format!("settings-account-logout-{index}"))
                                .label("ログアウト")
                                .disabled(disabled || self.snapshot.account.login.is_some())
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.account_operation(Intent::LogoutAccount(op::LogoutAccount {
                                        id: logout_id.clone(),
                                    }));
                                    cx.notify();
                                })),
                        ),
                );
            }
            body = body.child(rows).child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa3a3a3))
                    .child("使用するアカウントは、チャットの入力欄で切り替えます。"),
            );
        } else {
            body = body.child(
                div()
                    .text_color(rgb(0xa3a3a3))
                    .child(if self.snapshot.connected {
                        "アカウントを読み込み中…"
                    } else {
                        "接続されていません。接続後にアカウントを管理できます。"
                    }),
            );
        }
        let mut add = h_flex().gap_3().flex_wrap();
        for (id, label, provider) in [
            (
                "account-start-login",
                "Codex アカウントを追加",
                agent_protocol::session::ProviderKind::Codex,
            ),
            (
                "account-start-claude-login",
                "Claude アカウントを追加",
                agent_protocol::session::ProviderKind::Claude,
            ),
        ] {
            add = add.child(
                self.button(id, label, cx, move |s, window, cx| {
                    s.account_code
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    s.account_operation(Intent::StartAccountLogin(op::StartAccountLogin {
                        provider,
                    }));
                })
                .icon(IconName::Plus)
                .disabled(disabled || self.snapshot.account.login.is_some()),
            );
        }
        body = body.child(add);
        if let Some(login) = &self.snapshot.account.login {
            let code = login.user_code.clone();
            let url = login.verification_url.clone();
            let cancel_id = login.login_id.clone();
            if login.requires_code_submission {
                let submit_id = login.login_id.clone();
                body = body.child("ブラウザで Claude にログインし、表示された認証コードを貼り付けてください。")
                    .child(Input::new(&self.account_code))
                    .child(self.button("account-submit-code", "認証コードを送信", cx, move |s, window, cx| {
                        let code = s.account_code.read(cx).value().to_string();
                        if code.trim().is_empty() { return; }
                        s.account_operation(Intent::SubmitAccountLogin(op::SubmitAccountLogin { id: submit_id.clone(), code }));
                        s.account_code.update(cx, |input, cx| input.set_value("", window, cx));
                    }).disabled(disabled));
            }
            body = body
                .when(!login.requires_code_submission, |body| {
                    body.child("ブラウザでログインし、次のコードを入力してください。")
                })
                .child(div().text_xl().child(login.user_code.clone()))
                .child(
                    h_flex()
                        .gap_2()
                        .when(!login.requires_code_submission, |row| {
                            row.child(self.button(
                                "account-copy-code",
                                "コードをコピー",
                                cx,
                                move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                                },
                            ))
                        })
                        .child(self.button(
                            "account-open-login",
                            "ブラウザでログイン",
                            cx,
                            move |_, _, cx| cx.open_url(&url),
                        ))
                        .child(
                            self.button(
                                "account-cancel-login",
                                "キャンセル",
                                cx,
                                move |s, _, _| {
                                    s.account_operation(Intent::CancelAccountLogin(
                                        op::CancelAccountLogin {
                                            id: cancel_id.clone(),
                                        },
                                    ));
                                },
                            )
                            .disabled(disabled),
                        ),
                )
                .child(
                    self.button(
                        "account-check-login",
                        "ログイン状態を確認",
                        cx,
                        |s, _, _| {
                            s.account_polling = true;
                        },
                    )
                    .disabled(disabled),
                );
        }
        body.into_any_element()
    }
}

use super::*;
use agent_protocol::session::ProviderKind;

impl Desktop {
    pub(super) fn model_menu(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let opening = entity.clone();
        popover::Popover::new("model-controls")
            .bg(rgb(0x2b2b2b))
            .rounded(px(16.))
            .border_color(rgb(0x3b3b3b))
            // Open toward the conversation. Native terminal/browser views in
            // the right panel sit above GPUI's in-window popup layer.
            .anchor(Anchor::BottomRight)
            .trigger(
                Button::new("model-select")
                    .label(
                        self.selected_model()
                            .map_or("モデルを選択".to_owned(), |model| {
                                model.display_name.clone()
                            }),
                    )
                    .debug_selector(|| "model-select".into())
                    .accessibility_label("モデルとアカウント")
                    .tooltip("モデルとアカウント")
                    .large()
                    .px_1()
                    .max_w(px(180.))
                    .h(px(44.))
                    .ghost(),
            )
            .on_open_change(move |open, window, cx| {
                if *open {
                    let _ = opening.update(cx, |view, cx| {
                        if view.snapshot.account.login.is_some() {
                            view.model_panel = ModelPanel::Manage;
                        } else {
                            view.model_provider = None;
                            view.model_panel = ModelPanel::Models;
                        }
                        view.model_search
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        view.account_sign_out = None;
                        view.dispatch(Intent::ListAccounts(op::ListAccounts {}));
                        view.dispatch(Intent::LoadModels(op::LoadModels {}));
                    });
                }
            })
            .content(move |_, _, cx| {
                entity
                    .update(cx, |s, cx| {
                        v_flex()
                            .w(px(440.))
                            .child(s.model_panel_content(cx))
                            .into_any_element()
                    })
                    .unwrap_or_else(|_| div().into_any_element())
            })
            .into_any_element()
    }
    pub(super) fn account_controls(&self, manage: bool, cx: &Context<Self>) -> AnyElement {
        let accounts = self.snapshot.account.accounts.as_ref();
        let provider = self.model_provider.unwrap_or_else(|| {
            self.snapshot
                .model_provider_for_draft(self.draft_key().into())
        });
        let disabled = self.account_busy
            || self.busy > 0
            || self.snapshot.account.login.is_some()
            || !self.snapshot.connected;
        let mut services = h_flex().gap_1();
        for (value, label) in [
            (ProviderKind::Codex, "Codex"),
            (ProviderKind::Claude, "Claude"),
        ] {
            services = services.child(
                self.button(
                    format!("model-provider-{label}"),
                    label,
                    cx,
                    move |s, _, _| {
                        s.model_provider = Some(value);
                        s.account_sign_out = None;
                    },
                )
                .debug_selector(move || format!("model-provider-{label}"))
                .selected(provider == value)
                .disabled(disabled)
                .flex_1(),
            );
        }
        let mut body = v_flex()
            .id("model-account-controls")
            .gap_3()
            .when(manage, |body| body.child(services));
        if self.snapshot.account.login.is_some() {
            body = body.child(self.account_login_controls(cx));
        } else {
            if let Some(accounts) = accounts {
                let mut found = false;
                for (index, account) in accounts
                    .accounts
                    .iter()
                    .enumerate()
                    .filter(|(_, account)| account.provider == provider)
                {
                    found = true;
                    let id = account.id.clone();
                    let logout_id = id.clone();
                    let mut row = v_flex()
                        .gap_2()
                        .p_3()
                        .rounded(px(10.))
                        .bg(rgb(0x333333))
                        .child(
                            self.button(
                                format!("account-choice-{index}"),
                                account.email.as_deref().unwrap_or(&account.id).to_owned(),
                                cx,
                                move |s, _, _| {
                                    s.account_operation(Intent::SelectAccountForDraft(
                                        op::SelectAccountForDraft {
                                            id: id.clone(),
                                            thread_id: s.draft_key().into(),
                                        },
                                    ));
                                },
                            )
                            .debug_selector(move || format!("account-choice-{index}"))
                            .when(accounts.is_selected(account), |button| {
                                button.icon(IconName::Check)
                            })
                            .w_full()
                            .disabled(disabled),
                        )
                        .when_some(account.plan_type.as_ref(), |row, plan| {
                            row.child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(0xa3a3a3))
                                    .child(plan.to_uppercase()),
                            )
                        })
                        .child(if manage {
                            account_usage_view(account.usage.as_ref())
                        } else {
                            weekly_usage_view(
                                self.snapshot.account_weekly_usage(account.id.clone()),
                            )
                        });
                    if manage && self.account_sign_out.as_deref() == Some(account.id.as_str()) {
                        row =
                            row.child(div().text_sm().child(
                                "サインアウトしますか？ 再び使うにはサインインが必要です。",
                            ))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        self.button(
                                            format!("account-logout-confirm-{index}"),
                                            "サインアウト",
                                            cx,
                                            move |s, _, _| {
                                                s.account_sign_out = None;
                                                s.account_operation(Intent::LogoutAccount(
                                                    op::LogoutAccount {
                                                        id: logout_id.clone(),
                                                    },
                                                ));
                                            },
                                        )
                                        .disabled(disabled),
                                    )
                                    .child(
                                        self.button(
                                            format!("account-logout-cancel-{index}"),
                                            "キャンセル",
                                            cx,
                                            |s, _, _| s.account_sign_out = None,
                                        )
                                        .debug_selector(
                                            move || format!("account-logout-cancel-{index}"),
                                        ),
                                    ),
                            );
                    } else if manage {
                        row = row.child(
                            self.button(
                                format!("account-logout-{index}"),
                                "サインアウト",
                                cx,
                                move |s, _, _| s.account_sign_out = Some(logout_id.clone()),
                            )
                            .debug_selector(move || format!("account-logout-{index}"))
                            .disabled(disabled),
                        );
                    }
                    body = body.child(row);
                }
                if !found {
                    body = body.child(div().text_sm().child("サインインして利用を開始できます。"));
                }
            } else {
                body = body.child("アカウントを読み込み中…");
            }
            if manage {
                body = body.child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .child(
                            self.button(
                                "account-start-login",
                                "アカウントを追加",
                                cx,
                                move |s, window, cx| {
                                    s.account_code
                                        .update(cx, |input, cx| input.set_value("", window, cx));
                                    s.account_operation(Intent::StartAccountLogin(
                                        op::StartAccountLogin { provider },
                                    ));
                                },
                            )
                            .icon(IconName::Plus)
                            .disabled(disabled),
                        )
                        .child(
                            self.button("accounts-refresh", "更新", cx, |s, _, _| {
                                s.dispatch(Intent::ListAccounts(op::ListAccounts {}));
                                s.dispatch(Intent::LoadModels(op::LoadModels {}));
                            })
                            .tooltip("モデルと使用量を更新")
                            .disabled(disabled),
                        ),
                );
            } else {
                body = body.child(
                    self.button(
                        "model-accounts-manage",
                        "アカウントを管理",
                        cx,
                        |s, _, _| {
                            s.model_panel = ModelPanel::Manage;
                        },
                    )
                    .icon(IconName::Settings)
                    .debug_selector(|| "model-accounts-manage".into())
                    .w_full(),
                );
            }
        }
        body = body
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0xa3a3a3))
                    .child("アカウントの切替は、同じ接続先を使う端末にも反映されます。"),
            )
            .when(self.account_busy, |body| {
                body.child(div().text_sm().child("アカウントを更新中…"))
            })
            .when_some(
                accounts.and_then(|accounts| accounts.error.as_ref()),
                |body, error| {
                    body.child(
                        div()
                            .text_xs()
                            .text_color(rgb(0xff7777))
                            .child(error_message(error)),
                    )
                },
            )
            .children(
                self.snapshot
                    .model_error_messages()
                    .into_iter()
                    .map(|error| {
                        div()
                            .text_xs()
                            .text_color(rgb(0xff7777))
                            .child(error_message(&error))
                    }),
            );
        if !self.error.is_empty() && self.tab != Tab::Settings {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(rgb(0xff7777))
                    .child(error_message(&self.error)),
            );
        }
        body.into_any_element()
    }
}

impl Desktop {
    pub(in crate::app) fn account_operation(&mut self, intent: Intent) {
        if self.account_busy || self.session.is_none() || !self.snapshot.connected {
            return;
        }
        if matches!(intent, Intent::StartAccountLogin(_)) {
            self.account_login_draft = Some(self.draft_key().into());
        }
        self.account_busy = true;
        self.perform(intent, OperationCompletion::Account);
    }

    fn account_login_controls(&self, cx: &Context<Self>) -> AnyElement {
        let disabled = !self.snapshot.connected || self.account_busy || self.busy > 0;
        let mut body = v_flex().gap_3();
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
                        .flex_wrap()
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

fn account_usage_view(usage: Option<&agent_protocol::operations::AccountUsage>) -> AnyElement {
    let mut body = v_flex().w_full().gap_2().text_sm();
    let Some(usage) = usage else {
        return body
            .text_color(rgb(0xa3a3a3))
            .child("使用量は未取得です")
            .into_any_element();
    };
    if let Some(error) = &usage.error {
        body = body.child(div().text_color(rgb(0xe9b56f)).child(error_message(error)));
    }
    for window in &usage.windows {
        body = body.child(
            usage_window_view(window).when_some(
                window
                    .resets_at
                    .and_then(|at| chrono::DateTime::from_timestamp(at, 0)),
                |body, at| {
                    body.child(div().text_color(rgb(0xa3a3a3)).child(format!(
                        "{} にリセット",
                        at.with_timezone(&chrono::Local).format("%m/%d %H:%M")
                    )))
                },
            ),
        );
    }
    body.when_some(
        chrono::DateTime::from_timestamp(usage.fetched_at, 0).filter(|_| usage.error.is_none()),
        |body, at| {
            body.child(div().text_color(rgb(0xa3a3a3)).child(format!(
                "{} 時点",
                at.with_timezone(&chrono::Local).format("%H:%M")
            )))
        },
    )
    .into_any_element()
}

impl Desktop {
    fn model_panel_content(&self, cx: &Context<Self>) -> AnyElement {
        let mut body = v_flex().w_full().gap_2();
        if self.model_panel != ModelPanel::Models {
            let title = if self.model_panel == ModelPanel::Accounts {
                "接続先・アカウント"
            } else {
                "アカウントを管理"
            };
            let header = h_flex()
                .gap_2()
                .h(px(44.))
                .child(
                    self.icon_button(
                        "model-panel-back",
                        IconName::ArrowLeft,
                        "戻る",
                        cx,
                        |s, _, _| {
                            s.account_sign_out = None;
                            s.model_panel = match s.model_panel {
                                ModelPanel::Manage => {
                                    s.model_provider = None;
                                    ModelPanel::Accounts
                                }
                                _ => ModelPanel::Models,
                            };
                        },
                    )
                    .disabled(self.snapshot.account.login.is_some()),
                )
                .child(div().text_base().child(title));
            return body
                .child(header)
                .child(
                    div()
                        .id("model-accounts-scroll")
                        .max_h(px(440.))
                        .overflow_y_scroll()
                        .child(self.account_controls(self.model_panel == ModelPanel::Manage, cx)),
                )
                .into_any_element();
        }
        let provider = self.model_provider.unwrap_or_else(|| {
            self.snapshot
                .model_provider_for_draft(self.draft_key().into())
        });
        let disabled = self.account_busy
            || self.busy > 0
            || !self.snapshot.connected
            || self.snapshot.account.login.is_some();
        let entity = cx.entity().downgrade();
        body = body.child(
            h_flex()
                .w_full()
                .h(px(44.))
                .px_2()
                .justify_between()
                .border_b_1()
                .border_color(rgb(0x3b3b3b))
                .child(
                    div()
                        .id("model-agent-label")
                        .debug_selector(|| "model-agent-label".into())
                        .text_base()
                        .line_height(px(20.))
                        .font_weight(FontWeight::NORMAL)
                        .child("エージェント"),
                )
                .child(
                    Button::new("model-agent")
                        .accessibility_label("エージェントを選択")
                        .child(
                            div()
                                .text_base()
                                .line_height(px(20.))
                                .font_weight(FontWeight::NORMAL)
                                .child(match provider {
                                    ProviderKind::Codex => "Codex",
                                    ProviderKind::Claude => "Claude Code",
                                }),
                        )
                        .child(Icon::new(IconName::ChevronRight).size(px(16.)))
                        .large()
                        .px_0()
                        .debug_selector(|| "model-agent".into())
                        .ghost()
                        .disabled(disabled)
                        .dropdown_menu(move |mut menu, _, _| {
                            for (provider, label) in [
                                (ProviderKind::Codex, "Codex"),
                                (ProviderKind::Claude, "Claude Code"),
                            ] {
                                let entity = entity.clone();
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_, _, cx| {
                                        let _ = entity.update(cx, |s, cx| {
                                            s.model_provider = Some(provider);
                                            if let Some(model) = s
                                                .snapshot
                                                .model_for_provider(s.draft_key().into(), provider)
                                            {
                                                s.dispatch(Intent::SelectModel {
                                                    thread_id: s.draft_key().into(),
                                                    model,
                                                });
                                            }
                                            cx.notify();
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                ),
        );
        let account =
            self.snapshot
                .account
                .accounts
                .as_ref()
                .and_then(|accounts| {
                    accounts.accounts.iter().find(|account| {
                        account.provider == provider && accounts.is_selected(account)
                    })
                });
        body = body.child(
            Button::new("model-account-summary")
                .accessibility_label("接続先・アカウントと週間残量")
                .ghost()
                .w_full()
                .h_auto()
                .debug_selector(|| "model-account-summary".into())
                .p_2()
                .rounded(px(8.))
                .on_click(cx.listener(|s, _, _, cx| {
                    s.model_panel = ModelPanel::Accounts;
                    cx.notify();
                }))
                .child(
                    v_flex()
                        .id("model-account-row")
                        .debug_selector(|| "model-account-row".into())
                        .w_full()
                        .gap_2()
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .gap_2()
                                .text_base()
                                .line_height(px(20.))
                                .font_weight(FontWeight::NORMAL)
                                .child(
                                    div()
                                        .id("model-account-label")
                                        .debug_selector(|| "model-account-label".into())
                                        .flex_shrink_0()
                                        .child("接続先・アカウント"),
                                )
                                .child(
                                    div().flex_1().min_w_0().text_right().truncate().child(
                                        account
                                            .map_or("未選択", |a| {
                                                a.email.as_deref().unwrap_or(&a.id)
                                            })
                                            .to_owned(),
                                    ),
                                )
                                .child(Icon::new(IconName::ChevronRight).size(px(16.))),
                        )
                        .child(weekly_usage_view(
                            account
                                .map(|a| self.snapshot.account_weekly_usage(a.id.clone()))
                                .unwrap_or_default(),
                        )),
                ),
        );
        body = body
            .child(div().h(px(1.)).w_full().bg(rgb(0x3b3b3b)))
            .child(
                Input::new(&self.model_search)
                    .prefix(IconName::Search)
                    .border_1()
                    .border_color(rgb(0x454b55))
                    .large()
                    .aria_label("モデルを検索"),
            );
        let models = self
            .snapshot
            .provider_models_matching(provider, self.model_search.read(cx).value().to_string());
        let mut list = v_flex()
            .id("model-catalog")
            .max_h(px(240.))
            .overflow_y_scroll()
            .gap_1();
        if models.is_empty() {
            list = list.child("利用可能なモデルがありません");
        }
        for model in models {
            let value = model.model.clone();
            let selector = format!("model-choice-{}", model.id);
            let selected = self.draft().model.as_ref() == Some(&model.model);
            list = list.child(
                Button::new(selector.clone())
                    .accessibility_label(model.display_name.clone())
                    .on_click(cx.listener(move |s, _, _, _| {
                        s.dispatch(Intent::SelectModel {
                            thread_id: s.draft_key().into(),
                            model: value.clone(),
                        });
                    }))
                    .debug_selector(move || selector.clone())
                    .ghost()
                    .w_full()
                    .h(px(44.))
                    .px_2()
                    .disabled(disabled)
                    .selected(selected)
                    .when(selected, |button| button.bg(rgb(0x343b46)))
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_base()
                                    .line_height(px(20.))
                                    .font_weight(FontWeight::NORMAL)
                                    .child(model.display_name),
                            )
                            .when(selected, |row| {
                                row.child(
                                    Icon::new(IconName::Check)
                                        .size(px(20.))
                                        .text_color(rgb(0x5aa2ff)),
                                )
                            }),
                    ),
            );
        }
        body.child(list)
            .children(
                self.snapshot
                    .model_error_messages()
                    .into_iter()
                    .map(|error| {
                        div()
                            .text_xs()
                            .text_color(rgb(0xff7777))
                            .child(error_message(&error))
                    }),
            )
            .when(!self.error.is_empty(), |body| {
                body.child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xff7777))
                        .child(error_message(&self.error)),
                )
            })
            .into_any_element()
    }

    pub(super) fn fast_control(&self, cx: &Context<Self>) -> AnyElement {
        let controls = self.snapshot.model_quick_controls(self.draft_key().into());
        let Some(next) = controls.toggle_fast_to else {
            return div().into_any_element();
        };
        let label = if controls.fast {
            "Fast：オン"
        } else {
            "Fast：オフ"
        };
        self.icon_button(
            "model-fast",
            Icon::default().path("bex/bolt.svg"),
            label,
            cx,
            move |s, _, _| {
                s.dispatch(Intent::SelectServiceTier {
                    thread_id: s.draft_key().into(),
                    service_tier: next.clone(),
                });
            },
        )
        .debug_selector(|| "model-fast".into())
        .w(px(40.))
        .h(px(44.))
        .text_color(if controls.fast {
            rgb(0x78adff)
        } else {
            rgb(0x999999)
        })
        .disabled(!self.snapshot.connected || self.account_busy || self.busy > 0)
        .into_any_element()
    }

    pub(super) fn effort_control(&self, cx: &Context<Self>) -> AnyElement {
        let controls = self.snapshot.model_quick_controls(self.draft_key().into());
        if controls.efforts.is_empty() {
            return div().into_any_element();
        }
        let label = format!("推論の強度：{}", controls.effort);
        let mut bars = h_flex().items_end().gap(px(2.));
        for index in 0..controls.efforts.len() {
            bars = bars.child(
                div()
                    .w(px(3.))
                    .h(px(
                        6. + 10. * (index + 1) as f32 / controls.efforts.len() as f32
                    ))
                    .rounded(px(1.))
                    .bg(if (index as u32) < controls.effort_level {
                        rgb(0x78adff)
                    } else {
                        rgb(0x555555)
                    }),
            );
        }
        let entity = cx.entity().downgrade();
        Button::new("model-effort")
            .child(bars)
            .ghost()
            .w(px(44.))
            .h(px(44.))
            .debug_selector(|| "model-effort".into())
            .tooltip(label.clone())
            .accessibility_label(label)
            .disabled(
                !self.snapshot.connected
                    || self.account_busy
                    || self.busy > 0
                    || self.snapshot.account.login.is_some(),
            )
            .dropdown_menu_with_anchor(Anchor::BottomRight, move |mut menu, _, _| {
                for effort in &controls.efforts {
                    let value = effort.clone();
                    let entity = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(effort.clone())
                            .checked(*effort == controls.effort)
                            .on_click(move |_, _, cx| {
                                let _ = entity.update(cx, |s, cx| {
                                    s.dispatch(Intent::SelectEffort {
                                        thread_id: s.draft_key().into(),
                                        effort: value.clone(),
                                    });
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }
}

fn weekly_usage_view(windows: Vec<agent_protocol::operations::UsageWindow>) -> AnyElement {
    let mut body = v_flex()
        .w_full()
        .gap_2()
        .text_sm()
        .font_weight(FontWeight::NORMAL);
    if windows.is_empty() {
        return body
            .text_color(rgb(0xa3a3a3))
            .child("残量未取得")
            .into_any_element();
    }
    for window in windows {
        body = body.child(
            h_flex()
                .w_full()
                .gap_3()
                .child(usage_bar(window.remaining_percent).flex_1())
                .child(
                    div()
                        .line_height(px(20.))
                        .text_color(rgb(0xa3a3a3))
                        .child(format!("{}%", window.remaining_percent)),
                ),
        );
    }
    body.into_any_element()
}

fn usage_window_view(window: &agent_protocol::operations::UsageWindow) -> Div {
    v_flex()
        .w_full()
        .gap_1()
        .child(
            h_flex()
                .justify_between()
                .gap_2()
                .child(window.label.clone())
                .child(format!("残り {}%", window.remaining_percent)),
        )
        .child(usage_bar(window.remaining_percent))
}

fn usage_bar(remaining_percent: u32) -> Div {
    div()
        .h(px(4.))
        .w_full()
        .rounded(px(4.))
        .bg(rgb(0x474747))
        .child(
            div()
                .h_full()
                .w(relative(remaining_percent.min(100) as f32 / 100.))
                .rounded(px(4.))
                .bg(if remaining_percent <= 20 {
                    rgb(0xe9b56f)
                } else {
                    rgb(0x8acfac)
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::{Desktop, Draft, Mode, ModelPanel, RemoteHost};
    use gpui_kit as gpui;
    use gpui_kit::component::{h_flex, v_flex};
    use gpui_kit::{AppContext, Modifiers, Render, TestAppContext};
    use gpui_kit::{Context, Entity, IntoElement, ParentElement, Styled, Window, px};
    use std::sync::Arc;

    struct PickerView(Entity<Desktop>);
    impl Render for PickerView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |view, cx| {
                v_flex()
                    .w(px(420.))
                    .gap_3()
                    .child(
                        h_flex()
                            .child(view.fast_control(cx))
                            .child(view.model_menu(cx))
                            .child(view.effort_control(cx)),
                    )
                    .child(view.model_panel_content(cx))
            })
        }
    }

    #[gpui::test]
    fn model_picker_keeps_quick_controls_and_routes_quota_to_account_management(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let (view, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| Desktop::new(Mode::SideChat {
                remote: Some(RemoteHost { id: "fixture".into(), name: "fixture".into(), ticket: "invalid-fixture-ticket".into() }), cwd: "/fixture".into(),
            }, window, cx));
            desktop.update(cx, |view, _| {
                let key = view.draft_key().to_owned();
                let snapshot = Arc::make_mut(&mut view.snapshot);
                snapshot.connected = true;
                Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(serde_json::from_value(serde_json::json!({
                    "accounts":[{"provider":"codex","id":"first","email":"first@example.invalid","usage":{"windows":[{"label":"週間枠","remainingPercent":42}],"fetchedAt":1}},
                    {"provider":"claude","id":"claude:second"}],"selectedId":"first","selectedClaudeId":"claude:second"
                })).unwrap()));
                snapshot.models = Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":"gpt","model":"gpt","displayName":"GPT","defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],"serviceTiers":[{"id":"priority"}]},
                    {"id":"claude:sonnet","model":"claude:sonnet","displayName":"Sonnet","defaultReasoningEffort":"","supportedReasoningEfforts":[]}
                ])).unwrap());
                Arc::make_mut(&mut snapshot.drafts).insert(key, Arc::new(Draft { model: Some("gpt".into()), ..Default::default() }));
            });
            cx.observe(&desktop, |_, _, cx| cx.notify()).detach();
            PickerView(desktop)
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("model-fast").unwrap().right()
                <= window.debug_bounds("model-select").unwrap().left()
        );
        assert!(
            window.debug_bounds("model-select").unwrap().right()
                <= window.debug_bounds("model-effort").unwrap().left()
        );
        assert!(window.debug_bounds("model-choice-gpt").is_some());
        assert!(window.debug_bounds("model-choice-claude:sonnet").is_none());
        assert!(window.debug_bounds("account-logout-0").is_none());
        let summary = window.debug_bounds("model-account-summary").unwrap();
        let agent_label = window.debug_bounds("model-agent-label").unwrap();
        let account_label = window.debug_bounds("model-account-label").unwrap();
        assert!((agent_label.left() - account_label.left()).abs() <= px(1.));
        assert_eq!(agent_label.size.height, account_label.size.height);
        let account_row = window.debug_bounds("model-account-row").unwrap();
        assert!(account_row.size.width >= summary.size.width - px(24.));
        window.simulate_click(summary.center(), Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("account-choice-0").is_some());
        assert!(window.debug_bounds("account-choice-1").is_none());
        assert!(window.debug_bounds("account-logout-0").is_none());
        let manage = window.debug_bounds("model-accounts-manage").unwrap();
        window.simulate_click(manage.center(), Modifiers::default());
        window.run_until_parked();
        let logout = window.debug_bounds("account-logout-0").unwrap();
        window.simulate_click(logout.center(), Modifiers::default());
        window.run_until_parked();
        window.update(|_, cx| {
            assert_eq!(
                view.read(cx).0.read(cx).account_sign_out.as_deref(),
                Some("first")
            )
        });
        let cancel = window.debug_bounds("account-logout-cancel-0").unwrap();
        window.simulate_click(cancel.center(), Modifiers::default());
        window.update(|_, cx| assert!(view.read(cx).0.read(cx).account_sign_out.is_none()));
        window.update(|window, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.model_panel = ModelPanel::Models;
                view.model_search
                    .update(cx, |input, cx| input.set_value("no match", window, cx));
                cx.notify();
            })
        });
        window.run_until_parked();
        assert!(window.debug_bounds("model-choice-gpt").is_none());
        window.update(|_, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.model_panel = ModelPanel::Manage;
                view.account_busy = true;
                cx.notify();
            })
        });
        window.run_until_parked();
        let claude = window.debug_bounds("model-provider-Claude").unwrap();
        window.simulate_click(claude.center(), Modifiers::default());
        window.update(|_, cx| assert!(view.read(cx).0.read(cx).model_provider.is_none()));
    }
}

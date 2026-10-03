use super::*;
use agent_protocol::session::ProviderKind;

fn provider_icon(provider: ProviderKind) -> Icon {
    Icon::default().path(match provider {
        ProviderKind::Codex => "bex/openai.svg",
        ProviderKind::Claude => "bex/anthropic.svg",
    })
}

fn account_identity(provider: Option<ProviderKind>, identity: &str) -> Div {
    h_flex()
        .min_w_0()
        .gap_1()
        .when_some(provider, |row, provider| {
            row.child(provider_icon(provider).size(px(12.)))
        })
        .child(div().min_w_0().text_ellipsis().child(identity.to_owned()))
}

impl Desktop {
    pub(super) fn default_model_settings(&self, cx: &Context<Self>) -> AnyElement {
        let defaults = self
            .snapshot
            .model_defaults(self.settings_model_scope.clone());
        let model = self
            .snapshot
            .default_model(self.settings_model_scope.clone());
        let label = defaults
            .model
            .as_ref()
            .and(model.as_ref())
            .map(|model| model.display_name.clone())
            .or_else(|| defaults.model.as_ref().map(|model| model.id.clone()))
            .unwrap_or_else(|| "自動".into());
        let selected = defaults.model.clone();
        let models = self.snapshot.models.clone();
        let entity = cx.entity().downgrade();
        let scope = self.settings_model_scope.clone();
        let model_picker = Button::new("default-model")
            .disabled(self.session.is_none())
            .label(label)
            .accessibility_label("新しい会話のモデル")
            .debug_selector(|| "default-model".into())
            .ghost()
            .when_some(model.as_ref(), |button, model| {
                button.icon(provider_icon(model.model.provider))
            })
            .child(Icon::new(IconName::ChevronDown).size(px(14.)))
            .dropdown_menu(move |mut menu, _, _| {
                let automatic = entity.clone();
                let automatic_scope = scope.clone();
                menu = menu.item(
                    PopupMenuItem::new("自動")
                        .checked(selected.is_none())
                        .on_click(move |_, _, cx| {
                            let _ = automatic.update(cx, |s, cx| {
                                s.dispatch(Intent::SelectDefaultModel {
                                    scope: automatic_scope.clone(),
                                    model: None,
                                });
                                cx.notify();
                            });
                        }),
                );
                for model in models.iter() {
                    let value = model.model.clone();
                    let scope = scope.clone();
                    let entity = entity.clone();
                    let provider = match model.model.provider {
                        ProviderKind::Codex => "Codex",
                        ProviderKind::Claude => "Claude",
                    };
                    menu = menu.item(
                        PopupMenuItem::new(format!("{provider} · {}", model.display_name))
                            .checked(selected.as_ref() == Some(&model.model))
                            .on_click(move |_, _, cx| {
                                let _ = entity.update(cx, |s, cx| {
                                    s.dispatch(Intent::SelectDefaultModel {
                                        scope: scope.clone(),
                                        model: Some(value.clone()),
                                    });
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            });
        let mut controls = h_flex().gap_2().child(model_picker);
        let quick = self
            .snapshot
            .default_model_controls(self.settings_model_scope.clone());
        if !quick.efforts.is_empty() {
            let efforts = quick.efforts.clone();
            let entity = cx.entity().downgrade();
            let effort = defaults.effort.clone();
            let scope = self.settings_model_scope.clone();
            controls = controls.child(
                Button::new("default-model-effort")
                    .disabled(self.session.is_none())
                    .label(effort.clone().unwrap_or_else(|| "自動".into()))
                    .accessibility_label("新しい会話の推論強度")
                    .debug_selector(|| "default-model-effort".into())
                    .ghost()
                    .child(Icon::new(IconName::ChevronDown).size(px(14.)))
                    .dropdown_menu(move |mut menu, _, _| {
                        for value in std::iter::once(None).chain(efforts.iter().cloned().map(Some))
                        {
                            let entity = entity.clone();
                            let scope = scope.clone();
                            menu = menu.item(
                                PopupMenuItem::new(value.clone().unwrap_or_else(|| "自動".into()))
                                    .checked(value == effort)
                                    .on_click(move |_, _, cx| {
                                        let _ = entity.update(cx, |s, cx| {
                                            s.dispatch(Intent::SelectDefaultEffort {
                                                scope: scope.clone(),
                                                effort: value.clone(),
                                            });
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            );
        }
        if let Some(fast_tier) = quick.fast_service_tier {
            let entity = cx.entity().downgrade();
            let selected = defaults.service_tier.clone();
            let scope = self.settings_model_scope.clone();
            controls = controls.child(
                Button::new("default-model-speed")
                    .disabled(self.session.is_none())
                    .label(if quick.fast { "高速" } else { "通常" })
                    .icon(Icon::default().path("bex/bolt.svg"))
                    .accessibility_label("新しい会話の速度")
                    .debug_selector(|| "default-model-speed".into())
                    .ghost()
                    .child(Icon::new(IconName::ChevronDown).size(px(14.)))
                    .dropdown_menu(move |mut menu, _, _| {
                        for (label, value) in [
                            ("自動", None),
                            ("通常", Some("default".to_owned())),
                            ("高速", Some(fast_tier.clone())),
                        ] {
                            let entity = entity.clone();
                            let scope = scope.clone();
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .checked(value == selected)
                                    .on_click(move |_, _, cx| {
                                        let _ = entity.update(cx, |s, cx| {
                                            s.dispatch(Intent::SelectDefaultServiceTier {
                                                scope: scope.clone(),
                                                service_tier: value.clone(),
                                            });
                                            cx.notify();
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            );
        }
        v_flex()
            .gap_4()
            .child(div().text_lg().font_semibold().child("新しい会話"))
            .child(
                h_flex()
                    .items_center()
                    .gap_5()
                    .p_4()
                    .rounded(px(14.))
                    .border_1()
                    .border_color(rgb(0x2b2f35))
                    .child(
                        v_flex().flex_1().min_w_0().gap_1().child("モデル").child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x949ca8))
                                .child("新しい会話で使うモデル・推論強度・速度の初期値です。"),
                        ),
                    )
                    .child(controls),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x949ca8))
                    .child("変更はこの端末に自動保存されます。会話ごとに変更できます。"),
            )
            .when(
                self.snapshot
                    .has_model_defaults_override(self.settings_model_scope.clone()),
                |view| {
                    let scope = self.settings_model_scope.clone();
                    view.child(
                        self.button(
                            "model-defaults-inherit",
                            "共通設定を使う",
                            cx,
                            move |s, _, _| {
                                s.dispatch(Intent::InheritModelDefaults {
                                    scope: scope.clone(),
                                });
                            },
                        )
                        .ghost(),
                    )
                },
            )
            .children(
                self.snapshot
                    .model_error_messages(defaults.model.as_ref().map(|model| model.provider))
                    .into_iter()
                    .map(|error| {
                        div()
                            .text_sm()
                            .text_color(rgb(0xff8e86))
                            .child(error_message(&error))
                    }),
            )
            .into_any_element()
    }

    pub(super) fn refresh_accounts_and_models(&self) {
        self.dispatch(Intent::ListAccounts(op::ListAccounts {}));
        self.dispatch(Intent::LoadModels(op::LoadModels {}));
    }

    pub(super) fn model_menu(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let opening = entity.clone();
        popover::Popover::new("model-controls")
            .bg(rgb(0x171819))
            .rounded(px(10.))
            .border_color(rgb(0x2a2c2e))
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
                        view.refresh_accounts_and_models();
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
        let in_settings = self.tab == Tab::Settings;
        let accounts = self.snapshot.account.accounts.as_ref();
        let provider = self.account_provider();
        let disabled = self.account_busy
            || self.busy > 0
            || self.snapshot.account.login.is_some()
            || !self.snapshot.connected;
        let mut services = h_flex()
            .gap_1()
            .when(in_settings, |row| row.max_w(px(320.)));
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
                .when(in_settings, |button| {
                    button.small().ghost().icon(provider_icon(value))
                })
                .disabled(disabled)
                .flex_1(),
            );
        }
        let mut body = v_flex()
            .id("model-account-controls")
            .gap_3()
            .when(in_settings, |body| body.gap_5())
            .when(manage, |body| body.child(services));
        if self.snapshot.account.login.is_some() {
            body = body.child(self.account_login_controls(None, cx));
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
                    let provider = account.provider;
                    let logout_id = id.clone();
                    let identity = account.email.as_deref().unwrap_or(&account.id);
                    let mut row = v_flex()
                        .gap_2()
                        .p_3()
                        .rounded(px(10.))
                        .when(!in_settings, |row| row.bg(rgb(0x333333)))
                        .when(in_settings, |row| {
                            row.p_4().border_1().border_color(rgb(0x2b2f35))
                        })
                        .child(
                            Button::new(format!("account-choice-{index}"))
                                .accessibility_label(identity.to_owned())
                                .small()
                                .ghost()
                                .child(account_identity(Some(account.provider), identity).flex_1())
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    s.account_operation(Intent::SelectAccountForDraft(
                                        op::SelectAccountForDraft {
                                            provider,
                                            id: id.clone(),
                                            thread_id: s.draft_key().clone(),
                                        },
                                    ));
                                    cx.notify();
                                }))
                                .debug_selector(move || format!("account-choice-{index}"))
                                .when(accounts.is_selected(account), |button| {
                                    button.child(Icon::new(IconName::Check).size(px(14.)))
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
                                self.snapshot
                                    .account_weekly_usage(account.provider, account.id.clone()),
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
                                                        provider,
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
                        let logout = self
                            .button(
                                format!("account-logout-{index}"),
                                "サインアウト",
                                cx,
                                move |s, _, _| s.account_sign_out = Some(logout_id.clone()),
                            )
                            .debug_selector(move || format!("account-logout-{index}"))
                            .disabled(disabled);
                        row = if in_settings {
                            row.child(h_flex().justify_end().child(logout))
                        } else {
                            row.child(logout)
                        };
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
                                move |s, _, _| {
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
                                s.refresh_accounts_and_models();
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
                    .model_error_messages(Some(provider))
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
    pub(super) fn account_provider(&self) -> ProviderKind {
        self.model_provider.unwrap_or_else(|| {
            self.snapshot
                .model_provider_for_draft(self.draft_key().clone())
        })
    }

    pub(in crate::app) fn account_operation(&mut self, intent: Intent) {
        if self.account_busy || self.session.is_none() || !self.snapshot.connected {
            return;
        }
        if let Intent::StartAccountLogin(start) = &intent {
            self.model_provider = Some(start.provider);
            self.account_login_draft = Some(self.draft_key().clone());
        }
        self.account_busy = true;
        self.perform(intent, OperationCompletion::Account);
    }

    pub(super) fn account_login_controls(
        &self,
        title: Option<&str>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(login) = &self.snapshot.account.login else {
            return div().into_any_element();
        };
        let disabled = !self.snapshot.connected || self.account_busy || self.busy > 0;
        let url = login.verification_url.clone();
        let cancel_id = login.login_id.clone();
        let provider = login.provider;
        let cancel = self
            .button(
                "account-cancel-login",
                "キャンセル",
                cx,
                move |s, _, _| {
                    s.account_operation(Intent::CancelAccountLogin(op::CancelAccountLogin {
                        provider,
                        id: cancel_id.clone(),
                    }));
                },
            )
            .debug_selector(|| "account-cancel-login".into())
            .small()
            .ghost()
            .disabled(disabled);
        let mut form = v_flex()
            .gap_3()
            .text_sm()
            .child(if login.requires_code_submission {
                "ブラウザでログインし、認証コードを貼り付けてください。"
            } else {
                "ブラウザでログインし、次のコードを入力してください。"
            })
            .child(
                self.button(
                    "account-open-login",
                    "ブラウザでログイン",
                    cx,
                    move |_, _, cx| {
                        cx.open_url(&url);
                    },
                )
                .debug_selector(|| "account-open-login".into())
                .small(),
            );
        if login.requires_code_submission {
            let submit_id = login.login_id.clone();
            form = form.child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.account_code)),
                    )
                    .child(
                        self.button("account-submit-code", "接続", cx, move |s, window, cx| {
                            let code = s.account_code.read(cx).value().to_string();
                            if code.trim().is_empty() {
                                return;
                            }
                            s.account_operation(Intent::SubmitAccountLogin(
                                op::SubmitAccountLogin {
                                    provider,
                                    id: submit_id.clone(),
                                    code,
                                },
                            ));
                            s.account_code
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        })
                        .debug_selector(|| "account-submit-code".into())
                        .disabled(disabled || self.account_code.read(cx).value().trim().is_empty()),
                    ),
            );
        } else {
            let code = login.user_code.clone();
            form = form.child(
                h_flex()
                    .gap_3()
                    .child(div().text_xl().child(login.user_code.clone()))
                    .child(
                        self.button(
                            "account-copy-code",
                            "コードをコピー",
                            cx,
                            move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code.clone()));
                            },
                        )
                        .small()
                        .ghost(),
                    ),
            );
        }
        form = form
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(0x949494))
                    .child("ログインが完了すると、自動で接続されます。"),
            )
            .when(!self.account_polling && !self.account_busy, |form| {
                form.child(
                    self.button(
                        "account-retry-login",
                        "接続状態を再確認",
                        cx,
                        |s, _, _| {
                            s.account_polling = true;
                        },
                    )
                    .small()
                    .ghost()
                    .disabled(disabled),
                )
            });
        match title {
            Some(title) => v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .gap_3()
                        .py_1()
                        .child(div().flex_1().child(title.to_owned()))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(0x949494))
                                .child("ログイン中"),
                        )
                        .child(cancel),
                )
                .child(
                    form.p_3()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(rgb(0x303030))
                        .bg(rgb(0x191919)),
                )
                .into_any_element(),
            None => form.child(cancel).into_any_element(),
        }
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
                "アカウント"
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
                .model_provider_for_draft(self.draft_key().clone())
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
                .border_color(rgb(0x2a2c2e))
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
                                                .model_for_provider(s.draft_key().clone(), provider)
                                            {
                                                s.dispatch(Intent::SelectModel {
                                                    thread_id: s.draft_key().clone(),
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
                .accessibility_label("アカウントと週間残量")
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
                                        .child("アカウント"),
                                )
                                .child(
                                    account_identity(
                                        account.map(|a| a.provider),
                                        account.map_or("未選択", |a| {
                                            a.email.as_deref().unwrap_or(&a.id)
                                        }),
                                    )
                                    .flex_1()
                                    .justify_end(),
                                )
                                .child(Icon::new(IconName::ChevronRight).size(px(16.))),
                        )
                        .child(weekly_usage_view(
                            account
                                .map(|a| {
                                    self.snapshot.account_weekly_usage(a.provider, a.id.clone())
                                })
                                .unwrap_or_default(),
                        )),
                ),
        );
        body = body
            .child(div().h(px(1.)).w_full().bg(rgb(0x2a2c2e)))
            .child(
                Input::new(&self.model_search)
                    .prefix(IconName::Search)
                    .border_1()
                    .border_color(rgb(0x2a2c2e))
                    .large()
                    .aria_label("モデルを検索"),
            );
        let models = self.snapshot.models_matching(
            Some(provider),
            self.model_search.read(cx).value().to_string(),
        );
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
                            thread_id: s.draft_key().clone(),
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
                    .when(selected, |button| button.bg(rgb(0x262b32)))
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
                    .model_error_messages(Some(provider))
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
            .child(
                v_flex()
                    .border_t_1()
                    .border_color(rgb(0x2a2c2e))
                    .child(self.effort_control("model-picker-effort", true, cx))
                    .child(self.fast_control("model-picker-speed", true, cx)),
            )
            .into_any_element()
    }

    pub(super) fn fast_control(
        &self,
        id: &'static str,
        expanded: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let controls = self.snapshot.model_quick_controls(self.draft_key().clone());
        let Some(next) = controls.toggle_fast_to else {
            return div().into_any_element();
        };
        let label = if controls.fast {
            "Fast：オン"
        } else {
            "Fast：オフ"
        };
        self.icon_button(
            id,
            Icon::default().path("bex/bolt.svg"),
            label,
            cx,
            move |s, _, _| {
                s.dispatch(Intent::SelectServiceTier {
                    thread_id: s.draft_key().clone(),
                    service_tier: next.clone(),
                });
            },
        )
        .debug_selector(move || id.into())
        .when(!expanded, |button| button.w(px(40.)))
        .when(expanded, |button| {
            button.w_full().label(if controls.fast {
                "速度：高速"
            } else {
                "速度：通常"
            })
        })
        .h(px(44.))
        .text_color(if controls.fast {
            rgb(0x78adff)
        } else {
            rgb(0x999999)
        })
        .disabled(
            !self.snapshot.connected
                || self.account_busy
                || self.busy > 0
                || self.snapshot.account.login.is_some(),
        )
        .into_any_element()
    }

    pub(super) fn effort_control(
        &self,
        id: &'static str,
        expanded: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let controls = self.snapshot.model_quick_controls(self.draft_key().clone());
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
        Button::new(id)
            .child(bars)
            .ghost()
            .when(!expanded, |button| button.w(px(44.)))
            .when(expanded, |button| {
                button
                    .w_full()
                    .label(format!("思考の深さ：{}", controls.effort))
                    .child(Icon::new(IconName::ChevronDown).size(px(14.)))
            })
            .h(px(44.))
            .debug_selector(move || id.into())
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
                                        thread_id: s.draft_key().clone(),
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
                            .child(view.fast_control("model-fast", false, cx))
                            .child(view.model_menu(cx))
                            .child(view.effort_control("model-effort", false, cx)),
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
                    {"provider":"claude","id":"claude:second"}],"selected":{"codex":"first","claude":"claude:second"}
                })).unwrap()));
                snapshot.models = Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":"gpt","model":{"provider": "codex", "id": "gpt"},"displayName":"GPT","defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],"serviceTiers":[{"id":"priority"}]},
                    {"id":"claude:sonnet","model":{"provider": "claude", "id": "sonnet"},"displayName":"Sonnet","defaultReasoningEffort":"","supportedReasoningEfforts":[]}
                ])).unwrap());
                Arc::make_mut(&mut snapshot.drafts).insert(key, Arc::new(Draft { model: Some(agent_protocol::models::ModelRef { provider: agent_protocol::session::ProviderKind::Codex, id: "gpt".into() }), ..Default::default() }));
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
        let model = window.debug_bounds("model-choice-gpt").unwrap();
        let effort = window.debug_bounds("model-picker-effort").unwrap();
        let speed = window.debug_bounds("model-picker-speed").unwrap();
        assert!(model.bottom() <= effort.top());
        assert!(effort.bottom() <= speed.top());
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
    struct DefaultsView(Entity<Desktop>);
    impl Render for DefaultsView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |view, cx| {
                h_flex()
                    .size_full()
                    .child(view.settings_sidebar(cx))
                    .child(view.settings(cx))
            })
        }
    }

    #[gpui::test]
    fn default_model_settings_keep_speed_beside_effort_and_worktrees_separate(
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
                remote: Some(RemoteHost { id: "fixture".into(), name: "fixture".into(), ticket: "invalid-fixture-ticket".into() }),
                cwd: "/fixture".into(),
            }, window, cx));
            desktop.update(cx, |view, _| {
                view.tab = super::Tab::Settings;
                view.settings_page = super::SettingsPage::Models;
                let snapshot = Arc::make_mut(&mut view.snapshot);
                snapshot.models = Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":"gpt","model":{"provider":"codex","id":"gpt"},"displayName":"GPT-6-Astra",
                     "isDefault":true,"defaultReasoningEffort":"medium",
                     "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
                     "serviceTiers":[{"id":"priority"}]}
                ])).unwrap());
            });
            cx.observe(&desktop, |_, _, cx| cx.notify()).detach();
            DefaultsView(desktop)
        });
        for width in [1000., 1280.] {
            window.simulate_resize(gpui_kit::size(px(width), px(720.)));
            window.run_until_parked();
            let model = window.debug_bounds("default-model").unwrap();
            let effort = window.debug_bounds("default-model-effort").unwrap();
            let speed = window.debug_bounds("default-model-speed").unwrap();
            assert!(model.right() <= effort.left());
            assert!(effort.right() <= speed.left());
            assert_eq!(model.top(), speed.top());
            assert!(speed.right() < px(width));
            assert!(window.debug_bounds("worktree-create").is_none());
        }
        window.update(|_, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.settings_page = super::SettingsPage::Worktrees;
                cx.notify();
            });
        });
        window.run_until_parked();
        assert!(window.debug_bounds("default-model").is_none());
    }
}

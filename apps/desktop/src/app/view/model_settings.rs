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
                    .dropdown_caret(true)
                    .accessibility_label("モデルとアカウント")
                    .tooltip("モデルとアカウント")
                    .large()
                    .max_w(px(220.))
                    .h(px(44.))
                    .ghost(),
            )
            .on_open_change(move |open, _, cx| {
                if *open {
                    let _ = opening.update(cx, |view, _| {
                        view.model_provider = None;
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
                            .id("model-account-scroll")
                            .w(px(400.))
                            .max_h(px(560.))
                            .overflow_y_scroll()
                            .p_2()
                            .gap_3()
                            .child(div().text_lg().child("モデルとアカウント"))
                            .child(s.model_controls(cx))
                            .into_any_element()
                    })
                    .unwrap_or_else(|_| div().into_any_element())
            })
            .into_any_element()
    }
    fn account_model_controls(
        &self,
        account_id: String,
        disabled: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().downgrade();
        let model = self.selected_model().filter(|_| {
            self.snapshot
                .account_is_active_for_draft(account_id.clone(), self.draft_key().into())
        });
        let effort = self
            .draft()
            .effort
            .as_deref()
            .or_else(|| model.map(|model| model.default_reasoning_effort.as_str()))
            .unwrap_or_default();
        let model_label = model
            .map_or("モデルを選択", |model| model.display_name.as_str())
            .to_owned();
        let models = Button::new("model-choice")
            .debug_selector(|| "model-choice".into())
            .label(model_label)
            .dropdown_caret(true)
            .disabled(disabled)
            .small()
            .ghost()
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade() {
                    let view = owner.read(cx);
                    for model in view.snapshot.account_models(account_id.clone()) {
                        let value = model.model.clone();
                        let entity = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(model.display_name.clone())
                                .checked(
                                    view.selected_model()
                                        .is_some_and(|model| model.model == value),
                                )
                                .on_click(move |_, _, cx| {
                                    let _ = entity.update(cx, |view, cx| {
                                        view.dispatch(Intent::SelectModel {
                                            thread_id: view.draft_key().into(),
                                            model: value.clone(),
                                        });
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                menu
            });
        let entity = cx.entity().downgrade();
        let tier = self
            .draft()
            .service_tier
            .as_deref()
            .or_else(|| model.and_then(|model| model.default_service_tier.as_deref()))
            .unwrap_or("default");
        let speed_label = model
            .and_then(|model| model.service_tiers.as_deref())
            .and_then(|tiers| tiers.iter().find(|value| value.id == tier))
            .map(|tier| tier.name.as_deref().unwrap_or(&tier.id))
            .unwrap_or("標準");
        let speed = Button::new("model-speed")
            .debug_selector(|| "model-speed".into())
            .label(speed_label.to_owned())
            .accessibility_label("速度")
            .dropdown_caret(true)
            .disabled(disabled)
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade() {
                    let view = owner.read(cx);
                    let current = view.selected_model();
                    let selected = view
                        .draft()
                        .service_tier
                        .as_deref()
                        .or_else(|| current.and_then(|model| model.default_service_tier.as_deref()))
                        .unwrap_or("default");
                    let tiers = std::iter::once(("default", "標準")).chain(
                        current
                            .and_then(|model| model.service_tiers.as_deref())
                            .unwrap_or_default()
                            .iter()
                            .filter(|tier| tier.id != "default")
                            .map(|tier| {
                                (tier.id.as_str(), tier.name.as_deref().unwrap_or(&tier.id))
                            }),
                    );
                    for (id, label) in tiers {
                        let value = id.to_owned();
                        let entity = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label.to_owned())
                                .checked(selected == id)
                                .on_click(move |_, _, cx| {
                                    let _ = entity.update(cx, |view, cx| {
                                        view.dispatch(Intent::SelectServiceTier {
                                            thread_id: view.draft_key().into(),
                                            service_tier: value.clone(),
                                        });
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                menu
            });
        let mut body = v_flex()
            .gap_3()
            .p_3()
            .rounded(px(10.))
            .bg(rgb(0x333333))
            .child(h_flex().justify_between().child("モデル").child(models));
        if let Some(model) = model {
            if !model.supported_reasoning_efforts.is_empty() {
                let mut choices = h_flex().gap_1().flex_wrap();
                for choice in &model.supported_reasoning_efforts {
                    let value = choice.reasoning_effort.clone();
                    let selector = format!("model-effort-{value}");
                    choices = choices.child(
                        self.button(
                            format!("model-effort-{value}"),
                            value.clone(),
                            cx,
                            move |s, _, _| {
                                s.dispatch(Intent::SelectEffort {
                                    thread_id: s.draft_key().into(),
                                    effort: value.clone(),
                                });
                            },
                        )
                        .small()
                        .debug_selector(move || selector.clone())
                        .selected(effort == choice.reasoning_effort)
                        .disabled(disabled),
                    );
                }
                body = body
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(0xa3a3a3))
                            .child("推論の強度"),
                    )
                    .child(choices);
            }
            if model
                .service_tiers
                .as_ref()
                .is_some_and(|tiers| !tiers.is_empty())
            {
                body = body.child(h_flex().justify_between().child("速度").child(speed));
            }
        }
        body.into_any_element()
    }

    pub(super) fn model_controls(&self, cx: &Context<Self>) -> AnyElement {
        let accounts = self.snapshot.account.accounts.as_ref();
        let provider = self.model_provider.unwrap_or_else(|| {
            accounts
                .and_then(|accounts| {
                    accounts
                        .accounts
                        .iter()
                        .find(|account| {
                            self.snapshot.account_is_active_for_draft(
                                account.id.clone(),
                                self.draft_key().into(),
                            )
                        })
                        .or_else(|| {
                            accounts
                                .accounts
                                .iter()
                                .find(|account| accounts.is_selected(account))
                        })
                })
                .map_or(ProviderKind::Codex, |account| account.provider)
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
                        if let Some(account) =
                            s.snapshot.account.accounts.as_ref().and_then(|accounts| {
                                accounts.accounts.iter().find(|account| {
                                    account.provider == value && accounts.is_selected(account)
                                })
                            })
                        {
                            s.account_operation(Intent::SelectAccountForDraft(
                                op::SelectAccountForDraft {
                                    id: account.id.clone(),
                                    thread_id: s.draft_key().into(),
                                },
                            ));
                        }
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
            .child(services);
        if self.snapshot.account.login.is_some() {
            body = body.child(self.account_login_controls(cx));
        } else {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa3a3a3))
                    .child("モデル設定"),
            );
            if let Some(account) = accounts.and_then(|accounts| {
                accounts
                    .accounts
                    .iter()
                    .find(|account| account.provider == provider && accounts.is_selected(account))
            }) {
                body = body.child(self.account_model_controls(account.id.clone(), disabled, cx));
            } else {
                body = body.child(div().text_sm().child("サインインするとモデルを選べます。"));
            }
            body = body.child(
                div()
                    .text_sm()
                    .text_color(rgb(0xa3a3a3))
                    .child("アカウント"),
            );
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
                            .ghost()
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
                        .child(account_usage_view(account.usage.as_ref()));
                    if self.account_sign_out.as_deref() == Some(account.id.as_str()) {
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
                    } else {
                        row = row.child(
                            self.button(
                                format!("account-logout-{index}"),
                                "サインアウト",
                                cx,
                                move |s, _, _| s.account_sign_out = Some(logout_id.clone()),
                            )
                            .debug_selector(move || format!("account-logout-{index}"))
                            .small()
                            .ghost()
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
            body = body
                .child(
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
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0xa3a3a3))
                        .child("アカウントの切替は、同じ接続先を使う端末にも反映されます。"),
                );
        }
        body = body
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
    let mut body = v_flex().gap_2().text_xs();
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
            v_flex()
                .gap_1()
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .child(window.label.clone())
                        .child(format!("残り {}%", window.remaining_percent)),
                )
                .child(
                    div()
                        .h(px(4.))
                        .w_full()
                        .rounded(px(4.))
                        .bg(rgb(0x474747))
                        .child(
                            div()
                                .h_full()
                                .w(relative(window.remaining_percent as f32 / 100.))
                                .rounded(px(4.))
                                .bg(if window.remaining_percent <= 20 {
                                    rgb(0xe9b56f)
                                } else {
                                    rgb(0x8acfac)
                                }),
                        ),
                )
                .when_some(
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

#[cfg(test)]
mod tests {
    use super::{Desktop, Draft, Mode, ProviderKind, RemoteHost};
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, IntoElement, Modifiers, ParentElement, Render, Styled,
        TestAppContext, Window, div, px,
    };
    use std::sync::Arc;

    struct SettingsView(Entity<Desktop>);
    impl Render for SettingsView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .w(px(400.))
                .child(self.0.update(cx, |view, cx| view.model_controls(cx)))
        }
    }

    #[gpui::test]
    fn model_settings_filter_services_keep_controls_first_and_confirm_sign_out(
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
                let key = view.draft_key().to_owned();
                let snapshot = Arc::make_mut(&mut view.snapshot);
                snapshot.connected = true;
                Arc::make_mut(&mut snapshot.account).accounts = Some(Arc::new(serde_json::from_value(serde_json::json!({
                    "accounts": [{"provider":"codex","id":"first","email":"first@example.invalid"},
                        {"provider":"claude","id":"claude:second","email":"second@example.invalid"}],
                    "selectedId":"first", "selectedClaudeId":"claude:second"
                })).unwrap()));
                snapshot.models = Arc::new(serde_json::from_value(serde_json::json!([
                    {"id":"gpt","model":"gpt","displayName":"GPT","defaultReasoningEffort":"medium",
                    "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
                    "serviceTiers":[{"id":"fast"}]},
                    {"id":"claude:sonnet","model":"claude:sonnet","displayName":"Sonnet","defaultReasoningEffort":"","supportedReasoningEfforts":[]}
                ])).unwrap());
                Arc::make_mut(&mut snapshot.drafts).insert(key, Arc::new(Draft { model: Some("gpt".into()), ..Default::default() }));
            });
            cx.observe(&desktop, |_, _, cx| cx.notify()).detach();
            SettingsView(desktop)
        });
        window.run_until_parked();
        let model = window.debug_bounds("model-choice").unwrap();
        let effort = window.debug_bounds("model-effort-high").unwrap();
        let speed = window.debug_bounds("model-speed").unwrap();
        let account = window.debug_bounds("account-choice-0").unwrap();
        assert!(
            model.top() < effort.top() && effort.top() < speed.top() && speed.top() < account.top()
        );
        assert!(window.debug_bounds("account-choice-1").is_none());
        let logout = window.debug_bounds("account-logout-0").unwrap();
        window.simulate_click(logout.center(), Modifiers::default());
        window.update(|_, cx| {
            assert_eq!(
                view.read(cx).0.read(cx).account_sign_out.as_deref(),
                Some("first")
            )
        });
        window.run_until_parked();
        let cancel = window.debug_bounds("account-logout-cancel-0").unwrap();
        window.simulate_click(cancel.center(), Modifiers::default());
        window.update(|_, cx| assert!(view.read(cx).0.read(cx).account_sign_out.is_none()));
        let claude = window.debug_bounds("model-provider-Claude").unwrap();
        window.simulate_click(claude.center(), Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("account-choice-0").is_none());
        assert!(window.debug_bounds("account-choice-1").is_some());
        assert!(
            window.debug_bounds("model-effort-high").is_none(),
            "another provider must not show the active model's controls"
        );
        assert!(window.debug_bounds("model-speed").is_none());
        window.update(|_, cx| {
            view.read(cx).0.clone().update(cx, |view, cx| {
                view.account_busy = true;
                cx.notify();
            })
        });
        window.run_until_parked();
        let codex = window.debug_bounds("model-provider-Codex").unwrap();
        window.simulate_click(codex.center(), Modifiers::default());
        window.update(|_, cx| {
            assert_eq!(
                view.read(cx).0.read(cx).model_provider,
                Some(ProviderKind::Claude)
            )
        });
    }
}

use crate::l10n::{localized, tr, tr_format};
use crate::ui::{self, Enabled};
use crate::{Connection, native};
use agent_core::{
    presentation::model_settings::OPENROUTER_MODEL_QUERY,
    state::{Intent, operations as op},
};
use agent_protocol::{dsh::KeyProvider, session::ProviderKind};
use nsh_core::{element::input, prelude::*};

pub(crate) fn icon(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Codex => "openai",
        ProviderKind::Claude => "claude",
        ProviderKind::DeepSeek => "deepseek",
    }
}

pub(crate) fn screen(provider: ProviderKind) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let publication = connection.publication;
    let agent = computed(move || {
        publication.with(|p| {
            p.settings
                .connection_setup()
                .agents
                .into_iter()
                .find(|a| a.provider == provider)
        })
    });
    let host = stored(connection.host.get_untracked());
    let pending = computed(move || connection.state.with(|state| state.account_pending));
    let perform = move |intent| {
        connection
            .application
            .with(|app| app.account_request(host.get(), intent))
    };
    let login = computed(move || connection.settings.with(|p| p.account_login()));
    let busy = computed(move || {
        pending.get()
            || login.get().is_some()
            || publication.with(|p| {
                !p.settings.connected() || p.snapshot.operation_running(op::OperationKey::Accounts)
            })
    });
    let ready = computed(move || connection.settings.with(|p| p.connected()));
    effect(move || {
        if ready.get() {
            connection.dispatch(Intent::ListAccounts(op::ListAccounts {}));
            if provider == ProviderKind::DeepSeek {
                connection.dispatch(Intent::ReadDshCredentials(op::ReadDshCredentials {}));
            }
            if provider == ProviderKind::Codex {
                connection.dispatch(Intent::ManageCodexCli(op::ManageCodexCli { update: false }));
            }
        }
    });
    view()
        .gap_4()
        .child(
            text(move || {
                connection
                    .settings
                    .with(|p| p.host_name().unwrap_or_default())
            })
            .text_size(px(13.0))
            .text_color(ui::colors::muted()),
        )
        .child(
            text(move || {
                publication.with(|p| {
                    p.settings
                        .account_login_error(provider)
                        .map(|error| error.to_string())
                        .unwrap_or_default()
                })
            })
            .selectable(true),
        )
        .child(when(
            move || login.get().is_some(),
            move || login_panel(login, pending, perform),
        ))
        .child(when(
            move || login.get().is_none(),
            move || {
                view()
                    .gap_4()
                    .child(
                        text(move || {
                            agent.with(|agent| {
                                agent
                                    .as_ref()
                                    .map(|a| {
                                        a.error
                                            .as_ref()
                                            .map(ToString::to_string)
                                            .unwrap_or_else(|| a.label.clone())
                                    })
                                    .unwrap_or_default()
                            })
                        })
                        .selectable(true),
                    )
                    .child(if provider == ProviderKind::DeepSeek {
                        credentials(busy, perform).into_element()
                    } else {
                        account_list(provider, busy, perform).into_element()
                    })
                    .child(runtime(provider, busy, perform))
            },
        ))
}

fn account_list(
    provider: ProviderKind,
    busy: Memo<bool>,
    perform: impl Fn(Intent) + Copy + 'static,
) -> impl IntoElement {
    let publication = use_context::<Connection>().unwrap().publication;
    view()
        .gap_3()
        .child(
            view()
                .flex_row()
                .items_center()
                .gap_2()
                .child(heading(tr("アカウント")).flex_1())
                .child(
                    ui::text_button(tr("アカウントを再読み込み"), move || {
                        perform(Intent::ListAccounts(op::ListAccounts {}))
                    })
                    .enabled(move || !busy.get()),
                )
                .child(
                    ui::text_button(tr("アカウントを追加"), move || {
                        perform(Intent::StartAccountLogin(op::StartAccountLogin {
                            provider,
                            thread_id: None,
                        }))
                    })
                    .enabled(move || !busy.get()),
                ),
        )
        .child(each(
            move || {
                publication.with(|p| {
                    p.settings
                        .accounts()
                        .map(|accounts| {
                            accounts
                                .accounts
                                .into_iter()
                                .filter(|a| a.provider == provider)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
            },
            |account: &agent_protocol::operations::Account| account.id.clone(),
            move |account, _| account_row(account, busy, perform),
        ))
}

fn heading(title: &'static str) -> Text {
    text(title)
        .text_size(px(16.0))
        .font_weight(FontWeight::SEMIBOLD)
}

fn account_row(
    account: ReadSignal<agent_protocol::operations::Account>,
    busy: Memo<bool>,
    perform: impl Fn(Intent) + Copy + 'static,
) -> impl IntoElement {
    let publication = use_context::<Connection>().unwrap().publication;
    let selected = move || {
        publication
            .with(|p| account.with(|a| p.settings.account_is_selected(a.provider, a.id.clone())))
    };
    let name = move || account.with(|a| a.email.as_ref().unwrap_or(&a.id).clone());
    let actions = view().flex_row().gap_2()
        .child(ui::labelled_text_button(move || SharedString::from(if selected() { tr("選択中") } else { tr("このアカウントを使う") }),
            move || SharedString::from(if selected() { tr_format("{0}を選択中", [&name()]) } else { tr_format("{0}を使う", [&name()]) }), move || {
            let (provider, id) = account.with_untracked(|a| (a.provider, a.id.clone()));
            perform(Intent::SelectAccount(op::SelectAccount { provider, id }));
        }).enabled(move || !busy.get() && !selected()))
        .child(ui::labelled_text_button(tr("サインアウト"), move || SharedString::from(tr_format("{0}からサインアウト", [&name()])), move || {
            let (provider, id) = account.with_untracked(|a| (a.provider, a.id.clone()));
            spawn(async move {
                if nsh_base::alert_dialog().title(tr("サインアウトしますか？"))
                    .description(tr("この環境に保存されたアカウントからサインアウトします。再び使うにはサインインが必要です。"))
                    .cancel("cancel", tr("キャンセル")).destructive("logout", tr("サインアウト"))
                    .show().await.is_ok_and(|answer| answer.action.as_deref() == Some("logout")) {
                    perform(Intent::LogoutAccount(op::LogoutAccount { provider, id }));
                }
            });
        }).enabled(move || !busy.get()));
    ui::card(vec![
        view()
            .px_5()
            .py_4()
            .gap_3()
            .child(
                view()
                    .flex_row()
                    .items_center()
                    .gap_4()
                    .child(
                        view()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                text(name)
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::MEDIUM),
                            )
                            .child(
                                text(move || {
                                    account.with(|a| a.plan_type.clone().unwrap_or_default())
                                })
                                .text_size(px(13.0))
                                .text_color(ui::colors::muted()),
                            ),
                    )
                    .child(actions),
            )
            .child(when(selected, move || usage(account))),
    ])
}

fn usage(account: ReadSignal<agent_protocol::operations::Account>) -> impl IntoElement {
    view()
        .gap_2()
        .child(when(
            move || account.with(|a| a.usage.is_none()),
            || text(tr("使用量を取得中…")),
        ))
        .child(
            text(move || {
                account.with(|a| {
                    a.usage
                        .as_ref()
                        .and_then(|u| u.error.clone())
                        .map(localized)
                        .unwrap_or_default()
                })
            })
            .selectable(true),
        )
        .child(each(
            move || {
                account.with(|a| {
                    a.usage
                        .as_ref()
                        .map(|u| u.windows.clone())
                        .unwrap_or_default()
                })
            },
            |window: &agent_protocol::operations::UsageWindow| {
                format!("{:?}{:?}", window.period, window.name)
            },
            move |window, _| {
                view()
                    .gap_2()
                    .child(text(move || {
                        window.with(|w| {
                            tr_format(
                                "{0} 残り {1}%",
                                [
                                    &agent_core::presentation::model_settings::usage_window_label(
                                        crate::l10n::language(),
                                        w.period,
                                        w.name.clone(),
                                    ),
                                    &w.remaining_percent,
                                ],
                            )
                        })
                    }))
                    .child(ui::progress(move || {
                        window.with(|w| f64::from(w.remaining_percent))
                    }))
                    .child(
                        text(move || {
                            window.with(|w| {
                                w.resets_at
                                    .map(|at| tr_format("リセット: {0}", [&native::date_time(at)]))
                                    .unwrap_or_default()
                            })
                        })
                        .text_size(px(12.0))
                        .text_color(ui::colors::muted()),
                    )
            },
        ))
        .child(
            text(move || {
                account.with(|a| {
                    a.usage
                        .as_ref()
                        .filter(|u| u.error.is_none())
                        .map(|u| tr_format("{0} 更新", [&native::date_time(u.fetched_at)]))
                        .unwrap_or_default()
                })
            })
            .text_size(px(12.0))
            .text_color(ui::colors::muted()),
        )
}

fn login_panel(
    login: Memo<Option<agent_protocol::operations::AccountLogin>>,
    pending: Memo<bool>,
    perform: impl Fn(Intent) + Copy + 'static,
) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let code = signal(String::new());
    // Core confirms the login itself and stops when a read fails.
    let login_error = computed(move || {
        login.with(|login| {
            login.as_ref().and_then(|login| {
                connection
                    .settings
                    .with(|p| p.account_login_error(login.provider))
            })
        })
    });
    let requires_code =
        move || login.with(|l| l.as_ref().is_some_and(|l| l.requires_code_submission));
    let current = move || login.get_untracked();
    view()
        .items_start()
        .gap_3()
        .child(text(tr("1. ブラウザでサインイン")).text_size(px(18.0)))
        .child(when(
            move || !requires_code(),
            move || {
                view()
                    .gap_2()
                    .child(text(tr("ログインページで次のコードを入力してください。")))
                    .child(
                        text(move || {
                            login.with(|l| {
                                l.as_ref().map(|l| l.user_code.clone()).unwrap_or_default()
                            })
                        })
                        .selectable(true),
                    )
                    .child(ui::text_button(tr("コピー"), move || {
                        if let Some(login) = current() {
                            set_clipboard(login.user_code);
                        }
                    }))
            },
        ))
        .child(ui::text_button(
            tr("ログインページを開く"),
            move || {
                if let Some(login) = current()
                    && login.verification_url.starts_with("https://")
                {
                    native::open_link(login.verification_url);
                }
            },
        ))
        .child(when(requires_code, move || {
            view()
                .gap_2()
                .child(text(tr("2. 認証コードを貼り付け")))
                .child(
                    input()
                        .masked(true)
                        .auto_correct(false)
                        .auto_capitalize(AutoCapitalize::None)
                        .placeholder(tr("認証コード"))
                        .value(move || code.get())
                        .on_change(move |value: &str| code.set(value.into()))
                        .text_color(move || nsh_base::theme().foreground)
                        .w_full()
                        .h(px(36.0))
                        .px_3()
                        .rounded(px(ui::RADIUS))
                        .border_1()
                        .border_color(ui::colors::border()),
                )
                .child(
                    ui::text_button(tr("サインインを完了"), move || {
                        if let Some(login) = current() {
                            let value = code.get_untracked();
                            code.set(String::new());
                            perform(Intent::SubmitAccountLogin(op::SubmitAccountLogin {
                                provider: login.provider,
                                id: login.login_id,
                                code: value,
                            }));
                        }
                    })
                    .enabled(move || !pending.get() && !code.with(|code| code.trim().is_empty())),
                )
        }))
        .child(text(move || {
            if pending.get() {
                tr("認証を確認中…")
            } else {
                tr("ブラウザでの認証を待っています…")
            }
        }))
        .child(when(
            move || login_error.with(Option::is_some),
            move || {
                view()
                    .gap_2()
                    .child(text(move || {
                        login_error.with(|error| {
                            error.as_ref().map(ToString::to_string).unwrap_or_default()
                        })
                    }))
                    .child(ui::text_button(tr("認証状態を再確認"), move || {
                        if let Some(login) = current() {
                            perform(Intent::ReadAccountLogin(op::ReadAccountLogin {
                                provider: login.provider,
                                id: login.login_id,
                            }));
                        }
                    }))
            },
        ))
        .child(
            ui::text_button(tr("サインインを中止"), move || {
                if let Some(login) = current() {
                    code.set(String::new());
                    perform(Intent::CancelAccountLogin(op::CancelAccountLogin {
                        provider: login.provider,
                        id: login.login_id,
                    }));
                }
            })
            .enabled(move || !pending.get()),
        )
}

fn credentials(busy: Memo<bool>, perform: impl Fn(Intent) + Copy + 'static) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let selected = signal(KeyProvider::DeepSeek);
    let key = signal(String::new());
    let control = computed(move || {
        connection.publication.with(|p| {
            p.settings
                .credential_controls()
                .into_iter()
                .find(|c| c.provider == selected.get())
        })
    });
    let enabled = move || !busy.get() && !connection.settings.with(|p| p.credential_busy());
    view().items_start().gap_3()
        .child(text(tr("APIキー")).text_size(px(18.0)))
        .child(ui::choice_menu("key", tr("接続先"), move || selected.get().label().into(), || native::items(KeyProvider::ALL.into_iter().map(|provider| (provider.reference(), provider.label()))), move |id: &str| {
            if let Some(provider) = KeyProvider::ALL.into_iter().find(|p| p.reference() == id) { selected.set(provider); key.set(String::new()); }
        }).enabled(enabled))
        .child(text(move || control.with(|c| c.as_ref().map(|c| c.status.clone()).unwrap_or_default())))
        .child(ui::text_button(tr("キーの発行ページ"), move || { native::open_link(selected.get_untracked().key_url()); }))
        .child(input().masked(true).auto_correct(false).auto_capitalize(AutoCapitalize::None)
            .placeholder(tr("APIキー")).value(move || key.get()).on_change(move |value: &str| key.set(value.into()))
            .text_color(move || nsh_base::theme().foreground).w_full().h(px(36.0)).px_3().rounded(px(ui::RADIUS)).border_1().border_color(ui::colors::border()))
        .child(ui::text_button(tr("保存"), move || {
            let value = key.get_untracked(); key.set(String::new());
            perform(Intent::UpdateDshCredential(op::UpdateDshCredential { provider: selected.get_untracked(), key: Some(value) }));
        }).enabled(move || enabled() && !key.with(|key| key.trim().is_empty())))
        .child(when(move || control.with(|c| c.as_ref().is_some_and(|c| c.can_remove)), move || {
            ui::text_button(tr("保存済みキーを削除"), move || {
                let provider = selected.get_untracked();
                spawn(async move {
                    if nsh_base::alert_dialog().title(tr("保存済みキーを削除しますか？"))
                        .description(tr("このPCにBexが保存した選択中のキーを削除します。ホストの外部設定は変更されません。"))
                        .cancel("cancel", tr("キャンセル")).destructive("remove", tr("削除"))
                        .show().await.is_ok_and(|a| a.action.as_deref() == Some("remove")) {
                        perform(Intent::UpdateDshCredential(op::UpdateDshCredential { provider, key: None }));
                    }
                });
            }).enabled(enabled)
        }))
        // As the desktop client's, a saved OpenRouter key leads to OpenRouter's
        // models in the composer's model form.
        .child(when(move || selected.get() == KeyProvider::OpenRouter && control.with(|c| c.as_ref().is_some_and(|c| c.can_remove)), move || {
            ui::text_button(tr("OpenRouterのモデル一覧を開く"), move || {
                connection.application.with(|app| app.navigate(crate::Navigation::Settings(false)));
                crate::model_controls::open(connection, Some(ProviderKind::DeepSeek), OPENROUTER_MODEL_QUERY);
            }).enabled(enabled)
        }))
        .child(ui::text_button(tr("設定状態を取得"), move || perform(Intent::ReadDshCredentials(op::ReadDshCredentials {})))
            .enabled(enabled))
        .child(text(move || connection.settings.with(|p| p.credential_error().map(|error| error.to_string()).unwrap_or_default())).selectable(true))
        .child(text(tr("APIキーは選択したPCに保存されます。保存済みのキーは表示されません。")))
}

fn runtime(
    provider: ProviderKind,
    busy: Memo<bool>,
    perform: impl Fn(Intent) + Copy + 'static,
) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    if provider != ProviderKind::Codex {
        return text(tr("このプロバイダーはBexと一緒に更新されます。"))
            .text_size(px(13.0))
            .text_color(ui::colors::muted())
            .into_element();
    }
    let status = computed(move || connection.settings.with(|p| p.codex_cli_status()));
    let idle = move || !busy.get() && !connection.settings.with(|p| p.codex_cli_busy());
    let description = move || {
        status.with(|s| {
            let Some(s) = s.as_ref() else {
                return String::new();
            };
            [
                s.version.clone(),
                s.error.clone().map(localized),
                s.restart_required.then(|| {
                    tr("更新が完了しました。新しいCLIを使用するにはホストを再起動してください。")
                        .to_owned()
                }),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
        })
    };
    ui::card(vec![ui::setting(
        "Codex CLI",
        description,
        view()
            .flex_row()
            .gap_2()
            .child(
                ui::text_button(tr("状態を確認"), move || {
                    perform(Intent::ManageCodexCli(op::ManageCodexCli { update: false }))
                })
                .enabled(idle),
            )
            .child(
                ui::text_button(tr("最新版に更新"), move || {
                    perform(Intent::ManageCodexCli(op::ManageCodexCli { update: true }))
                })
                .enabled(idle),
            ),
    )])
    .into_element()
}

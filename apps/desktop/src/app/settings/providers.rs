//! The Providers page: each provider's accounts on the Host, signing in and
//! choosing the account threads use.
use super::{Row, notice, page_container, section};
use crate::app::{
    Desktop,
    ui::{color, driver_icon, icon, tint},
};
use agent_core::state::Intent;
use agent_domain::Driver;
use agent_protocol::{
    operations::{Account, AccountLogin},
    provider::ProviderKind,
};
use gpui_kit::{
    component::{
        Sizable, StyledExt,
        button::{Button, ButtonVariants},
        h_flex,
        input::{Input, InputState},
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::collections::BTreeSet;

const PROVIDERS: [ProviderKind; 2] = [ProviderKind::Codex, ProviderKind::Claude];

fn driver(provider: ProviderKind) -> Driver {
    match provider {
        ProviderKind::Codex => Driver::Codex,
        ProviderKind::Claude => Driver::Claude,
    }
}

fn provider_name(provider: ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Codex => "Codex",
        ProviderKind::Claude => "Claude",
    }
}

pub(super) struct ProvidersState {
    selected: ProviderKind,
    code: Entity<InputState>,
    /// Sign-ins finished or cancelled here; the Host keeps reporting the last one.
    closed_logins: BTreeSet<String>,
    /// A started sign-in and how many accounts its provider had then.
    started: Option<(String, usize)>,
}

impl ProvidersState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Desktop>) -> Self {
        Self {
            selected: ProviderKind::Codex,
            code: cx.new(|cx| InputState::new(window, cx)),
            closed_logins: BTreeSet::new(),
            started: None,
        }
    }
}

impl Desktop {
    fn provider_accounts(&self, provider: ProviderKind) -> Vec<Account> {
        self.snapshot
            .accounts
            .as_ref()
            .map(|accounts| {
                accounts
                    .accounts
                    .iter()
                    .filter(|account| account.provider == provider)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn selected_account(&self, provider: ProviderKind) -> Option<Account> {
        let accounts = self.snapshot.accounts.as_ref()?;
        accounts
            .accounts
            .iter()
            .find(|account| account.provider == provider && accounts.is_selected(account))
            .cloned()
    }

    /// The sign-in still waiting on the user for `provider`.
    fn open_login(&self, provider: ProviderKind) -> Option<AccountLogin> {
        let login = self.snapshot.account_login.as_ref()?;
        let state = &self.settings.providers;
        let finished = state.started.as_ref().is_some_and(|(id, count)| {
            id == &login.login_id && self.provider_accounts(provider).len() > *count
        });
        (login.provider == provider && !state.closed_logins.contains(&login.login_id) && !finished)
            .then(|| login.clone())
    }

    fn close_login(&mut self, login_id: String, cx: &mut Context<Self>) {
        self.settings.providers.closed_logins.insert(login_id);
        cx.notify();
    }

    pub(super) fn render_providers(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if !self.snapshot.connected {
            return page_container(
                896.,
                vec![notice("Connect an environment to set up its providers.")],
            );
        }
        let selected = self.settings.providers.selected;
        let mut list = v_flex();
        for (index, provider) in PROVIDERS.into_iter().enumerate() {
            let active = provider == selected;
            let status = self
                .selected_account(provider)
                .map(|account| account.email.unwrap_or(account.id))
                .unwrap_or_else(|| "Not signed in".into());
            list = list.child(
                h_flex()
                    .id(("provider", index))
                    .gap_3()
                    .px_4()
                    .py_3()
                    .cursor_pointer()
                    .when(index > 0, |row| {
                        row.border_t_1().border_color(tint("border", 0.5))
                    })
                    .when(active, |row| row.bg(color("accentSurface")))
                    .hover(|row| row.bg(color("accentSurface")))
                    .child(icon_badge(driver(provider)))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_sm().font_medium().child(provider_name(provider)))
                            .child(
                                div()
                                    .truncate()
                                    .text_xs()
                                    .text_color(color("textMuted"))
                                    .child(status),
                            ),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.settings.providers.selected = provider;
                        cx.notify();
                    })),
            );
        }
        let editor = self.render_provider(selected, window, cx);
        let refresh = Button::new("refresh-providers")
            .icon(icon("refresh-cw"))
            .ghost()
            .xsmall()
            .text_color(color("textMuted"))
            .tooltip("Refresh provider status")
            .accessibility_label("Refresh provider status")
            .on_click(cx.listener(|view, _, _, _| view.perform(Intent::LoadAccounts)));
        let card = h_flex()
            .items_start()
            .rounded(px(14.))
            .border_1()
            .border_color(tint("border", 0.6))
            .bg(tint("surface", 0.4))
            .overflow_hidden()
            .child(
                div()
                    .w(px(272.))
                    .flex_shrink_0()
                    .self_stretch()
                    .border_r_1()
                    .border_color(tint("border", 0.6))
                    .bg(tint("muted", 0.1))
                    .child(list),
            )
            .child(div().flex_1().min_w_0().p_4().child(editor));
        page_container(
            896.,
            vec![
                v_flex()
                    .gap(px(10.))
                    .child(
                        h_flex()
                            .min_h_7()
                            .px_4()
                            .justify_between()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(tint("text", 0.7))
                                    .child("Providers"),
                            )
                            .child(refresh),
                    )
                    .child(card)
                    .into_any_element(),
            ],
        )
    }

    fn render_provider(
        &mut self,
        provider: ProviderKind,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(accounts) = self.snapshot.accounts.clone() else {
            return notice("Loading…");
        };
        let host = self
            .snapshot
            .host_name
            .clone()
            .unwrap_or_else(|| "this environment".into());
        let login = self.open_login(provider);
        let provider_accounts = self.provider_accounts(provider);
        let signed_in = self.selected_account(provider);
        let description = match (&login, &signed_in) {
            (Some(_), _) => "Finish signing in in your browser.".to_owned(),
            (None, Some(account)) => format!(
                "Signed in as {}",
                account.email.clone().unwrap_or_else(|| account.id.clone())
            ),
            (None, None) => format!("Sign in on {host}."),
        };
        let mut controls = h_flex().gap(px(6.));
        let mut below = Vec::new();
        match &login {
            Some(login) => {
                let url = login.verification_url.clone();
                let copy = login.verification_url.clone();
                let cancel_id = login.login_id.clone();
                controls = controls
                    .child(
                        Button::new("login-open")
                            .outline()
                            .small()
                            .label("Open browser")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .child(
                        Button::new("login-copy")
                            .icon(icon("copy"))
                            .ghost()
                            .small()
                            .text_color(color("textMuted"))
                            .tooltip("Copy sign-in link")
                            .accessibility_label("Copy sign-in link")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                            }),
                    )
                    .child(
                        Button::new("login-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .accessibility_label("Cancel sign-in")
                            .on_click(cx.listener(move |view, _, _, _| {
                                let id = cancel_id.clone();
                                view.perform_then(
                                    Intent::CancelLogin {
                                        provider,
                                        id: id.clone(),
                                    },
                                    move |view, result, window, cx| match result {
                                        Ok(_) => view.close_login(id, cx),
                                        Err(error) => {
                                            let error = error.clone();
                                            view.show_error(&error, window, cx)
                                        }
                                    },
                                );
                            })),
                    );
                if !login.user_code.is_empty() {
                    below.push(
                        h_flex()
                            .py_2()
                            .gap_1()
                            .text_sm()
                            .text_color(color("textMuted"))
                            .child("Enter code")
                            .child(
                                div()
                                    .font_family("monospace")
                                    .text_color(color("text"))
                                    .child(login.user_code.clone()),
                            )
                            .child("in your browser.")
                            .into_any_element(),
                    );
                }
                if login.requires_code_submission {
                    let login_id = login.login_id.clone();
                    below.push(
                        v_flex()
                            .py_2()
                            .gap_2()
                            .text_sm()
                            .child("Paste the code the sign-in page shows.")
                            .child(Input::new(&self.settings.providers.code).small())
                            .child(
                                Button::new("login-complete")
                                    .outline()
                                    .small()
                                    .label("Continue")
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        let code = view
                                            .settings
                                            .providers
                                            .code
                                            .read(cx)
                                            .value()
                                            .trim()
                                            .to_owned();
                                        if code.is_empty() {
                                            return;
                                        }
                                        let id = login_id.clone();
                                        view.settings.providers.code.update(cx, |input, cx| {
                                            input.set_value("", window, cx)
                                        });
                                        view.perform_then(
                                            Intent::CompleteLogin {
                                                provider,
                                                id: id.clone(),
                                                code,
                                            },
                                            move |view, result, window, cx| match result {
                                                Ok(_) => view.close_login(id, cx),
                                                Err(error) => {
                                                    let error = error.clone();
                                                    view.show_error(&error, window, cx)
                                                }
                                            },
                                        );
                                    })),
                            )
                            .into_any_element(),
                    );
                }
            }
            None => {
                let count = provider_accounts.len();
                controls = controls.child(
                    Button::new("login-start")
                        .outline()
                        .small()
                        .label(if signed_in.is_some() {
                            "Change account"
                        } else {
                            "Sign in"
                        })
                        .on_click(cx.listener(move |view, _, _, _| {
                            view.perform_then(
                                Intent::StartLogin { provider },
                                move |view, result, window, cx| match result {
                                    Ok(_) => {
                                        if let Some(login) = &view.snapshot.account_login {
                                            view.settings.providers.started =
                                                Some((login.login_id.clone(), count));
                                        }
                                        cx.notify();
                                    }
                                    Err(error) => {
                                        let error = error.clone();
                                        view.show_error(&error, window, cx)
                                    }
                                },
                            );
                        })),
                );
            }
        }
        let mut account_row = Row::new("Account")
            .description(description)
            .control(controls);
        for element in below {
            account_row = account_row.below(element);
        }
        let mut rows = vec![account_row.render()];
        if let Some(error) = &accounts.error {
            rows.push(
                div()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(color("errorForeground"))
                    .child(error.clone())
                    .into_any_element(),
            );
        }
        for (index, account) in provider_accounts.into_iter().enumerate() {
            rows.push(self.render_account(
                index,
                account,
                &host,
                accounts.selected.get(&provider),
                cx,
            ));
        }
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .gap_2()
                    .child(icon_badge(driver(provider)))
                    .child(div().text_sm().font_medium().child(provider_name(provider))),
            )
            .child(section(None, None, None, rows))
            .into_any_element()
    }

    fn render_account(
        &self,
        index: usize,
        account: Account,
        host: &str,
        selected: Option<&String>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let in_use = selected == Some(&account.id);
        let mut details: Vec<String> = account.plan_type.iter().cloned().collect();
        let limit = self
            .snapshot
            .usage_limits()
            .into_iter()
            .find(|usage| usage.id == account.id);
        if let Some(usage) = &limit {
            details.extend(usage.windows.iter().map(|window| {
                format!("{} {}% left", window.label, window.remaining_percent)
            }));
            if usage.reset_credit_count > 0 {
                details.push(format!("{} reset credit(s)", usage.reset_credit_count));
            }
            if let Some(label) = usage.external_label.as_ref() {
                details.push(label.clone());
            }
        }
        let provider = account.provider;
        let id = account.id.clone();
        let delete_id = account.id.clone();
        let reset_id = account.id.clone();
        let reset_credit_id = limit.as_ref().and_then(|usage| usage.next_credit_id.clone());
        let can_reset = limit.as_ref().is_some_and(|usage| usage.reset_credit_count > 0);
        let external_url = limit.as_ref().and_then(|usage| usage.external_url.clone());
        let label = provider_name(provider);
        let message = format!(
            "Sign out of {label} on {host}? This stops running threads that share this sign-in. Thread history is kept."
        );
        let mut row = Row::new(account.email.clone().unwrap_or_else(|| account.id.clone()));
        if !details.is_empty() {
            row = row.description(details.join(" · "));
        }
        row.control(
            h_flex()
                .gap(px(6.))
                .when(can_reset, |actions| {
                    actions.child(
                        Button::new(("use-reset-credit", index))
                            .outline()
                            .small()
                            .label("Use reset")
                            .on_click(cx.listener(move |view, _, window, cx| {
                                let account_id = reset_id.clone();
                                let credit_id = reset_credit_id.clone();
                                view.confirm(
                                    crate::app::dialogs::Confirm {
                                        title: Some("Use a reset credit?".into()),
                                        message: "This redeems one credit and clears the current rate-limit windows.".into(),
                                        action: "Use credit".into(),
                                        destructive: false,
                                    },
                                    window,
                                    cx,
                                    move |view, _, _| {
                                        view.perform_then(
                                            Intent::ConsumeResetCredit {
                                                provider,
                                                account_id: account_id.clone(),
                                                credit_id: credit_id.clone(),
                                            },
                                            |view, result, _, _| {
                                                if result.is_ok() {
                                                    view.perform(Intent::LoadAccounts);
                                                }
                                            },
                                        )
                                    },
                                )
                            })),
                    )
                })
                .when_some(external_url, |actions, url| {
                    actions.child(
                        Button::new(("open-external-usage", index))
                            .ghost()
                            .small()
                            .label("Manage usage")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                })
                .child(if in_use {
                    div()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child("In use")
                        .into_any_element()
                } else {
                    Button::new(("use-account", index))
                        .outline()
                        .small()
                        .label("Use")
                        .on_click(cx.listener(move |view, _, _, _| {
                            view.perform(Intent::SelectAccount {
                                provider,
                                id: id.clone(),
                            })
                        }))
                        .into_any_element()
                })
                .child(
                    Button::new(("sign-out", index))
                        .ghost()
                        .small()
                        .label("Sign out")
                        .on_click(cx.listener(move |view, _, window, cx| {
                            let id = delete_id.clone();
                            view.confirm(
                                crate::app::dialogs::Confirm {
                                    title: None,
                                    message: message.clone(),
                                    action: "Sign out".into(),
                                    destructive: true,
                                },
                                window,
                                cx,
                                move |view, _, _| {
                                    view.perform(Intent::DeleteAccount {
                                        provider,
                                        id: id.clone(),
                                    })
                                },
                            );
                        })),
                ),
        )
        .render()
    }
}

fn icon_badge(driver: Driver) -> impl IntoElement {
    h_flex()
        .size_8()
        .flex_shrink_0()
        .justify_center()
        .rounded(px(8.))
        .border_1()
        .border_color(tint("border", 0.6))
        .bg(color("surface"))
        .child(driver_icon(driver).size_4())
}

use super::*;

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
                    .icon(Icon::default().path("bex/gauge.svg").size(px(23.)))
                    .accessibility_label("アカウントとモデル")
                    .tooltip("アカウントとモデル")
                    .large()
                    .w(px(44.))
                    .h(px(44.))
                    .ghost(),
            )
            .on_open_change(move |open, _, cx| {
                if *open {
                    let _ = opening.update(cx, |view, _| {
                        view.dispatch(Intent::ListAccounts(op::ListAccounts {}));
                        view.dispatch(Intent::LoadModels(op::LoadModels {}));
                    });
                }
            })
            .content(move |_, _, cx| {
                entity
                    .update(cx, |s, cx| s.model_controls(cx))
                    .unwrap_or_else(|_| div().into_any_element())
            })
            .into_any_element()
    }
    pub(super) fn model_controls(&self, cx: &Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let model = self.selected_model();
        let effort = self
            .draft()
            .effort
            .as_deref()
            .or_else(|| model.map(|model| model.default_reasoning_effort.as_str()))
            .unwrap_or_default();
        let effort_label = match effort {
            "none" => "なし",
            "minimal" => "最小",
            "low" => "低",
            "medium" => "中",
            "high" => "高",
            "xhigh" => "非常に高",
            "max" => "最大",
            "ultra" => "最高",
            value => value,
        };
        let model_label = format!(
            "{} {effort_label}",
            model.map_or("モデル", |model| model.display_name.as_str())
        );
        let models = Button::new("model-choice")
            .label(model_label)
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade() {
                    let view = owner.read(cx);
                    for model in view.snapshot.models.iter() {
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
            .label(format!("⚡︎ {speed_label}"))
            .accessibility_label("速度")
            .dropdown_caret(true)
            .small()
            .ghost()
            .dropdown_menu(move |mut menu, _, cx| {
                if let Some(owner) = entity.upgrade() {
                    let view = owner.read(cx);
                    let standard = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new("標準")
                            .checked(view.draft().service_tier.as_deref() == Some("default"))
                            .on_click(move |_, _, cx| {
                                let _ = standard.update(cx, |view, cx| {
                                    view.dispatch(Intent::SelectServiceTier {
                                        thread_id: view.draft_key().into(),
                                        service_tier: "default".into(),
                                    });
                                    cx.notify();
                                });
                            }),
                    );
                    if let Some(model) = view.selected_model() {
                        for tier in model
                            .service_tiers
                            .as_deref()
                            .unwrap_or_default()
                            .iter()
                            .filter(|tier| tier.id != "default")
                        {
                            let value = tier.id.clone();
                            let entity = entity.clone();
                            let label = tier.name.as_deref().unwrap_or(&tier.id).to_owned();
                            menu = menu.item(
                                PopupMenuItem::new(label)
                                    .checked(
                                        view.draft()
                                            .service_tier
                                            .as_deref()
                                            .or(model.default_service_tier.as_deref())
                                            == Some(&value),
                                    )
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
                }
                menu
            });
        let entity = cx.entity().downgrade();
        let accounts = self.snapshot.account.accounts.clone();
        let account_error = accounts
            .as_ref()
            .and_then(|accounts| accounts.error.clone());
        v_flex()
            .w(px(280.))
            .gap_2()
            .child(div().text_sm().child("アカウント"))
            .child(account_selector(
                accounts,
                self.busy > 0
                    || self.account_busy
                    || self.snapshot.account.login.is_some()
                    || !self.snapshot.connected,
                move |intent, _, cx| {
                    let _ = entity.update(cx, |view, cx| {
                        if view.busy > 0 || view.snapshot.account.login.is_some() {
                            return;
                        }
                        view.account_operation(intent);
                        cx.notify();
                    });
                },
            ))
            .child(self.button(
                "manage-accounts",
                "アカウントを管理",
                cx,
                |s, _, _| s.open_settings(),
            ))
            .when_some(account_error, |column, error| {
                column.child(div().text_xs().text_color(rgb(0xff7777)).child(error))
            })
            .child(h_flex().justify_between().child(speed).child(models))
            .children(
                self.snapshot
                    .model_error_messages()
                    .into_iter()
                    .map(|error| div().text_xs().text_color(rgb(0xff7777)).child(error)),
            )
            .child(model_effort_slider(
                &self.effort_slider,
                model.map_or(0, |model| model.supported_reasoning_efforts.len()),
                cx,
            ))
            .into_any_element()
    }

    pub(super) fn host_menu(&self, id: &'static str, cx: &Context<Self>) -> AnyElement {
        if let Some(hosts) = &self.hosts {
            Hosts::menu(
                hosts,
                id,
                self.remote.as_ref().map(|remote| remote.id.as_str()),
                self.busy > 0 || self.dictation.is_some(),
                cx,
            )
            .into_any_element()
        } else {
            div()
                .child(
                    self.remote
                        .as_ref()
                        .map_or("この端末", |remote| remote.name.as_str())
                        .to_owned(),
                )
                .into_any_element()
        }
    }

    pub(super) fn composer_folder(&self, cx: &Context<Self>) -> AnyElement {
        let selected_directory = self.snapshot.selected_directory();
        let entity = cx.entity().downgrade();
        Button::new("composer-folder")
            .label(if selected_directory.is_empty() {
                "チャット".into()
            } else {
                file_name(&selected_directory)
            })
            .accessibility_label(if selected_directory.is_empty() {
                "フォルダ: チャット".into()
            } else {
                format!("フォルダ: {}", selected_directory)
            })
            .icon(IconName::Folder)
            .dropdown_caret(true)
            .h(px(44.))
            .ghost()
            .disabled(self.busy > 0 || self.dictation.is_some())
            .dropdown_menu(move |mut menu, _, cx| {
                let Some(owner) = entity.upgrade() else {
                    return menu;
                };
                let state = owner.read(cx);
                let selected_directory = state.snapshot.selected_directory();
                let unassigned = entity.clone();
                menu = menu.item(
                    PopupMenuItem::new("チャット")
                        .checked(selected_directory.is_empty())
                        .on_click(move |_, window, cx| {
                            let _ = unassigned.update(cx, |s, cx| {
                                s.new_chat(String::new(), window, cx);
                                cx.notify();
                            });
                        }),
                );
                for project in state
                    .snapshot
                    .threads
                    .as_ref()
                    .map(|page| page.projects.as_slice())
                    .unwrap_or_default()
                {
                    for root in &project.roots {
                        let path = root.path.clone();
                        let label = if project.roots.len() == 1 {
                            project.name.clone()
                        } else {
                            path.clone()
                        };
                        let target = entity.clone();
                        menu = menu.item(
                            PopupMenuItem::new(label)
                                .checked(path == selected_directory)
                                .on_click(move |_, window, cx| {
                                    let _ = target.update(cx, |s, cx| {
                                        s.new_chat(path.clone(), window, cx);
                                        cx.notify();
                                    });
                                }),
                        );
                    }
                }
                if state
                    .snapshot
                    .threads
                    .as_ref()
                    .is_some_and(|page| page.has_more_projects)
                {
                    let target = entity.clone();
                    menu = menu.item(PopupMenuItem::new("さらにプロジェクトを読み込む").on_click(
                        move |_, _, cx| {
                            let _ = target.update(cx, |s, cx| {
                                s.dispatch(Intent::ExpandThreadList {
                                    project_id: None,
                                    projects: true,
                                });
                                cx.notify();
                            });
                        },
                    ));
                }
                if state.remote.is_none() {
                    let target = entity.clone();
                    menu = menu.item(PopupMenuItem::new("別のフォルダを選択…").on_click(
                        move |_, _, cx| {
                            let _ = target.update(cx, |s, _| s.pick_folder());
                        },
                    ));
                }
                menu
            })
            .into_any_element()
    }
    pub(super) fn chat(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let history = list(self.list.clone(), move |index, _, cx| {
            entity
                .update(cx, |view, cx| match view.rows.get(index).cloned() {
                    Some(ConversationRow::History) => {
                        if !view.has_older_history() {
                            return div().into_any_element();
                        }
                        let label = if view.history_loading {
                            "履歴を読み込み中…".into()
                        } else if !view.history_error.is_empty() {
                            format!("{} · 再試行", view.history_error)
                        } else {
                            "以前の履歴を読み込む".into()
                        };
                        h_flex()
                            .justify_center()
                            .p_4()
                            .child(view.button("older-history", label, cx, |view, window, cx| {
                                view.older(window, cx)
                            }))
                            .into_any_element()
                    }
                    Some(ConversationRow::Turn(turn)) => view.turn(&turn, cx),
                    Some(ConversationRow::Pending(id, pending)) => {
                        let row = view.pending_item(&id, &pending.draft, cx);
                        h_flex()
                            .justify_center()
                            .w_full()
                            .px_6()
                            .pb_8()
                            .child(
                                v_flex()
                                    .w_full()
                                    .max_w(px(CHAT_WIDTH))
                                    .gap_4()
                                    .child(row)
                                    .child(div().text_sm().text_color(rgb(0x999999)).child(
                                        if pending.accepted {
                                            "送信済み"
                                        } else {
                                            "送信中…"
                                        },
                                    )),
                            )
                            .into_any_element()
                    }
                    Some(ConversationRow::Request(key, request)) => view.request_card(
                        &key,
                        &agent_core::presentation::conversation::request(&key, &request),
                        cx,
                    ),
                    None => div().into_any_element(),
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
        .flex_1()
        .min_h_0();
        let mut body = v_flex().flex_1().min_w_0().h_full();
        if let Some(notice) = self
            .thread()
            .and_then(|thread| agent_core::session::input_unavailable_reason(thread))
        {
            body = body.child(div().px_4().py_2().text_sm().child(notice));
        }
        if let Some(notice) = self
            .thread()
            .and_then(|thread| agent_core::presentation::conversation::history_notice(thread))
        {
            body = body.child(div().px_4().py_2().text_sm().child(notice));
        }
        if self.thread().is_none() && pending_rows(&self.snapshot).next().is_none() {
            body = body.child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(div().text_2xl().child("何から始めましょうか？"))
                    .child(if self.snapshot.navigation.cwd.is_empty() {
                        "メッセージを入力して、作業を始めましょう。".into()
                    } else {
                        file_name(&self.snapshot.navigation.cwd)
                    }),
            );
        } else {
            let mut viewport = div()
                .relative()
                .pl_4()
                .flex_1()
                .min_h_0()
                .flex()
                .child(selection::ConversationSelection::wrap(
                    &self.selection,
                    history,
                ))
                .child(self.conversation_navigation(cx));
            if !self.list.is_following_tail() {
                viewport = viewport.child(
                    h_flex()
                        .absolute()
                        .bottom_2()
                        .left_0()
                        .w_full()
                        .justify_center()
                        .child(
                            self.icon_button(
                                "conversation-latest",
                                IconName::ArrowDown,
                                "最新のメッセージへ",
                                cx,
                                |view, _, _| {
                                    view.list.set_follow_mode(FollowMode::Tail);
                                },
                            )
                            .debug_selector(|| "conversation-latest".into())
                            .rounded_full()
                            .bg(rgb(0x303030))
                            .size(px(36.)),
                        ),
                );
            }
            body = body.child(viewport);
        }
        let key = self.draft_key().to_owned();
        let attachments = self.draft().attachments.clone();
        let mut files = h_flex().gap_2().flex_wrap();
        for (i, file) in attachments.iter().enumerate() {
            let key = key.clone();
            let remove = self
                .button(format!("attachment-{i}"), "×", cx, move |s, _, _| {
                    s.dispatch(Intent::RemoveAttachment {
                        draft_key: key.clone(),
                        index: i as u32,
                    });
                })
                .accessibility_label(format!("{}を外す", file.name))
                .w(px(28.))
                .h(px(28.))
                .rounded_full()
                .bg(rgb(0x222222));
            files = files.child(if file.is_image {
                div()
                    .relative()
                    .w(px(104.))
                    .h(px(104.))
                    .rounded_lg()
                    .overflow_hidden()
                    .child(self.image(&file.path, false, 104., true, cx))
                    .child(div().absolute().top_0().right_0().child(remove))
                    .into_any_element()
            } else {
                h_flex()
                    .gap_2()
                    .child(file.name.clone())
                    .child(remove)
                    .into_any_element()
            });
        }
        let running = self.thread().and_then(|thread| thread.active_turn_id());
        let empty = self.composer.read(cx).value().trim().is_empty() && attachments.is_empty();
        let phase = self.dictation.as_ref().map(|d| d.phase);
        let send = if let Some(id) = running.filter(|_| empty) {
            self.icon_button("stop", IconName::Pause, "停止", cx, move |s, _, _| {
                s.dispatch(Intent::Interrupt(op::Interrupt {
                    thread_id: s.selected().into(),
                    turn_id: id.clone(),
                }));
            })
            .icon(Icon::default().path("bex/stop.svg"))
            .disabled(!self.snapshot.connected || self.busy > 0)
        } else {
            self.icon_button("send", IconName::ArrowUp, "送信", cx, |s, _, cx| {
                s.send(cx)
            })
            .disabled(
                !self.snapshot.connected
                    || self.busy > 0
                    || empty
                    || self
                        .thread()
                        .and_then(|thread| agent_core::session::input_unavailable_reason(thread))
                        .is_some(),
            )
        };
        let microphone = Button::new("dictation-toggle")
            .icon(Icon::default().path("bex/microphone.svg").size(px(23.)))
            .ghost()
            .w(px(40.))
            .h(px(40.))
            .large()
            .tooltip("音声をCodexで文字起こし")
            .accessibility_label("音声をCodexで文字起こし")
            .disabled(!self.snapshot.connected || self.busy > 0)
            .on_click(cx.listener(|s, _, window, cx| {
                s.composer.read(cx).focus_handle(cx).focus(window, cx);
                s.start_dictation();
                cx.notify();
            }));
        let composer = v_flex()
            .key_context("ChatComposer")
            .track_focus(&self.composer.read(cx).focus_handle(cx))
            .capture_key_down(cx.listener(|s, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape"
                    && s.dictation
                        .as_ref()
                        .is_some_and(|state| state.phase != Phase::Transcribing)
                {
                    s.cancel_recording();
                    cx.stop_propagation();
                    cx.notify();
                }
            }))
            .capture_action(cx.listener(Self::paste_image))
            .capture_action(cx.listener(Self::composer_enter))
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .p(px(7.))
            .gap_2()
            .rounded(px(30.))
            .when(!attachments.is_empty(), |composer| composer.child(files))
            .bg(rgb(0x2b2b2b))
            .border_1()
            .border_color(rgb(0x363636))
            .when(phase.is_none(), |composer| {
                composer.child(
                    div().px_2().pt(px(10.)).pb_1().child(
                        Textarea::new(&self.composer)
                            .appearance(false)
                            .bordered(false)
                            .text_size(px(18.))
                            .aria_label("AI に依頼する")
                            .readonly(!self.snapshot.connected),
                    ),
                )
            })
            .when(phase.is_none(), |composer| {
                composer.child(
                    h_flex()
                        .gap_2()
                        .child(
                            self.icon_button(
                                "attach",
                                IconName::Plus,
                                "ファイルを添付",
                                cx,
                                |s, _, _| s.attach(),
                            )
                            .w(px(40.))
                            .h(px(40.))
                            .large()
                            .disabled(
                                !self.snapshot.connected
                                    || (!self.remote.is_none()
                                        && self.snapshot.navigation.cwd.is_empty())
                                    || self.busy > 0
                                    || phase.is_some(),
                            ),
                        )
                        .child(div().flex_1())
                        .child(self.model_menu(cx))
                        .child(microphone)
                        .child(
                            send.large()
                                .rounded(px(22.))
                                .w(px(44.))
                                .h(px(44.))
                                .primary(),
                        ),
                )
            })
            .when(phase.is_some(), |composer| {
                composer.child(self.dictation_bar(cx))
            });
        let controls = v_flex()
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .gap_3()
            .when(self.selected().is_empty(), |column| {
                column.child(
                    v_flex()
                        .items_start()
                        .gap_1()
                        .child(self.host_menu("composer-host", cx))
                        .child(self.composer_folder(cx)),
                )
            })
            .when(
                !self.selected().is_empty()
                    && self
                        .snapshot
                        .workspace
                        .review
                        .as_ref()
                        .is_some_and(|review| !review.files.is_empty()),
                |column| column.child(self.review_card(cx)),
            )
            .child(composer);
        body.child(
            h_flex()
                .justify_center()
                .px_6()
                .pt_2()
                .pb_4()
                .child(controls),
        )
        .into_any_element()
    }
}

fn account_selector(
    accounts: Option<Arc<agent_core::client::Accounts>>,
    disabled: bool,
    on_select: impl Fn(Intent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let selected = accounts
        .as_ref()
        .map(|accounts| {
            accounts
                .accounts
                .iter()
                .filter(|account| accounts.is_selected(account))
                .map(|account| account.email.as_deref().unwrap_or(&account.id))
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    let label = if selected.is_empty() {
        "アカウントを選択"
    } else {
        &selected
    };
    let on_select = Rc::new(on_select);
    Button::new("account-select")
        .debug_selector(|| "account-select".into())
        .label(label.to_owned())
        .accessibility_label("アカウントを変更")
        .dropdown_caret(true)
        .w_full()
        .small()
        .ghost()
        .disabled(
            disabled
                || accounts
                    .as_ref()
                    .is_none_or(|accounts| accounts.accounts.is_empty()),
        )
        .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
            if let Some(accounts) = &accounts {
                for account in &accounts.accounts {
                    let id = account.id.clone();
                    let select = on_select.clone();
                    let email = account.email.as_deref().unwrap_or(&account.id);
                    let label = account.plan_type.as_ref().map_or_else(
                        || email.to_owned(),
                        |plan| format!("{email} · {}", plan.to_uppercase()),
                    );
                    let provider = match account.provider {
                        agent_core::session::ProviderKind::Codex => "Codex",
                        agent_core::session::ProviderKind::Claude => "Claude",
                    };
                    menu = menu.item(
                        PopupMenuItem::new(format!("{provider} · {label}"))
                            .checked(accounts.is_selected(account))
                            .on_click(move |_, window, cx| {
                                select(
                                    Intent::SelectAccount(op::SelectAccount { id: id.clone() }),
                                    window,
                                    cx,
                                );
                            }),
                    );
                }
            }
            menu
        })
}

#[cfg(test)]
mod tests {
    use super::account_selector;
    use agent_core::state::Intent;
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, IntoElement, Modifiers, ParentElement, Render, Styled, TestAppContext,
        Window, component::Root, div,
    };
    use std::{cell::RefCell, rc::Rc, sync::Arc};

    struct Picker {
        accounts: Arc<agent_core::client::Accounts>,
        disabled: bool,
        selected: Rc<RefCell<Vec<String>>>,
    }
    impl Render for Picker {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let selected = self.selected.clone();
            div().size_full().child(account_selector(
                Some(self.accounts.clone()),
                self.disabled,
                move |intent, _, _| {
                    let Intent::SelectAccount(account) = intent else {
                        panic!("wrong account operation")
                    };
                    selected.borrow_mut().push(account.id);
                },
            ))
        }
    }

    #[gpui::test]
    fn account_menu_selects_the_requested_account_and_blocks_changes_while_busy(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let selected = Rc::new(RefCell::new(Vec::new()));
        let accounts = Arc::new(
            serde_json::from_value(serde_json::json!({
                "accounts":[{"provider":"codex","id":"first","email":"first@example.invalid","planType":"plus"},
                            {"provider":"codex","id":"second","email":"second@example.invalid","planType":"pro"},
                            {"provider":"claude","id":"claude:third","email":"claude@example.invalid","planType":"max"}],
                "selectedId":"first","error":null
            }))
            .unwrap(),
        );
        let captures = selected.clone();
        let picker = cx.new(|_| Picker {
            accounts,
            disabled: false,
            selected: captures,
        });
        let rendered = picker.clone();
        let (_, window) = cx.add_window_view(|window, cx| Root::new(rendered, window, cx));
        window.run_until_parked();
        let bounds = window.debug_bounds("account-select").unwrap();
        window.simulate_click(bounds.center(), Modifiers::default());
        window.simulate_keystrokes("down down enter");
        assert_eq!(*selected.borrow(), ["second"]);
        window.simulate_click(bounds.center(), Modifiers::default());
        window.simulate_keystrokes("down down down enter");
        assert_eq!(*selected.borrow(), ["second", "claude:third"]);
        window.update(|_, cx| {
            picker.update(cx, |picker, cx| {
                picker.disabled = true;
                cx.notify();
            })
        });
        window.run_until_parked();
        let bounds = window.debug_bounds("account-select").unwrap();
        window.simulate_click(bounds.center(), Modifiers::default());
        window.simulate_keystrokes("down enter");
        assert_eq!(
            *selected.borrow(),
            ["second", "claude:third"],
            "busy picker must not switch accounts"
        );
    }
}

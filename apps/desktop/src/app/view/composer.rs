use super::*;
use gpui_kit::component::button::ButtonCustomVariant;

impl Desktop {
    fn history_control(
        &self,
        control: agent_core::presentation::conversation::HistoryControl,
        cx: &Context<Self>,
    ) -> AnyElement {
        let label: SharedString = control.load_label.into();
        h_flex()
            .w_full()
            .justify_center()
            .px_6()
            .py_2()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(CHAT_WIDTH))
                    .gap_1p5()
                    .when(control.has_more, |body| {
                        body.child(
                            Button::new("conversation-history-load")
                                .debug_selector(|| "conversation-history-load".into())
                                .accessibility_label(label.clone())
                                .small()
                                .ghost()
                                .w_full()
                                .disabled(control.loading)
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap_3()
                                        .child(div().flex_1().h(px(1.)).bg(rgb(appearance::BORDER)))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(appearance::MUTED))
                                                .child(label),
                                        )
                                        .child(
                                            div().flex_1().h(px(1.)).bg(rgb(appearance::BORDER)),
                                        ),
                                )
                                .on_click(cx.listener(|view, _, _, cx| {
                                    if let Some(thread_id) = view.selected().cloned() {
                                        view.pause_tail();
                                        view.dispatch(Intent::ReadOlder { thread_id });
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .when_some(control.error, |body, error| {
                        body.child(
                            div()
                                .debug_selector(|| "conversation-history-error".into())
                                .text_xs()
                                .text_center()
                                .text_color(rgb(appearance::MUTED))
                                .child(error),
                        )
                    }),
            )
            .into_any_element()
    }
    fn composer_context(&self, cx: &Context<Self>) -> AnyElement {
        let mut row = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .px_2()
            .child(self.composer_folder(cx));
        if let Some(hosts) = &self.hosts {
            row = row.child(Hosts::menu(
                hosts,
                "composer-host",
                self.remote.as_ref().map(|remote| remote.id.as_str()),
                self.remote
                    .as_ref()
                    .map_or("Local", |remote| remote.name.as_str()),
                self.busy > 0 || self.dictation.is_some(),
                cx,
            ));
        } else {
            let name = self
                .remote
                .as_ref()
                .map_or("Local", |remote| remote.name.as_str())
                .to_owned();
            row = row.child(
                h_flex()
                    .id("composer-host")
                    .min_w_0()
                    .max_w(px(200.))
                    .h_8()
                    .px_2()
                    .gap_2()
                    .tooltip({
                        let name = name.clone();
                        move |window, cx| {
                            tooltip::Tooltip::new(format!("実行先: {name}")).build(window, cx)
                        }
                    })
                    .child(Icon::default().path("bex/monitor.svg").size_4())
                    .child(div().min_w_0().text_ellipsis().child(name)),
            );
        }
        if let Some(review) = self
            .snapshot
            .workspace
            .review
            .as_ref()
            .filter(|review| !review.branch.is_empty())
        {
            let entity = cx.entity().downgrade();
            row = row.child(
                Button::new("composer-branch")
                    .debug_selector(|| "composer-branch".into())
                    .label(review.branch.clone())
                    .icon(Icon::default().path("bex/branch.svg"))
                    .accessibility_label(format!("現在のブランチ: {}", review.branch))
                    .tooltip(format!("現在のブランチ: {}", review.branch))
                    .dropdown_caret(true)
                    .h_8()
                    .min_w_0()
                    .max_w(px(260.))
                    .flex_shrink_1()
                    .ghost()
                    .disabled(!self.snapshot.connected || self.dictation.is_some())
                    .dropdown_menu(move |menu, _, _| {
                        let refresh = entity.clone();
                        let changes = entity.clone();
                        menu.item(PopupMenuItem::new("変更を表示").on_click(move |_, _, cx| {
                            let _ = changes.update(cx, |s, cx| {
                                s.open_review(None, cx);
                                cx.notify();
                            });
                        }))
                        .item(PopupMenuItem::new("更新").on_click(move |_, _, cx| {
                            let _ = refresh.update(cx, |s, _| s.refresh_review());
                        }))
                    }),
            );
        }
        row.into_any_element()
    }

    fn composer_folder(&self, cx: &Context<Self>) -> AnyElement {
        let selected_directory = self.snapshot.selected_directory();
        let entity = cx.entity().downgrade();
        Button::new("composer-folder")
            .debug_selector(|| "composer-folder".into())
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
            .tooltip(if selected_directory.is_empty() {
                "チャット".into()
            } else {
                selected_directory.clone()
            })
            .h_8()
            .min_w_0()
            .max_w(px(260.))
            .flex_shrink_1()
            .ghost()
            .disabled(self.busy > 0 || self.dictation.is_some())
            .dropdown_menu(move |mut menu, _, cx| {
                let Some(owner) = entity.upgrade() else {
                    return menu;
                };
                let state = owner.read(cx);
                let selected_directory = state.snapshot.selected_directory();
                let list = state.snapshot.thread_list();
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
                for project in list
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
                if list.as_ref().is_some_and(|page| page.has_more_projects) {
                    let target = entity.clone();
                    menu = menu.item(PopupMenuItem::new("さらにプロジェクトを読み込む").on_click(
                        move |_, _, cx| {
                            let _ = target.update(cx, |s, cx| {
                                s.dispatch(Intent::ExpandProjects);
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
                    Some(ConversationRow::History(control)) => view.history_control(control, cx),
                    Some(ConversationRow::Turn(turn)) => {
                        let session = view
                            .rendered
                            .as_ref()
                            .and_then(|conversation| conversation.source.id.clone());
                        view.turn(session.as_ref(), &turn, cx)
                    }
                    Some(ConversationRow::Pending(id, pending)) => {
                        let row = view.pending_item(&id, &pending, cx);
                        h_flex()
                            .justify_center()
                            .w_full()
                            .px_6()
                            .pb_8()
                            .child(v_flex().w_full().max_w(px(CHAT_WIDTH)).gap_4().child(row))
                            .into_any_element()
                    }
                    Some(ConversationRow::Request(request)) => view.request_card(&request, cx),
                    None => div().into_any_element(),
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
        .flex_1()
        .min_h_0();
        let mut body = v_flex()
            .debug_selector(|| "conversation-chat".into())
            .flex_1()
            .min_w_0()
            .h_full();
        if let Some(notice) = self
            .thread()
            .and_then(|thread| agent_protocol::session::input_unavailable_reason(thread))
        {
            body = body.child(div().px_4().py_2().text_sm().child(notice));
        }
        if let Some(notice) = self
            .thread()
            .and_then(|thread| agent_core::presentation::conversation::history_notice(thread))
        {
            body = body.child(div().px_4().py_2().text_sm().child(notice));
        }
        if self.rendered.is_none() {
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
                            Self::icon_button(
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
                            .bg(rgb(appearance::RAISED))
                            .size(px(36.)),
                        ),
                );
            }
            body = body.child(viewport);
        }
        let key = self.draft_key().to_owned();
        let attachments = self.draft().attachments.clone();
        let mut files = h_flex().gap_3().p_2().flex_wrap();
        for (i, file) in attachments.iter().enumerate() {
            let key = key.clone();
            let remove = Self::icon_button(
                format!("attachment-{i}"),
                IconName::Close,
                format!("{}を外す", file.name),
                cx,
                move |s, _, cx| {
                    cx.stop_propagation();
                    s.dispatch(Intent::RemoveAttachment {
                        draft_key: key.clone(),
                        index: i as u32,
                    });
                },
            )
            .debug_selector(move || format!("attachment-remove-{i}"))
            .xsmall()
            .rounded_full();
            files = files.child(if file.is_image {
                div()
                    .id(format!("draft-attachment-{i}"))
                    .relative()
                    .size(px(120.))
                    .flex_shrink_0()
                    .child(
                        div()
                            .size_full()
                            .rounded_lg()
                            .overflow_hidden()
                            .child(self.image(&file.path, false, 120., true, cx)),
                    )
                    .child(
                        div()
                            .id(format!("attachment-remove-hitbox-{i}"))
                            .occlude()
                            .absolute()
                            .top(px(-4.))
                            .right(px(-4.))
                            .size(px(18.))
                            .child(
                                remove
                                    .size(px(18.))
                                    .custom(
                                        ButtonCustomVariant::new(cx)
                                            .foreground(rgb(0x222222).into())
                                            .hover(rgb(0xe6e6e6).into())
                                            .active(rgb(0xd4d4d4).into()),
                                    )
                                    .bg(rgb(0xffffff)),
                            ),
                    )
                    .into_any_element()
            } else {
                h_flex()
                    .gap_2()
                    .child(file.name.clone())
                    .child(remove.size(px(28.)).bg(rgb(appearance::RAISED)))
                    .into_any_element()
            });
        }
        let running = self.thread().and_then(|thread| thread.active_turn_id());
        let empty = self.composer.read(cx).value().trim().is_empty() && attachments.is_empty();
        let inactive = empty && running.is_none();
        let phase = self.dictation.as_ref().map(|d| d.phase);
        let send = if let Some(id) = running.filter(|_| empty) {
            Self::icon_button(
                "stop",
                Icon::default().path("bex/stop.svg"),
                "停止",
                cx,
                move |s, _, _| {
                    s.dispatch(Intent::Interrupt(op::Interrupt {
                        thread_id: s.selected().expect("selected conversation").clone(),
                        turn_id: id.clone(),
                    }));
                },
            )
            .disabled(!self.snapshot.connected || self.busy > 0)
        } else {
            Self::icon_button("send", IconName::ArrowUp, "送信", cx, |s, _, cx| {
                s.send(cx)
            })
            .disabled(
                !self.snapshot.connected
                    || self
                        .snapshot
                        .model_provider_for_draft(self.snapshot.navigation.draft_key.clone())
                        .is_none()
                    || self.busy > 0
                    || empty
                    || self
                        .thread()
                        .and_then(|thread| {
                            agent_protocol::session::input_unavailable_reason(thread)
                        })
                        .is_some(),
            )
        };
        let microphone = Button::new("dictation-toggle")
            .icon(Icon::default().path("bex/microphone.svg").size(px(20.)))
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
            .capture_key_down(cx.listener(|s, event: &KeyDownEvent, window, cx| {
                let modifiers = event.keystroke.modifiers;
                if !(modifiers.shift || modifiers.control || modifiers.alt || modifiers.platform)
                    && s.completion_key(&event.keystroke.key, window, cx)
                {
                    return;
                }
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
            .capture_action(
                cx.listener(|s, _: &gpui_kit::component::input::MoveUp, window, cx| {
                    s.composer_arrow("up", window, cx);
                }),
            )
            .capture_action(cx.listener(
                |s, _: &gpui_kit::component::input::MoveDown, window, cx| {
                    s.composer_arrow("down", window, cx);
                },
            ))
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .p(px(7.))
            .gap_2()
            .rounded(px(22.))
            .child(self.completion_menu(cx))
            .when(!attachments.is_empty(), |composer| composer.child(files))
            .bg(rgb(appearance::SURFACE))
            .border_1()
            .border_color(rgb(appearance::BORDER))
            .when(phase.is_none(), |composer| {
                composer.child(
                    div().px_2().pt(px(10.)).pb_1().child(
                        Textarea::new(&self.composer)
                            .appearance(false)
                            .bordered(false)
                            .text_size(px(14.))
                            .aria_label("AI に依頼する")
                            .readonly(!self.snapshot.connected),
                    ),
                )
            })
            .when(phase.is_none(), |composer| {
                composer.child(
                    h_flex()
                        .child(
                            Self::icon_button(
                                "attach",
                                IconName::Plus,
                                "ファイルを添付",
                                cx,
                                |s, _, _| s.attach(),
                            )
                            .debug_selector(|| "attach".into())
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
                        .child(self.permission_menu(cx))
                        .child(div().flex_1())
                        .child(self.fast_control("model-fast", false, cx))
                        .child(self.model_menu(cx))
                        .child(self.effort_control("model-effort", false, cx))
                        .child(microphone)
                        .child(
                            send.large().rounded_full().w(px(40.)).h(px(40.)).custom(
                                ButtonCustomVariant::new(cx)
                                    .color(
                                        rgb(if inactive {
                                            appearance::BORDER
                                        } else {
                                            0xffffff
                                        })
                                        .into(),
                                    )
                                    .foreground(
                                        rgb(if inactive {
                                            appearance::MUTED
                                        } else {
                                            appearance::SIDEBAR
                                        })
                                        .into(),
                                    )
                                    .hover(rgb(0xd4d4d4).into())
                                    .active(rgb(appearance::MUTED).into()),
                            ),
                        ),
                )
            })
            .when(phase.is_some(), |composer| {
                composer.child(self.dictation_bar(cx))
            });
        let controls = v_flex()
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .gap_1()
            .when(
                !self.selected().is_none()
                    && self
                        .snapshot
                        .workspace
                        .review
                        .as_ref()
                        .is_some_and(|review| !review.files.is_empty()),
                |column| column.child(self.review_card(cx)),
            )
            .child(self.composer_context(cx))
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

#[cfg(test)]
mod composer_tests {
    use super::*;
    use agent_protocol::{execution::TurnStatus, items::ItemBody};
    use core::prelude::v1::test;
    use gpui_kit as gpui;

    struct ComposerView(Entity<Desktop>);
    impl Render for ComposerView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |view, cx| view.chat(cx))
        }
    }

    #[gpui::test]
    fn history_control_preserves_visible_content_and_offers_retry_after_a_failed_page(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let session = SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "history".into(),
        };
        let turns = |range: std::ops::Range<usize>| {
            range
                .map(|index| {
                    Arc::new(Turn {
                        id: format!("turn-{index}").into(),
                        status: TurnStatus::Completed,
                        items: Some(vec![Arc::new(Item::new(
                            format!("answer-{index}").into(),
                            Default::default(),
                            ItemBody::AssistantText {
                                text: format!(
                                    "Answer {index}\n{}",
                                    "Earlier conversation text.\n".repeat(8)
                                ),
                                phase: agent_protocol::items::AssistantPhase::Final,
                            },
                        ))]),
                        ..Default::default()
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut handle = None;
        let (_, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::SideChat {
                        remote: Some(RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid".into(),
                        }),
                        cwd: "/fixture".into(),
                    },
                    window,
                    cx,
                )
            });
            desktop.update(cx, |view, cx| {
                let snapshot = Arc::make_mut(&mut view.snapshot);
                let navigation = Arc::make_mut(&mut snapshot.navigation);
                navigation.thread_id = Some(session.clone());
                navigation.draft_key = session.clone().into();
                Arc::make_mut(&mut snapshot.conversations).insert(
                    session.clone(),
                    Arc::new(Thread {
                        id: Some(session.clone()),
                        turns: Some(turns(2..8)),
                        history_has_more: Some(true),
                        history_cursor: Some("older-page".into()),
                        history_read_state: Some(agent_protocol::session::HistoryReadState::new(
                            agent_protocol::session::HistoryReadKind::Partial,
                            Vec::new(),
                        )),
                        ..Default::default()
                    }),
                );
                view.sync_rows(true, window, cx);
                view.list.scroll_to(ListOffset::default());
            });
            handle = Some(desktop.clone());
            ComposerView(desktop)
        });
        let desktop = handle.unwrap();
        window.simulate_resize(size(px(800.), px(700.)));
        window.run_until_parked();
        let button = window
            .debug_bounds("conversation-history-load")
            .expect("older-history control at the top");
        window.simulate_click(button.center(), Modifiers::default());
        let top = window.update(|_, cx| {
            desktop.update(cx, |view, _| {
                assert!(!view.list.is_following_tail());
                view.list.bounds_for_item(1).unwrap().top()
            })
        });
        window.update(|window, cx| {
            desktop.update(cx, |view, cx| {
                let thread = Arc::make_mut(
                    Arc::make_mut(&mut Arc::make_mut(&mut view.snapshot).conversations)
                        .get_mut(&session)
                        .unwrap(),
                );
                thread.turns = Some(turns(0..8));
                view.sync_rows(false, window, cx);
                cx.notify();
            })
        });
        window.run_until_parked();
        window.update(|_, cx| {
            desktop.update(cx, |view, _| {
                assert!(
                    (view.list.bounds_for_item(3).unwrap().top() - top).abs() < px(1.),
                    "prepending history must keep the old first answer at the same screen position"
                );
                view.list.scroll_to(ListOffset::default());
            })
        });
        let key = op::OperationKey::History {
            session: session.clone(),
        };
        let mut store_handle = None;
        window.update(|window, cx| {
            desktop.update(cx, |view, cx| {
                let store = runtime.block_on(async {
                    Arc::new(agent_core::store::Store::offline((*view.snapshot).clone()))
                });
                view.snapshot = store.snapshot();
                let app_runtime = view.runtime.clone();
                view.session = Some(runtime.block_on(async {
                    let (updates, receive) = async_channel::unbounded();
                    tokio::spawn(StoreSession::publish(
                        Ok(store.clone()),
                        app_runtime,
                        updates,
                        Some,
                        |_| None,
                    ));
                    receive.recv().await.unwrap().unwrap().unwrap()
                }));
                store_handle = Some(store);
                view.sync_rows(false, window, cx);
                cx.notify();
            })
        });
        window.run_until_parked();
        let store = store_handle.unwrap();
        for attempt in 0..2 {
            let previous_generation = store
                .snapshot()
                .operations
                .get(&key)
                .map_or(0, |state| state.generation);
            let button = window.debug_bounds("conversation-history-load").unwrap();
            window.simulate_click(button.center(), Modifiers::default());
            runtime.block_on(async {
                let mut updates = store.subscribe();
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let failed = updates
                            .borrow_and_update()
                            .operations
                            .get(&key)
                            .is_some_and(|state| {
                                state.generation > previous_generation
                                    && matches!(state.phase, op::OperationPhase::Failed { .. })
                            });
                        if failed {
                            break;
                        }
                        updates.changed().await.unwrap();
                    }
                })
                .await
                .expect("each button press must execute a history request, including retry");
            });
            window.update(|window, cx| {
                desktop.update(cx, |view, cx| {
                    view.accept_snapshot(window, cx);
                    cx.notify();
                })
            });
            window.run_until_parked();
            assert!(
                window.debug_bounds("conversation-history-error").is_some(),
                "request failure {attempt} must appear inline"
            );
        }
        assert!(
            window.debug_bounds("conversation-history-load").is_some(),
            "the same control remains available to retry"
        );
        window.update(|window, cx| {
            desktop.update(cx, |view, cx| {
                view.session.take();
                let snapshot = Arc::make_mut(&mut view.snapshot);
                Arc::make_mut(&mut snapshot.operations).remove(&key);
                let thread = Arc::make_mut(
                    Arc::make_mut(&mut snapshot.conversations)
                        .get_mut(&session)
                        .unwrap(),
                );
                thread.history_has_more = Some(false);
                thread.history_cursor = None;
                view.sync_rows(false, window, cx);
                cx.notify();
            })
        });
        window.run_until_parked();
        assert!(window.debug_bounds("conversation-history-load").is_none());
        assert!(window.debug_bounds("conversation-history-error").is_none());
    }

    #[gpui::test]
    fn removing_an_image_does_not_open_its_gallery(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let image_directory = tempfile::tempdir().unwrap();
        let image_path = image_directory.path().join("attachment.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .save(&image_path)
            .unwrap();
        let source = image_path.to_string_lossy().into_owned();
        let mut desktop_handle = None;
        let mut store_handle = None;
        let mut draft_key = None;
        let (_, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::SideChat {
                        remote: Some(RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid".into(),
                        }),
                        cwd: image_directory.path().to_string_lossy().into_owned(),
                    },
                    window,
                    cx,
                )
            });
            desktop.update(cx, |view, cx| {
                let key = view.draft_key().clone();
                draft_key = Some(key.clone());
                Arc::make_mut(&mut Arc::make_mut(&mut view.snapshot).drafts).insert(
                    key,
                    Arc::new(Draft {
                        text: "Keep this draft".into(),
                        attachments: vec![agent_core::state::Attachment {
                            path: source.clone(),
                            name: "attachment.png".into(),
                            is_image: true,
                        }],
                        ..Default::default()
                    }),
                );
                view.images.insert(
                    view.image_key(&source, false),
                    ImageState {
                        focus: cx.focus_handle(),
                        source: Arc::new(source.clone()),
                        path: Some(source.clone().into()),
                        error: None,
                    },
                );
                let store = runtime.block_on(async {
                    Arc::new(agent_core::store::Store::offline((*view.snapshot).clone()))
                });
                let app_runtime = view.runtime.clone();
                view.session = Some(runtime.block_on(async {
                    let (updates, receive) = async_channel::unbounded();
                    tokio::spawn(StoreSession::publish(
                        Ok(store.clone()),
                        app_runtime,
                        updates,
                        Some,
                        |_| None,
                    ));
                    receive.recv().await.unwrap().unwrap().unwrap()
                }));
                store_handle = Some(store);
            });
            desktop_handle = Some(desktop.clone());
            ComposerView(desktop)
        });
        window.simulate_resize(size(px(700.), px(700.)));
        window.run_until_parked();
        let bounds = window.debug_bounds("chat-image").unwrap();
        window.simulate_click(bounds.center(), Modifiers::default());
        let desktop = desktop_handle.unwrap();
        window.update(|_, cx| {
            desktop.update(cx, |view, _| {
                assert!(
                    view.image_gallery.is_some(),
                    "the thumbnail still opens the gallery"
                );
                view.image_gallery = None;
            })
        });
        window.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        window.run_until_parked();
        let remove = window.debug_bounds("attachment-remove-0").unwrap().center();
        window.simulate_click(remove, Modifiers::default());
        window.update(|_, cx| desktop.update(cx, |view, _| assert!(view.image_gallery.is_none())));
        let snapshot = store_handle.unwrap().snapshot();
        let draft = &snapshot.drafts[&draft_key.unwrap()];
        assert!(draft.attachments.is_empty());
        assert_eq!(draft.text, "Keep this draft");
    }
}

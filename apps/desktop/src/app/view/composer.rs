use super::*;

impl Desktop {
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
        let mut body = v_flex().flex_1().min_w_0().h_full();
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
        let mut files = h_flex().gap_3().p_2().flex_wrap();
        for (i, file) in attachments.iter().enumerate() {
            let key = key.clone();
            let remove = Button::new(format!("attachment-{i}"))
                .icon(IconName::Close)
                .xsmall()
                .ghost()
                .on_click(cx.listener(move |s, _, _, cx| {
                    s.dispatch(Intent::RemoveAttachment {
                        draft_key: key.clone(),
                        index: i as u32,
                    });
                    cx.notify();
                }))
                .accessibility_label(format!("{}を外す", file.name))
                .size(px(28.))
                .rounded_full()
                .bg(rgb(0x222222));
            files = files.child(if file.is_image {
                let group = SharedString::from(format!("draft-image-{i}"));
                div()
                    .relative()
                    .group(group.clone())
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
                            .absolute()
                            .top(px(-4.))
                            .right(px(-4.))
                            .invisible()
                            .group_hover(group, |style| style.visible())
                            .child(
                                remove
                                    .size(px(18.))
                                    .bg(rgb(0xffffff))
                                    .text_color(rgb(0x222222)),
                            ),
                    )
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
                    thread_id: s.selected().expect("selected conversation").clone(),
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
                        .and_then(|thread| {
                            agent_protocol::session::input_unavailable_reason(thread)
                        })
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
            .rounded(px(30.))
            .child(self.completion_menu(cx))
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
                        .child(
                            self.icon_button(
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
                        .child(send.large().rounded(px(22.)).w(px(44.)).h(px(44.)).ghost()),
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

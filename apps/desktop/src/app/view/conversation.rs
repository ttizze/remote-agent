use super::*;
use agent_core::presentation::conversation::{
    ActivityExpansion, ConversationRowContent, activity_is_expanded,
};

impl Desktop {
    pub(super) fn conversation_navigation(&self, cx: &Context<Self>) -> AnyElement {
        let entries: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let ConversationRow::Turn(turn) = row else {
                    return None;
                };
                turn.rows.iter().find_map(|row| match &row.content {
                    ConversationRowContent::User { item } => {
                        item.data.body.as_ref().map(|text| (index, text, turn))
                    }
                    _ => None,
                })
            })
            .collect();
        let count = entries.len().min(8);
        let mut markers = v_flex()
            .absolute()
            .left_0()
            .top_0()
            .h_full()
            .w(px(56.))
            .justify_center();
        let current = self.list.logical_scroll_top().item_ix;
        for position in 0..count {
            let sample = position * (entries.len() - 1) / count.saturating_sub(1).max(1);
            let (index, user, turn) = entries[sample];
            let active = current >= index
                && (position + 1 == count
                    || current < entries[(position + 1) * (entries.len() - 1) / (count - 1)].0);
            let user: String = user.chars().take(160).collect();
            let distance = self
                .hovered_conversation_marker
                .map(|hovered| position.abs_diff(hovered));
            let width = match distance {
                Some(0) => 28.,
                Some(1) => 20.,
                Some(2) => 14.,
                Some(3) => 10.,
                _ => 7.,
            };
            let marker_id = SharedString::from(format!("conversation-marker-{index}"));
            let preview = (distance == Some(0)).then(|| {
                let answer = turn.rows.iter().find_map(|row| match &row.content {
                    ConversationRowContent::Response { item, .. } => item
                        .data
                        .body
                        .as_ref()
                        .map(|text| text.chars().take(240).collect::<String>()),
                    _ => None,
                });
                let preview_width =
                    (self.list.viewport_bounds().size.width - px(48.)).clamp(px(100.), px(300.));
                v_flex()
                    .absolute()
                    .left(px(56.))
                    .top(-px(24.))
                    .w(preview_width)
                    .p_3()
                    .gap_2()
                    .rounded_lg()
                    .bg(rgb(0x303030))
                    .border_1()
                    .border_color(rgb(0x454545))
                    .shadow_md()
                    .text_sm()
                    .child(div().max_h(px(44.)).overflow_hidden().child(user.clone()))
                    .children(answer.map(|answer| {
                        div()
                            .max_h(px(66.))
                            .overflow_hidden()
                            .text_color(rgb(0xaaaaaa))
                            .child(answer)
                    }))
            });
            markers = markers.child(
                div()
                    .id(marker_id.clone())
                    .on_hover(cx.listener(move |view, hovered, _, cx| {
                        if *hovered {
                            view.hovered_conversation_marker = Some(position);
                        } else if view.hovered_conversation_marker == Some(position) {
                            view.hovered_conversation_marker = None;
                        }
                        cx.notify();
                    }))
                    .relative()
                    .child(
                        Button::new(marker_id)
                            .text()
                            .small()
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.list.scroll_to(ListOffset {
                                    item_ix: index,
                                    offset_in_item: px(0.),
                                });
                                cx.notify();
                            }))
                            .debug_selector(move || format!("conversation-marker-{index}"))
                            .accessibility_label(format!("この発言へ移動: {user}"))
                            .w(px(56.))
                            .h(px(10.))
                            .pl(px(16.5))
                            .pr(px(11.5))
                            .child(
                                h_flex().w_full().child(
                                    div()
                                        .debug_selector(move || {
                                            format!("conversation-marker-line-{index}")
                                        })
                                        .h(px(if distance == Some(0) { 2. } else { 1. }))
                                        .w(px(width))
                                        .bg(rgb(if active || distance == Some(0) {
                                            0xeeeeee
                                        } else {
                                            0x777777
                                        })),
                                ),
                            ),
                    )
                    .children(preview),
            );
        }
        markers.into_any_element()
    }

    pub(super) fn diff(
        diffs: &mut HashMap<String, Entity<DiffView>>,
        id: String,
        patch: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = diffs
            .entry(id.clone())
            .or_insert_with(|| {
                cx.new(|_| DiffView::new(patch.to_owned().into(), id == "workspace-patch"))
            })
            .clone();
        view.update(cx, |s, cx| s.set_source(patch, cx));
        view.into_any_element()
    }
    pub(super) fn activity_text(id: String, content: &str, language: &str) -> TextView {
        // Reserve the output viewport before asynchronous Markdown parsing.
        // Long tool output scrolls inside it instead of moving the conversation.
        let lines = content.lines().take(18).count().max(2);
        TextView::markdown(SharedString::from(id), fenced(content, language))
            .selectable(true)
            .scrollable(true)
            .h(px(lines as f32 * 22. + 32.))
    }
    pub(super) fn item(
        &mut self,
        projected: &agent_core::presentation::conversation::RenderedItem,
        item: &Item,
        turn: Option<&Turn>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = item.id.to_string();
        let title = projected.data.title.as_deref();
        let body_text = projected.data.body.as_deref();
        let expanded = self.expanded_items.contains(&id);
        let deferred = projected.data.deferred;
        if expanded && deferred {
            let key = (
                turn.map(|turn| turn.id.clone()).unwrap_or_default(),
                item.id.clone(),
            );
            let label = match self
                .item_details
                .get(&key)
                .map(|state| state.error.as_deref())
            {
                Some(None) => "詳細を読み込み中…".to_owned(),
                Some(Some(error)) => format!("{error} · 再試行"),
                None => "詳細を読み込む".to_owned(),
            };
            let title = title.expect("activity has a title");
            let toggle = id.clone();
            return v_flex()
                .gap_2()
                .child(self.button(
                    format!("collapse-{id}"),
                    format!("⌄ {title}"),
                    cx,
                    move |s, _, _| {
                        s.expanded_items.remove(&toggle);
                        s.remeasure_item(&toggle);
                    },
                ))
                .child(
                    self.button(format!("detail-{id}"), label, cx, move |s, _, _| {
                        s.detail(key.0.clone(), key.1.clone());
                        s.remeasure_item(&key.0);
                    }),
                )
                .into_any_element();
        }
        let turn_id = turn.map(|turn| turn.id.clone()).unwrap_or_default();
        match item.body() {
            agent_protocol::items::ItemBody::AssistantText { .. } => {
                self.markdown(id, body_text.expect("assistant message has text"), cx)
            }
            agent_protocol::items::ItemBody::ImageGeneration { .. } => {
                let mut body = v_flex().gap_2().w_full();
                if projected.data.image_placeholder {
                    body = body.child(
                        skeleton::Skeleton::new()
                            .w_full()
                            .max_w(px(320.))
                            .h(px(320.))
                            .rounded_lg()
                            .bg(cx.theme().secondary),
                    );
                }
                for source in &projected.data.image_sources {
                    body = body.child(self.image(source, false, 320., true, cx));
                }
                body.when_some(body_text, |body, text| {
                    body.child(div().text_sm().child(text.to_owned()))
                })
                .into_any_element()
            }
            agent_protocol::items::ItemBody::UserMessage { text, content } => {
                let mut body = user_message_bubble();
                let mut has_body = false;
                if !content.is_empty() {
                    for (i, part) in content.iter().enumerate() {
                        use agent_protocol::items::MessagePart;
                        let rendered = match part {
                            MessagePart::Image { .. } => continue,
                            MessagePart::Text { text } if text.is_empty() => continue,
                            MessagePart::Text { text } => TextView::markdown(
                                SharedString::from(format!("{id}-{i}")),
                                literal(text),
                            )
                            .selectable(true)
                            .into_any_element(),
                            MessagePart::Attachment { name, path }
                            | MessagePart::Invocation { name, path } => {
                                let path = path.clone();
                                self.button(
                                    format!("{id}-{i}"),
                                    if path.is_empty() {
                                        name.clone()
                                    } else {
                                        file_name(&path)
                                    },
                                    cx,
                                    move |s, _, _| {
                                        if !path.is_empty() {
                                            s.download(path.clone());
                                        }
                                    },
                                )
                                .into_any_element()
                            }
                        };
                        has_body = true;
                        body = body.child(rendered);
                    }
                } else {
                    has_body = !text.as_deref().unwrap_or_default().is_empty();
                    body = body.child(
                        TextView::markdown(
                            SharedString::from(id.clone()),
                            literal(text.as_deref().unwrap_or_default()),
                        )
                        .selectable(true),
                    );
                }
                let body = self.user_message_content(
                    body,
                    has_body,
                    projected.data.image_sources.iter().map(String::as_str),
                    cx,
                );
                let text = body_text.expect("user message has text");
                let copied = text.to_owned();
                let edit = self.button(format!("edit-{id}"), "", cx, move |s, window, cx| {
                    selection::set_composer_text(&s.composer, copied.clone().into(), window, cx);
                });
                let timestamp = turn.and_then(|turn| turn.started_at.map(|time| time as i64));
                user_message_row(&id, text, body, timestamp, edit).into_any_element()
            }
            agent_protocol::items::ItemBody::CommandExecution {
                command, exit_code, ..
            } => {
                let label = title.expect("command has a title");
                let toggle = id.clone();
                let mut body = v_flex().gap_2().child(
                    self.button(
                        format!("expand-{id}"),
                        format!("{label} {}", if expanded { "⌄" } else { "›" }),
                        cx,
                        move |s, _, _| {
                            toggle_set(&mut s.expanded_items, &toggle);
                            if s.expanded_items.contains(&toggle) {
                                s.detail(turn_id.clone(), toggle.clone().into());
                            }
                            s.pause_tail();
                            s.remeasure_item(&toggle);
                        },
                    )
                    .icon(IconName::SquareTerminal)
                    .text_color(rgb(0xa0a0a0)),
                );
                if expanded {
                    let output = projected.expanded_body();
                    let copied = output.clone();
                    let content = format!("$ {}\n\n{output}", command);
                    body = body
                        .child(
                            h_flex()
                                .justify_between()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(0x999999))
                                        .child("プレーンテキスト"),
                                )
                                .child(
                                    Button::new(format!("copy-output-{id}"))
                                        .label("出力をコピー")
                                        .small()
                                        .ghost()
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copied.clone(),
                                            ))
                                        }),
                                ),
                        )
                        .child(Self::activity_text(format!("output-{id}"), &content, ""))
                        .child(div().text_sm().text_color(rgb(0x999999)).child(format!(
                            "{} {}",
                            item.status.label(),
                            exit_code.map(|v| format!("exit {v}")).unwrap_or_default()
                        )));
                }
                body.into_any_element()
            }
            agent_protocol::items::ItemBody::FileChange { .. } => {
                let mut body = v_flex().gap_3();
                for (i, change) in agent_core::presentation::body::file_changes(item).enumerate() {
                    let path = change.path.into_owned();
                    let toggle = id.clone();
                    let turn_id = turn_id.clone();
                    let selector = format!("change-{id}-{i}");
                    let row = h_flex()
                        .debug_selector(move || selector.clone())
                        .gap_2()
                        .child(self.button(
                            format!("change-{id}-{i}"),
                            format!("{} {}", if expanded { "⌄" } else { "›" }, file_name(&path)),
                            cx,
                            move |s, _, _| {
                                toggle_set(&mut s.expanded_items, &toggle);
                                if s.expanded_items.contains(&toggle) {
                                    s.detail(turn_id.clone(), toggle.clone().into());
                                }
                                s.pause_tail();
                                s.remeasure_item(&toggle);
                            },
                        ))
                        .child(self.button(
                            format!("download-{id}-{i}"),
                            "ダウンロード",
                            cx,
                            move |s, _, _| s.download(path.clone()),
                        ));
                    body = body.child(row);
                    if expanded {
                        if let Some(diff) = &change.diff {
                            body = body.child(Self::diff(
                                &mut self.diffs,
                                format!("diff-{id}-{i}"),
                                diff,
                                cx,
                            ));
                        }
                        if change.diff.is_none() || change.proposal.is_some() {
                            let details = agent_core::presentation::body::file_change_details(
                                None,
                                change.proposal,
                            );
                            body = body.child(Self::activity_text(
                                format!("file-details-{id}-{i}"),
                                &details,
                                "",
                            ));
                        }
                    }
                }
                body.into_any_element()
            }
            _ => {
                let toggle = id.clone();
                v_flex()
                    .gap_2()
                    .child(self.button(
                        format!("unknown-{id}"),
                        format!(
                            "{} {}",
                            if expanded { "⌄" } else { "›" },
                            title.expect("activity has a title")
                        ),
                        cx,
                        move |s, _, _| {
                            toggle_set(&mut s.expanded_items, &toggle);
                            if s.expanded_items.contains(&toggle) {
                                s.detail(turn_id.clone(), toggle.clone().into());
                            }
                            s.pause_tail();
                            s.remeasure_item(&toggle);
                        },
                    ))
                    .when(expanded, |body| {
                        body.child(Self::activity_text(
                            format!("json-{id}"),
                            &projected.expanded_body(),
                            "json",
                        ))
                    })
                    .into_any_element()
            }
        }
    }
    pub(super) fn turn(
        &mut self,
        session: Option<&SessionRef>,
        projected: &Arc<agent_core::presentation::conversation::RenderedTurn>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let turn = &projected.source;
        let mut body = v_flex().w_full().max_w(px(CHAT_WIDTH)).gap_4();
        let mut expanded = false;
        for row in &projected.rows {
            match &row.content {
                ConversationRowContent::User { item } => {
                    body = body.child(self.projected_item(item, turn, cx));
                }
                ConversationRowContent::ActivityHeader { activity } => {
                    let id = &activity.id;
                    expanded = activity_is_expanded(activity, self.expanded_work.get(id).cloned());
                    let label = if activity.is_in_progress {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0., |duration| duration.as_secs_f64());
                        projected.progress_label(!expanded, now)
                    } else {
                        activity.activity_summary.clone()
                    };
                    let header = if activity.activity_can_collapse {
                        let toggle = id.clone();
                        let status = activity.status.clone();
                        let turn_id = turn.id.clone();
                        self.button(
                            format!("work-{id}"),
                            format!("{label} {}", if expanded { "⌄" } else { "›" }),
                            cx,
                            move |view, _, _| {
                                view.expanded_work.insert(
                                    toggle.clone(),
                                    ActivityExpansion {
                                        status: status.clone(),
                                        expanded: !expanded,
                                    },
                                );
                                view.pause_tail();
                                view.remeasure_item(&turn_id);
                            },
                        )
                        .text_color(rgb(0xa0a0a0))
                        .into_any_element()
                    } else {
                        div()
                            .text_color(rgb(0xa0a0a0))
                            .child(label.clone())
                            .into_any_element()
                    };
                    let selector = format!("work-{id}");
                    body = body.child(
                        h_flex()
                            .debug_selector(move || selector.clone())
                            .gap_2()
                            .child(header)
                            .when(activity.is_in_progress, |row| {
                                row.child(spinner::Spinner::new().small())
                            }),
                    );
                }
                ConversationRowContent::Activity { item, .. } => {
                    if expanded {
                        body = body.child(self.projected_item(item, turn, cx));
                    }
                }
                ConversationRowContent::PendingRequest { request } => {
                    body = body.child(self.request_card(request, cx));
                }
                ConversationRowContent::Error { error } => {
                    body = body.child(div().text_color(rgb(0xff8e86)).child(error.message.clone()));
                }
                ConversationRowContent::Response { item, fork_turn_id } => {
                    body = body.child(self.projected_item(item, turn, cx));
                    if item.data.kind == "agent" {
                        let text = item.data.body.clone().expect("assistant message has text");
                        body = body.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new(format!("copy-{}", item.data.id))
                                        .icon(IconName::Copy)
                                        .small()
                                        .ghost()
                                        .tooltip("回答をコピー")
                                        .accessibility_label("回答をコピー")
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                text.clone(),
                                            ));
                                        }),
                                )
                                .when_some(
                                    session.cloned().zip(fork_turn_id.clone()),
                                    |actions, (thread_id, turn_id)| {
                                        actions.child(
                                            Button::new(format!("fork-{}", item.data.id))
                                                .small()
                                                .ghost()
                                                .tooltip("ここから会話を分岐")
                                                .accessibility_label("ここから会話を分岐")
                                                .on_click(cx.listener(move |view, _, _, cx| {
                                                    if !view.snapshot.connected || view.busy > 0 {
                                                        return;
                                                    }
                                                    view.busy += 1;
                                                    view.perform(
                                                        Intent::ForkSession(op::ForkSession::new(
                                                            thread_id.clone(),
                                                            turn_id.clone(),
                                                        )),
                                                        OperationCompletion::Busy,
                                                    );
                                                    cx.notify();
                                                }))
                                                .icon(Icon::default().path("bex/branch.svg"))
                                                .debug_selector(|| "response-fork".into())
                                                .disabled(
                                                    !self.snapshot.connected || self.busy > 0,
                                                ),
                                        )
                                    },
                                ),
                        );
                    }
                }
                // Desktop pages via its virtual list and offers Stop in the composer.
                ConversationRowContent::InProgress { .. } => {}
            }
        }
        let selector = format!("conversation-turn-{}", turn.id);
        h_flex()
            .debug_selector(move || selector.clone())
            .justify_center()
            .w_full()
            .px_6()
            .pb_8()
            .child(body)
            .into_any_element()
    }
    pub(super) fn projected_item(
        &mut self,
        item: &agent_core::presentation::conversation::RenderedItem,
        turn: &Arc<Turn>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &item.source {
            agent_core::presentation::conversation::ItemSource::Native(native) => {
                self.item(item, native, Some(turn), cx)
            }
            agent_core::presentation::conversation::ItemSource::Pending(id, pending) => {
                self.pending_item(id, pending, cx)
            }
        }
    }
    pub(super) fn pending_item(
        &mut self,
        id: &str,
        pending: &agent_core::state::PendingSubmission,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let draft = &pending.draft;
        let mut body = user_message_bubble();
        if !draft.text.is_empty() {
            body = body.child(
                TextView::markdown(
                    SharedString::from(format!("pending-{id}")),
                    literal(&draft.text),
                )
                .selectable(true),
            );
        }
        for (index, attachment) in draft.attachments.iter().enumerate() {
            if !attachment.is_image {
                let path = attachment.path.clone();
                body = body.child(self.button(
                    format!("pending-{id}-file-{index}"),
                    attachment.name.clone(),
                    cx,
                    move |view, _, _| view.download(path.clone()),
                ));
            }
        }
        let body = self.user_message_content(
            body,
            !draft.text.is_empty() || draft.attachments.iter().any(|file| !file.is_image),
            draft
                .attachments
                .iter()
                .filter(|file| file.is_image)
                .map(|file| file.path.as_str()),
            cx,
        );
        h_flex()
            .w_full()
            .justify_end()
            .my_4()
            .child(
                v_flex().min_w_0().gap_2().child(body).child(
                    div()
                        .text_sm()
                        .text_color(rgb(0x999999))
                        .child(pending.delivery_label()),
                ),
            )
            .into_any_element()
    }

    fn user_message_content<'a>(
        &mut self,
        body: Div,
        has_body: bool,
        sources: impl Iterator<Item = &'a str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let images: Vec<_> = sources
            .map(|source| {
                div()
                    .w(px(80.))
                    .h(px(80.))
                    .flex_shrink_0()
                    .rounded_lg()
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgb(0x444444))
                    .debug_selector(|| "user-image-thumbnail".into())
                    .child(self.image(source, false, 80., true, cx))
            })
            .collect();
        v_flex()
            .min_w_0()
            .max_w(px(560.))
            .items_end()
            .gap_2()
            .when(!images.is_empty(), |column| {
                column.child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .flex_wrap()
                        .max_w(px(560.))
                        .children(images),
                )
            })
            .when(has_body, |column| column.child(body))
    }

    pub(super) fn request_card(
        &mut self,
        request: &agent_core::presentation::conversation::Request,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use agent_protocol::requests::{Answer, ElicitationAnswer, ElicitationInput, RequestBody};
        let id = &request.id;
        let key = id;
        let Some(inputs) = self.requests.get(key) else {
            return div().into_any_element();
        };
        let mut body = v_flex()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(0x30312a))
            .child(request.title.clone())
            .child(request.body.clone());
        if !request.details.is_empty() {
            body = body.child(
                TextView::markdown(
                    SharedString::from(format!("request-{key}")),
                    fenced(&request.details, "json"),
                )
                .selectable(true),
            );
        }
        if inputs.sent || !request.can_respond {
            return body.into_any_element();
        }
        match &request.request_body {
            RequestBody::Question { .. } => {
                for question in &inputs.questions {
                    body = body.child(question.definition.prompt.clone());
                    for option in &question.definition.choices {
                        let request_key = key.to_owned();
                        let question_id = question.definition.id.clone();
                        let choice_id = option.id.clone();
                        let selected = question.selected.contains(&option.id);
                        body = body.child(self.button(
                            format!("answer-{key}-{}-{}", question_id, option.id),
                            format!("{}{}", if selected { "✓ " } else { "" }, option.label),
                            cx,
                            move |view, window, cx| {
                                let Some(question) =
                                    view.requests.get_mut(&request_key).and_then(|inputs| {
                                        inputs
                                            .questions
                                            .iter_mut()
                                            .find(|question| question.definition.id == question_id)
                                    })
                                else {
                                    return;
                                };
                                if question.definition.multiple {
                                    toggle_set(&mut question.selected, &choice_id);
                                } else {
                                    question.selected.clear();
                                    question.selected.insert(choice_id.clone());
                                }
                                question
                                    .input
                                    .update(cx, |input, cx| input.set_value("", window, cx));
                            },
                        ));
                        if !option.description.is_empty() {
                            body = body.child(option.description.clone());
                        }
                    }
                    if question.definition.allow_free_text {
                        body = body.child(Input::new(&question.input));
                    }
                }
                let key = key.to_owned();
                let id = id.clone();
                body = body.child(self.button(
                    format!("respond-{key}"),
                    "回答を送信",
                    cx,
                    move |view, _, cx| {
                        let Some(inputs) = view.requests.get(&key) else {
                            return;
                        };
                        let answers = inputs
                            .questions
                            .iter()
                            .map(|question| {
                                (
                                    question.definition.id.clone(),
                                    agent_core::presentation::conversation::build_question_answer(
                                        question.definition.multiple,
                                        question.input.read(cx).value().to_string(),
                                        question.selected.iter().cloned().collect(),
                                    ),
                                )
                            })
                            .collect();
                        view.respond(id.clone(), Answer::Questions { answers });
                    },
                ));
            }
            RequestBody::Approval { choices, .. } | RequestBody::Permission { choices, .. } => {
                let permission = matches!(request.request_body, RequestBody::Permission { .. });
                let mut row = h_flex().gap_2().flex_wrap();
                for choice in choices {
                    let key = key.to_owned();
                    let id = id.clone();
                    let choice_id = choice.id.clone();
                    row = row.child(self.button(
                        format!("decision-{key}-{}", choice.id),
                        choice.label.clone(),
                        cx,
                        move |view, _, _| {
                            view.respond(
                                id.clone(),
                                if permission {
                                    Answer::Permission {
                                        choice_id: choice_id.clone(),
                                    }
                                } else {
                                    Answer::Approval {
                                        choice_id: choice_id.clone(),
                                    }
                                },
                            )
                        },
                    ));
                    if !choice.description.is_empty() {
                        body = body.child(choice.description.clone());
                    }
                }
                body = body.child(row);
            }
            RequestBody::Elicitation { input, .. } => {
                if let ElicitationInput::Url { url } = input {
                    let url = url.clone();
                    body = body.child(self.button(
                        format!("elicitation-url-{key}"),
                        "リンクを開く",
                        cx,
                        move |_, _, cx| cx.open_url(&url),
                    ));
                } else {
                    body = body.child(Textarea::new(&inputs.response));
                }
                let accept_key = key.to_owned();
                let accept_id = id.clone();
                let request_body = request.request_body.clone();
                body = body.child(self.button(
                    format!("elicitation-accept-{accept_key}"),
                    "確認して送信",
                    cx,
                    move |view, _, cx| {
                        let Some(inputs) = view.requests.get(&accept_key) else {
                            return;
                        };
                        match agent_core::presentation::conversation::answer_from_json(
                            &request_body,
                            &inputs.response.read(cx).value(),
                        ) {
                            Ok(answer) => view.respond(accept_id.clone(), answer),
                            Err(error) => view.set_error(error),
                        }
                    },
                ));
                for (action, label) in [
                    (ElicitationAnswer::Decline, "辞退"),
                    (ElicitationAnswer::Cancel, "キャンセル"),
                ] {
                    let key = key.to_owned();
                    let id = id.clone();
                    body = body.child(self.button(
                        format!("elicitation-{label}-{key}"),
                        label,
                        cx,
                        move |view, _, _| {
                            view.respond(
                                id.clone(),
                                Answer::Elicitation {
                                    action: action.clone(),
                                },
                            )
                        },
                    ));
                }
            }
            RequestBody::ToolExecution { .. } => {
                let key = key.to_owned();
                let id = id.clone();
                let request_body = request.request_body.clone();
                body = body
                    .child(Textarea::new(&inputs.response))
                    .child(self.button(
                        format!("tool-answer-{key}"),
                        "実行結果を送信",
                        cx,
                        move |view, _, cx| {
                            let Some(inputs) = view.requests.get(&key) else {
                                return;
                            };
                            match agent_core::presentation::conversation::answer_from_json(
                                &request_body,
                                &inputs.response.read(cx).value(),
                            ) {
                                Ok(answer) => view.respond(id.clone(), answer),
                                Err(error) => view.set_error(error),
                            }
                        },
                    ));
            }
        }
        h_flex()
            .w_full()
            .justify_center()
            .px_6()
            .pb_5()
            .child(body.max_w(px(CHAT_WIDTH)))
            .into_any_element()
    }
    pub(in crate::app) fn sync_request_inputs(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.requests
            .retain(|key, _| self.snapshot.request(key).is_some());
        for request in self.snapshot.requests() {
            let key = &request.id;
            if self.requests.contains_key(key) {
                continue;
            }
            let questions = match &request.body {
                agent_protocol::requests::RequestBody::Question { questions } => questions
                    .iter()
                    .map(|question| Question {
                        definition: question.clone(),
                        selected: HashSet::new(),
                        input: cx.new(|cx| {
                            InputState::new(window, cx)
                                .placeholder("回答を入力")
                                .masked(question.secret)
                        }),
                    })
                    .collect(),
                _ => Vec::new(),
            };
            self.requests.insert(
                key.clone(),
                RequestInputs {
                    questions,
                    response: cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .default_value(
                                agent_core::presentation::conversation::request_input_default(
                                    request.body.clone(),
                                ),
                            )
                            .auto_grow(3, 8)
                    }),
                    sent: false,
                },
            );
        }
    }
}

fn user_message_row(
    id: &str,
    text: &str,
    body: Div,
    timestamp: Option<i64>,
    edit: Button,
) -> impl IntoElement {
    let time = timestamp
        .and_then(|value| chrono::DateTime::from_timestamp(value, 0))
        .map(|value| {
            value
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string()
        });
    let copy_text = text.to_owned();
    let group = SharedString::from(format!("user-message-{id}"));
    h_flex().w_full().justify_end().my_4().child(
        v_flex()
            .min_w_0()
            .max_w_full()
            .group(group.clone())
            .items_end()
            .gap_1()
            .child(body.w_full())
            .child(
                h_flex()
                    .debug_selector(|| "user-message-actions".into())
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .children(
                        time.map(|time| {
                            div().text_xs().text_color(rgb(0x999999)).mr_2().child(time)
                        }),
                    )
                    .child(
                        Button::new(format!("copy-{id}"))
                            .debug_selector(|| "user-message-copy".into())
                            .icon(IconName::Copy)
                            .small()
                            .ghost()
                            .tooltip("発言をコピー")
                            .accessibility_label("発言をコピー")
                            .disabled(copy_text.is_empty())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                            }),
                    )
                    .child(
                        edit.icon(Icon::default().path("bex/pencil.svg"))
                            .small()
                            .ghost()
                            .debug_selector(|| "user-message-edit".into())
                            .tooltip("入力欄で編集")
                            .accessibility_label("入力欄で編集")
                            .disabled(text.is_empty()),
                    ),
            ),
    )
}

#[cfg(test)]
mod tests {
    use super::{user_message_bubble, user_message_row};
    use gpui_kit as gpui;
    use gpui_kit::{
        ClipboardItem, Context, IntoElement, Modifiers, ParentElement, Render, TestAppContext,
        Window,
    };

    struct Message;
    impl Render for Message {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            user_message_row(
                "test",
                "一行目\nsecond line",
                user_message_bubble().child("一行目\nsecond line"),
                Some(1),
                gpui::component::button::Button::new("edit"),
            )
        }
    }

    #[gpui::test]
    fn hovering_own_message_allows_copying_the_complete_text(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let (_, cx) = cx.add_window_view(|_, _| Message);
        cx.run_until_parked();
        let bounds = cx.debug_bounds("user-message-actions").unwrap();
        cx.update(|_, cx| cx.write_to_clipboard(ClipboardItem::new_string("previous".into())));
        cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        cx.run_until_parked();
        let copy = cx.debug_bounds("user-message-copy").unwrap().center();
        cx.simulate_click(copy, Modifiers::default());
        cx.update(|_, cx| {
            assert_eq!(
                cx.read_from_clipboard().unwrap().text().unwrap(),
                "一行目\nsecond line"
            );
        });
    }
}

#[cfg(test)]
mod rendering_tests {
    use super::{Desktop, Mode, Snapshot};
    use crate::app::ImageState;
    use agent_core::presentation::conversation::{RenderedConversation, project_conversation};
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, IntoElement, Render, RenderImage, TestAppContext, Window, px,
        size,
    };
    use std::sync::Arc;

    struct ConversationView {
        desktop: Entity<Desktop>,
        conversation: Arc<RenderedConversation>,
    }
    impl Render for ConversationView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.desktop.update(cx, |desktop, cx| {
                desktop.turn(
                    self.conversation.source.id.as_ref(),
                    &self.conversation.turns[0],
                    cx,
                )
            })
        }
    }

    fn init(cx: &mut TestAppContext) -> tokio::runtime::Runtime {
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
        runtime
    }

    impl ConversationView {
        fn new(
            snapshot: Snapshot,
            source: agent_protocol::models::Thread,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            Self {
                desktop: cx.new(|cx| {
                    Desktop::new(
                        Mode::SideChat {
                            // Reject before reading preferences or connecting to a real Host.
                            remote: Some(agent_protocol::models::RemoteHost {
                                id: "fixture".into(),
                                name: "fixture".into(),
                                ticket: "invalid-fixture-ticket".into(),
                            }),
                            cwd: "/fixture".into(),
                        },
                        window,
                        cx,
                    )
                }),
                conversation: project_conversation(&snapshot, Arc::new(source), &None),
            }
        }
    }

    #[gpui::test]
    fn completed_response_offers_fork_but_streaming_response_does_not(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        for status in ["completed", "running"] {
            let source = serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"fixture"},"capabilities":{"additionalInput":true,"fork":true,"rename":true,"modelChange":true},"turns":[{"id":"turn","status":status,"items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"Answer","phase":"final"}}}}}]}]}))
            .unwrap();
            let (_, window) = cx.add_window_view(|window, cx| {
                ConversationView::new(Snapshot::default(), source, window, cx)
            });
            window.run_until_parked();
            assert_eq!(
                window.debug_bounds("response-fork").is_some(),
                status == "completed"
            );
        }
    }

    struct ChatNavigationView(Entity<Desktop>);
    impl Render for ChatNavigationView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |desktop, cx| desktop.chat(cx))
        }
    }

    #[gpui::test]
    fn conversation_navigation_returns_to_latest_and_resumes_following(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let source = serde_json::from_value(serde_json::json!({
            "id": {"provider":"codex","id":"fixture"}, "turns": (0..20).map(|i| serde_json::json!({
                "id": format!("turn-{i}"), "status": "completed", "items": [
                    {"id": format!("user-{i}"), "body": {"inline":{"body":{"userMessage":{"text":format!("Question {i}"),"content":[]}}}}},
                    {"id": format!("answer-{i}"), "body":{"inline":{"body":{"assistantText":{"text":"A long answer\n".repeat(10),"phase":"final"}}}}}
                ]
            })).collect::<Vec<_>>()
        })).unwrap();
        let (view, window) = cx.add_window_view(|window, cx| {
            let fixture =
                cx.new(|cx| ConversationView::new(Snapshot::default(), source, window, cx));
            let fixture = ConversationView {
                desktop: fixture.read(cx).desktop.clone(),
                conversation: fixture.read(cx).conversation.clone(),
            };
            fixture.desktop.update(cx, |desktop, _| {
                let snapshot = std::sync::Arc::make_mut(&mut desktop.snapshot);
                Arc::make_mut(&mut snapshot.navigation).thread_id =
                    Some(agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "fixture".into(),
                    });
                std::sync::Arc::make_mut(&mut snapshot.conversations).insert(
                    agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "fixture".into(),
                    },
                    fixture.conversation.source.clone(),
                );
                desktop.rendered = Some(fixture.conversation.clone());
                desktop.rows = fixture
                    .conversation
                    .turns
                    .iter()
                    .cloned()
                    .map(super::super::super::ConversationRow::Turn)
                    .collect();
                desktop.list.reset(desktop.rows.len());
                desktop.list.set_follow_mode(gpui_kit::FollowMode::Tail);
            });
            ChatNavigationView(fixture.desktop)
        });
        window.simulate_resize(size(px(900.), px(650.)));
        window.run_until_parked();
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        assert!(window.debug_bounds("conversation-marker-0").is_some());
        assert!(window.debug_bounds("conversation-marker-19").is_some());
        assert!(
            window.debug_bounds("conversation-marker-1").is_none(),
            "Long histories must have at most eight evenly spaced markers"
        );
        let first = window.debug_bounds("conversation-marker-0").unwrap();
        let marker_left = window
            .debug_bounds("conversation-marker-line-0")
            .unwrap()
            .left();
        assert_eq!(
            marker_left - first.left(),
            px(16.5),
            "Preserve the original resting marker position"
        );
        let marker_width = window
            .debug_bounds("conversation-marker-line-0")
            .unwrap()
            .size
            .width;
        window.simulate_mouse_move(first.center(), None, gpui_kit::Modifiers::default());
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        let hovered_width = window
            .debug_bounds("conversation-marker-line-0")
            .unwrap()
            .size
            .width;
        let neighbor_width = window
            .debug_bounds("conversation-marker-line-2")
            .unwrap()
            .size
            .width;
        let next_width = window
            .debug_bounds("conversation-marker-line-5")
            .unwrap()
            .size
            .width;
        for selector in [
            "conversation-marker-line-0",
            "conversation-marker-line-2",
            "conversation-marker-line-5",
        ] {
            assert_eq!(
                window.debug_bounds(selector).unwrap().left(),
                marker_left,
                "Hover must keep the left edge fixed and expand only to the right"
            );
        }
        assert!(hovered_width >= px(28.));
        assert!(
            hovered_width > neighbor_width
                && neighbor_width > next_width
                && next_width > marker_width
        );
        window.simulate_mouse_move(
            gpui_kit::point(px(800.), px(30.)),
            None,
            gpui_kit::Modifiers::default(),
        );
        window.run_until_parked();
        assert_eq!(
            window
                .debug_bounds("conversation-marker-line-2")
                .unwrap()
                .size
                .width,
            marker_width
        );
        window.simulate_click(first.center(), gpui_kit::Modifiers::default());
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        let latest = window.debug_bounds("conversation-latest").unwrap();
        view.update(window, |view, cx| {
            assert!(!view.0.read(cx).list.is_following_tail());
            assert_eq!(view.0.read(cx).list.logical_scroll_top().item_ix, 0);
        });
        window.simulate_click(latest.center(), gpui_kit::Modifiers::default());
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        let last = window.debug_bounds("conversation-turn-turn-19").unwrap();
        view.update(window, |view, cx| {
            let desktop = view.0.read(cx);
            assert!(desktop.list.is_following_tail());
            assert!((last.bottom() - desktop.list.viewport_bounds().bottom()).abs() < px(1.));
        });
        assert!(window.debug_bounds("conversation-latest").is_none());
    }

    #[gpui::test]
    fn file_changes_render_and_expand_in_the_desktop_view(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let source = serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"fixture"},"turns":[{"id":"turn","status":"completed","items":[{"id":"files","status":"completed","clientInputId":null,"body":{"inline":{"body":{"fileChange":{"changes":[{"path":"/fixture/a.txt","kind":{"update":{"movePath":null}},"diff":"@@ -1 +1 @@\n-old\n+new","proposal":null},{"path":"/fixture/b.txt","kind":"add","diff":"@@ -0,0 +1 @@\n+second","proposal":null}],"output":""}}}}}]}]}))
        .unwrap();
        let (view, window) = cx.add_window_view(|window, cx| {
            ConversationView::new(Snapshot::default(), source, window, cx)
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("change-files-0").is_none(),
            "completed work starts collapsed"
        );
        let group = window.debug_bounds("work-turn").unwrap();
        window.simulate_click(
            gpui_kit::point(group.left() + px(20.), group.center().y),
            gpui_kit::Modifiers::default(),
        );
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        for selector in ["change-files-0", "change-files-1"] {
            assert!(
                window.debug_bounds(selector).is_some(),
                "file header must be visible"
            );
        }
        let before = window.debug_bounds("change-files-1").unwrap().top();
        let bounds = window.debug_bounds("change-files-0").unwrap();
        window.simulate_click(
            gpui_kit::point(bounds.left() + px(20.), bounds.center().y),
            gpui_kit::Modifiers::default(),
        );
        view.update(window, |_, cx| {
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("change-files-1").unwrap().top() > before,
            "expanded diff must occupy space in the rendered conversation"
        );
    }
    #[gpui::test]
    fn chat_images_stay_inside_the_bubble_at_different_window_sizes(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        for pending in [false, true] {
            for (width, height) in [(1156, 78), (78, 1156), (400, 400)] {
                let image = Arc::new(RenderImage::new(vec![image::Frame::new(
                    image::RgbaImage::new(width, height),
                )]));
                let items = if pending {
                    serde_json::json!([])
                } else {
                    serde_json::json!([{"id":"user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":null,"content":[{"text":{"text":"caption"}},{"image":{"source":"/fixture/image.png"}}]}}}}}])
                };
                let mut snapshot = Snapshot::default();
                if pending {
                    Arc::make_mut(&mut snapshot.pending_submissions).insert(
                        "pending".into(),
                        Arc::new(agent_core::state::PendingSubmission {
                            sequence: 0,
                            draft_key: agent_protocol::session::SessionRef {
                                provider: agent_protocol::session::ProviderKind::Codex,
                                id: "fixture".into(),
                            }
                            .into(),
                            draft: Arc::new(agent_core::state::Draft {
                                text: "caption".into(),
                                attachments: vec![agent_core::state::Attachment {
                                    path: "/fixture/image.png".into(),
                                    name: "image.png".into(),
                                    is_image: true,
                                }],
                                ..Default::default()
                            }),
                            turn_id: Some("turn".into()),
                            after_item_id: None,
                            accepted: true,
                            delivery_unknown: false,
                        }),
                    );
                }
                let source = serde_json::from_value(serde_json::json!({
                    "id":{"provider":"codex","id":"fixture"}, "turns":[{"id":"turn", "status":"completed", "items":items}]
                }))
                .unwrap();
                let (view, window) = cx.add_window_view(|window, cx| {
                    let view = ConversationView::new(snapshot, source, window, cx);
                    view.desktop.update(cx, |desktop, _| {
                        let source = "/fixture/image.png".to_owned();
                        let key = format!(
                            "{}:{}:false:{}",
                            desktop.remote_key(),
                            desktop.snapshot.navigation.cwd,
                            source
                        );
                        desktop.images.insert(
                            key,
                            ImageState {
                                source: Arc::new(source),
                                path: Some(image.into()),
                                error: None,
                            },
                        );
                    });
                    view
                });
                for viewport_width in [780_f32, 360.] {
                    window.simulate_resize(size(px(viewport_width), px(600.)));
                    window.run_until_parked();
                    view.update(window, |_, cx| cx.notify());
                    window.run_until_parked();
                    let thumbnail = window.debug_bounds("user-image-thumbnail").unwrap();
                    let bubble = window.debug_bounds("user-message-bubble").unwrap();
                    assert_eq!(thumbnail.size, size(px(80.), px(80.)));
                    assert!(thumbnail.bottom() <= bubble.top());
                    assert_eq!(thumbnail.right(), bubble.right());
                    let bounds = window
                        .debug_bounds("chat-image")
                        .expect("image must render");
                    assert!(
                        bounds.size.width > px(0.),
                        "image must remain visible: {bounds:?}"
                    );
                    assert!(
                        bounds.size.width <= px(528_f32.min(viewport_width - 32.)),
                        "image exceeds bubble: {bounds:?}"
                    );
                    assert!(
                        bounds.left() >= px(0.) && bounds.right() <= px(viewport_width),
                        "image overflows conversation: {bounds:?}"
                    );
                }
            }
        }
    }
}

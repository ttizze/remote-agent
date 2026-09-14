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
                    ConversationRowContent::User { item } => Some((index, &item.data.body, turn)),
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
                    ConversationRowContent::Response { item, .. } => {
                        Some(item.data.body.chars().take(240).collect::<String>())
                    }
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
        let id = item.id.clone();
        let kind = item.kind.as_deref().unwrap_or_default();
        let expanded = self.expanded_items.contains(&id);
        let deferred = projected.data.deferred;
        if expanded && deferred {
            let key = (
                turn.map(|turn| turn.id.clone()).unwrap_or_default(),
                id.clone(),
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
            let title = projected.data.title.clone();
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
        match kind {
            "agentMessage" => self.markdown(id, &projected.data.body, cx),
            "imageGeneration" => {
                let path = projected
                    .data
                    .image_sources
                    .first()
                    .map(String::as_str)
                    .unwrap_or_default();
                let title = projected.data.title.clone();
                let mut body = v_flex()
                    .gap_2()
                    .w_full()
                    .child(div().text_sm().child(title));
                if !path.is_empty() {
                    body = body.child(self.image(path, false, 320., true, cx));
                    let path = path.to_owned();
                    body = body.child(self.button(
                        format!("open-image-{id}"),
                        "画像を開く",
                        cx,
                        move |s, _, cx| {
                            s.open_image_gallery(std::sync::Arc::new(path.clone()), false, cx)
                        },
                    ));
                } else if item.status.as_deref().unwrap_or_default() == "inProgress" {
                    body = body.child(spinner::Spinner::new().small());
                }
                body.into_any_element()
            }
            "userMessage" => {
                let mut body = user_message_bubble();
                let mut images = projected.data.image_sources.iter();
                if extra(item, "content").is_array() {
                    for (i, part) in array(extra(item, "content")).iter().enumerate() {
                        body = body.child(match text(part, "type") {
                            "text" => TextView::markdown(
                                SharedString::from(format!("{id}-{i}")),
                                literal(text(part, "text")),
                            )
                            .selectable(true)
                            .into_any_element(),
                            "localImage" | "image" => images
                                .next()
                                .map(|source| self.image(source, false, 320., true, cx))
                                .unwrap_or_else(|| div().into_any_element()),
                            _ => {
                                let path = text(part, "path").to_owned();
                                self.button(
                                    format!("{id}-{i}"),
                                    if path.is_empty() {
                                        part.to_string()
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
                        });
                    }
                } else {
                    body = body.child(
                        TextView::markdown(
                            SharedString::from(id.clone()),
                            literal(item.text.as_deref().unwrap_or_default()),
                        )
                        .selectable(true),
                    );
                }
                let text = projected.data.body.clone();
                let edit = self.button(format!("edit-{id}"), "", cx, move |s, window, cx| {
                    selection::set_composer_text(&s.composer, text.clone().into(), window, cx);
                });
                let timestamp = turn.and_then(|turn| turn.started_at.as_ref()?.as_ref()?.as_i64());
                user_message_row(&id, &projected.data.body, body, timestamp, edit)
                    .into_any_element()
            }
            "commandExecution" => {
                let label = projected.data.title.clone();
                let toggle = id.clone();
                let mut body = v_flex().gap_2().child(
                    self.button(
                        format!("expand-{id}"),
                        format!("{label} {}", if expanded { "⌄" } else { "›" }),
                        cx,
                        move |s, _, _| {
                            toggle_set(&mut s.expanded_items, &toggle);
                            if s.expanded_items.contains(&toggle) {
                                s.detail(turn_id.clone(), toggle.clone());
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
                    let content = format!(
                        "$ {}\n\n{output}",
                        item.command.as_deref().unwrap_or_default()
                    );
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
                                item.status.as_deref().unwrap_or_default(),
                                item.extra.get("exitCode")
                                    .map(|v| format!("exit {v}"))
                                    .unwrap_or_default()
                            )));
                }
                body.into_any_element()
            }
            "fileChange" => {
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
                                    s.detail(turn_id.clone(), toggle.clone());
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
                        body = body.child(Self::diff(
                            &mut self.diffs,
                            format!("diff-{id}-{i}"),
                            &change.diff,
                            cx,
                        ));
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
                            projected.data.title.clone()
                        ),
                        cx,
                        move |s, _, _| {
                            toggle_set(&mut s.expanded_items, &toggle);
                            if s.expanded_items.contains(&toggle) {
                                s.detail(turn_id.clone(), toggle.clone());
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
                    body = body.child(self.request_card(&request.key, request, cx));
                }
                ConversationRowContent::Error { error } => {
                    body = body.child(div().text_color(rgb(0xff8e86)).child(error.message.clone()));
                }
                ConversationRowContent::Response { item, .. } => {
                    body = body.child(self.projected_item(item, turn, cx));
                    if item.data.kind == "agent" {
                        let item = item.clone();
                        body = body.child(
                            h_flex().child(
                                Button::new(format!("copy-{}", item.data.id))
                                    .icon(IconName::Copy)
                                    .small()
                                    .ghost()
                                    .tooltip("回答をコピー")
                                    .accessibility_label("回答をコピー")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            item.data.body.clone(),
                                        ));
                                    }),
                            ),
                        );
                    }
                }
                // Desktop pages via its virtual list and offers Stop in the composer.
                ConversationRowContent::OlderItems { .. }
                | ConversationRowContent::InProgress { .. } => {}
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
                self.pending_item(id, &pending.draft, cx)
            }
        }
    }
    pub(super) fn pending_item(
        &mut self,
        id: &str,
        draft: &Draft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
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
            if attachment.is_image {
                body = body.child(self.image(&attachment.path, false, 320., true, cx));
            } else {
                let path = attachment.path.clone();
                body = body.child(self.button(
                    format!("pending-{id}-file-{index}"),
                    attachment.name.clone(),
                    cx,
                    move |view, _, _| view.download(path.clone()),
                ));
            }
        }
        h_flex()
            .w_full()
            .justify_end()
            .my_4()
            .child(body)
            .into_any_element()
    }

    pub(super) fn request_card(
        &mut self,
        key: &str,
        request: &agent_core::presentation::conversation::Request,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = &request.id;
        let params = &request.params;
        let Some(inputs) = self.requests.get(key) else {
            return div().into_any_element();
        };
        let mut body = v_flex()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(rgb(0x30312a))
            .child(request.title.clone())
            .child(request.body.clone())
            .child(
                TextView::markdown(
                    SharedString::from(format!("request-{key}")),
                    fenced(
                        &serde_json::to_string_pretty(params).unwrap_or_default(),
                        "json",
                    ),
                )
                .selectable(true),
            );
        if inputs.sent {
            return body
                .child("回答を送信しました。Host の確認を待っています。")
                .into_any_element();
        }
        if !inputs.questions.is_empty() {
            for question in &inputs.questions {
                body = body.child(question.prompt.clone());
                for (index, option) in question.options.iter().enumerate() {
                    let input = question.input.clone();
                    let value = option.clone();
                    body = body.child(self.button(
                        format!("answer-{key}-{}-{index}", question.id),
                        option.clone(),
                        cx,
                        move |_, window, cx| {
                            input.update(cx, |input, cx| input.set_value(value.clone(), window, cx))
                        },
                    ));
                }
                body = body.child(Input::new(&question.input));
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
                                question.id.clone(),
                                question.input.read(cx).value().to_string(),
                            )
                        })
                        .collect();
                    view.respond(key.clone(), id.clone(), Answer::Questions { answers });
                },
            ));
        } else if matches!(
            request.kind,
            agent_core::presentation::conversation::RequestKind::CommandApproval
                | agent_core::presentation::conversation::RequestKind::FileApproval
        ) {
            let mut row = h_flex().gap_2().flex_wrap();
            for (index, label) in request.decision_labels.iter().enumerate() {
                let key = key.to_owned();
                let id = id.clone();
                row = row.child(self.button(
                    format!("decision-{key}-{index}"),
                    label.clone(),
                    cx,
                    move |view, _, _| {
                        view.respond(
                            key.clone(),
                            id.clone(),
                            Answer::Decision {
                                index: index as u32,
                            },
                        )
                    },
                ));
            }
            body = body.child(row);
        } else if matches!(
            request.kind,
            agent_core::presentation::conversation::RequestKind::Permissions
        ) {
            for (allow, label) in [(true, "今回の権限を許可"), (false, "拒否")] {
                let key = key.to_owned();
                let id = id.clone();
                body = body.child(self.button(
                    format!("permissions-{key}-{allow}"),
                    label,
                    cx,
                    move |view, _, _| {
                        view.respond(key.clone(), id.clone(), Answer::Permissions { allow })
                    },
                ));
            }
        } else {
            let key = key.to_owned();
            let id = id.clone();
            body = body
                .child("要求の形式に合わせて回答JSONを入力")
                .child(Textarea::new(&inputs.raw))
                .child(self.button(
                    format!("raw-{key}"),
                    "回答を送信",
                    cx,
                    move |view, _, cx| {
                        let Some(inputs) = view.requests.get(&key) else {
                            return;
                        };
                        match serde_json::from_str::<Value>(&inputs.raw.read(cx).value()) {
                            Ok(value) => {
                                view.respond(key.clone(), id.clone(), Answer::Raw { value })
                            }
                            Err(error) => view.set_error(error.to_string()),
                        }
                    },
                ));
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
            .retain(|key, _| self.snapshot.requests.contains_key(key));
        for (key, request) in self.snapshot.requests.iter() {
            if self.requests.contains_key(key) {
                continue;
            }
            let questions = array(field(&request.params, "questions"))
                .iter()
                .map(|question| Question {
                    id: text(question, "id").into(),
                    prompt: text(question, "question").into(),
                    options: array(&question["options"])
                        .iter()
                        .map(|option| text(option, "label").into())
                        .collect(),
                    input: cx.new(|cx| InputState::new(window, cx).placeholder("回答を入力")),
                })
                .collect();
            self.requests.insert(
                key.clone(),
                RequestInputs {
                    questions,
                    raw: cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .default_value("{}")
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
                desktop.turn(&self.conversation.turns[0], cx)
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
            source: agent_core::models::Thread,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            Self {
                desktop: cx.new(|cx| {
                    Desktop::new(
                        Mode::SideChat {
                            // Reject before reading preferences or connecting to a real Host.
                            remote: Some(agent_core::models::RemoteHost {
                                id: "fixture".into(),
                                name: "fixture".into(),
                                ticket: "invalid-fixture-ticket".into(),
                                extra: Default::default(),
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
            "id": "fixture", "turns": (0..20).map(|i| serde_json::json!({
                "id": format!("turn-{i}"), "status": "completed", "items": [
                    {"id": format!("user-{i}"), "type": "userMessage", "content": [
                        {"type": "inputText", "text": format!("Question {i}")}
                    ]},
                    {"id": format!("answer-{i}"), "type": "agentMessage", "text": "A long answer\n".repeat(10)}
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
                Arc::make_mut(&mut snapshot.navigation).thread_id = Some("fixture".into());
                std::sync::Arc::make_mut(&mut snapshot.conversations)
                    .insert("fixture".into(), fixture.conversation.source.clone());
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
        let source = serde_json::from_value(serde_json::json!({
            "id":"fixture", "turns":[{"id":"turn", "status":"completed", "items":[{
                "id":"files", "type":"fileChange", "status":"completed", "changes":[
                    {"path":"/fixture/a.txt", "kind":"update", "diff":"@@ -1 +1 @@\n-old\n+new"},
                    {"path":"/fixture/b.txt", "kind":"add", "diff":"@@ -0,0 +1 @@\n+second"}
                ]
            }]}]
        }))
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
                    serde_json::json!([{
                        "id":"user", "type":"userMessage", "content":[{"type":"localImage","path":"/fixture/image.png"}]
                    }])
                };
                let mut snapshot = Snapshot::default();
                if pending {
                    Arc::make_mut(&mut snapshot.pending_submissions).insert(
                        "pending".into(),
                        Arc::new(agent_core::state::PendingSubmission {
                            draft_key: "fixture".into(),
                            draft: Arc::new(agent_core::state::Draft {
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
                            recovery_text: None,
                            clear_draft: None,
                        }),
                    );
                }
                let source = serde_json::from_value(serde_json::json!({
                    "id":"fixture", "turns":[{"id":"turn", "status":"completed", "items":items}]
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

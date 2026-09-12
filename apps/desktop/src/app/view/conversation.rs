use super::*;
use agent_core::presentation::conversation::{
    ActivityExpansion, ConversationRowContent, activity_is_expanded,
};

impl Desktop {
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
        h_flex()
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
        let mut body = v_flex()
            .gap_3()
            .max_w(px(560.))
            .p_4()
            .rounded(px(18.))
            .bg(rgb(0x303030));
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
            .group(group.clone())
            .items_end()
            .gap_1()
            .child(body)
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
mod file_change_tests {
    use super::{Desktop, Mode, Snapshot};
    use agent_core::presentation::conversation::{RenderedConversation, project_conversation};
    use gpui_kit as gpui;
    use gpui_kit::{AppContext, Context, Entity, IntoElement, Render, TestAppContext, Window, px};
    use std::sync::Arc;

    struct FileActivity {
        desktop: Entity<Desktop>,
        conversation: Arc<RenderedConversation>,
    }
    impl Render for FileActivity {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.desktop.update(cx, |desktop, cx| {
                desktop.turn(&self.conversation.turns[0], cx)
            })
        }
    }

    #[gpui::test]
    fn file_changes_render_and_expand_in_the_desktop_view(cx: &mut TestAppContext) {
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
        let source = Arc::new(serde_json::from_value(serde_json::json!({
            "id":"fixture", "turns":[{"id":"turn", "status":"completed", "items":[{
                "id":"files", "type":"fileChange", "status":"completed", "changes":[
                    {"path":"/fixture/a.txt", "kind":"update", "diff":"@@ -1 +1 @@\n-old\n+new"},
                    {"path":"/fixture/b.txt", "kind":"add", "diff":"@@ -0,0 +1 @@\n+second"}
                ]
            }]}]
        })).unwrap());
        let conversation = project_conversation(&Snapshot::default(), source, &None);
        let (view, window) = cx.add_window_view(|window, cx| FileActivity {
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
            conversation,
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
        view.update(window, |view, cx| {
            assert!(view.desktop.read(cx).expanded_items.contains("files"));
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("change-files-1").unwrap().top() > before,
            "expanded diff must occupy space in the rendered conversation"
        );
        view.update(window, |view, cx| {
            view.desktop.update(cx, |desktop, _| {
                assert_eq!(desktop.diffs.len(), 2);
                assert!(desktop.diffs.contains_key("diff-files-0"));
                assert!(desktop.diffs.contains_key("diff-files-1"));
            });
        });
    }
}

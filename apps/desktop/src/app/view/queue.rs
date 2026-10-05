use super::*;
use agent_core::presentation::conversation::queue_messages;
use agent_protocol::queue::{QueueAction, QueueControl};

impl Desktop {
    pub(super) fn queue_panel(&self, cx: &Context<Self>) -> AnyElement {
        let Some(thread) = self.thread() else {
            return div().into_any_element();
        };
        let Some(session) = thread.id.clone() else {
            return div().into_any_element();
        };
        let messages = queue_messages(&thread.queued_inputs);
        if messages.is_empty() && !thread.queue_held {
            return div().into_any_element();
        }
        let enabled = self.snapshot.connected && self.busy == 0;
        let held = thread.queue_held;
        let target = session.clone();
        let mut panel = v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .rounded_lg()
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .justify_between()
                    .child(format!("キュー {}", messages.len()))
                    .child(
                        Button::new("queue-toggle")
                            .label(if held { "再開" } else { "一時停止" })
                            .small()
                            .ghost()
                            .disabled(!enabled)
                            .on_click(cx.listener(move |view, _, _, _| {
                                view.dispatch(Intent::QueueControl(QueueControl {
                                    session: target.clone(),
                                    action: if held {
                                        QueueAction::Resume
                                    } else {
                                        QueueAction::Pause
                                    },
                                }))
                            })),
                    ),
            );
        for message in messages {
            let mut controls = h_flex().gap_2().child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.status),
            );
            for (suffix, label, action) in [
                ("up", "上へ移動", message.move_up),
                ("down", "下へ移動", message.move_down),
                (
                    "remove",
                    "削除",
                    message.removable.then(|| QueueAction::Cancel {
                        id: message.id.clone(),
                    }),
                ),
            ] {
                if let Some(action) = action {
                    let target = session.clone();
                    controls = controls.child(
                        Button::new(format!("queue-{suffix}-{}", message.id))
                            .label(label)
                            .xsmall()
                            .ghost()
                            .disabled(!enabled)
                            .on_click(cx.listener(move |view, _, _, _| {
                                view.dispatch(Intent::QueueControl(QueueControl {
                                    session: target.clone(),
                                    action: action.clone(),
                                }))
                            })),
                    );
                }
            }
            if message.editable {
                let target = session.clone();
                let id = message.id.clone();
                let text = message.text.clone();
                controls = controls.child(
                    Button::new(format!("queue-edit-{id}"))
                        .label("編集")
                        .xsmall()
                        .ghost()
                        .disabled(!enabled)
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.queue_editor = Some(QueueEditor {
                                session: target.clone(),
                                id: id.clone(),
                                input: cx.new(|cx| {
                                    TextareaState::new(window, cx)
                                        .default_value(text.clone())
                                        .auto_grow(2, 6)
                                }),
                            });
                            cx.notify();
                        })),
                );
            }
            let mut row = v_flex().gap_1().child(div().text_sm().child(message.text));
            if !message.images.is_empty() {
                row = row.child(
                    div()
                        .text_xs()
                        .child(format!("添付画像 {}", message.images.len())),
                )
            }
            panel = panel.child(row.child(controls));
        }
        if let Some(editor) = &self.queue_editor
            && editor.session == session
        {
            let id = editor.id.clone();
            let target = editor.session.clone();
            panel = panel.child(
                v_flex()
                    .gap_2()
                    .child("キューを編集")
                    .child(Textarea::new(&editor.input).readonly(!enabled))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("queue-edit-save")
                                    .label("保存")
                                    .small()
                                    .disabled(!enabled)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        let Some(editor) = &view.queue_editor else {
                                            return;
                                        };
                                        let text = editor.input.read(cx).value().to_string();
                                        if text.trim().is_empty() {
                                            return;
                                        }
                                        view.busy += 1;
                                        view.perform(
                                            Intent::QueueControl(QueueControl {
                                                session: target.clone(),
                                                action: QueueAction::Edit {
                                                    id: id.clone(),
                                                    text,
                                                },
                                            }),
                                            OperationCompletion::QueueEdit(
                                                target.clone(),
                                                id.clone(),
                                            ),
                                        );
                                    })),
                            )
                            .child(
                                Button::new("queue-edit-cancel")
                                    .label("キャンセル")
                                    .small()
                                    .ghost()
                                    .disabled(!enabled)
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.queue_editor = None;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }
        panel.into_any_element()
    }
}

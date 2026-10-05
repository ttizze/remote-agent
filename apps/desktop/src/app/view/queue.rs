use super::*;
use agent_protocol::queue::{QueueAction, QueueControl};

impl Desktop {
    pub(super) fn queue_panel(&self, cx: &Context<Self>) -> AnyElement {
        let Some(thread) = self.thread() else {
            return div().into_any_element();
        };
        let Some(session) = thread.id.clone() else {
            return div().into_any_element();
        };
        let messages = self.snapshot.queue_messages();
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
                let id = message.id.clone();
                let editing = message.editing;
                controls = controls.child(
                    Button::new(format!("queue-edit-{id}"))
                        .label(if editing { "キャンセル" } else { "編集" })
                        .xsmall()
                        .ghost()
                        .disabled(!enabled)
                        .on_click(cx.listener(move |view, _, _, _| {
                            view.dispatch(if editing {
                                Intent::CancelQueueEdit
                            } else {
                                Intent::BeginQueueEdit { id: id.clone() }
                            });
                        })),
                );
            }
            if let Some(steer) = &message.steer {
                let steer = steer.clone();
                controls = controls.child(
                    Button::new(format!("queue-steer-{}", message.id))
                        .label("Steerへ昇格")
                        .xsmall()
                        .ghost()
                        .disabled(!enabled)
                        .on_click(cx.listener(move |view, _, _, _| {
                            view.dispatch(Intent::SteerQueued(steer.clone()))
                        })),
                );
            }
            let mut row = v_flex()
                .gap_1()
                .p_2()
                .rounded_md()
                .when(message.editing, |row| {
                    row.border_1().border_color(cx.theme().primary)
                })
                .child(div().text_sm().child(message.text));
            if !message.images.is_empty() {
                row = row.child(
                    div()
                        .text_xs()
                        .child(format!("添付画像 {}", message.images.len())),
                )
            }
            panel = panel.child(row.child(controls));
        }
        panel.into_any_element()
    }
}

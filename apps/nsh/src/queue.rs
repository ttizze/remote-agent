//! Windows' queue surface. Shared core owns order, previews and every action;
//! the Swift renderer consumes the same queue in its scrolling timeline.
use crate::{
    Connection,
    l10n::tr,
    ui::{self, Icon, colors},
};
use agent_core::presentation::queue::{QueueControl, QueuePresentation, QueuedMessage};
use agent_protocol::ids::ClientInputId;
use nsh_core::prelude::*;
use std::sync::Arc;

#[derive(Clone, PartialEq)]
struct Drag {
    id: ClientInputId,
    target: Option<(ClientInputId, bool)>,
}

pub(crate) fn queued(connection: Connection) -> View {
    let queue = computed(move || {
        connection
            .composer
            .with(|c| c.controls.follow_up.queue.clone())
    });
    let expanded = signal(true);
    let count = move || queue.with(|q| q.message_count());
    let act = move |id, control| {
        if let Some(intent) = queue.with_untracked(|q| q.action(id, control)) {
            connection.dispatch(intent);
        }
    };
    let reorder = move |id, target, after| {
        if let Some(intent) = queue.with_untracked(|q| q.drop_on(id, target, after)) {
            connection.dispatch(intent);
        }
    };
    let header = view()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .h(px(28.0))
        .rounded(px(ui::RADIUS))
        .role(AccessibilityRole::Button)
        .label(move || {
            if expanded.get() {
                tr("キューのメッセージを折りたたむ")
            } else {
                tr("キューのメッセージを展開")
            }
        })
        .on_press(move || expanded.update(|value| *value = !*value))
        .child(ui::icon(Icon::QUEUED, 14.0))
        .child(
            text(tr("キューのメッセージ"))
                .text_size(px(13.0))
                .text_color(colors::muted()),
        )
        .child(view().flex_1())
        .child(
            text(move || count().to_string())
                .text_size(px(12.0))
                .text_color(colors::muted()),
        )
        .child(dynamic(move || {
            ui::icon(
                if expanded.get() {
                    Icon::CHEVRON_UP
                } else {
                    Icon::CHEVRON_DOWN
                },
                14.0,
            )
        }));
    view()
        .w_full()
        .p_1()
        .gap_1()
        .rounded(px(12.0))
        .border_1()
        .border_color(colors::border())
        .bg(colors::SURFACE)
        .label(tr("キューのメッセージ"))
        .accessibility_value(move || {
            Some(nsh_core::AccessibilityValue {
                text: Some(count().to_string().into()),
                ..Default::default()
            })
        })
        .child(header)
        .child(when(expanded, move || {
            let drag = signal(None::<Drag>);
            scroll().max_h(px(128.0)).child(view().gap_1().child(each(
                move || queue.with(|q| q.messages(true)),
                |message: &QueuedMessage| message.id.to_string(),
                move |message, _| row(queue, message, drag, act, reorder),
            )))
        }))
}

fn grip(
    drag: Signal<Option<Drag>>,
    id: impl Fn() -> ClientInputId + Copy + 'static,
    adjacent: impl Fn(bool) + Copy + 'static,
    reorder: impl Fn(ClientInputId, ClientInputId, bool) + Copy + 'static,
) -> View {
    let focused = signal(false);
    view()
        .w(px(24.0))
        .h(px(24.0))
        .items_center()
        .justify_center()
        .rounded(px(ui::RADIUS))
        .text_color(colors::muted())
        .hover(|s| s.bg(colors::GHOST_HOVER))
        .role(AccessibilityRole::Button)
        .label(tr(
            "キューのメッセージを並べ替え（ドラッグ、または矢印キー）",
        ))
        .on_press(|| {})
        .on_focus(move || focused.set(true))
        .on_blur(move || focused.set(false))
        .on_pan_start(move |_: &PanEvent| {
            drag.set(Some(Drag {
                id: id(),
                target: None,
            }))
        })
        .on_pan_end(move |event: &PanEvent| {
            let dropped = drag.with_untracked(Clone::clone);
            drag.set(None);
            if !event.cancelled
                && let Some(Drag {
                    id,
                    target: Some((target, after)),
                }) = dropped
            {
                reorder(id, target, after);
            }
        })
        .child(ui::icon(Icon::GRIP, 14.0))
        .child(when(focused, move || {
            view()
                .child(crate::composer::shortcut(
                    "up",
                    nsh_core::keymap::When::NotEditing,
                    move |_| adjacent(true),
                ))
                .child(crate::composer::shortcut(
                    "down",
                    nsh_core::keymap::When::NotEditing,
                    move |_| adjacent(false),
                ))
        }))
}

fn row(
    queue: Memo<Arc<QueuePresentation>>,
    message: ReadSignal<QueuedMessage>,
    drag: Signal<Option<Drag>>,
    act: impl Fn(ClientInputId, QueueControl) + Copy + 'static,
    reorder: impl Fn(ClientInputId, ClientInputId, bool) + Copy + 'static,
) -> View {
    let id = move || message.with(|message| message.id.clone());
    let controls = computed(move || queue.with(|q| q.controls(id())));
    let can_reorder = move || {
        controls.with(|controls| {
            controls.iter().any(|control| {
                matches!(
                    control.action,
                    QueueControl::MoveUp | QueueControl::MoveDown
                ) && control.enabled
            })
        })
    };
    let height = signal(28.0);
    let adjacent = move |up| {
        act(
            id(),
            if up {
                QueueControl::MoveUp
            } else {
                QueueControl::MoveDown
            },
        )
    };
    let over = move |event: &MouseEvent| {
        if !can_reorder() || event.pressed_button.is_none() {
            return;
        }
        let target = (id(), event.position.y.0 >= height.get_untracked() / 2.0);
        drag.update(|drag| {
            if let Some(drag) = drag.as_mut() {
                drag.target = Some(target);
            }
        });
    };
    let insertion = move |after| {
        drag.with(|drag| {
            drag.as_ref().and_then(|drag| drag.target.as_ref()) == Some(&(id(), after))
        })
    };
    let line = |edge: f32| {
        view()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(edge))
            .h(px(2.0))
            .rounded_full()
            .bg(rgba(0xe6e6e6b3))
    };
    view()
        .flex_row()
        .items_center()
        .gap_1()
        .min_h(px(28.0))
        .on_layout(move |bounds: &Bounds<Points>| {
            let next = bounds.size.height.0;
            if height.get_untracked() != next {
                height.set(next);
            }
        })
        .rounded(px(ui::RADIUS))
        .style_when(
            move || drag.with(|drag| drag.as_ref().is_some_and(|drag| drag.id == id())),
            |s| s.opacity(0.5),
        )
        .bg(move || {
            if queue.with(|q| q.editing() == Some(id())) {
                colors::SELECTED
            } else {
                rgba(0x00000000)
            }
        })
        .on_mouse_move(over)
        .child(when(move || insertion(false), move || line(0.0)))
        .child(when(
            move || insertion(true),
            move || line(height.get() - 2.0),
        ))
        .child(when(can_reorder, move || grip(drag, id, adjacent, reorder)))
        .child(when(
            move || message.with(|message| message.pending),
            || {
                view()
                    .label(tr("キューへ保存中"))
                    .text_color(colors::muted())
                    .child(ui::icon(Icon::SAVING, 12.0))
            },
        ))
        .child(when(
            move || message.with(|message| message.fresh_context),
            || {
                view()
                    .label(tr("新しいセッション"))
                    .tooltip(ui::tooltip(tr("新しいセッション")))
                    .text_color(colors::muted())
                    .child(ui::icon(Icon::CLOSE, 14.0))
            },
        ))
        .child(
            view().flex_1().min_w_0().child(
                text(move || message.with(|message| message.text.clone())).text_size(px(13.0)),
            ),
        )
        .child(dynamic(move || {
            let view = view();
            if let Some(label) = queue.with(|q| q.attachment_label(id())) {
                view.child(text(label).text_size(px(12.0)).text_color(colors::muted()))
            } else {
                view
            }
        }))
        .child(each(
            move || {
                controls.with(|controls| {
                    controls
                        .iter()
                        .filter(|control| {
                            !matches!(
                                control.action,
                                QueueControl::MoveUp | QueueControl::MoveDown
                            )
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
            },
            |control: &agent_core::presentation::queue::QueueControlPresentation| {
                control.action as u8
            },
            move |control, _| {
                let action = control.with_untracked(|control| control.action);
                let title = move || -> SharedString {
                    control.with(|control| control.title.clone()).into()
                };
                let press = move || act(id(), action);
                let button = match action {
                    QueueControl::Edit => ui::icon_button(Icon::PENCIL, title, press),
                    QueueControl::Remove => ui::icon_button(Icon::CLOSE, title, press),
                    QueueControl::Steer => ui::row_labeled_button(Icon::STEER_NOW, title, press),
                    _ => ui::row_text_button(title, press),
                };
                button.disabled(move || !control.with(|control| control.enabled))
            },
        ))
}

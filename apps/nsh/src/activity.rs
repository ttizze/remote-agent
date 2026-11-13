use crate::l10n::tr;
use crate::shared::Shared;
use crate::ui::Enabled;
use agent_core::{
    presentation::conversation::RenderedItem,
    state::{Intent, operations as op},
};
use agent_protocol::ids::TurnId;
use nsh_core::prelude::*;
use std::collections::HashSet;

#[derive(Clone, PartialEq)]
pub(crate) struct Item {
    pub source: Shared<RenderedItem>,
    pub turn: TurnId,
    /// Its turn's live row (`RenderedTurn::live_activity`), which shimmers.
    pub live: bool,
    /// What its row reads (`TurnRow::Work`, `TurnRow::Live`).
    pub label: String,
}

/// T3 Code's work log line: `icon` in its box, then the label.
pub(crate) fn line(icon: impl IntoElement, label: impl IntoElement) -> View {
    use crate::conversation::look;
    view()
        .flex_1()
        .min_w_0()
        .flex_row()
        .items_center()
        .gap(px(look::ICON_GAP))
        .child(
            view()
                .w(px(look::ICON_BOX))
                .h(px(look::ICON_BOX))
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .child(icon),
        )
        .child(label)
}

pub(crate) fn details(
    surroundings: crate::rich_text::Surroundings,
    item: Memo<Option<Item>>,
    expanded: Signal<HashSet<String>>,
    column_width: Memo<f32>,
) -> impl Element {
    let connection = surroundings.connection;
    let open = move || {
        item.with(|item| {
            item.as_ref()
                .is_some_and(|item| expanded.with(|all| all.contains(&item.source.data.id)))
        })
    };
    let request = computed(move || {
        let item = item.get()?;
        Some(op::ReadItem {
            thread_id: connection.context.with(|p| p.navigation().thread_id)?,
            turn_id: item.turn,
            item_id: item.source.data.native_id.clone()?,
        })
    });
    let output = computed(move || {
        if !open()
            || item.with(|item| {
                item.as_ref()
                    .is_none_or(|item| item.source.data.kind != "command")
            })
        {
            return None;
        }
        let read = request.get()?;
        Some((read.thread_id, read.item_id))
    });
    effect(move || {
        if let Some((thread_id, item_id)) = output.get() {
            connection.dispatch(Intent::WatchCommandOutput {
                thread_id: thread_id.clone(),
                item_id: item_id.clone(),
                watching: true,
            });
            cleanup(move || {
                connection.dispatch(Intent::WatchCommandOutput {
                    thread_id,
                    item_id,
                    watching: false,
                })
            });
        }
    });
    let detail = computed(move || {
        request.with(|read| {
            read.as_ref()
                .map(|read| {
                    connection
                        .context
                        .with(|context| context.item_detail(read.clone()))
                })
                .unwrap_or_default()
        })
    });
    let running = move || detail.with(|detail| detail.loading);
    let error = move || detail.with(|detail| detail.error.clone());
    let load = move || {
        if let Some(read) = request.get_untracked() {
            connection.dispatch(Intent::ReadItem(read));
        }
    };
    let toggle = move || {
        let Some(item) = item.get_untracked() else {
            return;
        };
        // As T3 Code's, the reader stays where they were.
        let opening = !open();
        expanded.update(|all| {
            if opening {
                all.insert(item.source.data.id.clone());
            } else {
                all.remove(&item.source.data.id);
            }
        });
        if opening && item.source.data.deferred {
            load();
        }
    };
    let label = computed(move || {
        item.with(|item| {
            item.as_ref()
                .map(|item| (item.source.work_row_label(item.live), item.label.clone()))
        })
    });
    let heading = move || {
        label.with(|label| {
            label
                .as_ref()
                .map(|(label, _)| label.heading.clone())
                .unwrap_or_default()
        })
    };
    // As T3 Code's work log: reasoning reads as its trace on one line, and
    // opened as Thought above the whole of it.
    let reasoning = move || {
        item.with(|item| {
            item.as_ref()
                .is_some_and(|item| item.source.data.kind == "reasoning")
        })
    };
    let summary = move || {
        if reasoning() && open() {
            heading()
        } else {
            label.with(|label| {
                label
                    .as_ref()
                    .map(|(_, row)| row.clone())
                    .unwrap_or_default()
            })
        }
    };
    let accessible = move || {
        let summary = summary();
        SharedString::from(if reasoning() && !open() {
            format!("{}: {summary}", heading())
        } else {
            summary
        })
    };
    let live = move || item.with(|item| item.as_ref().is_some_and(|item| item.live));
    let expandable = computed(move || {
        item.with(|item| {
            item.as_ref().is_some_and(|item| {
                item.source.data.deferred || !item.source.detail_blocks().is_empty()
            })
        })
    });
    // A reused row shows another tool's icon: it is made anew for each kind.
    let icon = move || {
        view().child(each(
            move || {
                vec![item.with(|item| {
                    item.as_ref()
                        .map(|item| item.source.data.kind.clone())
                        .unwrap_or_default()
                })]
            },
            |kind: &String| kind.clone(),
            |kind, _| {
                crate::ui::icon(
                    crate::ui::Icon::work(&kind.get_untracked()),
                    crate::conversation::look::ICON_SIZE,
                )
            },
        ))
    };
    view()
        .w_full()
        // As T3 Code's work log: the tool's icon and a one-line summary.
        .child(
            nsh_base::button()
                .accessibility_label(nsh_core::element::Prop::from(
                    accessible.into_reactive().map(Some),
                ))
                .flex_row()
                .items_center()
                .min_h(px(24.0))
                .px(px(2.0))
                .rounded(px(6.0))
                .text_color(crate::conversation::look::muted())
                .on_press(toggle)
                .child(when(live, move || {
                    // As T3's `live-tool-shine`, over the label alone.
                    crate::shimmer::line(Some(icon), summary, crate::shimmer::Sweep::Shine)
                }))
                .child(when(
                    move || !live(),
                    move || {
                        line(
                            view()
                                .text_color(crate::conversation::look::icon())
                                .child(icon()),
                            text(summary)
                                .flex_1()
                                .min_w_0()
                                .number_of_lines(1)
                                .text_size(px(crate::conversation::look::TEXT))
                                .line_height(px(crate::conversation::look::LINE)),
                        )
                    },
                ))
                // T3 Code's chevron, held in place on a row that has nothing to open.
                .child(
                    view()
                        .w(px(16.0))
                        .h(px(16.0))
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .text_color(crate::conversation::look::icon())
                        .opacity(move || if expandable.get() { 0.7_f32 } else { 0.0 })
                        .child(dynamic(move || {
                            use crate::ui::Icon;
                            let chevron = if open() {
                                Icon::CHEVRON_DOWN
                            } else {
                                Icon::CHEVRON_RIGHT
                            };
                            crate::ui::icon(chevron, 12.0).into_element()
                        })),
                ),
        )
        .child(when(open, move || {
            view()
                .w_full()
                .pl(px(28.0))
                .gap_2()
                .child(when(
                    move || {
                        item.with(|item| {
                            item.as_ref().is_some_and(|item| item.source.data.deferred)
                        }) || running()
                            || error().is_some()
                    },
                    move || {
                        view()
                            .gap_2()
                            .child(text(move || {
                                error().map(|error| error.to_string()).unwrap_or_default()
                            }))
                            .child(
                                crate::ui::row_text_button(
                                    move || {
                                        SharedString::from(if running() {
                                            tr("詳細を読み込み中…")
                                        } else {
                                            tr("詳細を読み込む")
                                        })
                                    },
                                    load,
                                )
                                .enabled(move || !running()),
                            )
                    },
                ))
                .child(crate::rich_text::content(
                    surroundings,
                    move || {
                        item.with(|item| {
                            item.as_ref()
                                .filter(|item| !item.source.data.deferred)
                                .map(|item| item.source.detail_blocks())
                                .unwrap_or_default()
                        })
                    },
                    || crate::conversation::Kind::Response,
                    move || (column_width.try_get().unwrap_or_default() - 28.0).max(0.0),
                ))
        }))
}

/// A live status line that is not a work row: T3 Code's Thinking row, with
/// its brain icon, its retry of the provider, with a wrench, and setting up
/// a worktree, which its body names.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Status {
    /// The turn works with nothing else live.
    Thinking,
    /// The provider's stream dropped and is retried.
    Retrying,
    /// The turn waits for an answer: an approval (its body names what for)
    /// or input.
    Requested {
        approval: bool,
    },
    SettingUp,
}

/// A status line, as T3 Code's live activity row: thinking and setting up
/// sweep as its `ActivityShimmerOverlay`, a retry or a request as its live
/// tool's `live-tool-shine`.
pub(crate) fn status(
    status: Memo<Option<Status>>,
    body: impl Fn() -> String + Copy + 'static,
) -> View {
    use crate::{shimmer::Sweep, ui::Icon};
    // A reused cell shows another status: its line is made anew.
    view()
        .w_full()
        .min_h(px(24.0))
        // T3 Code's work log row: `py-0.5`.
        .py(px(2.0))
        .px(px(2.0))
        .flex_row()
        .items_center()
        .child(each(
            move || status.get().into_iter().collect::<Vec<_>>(),
            |status: &Status| *status,
            move |status, _| {
                let status = status.get_untracked();
                let (icon, sweep) = match status {
                    Status::Thinking => (Some(Icon::work("reasoning")), Sweep::Focus),
                    Status::Retrying => (Some(Icon::WRENCH), Sweep::Shine),
                    Status::Requested { approval: true } => {
                        (Some(Icon::work("command")), Sweep::Shine)
                    }
                    Status::Requested { approval: false } => (Some(Icon::QUESTION), Sweep::Shine),
                    Status::SettingUp => (None, Sweep::Focus),
                };
                crate::shimmer::line(
                    icon.map(|icon| {
                        move || crate::ui::icon(icon, crate::conversation::look::ICON_SIZE)
                    }),
                    move || match status {
                        Status::Thinking => tr("思考中").to_owned(),

                        _ => body(),
                    },
                    sweep,
                )
            },
        ))
}

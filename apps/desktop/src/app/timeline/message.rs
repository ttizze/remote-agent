//! Sent prompts, unsent prompts, responses and the changed-files card under a
//! response.
use super::{Desktop, color, icon, markdown::chat_markdown, mono, text_2xs, text_3xs, tint};
use agent_core::{
    commands::{build::checkpoint_after_run, outbox::Phase},
    state::Intent,
    view::{
        composer::chips::{
            ContextChipSurface, context_chips, mobile_message_markdown, standalone_attachment_ids,
        },
        thread::{ChangedFilesDisclosure, ThreadView},
        timeline::{
            changed_files::{
                ChangedFileRowKind, ChangedFilesCard, DiffStatLabel, toggle_directory,
            },
            message::{AssistantMeta, SHOW_FULL_MESSAGE, SHOW_LESS, user_message_collapsible},
            rows::{AssistantMessageRow, PendingMessageRow, TimelineRow, UserMessageRow},
        },
    },
};
use agent_domain::{Attachment, AttachmentKind, MessageContext};
use gpui_kit::{
    component::{
        Disableable, Sizable, WindowExt,
        button::{Button, ButtonVariants},
        dialog::DialogFooter,
        h_flex,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{collections::BTreeMap, sync::Arc};

/// The fade over the bottom of a collapsed prompt.
const PROMPT_FADE: f32 = 28.;

/// Marks single newlines outside code fences as hard breaks, the way a
/// response that keeps its line breaks reads.
fn hard_line_breaks(text: &str) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut fenced = false;
    let mut result = String::with_capacity(text.len() + lines.len() * 2);
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        }
        result.push_str(line);
        let next = lines.get(index + 1);
        if let Some(next) = next {
            if !fenced && !line.trim().is_empty() && !next.trim().is_empty() {
                result.push_str("  ");
            }
            result.push('\n');
        }
    }
    result
}

fn stat_label(stat: &DiffStatLabel) -> impl IntoElement {
    h_flex()
        .gap_1()
        .child(
            div()
                .text_color(color("successForeground"))
                .child(stat.additions.clone()),
        )
        .child(
            div()
                .text_color(color("error"))
                .child(stat.deletions.clone()),
        )
}

impl Desktop {
    pub(super) fn render_user_message(
        &mut self,
        row: &TimelineRow,
        user: &UserMessageRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = user.message.to_string();
        let group = SharedString::from(format!("user-{id}"));
        let decorations = &user.decorations;
        let bubble = self.render_prompt_bubble(
            &id,
            &user.text,
            &user.attachments,
            user.context.as_ref(),
            decorations.collapsible,
            cx,
        );
        v_flex()
            .group(group.clone())
            .w_full()
            .items_end()
            .gap_1()
            .when_some(decorations.attribution.clone(), |message, attribution| {
                let label = div()
                    .id(SharedString::from(format!("attribution-{id}")))
                    .mr_1()
                    .text_color(tint("textMuted", 0.7))
                    .child(attribution.label.clone());
                message.child(text_2xs(match attribution.sender_thread {
                    Some(thread) => {
                        let thread = thread.to_string();
                        let open = SharedString::from(attribution.open_label.clone());
                        label
                            .cursor_pointer()
                            .hover(|style| style.text_color(color("textMuted")).underline())
                            .tooltip(move |window, cx| Tooltip::new(open.clone()).build(window, cx))
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.open_thread(thread.clone(), cx)
                            }))
                    }
                    None => label,
                }))
            })
            .when_some(decorations.intent.clone(), |message, intent| {
                let tooltip = SharedString::from(intent.tooltip.clone());
                message.child(
                    h_flex()
                        .id(SharedString::from(format!("intent-{id}")))
                        .mr_1()
                        .gap_1()
                        .text_xs()
                        .line_height(px(12.))
                        .text_color(color("textMuted"))
                        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                        .when(intent.steer_icon, |marker| {
                            marker.child(icon("redo-2").size(px(12.)))
                        })
                        .child(intent.label),
                )
            })
            .child(bubble)
            .when_some(decorations.status_chip.clone(), |message, status| {
                message.child(
                    div().mr_1().child(text_3xs(
                        div()
                            .rounded_full()
                            .border_1()
                            .border_color(tint("error", 0.25))
                            .bg(tint("error", 0.08))
                            .px(px(6.))
                            .py(px(2.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color("error"))
                            .child(status),
                    )),
                )
            })
            .child(
                h_flex()
                    .w_full()
                    .max_w(relative(0.8))
                    .justify_end()
                    .pr_1()
                    .gap_2()
                    .text_xs()
                    .opacity(0.)
                    .group_hover(group.clone(), |style| style.opacity(1.))
                    .children(self.render_timestamp(
                        SharedString::from(format!("time-{id}")),
                        row.created_at.as_ref(),
                        None,
                    ))
                    .child(
                        h_flex()
                            .gap(px(2.))
                            .when_some(decorations.edit_from_here.clone(), |actions, edit| {
                                let turn_count = edit.turn_count;
                                actions.child(
                                    Button::new(SharedString::from(format!("edit-{id}")))
                                        .icon(icon("undo-2"))
                                        .ghost()
                                        .xsmall()
                                        .disabled(!edit.enabled)
                                        .tooltip(edit.label.clone())
                                        .accessibility_label(edit.label)
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            view.confirm_edit_from_here(turn_count, window, cx)
                                        })),
                                )
                            })
                            .when_some(decorations.copy.clone(), |actions, copy| {
                                actions.child(self.render_copy_button(id.clone(), copy.text, cx))
                            }),
                    ),
            )
            .into_any_element()
    }

    /// A message this device sent that the Host has not folded yet.
    pub(super) fn render_pending_message(
        &mut self,
        row: &TimelineRow,
        pending: &PendingMessageRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = pending.message.to_string();
        let group = SharedString::from(format!("user-{id}"));
        let bubble = self.render_prompt_bubble(
            &id,
            &pending.text,
            &pending.attachments,
            pending.context.as_ref(),
            user_message_collapsible(&pending.text),
            cx,
        );
        let command = pending.command.to_string();
        v_flex()
            .group(group.clone())
            .w_full()
            .items_end()
            .gap_1()
            .when(pending.queued, |message| {
                message.child(
                    div()
                        .mr_1()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child("Queued"),
                )
            })
            .child(bubble)
            .when(
                matches!(pending.phase, Phase::Uncertain { .. }),
                |message| {
                    message.child(
                        h_flex()
                            .mr_1()
                            .gap_2()
                            .text_xs()
                            .text_color(color("textMuted"))
                            .child("Delivery unconfirmed")
                            .child(
                                Button::new(SharedString::from(format!("discard-{id}")))
                                    .label("Stop retrying")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |view, _, _, _| {
                                        view.perform(Intent::DiscardPending {
                                            command_id: command.clone(),
                                        })
                                    })),
                            ),
                    )
                },
            )
            .child(
                h_flex()
                    .w_full()
                    .max_w(relative(0.8))
                    .justify_end()
                    .pr_1()
                    .gap_2()
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.))
                    .children(self.render_timestamp(
                        SharedString::from(format!("time-{id}")),
                        row.created_at.as_ref(),
                        None,
                    ))
                    .when(!pending.text.trim().is_empty(), |actions| {
                        actions.child(self.render_copy_button(id.clone(), pending.text.clone(), cx))
                    }),
            )
            .into_any_element()
    }

    /// The right-aligned bubble of a prompt: pictures, files, then the text,
    /// clipped behind "Show full message" when long.
    fn render_prompt_bubble(
        &mut self,
        id: &str,
        text: &str,
        attachments: &[Attachment],
        context: Option<&MessageContext>,
        collapsible: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let standalone = standalone_attachment_ids(text, context, attachments);
        let shown: Vec<&Attachment> = attachments
            .iter()
            .filter(|attachment| standalone.contains(&attachment.id))
            .collect();
        let images: Vec<AnyElement> = shown
            .iter()
            .filter(|attachment| attachment.kind == AttachmentKind::Image)
            .map(|attachment| self.render_image_tile(attachment, cx))
            .collect();
        let files: Vec<AnyElement> = shown
            .iter()
            .filter(|attachment| attachment.kind == AttachmentKind::File)
            .map(|attachment| render_file_row(attachment, cx))
            .collect();
        let expanded = self.timeline.expanded_messages.contains(id);
        let collapsed = collapsible && !expanded;
        let body = (!text.trim().is_empty()).then(|| {
            let threads = self
                .snapshot
                .shell
                .snapshot
                .as_ref()
                .map(|shell| shell.threads.as_slice())
                .unwrap_or_default();
            let chips = Arc::new(context_chips(
                text,
                context,
                attachments,
                threads,
                ContextChipSurface::Message,
            ));
            div()
                .relative()
                .text_sm()
                .line_height(px(22.75))
                .when(collapsed, |body| body.max_h(px(176.)).overflow_hidden())
                .child(chat_markdown(
                    SharedString::from(format!("prompt-{id}")),
                    mobile_message_markdown(text, context),
                    chips,
                    cx,
                ))
                .when(collapsed, |body| {
                    let surface = color("messageSurface");
                    body.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left_0()
                            .right_0()
                            .h(px(PROMPT_FADE))
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(surface.opacity(0.), 0.),
                                linear_color_stop(surface, 1.),
                            )),
                    )
                })
        });
        let toggle_id = id.to_owned();
        v_flex()
            .max_w(relative(0.8))
            .min_w_0()
            .rounded(px(18.))
            .bg(color("messageSurface"))
            .p_3()
            .text_color(color("messageForeground"))
            .when(!images.is_empty(), |bubble| {
                bubble.child(
                    h_flex()
                        .mb_2()
                        .max_w(px(210.))
                        .flex_wrap()
                        .gap_2()
                        .children(images),
                )
            })
            .when(!files.is_empty(), |bubble| {
                bubble.child(v_flex().mb_2().gap_1().children(files))
            })
            .children(body)
            .when(collapsible, |bubble| {
                bubble.child(
                    h_flex().mt(px(6.)).child(
                        Button::new(SharedString::from(format!("collapse-{id}")))
                            .label(if expanded {
                                SHOW_LESS
                            } else {
                                SHOW_FULL_MESSAGE
                            })
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |view, _, _, cx| {
                                let expanded = &mut view.timeline.expanded_messages;
                                if !expanded.remove(&toggle_id) {
                                    expanded.insert(toggle_id.clone());
                                }
                                cx.notify();
                            })),
                    ),
                )
            })
            .into_any_element()
    }

    /// A 4:3 picture in the prompt's two-column grid, opening larger on click.
    fn render_image_tile(&mut self, attachment: &Attachment, cx: &mut Context<Self>) -> AnyElement {
        let path = self.attachment_image(&attachment.id);
        let tile = div()
            .id(SharedString::from(format!("image-{}", attachment.id)))
            .w(px(101.))
            .h(px(76.))
            .rounded(px(10.))
            .overflow_hidden()
            .border_1()
            .border_color(tint("border", 0.8))
            .bg(tint("canvas", 0.7));
        match path {
            Some(path) => {
                let tile_path = path.clone();
                let name = attachment.name.clone();
                tile.cursor_pointer()
                    .on_click(cx.listener(move |_, _, window, cx| {
                        let path = path.clone();
                        let name = SharedString::from(name.clone());
                        window.open_dialog(cx, move |dialog, _, _| {
                            dialog.title(name.clone()).w(px(880.)).child(
                                img(path.clone())
                                    .w_full()
                                    .max_h(px(640.))
                                    .object_fit(ObjectFit::Contain),
                            )
                        });
                    }))
                    .child(img(tile_path).size_full().object_fit(ObjectFit::Cover))
                    .into_any_element()
            }
            None => tile
                .flex()
                .items_center()
                .justify_center()
                .px_2()
                .child(text_2xs(
                    div()
                        .text_color(tint("textMuted", 0.7))
                        .truncate()
                        .child(attachment.name.clone()),
                ))
                .into_any_element(),
        }
    }

    pub(super) fn render_assistant_message(
        &mut self,
        thread: &ThreadView,
        row: &TimelineRow,
        message: &AssistantMessageRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = message.message.to_string();
        let group = SharedString::from(format!("assistant-{id}"));
        let text = if message.preserve_line_breaks {
            hard_line_breaks(&message.text)
        } else {
            message.text.clone()
        };
        v_flex()
            .group(group.clone())
            .min_w_0()
            .px_1()
            .py(px(2.))
            .child(
                div()
                    .text_sm()
                    .line_height(px(22.75))
                    .text_color(tint("text", 0.8))
                    .child(chat_markdown(
                        SharedString::from(format!("response-{id}")),
                        text,
                        Arc::default(),
                        cx,
                    )),
            )
            .when_some(message.changed_files.clone(), |response, card| {
                response.child(self.render_changed_files(&card, cx))
            })
            .when_some(message.meta.clone(), |response, meta| {
                response.child(
                    div()
                        .mt(px(6.))
                        .child(self.render_assistant_meta(thread, row, &id, &meta, false, cx)),
                )
            })
            .into_any_element()
    }

    /// Fork, status, copy and time under a response; hidden until hovered
    /// unless `always`.
    pub(super) fn render_assistant_meta(
        &mut self,
        thread: &ThreadView,
        row: &TimelineRow,
        message: &str,
        meta: &AssistantMeta,
        always: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let source = thread.thread_id.clone();
        h_flex()
            .when(always, |meta| meta.px_1().mt(px(2.)))
            .gap_2()
            .text_xs()
            .when(!always, |meta_row| {
                meta_row
                    .opacity(0.)
                    .group_hover(format!("assistant-{message}"), |style| style.opacity(1.))
            })
            .when_some(meta.fork.clone(), |meta_row, fork| {
                let run = fork.run.to_string();
                meta_row.child(
                    Button::new(SharedString::from(format!("fork-{message}")))
                        .icon(icon("git-fork"))
                        .ghost()
                        .xsmall()
                        .tooltip(fork.label.clone())
                        .accessibility_label(fork.label)
                        .on_click(cx.listener(move |view, _, _, _| {
                            view.perform(Intent::Fork {
                                source_thread_id: source.clone(),
                                run_id: run.clone(),
                            })
                        })),
                )
            })
            .when_some(meta.status_chip.clone(), |meta_row, status| {
                meta_row.child(text_3xs(
                    div()
                        .rounded_full()
                        .border_1()
                        .border_color(tint("border", 0.7))
                        .px(px(6.))
                        .py(px(2.))
                        .font_family(mono(cx))
                        .text_color(color("textMuted"))
                        .child(status),
                ))
            })
            .when(meta.copy.visible, |meta_row| {
                meta_row.child(self.render_copy_button(
                    format!("response-{message}"),
                    meta.copy.text.clone().unwrap_or_default(),
                    cx,
                ))
            })
            .when(meta.show_timestamp, |meta_row| {
                meta_row.children(self.render_timestamp(
                    SharedString::from(format!("time-{message}")),
                    row.created_at.as_ref(),
                    None,
                ))
            })
            .into_any_element()
    }

    fn render_changed_files(
        &mut self,
        card: &ChangedFilesCard,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let surface = if super::super::ui::is_dark() {
            tint("input", 0.2)
        } else {
            color("secondary")
        };
        let run = card.run.to_string();
        let tree = card.rows.iter().enumerate().map(|(index, file)| {
            let depth = file.depth as f32;
            let path = file.path.clone();
            let run = run.clone();
            let key = card.expansion_key.clone();
            let entry = h_flex()
                .id(SharedString::from(format!("changed-{run}-{index}")))
                .w_full()
                .gap_2()
                .rounded(px(8.))
                .py(px(6.))
                .pr_2()
                .pl(px(8. + depth * 14.))
                .cursor_pointer()
                .hover(|style| style.bg(tint("accentSurface", 0.6)));
            match file.kind {
                ChangedFileRowKind::Directory { expanded } => entry
                    .child(
                        icon(if expanded {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        })
                        .size(px(14.))
                        .text_color(tint("textMuted", 0.7)),
                    )
                    .child(
                        icon(if expanded { "folder" } else { "folder-closed" })
                            .size(px(14.))
                            .text_color(tint("textMuted", 0.75)),
                    )
                    .child(text_2xs(
                        div()
                            .truncate()
                            .font_family(mono(cx))
                            .text_color(tint("textMuted", 0.9))
                            .child(file.name.clone()),
                    ))
                    .when_some(file.stat.as_ref(), |entry, stat| {
                        entry.child(text_3xs(
                            div()
                                .ml_auto()
                                .font_family(mono(cx))
                                .child(stat_label(stat)),
                        ))
                    })
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.toggle_changed_folder(&run, &key, &path, cx)
                    })),
                ChangedFileRowKind::File => entry
                    .when(file.leading_spacer, |entry| {
                        entry.child(div().flex_none().size(px(14.)))
                    })
                    .child(
                        icon("file")
                            .size(px(14.))
                            .text_color(tint("textMuted", 0.7)),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_family(mono(cx))
                            .text_color(tint("text", 0.85))
                            .child(file.name.clone()),
                    )
                    .when_some(file.stat.as_ref(), |entry, stat| {
                        entry.child(text_3xs(
                            div()
                                .ml_auto()
                                .flex_none()
                                .font_family(mono(cx))
                                .child(stat_label(stat)),
                        ))
                    })
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.open_diff(Some(run.clone()), Some(path.clone()), window, cx)
                    })),
            }
        });
        let open_path = card.open_diff_path.clone();
        let open_run = run.clone();
        let toggle_run = run.clone();
        v_flex()
            .mt_4()
            .rounded(px(10.))
            .bg(surface)
            .child(
                h_flex()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .child(
                        h_flex()
                            .min_w_0()
                            .flex_wrap()
                            .gap_3()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .child(card.title.clone())
                            .when_some(card.stat.as_ref(), |summary, stat| {
                                summary.child(stat_label(stat))
                            }),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .when_some(card.toggle_all_label.clone(), |actions, label| {
                                let all_expanded = label.starts_with("Collapse");
                                actions.child(
                                    Button::new(SharedString::from(format!("folders-{run}")))
                                        .icon(icon(if all_expanded {
                                            "chevrons-down-up"
                                        } else {
                                            "chevrons-up-down"
                                        }))
                                        .ghost()
                                        .xsmall()
                                        .tooltip(label.clone())
                                        .accessibility_label(label)
                                        .on_click(cx.listener(move |view, _, _, cx| {
                                            view.toggle_all_changed_folders(&toggle_run, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("open-diff-{run}")))
                                    .icon(icon("file-diff"))
                                    .label(card.open_diff_label.clone())
                                    .ghost()
                                    .xsmall()
                                    .tooltip(card.open_diff_tooltip.clone())
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.open_diff(
                                            Some(open_run.clone()),
                                            open_path.clone(),
                                            window,
                                            cx,
                                        )
                                    })),
                            ),
                    ),
            )
            .child(v_flex().p_2().children(tree))
            .into_any_element()
    }

    fn changed_files_disclosure(&mut self, run: &str) -> &mut ChangedFilesDisclosure {
        let cards = &mut self.timeline.disclosure.changed_files;
        let index = match cards.iter().position(|card| card.run_id == run) {
            Some(index) => index,
            None => {
                cards.push(ChangedFilesDisclosure {
                    run_id: run.to_owned(),
                    ..ChangedFilesDisclosure::default()
                });
                cards.len() - 1
            }
        };
        &mut cards[index]
    }

    fn toggle_all_changed_folders(&mut self, run: &str, cx: &mut Context<Self>) {
        let card = self.changed_files_disclosure(run);
        card.all_expanded = !card.all_expanded;
        card.overrides.clear();
        self.refresh_views(cx);
    }

    fn toggle_changed_folder(&mut self, run: &str, key: &str, path: &str, cx: &mut Context<Self>) {
        let stale = self
            .timeline
            .changed_files_keys
            .insert(run.to_owned(), key.to_owned())
            .is_some_and(|previous| previous != key);
        let card = self.changed_files_disclosure(run);
        if stale {
            card.overrides.clear();
        }
        let mut overrides: BTreeMap<String, bool> = card.overrides.drain().collect();
        toggle_directory(&mut overrides, path, card.all_expanded);
        card.overrides = overrides.into_iter().collect();
        self.refresh_views(cx);
    }

    fn confirm_edit_from_here(
        &mut self,
        turn_count: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let revert = |restore_files: bool| {
                let owner = owner.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    window.close_dialog(cx);
                    let _ = owner.update(cx, |view, cx| {
                        view.rollback_to(turn_count, restore_files, window, cx)
                    });
                }
            };
            alert
                .title("Edit from here?")
                .description(
                    "Rewind chat to before this message. Your prompt and attachments return to the composer.",
                )
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("edit-from-here-cancel")
                                .label("Cancel")
                                .outline()
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("edit-from-here-files")
                                .label("Revert files too")
                                .danger()
                                .on_click(revert(true)),
                        )
                        .child(
                            Button::new("edit-from-here-keep")
                                .label("Revert and keep changes")
                                .primary()
                                .on_click(revert(false)),
                        ),
                )
        });
    }

    fn rollback_to(
        &mut self,
        turn_count: u64,
        restore_files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let checkpoint = self
            .snapshot
            .selected_state()
            .and_then(|state| checkpoint_after_run(state, turn_count).ok())
            .map(|checkpoint| checkpoint.id.to_string());
        match checkpoint {
            Some(checkpoint_id) => self.perform(Intent::Rollback {
                checkpoint_id,
                restore_files,
            }),
            None => self.show_error("Checkpoint unavailable", window, cx),
        }
    }
}

/// A prompt's file: its name and a download button.
fn render_file_row(attachment: &Attachment, cx: &mut Context<Desktop>) -> AnyElement {
    let (id, name) = (attachment.id.clone(), attachment.name.clone());
    let label = format!("Download {name}");
    h_flex()
        .min_w_0()
        .gap_1()
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .py_1()
                .text_sm()
                .child(icon("file").size(px(16.)))
                .child(div().min_w_0().truncate().child(name.clone())),
        )
        .child(
            Button::new(SharedString::from(format!("download-{id}")))
                .icon(icon("download"))
                .ghost()
                .xsmall()
                .tooltip(label.clone())
                .accessibility_label(label)
                .on_click(
                    cx.listener(move |view, _, _, _| {
                        view.save_attachment(id.clone(), name.clone())
                    }),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::hard_line_breaks;

    #[test]
    fn single_newlines_become_hard_breaks_outside_code() {
        assert_eq!(hard_line_breaks("a\nb\n\nc"), "a  \nb\n\nc");
        assert_eq!(hard_line_breaks("```\nx\ny\n```\nz"), "```\nx\ny\n```  \nz");
    }
}

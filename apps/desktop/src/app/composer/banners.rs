//! The strips attached above the composer: the queue, the usage-limit
//! recovery, the approval, question or "Plan ready" drawer, the task list, the
//! stash and the command menu.
use super::{Desktop, composer_shadow, glass, on_click, outline, shown_thread};
use crate::app::ui::{color, icon, shortcut, text_2xs, tint};
use agent_core::{
    state::{AnswerEdit, Intent, QueueAction},
    view::{
        composer::{stash::StashEntryView, view::ComposerView},
        plan::{PlanStepStatus, PlanView},
        queue::{
            QueueDropTarget, QueueRowView, QueueView, RESUME_QUEUE_LABEL, queue_insert_target,
            queue_step_target,
        },
        requests::{ApprovalTone, ApprovalView, QuestionsView},
        thread::ThreadView,
        time::{locale_date_time, relative_time_label},
        timeline::banners::{
            LIMIT_RECOVERY_SAVING, RESET_UNAVAILABLE_DESKTOP, RecoveryAction, UsageLimitRecovery,
        },
    },
};
use base64::Engine;
use chrono::{Local, TimeZone};
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder,
    *,
};
use std::{collections::HashMap, sync::Arc, time::Duration};

/// How far a strip slides under the composer's top edge (1rem + 1px).
const OVERLAP: f32 = 17.;
/// The icon column of a strip's rows.
const ICON_COLUMN: f32 = 24.;

pub(super) struct BannerState {
    pub(super) stash_open: bool,
    stash_highlight: usize,
    pub(super) queue_collapsed: bool,
    tasks_open: bool,
    /// The question whose options are hidden.
    collapsed_question: Option<String>,
    question_focus: FocusHandle,
    /// The queue row being dragged and the index it would land at.
    queue_dragging: Option<String>,
    queue_insert: Option<usize>,
    queue_grips: HashMap<String, FocusHandle>,
    stash_thumbnails: HashMap<String, Vec<Arc<Image>>>,
    /// The usage-limit recovery change in flight, and the last failure.
    recovery_pending: bool,
    recovery_error: Option<String>,
    thread: Option<String>,
}
impl BannerState {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            stash_open: false,
            stash_highlight: 0,
            queue_collapsed: false,
            tasks_open: false,
            collapsed_question: None,
            question_focus: cx.focus_handle(),
            queue_dragging: None,
            queue_insert: None,
            queue_grips: HashMap::new(),
            stash_thumbnails: HashMap::new(),
            recovery_pending: false,
            recovery_error: None,
            thread: None,
        }
    }
}

/// The vertical padding of a strip.
#[derive(Clone, Copy)]
enum Density {
    Default,
    Comfortable,
    Spacious,
}

/// One strip of the stack above the composer.
pub(super) struct Piece {
    warning: bool,
    density: Density,
    /// Spans the column; a shoulder tab fits its content.
    fill: bool,
    content: AnyElement,
}
impl Piece {
    pub(super) fn new(content: impl IntoElement) -> Self {
        Self {
            warning: false,
            density: Density::Default,
            fill: true,
            content: content.into_any_element(),
        }
    }
    /// `first` strips round their top corners; later ones continue the one above.
    fn render(self, first: bool) -> Div {
        let (inline, block) = match self.density {
            Density::Default => (4., 4.),
            Density::Comfortable => (4., 5.),
            Density::Spacious => (12., 12.),
        };
        let (border, background) = if self.warning {
            (tint("warning", 0.28), glass().blend(tint("warning", 0.08)))
        } else {
            (outline(), glass())
        };
        div()
            .relative()
            .min_w_0()
            .when(self.fill, |piece| piece.w_full())
            .border_1()
            .border_color(border)
            .bg(background)
            .shadow(composer_shadow())
            .when(first, |piece| piece.rounded_t(px(18.)))
            .when(!first, |piece| piece.border_t_0())
            .px(px(inline))
            .pt(px(block))
            .pb(px(block + OVERLAP))
            .text_xs()
            .line_height(px(16.))
            .child(self.content)
    }
}

/// A row: the icon column, the content and trailing actions.
fn row() -> Div {
    h_flex()
        .w_full()
        .min_w_0()
        .min_h(px(ICON_COLUMN))
        .items_center()
        .gap(px(4.))
}

fn icon_cell(name: Option<&str>) -> Div {
    div()
        .w(px(ICON_COLUMN))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .text_color(color("textMuted"))
        .when_some(name, |cell, name| cell.child(icon(name).size(px(12.))))
}

fn content() -> Div {
    h_flex().flex_1().min_w_0().items_center().gap_1()
}

fn actions() -> Div {
    h_flex().flex_none().items_center().justify_end().gap_1()
}

fn count(value: impl ToString) -> Div {
    div()
        .min_w(px(ICON_COLUMN))
        .flex_none()
        .flex()
        .justify_center()
        .font_weight(FontWeight::MEDIUM)
        .text_color(color("textMuted"))
        .child(value.to_string())
}

/// The disclosure chevron; the row itself is the control.
fn toggle_icon(expanded: bool) -> Div {
    div()
        .size(px(24.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(
            icon(if expanded {
                "chevron-down"
            } else {
                "chevron-up"
            })
            .size(px(14.))
            .text_color(color("textMuted")),
        )
}

/// A small muted ghost button.
fn ghost(id: impl Into<ElementId>) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .text_color(color("textMuted"))
}

/// The truncating text of a row, with its full text as the tooltip.
fn truncated(id: impl Into<ElementId>, text: String) -> Stateful<Div> {
    let tooltip = text.clone();
    div()
        .id(id)
        .min_w_0()
        .flex_1()
        .truncate()
        .child(text)
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
}

/// The queue row being dragged by its grip.
#[derive(Clone)]
struct QueueDrag {
    run_id: String,
    title: String,
}
impl Render for QueueDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(320.))
            .truncate()
            .rounded_md()
            .border_1()
            .border_color(color("border"))
            .bg(color("surfaceOverlay"))
            .px_2()
            .py_1()
            .text_xs()
            .shadow_md()
            .child(self.title.clone())
    }
}

impl Desktop {
    /// Resets what belonged to the previous thread.
    pub(super) fn sync_banners(&mut self) {
        let thread = self.thread_id();
        let banners = &mut self.composer.banners;
        if banners.thread != thread {
            banners.thread = thread;
            banners.stash_open = false;
            banners.tasks_open = false;
            banners.queue_collapsed = false;
            banners.collapsed_question = None;
            banners.queue_grips.clear();
            banners.recovery_pending = false;
            banners.recovery_error = None;
        }
        let live: Vec<&str> = self
            .snapshot
            .stash
            .entries
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        self.composer
            .banners
            .stash_thumbnails
            .retain(|id, _| live.contains(&id.as_str()));
    }

    /// The column of strips above the composer and the stash tab beside it.
    pub(super) fn composer_banners(
        &mut self,
        thread: Option<&ThreadView>,
        composer: &ComposerView,
        approval: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut pieces = vec![];
        if let Some(thread) = thread {
            if let Some(queue) = thread.queue.as_ref().filter(|queue| !queue.rows.is_empty()) {
                pieces.push(self.composer_queue(queue, cx));
            }
            if let Some(recovery) = &thread.limit_recovery {
                pieces.push(self.composer_limit_recovery(&thread.thread_id, recovery, cx));
            }
            let requests = &thread.requests;
            let top = if let Some(approval) = &requests.approval {
                Some(self.composer_approval(approval, cx))
            } else if let Some(questions) = &requests.questions {
                Some(self.composer_questions(questions, cx))
            } else {
                thread
                    .plan
                    .as_ref()
                    .filter(|plan| plan.show_plan_follow_up_prompt)
                    .and_then(|plan| plan.banner.as_ref())
                    .map(|banner| {
                        Piece::new(
                            row().child(icon_cell(None)).child(
                                content()
                                    .child(
                                        div()
                                            .flex_none()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(color("textMuted"))
                                            .child(banner.label.clone()),
                                    )
                                    .when_some(banner.title.clone(), |content, title| {
                                        content.child(
                                            div()
                                                .min_w_0()
                                                .flex_1()
                                                .truncate()
                                                .text_color(color("text").opacity(0.85))
                                                .child(title),
                                        )
                                    }),
                            ),
                        )
                    })
            };
            let blocking = top.is_some();
            pieces.extend(top);
            if !blocking && let Some(plan) = &thread.plan {
                pieces.extend(self.composer_tasks(plan, cx));
            }
        }
        if let Some(menu) = self.composer_command_menu(cx) {
            pieces.push(menu);
        } else if self.composer.banners.stash_open && !approval {
            pieces.push(self.composer_stash_menu(&composer.stash, window, cx));
        }
        let badge = (!approval && !composer.stash.is_empty())
            .then(|| self.composer_stash_badge(composer.stash.len(), cx));
        if pieces.is_empty() && badge.is_none() {
            return div().into_any_element();
        }
        let last = pieces.len().saturating_sub(1);
        let column = v_flex()
            .flex_1()
            .min_w_0()
            .children(pieces.into_iter().enumerate().map(|(index, piece)| {
                piece
                    .render(index == 0)
                    .when(index < last, |piece| piece.mb(px(-OVERLAP)))
            }));
        h_flex()
            .w_full()
            .items_end()
            .gap_1()
            .px(px(22.))
            .mb(px(-OVERLAP))
            .child(column)
            .children(badge.map(|badge| badge.render(true)))
            .into_any_element()
    }

    /// "Usage limit reached": when it resets, and the resume and snooze
    /// choices while the reset lies ahead.
    fn composer_limit_recovery(
        &mut self,
        thread_id: &str,
        recovery: &UsageLimitRecovery,
        cx: &mut Context<Self>,
    ) -> Piece {
        let description = match recovery
            .reset_at
            .as_ref()
            .and_then(|reset| Local.timestamp_millis_opt(reset.millis()).single())
        {
            Some(reset) => format!("Resets {}", locale_date_time(&reset)),
            None => RESET_UNAVAILABLE_DESKTOP.to_owned(),
        };
        let pending = self.composer.banners.recovery_pending;
        let action = |id: &'static str, label: String, action: RecoveryAction| {
            let thread_id = thread_id.to_owned();
            ghost(id)
                .label(if pending {
                    LIMIT_RECOVERY_SAVING.to_owned()
                } else {
                    label
                })
                .disabled(pending)
                .on_click(cx.listener(move |view, _, _, cx| {
                    view.composer.banners.recovery_pending = true;
                    view.composer.banners.recovery_error = None;
                    cx.notify();
                    view.perform_then(
                        Intent::LimitRecovery {
                            thread_id: thread_id.clone(),
                            action,
                        },
                        |view, result, _, cx| {
                            view.composer.banners.recovery_pending = false;
                            view.composer.banners.recovery_error = result
                                .as_ref()
                                .err()
                                .map(|error| agent_core::presentation::error::error_message(error));
                            cx.notify();
                        },
                    );
                }))
        };
        let actions = recovery.can_schedule.then(|| {
            h_flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(action(
                    "limit-recovery-resume",
                    recovery.resume_label.clone(),
                    RecoveryAction::Resume,
                ))
                .when(!recovery.snoozed, |actions| {
                    actions.child(
                        action(
                            "limit-recovery-snooze",
                            recovery.snooze_label.clone(),
                            RecoveryAction::Snooze,
                        )
                        .disabled(pending || !recovery.snooze_enabled),
                    )
                })
        });
        let error = self.composer.banners.recovery_error.clone();
        let mut piece = Piece::new(
            row()
                .items_start()
                .child(icon_cell(Some("gauge")).text_color(color("warningForeground")))
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(color("warningForeground"))
                                .child(recovery.title.clone()),
                        )
                        .child(div().text_color(color("textMuted")).child(description))
                        .children(actions)
                        .when_some(error, |content, error| {
                            content.child(div().text_color(color("errorForeground")).child(error))
                        }),
                ),
        );
        piece.warning = true;
        piece
    }

    fn composer_queue(&mut self, queue: &QueueView, cx: &mut Context<Self>) -> Piece {
        if !cx.has_active_drag() {
            self.composer.banners.queue_dragging = None;
            self.composer.banners.queue_insert = None;
        }
        let expanded = !self.composer.banners.queue_collapsed;
        let header = row()
            .id("queue-header")
            .cursor_pointer()
            .rounded_md()
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.composer.banners.queue_collapsed = !view.composer.banners.queue_collapsed;
                cx.notify();
            }))
            .child(icon_cell(Some("list-ordered")))
            .child(
                content()
                    .text_color(color("textMuted"))
                    .child(queue.title.clone()),
            )
            .child(
                actions()
                    .child(count(queue.count))
                    .child(toggle_icon(expanded)),
            );
        let run_ids: Vec<String> = queue
            .rows
            .iter()
            .filter_map(|row| row.run_id.clone())
            .collect();
        let held = queue.held_notice.clone().map(|notice| {
            row()
                .child(icon_cell(Some("clock")))
                .child(content().text_color(color("textMuted")).child(notice))
                .when(queue.can_resume, |held| {
                    held.child(
                        actions().child(ghost("resume-queue").label(RESUME_QUEUE_LABEL).on_click(
                            cx.listener(|view, _: &ClickEvent, _, _| {
                                view.perform(Intent::Queue {
                                    action: QueueAction::Resume,
                                })
                            }),
                        )),
                    )
                })
        });
        let rows: Vec<AnyElement> = queue
            .rows
            .iter()
            .map(|item| self.composer_queue_row(queue, &run_ids, item, cx))
            .collect();
        Piece::new(
            v_flex()
                .w_full()
                .child(header)
                .children(held)
                .when(expanded, |list| {
                    list.child(
                        v_flex()
                            .id("queue-rows")
                            .max_h(px(128.))
                            .overflow_y_scroll()
                            .gap(px(1.))
                            .children(rows),
                    )
                }),
        )
    }

    fn composer_queue_row(
        &mut self,
        queue: &QueueView,
        run_ids: &[String],
        item: &QueueRowView,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let insert = self.composer.banners.queue_insert;
        let dragging = self.composer.banners.queue_dragging.is_some()
            && self.composer.banners.queue_dragging == item.run_id;
        let reorderable = queue.can_reorder && item.run_id.is_some() && !queue.busy;
        let grip = item
            .run_id
            .clone()
            .filter(|_| queue.can_reorder)
            .map(|run_id| {
                let handle = self
                    .composer
                    .banners
                    .queue_grips
                    .entry(run_id.clone())
                    .or_insert_with(|| cx.focus_handle())
                    .clone();
                let ids = run_ids.to_vec();
                let moved = run_id.clone();
                div()
                    .id(SharedString::from(format!("queue-grip-{run_id}")))
                    .track_focus(&handle)
                    .size(px(20.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_color(color("textMuted"))
                    .when(reorderable, |grip| {
                        grip.cursor_grab()
                            .hover(|grip| grip.bg(color("accentSurface")))
                            .on_drag(
                                QueueDrag {
                                    run_id: run_id.clone(),
                                    title: item.title.clone(),
                                },
                                |drag, _, _, cx| cx.new(|_| drag.clone()),
                            )
                            .on_key_down(cx.listener(move |view, event: &KeyDownEvent, _, cx| {
                                let up = match event.keystroke.key.as_str() {
                                    "up" => true,
                                    "down" => false,
                                    _ => return,
                                };
                                cx.stop_propagation();
                                if let Some(target) = queue_step_target(&ids, &moved, up) {
                                    view.move_queued_run(moved.clone(), target);
                                }
                            }))
                    })
                    .tooltip(|window, cx| {
                        Tooltip::new("Reorder queued message (drag, or press the arrow keys)")
                            .build(window, cx)
                    })
                    .child(icon("grip-vertical").size(px(12.)))
            });
        let server_index = item
            .run_id
            .as_ref()
            .and_then(|run_id| run_ids.iter().position(|id| id == run_id));
        let row_actions = if item.editing {
            actions().child(
                Button::new(SharedString::from(format!("queue-cancel-{}", item.key)))
                    .ghost()
                    .xsmall()
                    .label("Cancel")
                    .accessibility_label("Cancel editing queued message")
                    .on_click(cx.listener(|view, _: &ClickEvent, _, _| {
                        view.perform(Intent::Queue {
                            action: QueueAction::CancelEdit,
                        })
                    })),
            )
        } else {
            let run_id = item.run_id.clone().unwrap_or_default();
            let (edit, steer, cancel) = (run_id.clone(), run_id.clone(), run_id);
            actions()
                .child(
                    ghost(SharedString::from(format!("queue-edit-{}", item.key)))
                        .icon(icon("pencil"))
                        .disabled(!item.controls.can_edit)
                        .accessibility_label("Edit queued message")
                        .tooltip(item.edit_tooltip.clone())
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.perform(Intent::Queue {
                                action: QueueAction::Edit {
                                    run_id: edit.clone(),
                                },
                            })
                        })),
                )
                .child(
                    ghost(SharedString::from(format!("queue-steer-{}", item.key)))
                        .icon(icon("corner-up-right"))
                        .label("Steer")
                        .disabled(!item.controls.can_steer)
                        .accessibility_label(item.steer_accessibility_label.clone())
                        .tooltip(item.steer_tooltip.clone())
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.perform(Intent::Queue {
                                action: QueueAction::Steer {
                                    run_id: steer.clone(),
                                },
                            })
                        })),
                )
                .child(
                    ghost(SharedString::from(format!("queue-remove-{}", item.key)))
                        .icon(icon("x"))
                        .disabled(!item.controls.can_dismiss)
                        .accessibility_label(item.controls.dismiss_accessibility_label.clone())
                        .tooltip("Remove from queue")
                        .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                            view.perform(Intent::Queue {
                                action: QueueAction::Cancel {
                                    run_id: cancel.clone(),
                                },
                            })
                        })),
                )
        };
        let thumbnails: Vec<AnyElement> = item
            .thumbnails
            .iter()
            .map(|thumbnail| {
                let source = self.attachment_image(&thumbnail.attachment_id);
                div()
                    .size(px(16.))
                    .flex_none()
                    .overflow_hidden()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(color("border").opacity(0.7))
                    .bg(color("canvas"))
                    .children(source.map(|path| img(path).size_full().object_fit(ObjectFit::Cover)))
                    .into_any_element()
            })
            .collect();
        let last = run_ids.len();
        row()
            .id(SharedString::from(format!("queue-row-{}", item.key)))
            .relative()
            .rounded_sm()
            .when(item.editing, |row| {
                row.bg(color("accentSurface"))
                    .text_color(color("accentSurfaceForeground"))
            })
            .when(dragging, |row| row.opacity(0.5))
            .when_some(server_index, |row, server_index| {
                row.on_drag_move(cx.listener(
                    move |view, event: &DragMoveEvent<QueueDrag>, _, cx| {
                        let bounds = event.bounds;
                        let position = event.event.position;
                        let dragged = event.drag(cx).run_id.clone();
                        if view.composer.banners.queue_dragging.as_ref() != Some(&dragged) {
                            view.composer.banners.queue_dragging = Some(dragged);
                            cx.notify();
                        }
                        if !bounds.contains(&position) {
                            return;
                        }
                        let insert = if position.y < bounds.center().y {
                            server_index
                        } else {
                            server_index + 1
                        };
                        if view.composer.banners.queue_insert != Some(insert) {
                            view.composer.banners.queue_insert = Some(insert);
                            cx.notify();
                        }
                    },
                ))
                .on_drop({
                    let ids = run_ids.to_vec();
                    cx.listener(move |view, drag: &QueueDrag, _, cx| {
                        let insert = view.composer.banners.queue_insert.take();
                        if let Some(target) = insert
                            .and_then(|insert| queue_insert_target(&ids, &drag.run_id, insert))
                        {
                            view.move_queued_run(drag.run_id.clone(), target);
                        }
                        cx.notify();
                    })
                })
                .when(insert == Some(server_index), |row| {
                    row.child(drop_line(true))
                })
                .when(server_index + 1 == last && insert == Some(last), |row| {
                    row.child(drop_line(false))
                })
            })
            .child(icon_cell(None).children(grip))
            .child(
                content()
                    .text_color(color("text").opacity(0.8))
                    .when(item.pending, |content| {
                        content.child(
                            icon("clock")
                                .size(px(12.))
                                .text_color(color("textMuted").opacity(0.6)),
                        )
                    })
                    .when(!thumbnails.is_empty(), |content| {
                        content.child(h_flex().flex_none().gap(px(2.)).children(thumbnails))
                    })
                    .child(truncated(
                        SharedString::from(format!("queue-preview-{}", item.key)),
                        item.preview.clone(),
                    )),
            )
            .child(row_actions)
            .into_any_element()
    }

    fn move_queued_run(&self, run_id: String, target: QueueDropTarget) {
        self.perform(Intent::Queue {
            action: QueueAction::Move {
                run_id,
                before_run_id: match target {
                    QueueDropTarget::Before { run_id } => Some(run_id),
                    QueueDropTarget::End => None,
                },
            },
        });
    }

    fn composer_approval(&mut self, approval: &ApprovalView, cx: &mut Context<Self>) -> Piece {
        let mono = cx.theme().mono_font_family.clone();
        let detail = approval
            .unavailable_notice
            .clone()
            .unwrap_or_else(|| approval.detail.clone());
        let responding = approval.responding;
        let buttons = approval.actions.iter().map(|action| {
            let (request_id, decision) = (approval.request_id.clone(), action.decision.clone());
            let button = Button::new(SharedString::from(format!("approval-{}", action.decision)))
                .xsmall()
                .label(action.label.clone())
                .disabled(!action.enabled || responding || !approval.can_respond)
                .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                    view.perform(Intent::RespondApproval {
                        request_id: request_id.clone(),
                        decision: decision.clone(),
                    })
                }));
            match action.tone {
                ApprovalTone::Primary => button.primary(),
                ApprovalTone::Danger => button.danger(),
                ApprovalTone::Secondary => button.outline(),
            }
        });
        let more = (!approval.more_actions.is_empty()).then(|| {
            let view = cx.entity().downgrade();
            let entries = approval.more_actions.clone();
            let request_id = approval.request_id.clone();
            Button::new("approval-more")
                .icon(icon("ellipsis"))
                .outline()
                .xsmall()
                .disabled(responding)
                .accessibility_label("More approval options")
                .dropdown_menu_with_anchor(Anchor::BottomRight, move |mut menu, _, _| {
                    for entry in &entries {
                        let (request_id, decision) = (request_id.clone(), entry.decision.clone());
                        menu = menu.item(
                            PopupMenuItem::new(entry.label.clone())
                                .disabled(!entry.enabled)
                                .on_click(on_click(&view, move |view, _, _| {
                                    view.perform(Intent::RespondApproval {
                                        request_id: request_id.clone(),
                                        decision: decision.clone(),
                                    })
                                })),
                        );
                    }
                    menu
                })
        });
        Piece {
            warning: true,
            density: Density::Spacious,
            fill: true,
            content: h_flex()
                .w_full()
                .min_w_0()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .w(px(ICON_COLUMN))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .pt(px(2.))
                        .child(icon("shield").size(px(16.)).text_color(color("warning"))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            text_2xs(h_flex().w_full().min_w_0().gap_2())
                                .text_color(color("textMuted"))
                                .child(
                                    div()
                                        .flex_none()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(color("warning"))
                                        .child(approval.label.clone()),
                                )
                                .when_some(approval.app_name.clone(), |label, app| {
                                    label.child(div().min_w_0().truncate().child(app))
                                })
                                .when_some(approval.counter.clone(), |label, counter| {
                                    label.child(div().ml_auto().flex_none().child(counter))
                                }),
                        )
                        .child(
                            div()
                                .id("approval-detail")
                                .w_full()
                                .max_h(px(80.))
                                .overflow_y_scroll()
                                .text_xs()
                                .text_color(color("text"))
                                .when(approval.detail_monospace, |detail| {
                                    detail.font_family(mono).whitespace_nowrap()
                                })
                                .child(detail),
                        ),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .self_center()
                        .gap(px(6.))
                        .children(buttons)
                        .children(more),
                )
                .into_any_element(),
        }
    }

    fn composer_questions(&mut self, questions: &QuestionsView, cx: &mut Context<Self>) -> Piece {
        let Some(active) = questions.active.clone() else {
            return Piece::new(div());
        };
        let collapsed =
            self.composer.banners.collapsed_question.as_deref() == Some(active.id.as_str());
        let responding = questions.responding || !questions.can_respond;
        let question_id = active.id.clone();
        let header = row()
            .id("question-header")
            .cursor_pointer()
            .rounded_md()
            .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                let banners = &mut view.composer.banners;
                banners.collapsed_question =
                    if banners.collapsed_question.as_ref() == Some(&question_id) {
                        None
                    } else {
                        Some(question_id.clone())
                    };
                cx.notify();
            }))
            .child(icon_cell(None))
            .child(
                content()
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color("textMuted"))
                            .child(active.header.clone()),
                    )
                    .when(collapsed, |content| {
                        content.child(
                            div()
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_color(color("textMuted"))
                                .child(active.question.clone()),
                        )
                    }),
            )
            .child(
                actions()
                    .when_some(questions.counter.clone(), |actions, counter| {
                        actions.child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(color("textMuted"))
                                .child(counter),
                        )
                    })
                    .child(toggle_icon(!collapsed))
                    .when(questions.dismissible, |actions| {
                        let request_id = questions.request_id.clone();
                        actions.child(
                            ghost("dismiss-question")
                                .icon(icon("x"))
                                .disabled(questions.responding)
                                .accessibility_label("Dismiss question without answering")
                                .tooltip("Dismiss question without answering")
                                .on_click(cx.listener(move |view, _: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    view.perform(Intent::DismissInput {
                                        request_id: request_id.clone(),
                                    })
                                })),
                        )
                    }),
            );
        let options = active.options.iter().map(|option| {
            let (request_id, question_id, value) = (
                questions.request_id.clone(),
                active.id.clone(),
                option.value.clone(),
            );
            let multiple = active.multiple;
            h_flex()
                .id(SharedString::from(format!("option-{}", option.value)))
                .w_full()
                .items_center()
                .gap_2()
                .rounded(px(8.))
                .px(px(10.))
                .py_2()
                .map(|option_row| {
                    if option.selected {
                        option_row.bg(tint("muted", 0.55)).text_color(color("text"))
                    } else {
                        option_row
                            .text_color(color("text").opacity(0.85))
                            .hover(|row| row.bg(tint("muted", 0.3)))
                    }
                })
                .when(responding, |row| row.opacity(0.5))
                .when(!responding, |row| {
                    row.cursor_pointer().on_click(cx.listener(
                        move |view, _: &ClickEvent, window, cx| {
                            view.choose_answer_option(
                                request_id.clone(),
                                question_id.clone(),
                                value.clone(),
                                multiple,
                                window,
                                cx,
                            )
                        },
                    ))
                })
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(option.label.clone()),
                        )
                        .when_some(option.description.clone(), |column, description| {
                            column.child(
                                text_2xs(div())
                                    .text_color(color("textMuted"))
                                    .child(description),
                            )
                        }),
                )
                .child(if option.selected {
                    icon("check")
                        .size(px(14.))
                        .text_color(color("accent"))
                        .into_any_element()
                } else {
                    div()
                        .size(px(20.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(color("textMuted"))
                        .children(option.shortcut.map(|key| key.to_string()))
                        .into_any_element()
                })
        });
        let body = v_flex()
            .id("question-body")
            .max_h(px(384.))
            .overflow_y_scroll()
            .pl(px(28.))
            .pr_1()
            .pb_1()
            .child(
                div()
                    .text_sm()
                    .text_color(color("text").opacity(0.85))
                    .child(active.question.clone()),
            )
            .when_some(active.hint.clone(), |body, hint| {
                body.child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(color("textMuted"))
                        .child(hint),
                )
            })
            .when_some(questions.unavailable_notice.clone(), |body, notice| {
                body.child(
                    div()
                        .mt_1()
                        .text_xs()
                        .text_color(color("warningForeground"))
                        .child(notice),
                )
            })
            .child(v_flex().mt_2().gap(px(2.)).children(options));
        let keys = active.clone();
        let (request_id, multiple) = (questions.request_id.clone(), active.multiple);
        Piece::new(
            v_flex()
                .w_full()
                .track_focus(&self.composer.banners.question_focus)
                .on_key_down(cx.listener(move |view, event: &KeyDownEvent, window, cx| {
                    let modifiers = event.keystroke.modifiers;
                    if responding
                        || collapsed
                        || modifiers.secondary()
                        || modifiers.control
                        || modifiers.alt
                    {
                        return;
                    }
                    let Some(option) =
                        event.keystroke.key.parse::<u32>().ok().and_then(|digit| {
                            keys.options.iter().find(|o| o.shortcut == Some(digit))
                        })
                    else {
                        return;
                    };
                    cx.stop_propagation();
                    view.choose_answer_option(
                        request_id.clone(),
                        keys.id.clone(),
                        option.value.clone(),
                        multiple,
                        window,
                        cx,
                    );
                }))
                .child(header)
                .when(!collapsed, |panel| panel.child(body)),
        )
    }

    /// Picks an option; a single-choice question moves on shortly after.
    fn choose_answer_option(
        &mut self,
        request_id: String,
        question_id: String,
        value: String,
        multiple: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.perform(Intent::EditAnswer {
            request_id: request_id.clone(),
            question_id: question_id.clone(),
            edit: AnswerEdit::ToggleOption { value },
        });
        if multiple {
            return;
        }
        cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let _ = view.update_in(cx, |view, window, cx| {
                let still_active =
                    shown_thread(&view.views, view.snapshot.selected_thread.as_ref())
                        .and_then(|thread| thread.requests.questions.as_ref())
                        .is_some_and(|questions| {
                            questions.request_id == request_id
                                && questions
                                    .active
                                    .as_ref()
                                    .is_some_and(|active| active.id == question_id)
                        });
                if still_active {
                    view.advance_answer(window, cx);
                }
            });
        })
        .detach();
    }

    /// Shows the next question, or submits the answers after the last.
    pub(super) fn advance_answer(&mut self, _: &mut Window, _: &mut Context<Self>) {
        let Some(questions) = shown_thread(&self.views, self.snapshot.selected_thread.as_ref())
            .and_then(|thread| thread.requests.questions.as_ref())
        else {
            return;
        };
        let request_id = questions.request_id.clone();
        let progress = &questions.progress;
        if progress.is_last_question {
            if questions.submit_enabled {
                self.perform(Intent::SubmitAnswers { request_id });
            }
        } else if progress.can_advance {
            self.perform(Intent::ShowQuestion {
                request_id,
                index: progress.question_index + 1,
            });
        }
    }

    pub(super) fn previous_answer_question(&mut self) {
        if let Some(questions) = shown_thread(&self.views, self.snapshot.selected_thread.as_ref())
            .and_then(|thread| thread.requests.questions.as_ref())
            .filter(|questions| questions.progress.question_index > 0)
        {
            self.perform(Intent::ShowQuestion {
                request_id: questions.request_id.clone(),
                index: questions.progress.question_index - 1,
            });
        }
    }

    fn composer_tasks(&mut self, plan: &PlanView, cx: &mut Context<Self>) -> Option<Piece> {
        let progress = plan.tasks.as_ref().filter(|tasks| tasks.total_steps > 0)?;
        let steps = &plan.active_plan.as_ref()?.steps;
        let open = self.composer.banners.tasks_open;
        let done = progress.completed_steps >= progress.total_steps;
        let segments = (2..=10).contains(&steps.len()).then(|| {
            h_flex()
                .w(px(80.))
                .flex_none()
                .gap(px(2.))
                .children(steps.iter().map(|step| {
                    div()
                        .h(px(3.))
                        .flex_1()
                        .rounded_full()
                        .bg(match step.status {
                            PlanStepStatus::Completed => color("successForeground"),
                            PlanStepStatus::InProgress => color("accent"),
                            PlanStepStatus::Pending => color("textMuted").opacity(0.25),
                        })
                }))
        });
        let summary = row()
            .id("tasks-summary")
            .cursor_pointer()
            .rounded_md()
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.composer.banners.tasks_open = !view.composer.banners.tasks_open;
                cx.notify();
            }))
            .child(icon_cell(Some("list-todo")))
            .child(
                content()
                    .child(
                        div()
                            .flex_none()
                            .text_color(color("textMuted"))
                            .child("Tasks"),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(color("text").opacity(0.8))
                            .child(progress.step.clone()),
                    ),
            )
            .child(
                actions()
                    .child(
                        count(format!(
                            "{}/{}",
                            progress.completed_steps, progress.total_steps
                        ))
                        .when(done, |count| count.text_color(color("successForeground"))),
                    )
                    .children(segments)
                    .child(toggle_icon(open)),
            );
        if !open {
            return Some(Piece {
                density: Density::Comfortable,
                ..Piece::new(summary)
            });
        }
        let list = steps.iter().map(|step| {
            let (name, tone, text) = match step.status {
                PlanStepStatus::Completed => (
                    "check",
                    color("successForeground"),
                    color("textMuted").opacity(0.55),
                ),
                PlanStepStatus::InProgress => {
                    ("circle-dot", color("accent"), color("text").opacity(0.9))
                }
                PlanStepStatus::Pending => (
                    "circle",
                    color("textMuted").opacity(0.4),
                    color("textMuted").opacity(0.7),
                ),
            };
            row()
                .items_start()
                .py_1()
                .pr_2()
                .child(
                    icon_cell(None)
                        .h(px(16.))
                        .child(icon(name).size(px(12.)).text_color(tone)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(text)
                        .child(step.step.clone()),
                )
                .child(
                    div()
                        .w(px(48.))
                        .flex_none()
                        .text_right()
                        .text_size(px(10.))
                        .text_color(color("textMuted").opacity(0.45))
                        .when(step.status == PlanStepStatus::InProgress, |cell| {
                            cell.child("now")
                        }),
                )
        });
        Some(Piece::new(
            v_flex().w_full().child(summary).child(
                v_flex()
                    .id("tasks-list")
                    .max_h(px(384.))
                    .overflow_y_scroll()
                    .gap(px(1.))
                    .children(list),
            ),
        ))
    }

    fn composer_stash_badge(&mut self, entries: usize, cx: &mut Context<Self>) -> Piece {
        let open = self.composer.banners.stash_open;
        Piece {
            density: Density::Comfortable,
            fill: false,
            ..Piece::new(
                row()
                    .id("stash-badge")
                    .cursor_pointer()
                    .pr_1()
                    .text_color(color(if open { "text" } else { "textMuted" }))
                    .hover(|row| row.text_color(color("text")))
                    .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                        view.composer.banners.stash_open = !view.composer.banners.stash_open;
                        cx.notify();
                    }))
                    .child(icon_cell(Some("bookmark")))
                    .child(div().flex_none().child("Stash"))
                    .child(count(entries)),
            )
        }
    }

    /// Moves the stash highlight; false when the stash is closed.
    pub(super) fn step_stash_menu(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
        let entries = self
            .shown_composer()
            .map_or(0, |composer| composer.stash.len());
        if !self.composer.banners.stash_open {
            return false;
        }
        if entries > 0 {
            let current = self.composer.banners.stash_highlight.min(entries - 1);
            self.composer.banners.stash_highlight = if down {
                (current + 1) % entries
            } else {
                (current + entries - 1) % entries
            };
            cx.notify();
        }
        true
    }

    /// Restores the highlighted stash entry; false when the stash is closed.
    pub(super) fn restore_highlighted_stash(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.composer.banners.stash_open {
            return false;
        }
        let entry = self.shown_composer().and_then(|composer| {
            composer
                .stash
                .get(
                    self.composer
                        .banners
                        .stash_highlight
                        .min(composer.stash.len().saturating_sub(1)),
                )
                .map(|entry| entry.id.clone())
        });
        if let Some(entry_id) = entry {
            self.restore_stash_entry(entry_id);
        }
        cx.notify();
        true
    }

    fn restore_stash_entry(&mut self, entry_id: String) {
        self.composer.banners.stash_open = false;
        self.perform(Intent::RestoreStash { entry_id });
    }

    fn composer_stash_menu(
        &mut self,
        entries: &[StashEntryView],
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Piece {
        let highlight = self
            .composer
            .banners
            .stash_highlight
            .min(entries.len().saturating_sub(1));
        let now_ms = self.views.now_ms;
        let header = row()
            .id("stash-header")
            .cursor_pointer()
            .rounded_md()
            .on_click(cx.listener(|view, _: &ClickEvent, _, cx| {
                view.composer.banners.stash_open = false;
                cx.notify();
            }))
            .child(icon_cell(Some("bookmark")))
            .child(content().text_color(color("textMuted")).child("Stash"))
            .child(
                actions()
                    .child(count(entries.len()))
                    .child(toggle_icon(true)),
            );
        let rows: Vec<AnyElement> = if entries.is_empty() {
            vec![
                row()
                    .child(icon_cell(None))
                    .child(content().text_color(color("textMuted")).child(format!(
                        "Nothing stashed yet. Press {} with a prompt in the composer to stash it.",
                        shortcut("S")
                    )))
                    .into_any_element(),
            ]
        } else {
            entries
                .iter()
                .enumerate()
                .map(|(index, entry)| {
                    let thumbnails = self.stash_entry_thumbnails(entry);
                    let (restore, delete) = (entry.id.clone(), entry.id.clone());
                    row()
                        .id(SharedString::from(format!("stash-{}", entry.id)))
                        .rounded_sm()
                        .when(index == highlight, |row| {
                            row.bg(color("accentSurface"))
                                .text_color(color("accentSurfaceForeground"))
                        })
                        .on_mouse_move(cx.listener(move |view, _: &MouseMoveEvent, _, cx| {
                            if view.composer.banners.stash_highlight != index {
                                view.composer.banners.stash_highlight = index;
                                cx.notify();
                            }
                        }))
                        .child(icon_cell(Some("file-text")))
                        .child(
                            content().child(
                                div()
                                    .id(SharedString::from(format!("stash-restore-{}", entry.id)))
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .cursor_pointer()
                                    .text_color(color("text").opacity(0.8))
                                    .on_click(cx.listener(move |view, _: &ClickEvent, _, _| {
                                        view.restore_stash_entry(restore.clone())
                                    }))
                                    .child(entry.snippet.clone()),
                            ),
                        )
                        .child(
                            actions()
                                .when_some(entry.status.clone(), |actions, status| {
                                    actions.child(
                                        div()
                                            .flex_none()
                                            .text_color(color(if entry.status_warning {
                                                "warningForeground"
                                            } else {
                                                "textMuted"
                                            }))
                                            .child(status),
                                    )
                                })
                                .when(!thumbnails.is_empty(), |actions| {
                                    actions.child(h_flex().flex_none().children(
                                        thumbnails.into_iter().enumerate().map(|(index, image)| {
                                            img(image)
                                                .size(px(16.))
                                                .when(index > 0, |image| image.ml(px(-6.)))
                                                .rounded(px(4.))
                                                .border_1()
                                                .border_color(color("border").opacity(0.7))
                                                .object_fit(ObjectFit::Cover)
                                        }),
                                    ))
                                })
                                .when(entry.file_count > 0, |actions| {
                                    actions.child(
                                        h_flex()
                                            .flex_none()
                                            .gap_1()
                                            .text_color(color("textMuted"))
                                            .child(icon("file").size(px(12.)))
                                            .child(entry.file_count.to_string()),
                                    )
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(color("textMuted"))
                                        .child(relative_time_label(entry.created_at_ms, now_ms)),
                                )
                                .child(
                                    ghost(SharedString::from(format!("stash-delete-{}", entry.id)))
                                        .icon(icon("x"))
                                        .accessibility_label("Delete stashed prompt")
                                        .on_click(cx.listener(
                                            move |view, _: &ClickEvent, _, _| {
                                                view.perform(Intent::DeleteStash {
                                                    entry_id: delete.clone(),
                                                })
                                            },
                                        )),
                                ),
                        )
                        .into_any_element()
                })
                .collect()
        };
        Piece::new(
            v_flex().w_full().child(header).child(
                v_flex()
                    .id("stash-entries")
                    .max_h(px(384.))
                    .overflow_y_scroll()
                    .gap(px(1.))
                    .children(rows),
            ),
        )
    }

    /// The decoded thumbnails of a stash entry, decoded once.
    fn stash_entry_thumbnails(&mut self, entry: &StashEntryView) -> Vec<Arc<Image>> {
        self.composer
            .banners
            .stash_thumbnails
            .entry(entry.id.clone())
            .or_insert_with(|| {
                entry
                    .thumbnails
                    .iter()
                    .filter_map(|url| {
                        let (header, payload) = url.split_once(',')?;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(payload)
                            .ok()?;
                        let format = if header.contains("image/png") {
                            ImageFormat::Png
                        } else if header.contains("image/webp") {
                            ImageFormat::Webp
                        } else if header.contains("image/gif") {
                            ImageFormat::Gif
                        } else {
                            ImageFormat::Jpeg
                        };
                        Some(Arc::new(Image::from_bytes(format, bytes)))
                    })
                    .collect()
            })
            .clone()
    }
}

/// Where a dragged queue row would land: above this row, or below the last.
fn drop_line(top: bool) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(px(2.))
        .rounded_full()
        .bg(color("accent").opacity(0.7))
        .map(|line| if top { line.top_0() } else { line.bottom_0() })
}

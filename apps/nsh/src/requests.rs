//! Requests wait on the composer's top edge as T3 Code's attached banners
//! (`ComposerBanner`): an approval's label, reason and decisions, an input
//! request's answer, and the question the composer answers, with its choices.
use crate::l10n::{language, localized, tr};
use crate::ui::{self, Enabled, Icon, colors};
use crate::{Connection, native};
use agent_core::{
    presentation::conversation::{
        ActionTone, ApprovalAction, Request, answer_from_json, approval_actions,
        request_input_default,
    },
    questions::{QuestionAction, QuestionPrompt},
    state::{Intent, operations as op},
};
use agent_protocol::{
    ids::RequestId,
    requests::{Answer, ElicitationAnswer, ElicitationInput, RequestBody},
};
use nsh_core::{
    element::{Rgba, input, rgb, rgba},
    prelude::*,
};

const ERROR: Rgba = rgb(0xff8e86);
/// How far in from the composer's sides the banner stands (T3's 1.375rem
/// `--chat-composer-drawer-inset`, measured from inside the composer's outline).
const INSET: f32 = 21.0;
/// The banner's fill: T3's raised surface at its glass opacity over the page.
const SURFACE: Rgba = rgb(0x101010);
/// A waiting approval's fill: the surface under 8% of T3's warning.
const WARNING_SURFACE: Rgba = rgb(0x231b0f);
const WARNING_OUTLINE: Rgba = rgba(0xfe9a0047);
const NEUTRAL_OUTLINE: Rgba = rgba(0xffffff0d);
/// T3's `--composer-banner-icon-column` beside a row's content.
const ICON_COLUMN: f32 = 24.0;

/// The banner waiting on the composer: an approval or input request first, as
/// T3 Code's top drawer, else the question the composer answers. Placed right
/// above the composer without overlapping its input.
pub(crate) fn panels(following: Signal<bool>) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let docked = move || connection.context.with(|p| p.docked_request().is_some());
    view()
        .child(each(
            move || {
                connection
                    .context
                    .with(|p| p.docked_request().into_iter().collect::<Vec<_>>())
            },
            |request: &Request| request.id.clone(),
            move |request, _| request_banner(connection, following, request),
        ))
        .child(each(
            move || {
                if docked() {
                    return Vec::new();
                }
                connection
                    .composer
                    .with(|p| p.controls.question.clone().into_iter().collect::<Vec<_>>())
            },
            |prompt: &QuestionPrompt| (prompt.request_id.clone(), prompt.question.id.clone()),
            move |prompt, _| question_banner(connection, following, prompt),
        ))
}

/// A banner inset from the composer's sides, with its whole surface above it.
fn attached(fill: Rgba, outline: Rgba) -> View {
    view()
        .mx(px(INSET))
        .rounded(px(18.0))
        .border_1()
        .border_color(outline)
        .bg(fill)
}

/// Whether the request's answer is being sent.
fn sending(connection: Connection, id: RequestId) -> bool {
    connection.publication.with(|p| {
        matches!(
            p.context.request_phase(id),
            Some(op::OperationPhase::Running)
        )
    })
}

/// The failure of the request's last answer, below its banner's row while
/// there is one.
fn request_error(
    connection: Connection,
    id: impl Fn() -> RequestId + Copy + 'static,
) -> impl IntoElement {
    let error = computed(move || {
        connection
            .context
            .with(|p| p.request_error(id()).map(|error| error.to_string()))
    });
    when(
        move || error.with(Option::is_some),
        move || {
            text(move || error.get().unwrap_or_default())
                .selectable(true)
                .text_size(px(12.0))
                .text_color(ERROR)
        },
    )
}

/// T3 Code's approval row (`ComposerPendingApprovalPanel` in a spacious
/// warning banner): the shield, the request's label over its one-line
/// summary, and its decisions; an input request's answer below.
fn request_banner(
    connection: Connection,
    following: Signal<bool>,
    request: ReadSignal<Request>,
) -> impl IntoElement {
    let response = signal(request_input_default(
        request.with_untracked(|r| r.request_body.clone()),
    ));
    let error = signal(None::<String>);
    let enabled = move || {
        connection.composer.with(|p| p.controls.connected)
            && request.with(|request| request.can_respond)
            && !sending(connection, request.with(|request| request.id.clone()))
    };
    let respond = move |answer| {
        following.set(true);
        connection.dispatch(Intent::Respond(op::Respond {
            request_id: request.with_untracked(|r| r.id.clone()),
            answer,
        }))
    };
    let submit = move || match answer_from_json(
        &request.with_untracked(|r| r.request_body.clone()),
        &response.get_untracked(),
    ) {
        Ok(answer) => {
            error.set(None);
            respond(answer);
        }
        Err(reason) => error.set(Some(localized(reason))),
    };
    let answered = move || {
        request.with(|r| {
            matches!(
                r.request_body,
                RequestBody::Elicitation { .. } | RequestBody::ToolExecution { .. }
            )
        })
    };
    attached(WARNING_SURFACE, WARNING_OUTLINE)
        .px_3()
        .pt_3()
        .pb(px(12.0))
        .gap_3()
        .child(
            view()
                .flex_row()
                .items_start()
                .gap_2()
                .child(
                    view()
                        .w(px(ICON_COLUMN))
                        .pt(px(2.0))
                        .items_center()
                        .text_color(colors::WARNING)
                        .child(ui::icon(Icon::SHIELD, 16.0)),
                )
                .child(
                    view()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(
                            text(move || request.with(|r| r.title.clone()))
                                .text_size(px(11.0))
                                .line_height(px(16.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::WARNING)
                                .number_of_lines(1),
                        )
                        .child(
                            scroll().max_h(px(80.0)).child(
                                text(move || request.with(|r| r.summary().to_owned()))
                                    .selectable(true)
                                    .font_family(SharedString::from("monospace"))
                                    .text_size(px(12.0))
                                    .line_height(px(16.0))
                                    .text_color(colors::t3_foreground()),
                            ),
                        ),
                )
                .child(decisions(request, enabled, respond)),
        )
        .child(when(answered, move || {
            answer_form(request, response, enabled, submit, respond)
        }))
        .child(when(
            move || error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .selectable(true)
                    .text_size(px(12.0))
                    .text_color(ERROR)
            },
        ))
        .child(request_error(connection, move || {
            request.with(|request| request.id.clone())
        }))
}

/// An approval's or a permission's decisions as T3 Code lays them out:
/// declining outlined, approving once filled, the rest in a menu after them.
fn decisions(
    request: ReadSignal<Request>,
    enabled: impl Fn() -> bool + Copy + 'static,
    respond: impl Fn(Answer) + Copy + 'static,
) -> impl IntoElement {
    let actions =
        computed(move || request.with(|r| approval_actions(language(), r.request_body.choices())));
    let answer = move |choice_id: String| {
        respond(
            if request.with_untracked(|r| matches!(r.request_body, RequestBody::Permission { .. }))
            {
                Answer::Permission { choice_id }
            } else {
                Answer::Approval { choice_id }
            },
        )
    };
    let button = move |action: Option<ApprovalAction>, tone| {
        action.map(|action| {
            let ApprovalAction { choice_id, label } = action;
            small_button(label, tone, move || answer(choice_id.clone())).enabled(enabled)
        })
    };
    view()
        .self_center()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .child(dynamic(move || {
            button(
                actions.with(|a| a.decline.clone()),
                ActionTone::Secondary,
            )
        }))
        .child(dynamic(move || {
            button(actions.with(|a| a.approve.clone()), ActionTone::Primary)
        }))
        .child(when(
            move || actions.with(|a| !a.more.is_empty()),
            move || {
                outlined(
                    tr("その他の承認方法"),
                    view().child(ui::icon(Icon::ELLIPSIS, 14.0)),
                    move || {
                        let items = actions.with_untracked(|a| {
                            serde_json::json!(
                                a.more
                                    .iter()
                                    .map(|action| serde_json::json!({"id": action.choice_id, "title": action.label}))
                                    .collect::<Vec<_>>()
                            )
                        });
                        ui::popup(&items, move |id| answer(id.to_owned()));
                    },
                )
                .w(px(24.0))
                .enabled(enabled)
            },
        ))
}

/// T3 Code's extra-small button (`Button size="xs"`): 24 points, 12-point
/// medium text, filled in the primary colour or outlined.
fn small_button(
    title: String,
    tone: ActionTone,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    let label = text(title.clone())
        .text_size(px(12.0))
        .line_height(px(16.0))
        .font_weight(FontWeight::MEDIUM)
        .number_of_lines(1);
    if tone == ActionTone::Primary {
        return ui::bare_button(SharedString::from(title), press)
            .h(px(24.0))
            .px(px(7.0))
            .rounded(px(8.0))
            .border_1()
            .border_color(colors::ACCENT)
            .bg(colors::ACCENT)
            .text_color(rgb(0xffffff))
            .hover(|s| s.opacity(0.9))
            .child(label);
    }
    outlined(SharedString::from(title), view().child(label), press).px(px(7.0))
}

/// T3 Code's outlined control: a faint fill inside its input-coloured edge.
fn outlined(
    label: impl IntoReactive<SharedString>,
    content: View,
    press: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    ui::bare_button(label, press)
        .h(px(24.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(colors::T3_INPUT)
        .bg(rgba(0xffffff07))
        .text_color(colors::t3_foreground())
        .hover(|s| s.bg(rgba(0xffffff14)))
        .child(content)
}

/// An input request's answer: its link or field, then sending, declining or
/// cancelling it.
fn answer_form(
    request: ReadSignal<Request>,
    response: Signal<String>,
    enabled: impl Fn() -> bool + Copy + 'static,
    submit: impl Fn() + 'static,
    respond: impl Fn(Answer) + Copy + 'static,
) -> impl IntoElement {
    let url = computed(move || {
        request.with(|r| match &r.request_body {
            RequestBody::Elicitation {
                input: ElicitationInput::Url { url },
                ..
            } => Some(url.clone()),
            _ => None,
        })
    });
    let elicitation =
        move || request.with(|r| matches!(r.request_body, RequestBody::Elicitation { .. }));
    let refusal = move |label: &'static str, tone, action: ElicitationAnswer| {
        ui::action_button(label, tone, move || {
            respond(Answer::Elicitation {
                action: action.clone(),
            })
        })
        .enabled(enabled)
    };
    view()
        .pl(px(ICON_COLUMN + 8.0))
        .gap_3()
        .child(when(
            move || url.get().is_some(),
            move || {
                view()
                    .flex_row()
                    .child(ui::text_button(tr("リンクを開く"), move || {
                        if let Some(url) = url.get_untracked() {
                            let opened = open_url(url);
                            spawn(async move {
                                if !opened.await {
                                    native::message(
                                        tr("リンクを開けませんでした"),
                                        tr("OSがリンクを開けませんでした。"),
                                    );
                                }
                            });
                        }
                    }))
            },
        ))
        .child(when(
            move || url.get().is_none(),
            move || {
                input()
                    .multiline(true)
                    .value(move || response.get())
                    .on_change(move |text: &str| response.set(text.into()))
                    .text_color(move || nsh_base::theme().foreground)
                    .text_size(px(13.0))
                    .h(px(100.0))
                    .p_2()
                    .rounded(px(ui::RADIUS))
                    .border_1()
                    .border_color(colors::border())
            },
        ))
        .child(
            view()
                .flex_row()
                .flex_wrap()
                .gap_2()
                .child(
                    ui::action_button(tr("確認して送信"), ActionTone::Primary, submit)
                        .enabled(enabled),
                )
                .child(when(elicitation, move || {
                    refusal(tr("辞退"), ActionTone::Danger, ElicitationAnswer::Decline)
                }))
                .child(when(elicitation, move || {
                    refusal(
                        tr("キャンセル"),
                        ActionTone::Secondary,
                        ElicitationAnswer::Cancel,
                    )
                })),
        )
}

/// T3 Code's pending question (`ComposerPendingUserInputPanel`): its header,
/// which folds the rest away, and dismissing it unanswered; the prompt and
/// its numbered choices below. The composer answers it.
fn question_banner(
    connection: Connection,
    following: Signal<bool>,
    prompt: ReadSignal<QuestionPrompt>,
) -> impl IntoElement {
    let request_id = move || prompt.with(|p| p.request_id.clone());
    let busy = move || sending(connection, request_id());
    let enabled = move || {
        connection.composer.with(|p| p.controls.connected)
            && prompt.with(|p| p.can_respond)
            && !busy()
    };
    let collapsed = signal(false);
    attached(SURFACE, NEUTRAL_OUTLINE)
        .p_1()
        .pb(px(4.0))
        .child(question_header(prompt, collapsed, busy, move || {
            following.set(true);
            connection.dispatch(Intent::Respond(op::Respond {
                request_id: prompt.with_untracked(|p| p.request_id.clone()),
                answer: Answer::SkipQuestions,
            }));
        }))
        .child(when(
            move || !collapsed.get(),
            move || {
                view()
                    .pl(px(28.0))
                    .pr_1()
                    .pb_1()
                    .child(
                        text(move || prompt.with(|p| p.question.prompt.clone()))
                            .selectable(true)
                            .text_size(px(14.0))
                            .line_height(px(20.0))
                            .text_color(colors::t3_foreground().opacity(0.85)),
                    )
                    .child(when(
                        move || prompt.with(|p| p.question.multiple),
                        || {
                            text(tr("複数選択できます"))
                                .mt_1()
                                .text_size(px(12.0))
                                .line_height(px(16.0))
                                .text_color(colors::t3_muted())
                        },
                    ))
                    .child(view().mt_2().gap(px(2.0)).child(choices(
                        prompt,
                        enabled,
                        move |choice_id| {
                            following.set(true);
                            connection.dispatch(Intent::AnswerQuestion {
                                request_id: prompt.with_untracked(|p| p.request_id.clone()),
                                action: QuestionAction::Choose { choice_id },
                            })
                        },
                    )))
            },
        ))
        .child(request_error(connection, request_id))
}

/// The composer's actions while it answers a question, as T3 Code's: going
/// back to the previous question (outlined), then the pill that moves on or
/// sends the answers.
pub(crate) fn question_actions(
    answering: Memo<Option<QuestionPrompt>>,
    enabled: impl Fn() -> bool + Copy + 'static,
    submit: impl Fn() + 'static,
) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let action = move || {
        SharedString::from(
            answering.with(|q| q.as_ref().map(|q| q.action.clone()).unwrap_or_default()),
        )
    };
    view()
        .flex_row()
        .items_center()
        .gap_2()
        .child(when(
            move || answering.with(|q| q.as_ref().is_some_and(|q| q.can_go_back)),
            move || {
                outlined(
                    tr("前の質問へ"),
                    view().child(
                        text(tr("前の質問へ"))
                            .text_size(px(14.0))
                            .font_weight(FontWeight::MEDIUM)
                            .number_of_lines(1),
                    ),
                    move || {
                        if let Some(question) = answering.get_untracked() {
                            connection.dispatch(Intent::AnswerQuestion {
                                request_id: question.request_id,
                                action: QuestionAction::Back,
                            });
                        }
                    },
                )
                .h(px(28.0))
                .px(px(9.0))
            },
        ))
        .child(
            ui::bare_button(action, submit)
                .h(px(28.0))
                .px_3()
                .rounded_full()
                .bg(colors::ACCENT)
                .text_color(rgb(0xffffff))
                .hover(|s| s.opacity(0.9))
                .enabled(enabled)
                .child(
                    text(action)
                        .text_size(px(14.0))
                        .line_height(px(20.0))
                        .font_weight(FontWeight::MEDIUM)
                        .number_of_lines(1),
                ),
        )
}

/// The question's header row: its title (and, folded, its prompt), how far
/// through the request it is, folding, and dismissing it unanswered.
fn question_header(
    prompt: ReadSignal<QuestionPrompt>,
    collapsed: Signal<bool>,
    busy: impl Fn() -> bool + Copy + 'static,
    dismiss: impl Fn() + 'static,
) -> impl IntoElement {
    let fold_label = move || {
        SharedString::from(if collapsed.get() {
            tr("質問と選択肢を表示")
        } else {
            tr("質問と選択肢を隠す")
        })
    };
    view()
        .flex_row()
        .items_center()
        .gap_1()
        .min_h(px(ICON_COLUMN))
        .child(
            ui::bare_button(fold_label, move || collapsed.update(|c| *c = !*c))
                .flex_1()
                .min_w_0()
                .justify_start()
                .items_center()
                .gap_1()
                .pl(px(ICON_COLUMN + 4.0))
                .min_h(px(ICON_COLUMN))
                .rounded(px(6.0))
                .child(
                    text(move || {
                        prompt.with(|p| {
                            p.status
                                .clone()
                                .unwrap_or_else(|| p.question.header.clone())
                        })
                    })
                    .flex_shrink_0()
                    .text_size(px(12.0))
                    .line_height(px(16.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::t3_muted()),
                )
                .child(when(
                    move || collapsed.get(),
                    move || {
                        text(move || prompt.with(|p| p.question.prompt.clone()))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.0))
                            .line_height(px(16.0))
                            .text_color(colors::t3_muted())
                    },
                )),
        )
        .child(when(
            move || prompt.with(|p| p.progress.is_some()),
            move || {
                text(move || prompt.with(|p| p.progress.clone().unwrap_or_default()))
                    .text_size(px(10.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors::t3_muted())
            },
        ))
        .child(when(busy, ui::spinner))
        .child(
            view()
                .w(px(24.0))
                .h(px(24.0))
                .items_center()
                .justify_center()
                .text_color(colors::t3_muted())
                .child(dynamic(move || {
                    ui::icon(
                        if collapsed.get() {
                            Icon::CHEVRON_UP
                        } else {
                            Icon::CHEVRON_DOWN
                        },
                        14.0,
                    )
                })),
        )
        .child(
            ui::icon_button(Icon::CLOSE, tr("回答せずに閉じる"), dismiss)
                .text_color(colors::t3_muted())
                .enabled(move || !busy()),
        )
}

/// The question's choices as T3 Code's rows: label over description, the
/// number that picks it, or a check once chosen.
fn choices(
    prompt: ReadSignal<QuestionPrompt>,
    enabled: impl Fn() -> bool + Copy + 'static,
    choose: impl Fn(String) + Copy + 'static,
) -> impl IntoElement {
    each(
        move || prompt.with(|p| p.question.choices.clone()),
        |choice: &agent_protocol::requests::QuestionChoice| choice.id.clone(),
        move |choice, index| {
            let chosen = move || {
                prompt.with(|p| p.chosen_choice_ids.contains(&choice.with(|c| c.id.clone())))
            };
            ui::bare_button(
                move || SharedString::from(choice.with(|c| c.label.clone())),
                move || choose(choice.with_untracked(|c| c.id.clone())),
            )
            .flex_row()
            .items_start()
            .gap(px(10.0))
            .w_full()
            .py(px(6.0))
            .pl(px(6.0))
            .rounded(px(6.0))
            .text_color(move || {
                if chosen() {
                    colors::t3_foreground()
                } else {
                    colors::t3_foreground().opacity(0.7)
                }
            })
            .hover(|s| s.bg(rgba(0xffffff05)))
            .disabled_style(|s| s.opacity(0.5))
            .enabled(enabled)
            .child(
                view()
                    .h(px(20.0))
                    .flex_shrink_0()
                    .justify_center()
                    .child(dynamic(move || {
                        choice_mark(prompt.with(|p| p.question.multiple), chosen()).into_element()
                    })),
            )
            .child(
                view()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(
                        text(move || choice.with(|c| c.label.clone()))
                            .text_size(px(14.0))
                            .line_height(px(20.0))
                            .font_weight(FontWeight::MEDIUM),
                    )
                    .child(when(
                        move || {
                            choice.with(|c| !c.description.is_empty() && c.description != c.label)
                        },
                        move || {
                            text(move || choice.with(|c| c.description.clone()))
                                .text_size(px(11.0))
                                .line_height(px(16.0))
                                .text_color(colors::t3_muted())
                        },
                    )),
            )
            .child(when(
                move || index.get() < 9,
                move || {
                    view()
                        .w(px(24.0))
                        .h(px(20.0))
                        .flex_shrink_0()
                        .items_center()
                        .justify_center()
                        .child(
                            view()
                                .h(px(20.0))
                                .min_w(px(20.0))
                                .px(px(4.0))
                                .rounded(px(4.0))
                                .bg(rgba(0xffffff12))
                                .items_center()
                                .justify_center()
                                .child(
                                    text((index.get() + 1).to_string())
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(colors::t3_muted()),
                                ),
                        )
                },
            ))
        },
    )
}

/// T3 Code's `OptionRadio` and `CheckboxMark`: an empty mark, or one filled
/// with the accent around its dot or check.
fn choice_mark(multiple: bool, chosen: bool) -> View {
    let mark = view()
        .w(px(16.0))
        .h(px(16.0))
        .border_1()
        .border_color(colors::T3_INPUT)
        .bg(rgba(0xffffff05))
        .items_center()
        .justify_center();
    let mark = if multiple {
        mark.rounded(px(4.0))
    } else {
        mark.rounded_full()
    };
    match (chosen, multiple) {
        (false, _) => mark,
        (true, true) => mark
            .bg(colors::ACCENT)
            .border_color(colors::ACCENT)
            .text_color(colors::t3_foreground())
            .child(ui::icon(Icon::CHECK, 12.0)),
        (true, false) => mark.bg(colors::ACCENT).border_color(colors::ACCENT).child(
            view()
                .w(px(6.0))
                .h(px(6.0))
                .rounded_full()
                .bg(colors::t3_foreground()),
        ),
    }
}

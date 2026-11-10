use crate::l10n::{localized, tr, tr_format};
use crate::{Connection, native, workbench::Workbench};
use agent_core::state::{Intent, follow_up::QueueAction, operations as op};
use agent_core::{panels::PanelKind, presentation::composer::footer};
use agent_protocol::operations::FollowUp;
use nsh_core::{GlassEffect, element::input, prelude::*};
use serde_json::json;

/// A key bound while this view lives; it takes up no space.
pub(crate) fn shortcut(
    keystroke: &'static str,
    when: nsh_core::keymap::When,
    handler: impl Fn(&str) + 'static,
) -> View {
    on_key_with(
        keystroke,
        KeyOptions {
            when,
            repeat: false,
        },
        handler,
    );
    view()
}

/// The modifier the composer's shortcuts name, as T3 Code's copy does.
pub(crate) fn modifier() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Ctrl"
    }
}

pub(crate) fn composer(following: Signal<bool>) -> impl IntoElement {
    let connection = use_context::<Connection>().unwrap();
    let preview = use_context::<crate::media::Preview>().unwrap();
    let files = use_context::<crate::files::Browser>().unwrap();
    // What an input method composes. NSH reports the field's text only once
    // a composition commits, so Core's draft lacks it until then.
    let composition = signal(None::<String>);
    let composed = move || {
        composition.with(|text| text.as_deref().is_some_and(|text| !text.trim().is_empty()))
    };
    // A send pressed while composing, waiting for the composition to commit.
    let send_composed = signal(false);
    let focused = signal(false);
    let field_ref = connection.composer_ref;
    let file_pick = signal(0_u64);
    let file_target = stored(None::<(String, agent_core::state::DraftKey)>);
    #[cfg(dictation)]
    let dictation = crate::dictation::Dictation::new();
    #[cfg(dictation)]
    let (recording, requesting) = (dictation.active, dictation.requesting);
    #[cfg(not(dictation))]
    let (recording, requesting) = (signal(false), signal(false));
    let composer = connection.composer;
    let controls = computed(move || composer.with(|c| c.controls.clone()));
    let draft = computed(move || composer.with(|c| c.draft.text.clone()));
    let answering = computed(move || {
        if recording.get() || requesting.get() || controls.with(|c| c.transcribing) {
            None
        } else {
            controls.with(|c| c.question.clone())
        }
    });
    // A question's answer has no attachments; the draft keeps them.
    let can_attach = move || {
        composer.with(|c| c.can_attach())
            && !recording.get()
            && !requesting.get()
            && !connection.uploading()
            && answering.with(Option::is_none)
    };
    let follow_up = computed(move || controls.with(|c| c.follow_up.clone()));
    // Ctrl/⌘ held over the send button shows what its click does (T3 Code's alternate).
    let alternate_held = signal(false);
    let empty =
        move || composer.with(|c| c.draft.text.trim().is_empty() && c.draft.attachments.is_empty());
    let idle_input = move || {
        !recording.get()
            && !requesting.get()
            && !controls.with(|c| c.transcribing)
            && answering.with(Option::is_none)
            && !composed()
            && follow_up.with(|f| f.editing.is_none())
            && empty()
    };
    // Work an answered turn left running is stopped from its own strip.
    let stopping = move || {
        idle_input() && controls.with(|c| c.running_turn.is_some() && c.background.is_none())
    };
    // A stop or failure held the queue, or stopped the latest turn: the send
    // button resumes it, as T3 Code's Resume.
    let holding = move || follow_up.with(|f| f.held && !f.queue.is_empty());
    let resuming = move || {
        idle_input()
            && controls.with(|c| c.running_turn.is_none())
            && (holding() || follow_up.with(|f| f.resumable))
    };
    // What a send does while a turn runs: queue or steer, as T3 Code labels it.
    let action = move || {
        follow_up.with(|f| {
            (f.active && f.editing.is_none()).then(|| {
                if alternate_held.get() {
                    f.alternate_action
                } else {
                    f.default_action
                }
            })
        })
    };
    let stop_label = move || {
        controls.with(|c| {
            if c.interrupt_phase == Some(op::OperationPhase::Running) {
                tr("停止中")
            } else {
                tr("停止")
            }
        })
    };
    let primary_label = computed(move || {
        if let Some(question) = answering.get() {
            question.action
        } else if stopping() {
            stop_label().into()
        } else if resuming() {
            tr("スレッドを再開").into()
        } else if recording.get() {
            tr("文字起こしして送信").into()
        } else if follow_up.with(|f| f.editing.is_some()) {
            tr("キューのメッセージを更新").into()
        } else {
            match action() {
                Some(FollowUp::Queue) => tr("メッセージをキューに追加"),
                Some(FollowUp::Steer) => tr("追加の指示を送信"),
                None => tr("送信"),
            }
            .into()
        }
    });
    // T3 Code's tooltip names the click and the alternate while a turn runs.
    let primary_hint = move || -> SharedString {
        let verb = |action| match action {
            FollowUp::Queue => tr("キューに追加"),
            FollowUp::Steer => tr("追加で指示"),
        };
        follow_up
            .with(|f| {
                (f.active && f.editing.is_none() && !stopping() && !resuming()).then(|| {
                    tr_format(
                        "クリックで{0}、Ctrl/⌘-クリックまたは {1} で{2}",
                        [
                            &verb(f.default_action),
                            &format!("{} Enter", modifier()),
                            &verb(f.alternate_action),
                        ],
                    )
                })
            })
            .unwrap_or_else(|| primary_label.get().to_string())
            .into()
    };
    let enabled = move || {
        !requesting.get()
            && composer.with(|c| {
                if answering.with(Option::is_some) {
                    c.can_advance_question()
                } else if stopping() {
                    c.can_interrupt()
                } else if resuming() {
                    !connection.uploading() && c.can_send(true) && !c.controls.follow_up.busy
                } else {
                    // Text being composed is content Core is yet to hear of,
                    // as a recording is.
                    !connection.uploading()
                        && c.can_send(
                            recording.get()
                                || composed()
                                || !c.draft.text.trim().is_empty()
                                || !c.draft.attachments.is_empty(),
                        )
                }
            })
    };
    let collapsed = move || {
        !recording.get()
            && !requesting.get()
            && !focused.get()
            && !connection.uploading()
            && draft.with(String::is_empty)
            && composer.with(|c| {
                c.draft.attachments.is_empty()
                    && c.controls.navigation.thread_id.is_some()
                    && c.controls.question.is_none()
                    && !c.controls.transcribing
                    && !c.controls.transferring
            })
    };
    let interrupt = move || {
        if composer.with_untracked(|c| c.can_interrupt())
            && let Some((thread_id, turn_id)) = controls
                .with_untracked(|c| c.navigation.thread_id.clone().zip(c.running_turn.clone()))
        {
            connection.dispatch(Intent::Interrupt(op::Interrupt { thread_id, turn_id }));
        }
    };
    let queue = move |action| {
        if let Some(thread_id) = controls.with_untracked(|c| c.navigation.thread_id.clone()) {
            connection.dispatch(Intent::Queue { thread_id, action });
        }
    };
    // `alternate` does the opposite of the follow-up behavior for this message.
    let send = move |alternate: bool| {
        if !enabled() {
            return;
        }
        #[cfg(dictation)]
        if dictation.active.get_untracked() {
            following.set(true);
            dictation.finish(connection.client.get(), true);
            return;
        }
        if let Some(question) = answering.get_untracked() {
            following.set(true);
            connection.dispatch(Intent::AnswerQuestion {
                request_id: question.request_id,
                action: agent_core::questions::QuestionAction::Advance,
            });
        } else if stopping() {
            interrupt();
        } else if resuming() && holding() {
            queue(QueueAction::Resume);
        } else if resuming() {
            following.set(true);
            let thread_id = controls.with_untracked(|c| c.navigation.thread_id.clone());
            let draft_key = controls.with_untracked(|c| c.navigation.draft_key.clone());
            let text = agent_core::presentation::composer::RESUME_PROMPT.to_owned();
            connection.dispatch(Intent::EditComposer {
                thread_id: draft_key,
                cursor: text.len() as u32,
                text,
            });
            connection.dispatch(Intent::Submit {
                thread_id,
                client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                alternate: false,
            });
        } else if composition.with_untracked(Option::is_some) {
            // Japanese live conversion keeps a whole message marked until
            // return. Commit it, as a click outside a browser's field does,
            // and send once Core has the committed text.
            send_composed.set(true);
            field_ref.blur();
            field_ref.focus();
        } else {
            following.set(true);
            connection.dispatch(Intent::Submit {
                thread_id: controls.with_untracked(|c| c.navigation.thread_id.clone()),
                client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                alternate,
            });
        }
    };
    let submit = move || send(false);
    // T3 Code's composer shortcuts, bound only while their `when` holds so the
    // keys otherwise reach the text: Mod+Enter in the focused composer sends
    // (the alternate while a turn runs); Mod+Shift+Enter steers the first
    // queued message wherever the terminal lacks the keyboard, text fields
    // included; Alt+Up with the caret at the start of the focused composer
    // edits the latest queued message.
    let caret_at_start = signal(true);
    let terminal = use_context::<crate::terminal::Focus>().unwrap();
    let steerable = move || {
        follow_up.with(|f| {
            f.can_steer
                .then(|| f.queue.iter().find(|row| !row.pending))
                .flatten()
                .map(|row| row.id.clone())
        })
    };
    let editable = move || {
        follow_up.with(|f| {
            (f.editing.is_none() && !f.busy)
                .then(|| f.queue.iter().rev().find(|row| !row.pending))
                .flatten()
                .map(|row| row.id.clone())
        })
    };
    let steer_first = move |_: &str| {
        if let Some(id) = follow_up.with_untracked(|f| {
            f.queue
                .iter()
                .find(|row| !row.pending)
                .map(|row| row.id.clone())
        }) {
            queue(QueueAction::Steer {
                client_user_message_id: id,
            });
        }
    };
    let shortcuts = move || {
        use nsh_core::keymap::When;
        view()
            .absolute()
            .child(when(focused, move || {
                shortcut("mod+enter", When::Always, move |_| {
                    send(follow_up.with_untracked(|f| f.active))
                })
            }))
            .child(when(
                move || focused.get() && caret_at_start.get() && editable().is_some(),
                move || {
                    shortcut("alt+up", When::Always, move |_| {
                        if let Some(id) = editable() {
                            queue(QueueAction::Edit {
                                client_user_message_id: id,
                            });
                        }
                    })
                },
            ))
            .child(when(
                move || steerable().is_some() && !terminal.get(),
                move || shortcut("mod+shift+enter", When::Always, steer_first),
            ))
    };
    let prompt = move || -> String {
        answering.with(|question| {
            question
                .as_ref()
                .map(|question| question.placeholder.clone())
                .unwrap_or_else(|| {
                    if is_desktop() {
                        tr("AI に依頼する")
                    } else {
                        tr("メッセージを入力")
                    }
                    .into()
                })
        })
    };
    let field = move || {
        input()
            .element_ref(&field_ref)
            .auto_focus(connection.pane.is_some())
            .multiline(true)
            .value(move || {
                answering
                    .with(|q| q.as_ref().map(|q| q.text.clone()))
                    .unwrap_or_else(|| draft.get())
            })
            .placeholder(prompt)
            // A multiline field shows its placeholder as a separate label, so
            // screen readers name the field by the same prompt.
            .label(prompt)
            .masked(move || answering.with(|q| q.as_ref().is_some_and(|q| q.question.secret)))
            .readonly(move || {
                recording.get()
                    || requesting.get()
                    || controls.with(|c| c.transcribing)
                    || answering.with(|q| {
                        q.as_ref().is_some_and(|q| {
                            !q.can_respond
                                || !q.question.allow_free_text
                                || controls.with(|c| c.question_phase.clone())
                                    == Some(op::OperationPhase::Running)
                        })
                    })
            })
            .text_color(move || nsh_base::theme().foreground)
            .placeholder_color(move || color(nsh_base::theme().muted_foreground))
            // Enter sends as the send shortcut allows; Mod+Enter always sends.
            .submit_behavior(move || {
                if is_desktop()
                    && follow_up.with(|f| f.preferences.send_shortcut.enter_sends(&draft.get()))
                {
                    SubmitBehavior::Submit
                } else {
                    SubmitBehavior::Newline
                }
            })
            .on_selection_change(move |selection: &Selection| {
                caret_at_start.set(selection.start == 0 && selection.end == 0)
            })
            .on_submit(move |_: &str| submit())
            .on_change(move |text: &str| {
                if let Some(question) = answering.get_untracked() {
                    connection.dispatch(Intent::AnswerQuestion {
                        request_id: question.request_id,
                        action: agent_core::questions::QuestionAction::Edit { text: text.into() },
                    });
                } else {
                    connection.dispatch(Intent::EditComposer {
                        thread_id: controls.with_untracked(|c| c.navigation.draft_key.clone()),
                        text: text.into(),
                        cursor: text.len() as u32,
                    });
                }
            })
            // As T3 Code's and the desktop client's composers, pasted images (a
            // screenshot) and files are attached and the text copied with them
            // is left out. The field inserts a paste of text alone itself, so
            // it stays undoable.
            .paste_behavior(nsh_core::PasteBehavior::ReportFiles)
            .on_paste(move |paste: &nsh_core::PasteEvent| {
                let sources: Vec<_> = paste
                    .files
                    .iter()
                    .filter(|file| file.mime.starts_with("image/"))
                    .map(|file| crate::client::AttachmentSource::Image {
                        name: file.name.clone(),
                        bytes: file.data.clone(),
                    })
                    .chain(paste.uris.iter().filter_map(|uri| {
                        let path = url::Url::parse(uri).ok()?.to_file_path().ok()?;
                        let is_image = native::image_file(&path);
                        Some(crate::client::AttachmentSource::File { path, is_image })
                    }))
                    .collect();
                if !sources.is_empty() && can_attach() {
                    let host = connection.host.get_untracked();
                    let key = controls.with_untracked(|c| c.navigation.draft_key.clone());
                    connection
                        .application
                        .with(|app| app.attach(host, connection.pane, key, sources));
                }
            })
            .on_focus(move || focused.set(true))
            .on_blur(move || focused.set(false))
            .on_composition_change(move |event: &Composition| {
                composition.set(event.composing.then(|| event.text.clone()));
                // The committed text's change came first: Core holds it now.
                if !event.composing && send_composed.get_untracked() {
                    send_composed.set(false);
                    submit();
                }
            })
            .auto_correct(false)
            .auto_capitalize(AutoCapitalize::None)
            // Let the multiline input measure wrapped text; only constrain
            // its range so long drafts scroll after growing with their text.
            .min_h(move || {
                px(if is_desktop() {
                    40.0
                } else if collapsed() {
                    36.0
                } else {
                    64.0
                })
            })
            .max_h(px(240.0))
    };
    let dictation_button = move || {
        #[cfg(dictation)]
        {
            dictation.button()
        }
        #[cfg(not(dictation))]
        {
            view()
        }
    };
    let actions = move || {
        view()
            .grid()
            .grid_template_columns(move || {
                if collapsed() {
                    "44px minmax(0, 1fr) auto auto"
                } else {
                    "44px minmax(0, 1fr) auto auto auto"
                }
            })
            .grid_template_rows(move || if collapsed() { "auto" } else { "auto auto" })
            .items_center()
            .gap_1()
            .child(
                field()
                    .min_w_0()
                    .w_full()
                    .col_start(move || if collapsed() { 2_i16 } else { 1_i16 })
                    .col_end(move || if collapsed() { 3_i16 } else { 6_i16 })
                    .row_start(1),
            )
            .child(
                native::menu(
                    "plus",
                    tr("添付"),
                    native::choices(&[tr("写真を選択"), tr("ファイルを選択")]),
                    move |item| {
                        let host = connection.host.get_untracked();
                        let key = controls.with_untracked(|c| c.navigation.draft_key.clone());
                        if item == tr("写真を選択") {
                            let picked = pick_images(nsh_core::picker::PickRequest::new(true, 10));
                            spawn(async move {
                                let images = picked.await;
                                connection.application.with(|app| {
                                    app.attach(
                                        host,
                                        connection.pane,
                                        key,
                                        images
                                            .into_iter()
                                            .map(|file| crate::client::AttachmentSource::Image {
                                                name: file.name,
                                                bytes: file.data.to_vec(),
                                            })
                                            .collect(),
                                    )
                                });
                            });
                        } else if !is_desktop() {
                            file_target.replace(Some((host, key)));
                            file_pick.update(|value| *value += 1);
                        } else {
                            pick_files(connection.application.get(), host, connection.pane, key);
                        }
                    },
                )
                .prop(
                    "appearance",
                    json!(if is_desktop() { "plain" } else { "automatic" }),
                )
                .prop("enabled", move || json!(can_attach()))
                .min_h(px(44.0))
                .min_w(px(44.0))
                .col_start(1)
                .row_start(move || if collapsed() { 1_i16 } else { 2_i16 }),
            )
            .child(when(
                move || !collapsed(),
                || {
                    crate::model_controls::controls(signal(0.0), || 0.0)
                        .col_start(3)
                        .row_start(2)
                },
            ))
            .child(
                view()
                    .child(when(
                        move || answering.with(Option::is_none),
                        dictation_button,
                    ))
                    .child(when(
                        move || {
                            answering.with(Option::is_some)
                                && controls.with(|c| c.running_turn.is_some())
                        },
                        move || {
                            native::button("stop.fill", tr("停止"), interrupt)
                                .min_w(px(44.0))
                                .min_h(px(44.0))
                                .prop("enabled", move || {
                                    json!(composer.with(|c| c.can_interrupt()))
                                })
                                .prop("label", move || json!(stop_label()))
                        },
                    ))
                    .col_start(move || if collapsed() { 3_i16 } else { 4_i16 })
                    .row_start(move || if collapsed() { 1_i16 } else { 2_i16 }),
            )
            .child(
                native::button("arrow.up", tr("送信"), submit)
                    .min_w(px(44.0))
                    .min_h(px(44.0))
                    .col_start(move || if collapsed() { 4_i16 } else { 5_i16 })
                    .row_start(move || if collapsed() { 1_i16 } else { 2_i16 })
                    .prop("symbol", move || {
                        json!(if answering.with(Option::is_some) {
                            None
                        } else {
                            Some(if stopping() { "stop.fill" } else { "arrow.up" })
                        })
                    })
                    .prop("title", move || {
                        json!(
                            answering
                                .with(|q| q.as_ref().map(|q| q.action.clone()).unwrap_or_default())
                        )
                    })
                    .prop("label", move || json!(primary_label.get()))
                    .prop("appearance", move || {
                        json!(if is_desktop() && answering.with(Option::is_none) {
                            "circular"
                        } else {
                            "prominent"
                        })
                    })
                    .prop("enabled", move || json!(enabled())),
            )
    };
    // The existing desktop client's composer: the field over one row of controls.
    let desktop_box = move || {
        use crate::ui::{self, Enabled, Icon, colors};
        let inactive = move || {
            !enabled()
                || (composer.with(|c| c.draft.text.trim().is_empty()) && !stopping() && !resuming())
        };
        // T3 Code's footer: the controls between attaching and sending fit the
        // room the row leaves them, as `footer::controls_layout` decides.
        let row_width = signal(None::<f32>);
        let attach_width = signal(0_f32);
        let primary_width = signal(0_f32);
        let model_width = signal(0_f32);
        // The approval control is the one block after the model control, and
        // as wide as the "more" control as its icon.
        #[cfg(desktop)]
        let approval_width = signal(0_f32);
        #[cfg(desktop)]
        let blocks = move || {
            use crate::permissions::ICON_WIDTH;
            (vec![approval_width.get()], vec![ICON_WIDTH], ICON_WIDTH)
        };
        #[cfg(not(desktop))]
        let blocks = || (Vec::new(), Vec::new(), 0.0);
        let room =
            move || row_width.get().unwrap_or_default() - attach_width.get() - primary_width.get();
        let measurement = move || {
            let (blocks, icon_blocks, overflow) = blocks();
            footer::ControlsMeasurement {
                gap: 0.0,
                model: model_width.get(),
                blocks,
                icon_blocks,
                overflow,
            }
        };
        let layout = signal(footer::ControlsLayout::default());
        effect(move || {
            if row_width.get().is_some() {
                let previous = layout.get_untracked();
                let next = footer::controls_layout(&measurement(), room(), Some(previous));
                if next != previous {
                    layout.set(next);
                }
            }
        });
        let shown = move || layout.with(|layout| layout.visible);
        let controls_row = view()
            .flex_row()
            .items_center()
            .on_layout(move |bounds: &Bounds<Points>| row_width.set(Some(bounds.size.width.0)))
            .child(
                view()
                    .on_layout(move |bounds: &Bounds<Points>| attach_width.set(bounds.size.width.0))
                    .child(when(
                        move || answering.with(Option::is_none),
                        move || {
                            ui::icon_button_sized(
                                Icon::PLUS,
                                tr("ファイルを添付"),
                                40.0,
                                24.0,
                                move || {
                                    let host = connection.host.get_untracked();
                                    let key =
                                        controls.with_untracked(|c| c.navigation.draft_key.clone());
                                    pick_files(
                                        connection.application.get(),
                                        host,
                                        connection.pane,
                                        key,
                                    );
                                },
                            )
                            .enabled(can_attach)
                        },
                    )),
            );
        #[cfg(desktop)]
        let controls_row = controls_row.child(when(shown, move || {
            use crate::permissions::Look;
            crate::permissions::control(approval_width, move || {
                layout.with(|layout| {
                    if layout.hidden_at(0, 1) {
                        Look::More
                    } else if layout.icon_only_at(0, 1) {
                        Look::Icon
                    } else {
                        Look::Labelled
                    }
                })
            })
        }));
        view()
            .element_ref(&connection.composer_box)
            .w_full()
            .p(px(7.0))
            .gap_2()
            .rounded(px(22.0))
            .bg(colors::SURFACE)
            .border_1()
            .border_color(colors::border())
            .child(view().px_2().pt(px(10.0)).pb_1().child(field().w_full()))
            .child(
                controls_row
                    .child(view().flex_1())
                    .child(when(shown, move || {
                        crate::model_controls::controls(model_width, move || {
                            footer::model_width(&measurement(), room(), layout.get())
                        })
                    }))
                    .child(
                        view()
                            .flex_row()
                            .items_center()
                            .on_layout(move |bounds: &Bounds<Points>| {
                                primary_width.set(bounds.size.width.0)
                            })
                            .child(when(
                                move || answering.with(Option::is_none),
                                dictation_button,
                            ))
                            .child(when(
                                move || answering.with(Option::is_some),
                                move || {
                                    crate::requests::question_actions(answering, enabled, submit)
                                },
                            ))
                            .child(when(
                                move || answering.with(Option::is_none),
                                move || {
                                    view()
                                        .w(px(40.0))
                                        .h(px(40.0))
                                        .rounded_full()
                                        .items_center()
                                        .justify_center()
                                        .role(AccessibilityRole::Button)
                                        .label(move || primary_label.get())
                                        .bg(move || {
                                            if inactive() {
                                                colors::border()
                                            } else {
                                                rgb(0xffffff)
                                            }
                                        })
                                        .text_color(move || {
                                            if inactive() {
                                                colors::muted()
                                            } else {
                                                colors::SIDEBAR
                                            }
                                        })
                                        .style_when(inactive, |s| s.opacity(0.5))
                                        // VoiceOver says so when it cannot send or stop.
                                        .disabled(move || !enabled())
                                        .tooltip(ui::tooltip(primary_hint))
                                        .on_mouse_move(move |event: &MouseEvent| {
                                            alternate_held
                                                .set(event.modifiers.cmd || event.modifiers.ctrl)
                                        })
                                        .on_mouse_leave(move |_: &MouseEvent| {
                                            alternate_held.set(false)
                                        })
                                        .on_press(move |event: &PressEvent| {
                                            let alternate =
                                                event.modifiers.cmd || event.modifiers.ctrl;
                                            send(
                                                alternate && follow_up.with_untracked(|f| f.active),
                                            )
                                        })
                                        .child(dynamic(move || {
                                            let glyph = if stopping() {
                                                Icon::STOP
                                            } else if resuming() {
                                                Icon::RESUME
                                            } else if follow_up.with(|f| f.editing.is_some()) {
                                                Icon::CHECK
                                            } else {
                                                match action() {
                                                    Some(FollowUp::Queue) => Icon::QUEUE,
                                                    Some(FollowUp::Steer) => Icon::STEER_NOW,
                                                    None => Icon::ARROW_UP,
                                                }
                                            };
                                            ui::icon(
                                                glyph,
                                                if glyph == Icon::ARROW_UP || glyph == Icon::STOP {
                                                    24.0
                                                } else {
                                                    18.0
                                                },
                                            )
                                        }))
                                },
                            )),
                    ),
            )
    };
    use agent_core::presentation::conversation::{LANE_WIDTH, lane_padding};
    // The timeline's lane: the same width and padding beside it.
    let padding = move || lane_padding(window().width.0);
    let column = view()
        .w_full()
        .max_w(move || px(LANE_WIDTH + 2.0 * padding()))
        .self_center()
        .px(move || px(padding()))
        .pt(move || if is_desktop() { px(8.0) } else { px(12.0) })
        .pb(move || if is_desktop() { px(16.0) } else { px(12.0) })
        .gap(move || if is_desktop() { px(4.0) } else { px(8.0) });
    // The shared core predicate hides this for side chats, new threads, and
    // empty reviews. The Diff route and native glass button are available on
    // both NSH targets, so the adapter is not desktop-only.
    let column = column.child(review_card());
    // As the desktop client's composer, Escape discards a recording (or its
    // permission request) before it is transcribed.
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let column = column.child(when(
        move || recording.get() || requesting.get(),
        move || {
            on_key_with(
                "escape",
                KeyOptions {
                    when: nsh_core::keymap::When::Always,
                    repeat: false,
                },
                move |_| dictation.cancel(),
            );
            view()
        },
    ));
    column
        .child(crate::background_work::strip())
        .child(when(
            move || {
                composition.with(Option::is_none)
                    && !recording.get()
                    && !requesting.get()
                    && !controls.with(|c| c.transcribing)
                    && answering.with(Option::is_none)
            },
            move || crate::invocations::suggestions(move || field_ref.focus()),
        ))
        .child(when(
            move || connection.uploading() || controls.with(|c| c.transferring),
            || text(tr("添付ファイルを転送中…")),
        ))
        // Why Core holds the send back, as the other clients say it: a
        // disabled send button alone leaves the typed message unexplained.
        .child(when(
            move || controls.with(|c| c.unavailable.is_some()),
            move || {
                text(move || {
                    controls
                        .with(|c| c.unavailable.clone())
                        .map(localized)
                        .unwrap_or_default()
                })
                .selectable(true)
                .text_size(px(12.0))
                .text_color(move || nsh_base::theme().muted_foreground)
            },
        ))
        .child(when(
            move || !is_desktop(),
            move || {
                native("bex.filePicker")
                    .h(px(0.0))
                    .prop("open", move || json!(file_pick.get()))
                    .on_native_event(move |event: &NativeEvent| {
                        if &*event.name == "picked"
                            && let Some((host, key)) = file_target.replace(None)
                        {
                            let sources = event
                                .data
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(serde_json::Value::as_str)
                                .map(|path| crate::client::AttachmentSource::File {
                                    path: path.into(),
                                    is_image: native::image_file(std::path::Path::new(path)),
                                })
                                .collect();
                            connection
                                .application
                                .with(|app| app.attach(host, connection.pane, key, sources));
                        }
                    })
            },
        ))
        .child(when(
            move || {
                answering.with(Option::is_none)
                    && composer.with(|c| !c.draft.attachments.is_empty())
            },
            move || {
                scroll()
                    .horizontal()
                    .h(move || {
                        px(
                            if composer
                                .with(|c| c.draft.attachments.iter().any(|file| file.is_image))
                            {
                                136.0
                            } else {
                                60.0
                            },
                        )
                    })
                    .child(view().flex_row().items_start().gap_3().p_2().child(each(
                        move || composer.with(|c| c.draft.attachments.clone()),
                        |file: &agent_core::state::Attachment| file.path.clone(),
                        move |file, index| {
                            attachment(connection, preview, files, file, move || {
                                connection.dispatch(Intent::RemoveAttachment {
                                    draft_key: controls
                                        .with_untracked(|c| c.navigation.draft_key.clone()),
                                    index: index.get_untracked() as u32,
                                })
                            })
                        },
                    )))
            },
        ))
        .child(when(is_desktop, shortcuts))
        .child(when(
            move || follow_up.with(|f| !f.queue.is_empty()),
            move || crate::queue::queued(connection),
        ))
        // A waiting request's banner sits on the composer's top edge, its foot
        // under the composer drawn after it.
        .child(
            view()
                .child(crate::requests::panels(following))
                .child(when(is_desktop, desktop_box))
                .child(when(
                    move || !is_desktop(),
                    move || {
                        view()
                            .p_2()
                            .gap_1()
                            .rounded(px(26.0))
                            .glass_effect(GlassEffect::Regular)
                            .child(actions())
                    },
                )),
        )
        .child(when(
            move || controls.with(|c| !c.connected),
            move || {
                text(tr("Host 未接続"))
                    .text_color(move || nsh_base::theme().muted_foreground)
                    .text_size(px(11.0))
            },
        ))
}

/// The workspace-change badge shared by NSH composer adapters, matching the
/// current iOS composer: counts only, with the full diff opened separately.
fn review_card() -> impl IntoElement {
    use crate::ui::colors;
    let connection = use_context::<Connection>().unwrap();
    let workspace = use_context::<Workbench>().unwrap();
    let is_side_chat = connection.pane.is_some();
    let review = computed(move || {
        connection
            .publication
            .with(|p| p.pane().composer_review(is_side_chat))
    });
    let open = move || {
        let cwd = connection
            .composer
            .with_untracked(|p| p.controls.navigation.cwd.clone());
        workspace.open(PanelKind::Diff, Some(cwd), None);
    };
    when(
        move || review.with(Option::is_some),
        move || {
            nsh_base::button()
                .accessibility_label(Some(SharedString::from(tr("変更を表示"))))
                .self_center()
                .flex_row()
                .items_center()
                .justify_center()
                .gap_3()
                .h(px(32.0))
                .px_3()
                .rounded_full()
                .glass_effect(GlassEffect::Regular)
                .text_size(px(13.0))
                .hover(|s| s.bg(colors::GHOST_HOVER))
                .active(|s| s.bg(colors::SELECTED))
                .on_press(open)
                .child(
                    text(move || {
                        let files = review.with(|r| r.as_ref().map_or(0, |r| r.files));
                        tr_format("{0}件のファイル", [&files])
                    })
                    .text_color(colors::ACCENT),
                )
                .child(change_counts(
                    move || review.with(|r| r.as_ref().map_or(0, |r| r.additions)),
                    move || review.with(|r| r.as_ref().map_or(0, |r| r.deletions)),
                ))
        },
    )
}

/// Added and removed lines, in the same compact form as the iOS badge.
fn change_counts(
    additions: impl Fn() -> u64 + Copy + 'static,
    deletions: impl Fn() -> u64 + Copy + 'static,
) -> View {
    view()
        .flex_row()
        .gap_1()
        .child(
            text(move || format!("+{}", additions()))
                .text_size(px(12.0))
                .text_color(nsh_core::element::rgb(0x37cf77)),
        )
        .child(
            text(move || format!("−{}", deletions()))
                .text_size(px(12.0))
                .text_color(nsh_core::element::rgb(0xff6259)),
        )
}

/// An attached file as T3 Code's composer chips: an image's thumbnail or a
/// file's name, which opens it, with its removal at the corner.
fn attachment(
    connection: Connection,
    preview: crate::media::Preview,
    files: crate::files::Browser,
    file: ReadSignal<agent_core::state::Attachment>,
    remove: impl Fn() + 'static,
) -> impl IntoElement {
    use crate::ui::{self, Icon, colors};
    let open = move || {
        let (cwd, home) = connection
            .publication
            .with_untracked(|p| (p.composer.controls.navigation.cwd.clone(), p.context.home()));
        files.open(
            file.with_untracked(|file| file.path.clone()),
            cwd,
            home,
            connection.host.get_untracked(),
            connection.client.get(),
        );
    };
    view()
        .rounded(px(ui::RADIUS))
        .border_1()
        .border_color(colors::border())
        .bg(colors::RAISED)
        .overflow_hidden()
        .child(when(
            move || file.with(|file| file.is_image),
            move || {
                crate::media::image_view(
                    connection,
                    preview,
                    move || file.with(|file| file.path.clone()),
                    crate::media::ImageSize::Box(120.0, 120.0),
                    None,
                )
            },
        ))
        .child(when(
            move || file.with(|file| !file.is_image),
            move || {
                view()
                    .h(px(40.0))
                    .pl_1()
                    .pr(px(32.0))
                    .justify_center()
                    .child(ui::labeled_button(
                        Icon::FILE_TEXT,
                        move || file.with(|file| file.name.clone()),
                        move || file.with(|file| file.name.clone()),
                        open,
                    ))
            },
        ))
        .child(
            view()
                .absolute()
                .top(px(4.0))
                .right(px(4.0))
                .rounded_full()
                .bg(colors::SURFACE)
                .child(ui::small_icon_button(Icon::CLOSE, tr("添付を削除"), remove)),
        )
}

/// Both the composer and desktop workspace use the OS picker and Core's upload owner.
pub(crate) fn pick_files(
    application: std::sync::Arc<crate::application::Application>,
    host: String,
    pane: Option<u64>,
    key: agent_core::state::DraftKey,
) {
    let picked = pick_paths(nsh_core::file_dialog::OpenDialog::files().multiple(true));
    spawn(async move {
        match picked.await {
            Ok(Some(paths)) => application.attach(
                host,
                pane,
                key,
                paths
                    .into_iter()
                    .map(|path| {
                        let is_image = native::image_file(&path);
                        crate::client::AttachmentSource::File { path, is_image }
                    })
                    .collect(),
            ),
            Ok(None) => {}
            Err(reason) => native::message(tr("添付"), reason),
        }
    });
}

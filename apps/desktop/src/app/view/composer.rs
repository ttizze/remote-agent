use super::*;
use gpui_kit::component::button::ButtonCustomVariant;

impl Desktop {
    pub(in crate::app) fn sync_composer(&self, cx: &mut App) {
        let model = self.composer_model(cx);
        self.composer_view
            .read(cx)
            .content
            .clone()
            .update(cx, |view, cx| {
                view.pane = self.session.as_ref().map(|session| session.pane.clone());
                view.set_model(model, cx);
            });
    }
}

impl ComposerContent {
    fn composer_context(model: &ComposerModel, cx: &Context<Self>) -> AnyElement {
        let language = crate::localization::language(cx);
        let mut row = h_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .px_2()
            .child(Self::composer_folder(model, cx));
        if let Some(hosts) = &model.hosts {
            row = row.child(Hosts::menu(
                hosts,
                "composer-host",
                model.remote.as_ref().map(|remote| remote.id.as_str()),
                model
                    .remote
                    .as_ref()
                    .map_or("Local", |remote| remote.name.as_str()),
                model.busy || model.phase.is_some(),
                cx,
            ));
        } else {
            let name = model
                .remote
                .as_ref()
                .map_or("Local", |remote| remote.name.as_str())
                .to_owned();
            row = row.child(
                h_flex()
                    .id("composer-host")
                    .min_w_0()
                    .max_w(px(200.))
                    .h_8()
                    .px_2()
                    .gap_2()
                    .tooltip({
                        let name = name.clone();
                        move |window, cx| {
                            tooltip::Tooltip::new(agent_core::localization::localized_ui_format(
                                language,
                                "実行先: {0}".into(),
                                vec![name.to_string()],
                            ))
                            .build(window, cx)
                        }
                    })
                    .child(Icon::default().path("icons/monitor.svg").size_4())
                    .child(div().min_w_0().text_ellipsis().child(name)),
            );
        }
        if let Some(review) = model
            .review
            .as_ref()
            .filter(|review| !review.branch.is_empty())
        {
            let entity = cx.entity().downgrade();
            row = row.child(
                Button::new("composer-branch")
                    .debug_selector(|| "composer-branch".into())
                    .label(review.branch.clone())
                    .icon(Icon::default().path("icons/branch.svg"))
                    .accessibility_label(agent_core::localization::localized_ui_format(
                        language,
                        "現在のブランチ: {0}".into(),
                        vec![review.branch.to_string()],
                    ))
                    .tooltip(agent_core::localization::localized_ui_format(
                        language,
                        "現在のブランチ: {0}".into(),
                        vec![review.branch.to_string()],
                    ))
                    .dropdown_caret(true)
                    .h_8()
                    .min_w_0()
                    .max_w(px(260.))
                    .flex_shrink_1()
                    .ghost()
                    .disabled(!model.connected || model.phase.is_some())
                    .dropdown_menu(move |menu, _, _| {
                        let refresh = entity.clone();
                        let changes = entity.clone();
                        menu.item(PopupMenuItem::new(language.text("変更を表示")).on_click(
                            move |_, _, cx| {
                                let _ = changes.update(cx, |_, cx| {
                                    cx.emit(ComposerAction::Review(None));
                                    cx.notify();
                                });
                            },
                        ))
                        .item(
                            PopupMenuItem::new(language.text("更新")).on_click(move |_, _, cx| {
                                let _ = refresh
                                    .update(cx, |_, cx| cx.emit(ComposerAction::RefreshReview));
                            }),
                        )
                    }),
            );
        }
        row.into_any_element()
    }

    fn composer_folder(model: &ComposerModel, cx: &Context<Self>) -> AnyElement {
        let language = crate::localization::language(cx);
        let selected_directory = model.selected_directory.clone();
        let entity = cx.entity().downgrade();
        Button::new("composer-folder")
            .debug_selector(|| "composer-folder".into())
            .label(if selected_directory.is_empty() {
                language.text("チャット").into()
            } else {
                file_name(&selected_directory)
            })
            .accessibility_label(if selected_directory.is_empty() {
                language.text("フォルダ: チャット").into()
            } else {
                agent_core::localization::localized_ui_format(
                    language,
                    "フォルダ: {0}".into(),
                    vec![selected_directory.to_string()],
                )
            })
            .icon(IconName::Folder)
            .tooltip(if selected_directory.is_empty() {
                language.text("チャット").into()
            } else {
                selected_directory.clone()
            })
            .h_8()
            .min_w_0()
            .max_w(px(260.))
            .flex_shrink_1()
            .ghost()
            .disabled(model.busy || model.phase.is_some())
            .dropdown_menu(move |mut menu, _, cx| {
                let Some(owner) = entity.upgrade() else {
                    return menu;
                };
                let state = owner.read(cx);
                let selected_directory = state
                    .model
                    .as_ref()
                    .map(|model| model.selected_directory.as_str())
                    .unwrap_or_default();
                let unassigned = entity.clone();
                menu = menu.item(
                    PopupMenuItem::new(language.text("チャット"))
                        .checked(selected_directory.is_empty())
                        .on_click(move |_, _, cx| {
                            let _ = unassigned.update(cx, |_, cx| {
                                cx.emit(ComposerAction::NewChat(String::new()))
                            });
                        }),
                );
                for (path, label) in &state.folders {
                    let path = path.clone();
                    let label = label.clone();
                    let target = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(path == selected_directory)
                            .on_click(move |_, _, cx| {
                                let _ = target.update(cx, |_, cx| {
                                    cx.emit(ComposerAction::NewChat(path.clone()))
                                });
                            }),
                    );
                }
                if state.more_projects {
                    let target = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(language.text("さらにプロジェクトを読み込む")).on_click(
                            move |_, _, cx| {
                                let _ = target
                                    .update(cx, |view, _| view.dispatch(Intent::ExpandProjects));
                            },
                        ),
                    );
                }
                if state
                    .model
                    .as_ref()
                    .is_some_and(|model| model.remote.is_none())
                {
                    let target = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(language.text("別のフォルダを選択…")).on_click(
                            move |_, _, cx| {
                                let _ =
                                    target.update(cx, |_, cx| cx.emit(ComposerAction::PickFolder));
                            },
                        ),
                    );
                }
                menu
            })
            .into_any_element()
    }
}

impl Desktop {
    pub(super) fn chat(&self) -> AnyElement {
        v_flex()
            .debug_selector(|| "conversation-chat".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                self.conversation
                    .clone()
                    .cached(StyleRefinement::default().flex_1().min_h_0().w_full()),
            )
            .child(self.composer_view.clone())
            .into_any_element()
    }
}

impl ComposerContent {
    fn render_composer(&self, model: &ComposerModel, cx: &mut Context<Self>) -> AnyElement {
        let pane = &self.pane;
        let input = &self.input;
        let question_input = &self.question_input;
        let question_secret = &self.question_secret;
        let render_owner = cx.entity_id();
        let language = crate::localization::language(cx);
        let key = model.key.clone();
        let attachments = &model.attachments;
        let mut files = h_flex().gap_3().p_2().flex_wrap();
        for (i, file) in attachments.iter().enumerate() {
            let key = key.clone();
            let remove = icon_button(
                format!("attachment-{i}"),
                IconName::Close,
                agent_core::localization::localized_ui_format(
                    language,
                    "{0}を外す".into(),
                    vec![file.name.to_string()],
                ),
                cx,
                move |view, _, cx| {
                    cx.stop_propagation();
                    view.dispatch(Intent::RemoveAttachment {
                        draft_key: key.clone(),
                        index: i as u32,
                    });
                },
            )
            .debug_selector(move || format!("attachment-remove-{i}"))
            .xsmall()
            .rounded_full();
            files =
                files.child(if file.is_image {
                    div()
                        .id(format!("draft-attachment-{i}"))
                        .relative()
                        .size(px(120.))
                        .flex_shrink_0()
                        .child(div().size_full().rounded_lg().overflow_hidden().child(
                            Media::preview(
                                &self.media,
                                format!("draft-image-{i}"),
                                &file.path,
                                false,
                                120.,
                                true,
                                cx,
                            ),
                        ))
                        .child(deferred(
                            div()
                                .id(format!("attachment-remove-hitbox-{i}"))
                                .occlude()
                                .absolute()
                                .top(px(-4.))
                                .right(px(-4.))
                                .size(px(18.))
                                .child(
                                    remove
                                        .size(px(18.))
                                        .custom(
                                            ButtonCustomVariant::new(cx)
                                                .foreground(rgb(0x222222).into())
                                                .hover(rgb(0xe6e6e6).into())
                                                .active(rgb(0xd4d4d4).into()),
                                        )
                                        .bg(rgb(0xffffff)),
                                ),
                        ))
                        .into_any_element()
                } else {
                    h_flex()
                        .gap_2()
                        .child(file.name.clone())
                        .child(remove.size(px(28.)).bg(rgb(appearance::RAISED)))
                        .into_any_element()
                });
        }
        let running = model.running.clone();
        let empty = model.empty;
        let phase = model.phase;
        // Dictation in progress finishes into the draft before the composer answers.
        let answering = model.question.as_ref().filter(|_| phase.is_none());
        let stop_session = model.session.clone();
        let stop = running.clone().map(|id| {
            icon_button(
                "stop",
                Icon::default().path("icons/stop.svg"),
                language.text("停止"),
                cx,
                move |view, _, _| {
                    if let Some(thread_id) = &stop_session {
                        view.dispatch(Intent::Interrupt(op::Interrupt {
                            thread_id: thread_id.clone(),
                            turn_id: id.clone(),
                        }));
                    }
                },
            )
            .disabled(!model.connected || model.busy)
        });
        let trailing: Vec<AnyElement> = if let Some(question) = answering {
            stop.map(|stop| round_action(stop, false, cx))
                .into_iter()
                .chain(Self::question_actions(question, cx))
                .map(IntoElement::into_any_element)
                .collect()
        } else {
            let send = stop.filter(|_| empty).unwrap_or_else(|| {
                icon_button(
                    "send",
                    IconName::ArrowUp,
                    language.text("送信"),
                    cx,
                    |_, _, cx| cx.emit(ComposerAction::Send),
                )
                .disabled(
                    !model.connected
                        || !model.provider_available
                        || model.busy
                        || empty
                        || model.input_unavailable.is_some(),
                )
            });
            let microphone = Button::new("dictation-toggle")
                .icon(Icon::default().path("icons/microphone.svg").size(px(20.)))
                .ghost()
                .w(px(40.))
                .h(px(40.))
                .large()
                .tooltip(language.text("音声をCodexで文字起こし"))
                .accessibility_label(language.text("音声をCodexで文字起こし"))
                .disabled(!model.connected || model.busy)
                .on_click(cx.listener(|s, _, window, cx| {
                    s.input.read(cx).focus_handle(cx).focus(window, cx);
                    cx.emit(ComposerAction::StartDictation);
                }));
            vec![
                microphone.into_any_element(),
                round_action(send, empty && running.is_none(), cx).into_any_element(),
            ]
        };
        // The answer field replaces the draft's while a question awaits it.
        let (field, focus) = match answering {
            Some(question) => {
                let prompt = &question.prompt;
                let secret = prompt.question.secret;
                let disabled = !prompt.question.allow_free_text || !prompt.can_respond;
                let field = if secret {
                    Input::new(question_secret)
                        .appearance(false)
                        .bordered(false)
                        .text_size(px(14.))
                        .aria_label(prompt.question.prompt.clone())
                        .disabled(disabled)
                        .into_any_element()
                } else {
                    Textarea::new(question_input)
                        .appearance(false)
                        .bordered(false)
                        .text_size(px(14.))
                        .aria_label(prompt.question.prompt.clone())
                        .disabled(disabled)
                        .into_any_element()
                };
                (
                    field,
                    if secret {
                        question_secret.read(cx).focus_handle(cx)
                    } else {
                        question_input.read(cx).focus_handle(cx)
                    },
                )
            }
            None => (
                Textarea::new(input)
                    .appearance(false)
                    .bordered(false)
                    .text_size(px(14.))
                    .aria_label(language.text("AI に依頼する"))
                    .readonly(!model.connected)
                    .into_any_element(),
                input.read(cx).focus_handle(cx),
            ),
        };
        let composer = v_flex()
            .when(answering.is_none() && model.connected, |composer| {
                composer.on_drop(
                    cx.listener(|_, drag: &super::file_tree::FileTreeDrag, _, cx| {
                        cx.emit(ComposerAction::AddMentions(drag.mentions.clone()));
                        cx.stop_propagation();
                    }),
                )
            })
            .track_focus(&focus)
            .map(|composer| {
                if answering.is_some() {
                    return composer.capture_action(cx.listener(
                        |view, action: &gpui_kit::component::input::Enter, window, cx| {
                            let owner = view.desktop.clone();
                            let _ = owner
                                .update(cx, |desktop, cx| desktop.answer_enter(action, window, cx));
                        },
                    ));
                }
                composer
                    .key_context("ChatComposer")
                    .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                        let owner = view.desktop.clone();
                        let _ = owner.update(cx, |desktop, cx| {
                            let modifiers = event.keystroke.modifiers;
                            if !(modifiers.shift
                                || modifiers.control
                                || modifiers.alt
                                || modifiers.platform)
                                && desktop.completion_key(&event.keystroke.key, window, cx)
                            {
                                return;
                            }
                            if event.keystroke.key == "escape"
                                && desktop
                                    .dictation
                                    .as_ref()
                                    .is_some_and(|state| state.phase != Phase::Transcribing)
                            {
                                desktop.cancel_recording();
                                cx.stop_propagation();
                                cx.notify();
                            }
                        });
                    }))
                    .capture_action(cx.listener(
                        |view, action: &gpui_kit::component::input::Paste, window, cx| {
                            let owner = view.desktop.clone();
                            let _ = owner
                                .update(cx, |desktop, cx| desktop.paste_image(action, window, cx));
                        },
                    ))
                    .capture_action(cx.listener(
                        |view, action: &gpui_kit::component::input::Enter, window, cx| {
                            let owner = view.desktop.clone();
                            let _ = owner.update(cx, |desktop, cx| {
                                desktop.composer_enter(action, window, cx)
                            });
                        },
                    ))
                    .capture_action(cx.listener(
                        |view, _: &gpui_kit::component::input::MoveUp, window, cx| {
                            let owner = view.desktop.clone();
                            let _ = owner
                                .update(cx, |desktop, cx| desktop.composer_arrow("up", window, cx));
                        },
                    ))
                    .capture_action(cx.listener(
                        |view, _: &gpui_kit::component::input::MoveDown, window, cx| {
                            let owner = view.desktop.clone();
                            let _ = owner.update(cx, |desktop, cx| {
                                desktop.composer_arrow("down", window, cx)
                            });
                        },
                    ))
                    .child(Self::completion_menu(
                        model.suggestions.as_ref(),
                        model.completion_index,
                        cx,
                    ))
                    // A question's answer has no attachments; the draft keeps them.
                    .when(!attachments.is_empty(), |composer| composer.child(files))
            })
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .p(px(7.))
            .gap_2()
            .rounded(px(22.))
            .bg(rgb(appearance::SURFACE))
            .border_1()
            .border_color(rgb(appearance::BORDER))
            .when(phase.is_none(), |composer| {
                composer.child(div().px_2().pt(px(10.)).pb_1().child(field))
            })
            .when(phase.is_none(), |composer| {
                composer.child(
                    h_flex()
                        .when(answering.is_none(), |row| {
                            row.child(
                                icon_button(
                                    "attach",
                                    IconName::Plus,
                                    language.text("ファイルを添付"),
                                    cx,
                                    |_, _, cx| cx.emit(ComposerAction::Attach),
                                )
                                .debug_selector(|| "attach".into())
                                .w(px(40.))
                                .h(px(40.))
                                .large()
                                .disabled(
                                    !model.connected
                                        || (!model.remote.is_none() && model.cwd.is_empty())
                                        || model.busy,
                                ),
                            )
                        })
                        .child(Preferences::permission_menu(
                            model.permission_label,
                            model.permission_mode,
                            model.connected && model.provider_available,
                            render_owner,
                            &self.preferences.downgrade(),
                            cx,
                        ))
                        .child(div().flex_1())
                        .child(Preferences::fast_control(
                            model.quick_controls.clone(),
                            model.controls_enabled,
                            "model-fast",
                            false,
                            model.key.clone(),
                            pane.clone(),
                            cx,
                        ))
                        .child(Preferences::model_menu(
                            model.model_label.clone(),
                            render_owner,
                            &self.preferences.downgrade(),
                            cx,
                        ))
                        .child(Preferences::effort_control(
                            model.quick_controls.clone(),
                            model.controls_enabled,
                            "model-effort",
                            false,
                            model.key.clone(),
                            pane.clone(),
                            cx,
                        ))
                        .children(trailing),
                )
            })
            .when(phase.is_some(), |composer| {
                composer.child(Self::dictation_bar(
                    model.phase,
                    &model.levels,
                    model.connected,
                    cx,
                ))
            });
        let controls = v_flex()
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .gap_1()
            .when_some(
                model
                    .review
                    .as_deref()
                    .filter(|review| model.has_session && !review.files.is_empty()),
                |column, review| {
                    column.child(Self::review_card(
                        review,
                        model.review_expanded,
                        model.connected,
                        cx,
                    ))
                },
            )
            .when_some(model.question.as_ref(), |column, question| {
                column.child(Self::question_panel(question, cx))
            })
            .child(Self::composer_context(model, cx))
            .when_some(model.input_unavailable.as_ref(), |column, reason| {
                column.child(div().text_sm().text_color(rgb(appearance::MUTED)).child(
                    agent_core::localization::localized_ui_text(language, reason.clone()),
                ))
            })
            .child(composer);
        h_flex()
            .justify_center()
            .px_6()
            .pt_2()
            .pb_4()
            .child(controls)
            .into_any_element()
    }
}

/// The round primary action, inactive while there is nothing to send or stop.
fn round_action(button: Button, inactive: bool, cx: &App) -> Button {
    button.large().rounded_full().w(px(40.)).h(px(40.)).custom(
        ButtonCustomVariant::new(cx)
            .color(
                rgb(if inactive {
                    appearance::BORDER
                } else {
                    0xffffff
                })
                .into(),
            )
            .foreground(
                rgb(if inactive {
                    appearance::MUTED
                } else {
                    appearance::SIDEBAR
                })
                .into(),
            )
            .hover(rgb(0xd4d4d4).into())
            .active(rgb(appearance::MUTED).into()),
    )
}

#[derive(PartialEq)]
struct ComposerModel {
    key: DraftKey,
    session: Option<SessionRef>,
    attachments: Vec<Attachment>,
    running: Option<TurnId>,
    empty: bool,
    connected: bool,
    busy: bool,
    provider_available: bool,
    input_unavailable: Option<String>,
    phase: Option<Phase>,
    levels: std::collections::VecDeque<f32>,
    remote: Option<RemoteHost>,
    hosts: Option<Entity<Hosts>>,
    cwd: String,
    selected_directory: String,
    has_session: bool,
    review: Option<Arc<agent_protocol::models::WorkspaceReview>>,
    review_expanded: bool,
    model_label: String,
    permission_label: &'static str,
    permission_mode: Option<agent_protocol::permissions::PermissionMode>,
    quick_controls: agent_core::presentation::model_settings::ModelQuickControls,
    controls_enabled: bool,
    suggestions: Option<agent_core::composer::ComposerSuggestions>,
    completion_index: usize,
    question: Option<question::ComposerQuestion>,
}
impl Desktop {
    fn composer_model(&self, cx: &App) -> ComposerModel {
        let language = crate::localization::language(cx);
        let permission = self.snapshot.permission_control(self.draft_key());
        ComposerModel {
            key: self.draft_key().clone(),
            session: self.selected().cloned(),
            attachments: self.draft().attachments.clone(),
            running: self.thread().and_then(|thread| thread.active_turn_id()),
            empty: self.composer.input.read(cx).value().trim().is_empty()
                && self.draft().attachments.is_empty(),
            connected: self.snapshot.connection.connected,
            busy: self.busy > 0,
            provider_available: permission.provider.is_some(),
            input_unavailable: self.selected().and_then(|id| {
                self.snapshot
                    .input_unavailable_reason(id.clone(), permission.provider)
            }),
            phase: self.dictation.as_ref().map(|state| state.phase),
            levels: self
                .dictation
                .as_ref()
                .map(|state| state.levels.clone())
                .unwrap_or_default(),
            remote: self.remote.clone(),
            hosts: self.hosts.clone(),
            cwd: self.snapshot.pane.navigation.cwd.clone(),
            selected_directory: self.snapshot.selected_directory(),
            has_session: self.selected().is_some(),
            review: self.snapshot.pane.workspace.review.clone(),
            review_expanded: self.review_expanded,
            model_label: self
                .selected_model()
                .map_or(language.text("モデルを選択").to_owned(), |model| {
                    model.display_name.clone()
                }),
            permission_label: permission.label,
            permission_mode: permission.mode,
            quick_controls: self.snapshot.model_quick_controls(self.draft_key().clone()),
            controls_enabled: self.preferences.read(cx).model_controls_enabled(),
            suggestions: self.completion_suggestions(cx),
            completion_index: self.composer.completion_index,
            question: self.composer_question(),
        }
    }
}

/// The outer view measures natural height; only the fixed-size content is cached.
/// GPUI's cached views cannot measure their contents to determine their own size.
pub(in crate::app) struct ComposerView {
    pub(in crate::app) content: Entity<ComposerContent>,
    pub(super) height: Option<Pixels>,
    _measurement: Subscription,
}
pub(in crate::app) struct ComposerContent {
    desktop: WeakEntity<Desktop>,
    media: Entity<Media>,
    preferences: Entity<Preferences>,
    input: Entity<TextareaState>,
    question_input: Entity<TextareaState>,
    question_secret: Entity<InputState>,
    pane: Option<Arc<agent_core::store::Pane>>,
    model: Option<ComposerModel>,
    folders: Vec<(String, String)>,
    more_projects: bool,
    #[cfg(test)]
    pub(in crate::app) projections: usize,
    #[cfg(test)]
    pub(in crate::app) renders: usize,
}
pub(in crate::app) enum ComposerAction {
    NewChat(String),
    PickFolder,
    Send,
    Attach,
    StartDictation,
    CancelDictation,
    FinishDictation(bool),
    AddMentions(String),
    QuestionCollapsed(Option<(RequestId, String)>),
    Respond(RequestId, Answer),
    Review(Option<String>),
    RefreshReview,
    ToggleReview,
    Completion(agent_protocol::composer::Invocation),
}
impl EventEmitter<ComposerAction> for ComposerContent {}
struct ComposerHeight(Pixels);
impl EventEmitter<ComposerHeight> for ComposerContent {}
impl ComposerView {
    pub(in crate::app) fn new(
        desktop: &Entity<Desktop>,
        media: Entity<Media>,
        preferences: Entity<Preferences>,
        input: Entity<TextareaState>,
        question_input: Entity<TextareaState>,
        question_secret: Entity<InputState>,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = cx.new(|cx| {
            cx.observe_global::<crate::localization::Language>(|_, cx| cx.notify())
                .detach();
            ComposerContent {
                desktop: desktop.downgrade(),
                media,
                preferences,
                input: input.clone(),
                question_input,
                question_secret,
                pane: None,
                model: None,
                folders: Vec::new(),
                more_projects: false,
                #[cfg(test)]
                projections: 0,
                #[cfg(test)]
                renders: 0,
            }
        });
        Self {
            _measurement: cx.subscribe(&content, |view, _, height: &ComposerHeight, cx| {
                if view.height != Some(height.0) {
                    view.height = Some(height.0);
                    cx.notify();
                }
            }),
            content,
            height: None,
        }
    }
}
impl ComposerContent {
    pub(super) fn dispatch(&self, intent: Intent) {
        if let Some(pane) = &self.pane {
            drop(pane.dispatch(intent));
        }
    }
    pub(in crate::app) fn set_controls_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if let Some(model) = &mut self.model
            && model.controls_enabled != enabled
        {
            model.controls_enabled = enabled;
            cx.notify();
        }
    }
    pub(in crate::app) fn set_folders(
        &mut self,
        folders: Vec<(String, String)>,
        more: bool,
        cx: &mut Context<Self>,
    ) {
        if self.folders != folders || self.more_projects != more {
            self.folders = folders;
            self.more_projects = more;
            cx.notify();
        }
    }
    fn set_model(&mut self, model: ComposerModel, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.projections += 1;
        }
        if self.model.as_ref() != Some(&model) {
            self.model = Some(model);
            cx.notify();
        }
    }
}
impl Render for ComposerView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match self.height {
            Some(height) => self
                .content
                .clone()
                .cached(
                    StyleRefinement::default()
                        .w_full()
                        .h(height)
                        .flex_shrink_0(),
                )
                .into_any_element(),
            None => self.content.clone().into_any_element(),
        }
    }
}
impl Render for ComposerContent {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        let owner = cx.entity().downgrade();
        let content = self
            .model
            .as_ref()
            .map(|model| self.render_composer(model, cx))
            .unwrap_or_else(|| div().into_any_element());
        div()
            .relative()
            .w_full()
            .flex_shrink_0()
            .child(content)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ =
                            owner.update(cx, |_, cx| cx.emit(ComposerHeight(bounds.size.height)));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod composer_tests {
    use super::*;
    use agent_protocol::{execution::TurnStatus, items::ItemBody};
    use core::prelude::v1::test;
    use gpui_kit as gpui;

    struct ComposerView(Entity<Desktop>);
    impl Render for ComposerView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |view, _| view.chat())
        }
    }

    #[gpui::test]
    fn project_publications_refresh_settings_and_folder_choices_without_navigation(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                sessions: Default::default(),
                connections: Default::default(),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let (desktop, window) = cx.add_window_view(|window, cx| {
            let mut desktop = Desktop::new(
                Mode::Fixture {
                    remote: None,
                    cwd: "/fixture".into(),
                },
                window,
                cx,
            );
            desktop.project_fixture();
            desktop
        });
        window.run_until_parked();
        for name in [Some("Project"), Some("Renamed"), None] {
            window.update(|window, cx| {
                desktop.update(cx, |view, cx| {
                    Arc::make_mut(&mut view.snapshot).lists.threads = name.map(|name| {
                        let value = serde_json::json!({
                            "data":[], "projects":[{"id":"project", "name":name,
                            "roots":[{"path":"/project"}]}],
                            "hasMore":false, "hasMoreProjects":true, "projectPages":{}, "limit":5
                        });
                        Arc::new(serde_json::from_value(value).unwrap())
                    });
                    let previous = view.publication.clone();
                    view.project_fixture();
                    let changes = view.publication.changes(previous);
                    assert!(changes.list && !changes.groups.pane);
                    view.publish_presentation(changes, window, cx);
                    let expected = name
                        .map(|name| vec![("project".into(), name.into())])
                        .unwrap_or_default();
                    assert_eq!(view.preferences.read(cx).projects, expected);
                    let composer = view.composer_view.read(cx).content.read(cx);
                    let expected = name
                        .map(|name| vec![("/project".into(), name.into())])
                        .unwrap_or_default();
                    assert_eq!(composer.folders, expected);
                    assert_eq!(composer.more_projects, name.is_some());
                })
            });
            window.run_until_parked();
        }
    }

    #[gpui::test]
    fn history_control_preserves_visible_content_and_offers_retry_after_a_failed_page(
        cx: &mut TestAppContext,
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                sessions: Default::default(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let session = SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "history".into(),
        };
        let turns = |range: std::ops::Range<usize>| {
            range
                .map(|index| {
                    Arc::new(Turn {
                        id: format!("turn-{index}").into(),
                        status: TurnStatus::Completed,
                        items: Some(vec![Arc::new(Item::new(
                            format!("answer-{index}").into(),
                            Default::default(),
                            ItemBody::AssistantText {
                                text: format!(
                                    "Answer {index}\n{}",
                                    "Earlier conversation text.\n".repeat(8)
                                ),
                                phase: agent_protocol::items::AssistantPhase::Final,
                            },
                        ))]),
                        ..Default::default()
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut handle = None;
        let (_, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::Fixture {
                        remote: Some(RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid".into(),
                        }),
                        cwd: "/fixture".into(),
                    },
                    window,
                    cx,
                )
            });
            desktop.update(cx, |view, cx| {
                let snapshot = Arc::make_mut(&mut view.snapshot);
                let navigation = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.pane).navigation);
                navigation.thread_id = Some(session.clone());
                navigation.draft_key = session.clone().into();
                Arc::make_mut(&mut snapshot.sessions.conversations).insert(
                    session.clone(),
                    Arc::new(Thread {
                        id: Some(session.clone()),
                        turns: Some(turns(2..8)),
                        history_has_more: Some(true),
                        history_cursor: Some("older-page".into()),
                        history_read_state: Some(agent_protocol::session::HistoryReadState::new(
                            agent_protocol::session::HistoryReadKind::Partial,
                            Vec::new(),
                        )),
                        ..Default::default()
                    }),
                );
                view.project_fixture();
                view.publish_views(window, cx);
                view.conversation.update(cx, |view, cx| {
                    view.timeline.list.scroll_to(ListOffset::default());
                    cx.notify();
                });
            });
            handle = Some(desktop.clone());
            ComposerView(desktop)
        });
        let desktop = handle.unwrap();
        window.simulate_resize(size(px(800.), px(700.)));
        window.run_until_parked();
        let button = window
            .debug_bounds("conversation-history-load")
            .expect("older-history control at the top");
        window.simulate_click(button.center(), Modifiers::default());
        let top = window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                assert!(!view.conversation.read(cx).timeline.list.is_following_tail());
                view.conversation
                    .read(cx)
                    .timeline
                    .list
                    .bounds_for_item(1)
                    .unwrap()
                    .top()
            })
        });
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                let thread = Arc::make_mut(
                    Arc::make_mut(&mut Arc::make_mut(&mut view.snapshot).sessions.conversations)
                        .get_mut(&session)
                        .unwrap(),
                );
                thread.turns = Some(turns(0..8));
                view.project_fixture();
                cx.notify();
            })
        });
        window.run_until_parked();
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                assert!(
                    (view
                        .conversation
                        .read(cx)
                        .timeline
                        .list
                        .bounds_for_item(3)
                        .unwrap()
                        .top()
                        - top)
                        .abs()
                        < px(1.),
                    "prepending history must keep the old first answer at the same screen position"
                );
                view.conversation.update(cx, |view, cx| {
                    view.timeline.list.scroll_to(ListOffset::default());
                    cx.notify();
                });
            })
        });
        let key = op::OperationKey::History {
            session: session.clone(),
        };
        let mut store_handle = None;
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                let store = runtime.block_on(async {
                    Arc::new(agent_core::store::Store::offline((*view.snapshot).clone()))
                });
                view.snapshot = store.snapshot();
                let app_runtime = view.runtime.clone();
                view.session = Some(runtime.block_on(async {
                    let (updates, receive) = async_channel::unbounded();
                    tokio::spawn(StoreSession::publish(
                        Ok(store.clone()),
                        app_runtime,
                        updates,
                        Some,
                        |_| None,
                    ));
                    receive.recv().await.unwrap().unwrap().unwrap()
                }));
                store_handle = Some(store);

                cx.notify();
            })
        });
        window.run_until_parked();
        let store = store_handle.unwrap();
        for attempt in 0..2 {
            let previous_generation = store
                .snapshot()
                .operations
                .get(&key)
                .map_or(0, |state| state.generation);
            let button = window.debug_bounds("conversation-history-load").unwrap();
            window.simulate_click(button.center(), Modifiers::default());
            runtime.block_on(async {
                let mut updates = store.subscribe();
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let failed = updates
                            .borrow_and_update()
                            .operations
                            .get(&key)
                            .is_some_and(|state| {
                                state.generation > previous_generation
                                    && matches!(state.phase, op::OperationPhase::Failed { .. })
                            });
                        if failed {
                            break;
                        }
                        updates.changed().await.unwrap();
                    }
                })
                .await
                .expect("each button press must execute a history request, including retry");
            });
            window.update(|window, cx| {
                desktop.update(cx, |view, cx| {
                    view.accept_snapshot(window, cx);
                    cx.notify();
                })
            });
            window.run_until_parked();
            assert!(
                window.debug_bounds("conversation-history-error").is_some(),
                "request failure {attempt} must appear inline"
            );
        }
        assert!(
            window.debug_bounds("conversation-history-load").is_some(),
            "the same control remains available to retry"
        );
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                view.session.take();
                let snapshot = Arc::make_mut(&mut view.snapshot);
                snapshot.operations.remove(&key);
                let thread = Arc::make_mut(
                    Arc::make_mut(&mut snapshot.sessions.conversations)
                        .get_mut(&session)
                        .unwrap(),
                );
                thread.history_has_more = Some(false);
                thread.history_cursor = None;

                cx.notify();
            })
        });
        window.run_until_parked();
        assert!(window.debug_bounds("conversation-history-load").is_none());
        assert!(window.debug_bounds("conversation-history-error").is_none());
    }

    #[gpui::test]
    fn a_question_docks_above_the_composer_until_it_is_collapsed(cx: &mut TestAppContext) {
        use agent_protocol::requests::{Question, QuestionChoice, Request, RequestBody};
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                sessions: Default::default(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let session = SessionRef {
            provider: agent_protocol::session::ProviderKind::Claude,
            id: "asking".into(),
        };
        let question = |id: &str, multiple| Question {
            id: id.into(),
            header: format!("{id} header"),
            prompt: format!("{id}?"),
            secret: false,
            allow_free_text: true,
            multiple,
            choices: ["a", "b"]
                .map(|choice| QuestionChoice {
                    id: format!("{id}-{choice}"),
                    label: choice.into(),
                    description: format!("{choice} description"),
                })
                .into(),
        };
        let mut handle = None;
        let (_, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::Fixture {
                        remote: Some(RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid".into(),
                        }),
                        cwd: "/fixture".into(),
                    },
                    window,
                    cx,
                )
            });
            desktop.update(cx, |view, cx| {
                let snapshot = Arc::make_mut(&mut view.snapshot);
                let navigation = Arc::make_mut(&mut Arc::make_mut(&mut snapshot.pane).navigation);
                navigation.thread_id = Some(session.clone());
                navigation.draft_key = session.clone().into();
                let mut thread = Thread {
                    id: Some(session.clone()),
                    turns: Some(Vec::new()),
                    ..Default::default()
                };
                thread.requests.insert(
                    "request".into(),
                    Arc::new(Request {
                        id: "request".into(),
                        target: agent_protocol::requests::RequestTarget::Session,
                        delivery: agent_protocol::session::RequestDelivery::Awaiting,
                        body: RequestBody::Question {
                            questions: vec![question("scope", false), question("notes", true)],
                        },
                    }),
                );
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .insert(session.clone(), Arc::new(thread));
                Arc::make_mut(&mut snapshot.input.drafts).insert(
                    session.clone().into(),
                    Arc::new(Draft {
                        text: "Keep draft".into(),
                        ..Default::default()
                    }),
                );
                view.sync_inputs(window, cx);
                view.project_fixture();
                view.publish_views(window, cx);
            });
            handle = Some(desktop.clone());
            ComposerView(desktop)
        });
        let desktop = handle.unwrap();
        window.simulate_resize(size(px(800.), px(700.)));
        window.run_until_parked();
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                assert!(
                    view.conversation.read(cx).timeline.requests.is_empty(),
                    "the timeline keeps no answer inputs for a question"
                );
            })
        });
        assert!(window.debug_bounds("question-choice-scope-a").is_some());
        assert!(window.debug_bounds("question-choice-notes-a").is_none());
        assert!(window.debug_bounds("question-advance").is_some());
        assert!(window.debug_bounds("question-back").is_none());
        assert!(
            window.debug_bounds("attach").is_none(),
            "an answer has no attachments"
        );
        let toggle = window.debug_bounds("question-toggle").unwrap();
        window.simulate_click(toggle.center(), Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("question-choice-scope-a").is_none());
        assert!(window.debug_bounds("question-skip").is_none());
        let toggle = window.debug_bounds("question-toggle").unwrap();
        window.simulate_click(toggle.center(), Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("question-choice-scope-a").is_some());
        assert!(window.debug_bounds("question-skip").is_some());
        window.update(|window, cx| {
            desktop.update(cx, |view, cx| {
                let transition =
                    |view: &mut Desktop, action, window: &mut Window, cx: &mut Context<Desktop>| {
                        let (snapshot, _) = agent_core::state::reduce(
                            &view.snapshot,
                            agent_core::state::Event::Intent(Intent::AnswerQuestion {
                                request_id: "request".into(),
                                action,
                            }),
                        );
                        view.snapshot = Arc::new(snapshot);
                        view.sync_inputs(window, cx);
                    };
                use agent_core::questions::QuestionAction;
                transition(
                    view,
                    QuestionAction::Edit {
                        text: "own answer".into(),
                    },
                    window,
                    cx,
                );
                assert_eq!(view.question.input.read(cx).value(), "own answer");
                assert_eq!(view.question.secret.read(cx).value(), "own answer");
                let revision = view.question.text.record("unsaved".into());
                view.question.set_value("unsaved".into(), window, cx);
                view.sync_inputs(window, cx);
                assert_eq!(view.question.input.read(cx).value(), "unsaved");
                transition(
                    view,
                    QuestionAction::Choose {
                        choice_id: "scope-a".into(),
                    },
                    window,
                    cx,
                );
                view.question.text.saved(revision);
                assert_eq!(view.question.input.read(cx).value(), "");
                assert_eq!(
                    view.composer.input.read(cx).value(),
                    "Keep draft\n\nown answer"
                );
                transition(
                    view,
                    QuestionAction::Edit {
                        text: "notes answer".into(),
                    },
                    window,
                    cx,
                );
                transition(view, QuestionAction::Back, window, cx);
                assert_eq!(view.question.input.read(cx).value(), "");
                transition(view, QuestionAction::Advance, window, cx);
                assert_eq!(view.question.input.read(cx).value(), "notes answer");
                assert_eq!(view.question.secret.read(cx).value(), "notes answer");
                cx.notify();
            });
        });
        window.run_until_parked();
        let toggle = window.debug_bounds("question-toggle").unwrap();
        window.simulate_click(toggle.center(), Modifiers::default());
        window.run_until_parked();
        assert!(window.debug_bounds("question-choice-notes-a").is_none());
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                let snapshot = Arc::make_mut(&mut view.snapshot);
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .get_mut(&session)
                    .map(Arc::make_mut)
                    .unwrap()
                    .requests
                    .clear();
                cx.notify();
            })
        });
        window.run_until_parked();
        assert!(window.debug_bounds("question-toggle").is_none());
        assert!(
            window.debug_bounds("attach").is_some(),
            "the draft returns once the question is gone"
        );
        window.update(|window, cx| {
            desktop.update(cx, |view, cx| {
                let snapshot = Arc::make_mut(&mut view.snapshot);
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .get_mut(&session)
                    .map(Arc::make_mut)
                    .unwrap()
                    .requests
                    .insert(
                        "next-request".into(),
                        Arc::new(Request {
                            id: "next-request".into(),
                            target: agent_protocol::requests::RequestTarget::Session,
                            delivery: agent_protocol::session::RequestDelivery::Awaiting,
                            body: RequestBody::Question {
                                questions: vec![question("notes", true)],
                            },
                        }),
                    );
                view.sync_inputs(window, cx);
                cx.notify();
            });
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("question-choice-notes-a").is_some(),
            "a new request opens even when it reuses the previous question's ID"
        );
    }

    #[gpui::test]
    fn removing_an_image_does_not_open_its_gallery(cx: &mut TestAppContext) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                sessions: Default::default(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let image_directory = tempfile::tempdir().unwrap();
        let image_path = image_directory.path().join("attachment.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]))
            .save(&image_path)
            .unwrap();
        let source = image_path.to_string_lossy().into_owned();
        let mut desktop_handle = None;
        let mut store_handle = None;
        let mut draft_key = None;
        let (_, window) = cx.add_window_view(|window, cx| {
            let desktop = cx.new(|cx| {
                Desktop::new(
                    Mode::Fixture {
                        remote: Some(RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid".into(),
                        }),
                        cwd: image_directory.path().to_string_lossy().into_owned(),
                    },
                    window,
                    cx,
                )
            });
            desktop.update(cx, |view, cx| {
                let key = view.draft_key().clone();
                draft_key = Some(key.clone());
                Arc::make_mut(&mut Arc::make_mut(&mut view.snapshot).input.drafts).insert(
                    key,
                    Arc::new(Draft {
                        text: "Keep this draft".into(),
                        attachments: vec![agent_core::state::Attachment {
                            path: source.clone(),
                            name: "attachment.png".into(),
                            is_image: true,
                        }],
                        ..Default::default()
                    }),
                );
                view.publish_views(window, cx);
                view.media.update(cx, |media, cx| {
                    media.fixture_image(&source, source.clone().into(), cx);
                });
                let store = runtime.block_on(async {
                    Arc::new(agent_core::store::Store::offline((*view.snapshot).clone()))
                });
                let app_runtime = view.runtime.clone();
                view.session = Some(runtime.block_on(async {
                    let (updates, receive) = async_channel::unbounded();
                    tokio::spawn(StoreSession::publish(
                        Ok(store.clone()),
                        app_runtime,
                        updates,
                        Some,
                        |_| None,
                    ));
                    receive.recv().await.unwrap().unwrap().unwrap()
                }));
                store_handle = Some(store);
            });
            desktop_handle = Some(desktop.clone());
            let content = cx.new(|_| ComposerView(desktop));
            crate::activity::ActivityWindow::new(content, window, cx)
        });
        window.simulate_resize(size(px(700.), px(700.)));
        window.run_until_parked();
        let bounds = window.debug_bounds("chat-image").unwrap();
        window.simulate_click(bounds.center(), Modifiers::default());
        let desktop = desktop_handle.unwrap();
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                assert!(
                    view.media.read(cx).gallery.is_some(),
                    "the thumbnail still opens the gallery"
                );
                view.media.update(cx, |media, cx| media.close_gallery(cx));
            })
        });
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                view.media.update(cx, |media, cx| {
                    media.asset(&source, false, cx).update(cx, |asset, cx| {
                        asset.path = None;
                        cx.notify();
                    });
                });
                view.composer_view
                    .read(cx)
                    .content
                    .clone()
                    .update(cx, |_, cx| cx.notify());
            })
        });
        window.simulate_mouse_move(bounds.center(), None, Modifiers::default());
        window.run_until_parked();
        let remove = window.debug_bounds("attachment-remove-0").unwrap().center();
        window.update(|window, _| {
            let scale = window.scale_factor();
            let point = remove.scale(scale);
            let quads = window.painted_quads();
            let pulse = quads
                .iter()
                .filter(|quad| {
                    quad.bounds.size == size(px(120.), px(120.)).scale(scale)
                        && quad.bounds.contains(&point)
                })
                .max_by_key(|quad| quad.order)
                .expect("loading image pulse must be painted");
            let button = quads
                .iter()
                .find(|quad| {
                    quad.bounds.size == size(px(18.), px(18.)).scale(scale)
                        && quad.bounds.contains(&point)
                        && quad.background == rgb(0xffffff).into()
                })
                .expect("remove button must be painted");
            assert!(
                button.order > pulse.order,
                "the remove button must paint above the image loading pulse"
            );
        });
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let _observation = window.update(|_, cx| {
            let notifications = notifications.clone();
            cx.observe(&desktop, move |_, _| {
                notifications.set(notifications.get() + 1)
            })
        });
        window.simulate_click(remove, Modifiers::default());
        window.update(|_, cx| {
            desktop.update(cx, |view, cx| {
                assert!(view.media.read(cx).gallery.is_none())
            })
        });
        assert_eq!(
            notifications.get(),
            0,
            "removing an attachment must not notify Desktop observers"
        );
        let snapshot = store_handle.unwrap().snapshot();
        let draft = &snapshot.input.drafts[&draft_key.unwrap()];
        assert!(draft.attachments.is_empty());
        assert_eq!(draft.text, "Keep this draft");
    }
}

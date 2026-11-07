use super::*;
use agent_core::presentation::conversation::{
    ActionTone, ConversationRowContent, WorkProgress, activity_is_expanded,
};

#[derive(Default, PartialEq)]
pub(in crate::app) struct ConversationModel {
    draft_key: DraftKey,
    session: Option<SessionRef>,
    cwd: String,
    home: Option<String>,
    epoch: u64,
    connected: bool,
    busy: bool,
    history_notice: Option<String>,
    history: Option<agent_core::presentation::conversation::HistoryControl>,
    docked: Vec<agent_core::presentation::conversation::Request>,
    errors: Vec<super::errors::ErrorRow>,
    running: std::collections::BTreeSet<op::OperationKey>,
    failures: BTreeMap<op::OperationKey, agent_core::presentation::error::ErrorText>,
}

pub(in crate::app) enum ConversationAction {
    Detail(TurnId, ItemId),
    Download(String),
    Edit(String),
    Usage(Option<agent_protocol::session::ProviderKind>),
    OpenUsage,
    Fork(SessionRef, TurnId),
    Respond(RequestId, Answer),
    Error(String),
    Notice(errors::NoticeAction),
    OpenLink(String, bool),
}

pub(in crate::app) struct ConversationView {
    desktop: WeakEntity<Desktop>,
    pane: Option<Arc<agent_core::store::Pane>>,
    model: ConversationModel,
    metadata: agent_core::presentation::files::FileMetadataCache,
    media: Entity<Media>,
    pub(in crate::app) timeline: Timeline,
    selection: Entity<selection::ConversationSelection>,
    #[cfg(test)]
    pub(in crate::app) projections: usize,
    #[cfg(test)]
    pub(in crate::app) renders: usize,
}
impl EventEmitter<ConversationAction> for ConversationView {}
impl ConversationView {
    pub(in crate::app) fn new(
        desktop: &Entity<Desktop>,
        selection: Entity<selection::ConversationSelection>,
        media: Entity<Media>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe_global::<crate::localization::Language>(|_, cx| cx.notify())
            .detach();
        Self {
            desktop: desktop.downgrade(),
            pane: None,
            model: ConversationModel::default(),
            metadata: Default::default(),
            timeline: Timeline::new(),
            selection,
            media,
            #[cfg(test)]
            projections: 0,
            #[cfg(test)]
            renders: 0,
        }
    }
    pub(super) fn dispatch(&self, intent: Intent) {
        if let Some(pane) = &self.pane {
            drop(pane.dispatch(intent));
        }
    }
    pub(in crate::app) fn sync(
        &mut self,
        model: ConversationModel,
        rendered: Option<Arc<agent_core::presentation::conversation::RenderedConversation>>,
        requests: Vec<Arc<agent_protocol::requests::Request>>,
        metadata: agent_core::presentation::files::FileMetadataCache,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        #[cfg(test)]
        {
            self.projections += 1;
        }
        let same_content = match (&rendered, &self.timeline.rendered) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if self.metadata != metadata {
            for cached in self.timeline.markdown.values() {
                cached.file_chips.sync_metadata(&metadata, cx);
            }
            self.metadata = metadata;
        }
        if self.model == model && same_content {
            return;
        }
        if self.model.cwd != model.cwd || self.model.home != model.home {
            self.timeline.markdown.clear();
        } else if self.model.epoch != model.epoch
            && let Some(desktop) = self.desktop.upgrade()
        {
            // Rebind native handlers without dropping code/table presentation choices.
            for cached in self.timeline.markdown.values_mut() {
                cached.file_chips = super::markdown_file::FileChips::new(
                    cached.file_chips.files.clone(),
                    &desktop,
                    model.epoch,
                    &self.metadata,
                    cx,
                );
                cached.parser_revision = uuid::Uuid::new_v4().as_u64_pair().0;
            }
        }
        self.sync_request_inputs(&requests, window, cx);
        let reset = self.model.draft_key != model.draft_key;
        if reset {
            self.timeline.forget_rendering();
        }
        self.timeline.sync(rendered, model.history.clone(), reset);
        self.model = model;
        cx.notify();
    }
}
impl Desktop {
    pub(in crate::app) fn conversation_model(&self) -> ConversationModel {
        let mut running = std::collections::BTreeSet::new();
        let mut failures = BTreeMap::new();
        for key in self.snapshot.operations.keys().filter(|key| match key {
            op::OperationKey::Item { item } => self.selected() == Some(&item.thread_id),
            op::OperationKey::Fork { session, .. } => self.selected() == Some(session),
            op::OperationKey::Request { id } => self
                .thread()
                .is_some_and(|thread| thread.requests.contains_key(id)),
            _ => false,
        }) {
            if self.snapshot.operation_running(key.clone()) {
                running.insert(key.clone());
            }
            if let Some(error) = self.snapshot.operation_error(key.clone()) {
                failures.insert(key.clone(), error);
            }
        }
        ConversationModel {
            draft_key: self.draft_key().clone(),
            session: self.selected().cloned(),
            cwd: self.snapshot.pane.navigation.cwd.clone(),
            home: self.snapshot.connection.host_home_directory.clone(),
            epoch: self.snapshot.connection.epoch,
            connected: self.snapshot.connection.connected,
            busy: self.busy > 0,
            history_notice: self
                .thread()
                .and_then(|thread| agent_core::presentation::conversation::history_notice(thread)),
            history: self
                .selected()
                .cloned()
                .and_then(|session| self.snapshot.history_control(session)),
            docked: self
                .selected()
                .into_iter()
                .flat_map(|session| self.snapshot.docked_requests(session.clone()))
                // Questions use the composer's answer field and question panel.
                .filter(|request| {
                    !matches!(
                        request.request_body,
                        agent_protocol::requests::RequestBody::Question { .. }
                    )
                })
                .collect(),
            errors: self.error_rows(&ErrorScope::Conversation {
                draft_key: self.draft_key().clone(),
            }),
            running,
            failures,
        }
    }
    pub(in crate::app) fn publish_conversation(&self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.conversation_model();
        let rendered = self.publication.conversation.clone();
        let requests = rendered
            .as_ref()
            .into_iter()
            .flat_map(|value| value.source.requests.values())
            .cloned()
            .collect();
        self.conversation.update(cx, |view, cx| {
            view.pane = self.session.as_ref().map(|session| session.pane.clone());
            view.sync(
                model,
                rendered,
                requests,
                self.snapshot.files.workspace.file_metadata.clone(),
                window,
                cx,
            )
        });
    }

    pub(in crate::app) fn conversation_action(
        &mut self,
        action: &ConversationAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            ConversationAction::Detail(turn, item) => self.detail(turn.clone(), item.clone()),
            ConversationAction::Download(path) => self.download(path.clone()),
            ConversationAction::Edit(text) => {
                selection::set_composer_text(&self.composer.input, text.clone().into(), window, cx)
            }
            ConversationAction::Usage(provider) => {
                self.open_settings(SettingsPage::Providers, window, cx);
                self.preferences.update(cx, |view, cx| {
                    view.model_controls.provider = *provider;
                    cx.notify();
                });
            }
            ConversationAction::OpenUsage => {
                self.open_browser(self.panel_scope(), "https://chatgpt.com/settings/usage", cx)
            }
            ConversationAction::Fork(thread, turn) => {
                if !self.snapshot.connection.connected || self.busy > 0 {
                    return;
                }
                self.busy += 1;
                self.perform(
                    Intent::ForkSession(op::ForkSession::new(thread.clone(), turn.clone())),
                    OperationCompletion::Busy,
                );
                self.accept_snapshot(window, cx);
                self.conversation
                    .update(cx, |view, _| view.timeline.list.remeasure());
            }
            ConversationAction::Respond(id, answer) => {
                self.conversation.update(cx, |view, cx| {
                    if let Some(inputs) = view.timeline.requests.get_mut(id) {
                        inputs.sent = true;
                    }
                    cx.notify();
                });
                self.respond(id.clone(), answer.clone());
            }
            ConversationAction::Error(error) => self.set_error(error.clone()),
            ConversationAction::Notice(action) => self.notice_action(action.clone(), window, cx),
            ConversationAction::OpenLink(source, control) => {
                self.click_conversation_link(source, *control, window, cx)
            }
        }
    }
}

struct WorkProgressView {
    progress: WorkProgress,
    _clock: Task<()>,
}

impl WorkProgressView {
    fn view(id: &str, progress: WorkProgress, window: &mut Window, cx: &mut App) -> Entity<Self> {
        // use_keyed_state observes its entity and notifies the parent view.
        // Retain the clock without forwarding each tick as an entity notification.
        // Its changing text width still requires GPUI to lay out the ancestors.
        let view = window.with_global_id(
            SharedString::from(format!("work-progress-{id}")).into(),
            |id, window| {
                window.with_element_state::<Entity<Self>, _>(id, |retained, _| {
                    let view = retained.unwrap_or_else(|| {
                        cx.new(|cx| Self {
                            progress: progress.clone(),
                            _clock: cx.spawn(async move |view, cx| {
                                loop {
                                    cx.background_executor()
                                        .timer(std::time::Duration::from_secs(1))
                                        .await;
                                    if view.update(cx, |_, cx| cx.notify()).is_err() {
                                        break;
                                    }
                                }
                            }),
                        })
                    });
                    (view.clone(), view)
                })
            },
        );
        view.update(cx, |view, cx| {
            if view.progress != progress {
                view.progress = progress;
                cx.notify();
            }
        });
        view
    }
    fn label(&self, language: agent_core::localization::UiLanguage) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0., |duration| duration.as_secs_f64());
        agent_core::localization::localized_ui_text(language, self.progress.label(now))
    }
}

impl Render for WorkProgressView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().child(self.label(crate::localization::language(cx)))
    }
}

impl ConversationView {
    pub(super) fn conversation_navigation(&self, cx: &Context<Self>) -> AnyElement {
        let language = crate::localization::language(cx);
        let entries: Vec<_> = self
            .timeline
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let ConversationRow::Turn {
                    turn,
                    index: row_index,
                    ..
                } = row
                else {
                    return None;
                };
                match &turn.rows[*row_index].content {
                    ConversationRowContent::User { item } => {
                        item.data.body.as_ref().map(|text| (index, text, turn))
                    }
                    _ => None,
                }
            })
            .collect();
        let count = entries.len().min(8);
        let mut markers = v_flex()
            .absolute()
            .left_0()
            .top_0()
            .h_full()
            .w(px(56.))
            .justify_center();
        let current = self.timeline.list.logical_scroll_top().item_ix;
        for position in 0..count {
            let sample = position * (entries.len() - 1) / count.saturating_sub(1).max(1);
            let (index, user, turn) = entries[sample];
            let active = current >= index
                && (position + 1 == count
                    || current < entries[(position + 1) * (entries.len() - 1) / (count - 1)].0);
            let user: String = user.chars().take(160).collect();
            let distance = self
                .timeline
                .hovered_marker
                .map(|hovered| position.abs_diff(hovered));
            let width = match distance {
                Some(0) => 28.,
                Some(1) => 20.,
                Some(2) => 14.,
                Some(3) => 10.,
                _ => 7.,
            };
            let marker_id = SharedString::from(format!("conversation-marker-{index}"));
            let preview = (distance == Some(0)).then(|| {
                let answer = turn.rows.iter().find_map(|row| match &row.content {
                    ConversationRowContent::Response { item, .. } => item
                        .data
                        .body
                        .as_ref()
                        .map(|text| text.chars().take(240).collect::<String>()),
                    _ => None,
                });
                let preview_width = (self.timeline.list.viewport_bounds().size.width - px(48.))
                    .clamp(px(100.), px(300.));
                v_flex()
                    .absolute()
                    .left(px(56.))
                    .top(-px(24.))
                    .w(preview_width)
                    .p_3()
                    .gap_2()
                    .rounded_lg()
                    .bg(rgb(appearance::RAISED))
                    .border_1()
                    .border_color(rgb(appearance::BORDER))
                    .shadow_md()
                    .text_sm()
                    .child(div().max_h(px(44.)).overflow_hidden().child(user.clone()))
                    .children(answer.map(|answer| {
                        div()
                            .max_h(px(66.))
                            .overflow_hidden()
                            .text_color(rgb(appearance::MUTED))
                            .child(answer)
                    }))
            });
            markers = markers.child(
                div()
                    .id(marker_id.clone())
                    .on_hover(cx.listener(move |view, hovered, _, cx| {
                        if *hovered {
                            view.timeline.hovered_marker = Some(position);
                        } else if view.timeline.hovered_marker == Some(position) {
                            view.timeline.hovered_marker = None;
                        }
                        cx.notify();
                    }))
                    .relative()
                    .child(
                        Button::new(marker_id)
                            .text()
                            .small()
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.timeline.list.scroll_to(ListOffset {
                                    item_ix: index,
                                    offset_in_item: px(0.),
                                });
                                cx.notify();
                            }))
                            .debug_selector(move || format!("conversation-marker-{index}"))
                            .accessibility_label(agent_core::localization::localized_ui_format(
                                language,
                                "この発言へ移動: {0}".into(),
                                vec![user.to_string()],
                            ))
                            .w(px(56.))
                            .h(px(10.))
                            .pl(px(16.5))
                            .pr(px(11.5))
                            .child(
                                h_flex().w_full().child(
                                    div()
                                        .debug_selector(move || {
                                            format!("conversation-marker-line-{index}")
                                        })
                                        .h(px(if distance == Some(0) { 2. } else { 1. }))
                                        .w(px(width))
                                        .bg(rgb(if active || distance == Some(0) {
                                            appearance::FOREGROUND
                                        } else {
                                            appearance::MUTED
                                        })),
                                ),
                            ),
                    )
                    .children(preview),
            );
        }
        markers.into_any_element()
    }

    pub(super) fn diff(
        diffs: &mut HashMap<String, Entity<DiffView>>,
        id: String,
        patch: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let view = diffs
            .entry(id)
            .or_insert_with(|| cx.new(|_| DiffView::new(patch.to_owned().into(), false)))
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
    fn user_message_body(
        &mut self,
        id: &str,
        text: Option<&str>,
        content: &[agent_protocol::items::MessagePart],
        image_sources: &[String],
        cx: &mut Context<Self>,
    ) -> Div {
        let mut body = user_message_bubble();
        let mut has_body = false;
        if !content.is_empty() {
            for (i, part) in content.iter().enumerate() {
                use agent_protocol::items::MessagePart;
                let rendered = match part {
                    MessagePart::Image { .. } => continue,
                    MessagePart::Text { text } if text.is_empty() => continue,
                    MessagePart::Text { text } => self.markdown(format!("{id}-{i}"), text, cx),
                    MessagePart::Attachment { name, path }
                    | MessagePart::Invocation { name, path } => {
                        let path = path.clone();
                        button(
                            format!("{id}-{i}"),
                            if path.is_empty() {
                                name.clone()
                            } else {
                                file_name(&path)
                            },
                            cx,
                            move |_, _, cx| {
                                if !path.is_empty() {
                                    cx.emit(ConversationAction::Download(path.clone()));
                                }
                            },
                        )
                        .into_any_element()
                    }
                };
                has_body = true;
                body = body.child(rendered);
            }
        } else {
            has_body = !text.unwrap_or_default().is_empty();
            body = body.child(self.markdown(id.to_owned(), text.unwrap_or_default(), cx));
        }
        self.user_message_content(
            id,
            body,
            has_body,
            image_sources.iter().map(String::as_str),
            cx,
        )
    }
    pub(super) fn item(
        &mut self,
        projected: &agent_core::presentation::conversation::RenderedItem,
        item: &Item,
        turn: Option<&Turn>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language = crate::localization::language(cx);
        let id = item.id.to_string();
        let title = projected.data.title.as_deref();
        let body_text = projected.data.body.as_deref();
        let expanded = self.timeline.expanded_items.contains(&id);
        let deferred = projected.data.deferred;
        if expanded && deferred {
            let key = (
                turn.map(|turn| turn.id.clone()).unwrap_or_default(),
                item.id.clone(),
            );
            let operation =
                self.model
                    .session
                    .as_ref()
                    .cloned()
                    .map(|thread_id| op::OperationKey::Item {
                        item: op::ReadItem {
                            thread_id,
                            turn_id: turn.map(|turn| turn.id.clone()).unwrap_or_default(),
                            item_id: item.id.clone(),
                        },
                    });
            let error = operation
                .as_ref()
                .and_then(|key| self.model.failures.get(key));
            let label = if operation
                .as_ref()
                .is_some_and(|key| self.model.running.contains(key))
            {
                language.text("詳細を読み込み中…").to_owned()
            } else if error.is_some() {
                language.text("再試行").to_owned()
            } else {
                language.text("詳細を読み込む").to_owned()
            };
            let title = title.expect("activity has a title");
            let toggle = id.clone();
            return v_flex()
                .gap_2()
                .child(button(
                    format!("collapse-{id}"),
                    format!("⌄ {title}"),
                    cx,
                    move |s, _, _| {
                        s.timeline.expanded_items.remove(&toggle);
                        s.timeline.remeasure_item(&toggle);
                    },
                ))
                .child(button(
                    format!("detail-{id}"),
                    label,
                    cx,
                    move |s, _, cx| {
                        cx.emit(ConversationAction::Detail(key.0.clone(), key.1.clone()));
                        s.timeline.remeasure_item(key.1.as_str());
                    },
                ))
                .when_some(error, |body, error| body.child(error_view(error, language)))
                .into_any_element();
        }
        let turn_id = turn.map(|turn| turn.id.clone()).unwrap_or_default();
        match item.body() {
            agent_protocol::items::ItemBody::AssistantText { .. } => {
                self.markdown(id, body_text.expect("assistant message has text"), cx)
            }
            agent_protocol::items::ItemBody::ImageGeneration { .. } => {
                let mut body = v_flex().gap_2().w_full();
                if projected.data.image_placeholder {
                    body = body.child(image_skeleton(320., cx.theme().secondary));
                }
                for (index, source) in projected.data.image_sources.iter().enumerate() {
                    body = body.child(Media::preview(
                        &self.media,
                        format!("{id}-image-{index}"),
                        source,
                        false,
                        320.,
                        true,
                        cx,
                    ));
                }
                body.when_some(body_text, |body, text| {
                    body.child(div().text_sm().child(text.to_owned()))
                })
                .into_any_element()
            }
            agent_protocol::items::ItemBody::UserMessage { text, content } => {
                let body = self.user_message_body(
                    &id,
                    text.as_deref(),
                    content,
                    &projected.data.image_sources,
                    cx,
                );
                let text = body_text.expect("user message has text");
                let copied = text.to_owned();
                let edit = button(format!("edit-{id}"), "", cx, move |_, _, cx| {
                    cx.emit(ConversationAction::Edit(copied.clone()));
                });
                let timestamp = projected
                    .data
                    .timestamp_ms
                    .and_then(|time| i64::try_from(time / 1000).ok())
                    .or_else(|| turn.and_then(|turn| turn.started_at.map(|time| time as i64)));
                user_message_row(&id, text, body, timestamp, edit, language).into_any_element()
            }
            agent_protocol::items::ItemBody::CommandExecution {
                command,
                output,
                cwd,
                exit_code,
                ..
            } => {
                let label = title.expect("command has a title");
                let toggle = id.clone();
                let mut body = v_flex().gap_2().child(
                    button(
                        format!("expand-{id}"),
                        format!("{label} {}", if expanded { "⌄" } else { "›" }),
                        cx,
                        move |s, _, cx| {
                            toggle_set(&mut s.timeline.expanded_items, &toggle);
                            if s.timeline.expanded_items.contains(&toggle) {
                                cx.emit(ConversationAction::Detail(
                                    turn_id.clone(),
                                    toggle.clone().into(),
                                ));
                            }
                            s.timeline.pause_tail();
                            s.timeline.remeasure_item(&toggle);
                        },
                    )
                    .icon(IconName::SquareTerminal)
                    .text_color(rgb(appearance::MUTED)),
                );
                if expanded {
                    let copied = output.clone();
                    if let Some(cwd) = cwd {
                        body = body.child(
                            div()
                                .text_xs()
                                .text_color(rgb(appearance::MUTED))
                                .child(format!("cwd: {cwd}")),
                        );
                    }
                    body = body.child(Self::activity_text(format!("command-{id}"), command, "sh"));
                    body = body
                        .child(
                            h_flex()
                                .justify_between()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(rgb(appearance::MUTED))
                                        .child(language.text("プレーンテキスト")),
                                )
                                .child(
                                    Button::new(format!("copy-output-{id}"))
                                        .label(language.text("出力をコピー"))
                                        .small()
                                        .ghost()
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                copied.clone(),
                                            ))
                                        }),
                                ),
                        )
                        .child(Self::activity_text(format!("output-{id}"), output, ""))
                        .child(
                            div()
                                .text_sm()
                                .text_color(rgb(appearance::MUTED))
                                .child(format!(
                                    "{} {}",
                                    item.status.label(),
                                    exit_code.map(|v| format!("exit {v}")).unwrap_or_default()
                                )),
                        );
                }
                body.into_any_element()
            }
            agent_protocol::items::ItemBody::FileChange { .. } => {
                let mut body = v_flex().gap_3();
                for (i, change) in agent_core::presentation::body::file_changes(item).enumerate() {
                    let path = change.path.into_owned();
                    let toggle = id.clone();
                    let turn_id = turn_id.clone();
                    let selector = format!("change-{id}-{i}");
                    let row = h_flex()
                        .debug_selector(move || selector.clone())
                        .gap_2()
                        .child(button(
                            format!("change-{id}-{i}"),
                            format!("{} {}", if expanded { "⌄" } else { "›" }, file_name(&path)),
                            cx,
                            move |s, _, cx| {
                                toggle_set(&mut s.timeline.expanded_items, &toggle);
                                if s.timeline.expanded_items.contains(&toggle) {
                                    cx.emit(ConversationAction::Detail(
                                        turn_id.clone(),
                                        toggle.clone().into(),
                                    ));
                                }
                                s.timeline.pause_tail();
                                s.timeline.remeasure_item(&toggle);
                            },
                        ))
                        .child(button(
                            format!("download-{id}-{i}"),
                            language.text("ダウンロード"),
                            cx,
                            move |_, _, cx| cx.emit(ConversationAction::Download(path.clone())),
                        ));
                    body = body.child(row);
                    if expanded {
                        if let Some(diff) = &change.diff {
                            body = body.child(Self::diff(
                                &mut self.timeline.diffs,
                                format!("diff-{id}-{i}"),
                                diff,
                                cx,
                            ));
                        }
                        if change.diff.is_none() || change.proposal.is_some() {
                            let details = agent_core::presentation::body::file_change_details(
                                None,
                                change.proposal,
                            );
                            body = body.child(Self::activity_text(
                                format!("file-details-{id}-{i}"),
                                &details,
                                "",
                            ));
                        }
                    }
                }
                body.into_any_element()
            }
            agent_protocol::items::ItemBody::Plan { text } => {
                let copied = text.clone();
                let content = self.markdown(id.clone(), text, cx);
                v_flex()
                    .gap_2()
                    .w_full()
                    .p_3()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded_md()
                    .child(
                        h_flex()
                            .justify_between()
                            .child(language.text("計画"))
                            .child(
                                Button::new(format!("copy-plan-{id}"))
                                    .icon(IconName::Copy)
                                    .small()
                                    .ghost()
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copied.clone(),
                                        ))
                                    }),
                            ),
                    )
                    .child(content)
                    .into_any_element()
            }
            _ => {
                let toggle = id.clone();
                v_flex()
                    .gap_2()
                    .child(button(
                        format!("unknown-{id}"),
                        format!(
                            "{} {}",
                            if expanded { "⌄" } else { "›" },
                            title.expect("activity has a title")
                        ),
                        cx,
                        move |s, _, cx| {
                            toggle_set(&mut s.timeline.expanded_items, &toggle);
                            if s.timeline.expanded_items.contains(&toggle) {
                                cx.emit(ConversationAction::Detail(
                                    turn_id.clone(),
                                    toggle.clone().into(),
                                ));
                            }
                            s.timeline.pause_tail();
                            s.timeline.remeasure_item(&toggle);
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
    pub(super) fn turn_row(
        &mut self,
        session: Option<&SessionRef>,
        projected: &Arc<agent_core::presentation::conversation::RenderedTurn>,
        index: usize,
        last: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language = crate::localization::language(cx);
        let turn = &projected.source;
        let mut body = v_flex().w_full().max_w(px(CHAT_WIDTH)).gap_4();
        let row = &projected.rows[index];
        match &row.content {
            ConversationRowContent::User { item } => {
                body = body.child(self.projected_item(item, turn, cx));
            }
            ConversationRowContent::ActivityHeader { activity } => {
                let id = &activity.id;
                let expanded = activity_is_expanded(
                    &activity.status,
                    self.timeline.expanded_work.get(id).cloned(),
                );
                let (label, content) = if activity.is_in_progress {
                    let view = WorkProgressView::view(
                        &row.id,
                        projected.work_progress(!expanded),
                        window,
                        cx,
                    );
                    (
                        view.read(cx).label(crate::localization::language(cx)),
                        view.into_any_element(),
                    )
                } else {
                    let label = agent_core::localization::localized_ui_text(
                        language,
                        activity.activity_summary.clone(),
                    );
                    (label.clone(), div().child(label).into_any_element())
                };
                let header = if activity.activity_can_collapse {
                    let toggle = id.clone();
                    let status = activity.status.clone();
                    let load_items = activity.load_items.clone();
                    Button::new(format!("work-{id}"))
                        .accessibility_label(format!(
                            "{label} {}",
                            if expanded { "⌄" } else { "›" }
                        ))
                        .small()
                        .ghost()
                        .child(div().flex_1().min_w_0().text_ellipsis().child(content))
                        .child(if expanded { "⌄" } else { "›" })
                        .on_click(cx.listener(move |view, _, _, cx| {
                            if expanded {
                                view.timeline.expanded_work.remove(&toggle);
                            } else {
                                view.timeline
                                    .expanded_work
                                    .insert(toggle.clone(), status.clone());
                            }
                            if !expanded && let Some(params) = &load_items {
                                view.dispatch(Intent::LoadTurnItems(params.clone()));
                            }
                            view.timeline.pause_tail();
                            view.timeline
                                .rebuild_rows(view.model.history.clone(), false);
                            cx.notify();
                        }))
                        .text_color(rgb(appearance::MUTED))
                        .into_any_element()
                } else {
                    div()
                        .text_color(rgb(appearance::MUTED))
                        .child(content)
                        .into_any_element()
                };
                let selector = format!("work-{id}");
                body = body.child(
                    h_flex()
                        .debug_selector(move || selector.clone())
                        .gap_2()
                        .child(header)
                        .when(activity.is_in_progress, |row| {
                            row.child(crate::activity::indicator())
                        }),
                );
            }
            ConversationRowContent::Activity { item, .. } => {
                body = body.child(self.projected_item(item, turn, cx));
            }
            // Timeline rows leave requests to the composer's docked cards.
            ConversationRowContent::PendingRequest { .. } => {}
            ConversationRowContent::ProviderHandoff { text } => {
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(rgb(appearance::MUTED))
                        .py_2()
                        .child(text.clone()),
                );
            }
            ConversationRowContent::Error { error } => {
                let provider = turn.provider.or_else(|| session.map(|s| s.provider));
                let accent = if error.is_reconnecting {
                    rgb(appearance::MUTED)
                } else if error.is_usage_limit {
                    rgb(0xe5b567)
                } else {
                    rgb(0xff8e86)
                };
                let copied = format!(
                    "{}\n{}\n{}",
                    error.title,
                    error.message,
                    error.details.as_deref().unwrap_or_default()
                );
                body = body.child(
                    v_flex()
                        .debug_selector(|| "turn-error".into())
                        .p_3()
                        .rounded_lg()
                        .bg(rgb(appearance::RAISED))
                        .border_1()
                        .border_color(accent.opacity(0.25))
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(appearance::MUTED))
                                .child(language.text("この応答")),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .when(error.is_reconnecting, |heading| {
                                    heading.child(crate::activity::indicator())
                                })
                                .child(div().flex_1().font_semibold().text_color(accent).child(
                                    agent_core::localization::localized_ui_text(
                                        language,
                                        error.title.clone(),
                                    ),
                                ))
                                .child(icon_button(
                                    format!("copy-turn-error-{}", turn.id),
                                    IconName::Copy,
                                    language.text("エラーの詳細をコピー"),
                                    cx,
                                    move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            copied.clone(),
                                        ))
                                    },
                                )),
                        )
                        .child(
                            div()
                                .text_sm()
                                .child(agent_core::localization::localized_ui_text(
                                    language,
                                    error.message.clone(),
                                )),
                        )
                        .when_some(error.details.clone(), |card, details| {
                            card.child(
                                div()
                                    .id(format!("turn-error-details-{}", turn.id))
                                    .text_xs()
                                    .line_clamp(3)
                                    .text_color(rgb(appearance::MUTED))
                                    .tooltip({
                                        let details = details.clone();
                                        move |window, cx| {
                                            tooltip::Tooltip::new(details.clone()).build(window, cx)
                                        }
                                    })
                                    .child(details),
                            )
                        })
                        .when(error.is_usage_limit, |card| {
                            card.child(
                                h_flex()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(
                                        button(
                                            format!("usage-settings-{}", turn.id),
                                            language.text("使用量・リセット時刻"),
                                            cx,
                                            move |_, _, cx| {
                                                cx.emit(ConversationAction::Usage(provider));
                                            },
                                        )
                                        .small(),
                                    )
                                    .when(
                                        provider
                                            == Some(agent_protocol::session::ProviderKind::Codex),
                                        |actions| {
                                            actions.child(
                                                button(
                                                    format!("usage-manage-{}", turn.id),
                                                    language.text("追加クレジット・上限を管理"),
                                                    cx,
                                                    |_, _, cx| {
                                                        cx.emit(ConversationAction::OpenUsage);
                                                    },
                                                )
                                                .small(),
                                            )
                                        },
                                    ),
                            )
                        }),
                );
            }
            ConversationRowContent::Response {
                item,
                show_actions,
                fork_turn_id,
            } => {
                body = body.child(self.projected_item(item, turn, cx));
                if *show_actions {
                    let text = item.data.body.clone().expect("assistant message has text");
                    body = body.child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new(format!("copy-{}", item.data.id))
                                    .icon(IconName::Copy)
                                    .small()
                                    .ghost()
                                    .debug_selector(|| "response-copy".into())
                                    .tooltip(language.text("回答をコピー"))
                                    .accessibility_label(language.text("回答をコピー"))
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.clone(),
                                        ));
                                    }),
                            )
                            .when_some(
                                session.cloned().zip(fork_turn_id.clone()),
                                |actions, (thread_id, turn_id)| {
                                    let fork_error = self
                                        .model
                                        .failures
                                        .get(&op::OperationKey::Fork {
                                            session: thread_id.clone(),
                                            turn: turn_id.clone(),
                                        })
                                        .cloned();
                                    let forking =
                                        self.model.running.contains(&op::OperationKey::Fork {
                                            session: thread_id.clone(),
                                            turn: turn_id.clone(),
                                        });
                                    actions
                                        .when_some(fork_error, |row, error| {
                                            row.child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(0xff8e86))
                                                    .child(error_view(&error, language)),
                                            )
                                        })
                                        .child(
                                            Button::new(format!("fork-{}", item.data.id))
                                                .small()
                                                .ghost()
                                                .when(forking, |button| {
                                                    button
                                                        .child(crate::activity::indicator())
                                                        .size_6()
                                                        .px_0()
                                                })
                                                .tooltip(if forking {
                                                    language.text("分岐を作成中…")
                                                } else {
                                                    language.text("ここから会話を分岐")
                                                })
                                                .accessibility_label(if forking {
                                                    language.text("分岐を作成中…")
                                                } else {
                                                    language.text("ここから会話を分岐")
                                                })
                                                .on_click(cx.listener(move |_, _, _, cx| {
                                                    cx.emit(ConversationAction::Fork(
                                                        thread_id.clone(),
                                                        turn_id.clone(),
                                                    ));
                                                    cx.notify();
                                                }))
                                                .when(!forking, |button| {
                                                    button.icon(
                                                        Icon::default().path("icons/branch.svg"),
                                                    )
                                                })
                                                .debug_selector(|| "response-fork".into())
                                                .disabled(
                                                    !self.model.connected
                                                        || self.model.busy
                                                        || forking,
                                                ),
                                        )
                                        .when(forking, |actions| {
                                            actions.child(
                                                h_flex()
                                                    .gap_2()
                                                    .text_xs()
                                                    .text_color(rgb(appearance::MUTED))
                                                    .child(language.text("分岐を作成中…"))
                                                    .debug_selector(|| {
                                                        "response-fork-progress".into()
                                                    }),
                                            )
                                        })
                                },
                            )
                            .children(
                                row.timestamp_ms
                                    .and_then(|time| {
                                        i64::try_from(time)
                                            .ok()
                                            .and_then(chrono::DateTime::from_timestamp_millis)
                                    })
                                    .map(|time| {
                                        div().text_xs().text_color(rgb(appearance::MUTED)).child(
                                            time.with_timezone(&chrono::Local)
                                                .format("%H:%M")
                                                .to_string(),
                                        )
                                    }),
                            ),
                    );
                }
            }
            ConversationRowContent::InProgress { .. } => {
                body = body.child(
                    h_flex()
                        .debug_selector(|| "conversation-in-progress".into())
                        .gap_2()
                        .text_sm()
                        .text_color(rgb(appearance::MUTED))
                        .child(crate::activity::indicator())
                        .child(WorkProgressView::view(
                            &row.id,
                            projected.work_progress(false),
                            window,
                            cx,
                        )),
                );
            }
            ConversationRowContent::Preparation { preparation } => {
                body = body.child(self.preparation_card(preparation, cx));
            }
        }
        let selector = format!("conversation-row-{}", row.id);
        h_flex()
            .debug_selector(move || selector.clone())
            .justify_center()
            .w_full()
            .px_6()
            .pb(px(if last { 32. } else { 16. }))
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
            agent_core::presentation::conversation::ItemSource::Pending(_, pending) => {
                self.pending_item(&item.data, &pending.draft, cx)
            }
        }
    }
    fn preparation_card(
        &self,
        preparation: &agent_core::presentation::conversation::PreparationPresentation,
        cx: &Context<Self>,
    ) -> AnyElement {
        let language = crate::localization::language(cx);
        let text = agent_core::localization::localized_ui_text(language, preparation.text.clone());
        if preparation.in_progress {
            return h_flex()
                .debug_selector(|| "task-preparation".into())
                .gap_2()
                .text_sm()
                .text_color(rgb(appearance::MUTED))
                .child(crate::activity::indicator())
                .child(text)
                .into_any_element();
        }
        let mut body = v_flex()
            .gap_2()
            .debug_selector(|| "task-preparation".into())
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(appearance::MUTED))
                    .child(text),
            );
        if let Some(retry) = &preparation.retry {
            let retry = retry.clone();
            body = body.child(button(
                "retry-preparation",
                language.text("再試行"),
                cx,
                move |view, _, _| {
                    view.dispatch(Intent::RetryPreparation(retry.clone()));
                },
            ));
        }
        body.into_any_element()
    }
    pub(super) fn pending_item(
        &mut self,
        data: &agent_core::presentation::conversation::ItemPresentation,
        draft: &agent_core::state::Draft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language = crate::localization::language(cx);
        let id = &data.id;
        let mut body = user_message_bubble();
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
            if !attachment.is_image {
                let path = attachment.path.clone();
                body = body.child(button(
                    format!("pending-{id}-file-{index}"),
                    attachment.name.clone(),
                    cx,
                    move |_, _, cx| cx.emit(ConversationAction::Download(path.clone())),
                ));
            }
        }
        let body = self.user_message_content(
            id,
            body,
            !draft.text.is_empty() || draft.attachments.iter().any(|file| !file.is_image),
            draft
                .attachments
                .iter()
                .filter(|file| file.is_image)
                .map(|file| file.path.as_str()),
            cx,
        );
        let text = data.body.as_deref().unwrap_or_default();
        let copied = text.to_owned();
        let edit = button(format!("edit-{id}"), "", cx, move |_, _, cx| {
            cx.emit(ConversationAction::Edit(copied.clone()));
        });
        let timestamp = data
            .timestamp_ms
            .and_then(|time| i64::try_from(time / 1000).ok());
        v_flex()
            .w_full()
            .min_w_0()
            .items_end()
            .gap_2()
            .child(user_message_row(id, text, body, timestamp, edit, language))
            .when_some(data.title.as_deref(), |row, label| {
                row.child(div().text_sm().text_color(rgb(appearance::MUTED)).child(
                    agent_core::localization::localized_ui_text(language, label.into()),
                ))
            })
            .into_any_element()
    }

    fn user_message_content<'a>(
        &mut self,
        id: &str,
        body: Div,
        has_body: bool,
        sources: impl Iterator<Item = &'a str>,
        cx: &mut Context<Self>,
    ) -> Div {
        let images: Vec<_> = sources
            .enumerate()
            .map(|(index, source)| {
                div()
                    .w(px(80.))
                    .h(px(80.))
                    .flex_shrink_0()
                    .rounded_lg()
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgb(appearance::BORDER))
                    .debug_selector(|| "user-image-thumbnail".into())
                    .child(Media::preview(
                        &self.media,
                        format!("{id}-image-{index}"),
                        source,
                        false,
                        80.,
                        true,
                        cx,
                    ))
            })
            .collect();
        v_flex()
            .w_full()
            .min_w_0()
            .max_w(px(560.))
            .items_end()
            .gap_2()
            .when(!images.is_empty(), |column| {
                column.child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .flex_wrap()
                        .max_w_full()
                        .children(images),
                )
            })
            .when(has_body, |column| column.child(body))
    }

    fn request_card(
        &mut self,
        request: &agent_core::presentation::conversation::Request,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language = crate::localization::language(cx);
        use agent_protocol::requests::{Answer, ElicitationAnswer, ElicitationInput, RequestBody};
        let key = &request.id;
        let Some(inputs) = self.timeline.requests.get(key) else {
            return div().into_any_element();
        };
        let mut body = v_flex()
            .w_full()
            .max_w(px(CHAT_WIDTH))
            .gap_3()
            .p_4()
            .rounded(px(20.))
            .bg(rgb(appearance::SURFACE))
            .border_1()
            .border_color(rgb(appearance::BORDER))
            .child(div().text_sm().text_color(rgb(appearance::MUTED)).child(
                agent_core::localization::localized_ui_text(language, request.title.clone()),
            ))
            .child(request.body.clone());
        if let Some(error) = self
            .model
            .failures
            .get(&op::OperationKey::Request { id: key.clone() })
        {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(rgb(0xff8e86))
                    .child(error_view(error, language)),
            );
        }
        if !request.details.is_empty() {
            body = body.child(
                TextView::markdown(
                    SharedString::from(format!("request-{key}")),
                    fenced(&request.details, "json"),
                )
                .selectable(true),
            );
        }
        if inputs.sent || !request.can_respond {
            return body.into_any_element();
        }
        match &request.request_body {
            // The composer answers questions from its docked panel.
            RequestBody::Question { .. } => {}
            RequestBody::Approval { choices, .. } | RequestBody::Permission { choices, .. } => {
                let permission = matches!(request.request_body, RequestBody::Permission { .. });
                let mut row = h_flex().gap_2().flex_wrap();
                for choice in choices {
                    let key = key.to_owned();
                    let choice_id = choice.id.clone();
                    let tone = agent_core::presentation::conversation::choice_tone(choice.effect);
                    row = row.child(ranked(
                        button(
                            format!("decision-{key}-{}", choice.id),
                            choice.label.clone(),
                            cx,
                            move |_, _, cx| {
                                cx.emit(ConversationAction::Respond(
                                    key.clone(),
                                    if permission {
                                        Answer::Permission {
                                            choice_id: choice_id.clone(),
                                        }
                                    } else {
                                        Answer::Approval {
                                            choice_id: choice_id.clone(),
                                        }
                                    },
                                ))
                            },
                        ),
                        tone,
                        cx,
                    ));
                    if !choice.description.is_empty() {
                        body = body.child(choice.description.clone());
                    }
                }
                body = body.child(row);
            }
            RequestBody::Elicitation { input, .. } => {
                if let ElicitationInput::Url { url } = input {
                    let url = url.clone();
                    body = body.child(button(
                        format!("elicitation-url-{key}"),
                        language.text("リンクを開く"),
                        cx,
                        move |_, window, cx| {
                            cx.emit(ConversationAction::OpenLink(
                                url.clone(),
                                window.modifiers().control,
                            ))
                        },
                    ));
                } else {
                    body = body.child(Textarea::new(&inputs.response));
                }
                let accept_key = key.to_owned();
                let request_body = request.request_body.clone();
                let mut row = h_flex().gap_2().flex_wrap().child(ranked(
                    button(
                        format!("elicitation-accept-{accept_key}"),
                        language.text("確認して送信"),
                        cx,
                        move |view, _, cx| {
                            let Some(inputs) = view.timeline.requests.get(&accept_key) else {
                                return;
                            };
                            match agent_core::presentation::conversation::answer_from_json(
                                &request_body,
                                &inputs.response.read(cx).value(),
                            ) {
                                Ok(answer) => {
                                    cx.emit(ConversationAction::Respond(accept_key.clone(), answer))
                                }
                                Err(error) => cx.emit(ConversationAction::Error(error)),
                            }
                        },
                    ),
                    ActionTone::Primary,
                    cx,
                ));
                for (action, label, tone) in [
                    (
                        ElicitationAnswer::Decline,
                        language.text("辞退"),
                        ActionTone::Danger,
                    ),
                    (
                        ElicitationAnswer::Cancel,
                        language.text("キャンセル"),
                        ActionTone::Secondary,
                    ),
                ] {
                    let key = key.to_owned();
                    row = row.child(ranked(
                        button(
                            format!("elicitation-{label}-{key}"),
                            label,
                            cx,
                            move |_, _, cx| {
                                cx.emit(ConversationAction::Respond(
                                    key.clone(),
                                    Answer::Elicitation {
                                        action: action.clone(),
                                    },
                                ))
                            },
                        ),
                        tone,
                        cx,
                    ));
                }
                body = body.child(row);
            }
            RequestBody::ToolExecution { .. } => {
                let key = key.to_owned();
                let request_body = request.request_body.clone();
                body = body.child(Textarea::new(&inputs.response)).child(ranked(
                    button(
                        format!("tool-answer-{key}"),
                        language.text("実行結果を送信"),
                        cx,
                        move |view, _, cx| {
                            let Some(inputs) = view.timeline.requests.get(&key) else {
                                return;
                            };
                            match agent_core::presentation::conversation::answer_from_json(
                                &request_body,
                                &inputs.response.read(cx).value(),
                            ) {
                                Ok(answer) => {
                                    cx.emit(ConversationAction::Respond(key.clone(), answer))
                                }
                                Err(error) => cx.emit(ConversationAction::Error(error)),
                            }
                        },
                    ),
                    ActionTone::Primary,
                    cx,
                ));
            }
        }
        body.into_any_element()
    }
    /// Requests wait above the composer, as in T3 Code.
    fn docked_requests(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let docked = self.model.docked.clone();
        (!docked.is_empty()).then(|| {
            // Long details scroll instead of hiding the conversation.
            v_flex()
                .id("docked-requests")
                .w_full()
                .max_h(relative(0.5))
                .overflow_y_scroll()
                .items_center()
                .gap_3()
                .px_6()
                .pt_2()
                .children(docked.iter().map(|request| self.request_card(request, cx)))
                .into_any_element()
        })
    }
    pub(in crate::app) fn sync_request_inputs(
        &mut self,
        requests: &[Arc<agent_protocol::requests::Request>],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.timeline
            .requests
            .retain(|key, _| requests.iter().any(|request| &request.id == key));
        for request in requests {
            let key = &request.id;
            // The composer's answer field answers questions.
            if self.timeline.requests.contains_key(key)
                || matches!(
                    request.body,
                    agent_protocol::requests::RequestBody::Question { .. }
                )
            {
                continue;
            }
            self.timeline.requests.insert(
                key.clone(),
                RequestInputs {
                    response: cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .default_value(
                                agent_core::presentation::conversation::request_input_default(
                                    request.body.clone(),
                                ),
                            )
                            .auto_grow(3, 8)
                    }),
                    sent: false,
                },
            );
        }
    }
}

/// Request actions in the tone the core ranks them, with T3 Code's danger tint.
fn ranked(button: Button, tone: ActionTone, cx: &App) -> Button {
    use gpui_kit::component::button::{ButtonCustomVariant, ButtonVariant};
    match tone {
        ActionTone::Primary => button.primary(),
        ActionTone::Secondary => button.with_variant(ButtonVariant::Secondary),
        ActionTone::Danger => button.custom(
            ButtonCustomVariant::new(cx)
                .color(rgb(0x301214).into())
                .foreground(rgb(0xff6467).into())
                .hover(rgb(0x3d171a).into())
                .active(rgb(0x4a1c20).into()),
        ),
    }
}

fn user_message_row(
    id: &str,
    text: &str,
    body: Div,
    timestamp: Option<i64>,
    edit: Button,
    language: agent_core::localization::UiLanguage,
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
    v_flex()
        .w_full()
        .min_w_0()
        .group(group.clone())
        .items_end()
        .gap_1()
        .my_4()
        .child(body)
        .child(
            h_flex()
                .debug_selector(|| "user-message-actions".into())
                .invisible()
                .group_hover(group, |style| style.visible())
                .children(time.map(|time| {
                    div()
                        .text_xs()
                        .text_color(rgb(appearance::MUTED))
                        .mr_2()
                        .child(time)
                }))
                .child(
                    Button::new(format!("copy-{id}"))
                        .debug_selector(|| "user-message-copy".into())
                        .icon(IconName::Copy)
                        .small()
                        .ghost()
                        .tooltip(language.text("発言をコピー"))
                        .accessibility_label(language.text("発言をコピー"))
                        .disabled(copy_text.is_empty())
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_text.clone()));
                        }),
                )
                .child(
                    edit.icon(Icon::default().path("icons/pencil.svg"))
                        .small()
                        .ghost()
                        .debug_selector(|| "user-message-edit".into())
                        .tooltip(language.text("入力欄で編集"))
                        .accessibility_label(language.text("入力欄で編集"))
                        .disabled(text.is_empty()),
                ),
        )
}

impl ConversationView {
    fn history_control(
        &self,
        control: agent_core::presentation::conversation::HistoryControl,
        cx: &Context<Self>,
    ) -> AnyElement {
        let language = crate::localization::language(cx);
        let label: SharedString = if control.has_more {
            agent_core::localization::localized_ui_text(language, control.load_label).into()
        } else {
            language.text("再試行").into()
        };
        h_flex()
            .w_full()
            .justify_center()
            .px_6()
            .py_2()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(CHAT_WIDTH))
                    .gap_1p5()
                    .when(control.has_more || control.error.is_some(), |body| {
                        body.child(
                            Button::new("conversation-history-load")
                                .debug_selector(|| "conversation-history-load".into())
                                .accessibility_label(label.clone())
                                .small()
                                .ghost()
                                .w_full()
                                .disabled(control.loading)
                                .child(
                                    h_flex()
                                        .w_full()
                                        .gap_3()
                                        .child(div().flex_1().h(px(1.)).bg(rgb(appearance::BORDER)))
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(rgb(appearance::MUTED))
                                                .child(label),
                                        )
                                        .child(
                                            div().flex_1().h(px(1.)).bg(rgb(appearance::BORDER)),
                                        ),
                                )
                                .on_click(cx.listener(|view, _, _, cx| {
                                    if let Some(thread_id) = view.model.session.as_ref().cloned() {
                                        view.timeline.pause_tail();
                                        view.dispatch(Intent::ReadOlder { thread_id });
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .when_some(control.error, |body, error| {
                        body.child(
                            div()
                                .debug_selector(|| "conversation-history-error".into())
                                .text_xs()
                                .text_center()
                                .text_color(rgb(appearance::MUTED))
                                .child(error_view(&error, language)),
                        )
                    }),
            )
            .into_any_element()
    }
    pub(super) fn conversation_list(&self, cx: &Context<Self>) -> List {
        let entity = cx.entity().downgrade();
        list(self.timeline.list.clone(), move |index, window, cx| {
            entity
                .update(cx, |view, cx| {
                    match view.timeline.rows.get(index).cloned() {
                        Some(ConversationRow::History(control)) => {
                            view.history_control(control, cx)
                        }
                        Some(ConversationRow::Turn { turn, index, last }) => {
                            let session = view
                                .timeline
                                .rendered
                                .as_ref()
                                .and_then(|conversation| conversation.source.id.clone());
                            view.turn_row(session.as_ref(), &turn, index, last, window, cx)
                        }
                        Some(ConversationRow::Queued(item)) => {
                            let row = match &item.source {
                                agent_core::presentation::conversation::ItemSource::Native(
                                    native,
                                ) => view.item(&item, native, None, cx),
                                agent_core::presentation::conversation::ItemSource::Pending(
                                    _,
                                    pending,
                                ) => view.pending_item(&item.data, &pending.draft, cx),
                            };
                            h_flex()
                                .justify_center()
                                .w_full()
                                .px_6()
                                .pb_8()
                                .child(v_flex().w_full().max_w(px(CHAT_WIDTH)).gap_4().child(row))
                                .into_any_element()
                        }
                        Some(ConversationRow::Preparation(preparation)) => h_flex()
                            .justify_center()
                            .w_full()
                            .px_6()
                            .pb_8()
                            .child(
                                v_flex()
                                    .w_full()
                                    .max_w(px(CHAT_WIDTH))
                                    .child(view.preparation_card(&preparation, cx)),
                            )
                            .into_any_element(),
                        None => div().into_any_element(),
                    }
                })
                .unwrap_or_else(|_| div().into_any_element())
        })
        .flex_1()
        .min_h_0()
    }
    fn viewport(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let language = crate::localization::language(cx);
        let history = self.conversation_list(cx);
        let mut body = v_flex()
            .debug_selector(|| "conversation-viewport".into())
            .w_full()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(div().w_full().max_w(px(CHAT_WIDTH)).mx_auto().px_4().child(
                errors::error_list(
                    &self.model.errors,
                    ErrorScope::Conversation {
                        draft_key: self.model.draft_key.clone(),
                    },
                    None,
                    cx,
                    |_, action, _, cx| cx.emit(ConversationAction::Notice(action)),
                ),
            ));
        if let Some(notice) = &self.model.history_notice {
            body = body.child(div().px_4().py_2().text_sm().child(
                agent_core::localization::localized_ui_text(language, notice.clone()),
            ));
        }
        if self.timeline.rendered.is_none() {
            if let Some(control) = self.model.history.clone() {
                body = body.child(self.history_control(control, cx));
            }
            body = body.child(
                v_flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(
                        div()
                            .text_2xl()
                            .child(language.text("何から始めましょうか？")),
                    )
                    .child(if self.model.cwd.is_empty() {
                        language
                            .text("メッセージを入力して、作業を始めましょう。")
                            .into()
                    } else {
                        file_name(&self.model.cwd)
                    }),
            );
        } else {
            let mut viewport = div()
                .relative()
                .pl_4()
                .flex_1()
                .min_h_0()
                .flex()
                .child(selection::ConversationSelection::wrap(
                    &self.selection,
                    history,
                ))
                .child(deferred(self.conversation_navigation(cx)));
            if !self.timeline.list.is_following_tail() {
                viewport = viewport.child(deferred(
                    h_flex()
                        .absolute()
                        .bottom_2()
                        .left_0()
                        .w_full()
                        .justify_center()
                        .child(
                            icon_button(
                                "conversation-latest",
                                IconName::ArrowDown,
                                language.text("最新のメッセージへ"),
                                cx,
                                |view, _, _| {
                                    view.timeline.list.set_follow_mode(FollowMode::Tail);
                                },
                            )
                            .debug_selector(|| "conversation-latest".into())
                            .rounded_full()
                            .bg(rgb(appearance::RAISED))
                            .size(px(36.)),
                        ),
                ));
            }
            body = body.child(viewport);
        }
        body.children(self.docked_requests(cx)).into_any_element()
    }
}
impl Render for ConversationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.renders += 1;
        }
        self.viewport(cx)
    }
}

impl ConversationView {
    pub(super) fn markdown(
        &mut self,
        id: String,
        source: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let language = crate::localization::language(cx);
        if self
            .timeline
            .markdown
            .get(&id)
            .is_none_or(|cached| cached.source != source)
        {
            let presentation = agent_core::presentation::markdown::markdown_presentation(
                source,
                &self.model.cwd,
                self.model.home.as_deref(),
            );
            let Some(desktop) = self.desktop.upgrade() else {
                return div().into_any_element();
            };
            let previous = self.timeline.markdown.get(&id);
            let same_files =
                previous.is_some_and(|cached| *cached.file_chips.files == presentation.files);
            let file_chips = previous
                .filter(|_| same_files)
                .map(|cached| cached.file_chips.clone())
                .unwrap_or_else(|| {
                    super::markdown_file::FileChips::new(
                        presentation.files.into(),
                        &desktop,
                        self.model.epoch,
                        &self.metadata,
                        cx,
                    )
                });
            // A new parser revision replaces the entire parsed document and its
            // measured blocks. Ordinary streaming must keep the incremental parser.
            let parser_revision = previous
                .filter(|cached| same_files && *cached.definitions == presentation.definitions)
                .map_or_else(
                    || uuid::Uuid::new_v4().as_u64_pair().0,
                    |cached| cached.parser_revision,
                );
            let source: SharedString = source.to_owned().into();
            let rendered = match presentation.source {
                std::borrow::Cow::Borrowed(_) => source.clone(),
                std::borrow::Cow::Owned(rendered) => rendered.into(),
            };
            let (wrap_code, compact_tables) = previous.map_or((false, false), |cached| {
                (cached.wrap_code, cached.compact_tables)
            });
            self.timeline.markdown.insert(
                id.clone(),
                MarkdownContent {
                    source,
                    rendered,
                    definitions: Arc::new(presentation.definitions),
                    code_headers: Arc::new(presentation.code_headers),
                    file_chips,
                    parser_revision,
                    diagrams: HashMap::new(),
                    wrap_code,
                    compact_tables,
                },
            );
        }
        let cached = &self.timeline.markdown[&id];
        let definitions = cached.definitions.clone();
        let view = cx.entity().downgrade();
        let wrap_code = cached.wrap_code;
        let compact_tables = cached.compact_tables;
        let headers = cached.code_headers.clone();
        let wrap_view = view.clone();
        let table_view = view.clone();
        let code_key = id.clone();
        let table_key = id.clone();
        let diagram_key = id.clone();
        let diagram_view = view.clone();
        let image_key = id.clone();
        let media = self.media.clone();
        let mut code_style = gpui_kit::StyleRefinement::default()
            .pt(px(44.))
            .border_1()
            .border_color(cx.theme().border);
        code_style.overflow.x = Some(gpui_kit::Overflow::Scroll);
        code_style.text.white_space = Some(if wrap_code {
            gpui_kit::WhiteSpace::Normal
        } else {
            gpui_kit::WhiteSpace::Nowrap
        });
        let mut table_cell = gpui_kit::StyleRefinement::default();
        table_cell.text.white_space = Some(if compact_tables {
            gpui_kit::WhiteSpace::Nowrap
        } else {
            gpui_kit::WhiteSpace::Normal
        });
        let mut table_style = gpui_kit::StyleRefinement::default();
        table_style.overflow.x = Some(gpui_kit::Overflow::Scroll);
        let style = gpui_kit::component::text::TextViewStyle::default()
            .code_block(code_style)
            .table(table_style)
            .table_cell(table_cell);
        v_flex()
            .w_full()
            .child(
                TextView::markdown(SharedString::from(id.clone()), cached.rendered.clone())
                    .selectable(true)
                    .markdown_extensions(
                        gpui_kit::base::text::MarkdownExtensions::default()
                            .parser_revision(cached.parser_revision),
                    )
                    .plugin(cached.file_chips.clone())
                    .markdown_block_parser(move |node, context| {
                        let images = agent_core::presentation::markdown::markdown_image_paragraph(
                            node,
                            &definitions,
                        )?;
                        Some(
                            gpui_kit::base::text::MarkdownNode::new("images", images)
                                .markdown(context.node_source(node)?.to_owned()),
                        )
                    })
                    .markdown_block_renderer("images", move |node, _, cx| {
                        let start = node.source_range().map_or(0, |range| range.start);
                        let mut images = v_flex().gap_3().w_full();
                        for (index, source) in
                            node.data::<Vec<String>>().into_iter().flatten().enumerate()
                        {
                            images = images.child(Media::preview(
                                &media,
                                format!("{image_key}-markdown-image-{start}-{index}"),
                                source,
                                false,
                                320.,
                                true,
                                cx,
                            ));
                        }
                        images.into_any_element()
                    })
                    .markdown_block_parser(|node, context| {
                        let original = context.node_source(node)?;
                        let diagram =
                            agent_core::presentation::markdown::markdown_diagram(original)?;
                        Some(
                            gpui_kit::base::text::MarkdownNode::new("mermaid", diagram.clone())
                                .text(diagram.source)
                                .markdown(original.to_owned()),
                        )
                    })
                    .markdown_block_renderer("mermaid", move |node, _, cx| {
                        diagram_view
                            .update(cx, |view, cx| view.markdown_diagram(&diagram_key, node, cx))
                            .unwrap_or_else(|_| div().into_any_element())
                    })
                    .style(style)
                    .selection_format(gpui_kit::component::text::SelectionFormat::Source)
                    .code_block_actions(move |block, _, _| {
                        let text = block.code();
                        let label = block
                            .span
                            .and_then(|span| headers.get(&span.start))
                            .cloned()
                            .unwrap_or_else(|| {
                                block.lang().unwrap_or_else(|| "CODE".into()).to_string()
                            });
                        let wrap_view = wrap_view.clone();
                        let code_key = code_key.clone();
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                div()
                                    .min_w_0()
                                    .max_w(px(220.))
                                    .text_ellipsis()
                                    .text_xs()
                                    .child(label),
                            )
                            .child(
                                Button::new("wrap-code")
                                    .label(if wrap_code {
                                        language.text("折り返し解除")
                                    } else {
                                        language.text("折り返し")
                                    })
                                    .small()
                                    .ghost()
                                    .on_click(move |_, _, cx| {
                                        let _ = wrap_view.update(cx, |view, cx| {
                                            if let Some(cached) =
                                                view.timeline.markdown.get_mut(&code_key)
                                            {
                                                cached.wrap_code = !cached.wrap_code;
                                            }
                                            view.timeline.remeasure_item(&code_key);
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                Button::new("copy-code")
                                    .icon(IconName::Copy)
                                    .small()
                                    .ghost()
                                    .tooltip(language.text("コードをコピー"))
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.to_string(),
                                        ))
                                    }),
                            )
                    })
                    .table_actions(move |table, _, _| {
                        let table_view = table_view.clone();
                        let table_key = table_key.clone();
                        let markdown = table.markdown.clone();
                        let mut rows = vec![table.headers.clone()];
                        rows.extend(table.rows.clone());
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("wrap-table")
                                    .label(if compact_tables {
                                        language.text("セルを折り返す")
                                    } else {
                                        language.text("セルをコンパクトに表示")
                                    })
                                    .small()
                                    .ghost()
                                    .on_click(move |_, _, cx| {
                                        let _ = table_view.update(cx, |view, cx| {
                                            if let Some(cached) =
                                                view.timeline.markdown.get_mut(&table_key)
                                            {
                                                cached.compact_tables = !cached.compact_tables;
                                            }
                                            view.timeline.remeasure_item(&table_key);
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                Button::new("copy-markdown")
                                    .label(language.text("Markdownをコピー"))
                                    .small()
                                    .ghost()
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            markdown.clone(),
                                        ))
                                    }),
                            )
                            .child(
                                Button::new("copy-csv")
                                    .label(language.text("CSVをコピー"))
                                    .small()
                                    .ghost()
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            agent_core::presentation::markdown::markdown_table_csv(
                                                rows.clone(),
                                            ),
                                        ))
                                    }),
                            )
                    })
                    .on_link_click(move |url, event, window, cx| {
                        if let gpui_kit::ClickEvent::Mouse(click) = event
                            && click.up.button == MouseButton::Right
                        {
                            let _ = view.update(cx, |view, cx| {
                                view.selection.update(cx, |selection, cx| {
                                    selection.show_context_menu(
                                        Some(url.to_string()),
                                        click.up.position,
                                        window,
                                        cx,
                                    );
                                });
                            });
                            return;
                        }
                        let _ = view.update(cx, |_, cx| {
                            cx.emit(ConversationAction::OpenLink(
                                url.to_string(),
                                event.modifiers().control,
                            ))
                        });
                    }),
            )
            .into_any_element()
    }
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
                agent_core::localization::UiLanguage::Japanese,
            )
        }
    }

    #[gpui::test]
    fn hovering_own_message_allows_copying_the_complete_text(cx: &mut TestAppContext) {
        cx.update(crate::appearance::init);
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
mod rendering_tests {
    use super::{Desktop, Mode, Snapshot};
    use agent_core::state::operations as op;
    use gpui_kit as gpui;
    use gpui_kit::{
        AppContext, Context, Entity, IntoElement, ParentElement, Render, RenderImage, Styled,
        TestAppContext, Window, px, size,
    };
    use std::sync::Arc;

    struct ClockView {
        progress: super::WorkProgress,
        clock: Option<Entity<super::WorkProgressView>>,
    }

    impl Render for ClockView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let clock = super::WorkProgressView::view("fixture", self.progress.clone(), window, cx);
            self.clock = Some(clock.clone());
            clock
        }
    }

    struct ConversationView {
        desktop: Entity<Desktop>,
    }
    impl Render for ConversationView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.desktop.update(cx, |desktop, cx| {
                desktop.project_fixture();
                desktop.publish_views(window, cx);
                gpui_kit::div().size_full().flex().flex_col().child(
                    desktop
                        .conversation
                        .clone()
                        .cached(gpui_kit::StyleRefinement::default().size_full()),
                )
            })
        }
    }

    fn init(cx: &mut TestAppContext) -> tokio::runtime::Runtime {
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
        runtime
    }

    impl ConversationView {
        fn new(
            mut snapshot: Snapshot,
            source: agent_protocol::models::Thread,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) -> Self {
            Arc::make_mut(&mut Arc::make_mut(&mut snapshot.pane).navigation).thread_id =
                source.id.clone();
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .insert(source.id.clone().unwrap(), Arc::new(source));
            let desktop = cx.new(|cx| {
                let mut desktop = Desktop::new(
                    Mode::Fixture {
                        // Reject before reading preferences or connecting to a real Host.
                        remote: Some(agent_protocol::models::RemoteHost {
                            id: "fixture".into(),
                            name: "fixture".into(),
                            ticket: "invalid-fixture-ticket".into(),
                        }),
                        cwd: "/fixture".into(),
                    },
                    window,
                    cx,
                );
                desktop.snapshot = Arc::new(snapshot);
                desktop.project_fixture();
                desktop.publish_views(window, cx);
                desktop
            });
            Self { desktop }
        }
    }

    #[gpui::test]
    fn streaming_markdown_keeps_selected_text_and_grows_without_shrinking(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let mut text = "Alpha Bravo\n\nA streamed response.".to_owned();
        let thread = |text: &str| {
            serde_json::from_value(serde_json::json!({
                "id": {"provider":"codex", "id":"fixture"},
                "turns": [{"id":"turn", "status":"running", "items": [{
                    "id":"answer", "body":{"inline":{"body":{"assistantText":{
                        "text":text, "phase":"final"
                    }}}}
                }]}]
            }))
            .unwrap()
        };
        let mut desktop = None;
        let (_, window) = cx.add_window_view(|window, cx| {
            let view =
                cx.new(|cx| ConversationView::new(Snapshot::default(), thread(&text), window, cx));
            desktop = Some(view.read(cx).desktop.clone());
            gpui::component::Root::new(view, window, cx)
        });
        let desktop = desktop.unwrap();
        window.simulate_resize(size(px(600.), px(650.)));
        window.run_until_parked();
        let row = "conversation-row-history-item:turn:answer:occurrence:0:turn:turn:0";
        let bounds = window.debug_bounds(row).unwrap();
        let start = gpui::point(bounds.left() + px(25.), bounds.top() + px(12.));
        let end = start + gpui::point(px(200.), px(0.));
        window.simulate_mouse_move(start, None, gpui::Modifiers::default());
        window.simulate_mouse_down(start, gpui::MouseButton::Left, gpui::Modifiers::default());
        window.simulate_mouse_move(
            end,
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::default(),
        );
        window.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());
        window.run_until_parked();
        let selected = window.update(gpui::base::TextSelection::selected_text);
        assert_eq!(selected.trim(), "Alpha Bravo");
        let mut height = bounds.size.height;
        for suffix in [
            " More words.",
            "\n\nAnother paragraph.",
            "\n\n```rust\nfn main() {}\n```",
        ] {
            text.push_str(suffix);
            desktop.update(window, |desktop, cx| {
                let session = desktop.selected().unwrap().clone();
                Arc::make_mut(&mut Arc::make_mut(&mut desktop.snapshot).sessions.conversations)
                    .insert(session, Arc::new(thread(&text)));
                desktop.project_fixture();
                cx.notify();
            });
            for _ in 0..3 {
                window.update(|window, cx| window.draw(cx).clear(cx));
                let current = window.debug_bounds(row).unwrap().size.height;
                assert!(
                    current >= height,
                    "an append must not collapse the message: {height:?} -> {current:?}"
                );
                height = current;
                window.run_until_parked();
            }
            assert_eq!(
                window.update(gpui::base::TextSelection::selected_text),
                selected,
                "appending Markdown must retain the selected earlier paragraph"
            );
        }
        assert!(
            height > bounds.size.height,
            "new paragraphs must increase the message height"
        );
    }

    #[gpui::test]
    fn streaming_keeps_an_unfinished_request_answer(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let mut source: agent_protocol::models::Thread =
            serde_json::from_value(serde_json::json!({
                "id":{"provider":"codex","id":"fixture"}, "turns":[]
            }))
            .unwrap();
        source.requests.insert(
            "request".into(),
            Arc::new(agent_protocol::requests::Request {
                id: "request".into(),
                target: agent_protocol::requests::RequestTarget::Session,
                delivery: agent_protocol::session::RequestDelivery::Awaiting,
                body: agent_protocol::requests::RequestBody::Elicitation {
                    server: "fixture".into(),
                    message: "Answer".into(),
                    input: agent_protocol::requests::ElicitationInput::Form { fields: vec![] },
                },
            }),
        );
        let (view, window) = cx.add_window_view(|window, cx| {
            ConversationView::new(Snapshot::default(), source, window, cx)
        });
        window.run_until_parked();
        let desktop = view.read_with(window, |view, _| view.desktop.clone());
        window.update(|window, cx| {
            let input = desktop.read(cx).conversation.read(cx).timeline.requests["request"]
                .response
                .clone();
            input.update(cx, |input, cx| {
                input.set_value("{\"answer\":\"Keep my answer\"}", window, cx)
            });
        });
        desktop.update(window, |desktop, cx| {
            let session = desktop.selected().unwrap().clone();
            let snapshot = Arc::make_mut(&mut desktop.snapshot);
            let thread = Arc::make_mut(
                Arc::make_mut(&mut snapshot.sessions.conversations)
                    .get_mut(&session)
                    .unwrap(),
            );
            thread.name = Some("Updated while answering".into());
            cx.notify();
        });
        window.run_until_parked();
        desktop.read_with(window, |desktop, cx| {
            assert_eq!(
                desktop.conversation.read(cx).timeline.requests["request"]
                    .response
                    .read(cx)
                    .value()
                    .as_ref(),
                "{\"answer\":\"Keep my answer\"}"
            );
        });
    }

    #[gpui::test]
    fn markdown_file_chips_remain_native_click_targets_across_connection_epochs(
        cx: &mut TestAppContext,
    ) {
        let _runtime = init(cx);
        let source = serde_json::from_value(serde_json::json!({
            "id": {"provider":"codex", "id":"fixture"},
            "turns": [{"id":"turn", "status":"completed", "items": [{
                "id":"answer", "body":{"inline":{"body":{"assistantText":{
                    "text":"[**設計資料**](src/main.rs:12:3) and `src/other.rs`", "phase":"final"
                }}}}
            }]}]
        }))
        .unwrap();
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(&mut snapshot.pane).navigation).cwd = "/fixture".into();
        let (view, window) =
            cx.add_window_view(|window, cx| ConversationView::new(snapshot, source, window, cx));
        window.simulate_resize(size(px(380.), px(600.)));
        window.run_until_parked();
        let chip = window
            .debug_bounds("markdown-file:/fixture/src/main.rs")
            .expect("native file chip");
        assert!(chip.size.width > px(30.) && chip.size.width < px(380.));
        assert!(
            window
                .debug_bounds("markdown-file:/fixture/src/other.rs")
                .is_some()
        );
        let caption = window
            .debug_bounds("markdown-file-caption:/fixture/src/main.rs")
            .expect("the caption is inside the same atomic inline element");
        assert!(chip.contains(&caption.origin));
        assert!(caption.right() <= chip.right());
        assert!(caption.bottom() <= chip.bottom());
        assert!(
            window
                .debug_bounds("markdown-file-caption:/fixture/src/other.rs")
                .is_none(),
            "a path-only reference has no additional caption"
        );
        let desktop = view.read_with(window, |view, _| view.desktop.clone());
        let conversation = desktop.read_with(window, |desktop, _| desktop.conversation.clone());
        conversation.update(window, |view, cx| {
            assert!(
                view.timeline
                    .markdown
                    .values()
                    .all(|cached| { cached.source.as_ptr() == cached.rendered.as_ptr() })
            );
            let _ = view.markdown(
                "transformed-source-test".into(),
                "<summary>caption</summary>",
                cx,
            );
            let cached = view
                .timeline
                .markdown
                .remove("transformed-source-test")
                .unwrap();
            assert_eq!(cached.source.as_ref(), "<summary>caption</summary>");
            assert_eq!(cached.rendered.as_ref(), "caption");
        });
        let before = desktop.read_with(window, |view, _| view.panels.get(view.panel_scope()));
        desktop.update(window, |desktop, cx| {
            desktop.local_shell = true;
            cx.notify();
        });
        window.simulate_click(
            chip.center(),
            gpui_kit::Modifiers {
                platform: cfg!(target_os = "macos"),
                control: !cfg!(target_os = "macos"),
                ..Default::default()
            },
        );
        window.run_until_parked();
        assert_eq!(
            desktop.read_with(window, |view, _| view.panels.get(view.panel_scope())),
            before,
            "the local editor modifier bypasses the file panel"
        );
        let previous_parser = desktop.read_with(window, |desktop, cx| {
            desktop
                .conversation
                .read(cx)
                .timeline
                .markdown
                .values()
                .next()
                .unwrap()
                .parser_revision
        });
        desktop.update(window, |desktop, cx| {
            desktop.local_shell = false;
            // A Store reconnect advances the snapshot epoch without changing the
            // conversation or its shared rendered-body allocation.
            desktop.conversation.update(cx, |view, _| {
                for cached in view.timeline.markdown.values_mut() {
                    cached.wrap_code = true;
                }
            });
            Arc::make_mut(&mut desktop.snapshot).connection.epoch += 2;
            desktop.project_fixture();
            cx.notify();
        });
        window.run_until_parked();
        let current_parser = desktop.read_with(window, |desktop, cx| {
            desktop
                .conversation
                .read(cx)
                .timeline
                .markdown
                .values()
                .next()
                .unwrap()
                .parser_revision
        });
        assert_ne!(
            previous_parser, current_parser,
            "cached native handlers must use the current epoch"
        );
        desktop.read_with(window, |desktop, cx| {
            assert!(
                desktop
                    .conversation
                    .read(cx)
                    .timeline
                    .markdown
                    .values()
                    .all(|cached| cached.wrap_code)
            );
        });
        let chip = window
            .debug_bounds("markdown-file:/fixture/src/main.rs")
            .unwrap();
        window.simulate_click(chip.center(), gpui_kit::Modifiers::default());
        window.run_until_parked();
        desktop.read_with(window, |view, _| {
            let panel = view.panels.get(view.panel_scope());
            assert!(panel.open);
            assert_eq!(panel.kind(), agent_core::panels::PanelKind::File);
            assert_eq!(
                panel.selected().unwrap().path.as_deref(),
                Some("/fixture/src/main.rs")
            );
        });
    }

    #[gpui::test]
    fn elapsed_work_ticks_without_notifying_its_conversation(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc, time::Duration};
        let _runtime = init(cx);
        let source = serde_json::from_value(serde_json::json!({
            "turns":[{"id":"running", "status":"running", "startedAt":100.}]
        }))
        .unwrap();
        let conversation = agent_core::presentation::conversation::project_conversation(
            &Snapshot::default().pane.navigation.draft_key,
            &Snapshot::default().sessions.pending_submissions,
            Arc::new(source),
            &None,
        );
        let progress = conversation.turns[0].work_progress(false);
        let (view, window) = cx.add_window_view(|_, _| ClockView {
            progress,
            clock: None,
        });
        window.run_until_parked();
        let clock = view.read_with(window, |view, _| view.clock.clone().unwrap());
        let parent_notifications = Rc::new(Cell::new(0));
        let ticks = Rc::new(Cell::new(0));
        let _subscriptions = window.update(|_, cx| {
            let parents = parent_notifications.clone();
            let ticks = ticks.clone();
            (
                cx.observe(&view, move |_, _| parents.set(parents.get() + 1)),
                cx.observe(&clock, move |_, _| ticks.set(ticks.get() + 1)),
            )
        });
        window.executor().advance_clock(Duration::from_secs(1));
        window.run_until_parked();
        assert!(ticks.get() > 0, "the visible elapsed label must update");
        assert_eq!(
            parent_notifications.get(),
            0,
            "a clock tick must not notify conversation observers"
        );
    }

    #[gpui::test]
    fn a_long_single_turn_only_mounts_messages_near_the_viewport(cx: &mut TestAppContext) {
        use std::{cell::Cell, rc::Rc, time::Duration};
        let _runtime = init(cx);
        let source = serde_json::from_value(serde_json::json!({
            "id": {"provider":"codex", "id":"fixture"},
            "turns": [{"id":"long-turn", "status":"running", "items":
                (0..1000).map(|index| serde_json::json!({
                    "id":format!("answer-{index}"),
                    "body":{"inline":{"body":{"assistantText":{
                        "text":format!("Update {index}\n\nA paragraph explaining the current work."),
                        "phase":"final"
                    }}}}
                })).collect::<Vec<_>>()
            }]
        })).unwrap();
        let (view, window) = cx.add_window_view(|window, cx| {
            ConversationView::new(Snapshot::default(), source, window, cx)
        });
        window.simulate_resize(size(px(900.), px(650.)));
        window.run_until_parked();
        view.update(window, |view, cx| {
            let desktop = view.desktop.read(cx);
            assert!(desktop.conversation.read(cx).timeline.rows.len() >= 1000);
            assert!(
                desktop
                    .conversation
                    .read(cx)
                    .timeline
                    .markdown
                    .contains_key("answer-999")
            );
            assert!(
                !desktop
                    .conversation
                    .read(cx)
                    .timeline
                    .markdown
                    .contains_key("answer-0")
            );
            assert!(
                desktop.conversation.read(cx).timeline.markdown.len() < 40,
                "opening a turn must not parse its offscreen messages"
            );
        });
        let notifications = Rc::new(Cell::new(0));
        let _subscription = window.update(|_, cx| {
            let count = notifications.clone();
            let desktop = view.read(cx).desktop.clone();
            cx.observe(&desktop, move |_, _| count.set(count.get() + 1))
        });
        window.executor().advance_clock(Duration::from_secs(1));
        window.run_until_parked();
        assert_eq!(
            notifications.get(),
            0,
            "elapsed time must not notify Desktop observers"
        );
        view.update(window, |view, cx| {
            view.desktop.update(cx, |desktop, cx| {
                desktop.conversation.update(cx, |view, cx| {
                    view.timeline
                        .list
                        .scroll_to(gpui_kit::ListOffset::default());
                    cx.notify();
                });
                cx.notify();
            });
        });
        window.run_until_parked();
        view.update(window, |view, cx| {
            assert!(
                view.desktop
                    .read(cx)
                    .conversation
                    .read(cx)
                    .timeline
                    .markdown
                    .contains_key("answer-0")
            );
        });
        let first_message =
            "conversation-row-history-item:long-turn:answer-0:occurrence:0:turn:long%2Dturn:0";
        let before = window.debug_bounds(first_message).unwrap().top();
        view.update(window, |view, cx| {
            view.desktop.update(cx, |desktop, cx| {
                let session = desktop.selected().unwrap().clone();
                let snapshot = Arc::make_mut(&mut desktop.snapshot);
                let thread = Arc::make_mut(
                    Arc::make_mut(&mut snapshot.sessions.conversations)
                        .get_mut(&session)
                        .unwrap(),
                );
                let items = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[0])
                    .items
                    .as_mut()
                    .unwrap();
                items.insert(
                    0,
                    Arc::new(
                        serde_json::from_value(serde_json::json!({
                            "id":"older-answer", "body":{"inline":{"body":{"assistantText":{
                                "text":"An earlier message from the same turn.", "phase":"final"
                            }}}}
                        }))
                        .unwrap(),
                    ),
                );
                items.push(Arc::new(
                    serde_json::from_value(serde_json::json!({
                        "id":"new-answer", "body":{"inline":{"body":{"assistantText":{
                            "text":"A live update arriving with the history page.", "phase":"final"
                        }}}}
                    }))
                    .unwrap(),
                ));

                desktop.project_fixture();
                cx.notify();
            })
        });
        window.run_until_parked();
        view.update(window, |view, cx| {
            let desktop = view.desktop.read(cx);
            assert!(
                !desktop
                    .conversation
                    .read(cx)
                    .timeline
                    .list
                    .is_following_tail()
            );
        });
        assert!(
            (window.debug_bounds(first_message).unwrap().top() - before).abs() < px(1.),
            "prepending history while the same turn streams must preserve the visible message"
        );
    }

    #[gpui::test]
    fn fork_progress_is_visible_only_at_the_pending_source(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        for (session, turn, phase, visible) in [
            ("fixture", "turn", op::OperationPhase::Running, true),
            ("other", "turn", op::OperationPhase::Running, false),
            ("fixture", "other", op::OperationPhase::Running, false),
            (
                "fixture",
                "turn",
                op::OperationPhase::Failed {
                    message: "failed".into(),
                },
                false,
            ),
        ] {
            let source = serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"fixture"},"capabilities":{"additionalInput":true,"fork":true,"rename":true,"modelChange":true},"turns":[{"id":"turn","status":"completed","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"Answer","phase":"final"}}}}}]}]})).unwrap();
            let (_, window) = cx.add_window_view(|window, cx| {
                let view = ConversationView::new(Snapshot::default(), source, window, cx);
                view.desktop.update(cx, |desktop, _| {
                    Arc::make_mut(&mut desktop.snapshot).operations.insert(
                        op::OperationKey::Fork {
                            session: agent_protocol::session::SessionRef {
                                provider: agent_protocol::session::ProviderKind::Codex,
                                id: session.into(),
                            },
                            turn: turn.into(),
                        },
                        op::OperationState {
                            generation: 1,
                            phase,
                        },
                    );
                });
                view
            });
            window.run_until_parked();
            assert_eq!(
                window.debug_bounds("response-fork-progress").is_some(),
                visible
            );
            assert_eq!(
                window.debug_bounds("response-fork").unwrap().size,
                size(px(24.), px(24.)),
                "pending and idle fork buttons must keep the same icon-button dimensions"
            );
        }
    }

    #[gpui::test]
    fn completed_response_offers_fork_but_streaming_response_does_not(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        for status in ["completed", "running"] {
            let source = serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"fixture"},"capabilities":{"additionalInput":true,"fork":true,"rename":true,"modelChange":true},"turns":[{"id":"turn","status":status,"items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"Answer","phase":"final"}}}}}]}]}))
            .unwrap();
            let (_, window) = cx.add_window_view(|window, cx| {
                ConversationView::new(Snapshot::default(), source, window, cx)
            });
            window.run_until_parked();
            for selector in ["response-fork", "response-copy"] {
                assert_eq!(
                    window.debug_bounds(selector).is_some(),
                    status == "completed",
                    "{selector}/{status}"
                );
            }
        }
    }

    #[gpui::test]
    fn background_work_keeps_progress_below_the_response_until_the_turn_ends(
        cx: &mut TestAppContext,
    ) {
        let _runtime = init(cx);
        for provider in ["codex", "claude"] {
            for status in ["running", "completed", "failed", "interrupted"] {
                let source = serde_json::from_value(serde_json::json!({
                    "id":{"provider":provider,"id":"fixture"}, "turns":[{
                        "id":"turn", "status":status, "items":[
                            {"id":"command","status":"completed","body":{"inline":{"body":{
                                "commandExecution":{"command":"review","cwd":"/fixture","output":"launched","exitCode":null}
                            }}}},
                            {"id":"answer","status":"completed","body":{"inline":{"body":{
                                "assistantText":{"text":"Long answer\n\n".repeat(20)+"結果が出たら報告します。","phase":"final"}
                            }}}}
                        ]
                    }]
                })).unwrap();
                let (_, window) = cx.add_window_view(|window, cx| {
                    ConversationView::new(Snapshot::default(), source, window, cx)
                });
                window.run_until_parked();
                let progress = window.debug_bounds("conversation-in-progress");
                if status == "running" {
                    let progress = progress.expect("background work must remain visible");
                    let response = window
                        .debug_bounds(
                            "conversation-row-history-item:turn:answer:occurrence:0:turn:turn:0",
                        )
                        .expect("the response must remain visible while background work runs");
                    assert!(window.debug_bounds("response-copy").is_none());
                    assert!(progress.top() >= response.bottom());
                    assert!(progress.size.height > px(0.));
                } else {
                    assert!(
                        progress.is_none(),
                        "{provider}/{status} still shows progress"
                    );
                }
            }
        }
    }

    #[gpui::test]
    fn running_work_header_shows_progress_once(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let source = serde_json::from_value(serde_json::json!({
            "id":{"provider":"codex","id":"fixture"}, "turns":[{
                "id":"turn", "status":"running", "items":[
                    {"id":"command","status":"running","body":{"inline":{"body":{
                        "commandExecution":{"command":"sed -n 1,15p worktrees.rs","cwd":"/fixture","output":"","exitCode":null}
                    }}}}
                ]
            }]
        }))
        .unwrap();
        let (_, window) = cx.add_window_view(|window, cx| {
            ConversationView::new(Snapshot::default(), source, window, cx)
        });
        window.run_until_parked();
        assert!(
            window
                .debug_bounds("work-turn:occurrence:0:turn:turn:0")
                .is_some()
        );
        assert!(
            window.debug_bounds("conversation-in-progress").is_none(),
            "the in-progress header must not be followed by a second progress row"
        );
    }

    struct ChatNavigationView(Entity<Desktop>);
    impl Render for ChatNavigationView {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.0.update(cx, |desktop, _| desktop.chat())
        }
    }

    #[gpui::test]
    fn conversation_navigation_returns_to_latest_and_resumes_following(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let source = serde_json::from_value(serde_json::json!({
            "id": {"provider":"codex","id":"fixture"}, "turns": (0..20).map(|i| serde_json::json!({
                "id": format!("turn-{i}"), "status": "completed", "items": [
                    {"id": format!("user-{i}"), "body": {"inline":{"body":{"userMessage":{"text":format!("Question {i}"),"content":[]}}}}},
                    {"id": format!("answer-{i}"), "body":{"inline":{"body":{"assistantText":{"text":"A long answer\n".repeat(10),"phase":"final"}}}}}
                ]
            })).collect::<Vec<_>>()
        })).unwrap();
        let (view, window) = cx.add_window_view(|window, cx| {
            let fixture =
                cx.new(|cx| ConversationView::new(Snapshot::default(), source, window, cx));
            ChatNavigationView(fixture.read(cx).desktop.clone())
        });
        window.simulate_resize(size(px(900.), px(650.)));
        window.run_until_parked();
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        assert!(window.debug_bounds("conversation-marker-0").is_some());
        assert!(window.debug_bounds("conversation-marker-38").is_some());
        assert!(
            window.debug_bounds("conversation-marker-1").is_none(),
            "Long histories must have at most eight evenly spaced markers"
        );
        let first = window.debug_bounds("conversation-marker-0").unwrap();
        let marker_left = window
            .debug_bounds("conversation-marker-line-0")
            .unwrap()
            .left();
        assert_eq!(
            marker_left - first.left(),
            px(16.5),
            "Preserve the original resting marker position"
        );
        let marker_width = window
            .debug_bounds("conversation-marker-line-0")
            .unwrap()
            .size
            .width;
        window.simulate_mouse_move(first.center(), None, gpui_kit::Modifiers::default());
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        let hovered_width = window
            .debug_bounds("conversation-marker-line-0")
            .unwrap()
            .size
            .width;
        let neighbor_width = window
            .debug_bounds("conversation-marker-line-4")
            .unwrap()
            .size
            .width;
        let next_width = window
            .debug_bounds("conversation-marker-line-10")
            .unwrap()
            .size
            .width;
        for selector in [
            "conversation-marker-line-0",
            "conversation-marker-line-4",
            "conversation-marker-line-10",
        ] {
            assert_eq!(
                window.debug_bounds(selector).unwrap().left(),
                marker_left,
                "Hover must keep the left edge fixed and expand only to the right"
            );
        }
        assert!(hovered_width >= px(28.));
        assert!(
            hovered_width > neighbor_width
                && neighbor_width > next_width
                && next_width > marker_width
        );
        window.simulate_mouse_move(
            gpui_kit::point(px(800.), px(30.)),
            None,
            gpui_kit::Modifiers::default(),
        );
        window.run_until_parked();
        assert_eq!(
            window
                .debug_bounds("conversation-marker-line-4")
                .unwrap()
                .size
                .width,
            marker_width
        );
        window.simulate_click(first.center(), gpui_kit::Modifiers::default());
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        let latest = window.debug_bounds("conversation-latest").unwrap();
        view.update(window, |view, cx| {
            assert!(
                !view
                    .0
                    .read(cx)
                    .conversation
                    .read(cx)
                    .timeline
                    .list
                    .is_following_tail()
            );
            assert_eq!(
                view.0
                    .read(cx)
                    .conversation
                    .read(cx)
                    .timeline
                    .list
                    .logical_scroll_top()
                    .item_ix,
                0
            );
        });
        window.simulate_click(latest.center(), gpui_kit::Modifiers::default());
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        let last = window
            .debug_bounds(
                "conversation-row-history-item:turn-19:answer-19:occurrence:0:turn:turn%2D19:0",
            )
            .unwrap();
        view.update(window, |view, cx| {
            let desktop = view.0.read(cx);
            assert!(
                desktop
                    .conversation
                    .read(cx)
                    .timeline
                    .list
                    .is_following_tail()
            );
            assert!(
                (last.bottom()
                    - desktop
                        .conversation
                        .read(cx)
                        .timeline
                        .list
                        .viewport_bounds()
                        .bottom())
                .abs()
                    < px(1.)
            );
        });
        assert!(window.debug_bounds("conversation-latest").is_none());
    }

    #[gpui::test]
    fn file_changes_render_and_expand_in_the_desktop_view(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let source = serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"fixture"},"turns":[{"id":"turn","status":"completed","items":[{"id":"files","status":"completed","clientInputId":"client-files","body":{"inline":{"body":{"fileChange":{"changes":[{"path":"/fixture/a.txt","kind":{"update":{"movePath":null}},"diff":"@@ -1 +1 @@\n-old\n+new","proposal":null},{"path":"/fixture/b.txt","kind":"add","diff":"@@ -0,0 +1 @@\n+second","proposal":null}],"output":""}}}}}]}]}))
        .unwrap();
        let (view, window) = cx.add_window_view(|window, cx| {
            ConversationView::new(Snapshot::default(), source, window, cx)
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("change-files-0").is_none(),
            "completed work starts collapsed"
        );
        let group = window
            .debug_bounds("work-turn:occurrence:0:turn:turn:0")
            .unwrap();
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
        let before = window.debug_bounds("change-files-1").unwrap().top()
            - window.debug_bounds("change-files-0").unwrap().top();
        let bounds = window.debug_bounds("change-files-0").unwrap();
        window.simulate_click(
            gpui_kit::point(bounds.left() + px(20.), bounds.center().y),
            gpui_kit::Modifiers::default(),
        );
        view.update(window, |_, cx| {
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("change-files-1").unwrap().top()
                - window.debug_bounds("change-files-0").unwrap().top()
                > before,
            "expanded diff must occupy space in the rendered conversation"
        );
    }
    #[gpui::test]
    fn markdown_images_render_where_they_are_written(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(40, 40),
        )]));
        let (snapshot, source) = user_message_fixture(
            "![図](/fixture/image.png)\n\n一行目\n\n二行目\n\n三行目\n\n四行目",
            None,
            false,
        );
        let (view, window) = cx.add_window_view(|window, cx| {
            let view = ConversationView::new(snapshot, source, window, cx);
            view.desktop.update(cx, |desktop, cx| {
                desktop.publish_views(window, cx);
                desktop.media.update(cx, |media, cx| {
                    media.fixture_image("/fixture/image.png", image.into(), cx);
                });
            });
            view
        });
        window.simulate_resize(size(px(780.), px(900.)));
        window.run_until_parked();
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        let image = window
            .debug_bounds("chat-image")
            .expect("image must render");
        let bubble = window.debug_bounds("user-message-bubble").unwrap();
        // The image leads the message as written instead of trailing its text.
        assert!(
            image.top() < bubble.top() + px(40.),
            "{image:?} in {bubble:?}"
        );
        assert!(
            image.bottom() + px(80.) < bubble.bottom(),
            "{image:?} in {bubble:?}"
        );
    }

    #[gpui::test]
    fn streaming_a_reference_definition_resolves_the_earlier_image(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        let image = Arc::new(RenderImage::new(vec![image::Frame::new(
            image::RgbaImage::new(40, 40),
        )]));
        let source = "![diagram][art]\n\nA paragraph after the image.";
        let (snapshot, thread) = user_message_fixture(source, None, false);
        let (view, window) = cx.add_window_view(|window, cx| {
            let view = ConversationView::new(snapshot, thread, window, cx);
            view.desktop.update(cx, |desktop, cx| {
                desktop.publish_views(window, cx);
                desktop.media.update(cx, |media, cx| {
                    media.fixture_image("/fixture/image.png", image.into(), cx);
                });
            });
            view
        });
        window.simulate_resize(size(px(780.), px(900.)));
        window.run_until_parked();
        assert!(window.debug_bounds("chat-image").is_none());
        let desktop = view.read_with(window, |view, _| view.desktop.clone());
        let (_, source) = user_message_fixture(
            &format!("{source}\n\n[art]: /fixture/image.png"),
            None,
            false,
        );
        desktop.update(window, |desktop, cx| {
            let session = desktop.selected().unwrap().clone();
            Arc::make_mut(&mut Arc::make_mut(&mut desktop.snapshot).sessions.conversations)
                .insert(session, Arc::new(source));
            desktop.project_fixture();
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds("chat-image").is_some(),
            "a streamed definition must resolve its earlier image instead of only reparsing the tail"
        );
    }

    fn user_message_fixture(
        text: &str,
        image: Option<&str>,
        pending: bool,
    ) -> (Snapshot, agent_protocol::models::Thread) {
        let message = image.map_or_else(
            || serde_json::json!({"text":text,"content":[]}),
            |source| serde_json::json!({"text":null,"content":[{"text":{"text":text}},{"image":{"source":source}}]}),
        );
        let items = if pending {
            serde_json::json!([])
        } else {
            serde_json::json!([{"id":"user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":message}}}}])
        };
        let mut snapshot = Snapshot::default();
        if pending {
            Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
                "pending".into(),
                Arc::new(agent_core::state::PendingSubmission {
                    sequence: 0,
                    submitted_at_ms: None,
                    draft_key: agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "fixture".into(),
                    }
                    .into(),
                    draft: Arc::new(agent_core::state::Draft {
                        text: text.into(),
                        attachments: image
                            .map(|source| agent_core::state::Attachment {
                                path: source.into(),
                                name: "image.png".into(),
                                is_image: true,
                            })
                            .into_iter()
                            .collect(),
                        ..Default::default()
                    }),
                    turn_id: Some("turn".into()),
                    after_item_id: None,
                    accepted: true,
                    delivery_unknown: false,
                }),
            );
        }
        let source = serde_json::from_value(serde_json::json!({
            "id":{"provider":"codex","id":"fixture"},
            "turns":[{"id":"turn","status":"completed","items":items}]
        }))
        .unwrap();
        (snapshot, source)
    }

    fn user_bubble_bounds(
        text: &str,
        viewport_width: f32,
        cx: &mut TestAppContext,
    ) -> gpui::Bounds<gpui::Pixels> {
        let (snapshot, source) = user_message_fixture(text, None, false);
        let (view, window) =
            cx.add_window_view(|window, cx| ConversationView::new(snapshot, source, window, cx));
        window.simulate_resize(size(px(viewport_width), px(600.)));
        window.run_until_parked();
        view.update(window, |_, cx| cx.notify());
        window.run_until_parked();
        window.debug_bounds("user-message-bubble").unwrap()
    }

    fn assert_user_list_bodies_visible(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        for width in [780., 360.] {
            for (source, body) in [
                ("3. Hello world", "Hello world"),
                ("3. はい", "はい"),
                ("3. 長めの内容をここに書いた", "長めの内容をここに書いた"),
                ("- はい", "はい"),
                ("はい\n\n- はい", "はい"),
                ("- 親\n  - はい", "はい"),
                ("- [ ] はい", "はい"),
                ("- [x] はい", "はい"),
            ] {
                let plain = user_bubble_bounds(body, width, cx);
                let list = user_bubble_bounds(source, width, cx);
                assert!(
                    list.size.width > plain.size.width,
                    "list must include its marker and full text: {source:?}, {list:?}, {plain:?}"
                );
                assert!(list.left() >= px(0.) && list.right() <= px(width));
            }
        }
    }

    #[gpui::test]
    fn user_lists_keep_their_bodies_visible(cx: &mut TestAppContext) {
        assert_user_list_bodies_visible(cx);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn user_lists_keep_their_bodies_visible_with_native_fonts() {
        let mut cx = TestAppContext::build_with_text_system(
            gpui::TestDispatcher::new(0),
            None,
            Arc::new(gpui_apple::AppleTextSystem::new()),
        );
        assert_user_list_bodies_visible(&mut cx);
    }

    #[gpui::test]
    fn user_messages_keep_readable_widths(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        for pending in [false, true] {
            let long = "The development Host inherits `$SHELL` from its environment. \
                長い発言も会話の幅に合わせて折り返し、箇条書きの本文をすべて読めるようにする。";
            for text in ["おｋ".to_owned(), long.to_owned(), format!("- {long}")] {
                let short = text == "おｋ";
                let (snapshot, source) = user_message_fixture(&text, None, pending);
                let (view, window) = cx.add_window_view(|window, cx| {
                    ConversationView::new(snapshot, source, window, cx)
                });
                for viewport_width in [780_f32, 360.] {
                    window.simulate_resize(size(px(viewport_width), px(600.)));
                    window.run_until_parked();
                    view.update(window, |_, cx| cx.notify());
                    window.run_until_parked();
                    let bubble = window.debug_bounds("user-message-bubble").unwrap();
                    if short {
                        assert!(bubble.size.width < px(120.), "short message: {bubble:?}");
                    } else {
                        assert!(
                            bubble.size.width >= px((viewport_width - 48.).min(560.) * 0.9),
                            "long message must use the available width (pending={pending}): {bubble:?}"
                        );
                    }
                    assert!(
                        bubble.size.height < px(240.),
                        "message must wrap into readable lines: {bubble:?}"
                    );
                    assert!(
                        bubble.left() >= px(0.) && bubble.right() <= px(viewport_width),
                        "message overflows the conversation (pending={pending}, viewport={viewport_width}): {bubble:?}"
                    );
                }
            }
        }
    }

    #[gpui::test]
    fn chat_images_stay_inside_the_bubble_at_different_window_sizes(cx: &mut TestAppContext) {
        let _runtime = init(cx);
        for pending in [false, true] {
            for (width, height) in [(1156, 78), (78, 1156), (400, 400)] {
                let image = Arc::new(RenderImage::new(vec![image::Frame::new(
                    image::RgbaImage::new(width, height),
                )]));
                let (snapshot, source) =
                    user_message_fixture("caption", Some("/fixture/image.png"), pending);
                let (view, window) = cx.add_window_view(|window, cx| {
                    let view = ConversationView::new(snapshot, source, window, cx);
                    view.desktop.update(cx, |desktop, cx| {
                        desktop.publish_views(window, cx);
                        desktop.media.update(cx, |media, cx| {
                            media.fixture_image("/fixture/image.png", image.into(), cx);
                        });
                    });
                    view
                });
                for viewport_width in [780_f32, 360.] {
                    window.simulate_resize(size(px(viewport_width), px(600.)));
                    window.run_until_parked();
                    view.update(window, |_, cx| cx.notify());
                    window.run_until_parked();
                    let thumbnail = window.debug_bounds("user-image-thumbnail").unwrap();
                    let bubble = window.debug_bounds("user-message-bubble").unwrap();
                    assert_eq!(thumbnail.size, size(px(80.), px(80.)));
                    assert!(thumbnail.bottom() <= bubble.top());
                    assert_eq!(thumbnail.right(), bubble.right());
                    let bounds = window
                        .debug_bounds("chat-image")
                        .expect("image must render");
                    assert!(
                        bounds.size.width > px(0.),
                        "image must remain visible: {bounds:?}"
                    );
                    assert!(
                        bounds.size.width <= px(528_f32.min(viewport_width - 32.)),
                        "image exceeds bubble: {bounds:?}"
                    );
                    assert!(
                        bounds.left() >= px(0.) && bounds.right() <= px(viewport_width),
                        "image overflows conversation: {bounds:?}"
                    );
                }
            }
        }
    }
}

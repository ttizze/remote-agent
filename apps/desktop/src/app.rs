//! GPUI rendering of the shared T3 conversation presentation.
mod dictation;
mod hosts;
mod view;
use crate::{Runtime, diff::DiffView, platform, store_session::StoreSession};
use agent_core::{
    presentation::*,
    state::{Intent, QuestionAnswer, QueueAction, SendBehavior, Snapshot, ThreadAction},
    store::Outcome,
};
use agent_protocol::{models::RemoteHost, session::ProviderKind};
use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        input::{Editor, EditorState, Input, InputEvent, InputState, Textarea, TextareaState},
        menu::{ContextMenuExt, DropdownMenu, PopupMenuItem},
        text::TextView,
        *,
    },
    prelude::FluentBuilder,
    *,
};
use hosts::{ConnectionLayout, HostEvent, Hosts};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, OnceLock},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Diff,
    Terminal,
    Files,
    Browser,
}
enum Update {
    Connected(Result<(StoreSession, PathBuf), String>),
    Snapshot(Arc<Snapshot>),
    Completed(Option<u64>, Result<Outcome, String>),
    Folder(Option<PathBuf>),
    PersistenceError(String),
    Recording(uuid::Uuid, platform::RecordingEvent),
    Tick,
}
struct QuestionInput {
    selected: BTreeSet<String>,
    custom: Entity<InputState>,
    multi: bool,
}
pub(crate) struct Desktop {
    session: Option<StoreSession>,
    snapshot: Arc<Snapshot>,
    runtime: Runtime,
    updates: async_channel::Sender<(u64, Update)>,
    epoch: u64,
    connecting: bool,
    remote: Option<RemoteHost>,
    hosts: Entity<Hosts>,
    settings: bool,
    error: String,
    composer: Entity<TextareaState>,
    composer_revision: u64,
    pending_draft: Option<u64>,
    search: Entity<InputState>,
    rename: Entity<InputState>,
    renaming: bool,
    collapsed_shelves: BTreeSet<ShelfKind>,
    settled_limit: usize,
    show_archive: bool,
    expanded: BTreeSet<String>,
    questions: BTreeMap<(String, String), QuestionInput>,
    timeline_scroll: ScrollHandle,
    anchors: BTreeMap<String, ScrollAnchor>,
    panel: Option<Panel>,
    terminal: Option<Entity<crate::terminal::Terminal>>,
    browser: Option<Entity<crate::browser::Browser>>,
    diff: Entity<DiffView>,
    file_path: Entity<InputState>,
    editor: Entity<EditorState>,
    editor_path: Option<String>,
    editor_value: String,
    account_code: Entity<InputState>,
    dictation: Option<dictation::Dictation>,
    tick: Option<tokio_util::task::AbortOnDropHandle<()>>,
    _subscriptions: Vec<Subscription>,
}
fn palette() -> &'static agent_core::presentation::theme::Theme {
    static THEME: OnceLock<agent_core::presentation::theme::Theme> = OnceLock::new();
    THEME.get_or_init(|| agent_core::presentation::theme::theme(true))
}
pub(crate) fn color(role: &str) -> Rgba {
    rgb(u32::from_str_radix(
        palette()
            .colors
            .get(role)
            .map_or("ffffff", |v| v.trim_start_matches('#')),
        16,
    )
    .unwrap_or(0xffffff))
}
pub(crate) fn apply_theme(cx: &mut App) {
    let theme = gpui_kit::component::Theme::global_mut(cx);
    theme.font_size = px(palette().prompt_size);
    theme.mono_font_size = px(palette().code_size);
    theme.radius = px(palette().radius);
    for (target, role) in [
        (&mut theme.colors.background, "canvas"),
        (&mut theme.colors.foreground, "text"),
        (&mut theme.colors.border, "border"),
        (&mut theme.colors.input, "input"),
        (&mut theme.colors.muted, "muted"),
        (&mut theme.colors.muted_foreground, "textMuted"),
        (&mut theme.colors.popover, "surfaceOverlay"),
        (&mut theme.colors.popover_foreground, "text"),
        (&mut theme.colors.primary, "accent"),
        (&mut theme.colors.primary_hover, "messageActionHover"),
        (&mut theme.colors.primary_foreground, "text"),
        (&mut theme.colors.ring, "focus"),
        (&mut theme.colors.accent, "accentSurface"),
        (&mut theme.colors.accent_foreground, "text"),
        (&mut theme.colors.sidebar, "sidebar"),
        (&mut theme.colors.sidebar_foreground, "sidebarForeground"),
        (&mut theme.colors.sidebar_border, "sidebarBorder"),
        (&mut theme.colors.link, "accent"),
        (&mut theme.colors.button, "surface"),
        (&mut theme.colors.button_foreground, "text"),
        (&mut theme.colors.button_hover, "toolbarControlHover"),
        (&mut theme.colors.tab_bar, "toolbar"),
    ] {
        *target = color(role).into();
    }
}
fn now() -> orchestration::Timestamp {
    orchestration::Timestamp::parse(&chrono::Utc::now().to_rfc3339()).expect("UTC timestamp")
}
#[derive(Clone)]
struct PinnedDrag {
    id: String,
    title: String,
}
impl Render for PinnedDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .p_3()
            .rounded(px(8.))
            .bg(color("sidebarRowSelected"))
            .text_color(color("text"))
            .child(self.title.clone())
    }
}
impl Desktop {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = cx.global::<Runtime>().clone();
        let (updates, incoming) = async_channel::bounded(16);
        StoreSession::on_app_quit(cx, |view| &mut view.session);
        cx.spawn_in(window, async move |view, cx| {
            while let Ok((epoch, update)) = incoming.recv().await {
                if view
                    .update_in(cx, |view, window, cx| {
                        if view.epoch == epoch {
                            view.receive(update, window, cx);
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Ask anything…")
                .auto_grow(2, 10)
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search threads…"));
        let rename = cx.new(|cx| InputState::new(window, cx).placeholder("Thread title"));
        let file_path = cx.new(|cx| InputState::new(window, cx).placeholder("Path on Host"));
        let editor = cx.new(|cx| EditorState::new(window, cx));
        let account_code = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Authentication code")
                .masked(true)
        });
        let hosts = cx.new(|cx| Hosts::new(window, cx));
        let subscriptions = vec![
            cx.subscribe(&composer, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if text != view.snapshot.current_draft().text {
                        let mut draft = view.snapshot.current_draft();
                        draft.text = text;
                        view.composer_revision += 1;
                        view.pending_draft = Some(view.composer_revision);
                        view.perform(Intent::EditDraft { draft }, Some(view.composer_revision));
                    }
                }
            }),
            cx.subscribe(&search, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    view.perform(
                        Intent::Search {
                            query: input.read(cx).value().to_string(),
                        },
                        None,
                    );
                }
            }),
            cx.subscribe(&editor, |view, input, event, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(path) = view.editor_path.clone()
                {
                    let text = input.read(cx).value().to_string();
                    if text != view.editor_value {
                        view.editor_value = text.clone();
                        view.perform(Intent::EditFile { path, text }, None);
                    }
                }
            }),
            cx.subscribe_in(&hosts, window, |view, _, event, _, cx| {
                match event {
                    HostEvent::Selected(remote) => {
                        view.settings = false;
                        view.connect(remote.clone());
                    }
                    HostEvent::Removed(id) if view.remote.as_ref().is_some_and(|r| &r.id == id) => {
                        view.connect(None)
                    }
                    _ => {}
                }
                cx.notify();
            }),
        ];
        let mut view = Self {
            session: None,
            snapshot: Arc::default(),
            runtime,
            updates,
            epoch: 0,
            connecting: false,
            remote: None,
            hosts,
            settings: false,
            error: String::new(),
            composer,
            composer_revision: 0,
            pending_draft: None,
            search,
            rename,
            renaming: false,
            collapsed_shelves: BTreeSet::new(),
            settled_limit: 10,
            show_archive: false,
            expanded: BTreeSet::new(),
            questions: BTreeMap::new(),
            timeline_scroll: ScrollHandle::new(),
            anchors: BTreeMap::new(),
            panel: None,
            terminal: None,
            browser: None,
            diff: cx.new(|_| DiffView::new("".into(), true)),
            file_path,
            editor,
            editor_path: None,
            editor_value: String::new(),
            account_code,
            dictation: None,
            tick: None,
            _subscriptions: subscriptions,
        };
        if let Some(error) = &view.runtime.logging_error {
            view.error = error.clone();
        }
        view.connect(None);
        view
    }
    fn connect(&mut self, remote: Option<RemoteHost>) {
        self.cancel_recording();
        self.epoch += 1;
        self.connecting = true;
        self.remote = remote;
        self.pending_draft = None;
        self.snapshot = Arc::default();
        self.session.take();
        self.terminal = None;
        self.browser = None;
        self.panel = None;
        self.questions.clear();
        let updates = self.updates.clone();
        let epoch = self.epoch;
        let runtime = self.runtime.clone();
        let connections = runtime.connections.clone();
        let ticket = self.remote.as_ref().map(|r| r.ticket.clone());
        let name = self
            .remote
            .as_ref()
            .map(|r| r.id.clone())
            .unwrap_or_else(|| "local".into());
        self.runtime.handle.spawn(async move {
            let state = (|| -> anyhow::Result<(PathBuf, Snapshot)> {
                let path = platform::state_dir()
                    .map_err(anyhow::Error::msg)?
                    .join(format!("orchestration-{name}.json"));
                let bytes = std::fs::read(&path).unwrap_or_default();
                let preferences =
                    std::fs::read(path.with_file_name("orchestration-model-preferences.json"))
                        .unwrap_or_default();
                let bytes = agent_core::persistence::apply_model_preferences(&bytes, &preferences)?;
                Ok((path, agent_core::persistence::decode(&bytes)?))
            })();
            match state {
                Err(error) => {
                    let _ = updates
                        .send((epoch, Update::Connected(Err(error.to_string()))))
                        .await;
                }
                Ok((path, snapshot)) => {
                    let (tx, rx) = async_channel::bounded(8);
                    let relay = updates.clone();
                    let forward = tokio::spawn(async move {
                        while let Ok(event) = rx.recv().await {
                            if relay.send((epoch, event)).await.is_err() {
                                break;
                            }
                        }
                    });
                    StoreSession::publish(
                        connections.connect(ticket.as_deref(), snapshot).await,
                        runtime.clone(),
                        tx,
                        move |result| {
                            Update::Connected(result.map(|session| (session, path.clone())))
                        },
                        Update::Snapshot,
                    )
                    .await;
                    let _ = forward.await;
                }
            }
        });
        let updates = self.updates.clone();
        self.tick = Some(tokio_util::task::AbortOnDropHandle::new(
            self.runtime.handle.spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    if updates.send((epoch, Update::Tick)).await.is_err() {
                        break;
                    }
                }
            }),
        ));
    }
    fn perform(&self, intent: Intent, revision: Option<u64>) {
        if let Some(session) = &self.session {
            let receipt = session.store.dispatch(intent);
            let updates = self.updates.clone();
            let epoch = self.epoch;
            self.runtime.handle.spawn(async move {
                let result = receipt
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r.map_err(|e| e.to_string()));
                let _ = updates
                    .send((epoch, Update::Completed(revision, result)))
                    .await;
            });
        }
    }
    fn receive(&mut self, update: Update, window: &mut Window, cx: &mut Context<Self>) {
        let before = conversation(&self.snapshot);
        let follow =
            self.timeline_scroll.max_offset().y + self.timeline_scroll.offset().y <= px(80.);
        match update {
            Update::Connected(Ok((mut session, path))) => {
                let (tx, rx) = async_channel::bounded(4);
                let updates = self.updates.clone();
                let epoch = self.epoch;
                self.runtime.handle.spawn(async move {
                    while let Ok(event) = rx.recv().await {
                        if updates.send((epoch, event)).await.is_err() {
                            break;
                        }
                    }
                });
                session.persist(path, tx, Update::PersistenceError);
                self.snapshot = session.store.snapshot();
                self.session = Some(session);
                self.connecting = false;
                self.perform(Intent::LoadAccounts, None);
            }
            Update::Connected(Err(error)) => {
                self.connecting = false;
                self.error = error;
            }
            Update::Snapshot(snapshot) => {
                self.snapshot = snapshot;
                if let Some(session) = &self.session {
                    session.save(self.snapshot.clone());
                }
            }
            Update::Completed(revision, result) => {
                if revision.is_some() && revision == self.pending_draft {
                    self.pending_draft = None;
                }
                if let Some(session) = &self.session {
                    self.snapshot = session.store.snapshot();
                    session.save(self.snapshot.clone());
                }
                if let Err(error) = result {
                    self.error = error;
                }
            }
            Update::Folder(Some(path)) => self.perform(
                Intent::RegisterProject {
                    path: path.to_string_lossy().into(),
                },
                None,
            ),
            Update::Folder(None) => {}
            Update::PersistenceError(error) => self.error = error,
            Update::Recording(id, event) => self.recording_update(id, event),
            Update::Tick => {}
        }
        if self.pending_draft.is_none() {
            let text = self.snapshot.current_draft().text;
            if self.composer.read(cx).value().as_ref() != text {
                self.composer
                    .update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
        let conversation = conversation(&self.snapshot);
        let rows = conversation
            .rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<BTreeSet<_>>();
        self.anchors.retain(|id, _| rows.contains(id));
        for id in rows {
            self.anchors
                .entry(id)
                .or_insert_with(|| ScrollAnchor::for_handle(self.timeline_scroll.clone()));
        }
        if before.thread_id == conversation.thread_id
            && before.rows.first().map(|r| &r.id) != conversation.rows.first().map(|r| &r.id)
            && before.rows.last().map(|r| &r.id) == conversation.rows.last().map(|r| &r.id)
            && conversation.rows.len() > before.rows.len()
        {
            if let Some(anchor) = before
                .rows
                .first()
                .and_then(|row| self.anchors.get(&row.id))
            {
                anchor.scroll_to(window, cx);
            }
        } else if before.thread_id != conversation.thread_id
            || (follow && before.rows.last() != conversation.rows.last())
        {
            self.timeline_scroll.scroll_to_bottom();
        }
        let mut live = BTreeSet::new();
        for row in &conversation.rows {
            if let Some(request) = &row.request_id {
                for question in &row.questions {
                    let key = (request.clone(), question.id.clone());
                    live.insert(key.clone());
                    self.questions.entry(key).or_insert_with(|| QuestionInput {
                        selected: BTreeSet::new(),
                        multi: question.multi_select,
                        custom: cx
                            .new(|cx| InputState::new(window, cx).placeholder("Your answer…")),
                    });
                }
            }
        }
        self.questions.retain(|key, _| live.contains(key));
        if let Some(file) = &self.snapshot.workspace.file {
            let text = self
                .snapshot
                .workspace
                .file_drafts
                .get(&file.path)
                .map(|draft| &draft.text)
                .unwrap_or(&file.text);
            if self.editor_path.as_ref() != Some(&file.path) || self.editor_value != *text {
                self.editor_path = Some(file.path.clone());
                self.editor_value = text.clone();
                self.editor
                    .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
            }
        }
        if let Some(review) = &self.snapshot.workspace.review {
            self.diff
                .update(cx, |view, cx| view.set_source(&review.diff, cx));
        }
        cx.notify();
    }
    fn action(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        intent: Intent,
        cx: &Context<Self>,
    ) -> Button {
        Button::new(id)
            .label(label)
            .small()
            .ghost()
            .on_click(cx.listener(move |view, _, _, _| view.perform(intent.clone(), None)))
    }
    fn thread_action(&self, action: ThreadAction) {
        if let Some(id) = &self.snapshot.selected_thread {
            self.perform(
                Intent::Thread {
                    thread_id: id.to_string(),
                    action,
                },
                None,
            );
        }
    }
    fn pick_folder(&self) {
        let updates = self.updates.clone();
        let epoch = self.epoch;
        self.runtime.handle.spawn(async move {
            let path = tokio::task::spawn_blocking(platform::choose_folder)
                .await
                .ok()
                .flatten();
            let _ = updates.send((epoch, Update::Folder(path))).await;
        });
    }
    fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        if self.panel == Some(panel) {
            self.panel = None;
        } else {
            match panel {
                Panel::Terminal if self.terminal.is_none() => {
                    self.terminal = Some(crate::terminal::Terminal::new(
                        self.remote.as_ref().map_or("", |r| &r.ticket),
                        self.snapshot.cwd(),
                        window,
                        cx,
                    ));
                }
                Panel::Browser if self.browser.is_none() => {
                    match crate::browser::Browser::new(
                        wry::WebViewBuilder::new(),
                        #[cfg(target_os = "macos")]
                        crate::browser::ChromeProfileSource::default(),
                        window,
                        cx,
                    ) {
                        Ok(browser) => self.browser = Some(browser),
                        Err(error) => self.error = error,
                    }
                }
                Panel::Files => self.perform(
                    Intent::ListFiles {
                        path: self.snapshot.cwd(),
                    },
                    None,
                ),
                Panel::Diff => self.perform(
                    Intent::ReviewWorkspace {
                        cwd: self.snapshot.cwd(),
                    },
                    None,
                ),
                _ => {}
            }
            self.panel = Some(panel);
        }
        if let Some(browser) = &self.browser {
            browser.update(cx, |browser, cx| {
                browser.set_visible(self.panel == Some(Panel::Browser), cx)
            });
        }
        cx.notify();
    }
    fn answers(&self, request: &str, cx: &App) -> Vec<QuestionAnswer> {
        self.questions
            .iter()
            .filter(|((id, _), _)| id == request)
            .map(|((_, id), input)| {
                let mut values = input.selected.iter().cloned().collect::<Vec<_>>();
                let custom = input.custom.read(cx).value().to_string();
                if !custom.trim().is_empty() {
                    if !input.multi {
                        values.clear();
                    }
                    values.push(custom);
                }
                QuestionAnswer {
                    question_id: id.clone(),
                    values,
                }
            })
            .collect()
    }
}
impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.view(window, cx)
    }
}

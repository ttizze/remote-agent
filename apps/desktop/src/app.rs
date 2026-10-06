//! GPUI rendering of the shared conversation presentation.
mod attachments;
mod dictation;
mod hosts;
mod view;
use crate::{Runtime, diff::DiffView, platform, store_session::StoreSession};
use agent_core::{
    presentation::*,
    state::{Intent, QuestionAnswer, QueueAction, SendBehavior, Snapshot, ThreadAction},
    store::Outcome,
};
use agent_protocol::{models::RemoteHost, provider::ProviderKind};
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
use hosts::{HostEvent, Hosts};
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
#[derive(Clone, Copy)]
enum BufferRevision {
    Draft(u64),
    Editor(u64),
}
enum Update {
    AttachmentsPicked(String, Result<Vec<PathBuf>, String>),
    AttachmentReady(String, Result<PathBuf, String>),
    Connected(Result<(StoreSession, PathBuf), String>),
    Snapshot(Arc<Snapshot>),
    Presentation {
        revision: u64,
        view: Box<ConversationView>,
    },
    Completed(Option<BufferRevision>, Result<Outcome, String>),
    Folder(Option<PathBuf>),
    PersistenceError(String),
    Recording(uuid::Uuid, platform::RecordingEvent),
    Transcribed(uuid::Uuid, Result<Outcome, String>),
    Tick,
}
struct QuestionInput {
    selected: BTreeSet<String>,
    custom: Entity<InputState>,
    multi: bool,
}
pub(crate) struct Desktop {
    attachment_cache: BTreeMap<String, Option<PathBuf>>,
    attachment_directory: Arc<tempfile::TempDir>,
    session: Option<StoreSession>,
    snapshot: Arc<Snapshot>,
    conversation: Arc<ConversationView>,
    presentation_running: bool,
    presented_revision: u64,
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
    composer_base: String,
    composer_key: String,
    search: Entity<InputState>,
    rename: Entity<InputState>,
    renaming: bool,
    collapsed_shelves: BTreeSet<ShelfKind>,
    settled_limit: usize,
    show_archive: bool,
    expanded: BTreeSet<String>,
    questions: BTreeMap<(String, String), QuestionInput>,
    timeline: ListState,
    panel: Option<Panel>,
    terminal: Option<Entity<crate::terminal::Terminal>>,
    browser: Option<Entity<crate::browser::Browser>>,
    diff: Entity<DiffView>,
    file_path: Entity<InputState>,
    editor: Entity<EditorState>,
    editor_path: Option<String>,
    editor_value: String,
    editor_revision: u64,
    pending_editor: Option<u64>,
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
                .submit_on_enter(true)
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
            cx.subscribe(&rename, |view, _, event, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    view.thread_action(ThreadAction::Rename {
                        title: view.rename.read(cx).value().to_string(),
                    });
                    view.renaming = false;
                    cx.notify();
                }
            }),
            cx.subscribe(&composer, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if view.session.is_some() && text != view.composer_base {
                        view.composer_revision += 1;
                        view.pending_draft = Some(view.composer_revision);
                        let base_text =
                            Some(std::mem::replace(&mut view.composer_base, text.clone()));
                        view.perform(
                            Intent::EditDraft { text, base_text },
                            Some(BufferRevision::Draft(view.composer_revision)),
                        );
                    }
                }
            }),
            cx.subscribe(&composer, |view, _, event, _| {
                if let InputEvent::PressEnter { shift: false, .. } = event
                    && (view.conversation.composer.enabled
                        || view.conversation.composer.plan_follow_up)
                {
                    view.perform(
                        Intent::Send {
                            behavior: SendBehavior::Default,
                        },
                        None,
                    );
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
                        view.editor_revision += 1;
                        view.pending_editor = Some(view.editor_revision);
                        view.perform(
                            Intent::EditFile { path, text },
                            Some(BufferRevision::Editor(view.editor_revision)),
                        );
                    }
                }
            }),
            cx.subscribe_in(&hosts, window, |view, _, event, _, cx| {
                match event {
                    HostEvent::Selected(remote) => {
                        view.settings = false;
                        if view.remote.as_ref().map(|r| (&r.id, &r.ticket))
                            != remote.as_ref().map(|r| (&r.id, &r.ticket))
                            || !view.snapshot.connected
                        {
                            view.connect(remote.clone());
                        }
                        view.sync_browser_visibility(cx);
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
            attachment_cache: Default::default(),
            attachment_directory: Arc::new(
                tempfile::tempdir().expect("attachment cache directory"),
            ),
            session: None,
            snapshot: Arc::default(),
            conversation: Arc::new(conversation(&Snapshot::default(), &now())),
            presentation_running: false,
            presented_revision: 0,
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
            composer_base: String::new(),
            composer_key: String::new(),
            search,
            rename,
            renaming: false,
            collapsed_shelves: BTreeSet::from([ShelfKind::Working, ShelfKind::Snoozed]),
            settled_limit: 10,
            show_archive: false,
            expanded: BTreeSet::new(),
            questions: BTreeMap::new(),
            timeline: ListState::new(1, ListAlignment::Bottom, px(600.)),
            panel: None,
            terminal: None,
            browser: None,
            diff: cx.new(|_| DiffView::new("".into(), true)),
            file_path,
            editor,
            editor_path: None,
            editor_value: String::new(),
            editor_revision: 0,
            pending_editor: None,
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
        self.attachment_cache.clear();
        self.cancel_recording();
        self.epoch += 1;
        self.presentation_running = false;
        self.presented_revision = 0;
        self.connecting = true;
        self.remote = remote;
        self.pending_draft = None;
        self.snapshot = Arc::default();
        self.conversation = Arc::new(conversation(&self.snapshot, &now()));
        self.timeline.reset(1);
        self.editor_path = None;
        self.editor_value.clear();
        self.pending_editor = None;
        self.renaming = false;
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
                let snapshot = agent_core::persistence::recover(&bytes, &preferences);
                Ok((path, snapshot))
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
    fn perform(&self, intent: Intent, revision: Option<BufferRevision>) {
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
        if matches!(update, Update::Tick) {
            cx.notify();
            return;
        }
        let mut prepared = None;
        match update {
            Update::AttachmentsPicked(key, paths) => {
                let paths = match paths {
                    Ok(paths) => paths,
                    Err(error) => {
                        self.error = error;
                        return;
                    }
                };
                for path in paths {
                    if let Some(name) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned)
                    {
                        self.perform(
                            Intent::AttachFile {
                                path: path.to_string_lossy().into_owned(),
                                mime_type: agent_core::commands::attachment_mime(&name).into(),
                                name,
                                draft_key: key.clone(),
                            },
                            None,
                        );
                    }
                }
            }
            Update::AttachmentReady(id, result) => {
                match result {
                    Ok(path) => {
                        self.attachment_cache.insert(id, Some(path));
                    }
                    Err(error) => self.error = error,
                };
            }
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
                if !snapshot_is_newer(self.snapshot.revision, snapshot.revision) {
                    return;
                }
                self.snapshot = snapshot;
                if let Some(session) = &self.session {
                    session.save(self.snapshot.clone());
                }
            }
            Update::Completed(revision, result) => {
                if matches!(revision, Some(BufferRevision::Editor(r)) if self.pending_editor == Some(r))
                {
                    self.pending_editor = None;
                }
                if matches!(revision, Some(BufferRevision::Draft(r)) if self.pending_draft == Some(r))
                {
                    self.pending_draft = None;
                }
                if let Some(session) = &self.session {
                    let snapshot = session.store.snapshot();
                    if snapshot_is_newer(self.snapshot.revision, snapshot.revision) {
                        self.snapshot = snapshot;
                    }
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
            Update::Transcribed(id, result) => {
                if self.dictation.as_ref().is_some_and(|d| d.id == id) {
                    self.dictation = None;
                }
                if let Some(session) = &self.session {
                    self.snapshot = session.store.snapshot();
                }
                if let Err(error) = result {
                    self.error = error;
                }
            }
            Update::Presentation { revision, view } => {
                self.presentation_running = false;
                if revision >= self.presented_revision
                    && view.thread_id.as_deref()
                        == self.snapshot.selected_thread.as_ref().map(|id| id.as_str())
                {
                    self.presented_revision = revision;
                    prepared = Some(*view);
                }
            }
            Update::Tick => {}
        }
        let key = self.snapshot.draft_key();
        if key != self.composer_key {
            self.pending_draft = None;
            self.composer_key = key;
        }
        if self.pending_draft.is_none() {
            let text = self.snapshot.current_draft().text;
            if self.composer.read(cx).value().as_ref() != text {
                self.composer_base = text.clone();
                self.composer
                    .update(cx, |input, cx| input.set_value(text, window, cx));
            }
        }
        if let Some(view) = prepared {
            self.preload_attachments(&view);
            self.set_conversation(view, window, cx);
        } else if self.conversation.thread_id.as_deref()
            != self.snapshot.selected_thread.as_ref().map(|id| id.as_str())
            || self.conversation.cwd != self.snapshot.cwd()
        {
            self.presented_revision = self.snapshot.revision;
            self.set_conversation(conversation(&self.snapshot, &now()), window, cx);
        }
        self.schedule_presentation();
        if let Some(file) = &self.snapshot.workspace.file {
            let text = self
                .snapshot
                .workspace
                .file_drafts
                .get(&file.path)
                .map(|draft| &draft.text)
                .unwrap_or(&file.text);
            if self.editor_path.as_ref() != Some(&file.path)
                || (self.pending_editor.is_none() && self.editor_value != *text)
            {
                self.editor_path = Some(file.path.clone());
                self.editor_value = text.clone();
                self.editor
                    .update(cx, |input, cx| input.set_value(text.clone(), window, cx));
            }
        }
        self.diff.update(cx, |view, cx| {
            view.set_source(
                self.snapshot
                    .workspace
                    .review
                    .as_ref()
                    .map_or("", |review| &review.diff),
                cx,
            )
        });
        cx.notify();
    }
    fn schedule_presentation(&mut self) {
        if self.presentation_running || self.presented_revision >= self.snapshot.revision {
            return;
        }
        self.presentation_running = true;
        let snapshot = self.snapshot.clone();
        let epoch = self.epoch;
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            let revision = snapshot.revision;
            if let Ok(view) =
                tokio::task::spawn_blocking(move || conversation(&snapshot, &now())).await
            {
                let _ = updates
                    .send((
                        epoch,
                        Update::Presentation {
                            revision,
                            view: Box::new(view),
                        },
                    ))
                    .await;
            }
        });
    }
    fn set_conversation(
        &mut self,
        conversation: ConversationView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.conversation.clone();
        let follow = self.timeline.max_offset_for_scrollbar().y
            + self.timeline.scroll_px_offset_for_scrollbar().y
            <= px(80.);
        let switched = before.thread_id != conversation.thread_id || before.cwd != conversation.cwd;
        if switched {
            self.renaming = false;
            self.pending_editor = None;
            self.editor_path = None;
            self.editor_value.clear();
            self.terminal = None;
            if !conversation.cwd.is_empty() {
                match self.panel {
                    Some(Panel::Diff) => self.perform(
                        Intent::ReviewWorkspace {
                            cwd: conversation.cwd.clone(),
                        },
                        None,
                    ),
                    Some(Panel::Files) => self.perform(
                        Intent::ListFiles {
                            path: conversation.cwd.clone(),
                        },
                        None,
                    ),
                    Some(Panel::Terminal) => {
                        if let Some(session) = &self.session {
                            self.terminal = Some(crate::terminal::Terminal::new(
                                session.store.clone(),
                                conversation.cwd.clone(),
                                window,
                                cx,
                            ));
                        }
                    }
                    _ => {}
                }
            }
            self.timeline.reset(conversation.rows.len() + 1);
        } else {
            let (range, count) = timeline_splice(&before.rows, &conversation.rows);
            self.timeline.splice(range, count);
            for (index, (old, new)) in before.rows.iter().zip(&conversation.rows).enumerate() {
                if old != new {
                    self.timeline.remeasure_items(index + 1..index + 2);
                }
            }
        }
        if switched || (follow && before.rows.last() != conversation.rows.last()) {
            self.timeline.scroll_to_end();
        }
        self.conversation = Arc::new(conversation);
        self.composer.update(cx, |input, cx| {
            input.set_placeholder(self.conversation.composer.placeholder.clone(), window, cx)
        });
        let mut live = BTreeSet::new();
        for row in &self.conversation.requests {
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
                Panel::Terminal if !self.snapshot.terminal_available() => return,
                Panel::Terminal if self.terminal.is_none() => {
                    if let Some(session) = &self.session {
                        self.terminal = Some(crate::terminal::Terminal::new(
                            session.store.clone(),
                            self.snapshot.cwd(),
                            window,
                            cx,
                        ));
                    }
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
                browser.set_visible(!self.settings && self.panel == Some(Panel::Browser), cx)
            });
        }
        cx.notify();
    }
    fn sync_browser_visibility(&mut self, cx: &mut Context<Self>) {
        if let Some(browser) = &self.browser {
            browser.update(cx, |browser, cx| {
                browser.set_visible(!self.settings && self.panel == Some(Panel::Browser), cx)
            });
        }
    }
    fn confirm_thread_action(
        &self,
        id: String,
        action: ThreadAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(action, ThreadAction::Delete) {
            let answer = window.prompt(
                gpui::PromptLevel::Warning,
                "Delete this thread?",
                Some("This permanently deletes the conversation."),
                &["Cancel", "Delete"],
                cx,
            );
            cx.spawn(async move |view, cx| {
                if let Ok(1) = answer.await {
                    let _ = view.update(cx, |view, _| {
                        view.perform(
                            Intent::Thread {
                                thread_id: id,
                                action: ThreadAction::Delete,
                            },
                            None,
                        )
                    });
                }
            })
            .detach();
        } else {
            self.perform(
                Intent::Thread {
                    thread_id: id,
                    action,
                },
                None,
            );
        }
    }
    fn answers(&self, request: &str, cx: &App) -> Vec<QuestionAnswer> {
        self.questions
            .iter()
            .filter(|((id, _), _)| id == request)
            .map(|((_, id), input)| {
                let values = question_answer_values(
                    input.selected.iter().cloned().collect(),
                    input.custom.read(cx).value().to_string(),
                    input.multi,
                );
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

fn timeline_splice(old: &[TimelineRow], new: &[TimelineRow]) -> (std::ops::Range<usize>, usize) {
    let prefix = old
        .iter()
        .zip(new)
        .take_while(|(a, b)| a.id == b.id)
        .count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a.id == b.id)
        .count();
    (
        prefix + 1..old.len() - suffix + 1,
        new.len() - prefix - suffix,
    )
}

#[cfg(test)]
mod tests {
    use super::{ListAlignment, ListOffset, ListState, RowKind, TimelineRow, px, timeline_splice};
    fn row(id: &str) -> TimelineRow {
        TimelineRow {
            attachments: vec![],
            id: id.into(),
            kind: RowKind::Assistant,
            text: id.into(),
            title: String::new(),
            status: String::new(),
            streaming: false,
            collapsible: false,
            work: vec![],
            request_id: None,
            choices: vec![],
            questions: vec![],
            response_mode_message: false,
            actionable: false,
            run_id: None,
            rollback_checkpoint_id: None,
            fork_source_thread_id: None,
            duration_ms: None,
        }
    }
    #[test]
    fn prepending_history_preserves_the_visible_item_anchor() {
        let old = vec![row("a"), row("b")];
        let new = vec![row("history"), row("older"), row("a"), row("b")];
        let list = ListState::new(old.len() + 1, ListAlignment::Bottom, px(600.));
        list.scroll_to(ListOffset {
            item_ix: 2,
            offset_in_item: px(17.),
        });
        let (range, count) = timeline_splice(&old, &new);
        assert_eq!(range, 1..1);
        assert_eq!(count, 2);
        list.splice(range, count);
        assert_eq!(list.logical_scroll_top().item_ix, 4);
        assert_eq!(list.logical_scroll_top().offset_in_item, px(17.));
    }
    #[test]
    fn replacing_middle_rows_preserves_the_suffix_and_streaming_reuses_identity() {
        let old = vec![row("a"), row("b"), row("c")];
        let new = vec![row("a"), row("replacement"), row("extra"), row("c")];
        assert_eq!(timeline_splice(&old, &new), (2..3, 2));
        let mut streamed = old.clone();
        streamed[2].text.push_str(" more output");
        assert_eq!(timeline_splice(&old, &streamed), (4..4, 0));
    }
}

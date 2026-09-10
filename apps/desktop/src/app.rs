mod clipboard;
mod dictation;
mod hosts;
mod view;

use crate::{Runtime, diff::DiffView, platform};
use agent_core::{
    client::{Answer, ServerRequest},
    models::{Item, Model, RemoteHost, Thread, Turn, WorktreeSettings},
    state::{Attachment, Draft, Intent, PendingSubmission, Snapshot},
    store::{Outcome, Store},
};
use dictation::{Dictation, Phase};
use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        input::{Editor, EditorState, Input, InputEvent, InputState, Textarea, TextareaState},
        menu::{DropdownMenu, PopupMenuItem},
        text::TextView,
        *,
    },
    prelude::FluentBuilder,
    *,
};
use hosts::{HostEvent, Hosts};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

type UiEffect = Box<dyn FnOnce(&mut Desktop, &mut Window, &mut Context<Desktop>) + Send>;
enum Update {
    Connected {
        epoch: u64,
        result: Result<(Arc<Store>, PathBuf), String>,
    },
    Snapshot(u64),
    Ui {
        epoch: u64,
        effect: UiEffect,
    },
    Recording(uuid::Uuid, platform::RecordingEvent),
    PersistenceError {
        epoch: u64,
        error: String,
    },
}
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Chat,
    Settings,
}
#[derive(Clone, Copy, PartialEq)]
enum Panel {
    Home,
    Terminal,
    SideChat,
    Browser,
    Files,
    Diff,
}
pub(crate) enum Mode {
    Main,
    SideChat {
        remote: Option<RemoteHost>,
        cwd: String,
    },
}
struct Question {
    id: String,
    input: Entity<InputState>,
    options: Vec<String>,
    prompt: String,
}
struct RequestInputs {
    questions: Vec<Question>,
    raw: Entity<TextareaState>,
    sent: bool,
}
struct ImageGallery {
    id: uuid::Uuid,
    entries: Vec<(Arc<String>, bool)>,
    initial: (Arc<String>, bool),
    selected: Option<usize>,
    list: ListState,
    loading: bool,
    saving: bool,
    saved: bool,
    error: String,
}
impl ImageGallery {
    fn current_image(&self) -> &(Arc<String>, bool) {
        self.selected
            .and_then(|index| self.entries.get(index))
            .unwrap_or(&self.initial)
    }
}
struct ImageState {
    source: Arc<String>,
    path: Option<ImageSource>,
    error: Option<String>,
}
struct MarkdownContent {
    source: SharedString,
    rendered: SharedString,
    images: Rc<[String]>,
}
struct DetailLoad {
    loading: bool,
    error: Option<String>,
}
#[derive(Clone)]
enum ConversationRow {
    History,
    Turn(Arc<agent_core::presentation::conversation::RenderedTurn>),
    Pending(String, Arc<PendingSubmission>),
    Request(String, Arc<ServerRequest>),
}
impl ConversationRow {
    fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::History, Self::History) => true,
            (Self::Turn(a), Self::Turn(b)) => a.source.id == b.source.id,
            (Self::Pending(a, _), Self::Pending(b, _))
            | (Self::Request(a, _), Self::Request(b, _)) => a == b,
            _ => false,
        }
    }
    fn unchanged(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Turn(a), Self::Turn(b)) => Arc::ptr_eq(a, b),
            (Self::Pending(a, x), Self::Pending(b, y)) => a == b && Arc::ptr_eq(x, y),
            (Self::Request(a, x), Self::Request(b, y)) => a == b && Arc::ptr_eq(x, y),
            _ => false,
        }
    }
}

/// Business state is owned by Store. Everything else here is a widget, render
/// cache, pending UI effect, or immutable snapshot retained for display.
pub(crate) struct Desktop {
    store: Option<Arc<Store>>,
    snapshot: Arc<Snapshot>,
    runtime: Runtime,
    updates: async_channel::Sender<Update>,
    persistence: Option<tokio::sync::watch::Sender<Arc<Snapshot>>>,
    persistence_task: Option<tokio::task::JoinHandle<()>>,
    epoch: u64,
    connecting: bool,
    remote: Option<RemoteHost>,
    hosts: Option<Entity<Hosts>>,
    side_chat_mode: bool,
    initial_cwd: Option<String>,
    busy: usize,
    error: String,
    composer: Entity<TextareaState>,
    composer_value: SharedString,
    composer_revision: u64,
    composer_pending: Option<u64>,
    editor_input: Entity<EditorState>,
    editor_value: SharedString,
    editor_path: Option<String>,
    editor_revision: u64,
    editor_pending: Option<u64>,
    search: Entity<InputState>,
    path: Entity<InputState>,
    effort_slider: Entity<slider::SliderState>,
    effort_position: (usize, usize),
    worktree_copy_paths: Entity<TextareaState>,
    worktree_directory: Entity<InputState>,
    worktree_dirty: bool,
    worktree_saved: bool,
    worktree_saving: bool,
    worktree_save_pending: bool,
    expanded_projects: HashSet<String>,
    expanded_items: HashSet<String>,
    expanded_work: HashMap<String, (String, bool)>,
    tab: Tab,
    sidebar: bool,
    panel_open: bool,
    panel: Panel,
    side_chat: Option<Entity<Desktop>>,
    terminal: Option<Entity<crate::terminal::Terminal>>,
    browser: Option<Entity<crate::browser::Browser>>,
    dictation: Option<Dictation>,
    review_expanded: bool,
    source_paths: Vec<String>,
    source_items: Vec<Arc<Item>>,
    requests: HashMap<String, RequestInputs>,
    list: ListState,
    rows: Vec<ConversationRow>,
    history_loading: bool,
    history_error: String,
    item_details: HashMap<(String, String), DetailLoad>,
    rendered: Option<Arc<agent_core::presentation::conversation::RenderedConversation>>,
    diffs: HashMap<String, Entity<DiffView>>,
    images: HashMap<String, ImageState>,
    image_gallery: Option<ImageGallery>,
    image_dir: tempfile::TempDir,
    markdown_cache: HashMap<String, MarkdownContent>,
    _subscriptions: Vec<Subscription>,
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.close();
    }
}
impl Desktop {
    fn close(&mut self) -> Option<tokio::task::JoinHandle<()>> {
        let store = self.store.take()?;
        let persistence = self.persistence.take();
        let persistence_task = self.persistence_task.take();
        Some(self.runtime.closing.spawn_on(
            async move {
                let _ = store.close().await;
                if let Some(persistence) = persistence {
                    persistence.send_replace(store.snapshot());
                }
                if let Some(task) = persistence_task {
                    let _ = task.await;
                }
            },
            &self.runtime.handle,
        ))
    }
    pub(crate) fn new(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.on_app_quit(|view, _| {
            // GPUI allows only 200 ms for asynchronous quit futures. Finish the
            // tracked close and disk flush before returning from the callback.
            if let Some(close) = view.close() {
                let _ = view.runtime.handle.block_on(close);
            }
            async {}
        })
        .detach();
        let (remote, initial_cwd, side_chat_mode) = match mode {
            Mode::Main => (None, None, false),
            Mode::SideChat { remote, cwd } => (remote, Some(cwd), true),
        };
        let (updates, incoming) = async_channel::unbounded();
        cx.spawn_in(window, async move |view, cx| {
            while let Ok(update) = incoming.recv().await {
                if view
                    .update_in(cx, |view, window, cx| view.receive(update, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Codex に依頼する")
                .auto_grow(2, 8)
        });
        let editor_input = cx.new(|cx| EditorState::new(window, cx));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("会話を検索"));
        let path = cx.new(|cx| InputState::new(window, cx).placeholder("絶対パス"));
        let worktree_copy_paths = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(".env\n.env.local\nconfig/local")
                .auto_grow(3, 8)
        });
        let worktree_directory = cx.new(|cx| {
            InputState::new(window, cx).placeholder("接続先 Host 上の絶対パス（空欄で既定）")
        });
        let effort_slider = cx.new(|_| slider::SliderState::new().max(1.).step(1.));
        let hosts = (!side_chat_mode).then(|| cx.new(|cx| Hosts::new(window, cx)));
        let mut subscriptions = vec![
            cx.subscribe(&composer, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    if value != view.composer_value {
                        view.composer_value = value.clone();
                        view.composer_revision += 1;
                        let revision = view.composer_revision;
                        view.composer_pending = Some(revision);
                        view.perform(
                            Intent::SetDraftText {
                                thread_id: view.draft_key().into(),
                                text: value.to_string(),
                            },
                            move |view, result, window, cx| {
                                if view.composer_pending == Some(revision) {
                                    view.composer_pending = None;
                                }
                                if let Err(error) = result {
                                    view.error = error;
                                }
                                view.accept_snapshot(window, cx);
                            },
                        );
                    }
                    cx.notify();
                }
            }),
            cx.subscribe(&editor_input, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    if value != view.editor_value
                        && let Some(path) = view.editor_path.clone()
                    {
                        view.editor_value = value.clone();
                        view.editor_revision += 1;
                        let revision = view.editor_revision;
                        view.editor_pending = Some(revision);
                        view.perform(
                            Intent::SetFileDraft {
                                path,
                                text: value.to_string(),
                            },
                            move |view, result, window, cx| {
                                if view.editor_pending == Some(revision) {
                                    view.editor_pending = None;
                                }
                                if let Err(error) = result {
                                    view.error = error;
                                }
                                view.accept_snapshot(window, cx);
                            },
                        );
                    }
                }
            }),
            cx.subscribe(&search, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    if value.as_ref() != view.snapshot.list_query.search_term {
                        let mut query = (*view.snapshot.list_query).clone();
                        query.search_term = value.to_string();
                        view.dispatch(Intent::ListThreads { query });
                    }
                }
            }),
            cx.subscribe(&effort_slider, |view, _, event, _| {
                if let slider::SliderEvent::Change(slider::SliderValue::Single(index)) = event
                    && let Some(model) = view.selected_model()
                    && let Some(effort) = model.supported_reasoning_efforts.get(*index as usize)
                    && view.draft().effort.as_deref() != Some(&effort.reasoning_effort)
                {
                    view.dispatch(Intent::SelectEffort {
                        thread_id: view.draft_key().into(),
                        effort: effort.reasoning_effort.clone(),
                    });
                }
            }),
            cx.subscribe(&worktree_copy_paths, |view, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    view.worktree_dirty = !view.settings_match_inputs(cx);
                    view.worktree_saved = false;
                    cx.notify();
                }
                if matches!(event, InputEvent::Blur) {
                    view.save_worktree_settings(None, cx);
                }
            }),
            cx.subscribe(&worktree_directory, |view, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    view.worktree_dirty = !view.settings_match_inputs(cx);
                    view.worktree_saved = false;
                    cx.notify();
                }
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    view.save_worktree_settings(None, cx);
                }
            }),
        ];
        if let Some(hosts) = &hosts {
            subscriptions.push(
                cx.subscribe_in(hosts, window, |view, _, event, window, cx| {
                    match event {
                        HostEvent::Selected(remote) => view.switch_host(remote.clone(), window, cx),
                        HostEvent::Removed(id)
                            if view.remote.as_ref().is_some_and(|remote| &remote.id == id) =>
                        {
                            view.switch_host(None, window, cx)
                        }
                        _ => {}
                    }
                    cx.notify();
                }),
            );
        }
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(FollowMode::Tail);
        let entity = cx.entity().downgrade();
        list.set_scroll_handler(move |event, window, _| {
            if event.is_scrolled && !event.is_following_tail && event.visible_range.start == 0 {
                let entity = entity.clone();
                window.on_next_frame(move |window, cx| {
                    let _ = entity.update(cx, |view, cx| {
                        if view.history_error.is_empty()
                            && view.list.logical_scroll_top().item_ix == 0
                        {
                            view.older(window, cx);
                        }
                    });
                });
            }
        });
        let mut view = Self {
            store: None,
            snapshot: Arc::default(),
            runtime: cx.global::<Runtime>().clone(),
            updates,
            persistence: None,
            persistence_task: None,
            epoch: 0,
            connecting: false,
            remote,
            hosts,
            side_chat_mode,
            initial_cwd,
            busy: 0,
            error: String::new(),
            composer,
            composer_value: "".into(),
            composer_revision: 0,
            composer_pending: None,
            editor_input,
            editor_value: "".into(),
            editor_path: None,
            editor_revision: 0,
            editor_pending: None,
            search,
            path,
            effort_slider,
            effort_position: (0, 0),
            worktree_copy_paths,
            worktree_directory,
            worktree_dirty: false,
            worktree_saved: false,
            worktree_saving: false,
            worktree_save_pending: false,
            expanded_projects: HashSet::new(),
            expanded_items: HashSet::new(),
            expanded_work: HashMap::new(),
            tab: Tab::Chat,
            sidebar: true,
            panel_open: false,
            panel: Panel::Home,
            side_chat: None,
            terminal: None,
            browser: None,
            dictation: None,
            review_expanded: false,
            source_paths: Vec::new(),
            source_items: Vec::new(),
            requests: HashMap::new(),
            list,
            rows: Vec::new(),
            history_loading: false,
            history_error: String::new(),
            item_details: HashMap::new(),
            rendered: None,
            diffs: HashMap::new(),
            images: HashMap::new(),
            image_gallery: None,
            image_dir: tempfile::Builder::new()
                .prefix("bex-images-")
                .tempdir()
                .expect("image temporary directory"),
            markdown_cache: HashMap::new(),
            _subscriptions: subscriptions,
        };
        view.connect();
        view
    }
    fn connect(&mut self) {
        self.epoch += 1;
        self.connecting = true;
        self.busy = 0;
        self.error.clear();
        let epoch = self.epoch;
        let remote = self.remote.clone();
        let side = self.side_chat_mode;
        let initial_cwd = self.initial_cwd.take();
        let updates = self.updates.clone();
        let connections = self.runtime.connections.clone();
        self.runtime.handle.spawn(async move {
            let result = async {
                let host = if let Some(remote) = &remote {
                    remote
                        .ticket
                        .parse::<agent_core::transport::Ticket>()
                        .map_err(|error| error.to_string())?
                        .node_id()
                        .to_string()
                } else {
                    "local".into()
                };
                let path = platform::state_dir()?.join(format!(
                    "desktop-{}-{host}.json",
                    if side { "side" } else { "main" }
                ));
                let mut snapshot: Snapshot = match tokio::fs::read(&path).await {
                    Ok(bytes) => serde_json::from_slice(&bytes)
                        .map_err(|error| format!("下書きを読み込めません: {error}"))?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Snapshot::default()
                    }
                    Err(error) => return Err(error.to_string()),
                };
                if let Some(cwd) = initial_cwd.or_else(|| {
                    snapshot
                        .navigation
                        .thread_id
                        .is_none()
                        .then(|| snapshot.navigation.cwd.clone())
                }) {
                    snapshot = agent_core::state::reduce(
                        &snapshot,
                        agent_core::state::Event::Intent(Intent::NewChat { cwd }),
                    )
                    .0;
                }
                let store = connections
                    .connect(
                        remote.as_ref().map(|remote| remote.ticket.as_str()),
                        snapshot,
                    )
                    .await?;
                Ok::<_, String>((Arc::new(store), path))
            }
            .await;
            match result {
                Ok((store, path)) => {
                    let mut snapshots = store.subscribe();
                    if updates
                        .send(Update::Connected {
                            epoch,
                            result: Ok((store, path)),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    loop {
                        snapshots.borrow_and_update();
                        if updates.send(Update::Snapshot(epoch)).await.is_err()
                            || snapshots.changed().await.is_err()
                        {
                            break;
                        }
                    }
                }
                Err(error) => {
                    let _ = updates
                        .send(Update::Connected {
                            epoch,
                            result: Err(error),
                        })
                        .await;
                }
            }
        });
    }
    fn effect<T: Send + 'static>(
        &self,
        future: impl Future<Output = Result<T, String>> + Send + 'static,
        apply: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>)
        + Send
        + 'static,
    ) {
        let epoch = self.epoch;
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            let result = future.await;
            let effect = Box::new(
                move |view: &mut Self, window: &mut Window, cx: &mut Context<Self>| {
                    apply(view, result, window, cx)
                },
            );
            let _ = updates.send(Update::Ui { epoch, effect }).await;
        });
    }
    fn perform(
        &self,
        intent: Intent,
        apply: impl FnOnce(&mut Self, Result<Outcome, String>, &mut Window, &mut Context<Self>)
        + Send
        + 'static,
    ) {
        let Some(store) = &self.store else {
            return;
        };
        let receipt = store.dispatch(intent);
        self.effect(
            async move { receipt.await.map_err(|error| error.to_string()) },
            apply,
        );
    }
    fn dispatch(&self, intent: Intent) {
        self.perform(intent, |view, result, window, cx| {
            if let Err(error) = result {
                view.error = error;
            }
            view.accept_snapshot(window, cx);
        });
    }
    fn receive(&mut self, update: Update, window: &mut Window, cx: &mut Context<Self>) {
        match update {
            Update::Connected { epoch, result } if epoch == self.epoch => {
                self.connecting = false;
                match result {
                    Ok((store, path)) => {
                        self.store = Some(store);
                        self.accept_snapshot(window, cx);
                        let (send, mut receive) =
                            tokio::sync::watch::channel(self.snapshot.clone());
                        self.persistence = Some(send);
                        let updates = self.updates.clone();
                        self.persistence_task = Some(self.runtime.closing.spawn_on(
                            async move {
                                while receive.changed().await.is_ok() {
                                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                                    let snapshot = receive.borrow_and_update().clone();
                                    let path = path.clone();
                                    let result = tokio::task::spawn_blocking(move || {
                                        platform::save_snapshot(&path, &snapshot)
                                    })
                                    .await;
                                    let result = result
                                        .map_err(|error| error.to_string())
                                        .and_then(|result| result);
                                    if let Err(error) = result {
                                        let _ = updates
                                            .send(Update::PersistenceError { epoch, error })
                                            .await;
                                    }
                                }
                            },
                            &self.runtime.handle,
                        ));
                        self.dispatch(Intent::ReadWorktreeSettings);
                    }
                    Err(error) => self.error = error,
                }
            }
            Update::Snapshot(epoch) if epoch == self.epoch => self.accept_snapshot(window, cx),
            Update::Ui { epoch, effect } if epoch == self.epoch => effect(self, window, cx),
            Update::Recording(id, event) => self.recording_update(id, event, window, cx),
            Update::PersistenceError { epoch, error } if epoch == self.epoch => self.error = error,
            _ => {}
        }
        cx.notify();
    }
    fn accept_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = &self.store else {
            return;
        };
        let snapshot = store.snapshot();
        let changed = !Arc::ptr_eq(&snapshot, &self.snapshot);
        let previous = std::mem::replace(&mut self.snapshot, snapshot);
        if previous.error != self.snapshot.error
            && let Some(error) = &self.snapshot.error
        {
            self.error = error.clone();
        }
        let previous_draft = previous.drafts.get(&previous.navigation.draft_key);
        let sources_changed = previous_draft.map(|draft| &draft.attachments)
            != self
                .snapshot
                .drafts
                .get(self.draft_key())
                .map(|draft| &draft.attachments);
        let navigated = previous.navigation.draft_key != self.snapshot.navigation.draft_key;
        if navigated {
            self.cancel_recording();
            self.composer_pending = None;
            self.history_loading = false;
            self.history_error.clear();
            self.item_details.clear();
            self.rendered = None;
            self.diffs.clear();
            self.markdown_cache.clear();
        }
        if (navigated || self.composer_pending.is_none())
            && self.composer_value.as_ref() != self.draft().text
        {
            let value: SharedString = self.draft().text.clone().into();
            self.composer_value = value.clone();
            self.composer
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let model = self.selected_model();
        let efforts = model
            .map(|model| model.supported_reasoning_efforts.as_slice())
            .unwrap_or_default();
        let index = efforts
            .iter()
            .position(|effort| {
                Some(effort.reasoning_effort.as_str()) == self.draft().effort.as_deref()
            })
            .unwrap_or_default();
        let position = (efforts.len(), index);
        if position != self.effort_position {
            self.effort_position = position;
            self.effort_slider.update(cx, |slider, cx| {
                *slider = slider::SliderState::new()
                    .max(position.0.saturating_sub(1).max(1) as f32)
                    .step(1.)
                    .default_value(position.1 as f32);
                cx.notify();
            });
        }
        if let Some(file) = &self.snapshot.workspace.file {
            let changed = self.editor_path.as_deref() != Some(&file.path);
            if changed || self.editor_pending.is_none() {
                let text = self
                    .snapshot
                    .file_drafts
                    .get(&file.path)
                    .map_or(&file.text, |draft| &draft.text);
                if changed || self.editor_value.as_ref() != text {
                    let value: SharedString = text.clone().into();
                    self.editor_path = Some(file.path.clone());
                    self.editor_value = value.clone();
                    self.editor_pending = None;
                    self.editor_input
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                }
            }
        }
        if self.snapshot.workspace.file.is_none() && self.editor_path.take().is_some() {
            self.editor_pending = None;
            self.editor_value = SharedString::default();
            self.editor_input
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if !self.worktree_dirty
            && !self.worktree_saving
            && previous.workspace.settings != self.snapshot.workspace.settings
            && let Some(settings) = &self.snapshot.workspace.settings
        {
            self.worktree_copy_paths.update(cx, |input, cx| {
                input.set_value(settings.copy_paths.join("\n"), window, cx)
            });
            self.worktree_directory.update(cx, |input, cx| {
                input.set_value(settings.worktree_directory.clone(), window, cx)
            });
        }
        if !Arc::ptr_eq(&previous.requests, &self.snapshot.requests) {
            self.sync_request_inputs(window, cx);
        }
        if navigated
            || !Arc::ptr_eq(&previous.conversations, &self.snapshot.conversations)
            || !Arc::ptr_eq(
                &previous.pending_submissions,
                &self.snapshot.pending_submissions,
            )
            || !Arc::ptr_eq(&previous.requests, &self.snapshot.requests)
        {
            self.sync_rows(navigated, window, cx);
        }
        let user_items_changed = {
            let mut current = self.user_items();
            !self.source_items.iter().all(|previous| {
                current
                    .next()
                    .is_some_and(|item| Arc::ptr_eq(previous, item))
            }) || current.next().is_some()
        };
        if sources_changed || user_items_changed || navigated {
            let mut paths: std::collections::BTreeSet<&str> = self
                .draft()
                .attachments
                .iter()
                .map(|attachment| attachment.path.as_str())
                .collect();
            for item in self.user_items() {
                if let Some(parts) = item.extra.get("content").and_then(Value::as_array) {
                    paths.extend(
                        parts
                            .iter()
                            .filter_map(|part| part.get("path").and_then(Value::as_str)),
                    );
                }
            }
            self.source_paths = paths.into_iter().map(str::to_owned).collect();
            self.source_items = self.user_items().cloned().collect();
        }
        if previous.workspace.directory != self.snapshot.workspace.directory
            && let Some(directory) = &self.snapshot.workspace.directory
        {
            let previous_path = previous
                .workspace
                .directory
                .as_ref()
                .map_or("", |directory| directory.path.as_str());
            if self.path.read(cx).value().as_ref() == previous_path {
                self.path.update(cx, |input, cx| {
                    input.set_value(directory.path.clone(), window, cx)
                });
            }
        }
        let cwd_changed = previous.navigation.cwd != self.snapshot.navigation.cwd;
        if cwd_changed {
            let cwd = self.snapshot.navigation.cwd.clone();
            self.path
                .update(cx, |input, cx| input.set_value(cwd, window, cx));
        }
        if changed && let Some(persistence) = &self.persistence {
            persistence.send_replace(self.snapshot.clone());
        }
    }
    fn draft_key(&self) -> &str {
        &self.snapshot.navigation.draft_key
    }
    fn selected(&self) -> &str {
        self.snapshot
            .navigation
            .thread_id
            .as_deref()
            .unwrap_or_default()
    }
    fn thread(&self) -> Option<&Arc<Thread>> {
        self.snapshot.conversations.get(self.selected())
    }
    fn draft(&self) -> &Draft {
        static EMPTY: Draft = Draft {
            text: String::new(),
            attachments: Vec::new(),
            model: None,
            effort: None,
            service_tier: None,
        };
        self.snapshot
            .drafts
            .get(self.draft_key())
            .map_or(&EMPTY, Arc::as_ref)
    }
    fn selected_model(&self) -> Option<&Model> {
        self.snapshot
            .models
            .iter()
            .find(|model| Some(model.model.as_str()) == self.draft().model.as_deref())
            .or_else(|| {
                self.snapshot
                    .models
                    .iter()
                    .find(|model| model.is_default == Some(true))
            })
            .or_else(|| self.snapshot.models.first())
    }
    fn active_turn(&self) -> Option<&Turn> {
        self.thread()?
            .turns
            .as_ref()?
            .iter()
            .rev()
            .find(|turn| turn.status.as_deref() == Some("inProgress"))
            .map(Arc::as_ref)
    }
    fn remote_key(&self) -> &str {
        self.remote.as_ref().map_or("local", |remote| &remote.id)
    }
    fn older_page(&self) -> Option<(Option<String>, Option<String>)> {
        let thread = self.thread()?;
        if let Some(turn) = thread
            .turns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .find(|turn| turn.items_has_more == Some(true))
        {
            return Some((
                Some(turn.id.clone()),
                turn.items_next_cursor.clone().flatten(),
            ));
        }
        thread
            .history_cursor
            .as_ref()?
            .as_ref()
            .filter(|cursor| !cursor.is_empty())
            .map(|cursor| (None, Some(cursor.clone())))
    }
    fn user_items(&self) -> impl Iterator<Item = &Arc<Item>> {
        self.thread()
            .and_then(|thread| thread.turns.as_deref())
            .unwrap_or_default()
            .iter()
            .flat_map(|turn| turn.items.as_deref().unwrap_or_default())
            .filter(|item| item.kind.as_deref() == Some("userMessage"))
    }
    fn pending_rows(&self) -> impl Iterator<Item = (&String, &Arc<PendingSubmission>)> {
        let turns = self
            .thread()
            .and_then(|thread| thread.turns.as_deref())
            .unwrap_or_default();
        self.snapshot
            .pending_submissions
            .iter()
            .filter(move |(_, pending)| {
                pending.draft_key == self.draft_key()
                    && !pending
                        .turn_id
                        .as_ref()
                        .is_some_and(|id| turns.iter().any(|turn| &turn.id == id))
            })
    }
    fn visible_requests(&self) -> impl Iterator<Item = (&String, &Arc<ServerRequest>)> {
        self.snapshot.requests.iter().filter(|(_, request)| {
            request
                .params
                .get("threadId")
                .and_then(Value::as_str)
                .is_none_or(|id| id == self.selected())
        })
    }
    fn sync_rows(&mut self, reset: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut rows = Vec::new();
        self.rendered = self.thread().map(|thread| {
            agent_core::presentation::conversation::project_conversation(
                &self.snapshot,
                thread.clone(),
                &self.rendered,
            )
        });
        if let Some(rendered) = &self.rendered {
            rows.push(ConversationRow::History);
            rows.extend(rendered.turns.iter().cloned().map(ConversationRow::Turn));
        }
        rows.extend(
            self.pending_rows()
                .map(|(id, pending)| ConversationRow::Pending(id.clone(), pending.clone())),
        );
        rows.extend(
            self.visible_requests()
                .map(|(id, request)| ConversationRow::Request(id.clone(), request.clone())),
        );
        if reset {
            self.list.reset(rows.len());
            self.list.scroll_to_end();
            self.rows = rows;
            return;
        }
        let old = &self.rows;
        let prefix = old
            .iter()
            .zip(&rows)
            .take_while(|(a, b)| a.same_identity(b))
            .count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(rows[prefix..].iter().rev())
            .take_while(|(a, b)| a.same_identity(b))
            .count();
        let anchor = self.list.logical_scroll_top();
        let preserve = old
            .get(anchor.item_ix)
            .zip(rows.get(anchor.item_ix))
            .filter(|(a, b)| a.same_identity(b) && !a.unchanged(b))
            .and_then(|(a, b)| {
                if let (ConversationRow::Turn(turn), ConversationRow::Turn(next)) = (a, b)
                    && let Some(first) = turn.source.items.as_ref().and_then(|items| items.first())
                    && next.source.items.as_ref().is_some_and(|items| {
                        items
                            .iter()
                            .position(|item| item.id == first.id)
                            .is_some_and(|index| index > 0)
                    })
                {
                    Some((
                        turn.source.id.clone(),
                        self.list.bounds_for_item(anchor.item_ix)?.size.height,
                    ))
                } else {
                    None
                }
            });
        if prefix + suffix != old.len() || old.len() != rows.len() {
            self.list
                .splice(prefix..old.len() - suffix, rows.len() - prefix - suffix);
        }
        for index in 0..prefix {
            if !old[index].unchanged(&rows[index]) {
                self.list.remeasure_items(index..index + 1);
            }
        }
        for offset in 0..suffix {
            let index = rows.len() - suffix + offset;
            if !old[old.len() - suffix + offset].unchanged(&rows[index]) {
                self.list.remeasure_items(index..index + 1);
            }
        }
        self.rows = rows;
        if self.history_loading
            && let Some((id, old_height)) = preserve
        {
            let generation = self.snapshot.epoch;
            let owner = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| { let _ = owner.update(cx, |view, cx| {
                if view.snapshot.epoch == generation && matches!(view.rows.get(anchor.item_ix), Some(ConversationRow::Turn(turn)) if turn.source.id == id)
                    && let Some(bounds) = view.list.bounds_for_item(anchor.item_ix)
                {
                    view.list.scroll_to(ListOffset { item_ix: anchor.item_ix, offset_in_item: anchor.offset_in_item + bounds.size.height - old_height }); cx.notify();
                }
            }); });
        }
    }
    fn pause_tail(&self) {
        if self.list.logical_scroll_top().item_ix == self.list.item_count() {
            self.list
                .scroll_by(-self.list.viewport_bounds().size.height);
        }
        self.list.pause_following_tail();
    }
    fn remeasure_item(&self, id: &str) {
        for (index, row) in self.rows.iter().enumerate() {
            if let ConversationRow::Turn(turn) = row
                && (turn.source.id == id
                    || turn
                        .source
                        .items
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|item| item.id == id))
            {
                self.list.remeasure_items(index..index + 1);
            }
        }
    }
    fn new_chat(&mut self, cwd: String) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        self.dispatch(Intent::NewChat { cwd });
    }
    fn open_chat(&mut self, id: String) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        self.busy += 1;
        self.perform(Intent::OpenThread { id }, |view, result, window, cx| {
            view.busy = view.busy.saturating_sub(1);
            if let Err(error) = result {
                view.error = error;
            }
            view.accept_snapshot(window, cx);
        });
    }
    fn older(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.history_loading {
            return;
        }
        let Some((turn_id, cursor)) = self.older_page() else {
            return;
        };
        let generation = self.snapshot.epoch;
        self.history_loading = true;
        self.history_error.clear();
        self.list.remeasure_items(0..1);
        self.perform(
            Intent::ReadOlder {
                thread_id: self.selected().into(),
                turn_id,
                cursor,
            },
            move |view, result, window, cx| {
                if view.snapshot.epoch != generation {
                    return;
                }
                view.accept_snapshot(window, cx);
                view.history_loading = false;
                if let Err(error) = result {
                    view.history_error = error;
                }
                view.list.remeasure_items(0..1);
            },
        );
        cx.notify();
    }
    fn detail(&mut self, turn_id: String, item_id: String) {
        let key = (turn_id.clone(), item_id.clone());
        if self
            .item_details
            .get(&key)
            .is_some_and(|detail| detail.loading)
        {
            return;
        }
        let needed = self
            .thread()
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| turns.iter().find(|turn| turn.id == turn_id))
            .and_then(|turn| turn.deferred_item_ids.as_ref())
            .is_some_and(|ids| ids.contains(&item_id));
        if !needed {
            return;
        }
        self.item_details.insert(
            key.clone(),
            DetailLoad {
                loading: true,
                error: None,
            },
        );
        let generation = self.snapshot.epoch;
        self.perform(
            Intent::ReadItem {
                thread_id: self.selected().into(),
                turn_id,
                item_id,
            },
            move |view, result, window, cx| {
                if view.snapshot.epoch != generation {
                    return;
                }
                view.item_details.insert(
                    key.clone(),
                    DetailLoad {
                        loading: false,
                        error: result.err(),
                    },
                );
                view.accept_snapshot(window, cx);
                view.remeasure_item(&key.0);
            },
        );
    }
    fn switch_host(
        &mut self,
        remote: Option<RemoteHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.remote == remote && self.snapshot.connected {
            return;
        }
        self.cancel_recording();
        self.dictation = None;
        self.close();
        self.remote = remote;
        self.snapshot = Arc::default();
        self.composer_pending = None;
        self.editor_pending = None;
        self.editor_path = None;
        self.worktree_dirty = false;
        self.worktree_saving = false;
        self.worktree_save_pending = false;
        self.side_chat = None;
        self.terminal = None;
        self.images.clear();
        self.image_gallery = None;
        self.rows.clear();
        self.list.reset(0);
        self.rendered = None;
        self.item_details.clear();
        self.requests.clear();
        self.diffs.clear();
        self.markdown_cache.clear();
        self.composer_value = "".into();
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.connect();
    }
    fn send(&mut self, cx: &Context<Self>) {
        if !self.snapshot.connected
            || self.busy > 0
            || self.dictation.as_ref().is_some_and(|dictation| {
                matches!(dictation.phase, Phase::Permission | Phase::Transcribing)
            })
        {
            return;
        }
        if self
            .dictation
            .as_ref()
            .is_some_and(|dictation| dictation.phase == Phase::Recording)
        {
            self.finish_dictation(true, cx);
            return;
        }
        if self.composer.read(cx).value().trim().is_empty() && self.draft().attachments.is_empty() {
            return;
        }
        self.busy += 1;
        self.perform(
            Intent::Submit {
                thread_id: None,
                client_user_message_id: uuid::Uuid::new_v4().to_string(),
            },
            |view, result, window, cx| {
                view.busy = view.busy.saturating_sub(1);
                if let Err(error) = result {
                    view.error = error;
                }
                view.accept_snapshot(window, cx);
            },
        );
    }
    fn refresh_threads(&self) {
        self.dispatch(Intent::ListThreads {
            query: (*self.snapshot.list_query).clone(),
        });
    }
    fn refresh_review(&self) {
        if !self.snapshot.navigation.cwd.is_empty() {
            self.dispatch(Intent::ReviewWorkspace {
                cwd: self.snapshot.navigation.cwd.clone(),
            });
        }
    }
    fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        let result = match panel {
            Panel::SideChat if self.side_chat.is_none() => {
                self.side_chat = Some(cx.new(|cx| {
                    Self::new(
                        Mode::SideChat {
                            remote: self.remote.clone(),
                            cwd: self.snapshot.navigation.cwd.clone(),
                        },
                        window,
                        cx,
                    )
                }));
                Ok(())
            }
            Panel::Terminal if self.terminal.is_none() => crate::terminal::Terminal::new(
                self.remote.as_ref().map_or("", |remote| &remote.ticket),
                self.snapshot.navigation.cwd.clone(),
                window,
                cx,
            )
            .map(|view| self.terminal = Some(view)),
            Panel::Browser if self.browser.is_none() => {
                crate::browser::Browser::new(window, cx).map(|view| self.browser = Some(view))
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.error = error;
            return;
        }
        self.panel = panel;
        self.panel_open = true;
        self.tab = Tab::Chat;
        cx.notify();
    }
    fn pick_folder(&mut self) {
        self.busy += 1;
        self.effect(
            async {
                tokio::task::spawn_blocking(platform::choose_folder)
                    .await
                    .map_err(|error| error.to_string())
            },
            |view, result, _, _| {
                view.busy = view.busy.saturating_sub(1);
                match result {
                    Ok(Some(path)) => view.new_chat(path.to_string_lossy().into_owned()),
                    Ok(None) => {}
                    Err(error) => view.error = error,
                }
            },
        );
    }
    fn composer_enter(
        &mut self,
        action: &gpui_kit::component::input::Enter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let submit = self.composer.update(cx, |input, cx| {
            composer_should_submit(input, action, window, cx)
        });
        if submit && self.snapshot.connected && self.busy == 0 {
            cx.stop_propagation();
            self.send(cx);
        }
    }
    fn paste_image(
        &mut self,
        _: &gpui_kit::component::input::Paste,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if !item
            .entries()
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
        {
            return;
        }
        cx.stop_propagation();
        if self.busy > 0 {
            return;
        }
        self.attach_sources(move || {
            let directory = platform::state_dir()?.join("attachments");
            item.into_entries()
                .filter_map(|entry| match entry {
                    ClipboardEntry::Image(image) => {
                        Some(clipboard::save_image(&directory, image).map(PathBuf::from))
                    }
                    _ => None,
                })
                .collect()
        });
        cx.notify();
    }
    fn attach(&mut self) {
        self.attach_sources(|| Ok(platform::choose_files().unwrap_or_default()));
    }
    fn attach_sources(
        &mut self,
        sources: impl FnOnce() -> Result<Vec<PathBuf>, String> + Send + 'static,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let key = self.draft_key().to_owned();
        let remote = self.remote.is_some();
        let directory = self.snapshot.navigation.cwd.clone();
        self.busy += 1;
        self.effect(
            async move {
                let paths = tokio::task::spawn_blocking(sources)
                    .await
                    .map_err(|error| error.to_string())??;
                for path in paths {
                    let is_image = path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .and_then(image::ImageFormat::from_extension)
                        .is_some();
                    let name = path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned();
                    let attachment = Attachment {
                        path: path.to_string_lossy().into_owned(),
                        name,
                        is_image,
                    };
                    let intent = if remote {
                        Intent::UploadAttachment {
                            draft_key: key.clone(),
                            attachment,
                            directory: directory.clone(),
                        }
                    } else {
                        Intent::AddAttachment {
                            draft_key: key.clone(),
                            attachment,
                        }
                    };
                    store
                        .dispatch(intent)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            },
            |view, result, window, cx| {
                view.busy = view.busy.saturating_sub(1);
                if let Err(error) = result {
                    view.error = error;
                }
                view.accept_snapshot(window, cx);
            },
        );
    }
    fn download(&mut self, source: String) {
        let Some(store) = self.store.clone() else {
            return;
        };
        self.busy += 1;
        self.effect(
            async move {
                let name = Path::new(&source)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let destination =
                    tokio::task::spawn_blocking(move || platform::choose_destination(&name))
                        .await
                        .map_err(|error| error.to_string())?;
                if let Some(destination) = destination {
                    store
                        .dispatch(Intent::DownloadFile {
                            source,
                            destination: destination
                                .into_os_string()
                                .into_string()
                                .map_err(|_| "download destination is not UTF-8")?,
                        })
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            },
            |view, result, _, _| {
                view.busy = view.busy.saturating_sub(1);
                if let Err(error) = result {
                    view.error = error;
                }
            },
        );
    }
    fn browse(&mut self, path: String) {
        self.panel = Panel::Files;
        self.panel_open = true;
        self.tab = Tab::Chat;
        self.dispatch(Intent::ListFiles { path });
    }
    fn edit(&mut self, path: String, discard_draft: bool) {
        self.dispatch(Intent::ReadFile {
            path,
            discard_draft,
        });
    }
    fn save_file(&mut self) {
        let Some(path) = self.editor_path.clone() else {
            return;
        };
        self.busy += 1;
        self.perform(Intent::SaveFile { path }, |view, result, window, cx| {
            view.busy = view.busy.saturating_sub(1);
            if let Err(error) = result {
                view.error = error;
            }
            view.accept_snapshot(window, cx);
        });
    }
    fn settings_match_inputs(&self, cx: &App) -> bool {
        self.snapshot
            .workspace
            .settings
            .as_ref()
            .is_some_and(|settings| {
                self.worktree_directory.read(cx).value().trim() == settings.worktree_directory
                    && self
                        .worktree_copy_paths
                        .read(cx)
                        .value()
                        .lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty())
                        .eq(settings.copy_paths.iter().map(String::as_str))
            })
    }
    fn save_worktree_settings(&mut self, toggle: Option<(bool, bool)>, cx: &Context<Self>) {
        let Some(settings) = &self.snapshot.workspace.settings else {
            return;
        };
        let mut settings: WorktreeSettings = settings.as_ref().clone();
        if let Some((create, checked)) = toggle {
            if create {
                settings.create_on_new_session = checked;
            } else {
                settings.copy_on_create = checked;
            }
        }
        settings.worktree_directory = self.worktree_directory.read(cx).value().trim().into();
        settings.copy_paths = self
            .worktree_copy_paths
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        if self.snapshot.workspace.settings.as_deref() == Some(&settings) {
            return;
        }
        if self.worktree_saving {
            self.worktree_save_pending = true;
            return;
        }
        self.worktree_saving = true;
        self.worktree_saved = false;
        self.perform(
            Intent::UpdateWorktreeSettings { settings },
            |view, result, window, cx| {
                view.worktree_saving = false;
                if let Err(error) = result {
                    view.error = error;
                }
                view.accept_snapshot(window, cx);
                view.worktree_dirty = !view.settings_match_inputs(cx);
                view.worktree_saved = !view.worktree_dirty;
                if std::mem::take(&mut view.worktree_save_pending) {
                    view.save_worktree_settings(None, cx);
                }
            },
        );
    }
    fn respond(&mut self, key: String, id: Value, answer: Answer) {
        if let Some(inputs) = self.requests.get_mut(&key) {
            inputs.sent = true;
        }
        self.perform(
            Intent::Respond {
                request_id: id,
                answer,
            },
            move |view, result, window, cx| {
                if let Err(error) = result {
                    view.error = error;
                    if let Some(inputs) = view.requests.get_mut(&key) {
                        inputs.sent = false;
                    }
                }
                view.accept_snapshot(window, cx);
                view.list.remeasure();
            },
        );
    }
}
fn toggle_set(set: &mut HashSet<String>, key: &str) {
    if !set.remove(key) {
        set.insert(key.into());
    }
}
fn fenced(text: &str, language: &str) -> String {
    let longest = text
        .lines()
        .map(|line| line.chars().take_while(|c| *c == '`').count())
        .max()
        .unwrap_or(0)
        .max(2)
        + 1;
    let fence = "`".repeat(longest);
    format!("{fence}{language}\n{text}\n{fence}")
}
fn literal(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\`*_{}[]<>()#+-.!|>~".contains(c) {
            result.push('\\');
        }
        if c == '\n' {
            result.push_str("  ");
        }
        result.push(c);
    }
    result
}
fn composer_should_submit(
    input: &TextareaState,
    action: &gpui_kit::component::input::Enter,
    window: &mut Window,
    cx: &mut Context<TextareaState>,
) -> bool {
    !action.shift
        && !action.secondary
        && input.selected_range().is_empty()
        && input.cursor() == input.text().len()
        && input.marked_text_range(window, cx).is_none()
}

#[cfg(test)]
mod composer_tests {
    use super::{TextareaState, composer_should_submit};
    use gpui_kit as gpui;
    use gpui_kit::{EntityInputHandler, TestAppContext};
    #[gpui::test]
    fn enter_submits_only_committed_text_at_the_end(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let input = cx.add_window(TextareaState::new);
        input
            .update(cx, |input, window, cx| {
                let enter = gpui_kit::component::input::Enter {
                    secondary: false,
                    shift: false,
                };
                input.set_value("日本語🙂", window, cx);
                let end = input.text().len();
                input.set_selected_range(end..end, cx);
                assert!(composer_should_submit(input, &enter, window, cx));
                input.set_selected_range(3..3, cx);
                assert!(!composer_should_submit(input, &enter, window, cx));
                input.set_selected_range(0..end, cx);
                assert!(!composer_should_submit(input, &enter, window, cx));
                input.set_selected_range(end..end, cx);
                assert!(!composer_should_submit(
                    input,
                    &gpui_kit::component::input::Enter {
                        shift: true,
                        secondary: false
                    },
                    window,
                    cx
                ));
                input.replace_and_mark_text_in_range(None, "変換", Some(2..2), window, cx);
                assert!(!composer_should_submit(input, &enter, window, cx));
                input.unmark_text(window, cx);
                assert!(composer_should_submit(input, &enter, window, cx));
            })
            .unwrap();
    }
}

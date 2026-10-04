use agent_core::state::operations as op;
mod clipboard;
mod completions;
mod dictation;
mod hosts;
mod selection;
mod view;

use crate::{Runtime, diff::DiffView, platform, store_session::StoreSession};
use agent_core::{
    presentation::conversation::{ActivityExpansion, ConversationRowContent},
    state::{Attachment, Draft, DraftKey, Intent, ModelDefaultsScope, PendingSubmission, Snapshot},
    store::Outcome,
};
use agent_protocol::{
    ids::{ItemId, RequestId, TurnId},
    models::{Item, Model, RemoteHost, Thread, Turn, WorktreeSettings},
    requests::Answer,
    session::SessionRef,
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
use hosts::{ConnectionLayout, HostEvent, Hosts};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

enum OperationCompletion {
    Busy,
    Composer(u64),
    Editor(u64),
    Item { generation: u64, turn_id: TurnId },
    WorktreeSettings,
    Request(RequestId),
    Dictation(uuid::Uuid),
    RemoveWorktree,
    Account,
    Gallery(uuid::Uuid),
}
enum Update {
    Connected(Result<(StoreSession, PathBuf), String>),
    Snapshot,
    Completed(OperationCompletion, Result<Outcome, String>),
    Folder(Result<Option<PathBuf>, String>),
    Image {
        key: String,
        result: Result<String, String>,
    },
    Recording(uuid::Uuid, platform::RecordingEvent),
    PersistenceError(String),
    OnboardingCompleted(Result<(), String>),
}
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Chat,
    Settings,
}
#[derive(Clone, Copy, PartialEq)]
enum SettingsPage {
    Models,
    Agents,
    Connections,
    Worktrees,
}
enum WorktreeToggle {
    Create(bool),
    Copy(bool),
    DeleteMerged(bool),
}
#[derive(Clone, Copy, PartialEq)]
enum ModelPanel {
    Models,
    Accounts,
    Manage,
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
    definition: agent_protocol::requests::Question,
    input: Entity<InputState>,
    selected: HashSet<String>,
}
struct RequestInputs {
    questions: Vec<Question>,
    response: Entity<TextareaState>,
    sent: bool,
}
struct ImageGallery {
    zoom: f32,
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
#[derive(Clone)]
enum ConversationRow {
    Turn(Arc<agent_core::presentation::conversation::RenderedTurn>),
    Pending(String, Arc<PendingSubmission>),
    Request(Box<agent_core::presentation::conversation::Request>),
}
impl ConversationRow {
    fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Turn(a), Self::Turn(b)) => a.source.id == b.source.id,
            (Self::Pending(a, _), Self::Pending(b, _)) => a == b,
            (Self::Request(a), Self::Request(b)) => a.id == b.id,
            _ => false,
        }
    }
    fn unchanged(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Turn(a), Self::Turn(b)) => Arc::ptr_eq(a, b),
            (Self::Pending(a, x), Self::Pending(b, y)) => a == b && Arc::ptr_eq(x, y),
            (Self::Request(a), Self::Request(b)) => a == b,
            _ => false,
        }
    }
}

/// Business state is owned by Store. Everything else here is a widget, render
/// cache, pending UI effect, or immutable snapshot retained for display.
pub(crate) struct Desktop {
    session: Option<StoreSession>,
    snapshot: Arc<Snapshot>,
    runtime: Runtime,
    updates: async_channel::Sender<(u64, Update)>,
    epoch: u64,
    connecting: bool,
    remote: Option<RemoteHost>,
    hosts: Option<Entity<Hosts>>,
    side_chat_mode: bool,
    onboarding: bool,
    initial_cwd: Option<String>,
    busy: usize,
    error: String,
    composer: Entity<TextareaState>,
    selection: Entity<selection::ConversationSelection>,
    pending_quote: Option<String>,
    pending_explanation: Option<String>,
    composer_value: SharedString,
    composer_revision: u64,
    completion_index: usize,
    completion_dismissed: bool,
    composer_pending: Option<u64>,
    editor_input: Entity<EditorState>,
    editor_value: SharedString,
    editor_path: Option<String>,
    editor_revision: u64,
    editor_pending: Option<u64>,
    search: Entity<InputState>,
    path: Entity<InputState>,
    model_panel: ModelPanel,
    model_search: Entity<InputState>,
    model_provider: Option<agent_protocol::session::ProviderKind>,
    settings_model_scope: ModelDefaultsScope,
    account_sign_out: Option<String>,
    account_login_draft: Option<DraftKey>,
    worktree_copy_paths: Entity<TextareaState>,
    worktree_directory: Entity<InputState>,
    worktree_dirty: bool,
    worktree_saved: bool,
    worktree_saving: bool,
    worktree_save_pending: bool,
    worktree_removal: Option<String>,
    worktree_busy: bool,
    account_code: Entity<InputState>,
    account_busy: bool,
    account_polling: bool,
    expanded_projects: HashSet<String>,
    expanded_items: HashSet<String>,
    expanded_work: HashMap<String, ActivityExpansion>,
    tab: Tab,
    settings_page: SettingsPage,
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
    requests: HashMap<RequestId, RequestInputs>,
    list: ListState,
    hovered_conversation_marker: Option<usize>,
    rows: Vec<ConversationRow>,
    rendered: Option<Arc<agent_core::presentation::conversation::RenderedConversation>>,
    diffs: HashMap<String, Entity<DiffView>>,
    images: HashMap<String, ImageState>,
    image_gallery: Option<ImageGallery>,
    image_dir: tempfile::TempDir,
    markdown_cache: HashMap<String, MarkdownContent>,
    _subscriptions: Vec<Subscription>,
}
impl Desktop {
    fn set_error(&mut self, error: String) {
        tracing::error!(target: "bex", operation = "desktop", message = %error);
        self.error = error;
    }
    pub(crate) fn new(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        StoreSession::on_app_quit(cx, |view| &mut view.session);
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
        cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                if view
                    .update(cx, |view, cx| {
                        if view.account_polling
                            && !view.account_busy
                            && view.snapshot.connected
                            && let Some(login) = &view.snapshot.account.login
                        {
                            let id = login.login_id.clone();
                            let provider = login.provider;
                            view.account_busy = true;
                            view.perform(
                                Intent::ReadAccountLogin(op::ReadAccountLogin {
                                    provider,
                                    id,
                                    thread_id: view.account_login_draft.clone(),
                                }),
                                OperationCompletion::Account,
                            );
                        }
                        if let Some(id) = view.thread().and_then(|thread| thread.active_turn_id()) {
                            view.remeasure_item(&id);
                            cx.notify();
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
                .placeholder("AI に依頼する")
                .auto_grow(2, 8)
        });
        let editor_input = cx.new(|cx| EditorState::new(window, cx));
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("モデルを検索"));
        let account_code = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("認証コードを貼り付け")
                .masked(true)
        });
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
        let hosts = (!side_chat_mode).then(|| cx.new(|cx| Hosts::new(window, cx)));
        let selection = cx.new(|_| selection::ConversationSelection::new(!side_chat_mode));
        let mut subscriptions = vec![
            cx.subscribe_in(
                &selection,
                window,
                |view, _, action, window, cx| match action {
                    selection::SelectionAction::AddToChat(text) => {
                        view.quote_selection(text, window, cx)
                    }
                    selection::SelectionAction::AskSideChat(text) => {
                        view.open_panel(Panel::SideChat, window, cx);
                        if let Some(chat) = &view.side_chat {
                            chat.update(cx, |chat, cx| chat.quote_selection(text, window, cx));
                        }
                    }
                    selection::SelectionAction::Explain(text) => {
                        if view.side_chat_mode {
                            view.explain_selection(text, cx);
                        } else {
                            view.open_panel(Panel::SideChat, window, cx);
                            if let Some(chat) = &view.side_chat {
                                chat.update(cx, |chat, cx| chat.explain_selection(text, cx));
                            }
                        }
                    }
                },
            ),
            cx.subscribe(&composer, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    if value != view.composer_value {
                        view.completion_index = 0;
                        view.completion_dismissed = false;
                        let cursor = input.read(cx).cursor();
                        view.composer_value = value.clone();
                        view.composer_revision += 1;
                        let revision = view.composer_revision;
                        view.composer_pending = Some(revision);
                        view.perform(
                            Intent::EditComposer {
                                thread_id: view.draft_key().clone(),
                                text: value.to_string(),
                                cursor: cursor as u32,
                            },
                            OperationCompletion::Composer(revision),
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
                            OperationCompletion::Editor(revision),
                        );
                    }
                }
            }),
            cx.subscribe(&model_search, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&account_code, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&search, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    if value.as_ref() != view.snapshot.list_query.search_term {
                        let mut query = (*view.snapshot.list_query).clone();
                        query.search_term = value.to_string();
                        view.dispatch(Intent::ListSessions(op::ListSessions::new(query)));
                    }
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
            subscriptions.push(cx.observe(hosts, |_, _, cx| cx.notify()));
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
        let mut view = Self {
            session: None,
            snapshot: Arc::default(),
            runtime: cx.global::<Runtime>().clone(),
            updates,
            epoch: 0,
            connecting: false,
            remote,
            hosts,
            side_chat_mode,
            onboarding: !side_chat_mode
                && !platform::state_dir()
                    .is_ok_and(|directory| directory.join("onboarding.completed").is_file()),
            initial_cwd,
            busy: 0,
            error: String::new(),
            composer,
            selection,
            pending_quote: None,
            pending_explanation: None,
            composer_value: "".into(),
            composer_revision: 0,
            completion_index: 0,
            completion_dismissed: false,
            composer_pending: None,
            editor_input,
            editor_value: "".into(),
            editor_path: None,
            editor_revision: 0,
            editor_pending: None,
            search,
            path,
            model_panel: ModelPanel::Models,
            model_search,
            model_provider: None,
            settings_model_scope: ModelDefaultsScope::Global,
            account_sign_out: None,
            account_login_draft: None,
            worktree_copy_paths,
            worktree_directory,
            worktree_dirty: false,
            worktree_saved: false,
            worktree_saving: false,
            worktree_save_pending: false,
            worktree_removal: None,
            worktree_busy: false,
            account_code,
            account_busy: false,
            account_polling: false,
            expanded_projects: HashSet::new(),
            expanded_items: HashSet::new(),
            expanded_work: HashMap::new(),
            tab: Tab::Chat,
            settings_page: SettingsPage::Agents,
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
            hovered_conversation_marker: None,
            rows: Vec::new(),
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
        view.connect(None);
        view
    }
    fn connect(&mut self, preferences: Option<Vec<u8>>) {
        self.epoch += 1;
        self.connecting = true;
        self.busy = 0;
        self.worktree_removal = None;
        self.worktree_busy = false;
        self.account_busy = false;
        self.account_polling = false;
        self.model_provider = None;
        self.account_sign_out = None;
        self.account_login_draft = None;
        self.error = self.runtime.logging_error.clone().unwrap_or_default();
        let epoch = self.epoch;
        let remote = self.remote.clone();
        let side = self.side_chat_mode;
        let initial_cwd = self.initial_cwd.take();
        let updates = self.updates.clone();
        let connections = self.runtime.connections.clone();
        let runtime = self.runtime.clone();
        self.runtime.handle.spawn(async move {
            let result = async {
                let host = if let Some(remote) = &remote {
                    remote
                        .ticket
                        .parse::<agent_transport::transport::Ticket>()
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
                    Ok(bytes) => agent_core::persistence::decode(&bytes)
                        .map_err(|error| format!("保存した入力状態を読み込めません: {error}"))?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        Snapshot::default()
                    }
                    Err(error) => return Err(error.to_string()),
                };
                let preferences = match preferences {
                    Some(preferences) => Some(preferences),
                    None => {
                        match tokio::fs::read(path.with_file_name("model-preferences.json")).await {
                            Ok(bytes) => Some(bytes),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                            Err(error) => return Err(error.to_string()),
                        }
                    }
                };
                if let Some(preferences) = preferences {
                    let saved = agent_core::persistence::encode(&snapshot)
                        .map_err(|error| error.to_string())?;
                    snapshot = agent_core::persistence::decode(
                        &agent_core::persistence::apply_model_preferences(&saved, &preferences)
                            .map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
                }
                let initial_cwd = initial_cwd.or_else(|| {
                    snapshot
                        .navigation
                        .thread_id
                        .is_none()
                        .then(|| snapshot.navigation.cwd.clone())
                });
                let store = connections
                    .connect(
                        remote.as_ref().map(|remote| remote.ticket.as_str()),
                        snapshot,
                    )
                    .await
                    .map_err(|error| format!("{error:#}"))?;
                if let Some(cwd) = initial_cwd {
                    drop(store.dispatch(Intent::NewChat { cwd }));
                }
                Ok::<_, String>((store, path))
            }
            .await;
            match result {
                Ok((store, path)) => {
                    StoreSession::publish(
                        Ok(store),
                        runtime,
                        updates,
                        move |session| {
                            (
                                epoch,
                                Update::Connected(session.map(|session| (session, path))),
                            )
                        },
                        move |_| (epoch, Update::Snapshot),
                    )
                    .await;
                }
                Err(error) => {
                    let _ = updates.send((epoch, Update::Connected(Err(error)))).await;
                }
            }
        });
    }
    fn effect<T: Send + 'static>(
        &self,
        future: impl Future<Output = Result<T, String>> + Send + 'static,
        complete: impl FnOnce(Result<T, String>) -> Update + Send + 'static,
    ) {
        let epoch = self.epoch;
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            let completion = complete(future.await);
            let _ = updates.send((epoch, completion)).await;
        });
    }
    fn perform(&self, intent: Intent, completion: OperationCompletion) {
        let Some(store) = self.session.as_ref().map(|session| &session.store) else {
            return;
        };
        let receipt = store.dispatch(intent);
        self.effect(
            async move { receipt.await.map_err(|error| error.to_string()) },
            move |result| Update::Completed(completion, result),
        );
    }
    fn dispatch(&self, intent: Intent) {
        if let Some(session) = &self.session {
            drop(session.store.dispatch(intent));
        }
    }
    fn receive(
        &mut self,
        (epoch, update): (u64, Update),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if epoch != self.epoch {
            return;
        }
        match update {
            Update::Connected(result) => {
                self.connecting = false;
                match result {
                    Ok((mut session, path)) => {
                        session.persist(path, self.updates.clone(), move |error| {
                            (epoch, Update::PersistenceError(error))
                        });
                        self.session = Some(session);
                        self.accept_snapshot(window, cx);
                        if let Some(text) = self.pending_quote.take() {
                            self.quote_selection(&text, window, cx);
                        }
                        if let Some(text) = self.pending_explanation.take() {
                            self.explain_selection(&text, cx);
                        }
                        self.dispatch(Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}));
                        if self.tab == Tab::Settings {
                            self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
                        }
                    }
                    Err(error) => self.set_error(error),
                }
            }
            Update::Snapshot => self.accept_snapshot(window, cx),
            Update::Completed(kind, result) => self.operation_completed(kind, result, window, cx),
            Update::Recording(id, event) => self.recording_update(id, event),
            Update::PersistenceError(error) => self.set_error(error),
            Update::OnboardingCompleted(result) => match result {
                Ok(()) => self.onboarding = false,
                Err(error) => self.set_error(error),
            },
            Update::Folder(result) => {
                self.busy = self.busy.saturating_sub(1);
                match result {
                    Ok(Some(path)) => {
                        self.tab = Tab::Chat;
                        self.cancel_recording();
                        self.dispatch(Intent::AddProject(op::AddProject {
                            cwd: path.to_string_lossy().into_owned(),
                        }));
                    }
                    Ok(None) => {}
                    Err(error) => self.set_error(error),
                }
            }
            Update::Image { key, result } => {
                if let Some(image) = self.images.get_mut(&key) {
                    match result {
                        Ok(path) => {
                            image.path = Some(if Path::new(&path).is_absolute() {
                                PathBuf::from(path).into()
                            } else {
                                ImageSource::from(path)
                            })
                        }
                        Err(error) => {
                            tracing::error!(target: "bex", operation = "image.load", message = %error);
                            image.error = Some(error);
                        }
                    }
                }
                self.list.remeasure();
            }
        }
        cx.notify();
    }
    fn operation_completed(
        &mut self,
        kind: OperationCompletion,
        result: Result<Outcome, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            OperationCompletion::Composer(revision) => {
                if self.composer_pending == Some(revision) {
                    self.composer_pending = None;
                }
            }
            OperationCompletion::Editor(revision) => {
                if self.editor_pending == Some(revision) {
                    self.editor_pending = None;
                }
            }
            OperationCompletion::Busy => {
                self.busy = self.busy.saturating_sub(1);
            }
            OperationCompletion::Item {
                generation,
                turn_id,
            } => {
                if self.snapshot.epoch != generation {
                    return;
                }
                self.accept_snapshot(window, cx);
                self.remeasure_item(&turn_id);
                return;
            }
            OperationCompletion::WorktreeSettings => {
                self.worktree_saving = false;
                if let Err(error) = result {
                    self.set_error(error);
                }
                self.accept_snapshot(window, cx);
                self.worktree_dirty = !self.settings_match_inputs(cx);
                self.worktree_saved = !self.worktree_dirty;
                if std::mem::take(&mut self.worktree_save_pending) {
                    self.save_worktree_settings(None, cx);
                }
                return;
            }
            OperationCompletion::Request(key) => {
                if let Err(error) = result {
                    self.set_error(error);
                    if let Some(inputs) = self.requests.get_mut(&key) {
                        inputs.sent = false;
                    }
                }
                self.accept_snapshot(window, cx);
                self.list.remeasure();
                return;
            }
            OperationCompletion::Account => {
                self.account_busy = false;
                if let Err(error) = result {
                    self.account_polling = false;
                    self.set_error(error);
                    self.dispatch(Intent::ListAccounts(op::ListAccounts {}));
                } else {
                    self.accept_snapshot(window, cx);
                    self.account_polling = self.snapshot.account.login.is_some();
                }
                if self.snapshot.account.login.is_none() {
                    self.account_login_draft = None;
                }
                return;
            }
            OperationCompletion::RemoveWorktree => {
                self.worktree_busy = false;
                match result {
                    Ok(_) => self.worktree_removal = None,
                    Err(error) => {
                        self.set_error(error);
                        self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
                    }
                }
                self.accept_snapshot(window, cx);
                return;
            }
            OperationCompletion::Dictation(id) => {
                if self.dictation.as_ref().is_some_and(|state| state.id == id) {
                    self.dictation = None;
                }
            }
            OperationCompletion::Gallery(id) => {
                let Some(gallery) = self
                    .image_gallery
                    .as_mut()
                    .filter(|gallery| gallery.id == id)
                else {
                    return;
                };
                gallery.loading = false;
                match result {
                    Ok(Outcome::SessionImages { images }) => {
                        let initial = gallery.current_image().clone();
                        let entries: Vec<_> = images
                            .into_iter()
                            .map(|image| (Arc::new(image.source), image.encoded))
                            .collect();
                        gallery.selected = entries.iter().position(|entry| entry == &initial);
                        gallery
                            .list
                            .splice(0..gallery.list.item_count(), entries.len());
                        gallery.entries = entries;
                        if let Some(index) = gallery.selected {
                            gallery.list.scroll_to_reveal_item(index);
                        }
                    }
                    Err(error) => {
                        tracing::error!(target: "bex", operation = "gallery.load", message = %error);
                        gallery.error = error;
                    }
                    _ => unreachable!("session image outcome"),
                }
                return;
            }
        }
        if let Err(error) = result {
            self.set_error(error);
        }
        self.accept_snapshot(window, cx);
    }
    fn accept_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.session.as_ref().map(|session| &session.store) else {
            return;
        };
        let snapshot = store.snapshot();
        let changed = !Arc::ptr_eq(&snapshot, &self.snapshot);
        let previous = std::mem::replace(&mut self.snapshot, snapshot);
        if previous.account.login.as_ref().map(|login| &login.login_id)
            != self
                .snapshot
                .account
                .login
                .as_ref()
                .map(|login| &login.login_id)
        {
            self.account_code
                .update(cx, |input, cx| input.set_value("", window, cx));
        }
        if let Some(request) = self
            .snapshot
            .permission_control(self.draft_key())
            .load_request
        {
            self.dispatch(Intent::ReadPermissionSettings(request));
        }
        if previous.error != self.snapshot.error
            && let Some(error) = &self.snapshot.error
        {
            tracing::error!(target: "bex", operation = "store", message = %error);
        }
        sync_error_banner(
            &mut self.error,
            previous.error.as_deref(),
            self.snapshot.error.as_deref(),
        );
        let previous_draft = previous.drafts.get(&previous.navigation.draft_key);
        let sources_changed = previous_draft.map(|draft| &draft.attachments)
            != self
                .snapshot
                .drafts
                .get(self.draft_key())
                .map(|draft| &draft.attachments);
        let navigated = previous.navigation.draft_key != self.snapshot.navigation.draft_key;
        let project_for_selected = |snapshot: &Snapshot| {
            snapshot
                .threads
                .as_ref()?
                .data
                .iter()
                .find(|thread| thread.id == snapshot.navigation.thread_id)?
                .project_id
                .as_ref()
                .cloned()
        };
        if (navigated || project_for_selected(&previous) != project_for_selected(&self.snapshot))
            && let Some(project) = project_for_selected(&self.snapshot)
        {
            self.expanded_projects.insert(project);
        }
        if navigated {
            self.selection
                .update(cx, |selection, cx| selection.clear(cx));
            self.cancel_recording();
            self.composer_pending = None;
            self.rendered = None;
            self.diffs.clear();
            self.markdown_cache.clear();
        }
        if self.composer_pending.is_none() && self.composer_value.as_ref() != self.draft().text {
            let value: SharedString = self.draft().text.clone().into();
            self.composer_value = value.clone();
            self.composer
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let file = self.snapshot.workspace.file.as_ref();
        let path = file.map(|file| file.path.as_str());
        let file_changed = self.editor_path.as_deref() != path;
        let text = file.map_or("", |file| {
            self.snapshot
                .file_drafts
                .get(&file.path)
                .map_or(file.text.as_str(), |draft| draft.text.as_str())
        });
        if file_changed || (self.editor_pending.is_none() && self.editor_value.as_ref() != text) {
            self.editor_path = path.map(str::to_owned);
            self.editor_value = text.to_owned().into();
            self.editor_pending = None;
            self.editor_input.update(cx, |input, cx| {
                input.set_value(self.editor_value.clone(), window, cx)
            });
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
        let conversations_changed =
            !Arc::ptr_eq(&previous.conversations, &self.snapshot.conversations);
        if conversations_changed {
            self.sync_request_inputs(window, cx);
        }
        if navigated
            || conversations_changed
            || !Arc::ptr_eq(
                &previous.pending_submissions,
                &self.snapshot.pending_submissions,
            )
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
                if let agent_protocol::items::ItemBody::UserMessage { content, .. } = item.body() {
                    paths.extend(content.iter().filter_map(|part| match part {
                        agent_protocol::items::MessagePart::Image { source }
                            if !source.starts_with("data:") =>
                        {
                            Some(source.as_str())
                        }
                        agent_protocol::items::MessagePart::Attachment { path, .. }
                        | agent_protocol::items::MessagePart::Invocation { path, .. } => {
                            Some(path.as_str())
                        }
                        _ => None,
                    }));
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
        if changed && let Some(session) = &self.session {
            session.save(self.snapshot.clone());
        }
    }
    fn draft_key(&self) -> &DraftKey {
        &self.snapshot.navigation.draft_key
    }
    fn selected(&self) -> Option<&SessionRef> {
        self.snapshot.navigation.thread_id.as_ref()
    }
    fn thread(&self) -> Option<&Arc<Thread>> {
        self.selected()
            .and_then(|id| self.snapshot.conversations.get(id))
    }
    fn draft(&self) -> &Draft {
        static EMPTY: Draft = Draft {
            invocations: Vec::new(),
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
            .find(|model| Some(&model.model) == self.draft().model.as_ref())
    }
    fn remote_key(&self) -> &str {
        self.remote.as_ref().map_or("local", |remote| &remote.id)
    }
    fn image_key(&self, source: &str, encoded: bool) -> String {
        format!(
            "{}:{}:{encoded}:{source}",
            self.remote_key(),
            self.snapshot.navigation.cwd
        )
    }
    fn history_key(&self) -> Option<op::OperationKey> {
        self.selected()
            .cloned()
            .map(|session| op::OperationKey::History { session })
    }
    fn history_loading(&self) -> bool {
        self.history_key()
            .is_some_and(|key| self.snapshot.operation_running(key))
    }
    fn history_error(&self) -> Option<String> {
        self.history_key()
            .and_then(|key| self.snapshot.operation_error(key))
    }
    fn has_older_history(&self) -> bool {
        self.thread()
            .is_some_and(|thread| thread.history_has_more == Some(true))
    }
    fn user_items(&self) -> impl Iterator<Item = &Arc<Item>> {
        self.thread()
            .and_then(|thread| thread.turns.as_deref())
            .unwrap_or_default()
            .iter()
            .flat_map(|turn| turn.items.as_deref().unwrap_or_default())
            .filter(|item| {
                matches!(
                    item.body(),
                    agent_protocol::items::ItemBody::UserMessage { .. }
                )
            })
    }
    fn sync_rows(&mut self, reset: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.rendered = self.snapshot.conversation_thread().map(|thread| {
            agent_core::presentation::conversation::project_conversation(
                &self.snapshot,
                thread,
                &self.rendered,
            )
        });
        let rows = conversation_rows(&self.rendered);
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
        if let Some((id, old_height)) = preserve {
            let generation = self.snapshot.epoch;
            let owner = cx.entity().downgrade();
            window.on_next_frame(move |_, cx| { let _ = owner.update(cx, |view, cx| {
                if view.snapshot.epoch == generation && !view.list.is_following_tail() && matches!(view.rows.get(anchor.item_ix), Some(ConversationRow::Turn(turn)) if turn.source.id == id)
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
                && (turn.source.id.as_str() == id
                    || turn
                        .source
                        .items
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|item| item.id.as_str() == id))
            {
                self.list.remeasure_items(index..index + 1);
            }
        }
    }
    fn new_chat(&mut self, cwd: String, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        self.dispatch(Intent::NewChat { cwd });
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
    }
    fn open_chat(&mut self, id: SessionRef, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        self.dispatch(Intent::ReadThread(op::ReadThread::open(id)));
        self.composer.read(cx).focus_handle(cx).focus(window, cx);
    }
    fn load_visible_history(&mut self, cx: &mut Context<Self>) {
        let viewport = self.list.viewport_bounds();
        let oldest_visible = viewport.size.height > px(0.)
            && (self.rows.is_empty()
                || self.list.bounds_for_item(0).is_some_and(|bounds| {
                    bounds.top() >= viewport.top() - viewport.size.height * 0.6
                        && bounds.top() <= viewport.bottom()
                }));
        if agent_core::presentation::conversation::should_load_history(
            self.has_older_history(),
            self.history_loading() || self.history_error().is_some(),
            oldest_visible,
            self.list.is_scrolled_to_end() != Some(false),
            self.list.is_following_tail(),
        ) {
            self.dispatch(Intent::ReadOlder {
                thread_id: self.selected().expect("selected conversation").clone(),
            });
            cx.notify();
        }
    }
    fn detail(&mut self, turn_id: TurnId, item_id: ItemId) {
        let Some(thread_id) = self.selected().cloned() else {
            return;
        };
        let read = op::ReadItem {
            thread_id,
            turn_id,
            item_id,
        };
        if self
            .snapshot
            .operation_running(op::OperationKey::Item { item: read.clone() })
        {
            return;
        }
        let needed = self
            .thread()
            .and_then(|thread| thread.turns.as_ref())
            .and_then(|turns| turns.iter().find(|turn| turn.id == read.turn_id))
            .and_then(|turn| turn.items.as_ref())
            .and_then(|items| items.iter().find(|item| item.id == read.item_id))
            .is_some_and(|item| item.is_deferred());
        if !needed {
            return;
        }
        let generation = self.snapshot.epoch;
        let turn_id = read.turn_id.clone();
        self.perform(
            Intent::ReadItem(read),
            OperationCompletion::Item {
                generation,
                turn_id,
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
        let preferences = self
            .session
            .as_ref()
            .map(|session| {
                agent_core::persistence::encode_model_preferences(&session.store.snapshot())
            })
            .transpose();
        let preferences = match preferences {
            Ok(preferences) => preferences,
            Err(error) => {
                self.error = error.to_string();
                return;
            }
        };
        self.session.take();
        self.remote = remote;
        self.settings_model_scope = ModelDefaultsScope::Global;
        self.snapshot = Arc::default();
        self.composer_pending = None;
        self.pending_quote = None;
        self.pending_explanation = None;
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
        self.requests.clear();
        self.diffs.clear();
        self.markdown_cache.clear();
        self.composer_value = "".into();
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.connect(preferences);
    }
    fn send(&mut self, cx: &Context<Self>) {
        if !self.snapshot.connected || self.busy > 0 {
            return;
        }
        if let Some(dictation) = &self.dictation {
            if dictation.phase == Phase::Recording {
                self.finish_dictation(true);
            }
            return;
        }
        if self.composer.read(cx).value().trim().is_empty() && self.draft().attachments.is_empty() {
            return;
        }
        self.busy += 1;
        self.perform(
            Intent::Submit {
                thread_id: None,
                client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
            },
            OperationCompletion::Busy,
        );
    }
    fn quote_selection(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.is_none() {
            let queued = self.pending_quote.get_or_insert_with(String::new);
            if !queued.is_empty() {
                queued.push_str("\n\n");
            }
            queued.push_str(text);
            return;
        }
        selection::append_to_composer(&self.composer, text, window, cx);
    }

    fn explain_selection(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(store) = self.session.as_ref().map(|session| session.store.clone()) else {
            self.pending_explanation = Some(text.into());
            return;
        };
        let cwd = self.snapshot.selected_directory();
        let provider = self
            .snapshot
            .model_provider_for_draft(self.draft_key().clone());
        let prompt = format!("次の選択範囲について詳しく説明してください。\n\n{text}");
        self.busy += 1;
        self.effect(
            async move {
                let Outcome::StartedThread { id } = store
                    .dispatch(Intent::CreateSession(op::CreateSession {
                        provider,
                        cwd: Some(cwd),
                        model: None,
                    }))
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Err("新しいサイドチャットを作成できませんでした".into());
                };
                // The new thread has its own draft; existing side-chat input stays intact.
                store
                    .dispatch(Intent::SetDraftText {
                        thread_id: id.clone().into(),
                        text: prompt,
                    })
                    .await
                    .map_err(|error| error.to_string())?;
                store
                    .dispatch(Intent::ReadThread(op::ReadThread::open(id.clone())))
                    .await
                    .map_err(|error| error.to_string())?;
                store
                    .dispatch(Intent::Submit {
                        thread_id: Some(id),
                        client_user_message_id: uuid::Uuid::new_v4().to_string().into(),
                    })
                    .await
                    .map_err(|error| error.to_string())
            },
            |result| Update::Completed(OperationCompletion::Busy, result),
        );
        cx.notify();
    }

    fn refresh_threads(&self) {
        self.dispatch(Intent::ListSessions(op::ListSessions::new(
            (*self.snapshot.list_query).clone(),
        )));
    }
    fn refresh_review(&self) {
        if !self.snapshot.selected_directory().is_empty() {
            self.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
                cwd: self.snapshot.navigation.cwd.clone(),
            }));
        }
    }
    fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        let result = match panel {
            Panel::SideChat if self.side_chat.is_none() => {
                self.side_chat = Some(cx.new(|cx| {
                    Self::new(
                        Mode::SideChat {
                            remote: self.remote.clone(),
                            cwd: self.snapshot.selected_directory(),
                        },
                        window,
                        cx,
                    )
                }));
                Ok(())
            }
            Panel::Terminal if self.terminal.is_none() => {
                self.terminal = Some(crate::terminal::Terminal::new(
                    self.remote.as_ref().map_or("", |remote| &remote.ticket),
                    self.snapshot.navigation.cwd.clone(),
                    window,
                    cx,
                ));
                Ok(())
            }
            Panel::Browser if self.browser.is_none() => crate::browser::Browser::new(
                wry::WebViewBuilder::new(),
                #[cfg(target_os = "macos")]
                crate::browser::ChromeProfileSource::default(),
                window,
                cx,
            )
            .map(|view| self.browser = Some(view)),
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.set_error(error);
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
            Update::Folder,
        );
    }
    fn composer_arrow(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.completion_key(key, window, cx) {
            return;
        }
        let moved = self.composer.update(cx, |input, cx| {
            if input.marked_text_range(window, cx).is_some() || !input.selected_range().is_empty() {
                return false;
            }
            let cursor = input.cursor();
            let target = if key == "up" { 0 } else { input.text().len() };
            // Off-screen offsets can be clamped to the first visible row by the input.
            if input.value()[cursor.min(target)..cursor.max(target)].contains('\n') {
                return false;
            }
            // Compare rendered rows so soft-wrapped text keeps normal vertical movement.
            let Some(caret) = input.range_to_bounds(&(cursor..cursor)) else {
                return false;
            };
            let Some(edge) = input.range_to_bounds(&(target..target)) else {
                return false;
            };
            if caret.origin.y != edge.origin.y {
                return false;
            }
            input.set_selected_range(target..target, cx);
            true
        });
        if moved {
            cx.stop_propagation();
        }
    }
    fn composer_enter(
        &mut self,
        action: &gpui_kit::component::input::Enter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !action.shift && !action.secondary && self.accept_completion(window, cx) {
            cx.stop_propagation();
            return;
        }
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
        if self.busy > 0 || self.dictation.is_some() {
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
        let Some(store) = self.session.as_ref().map(|session| session.store.clone()) else {
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
                        Intent::UploadAttachment(op::UploadAttachment {
                            draft_key: key.clone(),
                            attachment,
                            directory: directory.clone(),
                        })
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
                Ok(Outcome::Applied)
            },
            |result| Update::Completed(OperationCompletion::Busy, result),
        );
    }
    fn download(&mut self, source: String) {
        let Some(store) = self.session.as_ref().map(|session| session.store.clone()) else {
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
                        .dispatch(Intent::DownloadFile(op::DownloadFile {
                            source,
                            destination: destination
                                .into_os_string()
                                .into_string()
                                .map_err(|_| "download destination is not UTF-8")?,
                        }))
                        .await
                        .map_err(|error| error.to_string())?;
                }
                Ok(Outcome::Applied)
            },
            |result| Update::Completed(OperationCompletion::Busy, result),
        );
    }
    fn browse(&mut self, path: String) {
        self.panel = Panel::Files;
        self.panel_open = true;
        self.tab = Tab::Chat;
        self.dispatch(Intent::ListFiles(op::ListFiles { path }));
    }
    fn edit(&mut self, path: String, discard_draft: bool) {
        self.dispatch(Intent::ReadFile(op::ReadFile {
            path,
            discard_draft,
        }));
    }
    fn save_file(&mut self) {
        let Some(path) = self.editor_path.clone() else {
            return;
        };
        self.busy += 1;
        self.perform(
            Intent::SaveFile(op::SaveFile { path }),
            OperationCompletion::Busy,
        );
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
    fn save_worktree_settings(&mut self, toggle: Option<WorktreeToggle>, cx: &Context<Self>) {
        let Some(settings) = &self.snapshot.workspace.settings else {
            return;
        };
        let mut settings: WorktreeSettings = settings.as_ref().clone();
        if let Some(toggle) = toggle {
            match toggle {
                WorktreeToggle::Create(checked) => settings.create_on_new_session = checked,
                WorktreeToggle::Copy(checked) => settings.copy_on_create = checked,
                WorktreeToggle::DeleteMerged(checked) => settings.delete_merged = checked,
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
            Intent::UpdateWorktreeSettings(op::UpdateWorktreeSettings { settings }),
            OperationCompletion::WorktreeSettings,
        );
    }
    fn respond(&mut self, id: RequestId, answer: Answer) {
        if let Some(inputs) = self.requests.get_mut(&id) {
            inputs.sent = true;
        }
        self.perform(
            Intent::Respond(op::Respond {
                request_id: id.clone(),
                answer,
            }),
            OperationCompletion::Request(id),
        );
    }
}
fn conversation_rows(
    rendered: &Option<Arc<agent_core::presentation::conversation::RenderedConversation>>,
) -> Vec<ConversationRow> {
    let mut rows = Vec::new();
    if let Some(rendered) = rendered {
        rows.extend(rendered.turns.iter().cloned().map(ConversationRow::Turn));
        rows.extend(rendered.queued.iter().filter_map(|item| {
            if let agent_core::presentation::conversation::ItemSource::Pending(id, pending) =
                &item.source
            {
                Some(ConversationRow::Pending(id.clone(), pending.clone()))
            } else {
                None
            }
        }));
    }
    rows.extend(
        rendered
            .iter()
            .flat_map(|conversation| &conversation.request_rows)
            .filter_map(|row| {
                if let ConversationRowContent::PendingRequest { request } = &row.content {
                    Some(ConversationRow::Request(request.clone()))
                } else {
                    None
                }
            }),
    );
    rows
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
fn sync_error_banner(banner: &mut String, previous: Option<&str>, next: Option<&str>) {
    if previous != next {
        if let Some(error) = next {
            *banner = error.to_owned();
        } else if previous == Some(banner.as_str()) {
            banner.clear();
        }
    }
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

#[cfg(test)]
mod error_tests {
    use super::sync_error_banner;

    #[test]
    fn recovered_store_error_clears_its_banner() {
        let mut banner = String::new();
        sync_error_banner(&mut banner, None, Some("thread read failed"));
        assert_eq!(banner, "thread read failed");
        sync_error_banner(&mut banner, Some("thread read failed"), None);
        assert!(banner.is_empty());
    }

    #[test]
    fn recovery_preserves_a_newer_local_error_and_respects_dismissal() {
        let mut banner = "draft save failed".to_owned();
        sync_error_banner(&mut banner, Some("thread read failed"), None);
        assert_eq!(banner, "draft save failed");
        banner.clear();
        sync_error_banner(
            &mut banner,
            Some("thread read failed"),
            Some("thread read failed"),
        );
        assert!(banner.is_empty());
        sync_error_banner(
            &mut banner,
            Some("thread read failed"),
            Some("disconnected"),
        );
        assert_eq!(banner, "disconnected");
    }
}

#[cfg(test)]
mod completion_tests {
    use super::{
        Arc, Context, ConversationRow, Desktop, Mode, OperationCompletion, Outcome, Snapshot,
        Thread, Update, Window, conversation_rows,
    };
    use gpui_kit as gpui;
    use gpui_kit::TestAppContext;

    #[gpui::test]
    fn completion_messages_preserve_newer_host_navigation_and_input(cx: &mut TestAppContext) {
        // Do not drive this runtime: this test exercises UI result delivery, not provisioning.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_global(crate::Runtime {
                handle: runtime.handle().clone(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let view = cx.add_window(|window, cx| {
            Desktop::new(
                Mode::SideChat {
                    remote: None,
                    cwd: "/fixture".into(),
                },
                window,
                cx,
            )
        });
        view.update(cx, |view, window, cx| {
            view.epoch = 3;
            Arc::make_mut(&mut view.snapshot).epoch = 5;
            view.busy = 2;
            view.composer_pending = Some(9);
            view.editor_pending = Some(12);
            let deliver = |view: &mut Desktop,
                           host,
                           kind,
                           result,
                           window: &mut Window,
                           cx: &mut Context<Desktop>| {
                view.receive((host, Update::Completed(kind, result)), window, cx);
            };
            deliver(
                view,
                2,
                OperationCompletion::Busy,
                Err("obsolete host".into()),
                window,
                cx,
            );
            assert_eq!(view.busy, 2);
            assert!(view.error.is_empty());
            deliver(
                view,
                3,
                OperationCompletion::Composer(8),
                Ok(Outcome::Applied),
                window,
                cx,
            );
            deliver(
                view,
                3,
                OperationCompletion::Editor(11),
                Ok(Outcome::Applied),
                window,
                cx,
            );
            assert_eq!(view.composer_pending, Some(9));
            assert_eq!(view.editor_pending, Some(12));
            deliver(
                view,
                3,
                OperationCompletion::Composer(9),
                Ok(Outcome::Applied),
                window,
                cx,
            );
            deliver(
                view,
                3,
                OperationCompletion::Editor(12),
                Ok(Outcome::Applied),
                window,
                cx,
            );
            assert_eq!(view.composer_pending, None);
            assert_eq!(view.editor_pending, None);
            deliver(
                view,
                3,
                OperationCompletion::Busy,
                Ok(Outcome::Applied),
                window,
                cx,
            );
            assert_eq!(view.busy, 1);
        })
        .unwrap();
    }

    #[test]
    fn row_projection_keeps_repeated_turns_and_scopes_requests_without_changing_input() {
        let mut source: Arc<Thread> = Arc::new(serde_json::from_value(serde_json::json!({"id":{"provider":"codex","id":"selected"},"turns":[{"id":"repeated","items":[{"id":"first","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"first answer","phase":"unknown"}}}}}],"status":"unknown"},{"id":"repeated","items":[{"id":"second","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"second answer","phase":"unknown"}}}}}],"status":"unknown"}]})).unwrap());
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.navigation).thread_id =
            Some(agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "selected".into(),
            });
        let request = Arc::new(agent_protocol::requests::Request {
            id: "global".into(),
            target: agent_protocol::requests::RequestTarget::Session,
            delivery: agent_protocol::session::RequestDelivery::Awaiting,
            body: agent_protocol::requests::RequestBody::Elicitation {
                server: "fixture".into(),
                message: "input".into(),
                input: agent_protocol::requests::ElicitationInput::Form { fields: vec![] },
            },
        });
        Arc::make_mut(&mut source)
            .requests
            .insert(request.id.clone(), request.clone());
        let mut other = (*source).clone();
        let request = Arc::make_mut(other.requests.get_mut("global").unwrap());
        request.id = "other".into();
        other.id = Some(agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "other".into(),
        });
        Arc::make_mut(&mut snapshot.conversations)
            .insert(other.id.clone().unwrap(), Arc::new(other));
        let before = snapshot.clone();
        let rendered = Some(
            agent_core::presentation::conversation::project_conversation(
                &snapshot,
                source.clone(),
                &None,
            ),
        );
        let rows = conversation_rows(&rendered);
        let repeated = conversation_rows(&rendered);
        assert_eq!(rows.len(), 3);
        for (index, turn) in source.turns.as_ref().unwrap().iter().enumerate() {
            let ConversationRow::Turn(row) = &rows[index] else {
                panic!("missing turn")
            };
            assert!(Arc::ptr_eq(&row.source, turn));
        }
        assert!(
            matches!(&rows[2], ConversationRow::Request(request) if request.id.as_str() == "global")
        );
        assert!(rows.iter().zip(&repeated).all(|(a, b)| a.unchanged(b)));
        assert_eq!(snapshot, before);
    }
}

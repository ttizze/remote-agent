use agent_core::state::operations as op;
mod clipboard;
mod completions;
mod conversation;
mod dictation;
mod hosts;
mod lifecycle;
mod selection;
mod snapshot;
mod view;

use crate::{Runtime, diff::DiffView, platform, store_session::StoreSession};
use agent_core::{
    presentation::conversation::ActivityExpansion,
    state::{Attachment, Draft, DraftKey, Intent, Snapshot},
    store::Outcome,
};
use agent_protocol::{
    ids::{ItemId, RequestId, TurnId},
    models::{Item, Model, RemoteHost, Thread, Turn, WorktreeSettings},
    requests::Answer,
    session::SessionRef,
};
use conversation::ConversationRow;
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
use lifecycle::{OperationCompletion, Update};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Chat,
    Settings,
}
#[derive(Clone, Copy, PartialEq)]
enum SettingsPage {
    Accounts,
    Connections,
    Worktrees,
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
struct DetailLoad {
    loading: bool,
    error: Option<String>,
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
    history_loading: bool,
    history_error: String,
    item_details: HashMap<(TurnId, ItemId), DetailLoad>,
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
                            view.account_busy = true;
                            view.perform(
                                Intent::ReadAccountLogin(op::ReadAccountLogin {
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
            session: None,
            snapshot: Arc::default(),
            runtime: cx.global::<Runtime>().clone(),
            updates,
            epoch: 0,
            connecting: false,
            remote,
            hosts,
            side_chat_mode,
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
            account_code: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("ブラウザに表示された認証コード")
                    .masked(true)
            }),
            account_busy: false,
            account_polling: false,
            expanded_projects: HashSet::new(),
            expanded_items: HashSet::new(),
            expanded_work: HashMap::new(),
            tab: Tab::Chat,
            settings_page: SettingsPage::Accounts,
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
            |result| Update::Completed(OperationCompletion::Download, result),
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

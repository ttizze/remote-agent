use agent_client::conversation::{self, Conversation, array, text};
mod clipboard;
mod drafts;
mod host;
use host::Management;
use std::rc::{Rc, Weak};
mod chat;
mod view;
use chat::ConversationView;
use view::{button, diff, host_menu, icon_button};

use crate::{
    diff::DiffView,
    platform,
    rpc::{self, Rpc},
};
use base64::Engine;
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
use host_protocol::api::{FileEntry, WorkspaceReview, WorktreeSettings};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

type Apply = Box<dyn FnOnce(&mut Desktop, &mut Window, &mut Context<Desktop>) + Send>;
enum Event {
    Done {
        epoch: u64,
        apply: Apply,
        busy: bool,
    },
    Management(host::ManagementInput),
    Catalogue(EntityId, host::CatalogueInput),
    DraftError(drafts::SaveError),
}
#[derive(Clone, Copy, PartialEq)]
enum WorktreeToggle {
    Create(bool),
    Copy(bool),
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
struct OpenFile {
    path: std::path::PathBuf,
    revision: String,
}

struct ReviewSource {
    host: Rc<host::Connection>,
    rpc: Rpc,
    cwd: String,
}
pub(crate) struct Desktop {
    tx: async_channel::Sender<Event>,
    rpc: Rpc,
    manager: Rpc,
    manager_session: Entity<Management>,
    manager_timer: Option<tokio::task::AbortHandle>,
    host_leases: HashMap<String, Weak<host::Connection>>,
    host: Rc<host::Connection>,
    host_subscription: Option<[Subscription; 2]>,
    epoch: u64,
    busy: usize,
    error: String,
    remote: String,
    worktree_settings: Option<WorktreeSettings>,
    worktree_copy_paths: Entity<TextareaState>,
    worktree_directory: Entity<InputState>,
    worktree_saved: bool,
    worktree_saving: bool,
    worktree_save_pending: Option<WorktreeSettings>,
    expanded_projects: HashSet<String>,
    tab: Tab,
    sidebar: bool,
    panel_open: bool,
    panel: Panel,
    side_chat: Option<Entity<ConversationView>>,
    terminal: Option<Entity<crate::terminal::Terminal>>,
    browser: Option<Entity<crate::browser::Browser>>,
    cwd: String,
    search: Entity<InputState>,
    path: Entity<InputState>,
    editor_input: Entity<EditorState>,
    relay_url: Entity<InputState>,
    relay_token: Entity<InputState>,
    runner: Entity<InputState>,
    pairing: Entity<TextareaState>,
    invitation_input: Entity<TextareaState>,
    drafts: Entity<drafts::State>,
    draft_writer: std::sync::mpsc::Sender<drafts::Save>,
    draft_saved: [u64; 2],
    draft_pending: [bool; 2],
    entries: Vec<FileEntry>,
    editor: Option<OpenFile>,
    review: Option<std::sync::Arc<WorkspaceReview>>,
    review_error: String,
    _subscriptions: Vec<Subscription>,
    diffs: HashMap<String, Entity<DiffView>>,
    invitation_file: tempfile::NamedTempFile,
    invitation_qr: Option<ImageSource>,
    chat: Entity<ConversationView>,
    side_subscription: Option<Subscription>,
    review_source: Option<ReviewSource>,
}
impl Desktop {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        let (manager, manager_timer) = host::connect_management(&tx);
        let manager_session = cx.new(|_| Management::default());
        let mut host_leases = HashMap::new();
        let host = host::acquire(String::new(), &mut host_leases, &tx, cx);
        let drafts = cx.new(|_| drafts::read());
        let draft_writer = drafts::writer(tx.clone());
        let chat = cx.new(|cx| {
            ConversationView::new(
                (manager.clone(), manager_session.clone()),
                drafts.clone(),
                host.clone(),
                String::new(),
                drafts::Scope::Main,
                window,
                cx,
            )
        });
        let rpc = host.rpc.clone();
        let remote = String::new();
        let cwd = String::new();
        let error = String::new();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("会話を検索"));
        let path = cx.new(|cx| InputState::new(window, cx).placeholder("絶対パス"));
        let editor_input = cx.new(|cx| EditorState::new(window, cx));
        let relay_url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("relay URL")
                .default_value(
                    std::env::var("BEX_RELAY_URL")
                        .unwrap_or_else(|_| "ws://127.0.0.1:4000/socket/websocket".into()),
                )
        });
        let relay_token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("relay トークン")
                .masked(true)
                .default_value(std::env::var("BEX_RELAY_TOKEN").unwrap_or_default())
        });
        let runner = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("runner ID")
                .default_value(std::env::var("BEX_RUNNER_ID").unwrap_or_default())
        });
        let pairing = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("相手の Mac で発行した招待を貼り付け")
                .auto_grow(3, 6)
        });
        let worktree_copy_paths = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(".env\n.env.local\nconfig/local")
                .auto_grow(3, 8)
        });
        let worktree_directory = cx.new(|cx| {
            InputState::new(window, cx).placeholder("接続先Mac上の絶対パス（空欄で既定）")
        });
        let invitation_input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 6));
        cx.spawn_in(window, async move |owner, cx| {
            while let Ok(event) = rx.recv().await {
                if owner
                    .update_in(cx, |s, w, cx| s.event(event, w, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let subscriptions = vec![
            cx.on_release(|s, cx| {
                let state = s.drafts.update(cx, |state, _| std::mem::take(state));
                for save in drafts::pending_saves(state, s.draft_saved)
                    .into_iter()
                    .flatten()
                {
                    let _ = s.draft_writer.send(save);
                }
            }),
            cx.observe(&drafts, |s, _, cx| {
                for scope in [drafts::Scope::Main, drafts::Scope::SideChat] {
                    let index = scope as usize;
                    if !s.draft_pending[index]
                        && s.drafts.read(cx).revisions[index] != s.draft_saved[index]
                    {
                        s.draft_pending[index] = true;
                        cx.spawn(async move |owner, cx| {
                            cx.background_executor()
                                .timer(std::time::Duration::from_millis(200))
                                .await;
                            let _ = owner.update(cx, |s, cx| s.flush_drafts(scope, cx));
                        })
                        .detach();
                    }
                }
            }),
            cx.observe_in(&chat, window, |s, chat, w, cx| {
                s.sync_main(&chat, w, cx);
                cx.notify();
            }),
            cx.subscribe_in(&chat, window, |s, chat, intent: &chat::Intent, w, cx| {
                match intent {
                    chat::Intent::Selected => s.set_tab(Tab::Chat, cx),
                    chat::Intent::Review => s.show_review(chat, cx),
                    chat::Intent::SelectHost(id) => s.switch_chat_host(chat, id.clone(), w, cx),
                }
                cx.notify();
            }),
            cx.observe(&manager_session, |_, _, cx| cx.notify()),
            cx.subscribe(&drafts, |s, _, error: &drafts::SaveError, cx| {
                if error.scope == drafts::Scope::Main {
                    s.error = error.message.clone();
                    cx.notify();
                }
            }),
            cx.subscribe(&worktree_copy_paths, |s, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    s.worktree_saved = false;
                    cx.notify();
                }
                if matches!(event, InputEvent::Blur) {
                    s.save_worktree_settings(None, cx);
                }
            }),
            cx.subscribe(&worktree_directory, |s, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    s.worktree_saved = false;
                    cx.notify();
                }
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    s.save_worktree_settings(None, cx);
                }
            }),
            cx.subscribe_in(&search, window, |s, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    host::send(
                        &s.host,
                        host::CatalogueInput::Search(input.read(cx).value().to_string()),
                    );
                }
            }),
            cx.subscribe_in(&editor_input, window, |s, input, event, _, cx| {
                if matches!(event, InputEvent::Change)
                    && let Some(editor) = &s.editor
                {
                    let key = s.editor_key(&editor.path);
                    let value =
                        json!({"text":input.read(cx).value().as_ref(),"revision":editor.revision});
                    drafts::update(&s.drafts, drafts::Scope::Main, cx, |mut cache| {
                        cache["files"][key] = value;
                        cache
                    });
                }
            }),
        ];
        let mut desktop = Self {
            tx,
            rpc,
            manager,
            manager_session,
            manager_timer,
            host_leases,
            host,
            host_subscription: None,
            epoch: 1,
            busy: 0,
            error,
            remote,
            worktree_settings: None,
            worktree_copy_paths,
            worktree_directory,
            worktree_saved: false,
            worktree_saving: false,
            worktree_save_pending: None,
            expanded_projects: HashSet::new(),
            tab: Tab::Chat,
            sidebar: true,
            panel_open: false,
            panel: Panel::Home,
            side_chat: None,
            terminal: None,
            browser: None,
            cwd,
            search,
            path,
            editor_input,
            relay_url,
            relay_token,
            runner,
            pairing,
            invitation_input,
            drafts,
            draft_writer,
            draft_saved: [0; 2],
            draft_pending: [false; 2],
            entries: vec![],
            editor: None,
            review: None,
            review_error: String::new(),
            _subscriptions: subscriptions,
            diffs: HashMap::new(),
            invitation_file: tempfile::Builder::new()
                .prefix("bex-invitation-")
                .suffix(".svg")
                .tempfile()
                .expect("invitation temporary file"),
            invitation_qr: None,
            chat,
            side_subscription: None,
            review_source: None,
        };
        desktop.subscribe_host(window, cx);
        desktop
    }
    fn flush_drafts(&mut self, scope: drafts::Scope, cx: &mut App) {
        let index = scope as usize;
        self.draft_pending[index] = false;
        let state = self.drafts.read(cx);
        if state.revisions[index] != self.draft_saved[index] {
            self.draft_saved[index] = state.revisions[index];
            let _ = self.draft_writer.send(drafts::Save {
                scope,
                cache: state.cache(scope).clone(),
            });
        }
    }
    fn work(
        &mut self,
        busy: bool,
        task: impl FnOnce() -> Result<Value, String> + Send + 'static,
        apply: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + Send + 'static,
    ) {
        if busy {
            self.busy += 1;
            self.error.clear();
        }
        let tx = self.tx.clone();
        let epoch = self.epoch;
        std::thread::spawn(move || {
            let result = task();
            let _ = tx.send_blocking(Event::Done {
                epoch,
                apply: Box::new(move |s, w, cx| match result {
                    Ok(value) => apply(s, value, w, cx),
                    Err(error) => s.error = error,
                }),
                busy,
            });
        });
    }

    fn request(
        &mut self,
        manager: bool,
        method: &'static str,
        params: Value,
        busy: bool,
        apply: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + Send + 'static,
    ) {
        self.request_result(
            manager,
            method,
            params,
            busy,
            move |s, result, w, cx| match result {
                Ok(value) => apply(s, value, w, cx),
                Err(error) => s.error = error,
            },
        );
    }

    fn request_result(
        &mut self,
        manager: bool,
        method: &'static str,
        params: Value,
        busy: bool,
        apply: impl FnOnce(&mut Self, Result<Value, String>, &mut Window, &mut Context<Self>)
        + Send
        + 'static,
    ) {
        if busy {
            self.busy += 1;
            self.error.clear();
        }
        let tx = self.tx.clone();
        let epoch = self.epoch;
        let rpc = if manager { &self.manager } else { &self.rpc };
        rpc.request_async(method, params, move |result| {
            let _ = tx.send_blocking(Event::Done {
                epoch,
                apply: Box::new(move |s, w, cx| apply(s, result, w, cx)),
                busy,
            });
        });
    }

    fn agent_request<T, F, Fut>(
        &mut self,
        rpc: Rpc,
        busy: bool,
        operation: F,
        apply: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>)
        + Send
        + 'static,
    ) where
        T: Send + 'static,
        F: FnOnce(agent_client::operations::AgentClient) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<T, agent_client::operations::AgentError>>
            + Send
            + 'static,
    {
        if busy {
            self.busy += 1;
            self.error.clear();
        }
        let tx = self.tx.clone();
        let epoch = self.epoch;
        rpc.agent_async(operation, move |result| {
            let _ = tx.send_blocking(Event::Done {
                epoch,
                busy,
                apply: Box::new(move |s, w, cx| apply(s, result, w, cx)),
            });
        });
    }

    fn editor_key(&self, path: &Path) -> String {
        format!(
            "{}:{}",
            if self.remote.is_empty() {
                "local"
            } else {
                &self.remote
            },
            path.display()
        )
    }

    fn refresh_worktree_settings(&mut self, cx: &App) {
        if !self.host.state.read(cx).connected || self.worktree_saving {
            return;
        }
        self.agent_request(
            self.rpc.clone(),
            false,
            |client| async move { client.worktree_settings().await },
            |s, result, w, cx| {
                let settings = match result {
                    Ok(settings) => settings,
                    Err(error) => {
                        s.error = error.to_string();
                        return;
                    }
                };
                s.worktree_copy_paths.update(cx, |input, cx| {
                    input.set_value(settings.copy_paths.join("\n"), w, cx)
                });
                s.worktree_directory.update(cx, |input, cx| {
                    input.set_value(&settings.worktree_directory, w, cx)
                });
                s.worktree_settings = Some(settings);
                s.worktree_saved = false;
            },
        );
    }

    fn save_worktree_settings(&mut self, toggle: Option<WorktreeToggle>, cx: &mut Context<Self>) {
        if !self.host.state.read(cx).connected {
            return;
        }
        let Some(previous) = self
            .worktree_save_pending
            .as_ref()
            .or(self.worktree_settings.as_ref())
        else {
            return;
        };
        let mut settings = previous.clone();
        match toggle {
            Some(WorktreeToggle::Create(checked)) => settings.create_on_new_session = checked,
            Some(WorktreeToggle::Copy(checked)) => settings.copy_on_create = checked,
            None => {}
        }
        settings.copy_paths = self
            .worktree_copy_paths
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        settings.worktree_directory = self.worktree_directory.read(cx).value().trim().to_owned();
        if settings == *previous {
            return;
        }
        self.worktree_saved = false;
        if self.worktree_saving {
            self.worktree_save_pending = Some(settings);
        } else {
            self.persist_worktree_settings(settings);
        }
    }

    fn persist_worktree_settings(&mut self, settings: WorktreeSettings) {
        self.worktree_saving = true;
        self.error.clear();
        let previous = self.worktree_settings.replace(settings.clone());
        self.agent_request(
            self.rpc.clone(),
            false,
            move |client| async move { client.update_worktree_settings(&settings).await },
            move |s, result, _, cx| {
                s.worktree_saving = false;
                match result {
                    Ok(settings) => {
                        s.worktree_saved = s.worktree_directory.read(cx).value().trim()
                            == settings.worktree_directory
                            && s.worktree_copy_paths
                                .read(cx)
                                .value()
                                .lines()
                                .map(str::trim)
                                .filter(|line| !line.is_empty())
                                .eq(settings.copy_paths.iter().map(String::as_str));
                        s.worktree_settings = Some(settings);
                    }
                    Err(error) => {
                        s.worktree_settings = previous;
                        s.error = error;
                    }
                }
                if let Some(pending) = s.worktree_save_pending.take()
                    && Some(&pending) != s.worktree_settings.as_ref()
                {
                    s.worktree_saved = false;
                    s.persist_worktree_settings(pending);
                }
            },
        );
    }

    fn refresh_manager(&self, _: &App) {
        let _ = self
            .tx
            .send_blocking(Event::Management(host::ManagementInput::Refresh));
    }

    fn refresh_threads(&self, _: &mut App) {
        host::send(&self.host, host::CatalogueInput::Refresh);
    }

    fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Chat;
        let result = match panel {
            Panel::SideChat if self.side_chat.is_none() => {
                let chat = cx.new(|cx| {
                    ConversationView::new(
                        (self.manager.clone(), self.manager_session.clone()),
                        self.drafts.clone(),
                        self.host.clone(),
                        self.cwd.clone(),
                        drafts::Scope::SideChat,
                        window,
                        cx,
                    )
                });
                self.side_subscription = Some(cx.subscribe_in(
                    &chat,
                    window,
                    |s, chat, intent: &chat::Intent, w, cx| match intent {
                        chat::Intent::Review => {
                            s.show_review(chat, cx);
                            cx.notify();
                        }
                        chat::Intent::SelectHost(id) => s.switch_chat_host(chat, id.clone(), w, cx),
                        chat::Intent::Selected => {}
                    },
                ));
                self.side_chat = Some(chat);
                Ok(())
            }
            Panel::Terminal if self.terminal.is_none() => {
                if self.cwd.is_empty() {
                    Err("プロジェクトを選択してください".to_owned())
                } else {
                    crate::terminal::Terminal::new(&self.remote, self.cwd.clone(), window, cx)
                        .map(|view| self.terminal = Some(view))
                }
            }
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
        cx.notify();
    }

    fn pick_folder(&mut self) {
        self.work(
            true,
            || platform::choose("folder").map(|v| json!(v)),
            |s, v, w, cx| {
                if let Some(path) = v.as_str() {
                    s.new_thread(path.into(), w, cx);
                }
            },
        );
    }

    fn download(&mut self, source: String) {
        let remote = self.remote.clone();
        let manager = self.manager.clone();
        self.work(true,move||{let Some(destination)=platform::choose("download")? else{return Ok(Value::Null);};if remote.is_empty(){let mut input=std::fs::File::open(source).map_err(|e|e.to_string())?;let mut out=std::fs::OpenOptions::new().write(true).create_new(true).open(&destination).map_err(|e|e.to_string())?;std::io::copy(&mut input,&mut out).map_err(|e|e.to_string())?;}else{manager.request("host/transfer",json!({"profileId":remote,"direction":"download","source":source,"destination":destination}))?;}Ok(Value::Null)},|_,_,_,_|{});
    }

    fn browse(&mut self, path: String) {
        self.tab = Tab::Chat;
        self.panel_open = true;
        self.panel = Panel::Files;
        self.agent_request(
            self.rpc.clone(),
            true,
            move |client| async move { client.list_files(Path::new(&path)).await },
            |s, result, w, cx| {
                let files = match result {
                    Ok(files) => files,
                    Err(error) => {
                        s.error = error;
                        return;
                    }
                };
                s.path.update(cx, |i, cx| {
                    i.set_value(files.path.to_string_lossy().into_owned(), w, cx)
                });
                s.entries = files.entries;
                if files.truncated {
                    s.error =
                        "先頭2,000件を表示しています。下位フォルダを選択してください。".into();
                }
            },
        );
    }

    fn edit(&mut self, path: String, reload: bool) {
        self.agent_request(
            self.rpc.clone(),
            true,
            move |client| async move { client.read_file(Path::new(&path)).await },
            move |s, result, w, cx| {
                let mut document = match result {
                    Ok(document) => document,
                    Err(error) => {
                        s.error = error;
                        return;
                    }
                };
                let key = s.editor_key(&document.path);
                if reload {
                    let scope = drafts::Scope::Main;
                    drafts::update(&s.drafts, scope, cx, |mut cache| {
                        if let Some(files) = cache["files"].as_object_mut() {
                            files.remove(&key);
                        }
                        cache
                    });
                }
                let saved = &s.drafts.read(cx).cache(drafts::Scope::Main)["files"][&key];
                let value = if saved.is_object() {
                    if saved["revision"] != document.revision {
                        s.error =
                            "ホストのファイルが変更されています。下書きを保持しました。".into();
                    }
                    document.revision = text(saved, "revision").to_owned();
                    text(saved, "text").to_owned()
                } else {
                    document.text.into_owned()
                };
                s.editor = Some(OpenFile {
                    path: document.path.into_owned(),
                    revision: document.revision,
                });
                s.editor_input.update(cx, |i, cx| i.set_value(value, w, cx));
            },
        );
    }

    fn save_file(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &self.editor else {
            return;
        };
        let submitted = std::sync::Arc::new(self.editor_input.read(cx).value().to_string());
        let key = self.editor_key(&editor.path);
        let path = editor.path.clone();
        let revision = editor.revision.clone();
        let input = submitted.clone();
        self.agent_request(
            self.rpc.clone(),
            true,
            move |client| async move { client.write_file(&path, &revision, &input).await },
            move |s, result, w, cx| {
                let document = match result {
                    Ok(document) => document,
                    Err(error) => {
                        s.error = error;
                        return;
                    }
                };
                let scope = drafts::Scope::Main;
                drafts::update(&s.drafts, scope, cx, |mut cache| {
                    if cache["files"][&key]["text"].as_str() == Some(submitted.as_str()) {
                        cache["files"].as_object_mut().unwrap().remove(&key);
                    }
                    cache
                });
                if s.editor
                    .as_ref()
                    .is_some_and(|editor| editor.path == *document.path)
                {
                    let unchanged = s.editor_input.read(cx).value().as_ref() == submitted.as_str();
                    s.editor = Some(OpenFile {
                        path: document.path.into_owned(),
                        revision: document.revision,
                    });
                    if unchanged {
                        s.editor_input
                            .update(cx, |i, cx| i.set_value(document.text.into_owned(), w, cx));
                    }
                }
                s.chat.update(cx, |chat, _| chat.refresh_review());
            },
        );
    }

    fn invite(&mut self) {
        let manager = self.manager.clone();
        let destination = self.invitation_file.path().to_owned();
        self.work(
            true,
            move || {
                let invitation = manager.request("host/invite", json!({}))?.to_string();
                let code = qrcode::QrCode::with_error_correction_level(
                    invitation.as_bytes(),
                    qrcode::EcLevel::M,
                )
                .map_err(|e| e.to_string())?;
                std::fs::write(
                    &destination,
                    code.render::<qrcode::render::svg::Color>()
                        .min_dimensions(320, 320)
                        .build(),
                )
                .map_err(|e| e.to_string())?;
                Ok(json!({"invitation":invitation,"path":destination}))
            },
            |s, v, w, cx| {
                s.invitation_input.update(cx, |i, cx| {
                    i.set_value(text(&v, "invitation").to_owned(), w, cx)
                });
                s.invitation_qr = Some(std::path::PathBuf::from(text(&v, "path")).into());
            },
        );
    }

    fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Done { epoch, apply, busy } => {
                if epoch != self.epoch {
                    return;
                }
                if busy {
                    self.busy = self.busy.saturating_sub(1);
                }
                apply(self, window, cx);
            }
            Event::DraftError(error) => {
                self.drafts.update(cx, |_, cx| cx.emit(error));
            }
            Event::Management(input) => {
                let effect = self.manager_session.update(cx, |state, cx| {
                    let (next, effect) = host::reduce_management(std::mem::take(state), input);
                    *state = next;
                    cx.notify();
                    effect
                });
                if let Some(effect) = effect
                    && let Some(error) = host::run_management(effect, &self.manager, &self.tx)
                {
                    self.error = error;
                    self.set_tab(Tab::Settings, cx);
                }
            }
            Event::Catalogue(id, input) => {
                if let Some(connection) = self
                    .host_leases
                    .values()
                    .filter_map(Weak::upgrade)
                    .find(|connection| connection.state.entity_id() == id)
                {
                    host::receive(&connection, input, cx);
                }
            }
        }
        cx.notify();
    }
    fn set_tab(&mut self, tab: Tab, cx: &mut App) {
        self.tab = tab;
        let selected = self.chat.read(cx).session.selected.clone();
        host::send(
            &self.host,
            host::CatalogueInput::Visible(
                (tab == Tab::Chat && !selected.is_empty()).then_some(selected),
            ),
        );
    }
    fn subscribe_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let host = self.host.state.clone();
        self.host_subscription = Some([
            cx.observe(&host, |_, _, cx| cx.notify()),
            cx.subscribe(&host, |s, _, notice: &host::Notice, cx| {
                match notice {
                    host::Notice::Connected(true) => s.refresh_worktree_settings(cx),
                    host::Notice::Error(error) => s.error = error.clone(),
                    _ => {}
                }
                cx.notify();
            }),
        ]);
        if self.host.state.read(cx).connected {
            self.refresh_worktree_settings(cx);
        }
        let search = self.search.read(cx).value().to_string();
        host::send(&self.host, host::CatalogueInput::Search(search));
        self.worktree_copy_paths
            .update(cx, |i, cx| i.set_value("", window, cx));
        self.worktree_directory
            .update(cx, |i, cx| i.set_value("", window, cx));
    }
    fn sync_main(
        &mut self,
        chat: &Entity<ConversationView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = chat.read(cx);
        let changed_host = !Rc::ptr_eq(&state.session.host, &self.host);
        let changed_cwd = state.session.cwd != self.cwd;
        if changed_host {
            self.host = state.session.host.clone();
            self.rpc = self.host.rpc.clone();
            self.remote = state.session.remote.clone();
            self.epoch += 1;
            self.busy = 0;
            self.worktree_settings = None;
            self.worktree_saved = false;
            self.worktree_saving = false;
            self.worktree_save_pending = None;
        }
        if changed_host || changed_cwd {
            self.cwd = chat.read(cx).session.cwd.clone();
            self.side_chat = None;
            self.side_subscription = None;
            self.terminal = None;
            self.entries.clear();
            self.editor = None;
            self.review = None;
            self.review_source = None;
            self.review_error.clear();
            self.panel = Panel::Home;
            self.path
                .update(cx, |i, cx| i.set_value(self.cwd.clone(), window, cx));
        }
        if changed_host {
            self.subscribe_host(window, cx);
        }
    }
    fn new_thread(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.chat
            .update(cx, |chat, cx| chat.new_thread(path, window, cx));
    }
    fn open_thread(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.chat
            .update(cx, |chat, cx| chat.open_thread(id, window, cx));
    }
    fn switch_host(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.switch_chat_host(&self.chat.clone(), id, window, cx);
    }
    fn switch_chat_host(
        &mut self,
        chat: &Entity<ConversationView>,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let connection = host::acquire(id, &mut self.host_leases, &self.tx, cx);
        chat.update(cx, |chat, cx| chat.switch_host(connection, window, cx));
    }
    fn show_review(&mut self, source: &Entity<ConversationView>, cx: &mut Context<Self>) {
        let source = source.read(cx);
        self.review_source = Some(ReviewSource {
            host: source.session.host.clone(),
            rpc: source.session.host.rpc.clone(),
            cwd: source.session.cwd.clone(),
        });
        self.review = source.review.clone();
        self.review_error = source.review_error.clone();
        self.panel = Panel::Diff;
        self.panel_open = true;
        self.set_tab(Tab::Chat, cx);
        self.refresh_review();
    }
    fn refresh_review(&mut self) {
        let Some(source) = &self.review_source else {
            return;
        };
        let rpc = source.rpc.clone();
        let cwd = source.cwd.clone();
        let host = source.host.state.entity_id();
        self.agent_request(
            rpc,
            false,
            move |client| async move {
                let review = client.review_workspace(&cwd).await;
                Ok((cwd, review))
            },
            move |s, result, _, _| match result {
                Ok((cwd, result))
                    if s.review_source.as_ref().is_some_and(|source| {
                        source.host.state.entity_id() == host && source.cwd == cwd
                    }) =>
                {
                    match result {
                        Ok(review) => {
                            s.review = Some(std::sync::Arc::new(review));
                            s.review_error.clear();
                        }
                        Err(error) => {
                            s.review = None;
                            s.review_error = error.to_string();
                        }
                    }
                }
                Err(error) => s.review_error = error,
                _ => {}
            },
        );
    }
}
fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|p| p.to_str())
        .unwrap_or(path)
        .to_owned()
}

fn project_root(project: &Value) -> String {
    project["roots"][0]["path"]
        .as_str()
        .or(project["path"].as_str())
        .or(project["cwd"].as_str())
        .unwrap_or("")
        .into()
}

fn toggle_set(set: &mut HashSet<String>, key: &str) {
    if !set.remove(key) {
        set.insert(key.into());
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        if let Some(timer) = &self.manager_timer {
            timer.abort();
        }
        self.manager.close();
    }
}

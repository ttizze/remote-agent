mod view;

use crate::{
    conversation::{self, Conversation, array, text},
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
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::mpsc,
};

type Apply = Box<dyn FnOnce(&mut Desktop, Value, &mut Window, &mut Context<Desktop>) + Send>;
enum Event {
    Rpc {
        epoch: u64,
        manager: bool,
        event: rpc::Event,
    },
    Done {
        epoch: u64,
        result: Result<Value, String>,
        apply: Apply,
        busy: bool,
    },
    CacheError(String),
    Refresh,
    Persist,
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
    SideChat { remote: String, cwd: String },
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
struct ImageState {
    path: Option<ImageSource>,
    error: Option<String>,
}
struct MarkdownContent {
    source: String,
    rendered: SharedString,
    images: std::rc::Rc<[String]>,
}

pub(crate) struct Desktop {
    tx: async_channel::Sender<Event>,
    rpc: Rpc,
    manager: Rpc,
    epoch: u64,
    connected: bool,
    manager_connected: bool,
    attempted_start: bool,
    busy: usize,
    error: String,
    remote: String,
    hosts: Vec<Value>,
    status: Value,
    worktree_settings: Value,
    worktree_copy_paths: Entity<TextareaState>,
    worktree_saved: bool,
    projects: Vec<Value>,
    threads: Vec<Value>,
    models: Vec<Value>,
    model: String,
    query: Value,
    list_generation: u64,
    load_generation: u64,
    more: Value,
    expanded_projects: HashSet<String>,
    expanded_items: HashSet<String>,
    expanded_work: HashMap<String, bool>,
    tab: Tab,
    sidebar: bool,
    panel_open: bool,
    panel: Panel,
    side_chat_mode: bool,
    side_chat: Option<Entity<Desktop>>,
    terminal: Option<Entity<crate::terminal::Terminal>>,
    browser: Option<Entity<crate::browser::Browser>>,
    cwd: String,
    source_paths: Vec<String>,
    selected: String,
    conversation: Conversation,
    composer: Entity<TextareaState>,
    search: Entity<InputState>,
    path: Entity<InputState>,
    editor_input: Entity<EditorState>,
    relay_url: Entity<InputState>,
    relay_token: Entity<InputState>,
    runner: Entity<InputState>,
    pairing: Entity<TextareaState>,
    invitation_input: Entity<TextareaState>,
    cache: Value,
    cache_dirty: bool,
    save_tx: mpsc::Sender<Value>,
    entries: Vec<Value>,
    editor: Value,
    review: Value,
    review_error: String,
    requests: HashMap<String, RequestInputs>,
    list: ListState,
    _subscriptions: Vec<Subscription>,
    diffs: HashMap<String, Entity<DiffView>>,
    images: HashMap<String, ImageState>,
    image_dir: tempfile::TempDir,
    invitation_file: tempfile::NamedTempFile,
    invitation_qr: Option<ImageSource>,
    markdown_cache: HashMap<String, MarkdownContent>,
}
impl Drop for Desktop {
    fn drop(&mut self) {
        if self.cache_dirty {
            let _ = self.save_tx.send(self.cache.clone());
        }
        self.rpc.close();
        self.manager.close();
    }
}
impl Desktop {
    pub(crate) fn new(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (remote, cwd, side_chat_mode) = match mode {
            Mode::Main => (String::new(), String::new(), false),
            Mode::SideChat { remote, cwd } => (remote, cwd, true),
        };
        let cache_file = if side_chat_mode {
            "desktop-side-drafts.json"
        } else {
            "desktop-drafts.json"
        };
        let (tx, rx) = async_channel::unbounded();
        let manager = Self::connect(&tx, 0, true, "");
        let rpc = Self::connect(&tx, 1, false, &remote);
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Codex に依頼する")
                .auto_grow(2, 8)
        });
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
            TextareaState::new(window, cx).placeholder(".env\n.env.local\nconfig/local").auto_grow(3, 8)
        });
        let invitation_input = cx.new(|cx| TextareaState::new(window, cx).auto_grow(3, 6));
        let mut error = String::new();
        let cache = match std::fs::read(platform::state_dir().join(cache_file)) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) if value.is_object() => {
                    if value.get("messages").is_some() {
                        value
                    } else {
                        json!({"messages":value,"files":{},"attachments":{}})
                    }
                }
                _ => {
                    error = "下書きファイルを読み込めません".into();
                    json!({"messages":{},"files":{},"attachments":{}})
                }
            },
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    error = e.to_string();
                }
                json!({"messages":{},"files":{},"attachments":{}})
            }
        };
        let (save_tx, save_rx) = mpsc::channel();
        let errors = tx.clone();
        std::thread::spawn(move || {
            while let Ok(mut cache) = save_rx.recv() {
                while let Ok(newer) = save_rx.try_recv() {
                    cache = newer;
                }
                if let Err(e) = platform::save_cache(cache_file, &cache) {
                    let _ = errors.send_blocking(Event::CacheError(e));
                }
            }
        });
        let clock = tx.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(4));
                if clock.send_blocking(Event::Refresh).is_err() {
                    break;
                }
            }
        });
        cx.spawn_in(window, async move |view, cx| {
            while let Ok(event) = rx.recv().await {
                if view
                    .update_in(cx, |view, window, cx| view.event(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let subscriptions = vec![
            cx.subscribe(&worktree_copy_paths, |s, _, event, cx| {
                if matches!(event, InputEvent::Change) { s.worktree_saved = false; cx.notify(); }
            }),
            cx.subscribe_in(&composer,window,|this,input,event,_,cx| {if matches!(event,InputEvent::Change){let key=this.draft_key();this.cache["messages"][key]=json!(input.read(cx).value().as_ref());this.persist();cx.notify();}}),
            cx.subscribe_in(&search,window,|this,input,event,_,cx| {if matches!(event,InputEvent::Change){this.query["searchTerm"]=json!(input.read(cx).value().as_ref());this.refresh_threads();}}),
            cx.subscribe_in(&editor_input,window,|this,input,event,_,cx| {if matches!(event,InputEvent::Change) && !this.editor.is_null(){let key=this.editor_key();this.cache["files"][key]=json!({"text":input.read(cx).value().as_ref(),"revision":this.editor["revision"]});this.persist();}}),
        ];
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(FollowMode::Tail);
        let mut desktop = Self {
            tx,
            rpc,
            manager,
            epoch: 1,
            connected: false,
            manager_connected: false,
            attempted_start: false,
            busy: 0,
            error,
            remote,
            hosts: vec![],
            status: Value::Null,
            worktree_settings: Value::Null,
            worktree_copy_paths,
            worktree_saved: false,
            projects: vec![],
            threads: vec![],
            models: vec![],
            model: String::new(),
            query: json!({"titleOnly":true,"projectLimit":10,"chatLimit":5,"projectThreadLimits":{},"searchTerm":""}),
            list_generation: 0,
            load_generation: 0,
            more: Value::Null,
            expanded_projects: HashSet::new(),
            expanded_items: HashSet::new(),
            expanded_work: HashMap::new(),
            tab: Tab::Chat,
            sidebar: true,
            panel_open: false,
            panel: Panel::Home,
            side_chat_mode,
            side_chat: None,
            terminal: None,
            browser: None,
            cwd,
            source_paths: Vec::new(),
            selected: String::new(),
            conversation: Conversation::default(),
            composer,
            search,
            path,
            editor_input,
            relay_url,
            relay_token,
            runner,
            pairing,
            invitation_input,
            cache,
            cache_dirty: false,
            save_tx,
            entries: vec![],
            editor: Value::Null,
            review: Value::Null,
            review_error: String::new(),
            requests: HashMap::new(),
            list,
            _subscriptions: subscriptions,
            diffs: HashMap::new(),
            images: HashMap::new(),
            image_dir: tempfile::Builder::new()
                .prefix("bex-images-")
                .tempdir()
                .expect("image temporary directory"),
            invitation_file: tempfile::Builder::new()
                .prefix("bex-invitation-")
                .suffix(".svg")
                .tempfile()
                .expect("invitation temporary file"),
            invitation_qr: None,
            markdown_cache: HashMap::new(),
        };
        desktop.restore_draft(window, cx);
        desktop
    }
    fn connect(tx: &async_channel::Sender<Event>, epoch: u64, manager: bool, remote: &str) -> Rpc {
        let tx = tx.clone();
        let target = if manager {
            json!({"target":"manager"})
        } else if remote.is_empty() {
            json!({"target":"local"})
        } else {
            json!({"target":"remote","profileId":remote})
        };
        Rpc::connect(
            platform::state_dir().join("host.sock"),
            target,
            move |event| {
                let _ = tx.send_blocking(Event::Rpc {
                    epoch,
                    manager,
                    event,
                });
            },
        )
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
                result,
                apply: Box::new(apply),
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
                result,
                apply: Box::new(apply),
                busy,
            });
        });
    }
    fn persist(&mut self) {
        if self.cache_dirty {
            return;
        }
        self.cache_dirty = true;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let _ = tx.send_blocking(Event::Persist);
        });
    }
    fn draft_key(&self) -> String {
        format!(
            "{}:{}",
            if self.remote.is_empty() {
                "local"
            } else {
                &self.remote
            },
            if self.selected.is_empty() {
                format!("new:{}", self.cwd)
            } else {
                self.selected.clone()
            }
        )
    }
    fn editor_key(&self) -> String {
        format!(
            "{}:{}",
            if self.remote.is_empty() {
                "local"
            } else {
                &self.remote
            },
            text(&self.editor, "path")
        )
    }
    fn refresh_sources(&mut self) {
        let mut sources = HashSet::new();
        for file in array(&self.cache["attachments"][self.draft_key()]) {
            if let Some(path) = file["path"].as_str() {
                sources.insert(path.to_owned());
            }
        }
        for turn in array(&self.conversation.thread["turns"]) {
            for item in array(&turn["items"]) {
                if item["type"] == "userMessage" {
                    for part in array(&item["content"]) {
                        if let Some(path) = part["path"].as_str() {
                            sources.insert(path.to_owned());
                        }
                    }
                }
            }
        }
        self.source_paths = sources.into_iter().collect();
        self.source_paths.sort();
    }
    fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_sources();
        let value = text(&self.cache["messages"], &self.draft_key()).to_owned();
        self.composer
            .update(cx, |state, cx| state.set_value(value, window, cx));
    }
    fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Done {
                epoch,
                result,
                apply,
                busy,
            } => {
                if busy {
                    self.busy = self.busy.saturating_sub(1);
                }
                if epoch == self.epoch {
                    match result {
                        Ok(value) => apply(self, value, window, cx),
                        Err(e) => self.error = e,
                    }
                }
            }
            Event::CacheError(e) => self.error = e,
            Event::Persist => {
                self.cache_dirty = false;
                let _ = self.save_tx.send(self.cache.clone());
            }
            Event::Refresh => {
                if self.manager_connected {
                    self.refresh_manager();
                }
            }
            Event::Rpc {
                epoch,
                manager,
                event,
            } => {
                if !manager && epoch != self.epoch {
                    return;
                }
                match event {
                    rpc::Event::Connected(online, reason) => {
                        if manager {
                            self.manager_connected = online;
                            if online {
                                self.refresh_manager();
                            } else if !self.attempted_start {
                                self.attempted_start = true;
                                self.work(
                                    false,
                                    || {
                                        Ok(match platform::start_host(None) {
                                            Ok(()) => Value::Null,
                                            Err(e) => json!(e),
                                        })
                                    },
                                    |s, v, _, _| {
                                        if let Some(error) = v.as_str() {
                                            s.error = error.into();
                                            s.tab = Tab::Settings;
                                        }
                                    },
                                );
                            }
                        } else {
                            self.connected = online;
                            if online {
                                self.error.clear();
                                self.conversation.requests.clear();
                                self.refresh_threads();
                                self.refresh_models();
                                self.refresh_worktree_settings();
                                if !self.selected.is_empty() {
                                    self.open_thread(self.selected.clone(), window, cx);
                                }
                            } else {
                                self.error = reason;
                            }
                        }
                    }
                    rpc::Event::Message(message) => {
                        if !manager {
                            let completed = message["method"] == "turn/completed";
                            let sources_changed = matches!(
                                text(&message, "method"),
                                "item/started" | "item/completed"
                            ) && message["params"]["item"]["type"]
                                == "userMessage";
                            self.conversation.reduce(message);
                            if sources_changed {
                                self.refresh_sources();
                            }
                            self.sync_requests(window, cx);
                            self.sync_list(false);
                            if completed {
                                self.refresh_review();
                            }
                        }
                    }
                }
            }
        }
        cx.notify();
    }
    fn sync_list(&mut self, reset: bool) {
        let count =
            array(&self.conversation.thread["turns"]).len() + self.visible_requests().count();
        if reset {
            self.list.reset(count);
            self.list.scroll_to_end();
        } else {
            let old = self.list.item_count();
            if old != count {
                self.list
                    .splice(old.min(count)..old, count.saturating_sub(old));
            }
            self.list.remeasure();
        }
    }
    fn visible_requests(&self) -> impl Iterator<Item = &Value> {
        self.conversation.requests.iter().filter(|r| {
            r["params"]["threadId"].is_null()
                || r["params"]["threadId"] == self.conversation.thread["id"]
        })
    }
    fn refresh_worktree_settings(&mut self) {
        if !self.connected { return; }
        self.request(false, "host/worktree/settings/read", json!({}), false, |s, value, w, cx| {
            let paths = array(&value["copyPaths"]).iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n");
            s.worktree_copy_paths.update(cx, |input, cx| input.set_value(paths, w, cx));
            s.worktree_settings = value;
            s.worktree_saved = false;
        });
    }

    fn save_worktree_settings(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.worktree_settings.clone();
        settings["copyPaths"] = json!(self.worktree_copy_paths.read(cx).value().lines()
            .map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>());
        self.request(false, "host/worktree/settings/update", settings, true, |s, value, _, _| {
            s.worktree_settings = value;
            s.worktree_saved = true;
        });
    }

    fn refresh_manager(&mut self) {
        self.request(true, "host/status", json!({}), false, |s, v, _, _| {
            s.status = v
        });
        self.request(true, "host/listRemotes", json!({}), false, |s, v, _, _| {
            s.hosts = v.as_array().cloned().unwrap_or_default()
        });
    }
    fn refresh_threads(&mut self) {
        if !self.connected {
            return;
        }
        self.list_generation += 1;
        let generation = self.list_generation;
        self.request(
            false,
            "host/thread/list",
            self.query.clone(),
            false,
            move |s, v, _, _| {
                if generation != s.list_generation {
                    return;
                }
                s.projects = array(&v["projects"]).to_vec();
                s.threads = array(&v["data"]).to_vec();
                s.more = v;
            },
        );
    }
    fn refresh_models(&mut self) {
        let rpc = self.rpc.clone();
        self.work(
            false,
            move || {
                let mut models = Vec::new();
                let mut cursor = Value::Null;
                loop {
                    let result = rpc.request(
                        "model/list",
                        if cursor.is_null() {
                            json!({})
                        } else {
                            json!({"cursor":cursor})
                        },
                    )?;
                    models.extend(array(&result["data"]).iter().cloned());
                    cursor = result["nextCursor"].clone();
                    if cursor.is_null() {
                        break;
                    }
                }
                Ok(json!(models))
            },
            |s, v, _, _| {
                s.models = v.as_array().cloned().unwrap_or_default();
                if s.model.is_empty() {
                    s.model = s
                        .models
                        .iter()
                        .find(|m| m["isDefault"] == true)
                        .or(s.models.first())
                        .map(|m| text(m, "model").to_owned())
                        .unwrap_or_default();
                }
            },
        );
    }
    fn refresh_review(&mut self) {
        if !self.connected || self.cwd.is_empty() {
            return;
        }
        let rpc = self.rpc.clone();
        let path = self.cwd.clone();
        self.work(
            false,
            move || {
                Ok(
                    match rpc.request("host/workspace/review", json!({"cwd":path})) {
                        Ok(v) => json!({"path":path,"review":v}),
                        Err(e) => json!({"path":path,"error":e}),
                    },
                )
            },
            |s, v, _, _| {
                if text(&v, "path") == s.cwd {
                    s.review = v["review"].clone();
                    s.review_error = text(&v, "error").to_owned();
                }
            },
        );
    }
    fn release_content(&mut self) {
        self.images.clear();
        self.markdown_cache.clear();
        self.diffs.clear();
        self.expanded_work.clear();
        self.expanded_items.clear();
        match tempfile::Builder::new().prefix("bex-images-").tempdir() {
            Ok(directory) => self.image_dir = directory,
            Err(e) => self.error = e.to_string(),
        }
    }
    fn new_thread(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.load_generation += 1;
        self.release_content();
        self.selected.clear();
        self.conversation.thread = Value::Null;
        self.cwd = path;
        self.tab = Tab::Chat;
        self.restore_draft(window, cx);
        self.sync_list(true);
        self.refresh_review();
    }
    fn open_thread(&mut self, id: String, _: &mut Window, _: &mut Context<Self>) {
        self.load_generation += 1;
        let generation = self.load_generation;
        self.request(
            false,
            "thread/resume",
            json!({"threadId":id}),
            true,
            move |s, v, w, cx| {
                if generation != s.load_generation {
                    return;
                }
                s.release_content();
                s.selected = id;
                s.cwd = text(&v["thread"], "cwd").into();
                s.conversation.thread = v["thread"].clone();
                if let Some(model) = v["model"].as_str() {
                    s.model = model.into();
                }
                s.tab = Tab::Chat;
                s.restore_draft(w, cx);
                s.sync_requests(w, cx);
                s.sync_list(true);
                s.refresh_review();
            },
        );
    }
    fn switch_host(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.side_chat = None;
        self.terminal = None;
        self.panel = Panel::Home;
        self.rpc.close();
        self.epoch += 1;
        self.remote = id;
        self.rpc = Self::connect(&self.tx, self.epoch, false, &self.remote);
        self.connected = false;
        self.worktree_settings = Value::Null;
        self.worktree_saved = false;
        self.worktree_copy_paths.update(cx, |input, cx| input.set_value("", window, cx));
        self.models.clear();
        self.model.clear();
        self.projects.clear();
        self.threads.clear();
        self.conversation = Conversation::default();
        self.editor = Value::Null;
        self.entries.clear();
        self.review = Value::Null;
        self.new_thread(String::new(), window, cx);
    }
    fn send(&mut self, cx: &mut Context<Self>) {
        if !self.connected || self.busy > 0 {
            return;
        }
        let message = self.composer.read(cx).value().to_string();
        let key = self.draft_key();
        let files = array(&self.cache["attachments"][&key]).to_vec();
        if message.trim().is_empty() && files.is_empty() {
            return;
        }
        if self.selected.is_empty() {
            let generation = self.load_generation;
            let mut params = json!({"cwd":self.cwd});
            if !self.model.is_empty() {
                params["model"] = json!(self.model);
            }
            self.request(false, "thread/start", params, true, move |s, v, w, cx| {
                let id = text(&v["thread"], "id").to_owned();
                if id.is_empty() {
                    s.error = "Host が会話IDを返しませんでした".into();
                    return;
                }
                let target = format!(
                    "{}:{id}",
                    if s.remote.is_empty() {
                        "local"
                    } else {
                        &s.remote
                    }
                );
                s.cache["messages"][&target] = json!(message);
                s.cache["attachments"][&target] = json!(files);
                s.persist();
                if generation == s.load_generation {
                    s.selected = id.clone();
                    s.cwd = text(&v["thread"], "cwd").to_owned();
                    s.path.update(cx, |input, cx| input.set_value(s.cwd.clone(), w, cx));
                    s.terminal = None;
                    s.side_chat = None;
                    s.entries.clear();
                    s.editor = Value::Null;
                    s.review = Value::Null;
                    s.review_error.clear();
                    s.refresh_review();
                    s.conversation.thread = v["thread"].clone();
                    s.restore_draft(w, cx);
                    s.sync_list(true);
                }
                // Install the snapshot before sending: notifications may precede the turn/start reply.
                s.send_turn(id, None, key, message, files);
            });
        } else {
            let running = self.conversation.active().map(|t| t["id"].clone());
            self.send_turn(self.selected.clone(), running, key, message, files);
        }
    }
    fn send_turn(
        &mut self,
        id: String,
        running: Option<Value>,
        key: String,
        message: String,
        files: Vec<Value>,
    ) {
        let mut input = Vec::with_capacity(files.len() + 1);
        if !message.trim().is_empty() {
            input.push(json!({"type":"text","text":message,"text_elements":[]}));
        }
        for file in &files {
            let path = text(file, "path");
            let ext = Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            input.push(
                if matches!(
                    ext.as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic"
                ) {
                    json!({"type":"localImage","path":path})
                } else {
                    json!({"type":"mention","path":path,"name":file["name"]})
                },
            );
        }
        let method = if running.is_some() {
            "turn/steer"
        } else {
            "turn/start"
        };
        let mut params = json!({"threadId":id,"input":input});
        if let Some(turn) = running {
            params["expectedTurnId"] = turn;
        } else if !self.model.is_empty() {
            params["model"] = json!(self.model);
        }
        self.request(false, method, params, true, move |s, _, w, cx| {
            let target = format!(
                "{}:{id}",
                if s.remote.is_empty() {
                    "local"
                } else {
                    &s.remote
                }
            );
            for draft_key in [&key, &target] {
                if s.cache["messages"][draft_key] == message {
                    s.cache["messages"]
                        .as_object_mut()
                        .unwrap()
                        .remove(draft_key);
                }
                if let Some(current) = s.cache["attachments"][draft_key].as_array_mut() {
                    current.retain(|f| !files.iter().any(|sent| sent["path"] == f["path"]));
                }
            }
            s.persist();
            if s.selected == id {
                s.restore_draft(w, cx);
            }
            s.refresh_threads();
        });
    }
    fn open_panel(&mut self, panel: Panel, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Chat;
        let result = match panel {
            Panel::SideChat if self.side_chat.is_none() => {
                let mode = Mode::SideChat {
                    remote: self.remote.clone(),
                    cwd: self.cwd.clone(),
                };
                self.side_chat = Some(cx.new(|cx| Self::new(mode, window, cx)));
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
    fn attach(&mut self) {
        let remote = self.remote.clone();
        let cwd = self.cwd.clone();
        let manager = self.manager.clone();
        let key = self.draft_key();
        self.work(true,move||{let Some(source)=platform::choose("file")? else{return Ok(Value::Null);};let name=basename(&source);let path=if remote.is_empty(){source}else{text(&manager.request("host/transfer",json!({"profileId":remote,"direction":"upload","source":source,"directory":cwd,"fileName":name}))?,"path").to_owned()};Ok(json!({"key":key,"file":{"path":path,"name":name}}))},|s,v,_,_|{if v.is_null(){return;}let key=text(&v,"key");if !s.cache["attachments"][key].is_array(){s.cache["attachments"][key]=json!([]);}s.cache["attachments"][key].as_array_mut().unwrap().push(v["file"].clone());s.refresh_sources();
                s.persist();});
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
        self.request(
            false,
            "host/file/list",
            json!({"path":path}),
            true,
            |s, v, w, cx| {
                s.path
                    .update(cx, |i, cx| i.set_value(text(&v, "path").to_owned(), w, cx));
                s.entries = array(&v["entries"]).to_vec();
                if v["truncated"] == true {
                    s.error =
                        "先頭2,000件を表示しています。下位フォルダを選択してください。".into();
                }
            },
        );
    }
    fn edit(&mut self, path: String, reload: bool) {
        self.request(
            false,
            "host/file/read",
            json!({"path":path}),
            true,
            move |s, mut v, w, cx| {
                let key = format!(
                    "{}:{}",
                    if s.remote.is_empty() {
                        "local"
                    } else {
                        &s.remote
                    },
                    text(&v, "path")
                );
                if reload {
                    if let Some(files) = s.cache["files"].as_object_mut() {
                        files.remove(&key);
                    }
                    s.persist();
                }
                let saved = &s.cache["files"][&key];
                let value = if saved.is_object() {
                    if saved["revision"] != v["revision"] {
                        s.error =
                            "ホストのファイルが変更されています。下書きを保持しました。".into();
                    }
                    v["revision"] = saved["revision"].clone();
                    text(saved, "text").to_owned()
                } else {
                    text(&v, "text").to_owned()
                };
                s.editor = v;
                s.editor_input.update(cx, |i, cx| i.set_value(value, w, cx));
            },
        );
    }
    fn save_file(&mut self, cx: &mut Context<Self>) {
        let submitted = self.editor_input.read(cx).value().to_string();
        let key = self.editor_key();
        let path = text(&self.editor, "path").to_owned();
        self.request(
            false,
            "host/file/write",
            json!({"path":path,"revision":self.editor["revision"],"text":submitted}),
            true,
            move |s, v, w, cx| {
                if s.cache["files"][&key]["text"] == submitted {
                    s.cache["files"].as_object_mut().unwrap().remove(&key);
                }
                s.persist();
                if text(&s.editor, "path") == path {
                    let unchanged = s.editor_input.read(cx).value().as_ref() == submitted;
                    s.editor = v;
                    if unchanged {
                        s.editor_input.update(cx, |i, cx| {
                            i.set_value(text(&s.editor, "text").to_owned(), w, cx)
                        });
                    }
                }
                s.refresh_review();
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
    fn sync_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.requests.retain(|key, _| {
            self.conversation
                .requests
                .iter()
                .any(|r| r["id"].to_string() == *key)
        });
        for request in &self.conversation.requests {
            let key = request["id"].to_string();
            if self.requests.contains_key(&key) {
                continue;
            }
            let questions = array(&request["params"]["questions"])
                .iter()
                .map(|q| Question {
                    id: text(q, "id").into(),
                    prompt: text(q, "question").into(),
                    options: array(&q["options"])
                        .iter()
                        .map(|o| text(o, "label").into())
                        .collect(),
                    input: cx.new(|cx| InputState::new(window, cx).placeholder("回答を入力")),
                })
                .collect();
            self.requests.insert(
                key,
                RequestInputs {
                    questions,
                    raw: cx.new(|cx| {
                        TextareaState::new(window, cx)
                            .default_value("{}")
                            .auto_grow(3, 8)
                    }),
                    sent: false,
                },
            );
        }
    }
    fn respond(&mut self, id: Value, result: Value) {
        let client = self.rpc.clone();
        let key = id.to_string();
        self.work(
            true,
            move || client.respond(id, result).map(|_| Value::Null),
            move |s, _, _, _| {
                if let Some(request) = s.requests.get_mut(&key) {
                    request.sent = true;
                }
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

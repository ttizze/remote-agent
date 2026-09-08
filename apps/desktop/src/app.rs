mod clipboard;
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

type Apply =
    Box<dyn FnOnce(&mut Desktop, Result<Value, String>, &mut Window, &mut Context<Desktop>) + Send>;
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
struct Submission {
    id: String,
    key: String,
    message: String,
    files: Vec<Value>,
    input: Vec<Value>,
    model: String,
    effort: String,
    service_tier: String,
}

struct OutgoingMessage {
    id: String,
    key: String,
    item: Value,
    accepted: bool,
    turn_id: Option<String>,
    after_item_id: Option<String>,
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
    entries: Vec<(std::sync::Arc<String>, bool)>,
    initial: (std::sync::Arc<String>, bool),
    selected: Option<usize>,
    list: ListState,
    loading: bool,
    saving: bool,
    saved: bool,
    error: String,
}

impl ImageGallery {
    fn current_image(&self) -> &(std::sync::Arc<String>, bool) {
        self.selected.and_then(|index| self.entries.get(index)).unwrap_or(&self.initial)
    }
}

struct ImageState {
    source: std::sync::Arc<String>,
    path: Option<ImageSource>,
    error: Option<String>,
}
struct MarkdownContent {
    source: SharedString,
    rendered: SharedString,
    images: std::rc::Rc<[String]>,
}

struct DetailLoad {
    request: u64,
    error: Option<String>,
}

struct ProjectedTurn {
    segments: Vec<conversation_presentation::Segment>,
    // Native item indices, followed by native_count + outgoing-owner index.
    sources: Option<Vec<usize>>,
}

#[derive(Default)]
struct TaskIndicators {
    // Live events take precedence over list responses already in flight.
    active: HashMap<String, bool>,
    unread: HashSet<String>,
}

impl TaskIndicators {
    fn is_active(&self, thread: &Value) -> bool {
        self.active
            .get(text(thread, "id"))
            .copied()
            .unwrap_or(thread["status"]["type"] == "active")
    }

    fn reduce(&mut self, message: &Value, visible: Option<&str>) {
        let params = &message["params"];
        let Some(id) = params["threadId"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let active = match text(message, "method") {
            "thread/status/changed" => params["status"]["type"] == "active",
            "turn/started" => true,
            "turn/completed" => {
                if params["turn"]["status"] == "completed" && visible != Some(id) {
                    self.unread.insert(id.to_owned());
                }
                false
            }
            _ => return,
        };
        if active {
            self.unread.remove(id);
        }
        if let Some(previous) = self.active.get_mut(id) {
            *previous = active;
        } else {
            self.active.insert(id.to_owned(), active);
        }
    }
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
    worktree_directory: Entity<InputState>,
    worktree_saved: bool,
    projects: Vec<Value>,
    threads: Vec<Value>,
    task_indicators: TaskIndicators,
    models: Vec<Value>,
    model: String,
    effort: String,
    service_tier: String,
    effort_slider: Entity<slider::SliderState>,
    query: Value,
    list_generation: u64,
    load_generation: u64,
    more: Value,
    expanded_projects: HashSet<String>,
    expanded_items: HashSet<String>,
    expanded_work: HashMap<String, (String, bool)>,
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
    outgoing: Vec<OutgoingMessage>,
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
    image_gallery: Option<ImageGallery>,
    image_dir: tempfile::TempDir,
    invitation_file: tempfile::NamedTempFile,
    invitation_qr: Option<ImageSource>,
    markdown_cache: HashMap<String, MarkdownContent>,
    projected_turns: HashMap<usize, std::rc::Rc<ProjectedTurn>>,
    dirty_rows: Option<std::ops::Range<usize>>,
    conversation_pending: bool,
    history_loading: bool,
    history_watch: Option<u64>,
    history_refresh_pending: bool,
    history_refresh_dirty: bool,
    conversation_revision: u64,
    history_error: String,
    item_details: HashMap<(String, String), DetailLoad>,
    detail_request: u64,
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
        let worktree_directory = cx.new(|cx| InputState::new(window, cx).placeholder("接続先Mac上の絶対パス（空欄で既定）"));
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
        let effort_slider = cx.new(|_| slider::SliderState::new().max(1.).step(1.));
        let subscriptions = vec![
            cx.subscribe(&worktree_copy_paths, |s, _, event, cx| {
                if matches!(event, InputEvent::Change) { s.worktree_saved = false; cx.notify(); }
            }),
            cx.subscribe(&worktree_directory, |s, _, event, cx| {
                if matches!(event, InputEvent::Change) { s.worktree_saved = false; cx.notify(); }
            }),
            cx.subscribe(&effort_slider, |s, _, event, cx| {
                if let slider::SliderEvent::Change(slider::SliderValue::Single(index)) = event {
                    if let Some(model) = s.selected_model() {
                        if let Some(effort) = array(&model["supportedReasoningEfforts"]).get(*index as usize) {
                            s.effort = text(effort, "reasoningEffort").to_owned();
                            cx.notify();
                        }
                    }
                }
            }),
            cx.subscribe_in(&composer,window,|this,input,event,_,cx| {if matches!(event,InputEvent::Change){let key=this.draft_key();this.cache["messages"][key]=json!(input.read(cx).value().as_ref());this.persist();cx.notify();}}),
            cx.subscribe_in(&search,window,|this,input,event,_,cx| {if matches!(event,InputEvent::Change){this.query["searchTerm"]=json!(input.read(cx).value().as_ref());this.refresh_threads();}}),
            cx.subscribe_in(&editor_input,window,|this,input,event,_,cx| {if matches!(event,InputEvent::Change) && !this.editor.is_null(){let key=this.editor_key();this.cache["files"][key]=json!({"text":input.read(cx).value().as_ref(),"revision":this.editor["revision"]});this.persist();}}),
        ];
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(FollowMode::Tail);
        let entity = cx.entity().downgrade();
        list.set_scroll_handler(move |event, window, _cx| {
            if event.is_scrolled && !event.is_following_tail && event.visible_range.start == 0 {
                let entity = entity.clone();
                window.on_next_frame(move |window, cx| {
                    let _ = entity.update(cx, |s, cx| {
                        if s.history_error.is_empty() && s.list.logical_scroll_top().item_ix == 0 {
                            s.load_older(window, cx);
                        }
                    });
                });
            }
        });
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
            worktree_directory,
            worktree_saved: false,
            projects: vec![],
            threads: vec![],
            task_indicators: TaskIndicators::default(),
            models: vec![],
            model: String::new(),
            effort: String::new(),
            service_tier: "default".into(),
            effort_slider,
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
            outgoing: Vec::new(),
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
            image_gallery: None,
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
            projected_turns: HashMap::new(),
            dirty_rows: None,
            conversation_pending: false,
            history_loading: false,
            history_watch: None,
            history_refresh_pending: false,
            history_refresh_dirty: false,
            conversation_revision: 0,
            history_error: String::new(),
            item_details: HashMap::new(),
            detail_request: 0,
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
                apply: Box::new(move |s, result, w, cx| match result {
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
    fn thread_draft_key(&self, id: &str) -> String {
        format!(
            "{}:{id}",
            if self.remote.is_empty() {
                "local"
            } else {
                &self.remote
            }
        )
    }
    fn draft_key(&self) -> String {
        if self.selected.is_empty() {
            self.thread_draft_key(&format!("new:{}", self.cwd))
        } else {
            self.thread_draft_key(&self.selected)
        }
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
                    apply(self, result, window, cx);
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
                                self.task_indicators.active.clear();
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
                            if matches!(
                                text(&message, "method"),
                                "host/thread/changed" | "host/thread/watchFailed"
                            ) {
                                if self.history_watch.is_some()
                                    && message["params"]["watchId"].as_u64() == self.history_watch
                                    && message["params"]["threadId"] == self.selected
                                {
                                    if message["method"] == "host/thread/watchFailed" {
                                        self.error =
                                            "会話の自動更新が停止しました。再読み込みしてください"
                                                .into();
                                    } else {
                                        self.queue_history_refresh(window, cx);
                                    }
                                }
                                cx.notify();
                                return;
                            }
                            self.task_indicators.reduce(
                                &message,
                                (self.tab == Tab::Chat).then_some(self.selected.as_str()),
                            );
                            let completed = message["method"] == "turn/completed";
                            let change = self.conversation.reduce(message);
                            if completed && change.turn.is_some() {
                                self.refresh_review();
                            }
                            if change.changed() {
                                self.conversation_changed(change, window, cx);
                                return;
                            }
                        }
                    }
                }
            }
        }
        cx.notify();
    }
    fn project_turn(&mut self, index: usize, turn: &Value) -> std::rc::Rc<ProjectedTurn> {
        let id = text(turn, "id");
        if let Some(cached) = self.projected_turns.get(&index) {
            return cached.clone();
        }
        let key = self.draft_key();
        let items = array(&turn["items"]);
        let pending: Vec<_> = self
            .outgoing
            .iter()
            .enumerate()
            .filter(|(_, message)| message.key == key && message.turn_id.as_deref() == Some(id))
            .collect();
        let sources = if pending.is_empty() {
            None
        } else {
            let anchors: Vec<_> = pending
                .iter()
                .map(|(_, message)| message.after_item_id.as_deref())
                .collect();
            let mut order = conversation_presentation::source_order(
                items.len(),
                |index| text(&items[index], "id"),
                &anchors,
            );
            for source in &mut order {
                if *source >= items.len() {
                    *source = items.len() + pending[*source - items.len()].0;
                }
            }
            Some(order)
        };
        let item_at = |index: usize| {
            let source = sources.as_ref().map_or(index, |sources| sources[index]);
            if source < items.len() {
                &items[source]
            } else {
                &self.outgoing[source - items.len()].item
            }
        };
        let segments =
            conversation_presentation::project_items(turn, items.len() + pending.len(), item_at)
                .collect();
        let projected = std::rc::Rc::new(ProjectedTurn { segments, sources });
        self.projected_turns.insert(index, projected.clone());
        projected
    }
    fn mark_rows(&mut self, range: std::ops::Range<usize>) {
        match &mut self.dirty_rows {
            Some(dirty) => {
                dirty.start = dirty.start.min(range.start);
                dirty.end = dirty.end.max(range.end);
            }
            None => self.dirty_rows = Some(range),
        }
    }
    fn conversation_changed(
        &mut self,
        change: conversation::Change,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.conversation_revision += 1;
        if let Some(index) = change.turn {
            if change.projection {
                self.projected_turns.remove(&index);
            }
            self.mark_rows(index + 1..index + 2);
            let turn = &self.conversation.thread["turns"][index];
            if !self.item_details.is_empty()
                && let Some(item) = change.item
            {
                let key = (
                    text(turn, "id").to_owned(),
                    text(&turn["items"][item], "id").to_owned(),
                );
                if let Some(state) = self.item_details.get_mut(&key) {
                    state.error = Some("更新を受信しました。詳細を再読み込みしてください".into());
                }
            }
        }
        if change.sources {
            self.refresh_sources();
            let before = self.outgoing.len();
            let key = self.draft_key();
            reconcile_outgoing(&mut self.outgoing, &key, &self.conversation.thread);
            if before != self.outgoing.len() {
                self.projected_turns.clear();
                self.mark_rows(change.turn.map_or(0, |index| index + 1)..self.list.item_count());
            }
        }
        if change.requests {
            self.sync_requests(window, cx);
            self.mark_rows(
                usize::from(!self.conversation.thread.is_null())
                    + array(&self.conversation.thread["turns"]).len()
                    ..self.list.item_count().max(self.row_count()),
            );
        }
        if self.conversation_pending {
            return;
        }
        self.conversation_pending = true;
        cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(16))
                .await;
            let _ = view.update_in(cx, |view, _, cx| {
                view.conversation_pending = false;
                let count = view.row_count();
                let old = view.list.item_count();
                if old != count {
                    view.list
                        .splice(old.min(count)..old, count.saturating_sub(old));
                    let start = view
                        .dirty_rows
                        .as_ref()
                        .map_or(old.min(count), |range| range.start.min(old.min(count)));
                    view.mark_rows(start..count);
                }
                if let Some(range) = view.dirty_rows.take() {
                    view.list
                        .remeasure_items(range.start.min(count)..range.end.min(count));
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn row_count(&self) -> usize {
        usize::from(!self.conversation.thread.is_null())
            + array(&self.conversation.thread["turns"]).len()
            + self.visible_outgoing().count()
            + self.visible_requests().count()
    }
    fn pause_tail(&self) {
        // Bottom alignment stores an end sentinel, not the visible row offset.
        // Materialize that offset before an expansion changes the row height.
        if self.list.logical_scroll_top().item_ix == self.list.item_count() {
            self.list
                .scroll_by(-self.list.viewport_bounds().size.height);
        }
        self.list.pause_following_tail();
    }
    fn remeasure_item(&self, id: &str) {
        for (index, turn) in array(&self.conversation.thread["turns"]).iter().enumerate() {
            if text(turn, "id") == id
                || array(&turn["items"])
                    .iter()
                    .any(|item| text(item, "id") == id)
            {
                self.list.remeasure_items(index + 1..index + 2);
            }
        }
    }
    fn sync_list(&mut self, reset: bool) {
        let key = self.draft_key();
        reconcile_outgoing(&mut self.outgoing, &key, &self.conversation.thread);
        self.projected_turns.clear();
        let count = self.row_count();
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
    fn visible_outgoing(&self) -> impl Iterator<Item = (usize, &OutgoingMessage)> {
        let key = self.draft_key();
        self.outgoing
            .iter()
            .enumerate()
            .filter(move |(_, message)| {
                message.key == key
                    && !message.turn_id.as_deref().is_some_and(|id| {
                        array(&self.conversation.thread["turns"])
                            .iter()
                            .any(|turn| turn["id"] == id)
                    })
            })
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
            s.worktree_directory.update(cx, |input, cx| input.set_value(text(&value, "worktreeDirectory"), w, cx));
            s.worktree_settings = value;
            s.worktree_saved = false;
        });
    }

    fn save_worktree_settings(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.worktree_settings.clone();
        settings["copyPaths"] = json!(self.worktree_copy_paths.read(cx).value().lines()
            .map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>());
        settings["worktreeDirectory"] = json!(self.worktree_directory.read(cx).value().trim());
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
                    let mut result = rpc.request(
                        "model/list",
                        if cursor.is_null() {
                            json!({})
                        } else {
                            json!({"cursor":cursor})
                        },
                    )?;
                    if let Value::Array(mut page) = result["data"].take() {
                        if models.is_empty() { models = page; }
                        else { models.append(&mut page); }
                    }
                    cursor = result["nextCursor"].take();
                    if cursor.is_null() {
                        break;
                    }
                }
                Ok(Value::Array(models))
            },
            |s, v, _, cx| {
                s.models = match v { Value::Array(models) => models, _ => Vec::new() };
                let selected = s.models.iter().find(|m| m["model"] == s.model)
                    .or_else(|| s.models.iter().find(|m| m["isDefault"] == true))
                    .or(s.models.first()).map(|m| text(m, "model").to_owned()).unwrap_or_default();
                s.select_model(&selected, cx);
            },
        );
    }
    fn selected_model(&self) -> Option<&Value> {
        self.models.iter().find(|model| model["model"] == self.model)
    }
    fn select_model(&mut self, id: &str, cx: &mut Context<Self>) {
        let changed = self.model != id;
        let model = self.models.iter().find(|m| m["model"] == id);
        let (effort, tier, index) = supported_model_settings(
            model,
            if changed { "" } else { &self.effort },
            if changed { "" } else { &self.service_tier },
        );
        self.effort.replace_range(.., effort);
        self.service_tier.replace_range(.., tier);
        self.model.replace_range(.., id);
        let steps = model.map(|m| array(&m["supportedReasoningEfforts"]).len()).unwrap_or(0)
            .saturating_sub(1).max(1);
        self.effort_slider.update(cx, |slider, cx| {
            *slider = slider::SliderState::new().max(steps as f32)
                .step(1.).default_value(index as f32);
            cx.notify();
        });
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
        self.stop_history_watch();
        self.images.clear();
        self.image_gallery = None;
        self.markdown_cache.clear();
        self.projected_turns.clear();
        self.dirty_rows = None;
        self.history_loading = false;
        self.history_error.clear();
        self.item_details.clear();
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
            "host/thread/read",
            json!({"threadId":id,"includeTurns":true,"paginateHistory":true,"deferItemDetails":true}),
            true,
            move |s, mut v, w, cx| {
                if generation != s.load_generation {
                    return;
                }
                s.release_content();
                s.task_indicators.unread.remove(&id);
                s.selected = id;
                s.cwd = text(&v["thread"], "cwd").into();
                s.conversation.thread = v["thread"].take();
                if let Some(model) = v["model"].as_str() {
                    s.select_model(model, cx);
                }
                s.tab = Tab::Chat;
                s.restore_draft(w, cx);
                s.sync_requests(w, cx);
                s.sync_list(true);
                s.refresh_review();
                s.watch_history();
            },
        );
    }
    fn stop_history_watch(&mut self) {
        if let Some(watch_id) = self.history_watch.take() {
            self.request(
                false,
                "host/thread/unwatch",
                json!({"watchId":watch_id}),
                false,
                |_, _, _, _| {},
            );
        }
        self.history_refresh_pending = false;
        self.history_refresh_dirty = false;
    }
    fn watch_history(&mut self) {
        if self.conversation.thread["status"]["type"] != "notLoaded"
            || text(&self.conversation.thread, "path").is_empty()
        {
            return;
        }
        let generation = self.load_generation;
        self.history_watch = Some(generation);
        self.request_result(false, "host/thread/watch", json!({"threadId":self.selected,"watchId":generation,"path":self.conversation.thread["path"]}), false, move |s, result, w, cx| {
            if s.history_watch != Some(generation) { return; }
            match result {
                Ok(_) => s.queue_history_refresh(w, cx),
                Err(error) => { s.error = format!("会話の自動更新を開始できません: {error}"); s.history_watch = None; }
            }
        });
    }
    fn queue_history_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.history_refresh_dirty = true;
        if self.history_refresh_pending {
            return;
        }
        self.history_refresh_pending = true;
        let generation = self.load_generation;
        cx.spawn_in(window, async move |view, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
            let _ = view.update_in(cx, |s, _, _| {
                if s.history_watch != Some(generation) { return; }
                s.history_refresh_dirty = false;
                let revision = s.conversation_revision;
                s.request_result(false, "host/thread/read", json!({"threadId":s.selected,"includeTurns":true,"paginateHistory":true,"deferItemDetails":true}), false, move |s, result, w, cx| {
                    if s.history_watch != Some(generation) { return; }
                    s.history_refresh_pending = false;
                    if revision != s.conversation_revision { s.history_refresh_dirty = true; }
                    else {
                        match result.and_then(|mut value| s.conversation.refresh_history(value["thread"].take())) {
                            Ok(()) => { s.projected_turns.clear(); s.sync_list(false); s.refresh_sources(); }
                            Err(error) => s.error = format!("会話を更新できません: {error}"),
                        }
                    }
                    if s.conversation.thread["status"]["type"] != "notLoaded" { s.stop_history_watch(); }
                    else if s.history_refresh_dirty { s.queue_history_refresh(w, cx); }
                });
            });
        }).detach();
    }

    fn load_older(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.history_loading {
            return;
        }
        let Some(page) = self.conversation.older_page() else {
            return;
        };
        self.history_loading = true;
        self.history_error.clear();
        self.list.remeasure_items(0..1);
        let generation = self.load_generation;
        let method = if page.turn.is_some() {
            "host/thread/items/list"
        } else {
            "host/thread/turns/list"
        };
        self.request_result(false, method, json!({"threadId":self.selected,"turnId":page.turn,"cursor":page.cursor,"deferItemDetails":true}), false, move |s, result, w, cx| {
            if generation != s.load_generation { return; }
            s.history_loading = false;
            let anchor = s.list.logical_scroll_top();
            let old_height = s.list.bounds_for_item(anchor.item_ix).map(|bounds| bounds.size.height);
            let anchor_is_changed = page.turn.as_ref().is_some_and(|id| anchor.item_ix > 0 && s.conversation.thread["turns"][anchor.item_ix - 1]["id"] == *id);
            match result.and_then(|page_value| s.conversation.merge_older(page_value, &page)) {
                Ok(added) => {
                    s.conversation_revision += 1;
                    s.projected_turns.clear();
                    if added > 0 { s.list.splice(1..1, added); }
                    s.list.remeasure();
                    s.refresh_sources();
                    // Whole-turn prepends are handled by ListState::splice.
                    // Within a turn, retain the old visible suffix after layout.
                    if anchor_is_changed && let Some(old_height) = old_height {
                        let view = cx.entity().downgrade();
                        w.on_next_frame(move |_, cx| { let _ = view.update(cx, |s, cx| {
                            if generation != s.load_generation { return; }
                            if let Some(bounds) = s.list.bounds_for_item(anchor.item_ix) {
                                s.list.scroll_to(ListOffset { item_ix: anchor.item_ix, offset_in_item: anchor.offset_in_item + bounds.size.height - old_height });
                                cx.notify();
                            }
                        }); });
                    }
                }
                Err(error) => { s.history_error = error; s.list.remeasure_items(0..1); }
            }
        });
        cx.notify();
    }
    fn load_detail(&mut self, turn_id: String, item_id: String) {
        let key = (turn_id.clone(), item_id.clone());
        if self
            .item_details
            .get(&key)
            .is_some_and(|state| state.error.is_none())
        {
            return;
        }
        let deferred = array(&self.conversation.thread["turns"])
            .iter()
            .find(|turn| turn["id"] == turn_id)
            .is_some_and(|turn| {
                array(&turn["deferredItemIds"])
                    .iter()
                    .any(|id| id == &item_id)
            });
        if !deferred {
            return;
        }
        self.detail_request += 1;
        let request = self.detail_request;
        self.item_details.insert(
            key.clone(),
            DetailLoad {
                request,
                error: None,
            },
        );
        let generation = self.load_generation;
        self.request_result(
            false,
            "host/thread/item/read",
            json!({"threadId":self.selected,"turnId":turn_id,"itemId":item_id}),
            false,
            move |s, result, _, _| {
                if generation != s.load_generation
                    || !s
                        .item_details
                        .get(&key)
                        .is_some_and(|state| state.request == request && state.error.is_none())
                {
                    return;
                }
                match result.and_then(|mut value| {
                    s.conversation
                        .apply_detail(&turn_id, &item_id, value["item"].take())
                }) {
                    Ok(_) => {
                        s.conversation_revision += 1;
                        s.item_details.remove(&key);
                        s.projected_turns.clear();
                    }
                    Err(error) => {
                        s.item_details.get_mut(&key).unwrap().error = Some(error);
                    }
                }
                if s.expanded_items.contains(&item_id) {
                    s.pause_tail();
                }
                s.remeasure_item(&turn_id);
            },
        );
    }

    fn switch_host(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.side_chat = None;
        self.terminal = None;
        self.panel = Panel::Home;
        self.stop_history_watch();
        self.rpc.close();
        self.epoch += 1;
        self.remote = id;
        self.rpc = Self::connect(&self.tx, self.epoch, false, &self.remote);
        self.connected = false;
        self.worktree_settings = Value::Null;
        self.worktree_saved = false;
        self.worktree_copy_paths.update(cx, |input, cx| input.set_value("", window, cx));
        self.worktree_directory.update(cx, |input, cx| input.set_value("", window, cx));
        self.models.clear();
        self.select_model("", cx);
        self.projects.clear();
        self.threads.clear();
        self.task_indicators = TaskIndicators::default();
        self.conversation = Conversation::default();
        self.outgoing.clear();
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
        let input = message_input(&message, &files);
        let outgoing_id = uuid::Uuid::new_v4().to_string();
        self.outgoing.push(OutgoingMessage {
            id: outgoing_id.clone(),
            key: key.clone(),
            item: json!({"id":outgoing_id,"clientId":outgoing_id,"type":"userMessage","content":input}),
            turn_id: self.conversation.active().map(|turn| text(turn, "id").to_owned()),
            after_item_id: self.conversation.active().and_then(|turn| array(&turn["items"]).last()).map(|item| text(item, "id").to_owned()),
            accepted: false,
        });
        let submission = Submission {
            id: outgoing_id,
            key,
            message,
            files,
            input,
            model: self.model.clone(),
            effort: self.effort.clone(),
            service_tier: self.service_tier.clone(),
        };
        self.sync_list(false);
        self.list.scroll_to_end();
        cx.notify();
        if self.selected.is_empty() {
            let generation = self.load_generation;
            let mut params = json!({"cwd":self.cwd});
            if !self.model.is_empty() {
                params["model"] = json!(self.model);
            }
            self.request_result(
                false,
                "thread/start",
                params,
                true,
                move |s, result, w, cx| {
                    let mut v = match result {
                        Ok(value) => value,
                        Err(error) => {
                            s.fail_send(&submission.id, error);
                            return;
                        }
                    };
                    let id = text(&v["thread"], "id").to_owned();
                    if id.is_empty() {
                        s.fail_send(&submission.id, "Host が会話IDを返しませんでした".into());
                        return;
                    }
                    let target = s.thread_draft_key(&id);
                    if let Some(outgoing) = s.outgoing.iter_mut().find(|m| m.id == submission.id) {
                        outgoing.key = target.clone();
                    }
                    s.cache["messages"][&target] = json!(submission.message);
                    s.cache["attachments"][&target] = json!(submission.files);
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
                        s.conversation.thread = v["thread"].take();
                        s.restore_draft(w, cx);
                        s.sync_list(true);
                    }
                    // Install the snapshot before sending: notifications may precede the turn/start reply.
                    s.send_turn(id, None, submission);
                },
            );
        } else {
            let running = self.conversation.active().map(|t| t["id"].clone());
            if running.is_some() {
                self.send_turn(self.selected.clone(), running, submission);
            } else {
                let id = self.selected.clone();
                self.request_result(
                    false,
                    "thread/resume",
                    json!({"threadId":id}),
                    true,
                    move |s, result, _, _| match result {
                        Ok(_) => s.send_turn(id, None, submission),
                        Err(error) => s.fail_send(&submission.id, error),
                    },
                );
            }
        }
    }
    fn send_turn(&mut self, id: String, running: Option<Value>, submission: Submission) {
        let method = if running.is_some() {
            "turn/steer"
        } else {
            "turn/start"
        };
        let mut params = json!({"threadId":id,"clientUserMessageId":submission.id});
        params["input"] = Value::Array(submission.input);
        if let Some(turn) = running {
            params["expectedTurnId"] = turn;
        } else {
            if !submission.model.is_empty() { params["model"] = json!(submission.model); }
            if !submission.effort.is_empty() { params["effort"] = json!(submission.effort); }
            params["serviceTierForTurn"] = json!(submission.service_tier);
        }
        self.request_result(false, method, params, true, move |s, result, w, cx| {
            let response = match result {
                Ok(value) => value,
                Err(error) => {
                    s.fail_send(&submission.id, error);
                    return;
                }
            };
            if let Some(outgoing) = s.outgoing.iter_mut().find(|m| m.id == submission.id) {
                outgoing.accepted = true;
                if outgoing.turn_id.is_none() {
                    outgoing.turn_id = response["turn"]["id"]
                        .as_str()
                        .or(response["turnId"].as_str())
                        .map(str::to_owned);
                }
            }
            let target = s.thread_draft_key(&id);
            for draft_key in [&submission.key, &target] {
                if s.cache["messages"][draft_key] == submission.message {
                    s.cache["messages"]
                        .as_object_mut()
                        .unwrap()
                        .remove(draft_key);
                }
                if let Some(current) = s.cache["attachments"][draft_key].as_array_mut() {
                    current.retain(|f| {
                        !submission
                            .files
                            .iter()
                            .any(|sent| sent["path"] == f["path"])
                    });
                }
            }
            s.persist();
            if s.selected == id {
                s.restore_draft(w, cx);
            }
            s.sync_list(false);
            s.refresh_threads();
        });
    }
    fn fail_send(&mut self, id: &str, error: String) {
        self.outgoing.retain(|message| message.id != id);
        self.error = error;
        self.sync_list(false);
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
    fn composer_enter(
        &mut self,
        action: &gpui_kit::component::input::Enter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let submit = self.composer.update(cx, |input, cx| {
            composer_should_submit(input, action, window, cx)
        });
        if submit {
            cx.stop_propagation();
            if !self.cwd.is_empty() {
                self.send(cx);
            }
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
        // Capture before the text input can replace selected text with an empty string.
        cx.stop_propagation();
        if self.busy > 0 {
            return;
        }
        self.attach_sources(move || {
            let directory = platform::state_dir().join("attachments");
            item.into_entries()
                .filter_map(|entry| match entry {
                    ClipboardEntry::Image(image) => Some(clipboard::save_image(&directory, image)),
                    _ => None,
                })
                .collect()
        });
        cx.notify();
    }
    fn attach(&mut self) {
        self.attach_sources(|| platform::choose("file").map(|file| file.into_iter().collect()));
    }
    fn attach_sources(
        &mut self,
        sources: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static,
    ) {
        let remote = self.remote.clone();
        let cwd = self.cwd.clone();
        let manager = self.manager.clone();
        let key = self.draft_key();
        self.work(true, move || {
            let sources = sources()?;
            let mut files = Vec::with_capacity(sources.len());
            for source in sources {
                let name = basename(&source);
                let path = if remote.is_empty() { source } else {
                    text(&manager.request("host/transfer", json!({"profileId":remote,"direction":"upload","source":source,"directory":cwd,"fileName":name}))?, "path").to_owned()
                };
                files.push(json!({"path":path,"name":name}));
            }
            Ok(json!({"key":key,"files":files}))
        }, |s, v, _, _| {
            let files = array(&v["files"]);
            if files.is_empty() { return; }
            let key = text(&v, "key");
            if !s.cache["attachments"][key].is_array() {
                s.cache["attachments"][key] = json!([]);
            }
            s.cache["attachments"][key].as_array_mut().unwrap().extend_from_slice(files);
            s.refresh_sources();
            s.persist();
        });
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

// Keep valid choices on catalog refresh; changed models pass empty choices to use defaults.
fn supported_model_settings<'a>(model: Option<&'a Value>, effort: &str, tier: &str) -> (&'a str, &'a str, usize) {
    let Some(model) = model else { return ("", "default", 0); };
    let efforts = array(&model["supportedReasoningEfforts"]);
    let index = efforts.iter().position(|e| e["reasoningEffort"] == effort)
        .or_else(|| efforts.iter().position(|e| e["reasoningEffort"] == model["defaultReasoningEffort"]))
        .unwrap_or(0);
    let tiers = array(&model["serviceTiers"]);
    let supported_tier = |id: &str| {
        if id == "default" { Some("default") }
        else { tiers.iter().find(|t| t["id"] == id).map(|t| text(t, "id")) }
    };
    (
        efforts.get(index).map(|e| text(e, "reasoningEffort")).unwrap_or_default(),
        supported_tier(tier).or_else(|| supported_tier(text(model, "defaultServiceTier"))).unwrap_or("default"),
        index,
    )
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

// Selection offsets and Rope::len are both UTF-8 bytes, including Japanese text.
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

fn message_input(message: &str, files: &[Value]) -> Vec<Value> {
    let mut input = Vec::with_capacity(files.len() + usize::from(!message.trim().is_empty()));
    if !message.trim().is_empty() {
        input.push(json!({"type":"text","text":message,"text_elements":[]}));
    }
    for file in files {
        let path = text(file, "path");
        let image = Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                ["png", "jpg", "jpeg", "gif", "webp", "heic"]
                    .iter()
                    .any(|image| ext.eq_ignore_ascii_case(image))
            });
        input.push(if image {
            json!({"type":"localImage","path":path})
        } else {
            json!({"type":"mention","path":path,"name":file["name"]})
        });
    }
    input
}
fn user_items(thread: &Value) -> impl Iterator<Item = &Value> {
    array(&thread["turns"])
        .iter()
        .flat_map(|turn| array(&turn["items"]))
        .filter(|item| item["type"] == "userMessage")
}
fn reconcile_outgoing(outgoing: &mut Vec<OutgoingMessage>, key: &str, thread: &Value) {
    let pending: Vec<_> = outgoing
        .iter()
        .filter(|message| message.key == key)
        .map(|message| message.id.as_str())
        .collect();
    if pending.is_empty() {
        return;
    }
    let echoed = user_items(thread).filter_map(|item| item["clientId"].as_str());
    let retained = conversation_presentation::remaining_submissions(&pending, echoed);
    let mut retained = retained.into_iter().peekable();
    let mut index = 0;
    outgoing.retain(|message| {
        if message.key != key {
            return true;
        }
        let keep = retained.peek() == Some(&index);
        if keep {
            retained.next();
        }
        index += 1;
        keep
    });
}

#[cfg(test)]
mod model_settings_tests {
    use super::supported_model_settings;
    use serde_json::json;

    #[test]
    fn refreshed_catalog_preserves_supported_choices_and_tracks_reordered_efforts() {
        let mut model = json!({
            "defaultReasoningEffort":"medium", "defaultServiceTier":"priority",
            "supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"max"}],
            "serviceTiers":[{"id":"priority"}]
        });
        assert_eq!(supported_model_settings(Some(&model), "max", "default"), ("max", "default", 1));
        model["supportedReasoningEfforts"].as_array_mut().unwrap().reverse();
        assert_eq!(supported_model_settings(Some(&model), "max", "priority"), ("max", "priority", 0));
    }

    #[test]
    fn changed_model_or_removed_options_use_supported_defaults() {
        let mut model = json!({
            "defaultReasoningEffort":"low", "defaultServiceTier":"priority",
            "supportedReasoningEfforts":[{"reasoningEffort":"low"}],
            "serviceTiers":[{"id":"priority"}]
        });
        assert_eq!(supported_model_settings(Some(&model), "", ""), ("low", "priority", 0));
        model["serviceTiers"] = json!([]);
        assert_eq!(supported_model_settings(Some(&model), "max", "priority"), ("low", "default", 0));
        model["defaultReasoningEffort"] = json!("max");
        assert_eq!(supported_model_settings(Some(&model), "", ""), ("low", "default", 0));
    }

    #[test]
    fn no_model_or_no_options_clears_effort_and_fast() {
        assert_eq!(supported_model_settings(None, "max", "priority"), ("", "default", 0));
        assert_eq!(supported_model_settings(Some(&json!({})), "max", "priority"), ("", "default", 0));
    }
}

#[cfg(test)]
mod task_indicator_tests {
    use super::TaskIndicators;
    use serde_json::json;

    #[test]
    fn live_lifecycle_overrides_stale_list_and_marks_unseen_success() {
        let idle = json!({"id":"a", "status":{"type":"idle"}});
        let active = json!({"id":"a", "status":{"type":"active"}});
        let mut indicators = TaskIndicators::default();
        assert!(indicators.is_active(&active));
        assert!(!indicators.is_active(&idle));
        indicators.reduce(
            &json!({"method":"turn/started", "params":{"threadId":"a"}}),
            None,
        );
        assert!(indicators.is_active(&idle));
        indicators.reduce(&json!({"method":"turn/completed", "params":{"threadId":"a", "turn":{"status":"completed"}}}), Some("b"));
        assert!(!indicators.is_active(&active));
        assert!(indicators.unread.contains("a"));
        indicators.reduce(&json!({"method":"thread/status/changed", "params":{"threadId":"a", "status":{"type":"idle"}}}), None);
        assert!(indicators.unread.contains("a"));
        indicators.reduce(&json!({"method":"thread/status/changed", "params":{"threadId":"a", "status":{"type":"active"}}}), None);
        assert!(indicators.is_active(&idle));
        assert!(indicators.unread.is_empty());
    }

    #[test]
    fn visible_success_and_unsuccessful_turns_do_not_mark_unread() {
        let mut indicators = TaskIndicators::default();
        for (status, visible) in [
            ("completed", Some("a")),
            ("failed", None),
            ("interrupted", None),
        ] {
            indicators.reduce(&json!({"method":"turn/completed", "params":{"threadId":"a", "turn":{"status":status}}}), visible);
            assert!(indicators.unread.is_empty());
        }
        indicators.reduce(
            &json!({"method":"item/completed", "params":{"threadId":"a"}}),
            None,
        );
        assert!(indicators.unread.is_empty());
    }
}

#[cfg(test)]
mod composer_tests {
    use super::{TextareaState, composer_should_submit};
    use gpui_kit as gpui;
    use gpui_kit::{EntityInputHandler, TestAppContext};

    #[gpui::test]
    fn enter_submits_only_committed_text_at_the_end(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let input = cx.add_window(|window, cx| TextareaState::new(window, cx));
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
mod outgoing_tests {
    use super::{OutgoingMessage, message_input, reconcile_outgoing};
    use serde_json::{Value, json};

    fn pending(id: &str, key: &str, content: Vec<Value>) -> OutgoingMessage {
        OutgoingMessage {
            id: id.into(),
            key: key.into(),
            item: json!({"id":id,"type":"userMessage","content":content}),
            turn_id: None,
            after_item_id: None,
            accepted: false,
        }
    }
    #[test]
    fn delayed_echo_keeps_accepted_input_visible() {
        let mut messages = vec![pending(
            "local",
            "local:thread",
            message_input("hello", &[]),
        )];
        let empty = json!({"id":"thread","turns":[]});
        reconcile_outgoing(&mut messages, "local:thread", &empty);
        assert_eq!(messages[0].item["content"][0]["text"], "hello");
        messages[0].accepted = true;
        reconcile_outgoing(&mut messages, "local:thread", &empty);
        assert_eq!(messages.len(), 1);
        let thread = json!({"turns":[{"items":[{"id":"native","clientId":"local","type":"userMessage","content":[{"type":"text","text":"hello"}]}]}]});
        reconcile_outgoing(&mut messages, "local:other", &thread);
        assert_eq!(messages.len(), 1);
        reconcile_outgoing(&mut messages, "local:thread", &thread);
        assert!(messages.is_empty());
    }
    #[test]
    fn repeated_text_and_duplicate_events_consume_one_submission_each() {
        let mut messages = vec![
            pending("one", "key", message_input("same", &[])),
            pending("two", "key", message_input("same", &[])),
        ];
        let item = |id, client| json!({"id":id,"clientId":client,"type":"userMessage","content":[{"type":"text","text":"same"}]});
        let mut thread = json!({"turns":[{"items":[item("old", "old")]}]});
        reconcile_outgoing(&mut messages, "key", &thread);
        assert_eq!(messages.len(), 2);
        thread["turns"][0]["items"]
            .as_array_mut()
            .unwrap()
            .push(item("new", "one"));
        reconcile_outgoing(&mut messages, "key", &thread);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].id, "two");
        reconcile_outgoing(&mut messages, "key", &thread);
        assert_eq!(messages.len(), 1);
        thread["turns"][0]["items"]
            .as_array_mut()
            .unwrap()
            .push(item("second", "two"));
        reconcile_outgoing(&mut messages, "key", &thread);
        assert!(messages.is_empty());
    }
    #[test]
    fn attachment_echo_reconciles_by_client_id_even_when_content_is_normalized() {
        let files = vec![
            json!({"path":"/tmp/photo.PNG","name":"photo"}),
            json!({"path":"/tmp/note.txt","name":"note"}),
        ];
        let mut messages = vec![pending("one", "key", message_input("", &files))];
        assert_eq!(messages[0].item["content"][0]["type"], "localImage");
        let mut thread = json!({"turns":[{"items":[{"id":"native","type":"userMessage","content":[{"type":"localImage","path":"/tmp/other.PNG"},{"type":"mention","path":"/tmp/note.txt"}]}]}]});
        reconcile_outgoing(&mut messages, "key", &thread);
        assert_eq!(messages.len(), 1);
        thread["turns"][0]["items"][0]["clientId"] = json!("one");
        reconcile_outgoing(&mut messages, "key", &thread);
        assert!(messages.is_empty());
    }
}

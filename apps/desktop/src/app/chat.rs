use super::*;
#[path = "dictation.rs"]
mod dictation;
mod view;
use agent_client::operations::RequestAnswer;
use conversation_presentation::{models::supported_model_settings, requests::request_presentation};
pub(super) use dictation::DictationEvent;
use dictation::{Dictation, Phase};

// Main and Side reuse stable watch keys. Reopening a view must still issue a
// newer revision than its late registration/cancellation on the shared socket.
static NEXT_LOAD: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub(super) enum Intent {
    Selected,
    Review,
    SelectHost(String),
}
impl EventEmitter<Intent> for ConversationView {}

struct Submission {
    id: String,
    key: String,
    message: String,
    dictated_text: Option<String>,
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
        self.selected
            .and_then(|index| self.entries.get(index))
            .unwrap_or(&self.initial)
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

pub(super) struct ConversationSession {
    pub(super) host: Rc<host::Connection>,
    epoch: u64,
    pub(super) connected: bool,
    load_generation: u64,
    pub(super) cwd: String,
    pub(super) selected: String,
    pub(super) conversation: Conversation,
    outgoing: Vec<OutgoingMessage>,
    history_loading: bool,
    history_watch: Option<u64>,
    history_refresh_pending: bool,
    history_refresh_dirty: bool,
    conversation_revision: u64,
    history_error: String,
    item_details: HashMap<(String, String), DetailLoad>,
    detail_request: u64,
}
pub(super) struct ConversationView {
    pub(super) session: ConversationSession,
    owner: WeakEntity<ConversationView>,
    tx: async_channel::Sender<super::Event>,
    manager: Rpc,
    manager_session: Entity<Management>,
    host_subscription: Option<Subscription>,
    models_revision: u64,
    busy: usize,
    error: String,
    model: String,
    effort: String,
    service_tier: String,
    effort_slider: Entity<slider::SliderState>,
    expanded_items: HashSet<String>,
    expanded_work: HashMap<String, (String, bool)>,
    pub(super) source_paths: Vec<String>,
    composer: Entity<TextareaState>,
    dictation: Option<Dictation>,
    drafts: Entity<drafts::State>,
    draft_scope: drafts::Scope,
    pub(super) review: Option<std::sync::Arc<WorkspaceReview>>,
    pub(super) review_error: String,
    review_expanded: bool,
    requests: HashMap<Value, RequestInputs>,
    list: ListState,
    _subscriptions: Vec<Subscription>,
    diffs: HashMap<String, Entity<DiffView>>,
    images: HashMap<String, ImageState>,
    image_gallery: Option<ImageGallery>,
    image_dir: tempfile::TempDir,
    markdown_cache: HashMap<String, MarkdownContent>,
    projected_turns: HashMap<usize, std::rc::Rc<ProjectedTurn>>,
    dirty_rows: Option<std::ops::Range<usize>>,
    conversation_pending: bool,
}
impl Drop for ConversationView {
    fn drop(&mut self) {
        self.stop_history_watch();
    }
}
impl ConversationView {
    pub(super) fn new(
        management: (Rpc, Entity<Management>),
        drafts: Entity<drafts::State>,
        host: Rc<host::Connection>,
        cwd: String,
        draft_scope: drafts::Scope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (manager, manager_session) = management;
        let tx = host.events.clone();
        let connected = host.state.read(cx).connected;
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Codex に依頼する")
                .auto_grow(2, 8)
        });
        let error = drafts
            .read(cx)
            .error(draft_scope)
            .unwrap_or_default()
            .to_owned();
        let effort_slider = cx.new(|_| slider::SliderState::new().max(1.).step(1.));
        let subscriptions = vec![
            cx.subscribe(&drafts, |s, _, error: &drafts::SaveError, cx| {
                if error.scope == s.draft_scope {
                    s.error = error.message.clone();
                    cx.notify();
                }
            }),
            cx.observe(&manager_session, |_, _, cx| cx.notify()),
            cx.subscribe(&effort_slider, |s, _, event, cx| {
                if let slider::SliderEvent::Change(slider::SliderValue::Single(index)) = event
                    && let Some(model) = s.selected_model(cx)
                    && let Some(effort) =
                        array(&model["supportedReasoningEfforts"]).get(*index as usize)
                {
                    s.effort = text(effort, "reasoningEffort").to_owned();
                    cx.notify();
                }
            }),
            cx.subscribe_in(&composer, window, |this, input, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let key = this.draft_key();
                    let value = json!(input.read(cx).value().as_ref());
                    let scope = this.draft_scope;
                    drafts::update(&this.drafts, scope, cx, |mut cache| {
                        cache["messages"][key] = value;
                        cache
                    });
                    cx.notify();
                }
            }),
        ];
        let list = ListState::new(0, ListAlignment::Bottom, px(600.));
        list.set_follow_mode(FollowMode::Tail);
        let entity = cx.entity().downgrade();
        list.set_scroll_handler(move |event, window, _cx| {
            if event.is_scrolled && !event.is_following_tail && event.visible_range.start == 0 {
                let entity = entity.clone();
                window.on_next_frame(move |window, cx| {
                    let _ = entity.update(cx, |s, cx| {
                        if s.session.history_error.is_empty()
                            && s.list.logical_scroll_top().item_ix == 0
                        {
                            s.load_older(window, cx);
                        }
                    });
                });
            }
        });
        let mut desktop = Self {
            session: ConversationSession {
                host,
                epoch: 1,
                connected,
                load_generation: 0,
                cwd,
                selected: String::new(),
                conversation: Conversation::default(),
                outgoing: Vec::new(),
                history_loading: false,
                history_watch: None,
                history_refresh_pending: false,
                history_refresh_dirty: false,
                conversation_revision: 0,
                history_error: String::new(),
                item_details: HashMap::new(),
                detail_request: 0,
            },
            owner: cx.entity().downgrade(),
            tx,
            manager,
            manager_session,
            host_subscription: None,
            models_revision: 0,
            busy: 0,
            error,
            model: String::new(),
            effort: String::new(),
            service_tier: "default".into(),
            effort_slider,
            expanded_items: HashSet::new(),
            expanded_work: HashMap::new(),
            source_paths: Vec::new(),
            composer,
            dictation: None,
            drafts,
            draft_scope,
            review: None,
            review_error: String::new(),
            review_expanded: false,
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
            markdown_cache: HashMap::new(),
            projected_turns: HashMap::new(),
            dirty_rows: None,
            conversation_pending: false,
        };
        desktop.subscribe_host(cx);
        desktop.restore_draft(window, cx);
        desktop
    }
    fn complete<T: Send + 'static>(
        &mut self,
        busy: bool,
        apply: impl FnOnce(&mut Self, Result<T, String>, &mut Window, &mut Context<Self>)
        + Send
        + 'static,
    ) -> impl FnOnce(Result<T, String>) + Send + 'static {
        if busy {
            self.busy += 1;
            self.error.clear();
        }
        let tx = self.tx.clone();
        let epoch = self.session.epoch;
        let owner = self.owner.clone();
        move |result| {
            let _ = tx.send_blocking(super::Event::Done(Box::new(move |_, w, cx| {
                let _ = owner.update(cx, |s, cx| {
                    if epoch != s.session.epoch {
                        return;
                    }
                    if busy {
                        s.busy = s.busy.saturating_sub(1);
                    }
                    apply(s, result, w, cx);
                    cx.notify();
                });
            })));
        }
    }

    fn work(
        &mut self,
        busy: bool,
        task: impl FnOnce() -> Result<Value, String> + Send + 'static,
        apply: impl FnOnce(&mut Self, Value, &mut Window, &mut Context<Self>) + Send + 'static,
    ) {
        let done = self.complete(busy, move |s, result, w, cx| match result {
            Ok(value) => apply(s, value, w, cx),
            Err(error) => s.error = error,
        });
        std::thread::spawn(move || done(task()));
    }

    fn thread_draft_key(&self, id: &str) -> String {
        format!(
            "{}:{id}",
            if self.session.host.remote.is_empty() {
                "local"
            } else {
                &self.session.host.remote
            }
        )
    }

    pub(super) fn draft_key(&self) -> String {
        if self.session.selected.is_empty() {
            self.thread_draft_key(&format!("new:{}", self.session.cwd))
        } else {
            self.thread_draft_key(&self.session.selected)
        }
    }

    fn refresh_sources(&mut self, cx: &App) {
        let mut sources = HashSet::new();
        for file in
            array(&self.drafts.read(cx).cache(self.draft_scope)["attachments"][self.draft_key()])
        {
            if let Some(path) = file["path"].as_str() {
                sources.insert(path.to_owned());
            }
        }
        for turn in array(&self.session.conversation.thread["turns"]) {
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

    pub(super) fn restore_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_sources(cx);
        let value = text(
            &self.drafts.read(cx).cache(self.draft_scope)["messages"],
            &self.draft_key(),
        )
        .to_owned();
        self.composer
            .update(cx, |state, cx| state.set_value(value, window, cx));
    }

    pub(super) fn accepts(&self, host: EntityId, event: &rpc::Event) -> bool {
        if self.session.host.state.entity_id() != host {
            return false;
        }
        let rpc::Event::Message(message) = event else {
            return true;
        };
        if let Some(key) = message["params"]["watchKey"].as_u64() {
            return key == self.draft_scope as u64;
        }
        // Requests stay available when navigating to their conversation later.
        message.get("id").is_some()
            || message["method"] == "serverRequest/resolved"
            || message["params"]["threadId"]
                .as_str()
                .is_none_or(|id| id == self.session.selected)
    }

    pub(super) fn receive_rpc(
        &mut self,
        event: rpc::Event,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            rpc::Event::Connected(online, reason) => {
                self.session.connected = online;
                if online {
                    self.error.clear();
                    self.session.conversation.requests.clear();
                    if !self.session.selected.is_empty() {
                        self.open_thread(self.session.selected.clone(), window, cx);
                    }
                } else {
                    self.cancel_recording();
                    self.error = reason;
                }
            }
            rpc::Event::Message(message) => {
                if matches!(
                    text(&message, "method"),
                    "host/thread/changed" | "host/thread/watchFailed"
                ) {
                    if self.session.history_watch.is_some()
                        && message["params"]["watchId"].as_u64() == self.session.history_watch
                        && message["params"]["threadId"] == self.session.selected
                    {
                        if message["method"] == "host/thread/watchFailed" {
                            self.error =
                                "会話の自動更新が停止しました。再読み込みしてください".into();
                        } else {
                            self.queue_history_refresh(window, cx);
                        }
                    }
                } else {
                    let completed = message["method"] == "turn/completed";
                    let (next, change) = agent_client::conversation::reduce(
                        std::mem::take(&mut self.session.conversation),
                        message,
                    );
                    self.session.conversation = next;
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
            .session
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
                &self.session.outgoing[source - items.len()].item
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
        self.session.conversation_revision += 1;
        if let Some(index) = change.turn {
            if change.projection {
                self.projected_turns.remove(&index);
            }
            self.mark_rows(index + 1..index + 2);
            let turn = &self.session.conversation.thread["turns"][index];
            if !self.session.item_details.is_empty()
                && let Some(item) = change.item
            {
                let key = (
                    text(turn, "id").to_owned(),
                    text(&turn["items"][item], "id").to_owned(),
                );
                if let Some(state) = self.session.item_details.get_mut(&key) {
                    state.error = Some("更新を受信しました。詳細を再読み込みしてください".into());
                }
            }
        }
        if change.sources {
            self.refresh_sources(cx);
            let before = self.session.outgoing.len();
            let key = self.draft_key();
            reconcile_outgoing(
                &mut self.session.outgoing,
                &key,
                &self.session.conversation.thread,
            );
            if before != self.session.outgoing.len() {
                self.projected_turns.clear();
                self.mark_rows(change.turn.map_or(0, |index| index + 1)..self.list.item_count());
            }
        }
        if change.requests {
            self.sync_requests(window, cx);
            self.mark_rows(
                usize::from(!self.session.conversation.thread.is_null())
                    + array(&self.session.conversation.thread["turns"]).len()
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
        usize::from(!self.session.conversation.thread.is_null())
            + array(&self.session.conversation.thread["turns"]).len()
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
        for (index, turn) in array(&self.session.conversation.thread["turns"])
            .iter()
            .enumerate()
        {
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
        reconcile_outgoing(
            &mut self.session.outgoing,
            &key,
            &self.session.conversation.thread,
        );
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
        self.session
            .outgoing
            .iter()
            .enumerate()
            .filter(move |(_, message)| {
                message.key == key
                    && !message.turn_id.as_deref().is_some_and(|id| {
                        array(&self.session.conversation.thread["turns"])
                            .iter()
                            .any(|turn| turn["id"] == id)
                    })
            })
    }

    fn visible_requests(&self) -> impl Iterator<Item = &Value> {
        self.session.conversation.requests.iter().filter(|r| {
            r["params"]["threadId"].is_null()
                || r["params"]["threadId"] == self.session.conversation.thread["id"]
        })
    }

    fn subscribe_host(&mut self, cx: &mut Context<Self>) {
        self.models_revision = self.session.host.state.read(cx).models_revision;
        let host = self.session.host.state.clone();
        self.host_subscription = Some(cx.observe(&host, |s, host, cx| {
            let revision = host.read(cx).models_revision;
            if revision != s.models_revision {
                s.models_revision = revision;
                s.sync_models(cx);
            }
            cx.notify();
        }));
        self.sync_models(cx);
    }

    fn sync_models(&mut self, cx: &mut Context<Self>) {
        let models = &self.session.host.state.read(cx).models;
        let selected = models
            .iter()
            .find(|m| m["model"] == self.model)
            .or_else(|| models.iter().find(|m| m["isDefault"] == true))
            .or(models.first())
            .map(|m| text(m, "model").to_owned())
            .unwrap_or_default();
        self.select_model(&selected, cx);
    }

    fn refresh_threads(&self, _: &mut App) {
        host::send(&self.session.host, host::CatalogueInput::Refresh);
    }

    fn selected_model<'a>(&'a self, cx: &'a App) -> Option<&'a Value> {
        self.session
            .host
            .state
            .read(cx)
            .models
            .iter()
            .find(|model| model["model"] == self.model)
    }

    fn select_model(&mut self, id: &str, cx: &mut Context<Self>) {
        let changed = self.model != id;
        let model = self
            .session
            .host
            .state
            .read(cx)
            .models
            .iter()
            .find(|m| m["model"] == id);
        let (effort, tier, index) = supported_model_settings(
            model,
            if changed { "" } else { &self.effort },
            if changed { "" } else { &self.service_tier },
        );
        self.effort.replace_range(.., effort);
        self.service_tier.replace_range(.., tier);
        self.model.replace_range(.., id);
        let steps = model
            .map(|m| array(&m["supportedReasoningEfforts"]).len())
            .unwrap_or(0)
            .saturating_sub(1)
            .max(1);
        self.effort_slider.update(cx, |slider, cx| {
            *slider = slider::SliderState::new()
                .max(steps as f32)
                .step(1.)
                .default_value(index as f32);
            cx.notify();
        });
    }

    pub(super) fn refresh_review(&mut self) {
        if !self.session.connected || self.session.cwd.is_empty() {
            return;
        }
        let path = self.session.cwd.clone();
        let done = self.complete::<(String, Result<WorkspaceReview, AgentError>)>(
            false,
            |s, result, _, _| match result {
                Ok((path, result)) if path == s.session.cwd => match result {
                    Ok(review) => {
                        s.review = Some(std::sync::Arc::new(review));
                        s.review_error.clear();
                    }
                    Err(error) => {
                        s.review = None;
                        s.review_error = error.to_string();
                    }
                },
                Err(error) => s.review_error = error,
                _ => {}
            },
        );
        self.session.host.rpc.agent_async(
            move |client| async move {
                let review = client.review_workspace(&path).await;
                Ok((path, review))
            },
            done,
        );
    }

    fn release_content(&mut self) {
        self.stop_history_watch();
        self.images.clear();
        self.image_gallery = None;
        self.markdown_cache.clear();
        self.projected_turns.clear();
        self.dirty_rows = None;
        self.session.history_loading = false;
        self.session.history_error.clear();
        self.session.item_details.clear();
        self.diffs.clear();
        self.expanded_work.clear();
        self.expanded_items.clear();
        self.review_expanded = false;
        match tempfile::Builder::new().prefix("bex-images-").tempdir() {
            Ok(directory) => self.image_dir = directory,
            Err(e) => self.error = e.to_string(),
        }
    }

    pub(super) fn new_thread(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_recording();
        self.session.load_generation = NEXT_LOAD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.release_content();
        self.session.selected.clear();
        self.session.conversation.thread = Value::Null;
        self.session.cwd = path;
        cx.emit(Intent::Selected);
        self.restore_draft(window, cx);
        self.sync_list(true);
        self.refresh_review();
    }

    pub(super) fn open_thread(&mut self, id: String, _: &mut Window, _: &mut Context<Self>) {
        self.cancel_recording();
        self.session.load_generation = NEXT_LOAD.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let generation = self.session.load_generation;
        let done = self.complete::<Value>(true, move |s, result, w, cx| {
            if generation != s.session.load_generation {
                return;
            }
            let mut v = match result {
                Ok(result) => result,
                Err(error) => {
                    s.error = error;
                    return;
                }
            };
            s.release_content();
            s.session.selected = text(&v["thread"], "id").to_owned();
            s.session.cwd = text(&v["thread"], "cwd").into();
            s.session.conversation.thread = v["thread"].take();
            if let Some(model) = v["model"].as_str() {
                s.select_model(model, cx);
            }
            cx.emit(Intent::Selected);
            s.restore_draft(w, cx);
            s.sync_requests(w, cx);
            s.sync_list(true);
            s.refresh_review();
            s.watch_history();
        });
        self.session.host.rpc.read_thread(id, done);
    }

    fn stop_history_watch(&mut self) {
        if let Some(watch_id) = self.session.history_watch.take() {
            let key = self.draft_scope as u64;
            self.session.host.rpc.agent_async(
                move |client| async move { client.unwatch_thread(key, watch_id).await },
                |_| {},
            );
        }
        self.session.history_refresh_pending = false;
        self.session.history_refresh_dirty = false;
    }

    fn watch_history(&mut self) {
        let Some(path) =
            conversation_presentation::state::watch_path(&self.session.conversation.thread)
                .map(str::to_owned)
        else {
            return;
        };
        let generation = self.session.load_generation;
        let thread = self.session.selected.clone();
        let key = self.draft_scope as u64;
        self.session.history_watch = Some(generation);
        let done = self.complete::<()>(false, move |s, result, w, cx| {
            if s.session.history_watch != Some(generation) {
                return;
            }
            match result {
                Ok(_) => s.queue_history_refresh(w, cx),
                Err(error) => {
                    s.error = format!("会話の自動更新を開始できません: {error}");
                    s.session.history_watch = None;
                }
            }
        });
        self.session.host.rpc.agent_async(
            move |client| async move { client.watch_thread(&thread, key, generation, &path).await },
            done,
        );
    }

    fn queue_history_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.session.history_refresh_dirty = true;
        if self.session.history_refresh_pending {
            return;
        }
        self.session.history_refresh_pending = true;
        let generation = self.session.load_generation;
        cx.spawn_in(window, async move |view, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            let _ = view.update_in(cx, |s, _, _| {
                if s.session.history_watch != Some(generation) {
                    return;
                }
                s.session.history_refresh_dirty = false;
                let revision = s.session.conversation_revision;
                let id = s.session.selected.clone();
                let done = s.complete::<Value>(false, move |s, result, w, cx| {
                    if s.session.history_watch != Some(generation) {
                        return;
                    }
                    s.session.history_refresh_pending = false;
                    if revision != s.session.conversation_revision {
                        s.session.history_refresh_dirty = true;
                    } else {
                        match result.and_then(|mut value| {
                            let (next, result) = agent_client::conversation::refresh_history(
                                std::mem::take(&mut s.session.conversation),
                                value["thread"].take(),
                            );
                            s.session.conversation = next;
                            result
                        }) {
                            Ok(()) => {
                                s.projected_turns.clear();
                                s.sync_list(false);
                                s.refresh_sources(cx);
                            }
                            Err(error) => s.error = format!("会話を更新できません: {error}"),
                        }
                    }
                    if conversation_presentation::state::watch_path(&s.session.conversation.thread)
                        .is_none()
                    {
                        s.stop_history_watch();
                    } else if s.session.history_refresh_dirty {
                        s.queue_history_refresh(w, cx);
                    }
                });
                s.session.host.rpc.read_thread(id, done);
            });
        })
        .detach();
    }

    fn load_older(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.session.history_loading {
            return;
        }
        let Some(page) = self.session.conversation.older_page() else {
            return;
        };
        self.session.history_loading = true;
        self.session.history_error.clear();
        self.list.remeasure_items(0..1);
        let generation = self.session.load_generation;
        let id = self.session.selected.clone();
        let done = self.complete::<(conversation::HistoryPage, Result<Value, AgentError>)>(
            false,
            move |s, result, w, cx| {
                if generation != s.session.load_generation {
                    return;
                }
                s.session.history_loading = false;
                let (page, result) = match result {
                    Ok((page, result)) => (page, result.map_err(|error| error.to_string())),
                    Err(error) => {
                        s.session.history_error = error;
                        return;
                    }
                };
                let anchor = s.list.logical_scroll_top();
                let old_height = s
                    .list
                    .bounds_for_item(anchor.item_ix)
                    .map(|bounds| bounds.size.height);
                let anchor_is_changed = page.turn.as_ref().is_some_and(|id| {
                    anchor.item_ix > 0
                        && s.session.conversation.thread["turns"][anchor.item_ix - 1]["id"] == *id
                });
                match result.and_then(|page_value| {
                    let (next, result) = agent_client::conversation::merge_older(
                        std::mem::take(&mut s.session.conversation),
                        page_value,
                        &page,
                    );
                    s.session.conversation = next;
                    result
                }) {
                    Ok(added) => {
                        s.session.conversation_revision += 1;
                        s.projected_turns.clear();
                        if added > 0 {
                            s.list.splice(1..1, added);
                        }
                        s.list.remeasure();
                        s.refresh_sources(cx);
                        // Whole-turn prepends are handled by ListState::splice.
                        // Within a turn, retain the old visible suffix after layout.
                        if anchor_is_changed && let Some(old_height) = old_height {
                            let view = cx.entity().downgrade();
                            w.on_next_frame(move |_, cx| {
                                let _ = view.update(cx, |s, cx| {
                                    if generation != s.session.load_generation {
                                        return;
                                    }
                                    if let Some(bounds) = s.list.bounds_for_item(anchor.item_ix) {
                                        s.list.scroll_to(ListOffset {
                                            item_ix: anchor.item_ix,
                                            offset_in_item: anchor.offset_in_item
                                                + bounds.size.height
                                                - old_height,
                                        });
                                        cx.notify();
                                    }
                                });
                            });
                        }
                    }
                    Err(error) => {
                        s.session.history_error = error;
                        s.list.remeasure_items(0..1);
                    }
                }
            },
        );
        self.session.host.rpc.agent_async(
            move |client| async move {
                let result = client
                    .read_older(&id, page.cursor.as_str(), page.turn.as_deref(), true)
                    .await;
                Ok((page, result))
            },
            done,
        );
        cx.notify();
    }

    fn load_detail(&mut self, turn_id: String, item_id: String) {
        let key = (turn_id.clone(), item_id.clone());
        if self
            .session
            .item_details
            .get(&key)
            .is_some_and(|state| state.error.is_none())
        {
            return;
        }
        let deferred = array(&self.session.conversation.thread["turns"])
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
        self.session.detail_request += 1;
        let request = self.session.detail_request;
        self.session.item_details.insert(
            key.clone(),
            DetailLoad {
                request,
                error: None,
            },
        );
        let generation = self.session.load_generation;
        let id = self.session.selected.clone();
        let done = self.complete::<Value>(false, move |s, result, _, _| {
            if generation != s.session.load_generation
                || !s
                    .session
                    .item_details
                    .get(&key)
                    .is_some_and(|state| state.request == request && state.error.is_none())
            {
                return;
            }
            match result.and_then(|mut value| {
                let (next, result) = agent_client::conversation::apply_detail(
                    std::mem::take(&mut s.session.conversation),
                    &key.0,
                    &key.1,
                    value["item"].take(),
                );
                s.session.conversation = next;
                result
            }) {
                Ok(_) => {
                    s.session.conversation_revision += 1;
                    s.session.item_details.remove(&key);
                    s.projected_turns.clear();
                }
                Err(error) => {
                    s.session.item_details.get_mut(&key).unwrap().error = Some(error);
                }
            }
            if s.expanded_items.contains(&key.1) {
                s.pause_tail();
            }
            s.remeasure_item(&key.0);
        });
        self.session.host.rpc.agent_async(
            move |client| async move { client.read_item(&id, &turn_id, &item_id).await },
            done,
        );
    }

    pub(super) fn switch_host(
        &mut self,
        host: Rc<host::Connection>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_history_watch();
        self.session.epoch += 1;
        self.session.connected = host.state.read(cx).connected;
        self.session.host = host;
        self.busy = 0;
        self.model.clear();
        self.subscribe_host(cx);
        self.session.conversation = Conversation::default();
        self.session.outgoing.clear();
        self.review = None;
        self.new_thread(String::new(), window, cx);
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        if let Some(dictation) = &self.dictation {
            if dictation.phase == Phase::Recording {
                self.finish_dictation(true, cx);
            }
            return;
        }
        if !self.session.connected || self.busy > 0 {
            return;
        }
        let message = self.composer.read(cx).value().to_string();
        let key = self.draft_key();
        let files =
            array(&self.drafts.read(cx).cache(self.draft_scope)["attachments"][&key]).to_vec();
        self.submit(message, files, None, cx);
    }

    fn submit(
        &mut self,
        message: String,
        files: Vec<Value>,
        dictated_text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let key = self.draft_key();
        if message.trim().is_empty() && files.is_empty() && dictated_text.is_none() {
            return;
        }
        let input = if let Some(text) = &dictated_text {
            message_input(&dictation::append_text(&message, text), &files)
        } else {
            message_input(&message, &files)
        };
        let outgoing_id = uuid::Uuid::new_v4().to_string();
        self.session.outgoing.push(OutgoingMessage {
            id: outgoing_id.clone(),
            key: key.clone(),
            item: json!({"id":outgoing_id,"clientId":outgoing_id,"type":"userMessage","content":input}),
            turn_id: self.session.conversation.active().map(|turn| text(turn, "id").to_owned()),
            after_item_id: self.session.conversation.active().and_then(|turn| array(&turn["items"]).last()).map(|item| text(item, "id").to_owned()),
            accepted: false,
        });
        let submission = Submission {
            id: outgoing_id,
            key,
            message,
            dictated_text,
            files,
            input,
            model: self.model.clone(),
            effort: self.effort.clone(),
            service_tier: self.service_tier.clone(),
        };
        self.sync_list(false);
        self.list.scroll_to_end();
        cx.notify();
        if self.session.selected.is_empty() {
            let generation = self.session.load_generation;
            let cwd = self.session.cwd.clone();
            let done = self.complete::<(Submission, Result<Value, AgentError>)>(
                true,
                move |s, result, w, cx| {
                    let (submission, result) = match result {
                        Ok(result) => result,
                        Err(error) => {
                            s.error = error;
                            return;
                        }
                    };
                    let mut v = match result {
                        Ok(value) => value,
                        Err(error) => {
                            s.fail_send(&submission, error.to_string(), w, cx);
                            return;
                        }
                    };
                    let id = text(&v["thread"], "id").to_owned();
                    let plan =
                        conversation_presentation::state::plan_send(&v["thread"], &Value::Null);
                    let target = s.thread_draft_key(&id);
                    if let Some(outgoing) = s
                        .session
                        .outgoing
                        .iter_mut()
                        .find(|m| m.id == submission.id)
                    {
                        outgoing.key = target.clone();
                    }
                    let scope = s.draft_scope;
                    drafts::update(&s.drafts, scope, cx, |mut cache| {
                        cache["messages"][&target] = json!(submission.message);
                        cache["attachments"][&target] = json!(submission.files);
                        cache
                    });
                    if generation == s.session.load_generation {
                        s.session.selected = id.clone();
                        s.session.cwd = text(&v["thread"], "cwd").to_owned();
                        s.review = None;
                        s.review_error.clear();
                        cx.emit(Intent::Selected);
                        s.refresh_review();
                        s.session.conversation.thread = v["thread"].take();
                        s.restore_draft(w, cx);
                        s.sync_list(true);
                    }
                    // Install the snapshot before sending: notifications may precede the turn/start reply.
                    s.send_turn(id, plan, submission);
                },
            );
            self.session.host.rpc.agent_async(
                move |client| async move {
                    let result = client
                        .start_thread(
                            &cwd,
                            (!submission.model.is_empty()).then_some(submission.model.as_str()),
                        )
                        .await;
                    Ok((submission, result))
                },
                done,
            );
        } else {
            // Selection installs the current read/start snapshot before sending.
            let plan = conversation_presentation::state::plan_send(
                &self.session.conversation.thread,
                &Value::Null,
            );
            self.send_turn(self.session.selected.clone(), plan, submission);
        }
    }

    fn send_turn(
        &mut self,
        id: String,
        plan: conversation_presentation::state::SendPlan,
        submission: Submission,
    ) {
        let done = self.complete::<(String, Submission, Result<Option<String>, AgentError>)>(
            true,
            move |s, result, w, cx| {
                let (id, submission, result) = match result {
                    Ok(result) => result,
                    Err(error) => {
                        s.error = error;
                        return;
                    }
                };
                let turn_id = match result {
                    Ok(value) => value,
                    Err(error) => {
                        s.fail_send(&submission, error.to_string(), w, cx);
                        return;
                    }
                };
                if let Some(outgoing) = s
                    .session
                    .outgoing
                    .iter_mut()
                    .find(|m| m.id == submission.id)
                {
                    outgoing.accepted = true;
                    if outgoing.turn_id.is_none() {
                        outgoing.turn_id = turn_id;
                    }
                }
                let target = s.thread_draft_key(&id);
                let scope = s.draft_scope;
                drafts::update(&s.drafts, scope, cx, |mut cache| {
                    for draft_key in [&submission.key, &target] {
                        if cache["messages"][draft_key] == submission.message {
                            cache["messages"].as_object_mut().unwrap().remove(draft_key);
                        }
                        if let Some(current) = cache["attachments"][draft_key].as_array_mut() {
                            current.retain(|f| {
                                !submission
                                    .files
                                    .iter()
                                    .any(|sent| sent["path"] == f["path"])
                            });
                        }
                    }
                    cache
                });
                if s.session.selected == id {
                    s.restore_draft(w, cx);
                }
                s.sync_list(false);
                s.refresh_threads(cx);
            },
        );
        self.session.host.rpc.agent_async(
            move |client| async move {
                let result = client
                    .send_turn(
                        &id,
                        plan,
                        &submission.input,
                        &submission.id,
                        agent_client::operations::TurnOptions {
                            model: (!submission.model.is_empty())
                                .then_some(submission.model.as_str()),
                            effort: (!submission.effort.is_empty())
                                .then_some(submission.effort.as_str()),
                            service_tier_for_turn: Some(&submission.service_tier),
                        },
                    )
                    .await;
                Ok((id, submission, result))
            },
            done,
        );
    }

    fn fail_send(
        &mut self,
        submission: &Submission,
        error: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(transcript) = &submission.dictated_text {
            let target = self
                .session
                .outgoing
                .iter()
                .find(|m| m.id == submission.id)
                .map(|m| m.key.clone());
            let scope = self.draft_scope;
            drafts::update(&self.drafts, scope, cx, |mut cache| {
                for key in std::iter::once(&submission.key)
                    .chain(target.as_ref().filter(|key| *key != &submission.key))
                {
                    cache["messages"][key] = json!(dictation::append_text(
                        text(&cache["messages"], key),
                        transcript
                    ));
                }
                cache
            });
            self.restore_draft(window, cx);
        }
        self.session
            .outgoing
            .retain(|message| message.id != submission.id);
        self.error = error;
        self.sync_list(false);
    }

    pub(super) fn pick_folder(&mut self) {
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

    pub(super) fn attach(&mut self) {
        self.attach_sources(|| platform::choose("file").map(|file| file.into_iter().collect()));
    }

    fn attach_sources(
        &mut self,
        sources: impl FnOnce() -> Result<Vec<String>, String> + Send + 'static,
    ) {
        let remote = self.session.host.remote.clone();
        let cwd = self.session.cwd.clone();
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
        }, |s, v, _, cx| {
            let files = array(&v["files"]);
            if files.is_empty() { return; }
            let key = text(&v, "key");
            let scope = s.draft_scope;
            drafts::update(&s.drafts, scope, cx, |mut cache| {

            if !cache["attachments"][key].is_array() {
                cache["attachments"][key] = json!([]);
            }
            cache["attachments"][key].as_array_mut().unwrap().extend_from_slice(files);
             cache });
            s.refresh_sources(cx);
        });
    }

    fn download(&mut self, source: String) {
        let remote = self.session.host.remote.clone();
        let manager = self.manager.clone();
        self.work(true,move||{let Some(destination)=platform::choose("download")? else{return Ok(Value::Null);};if remote.is_empty(){let mut input=std::fs::File::open(source).map_err(|e|e.to_string())?;let mut out=std::fs::OpenOptions::new().write(true).create_new(true).open(&destination).map_err(|e|e.to_string())?;std::io::copy(&mut input,&mut out).map_err(|e|e.to_string())?;}else{manager.request("host/transfer",json!({"profileId":remote,"direction":"download","source":source,"destination":destination}))?;}Ok(Value::Null)},|_,_,_,_|{});
    }

    fn sync_requests(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.requests.retain(|key, _| {
            self.session
                .conversation
                .requests
                .iter()
                .any(|r| r["id"] == *key)
        });
        for request in &self.session.conversation.requests {
            let key = &request["id"];
            if self.requests.contains_key(key) {
                continue;
            }
            let questions = request_presentation(text(request, "method"), &request["params"])
                .questions
                .into_iter()
                .map(|q| Question {
                    id: q.id.into(),
                    prompt: q.prompt.into(),
                    options: q.options.into_iter().map(str::to_owned).collect(),
                    input: cx.new(|cx| InputState::new(window, cx).placeholder("回答を入力")),
                })
                .collect();
            self.requests.insert(
                key.clone(),
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

    fn respond(&mut self, id: Value, answer: RequestAnswer<'static>) {
        let Some(request) = self
            .session
            .conversation
            .requests
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
        else {
            return;
        };
        let done = self.complete(true, move |s, result, _, _| match result {
            Ok(()) => {
                if let Some(input) = s.requests.get_mut(&id) {
                    input.sent = true;
                }
            }
            Err(error) => s.error = error,
        });
        self.session.host.rpc.agent_async(
            move |client| async move { client.respond(&request, answer).await },
            done,
        );
    }

    pub(super) fn gallery_open(&self) -> bool {
        self.image_gallery.is_some()
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
    agent_client::operations::message_input(
        message,
        files.iter().map(|file| {
            let path = text(file, "path");
            let is_image = Path::new(path)
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| {
                    ["png", "jpg", "jpeg", "gif", "webp", "heic"]
                        .iter()
                        .any(|image| ext.eq_ignore_ascii_case(image))
                });
            agent_client::operations::Attachment {
                path: path.into(),
                name: text(file, "name").into(),
                is_image,
            }
        }),
    )
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

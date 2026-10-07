//! The desktop window: one Host connection, the views agent-core derives from
//! its snapshots, and the screens that draw them.
mod attachments;
mod composer;
mod dialogs;
mod dictation;
mod header;
mod hosts;
mod menus;
mod panel;
mod settings;
mod sidebar;
mod timeline;
mod ui;

pub(crate) use ui::{apply_appearance, color};

use crate::{Runtime, platform, store_session::StoreSession};
use agent_core::{
    connection::{Outcome, StoreOptions},
    state::{Intent, Snapshot},
    view::{
        new_thread::NewThreadView,
        sidebar::{SidebarOptions, SidebarView},
        thread::{ThreadView, ThreadViewOptions},
        timeline::rows::TimelineLayout,
    },
};
use agent_protocol::models::RemoteHost;
use gpui_kit::{
    component::{WindowExt, h_flex, notification::Notification, v_flex},
    prelude::FluentBuilder,
    *,
};
use hosts::{HostEvent, Hosts};
use std::{path::PathBuf, sync::Arc};

/// Runs once a dispatched intent resolves.
type Done = Box<
    dyn FnOnce(&mut Desktop, &Result<Outcome, String>, &mut Window, &mut Context<Desktop>) + Send,
>;

enum Update {
    Connected(Result<(StoreSession, PathBuf), String>),
    Snapshot(Arc<Snapshot>),
    Views(Box<Views>),
    Completed(Option<Done>, Result<Outcome, String>),
    PersistenceError(String),
    Attachments(attachments::Update),
    Recording(uuid::Uuid, platform::RecordingEvent),
    Transcribed(uuid::Uuid, Result<Outcome, String>),
    Tick,
}

/// Which screen the main area shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Chat,
    Settings,
}

/// What the views were derived from, so a newer request replaces them.
#[derive(Clone, PartialEq)]
struct ViewInputs {
    sidebar: SidebarOptions,
    thread: ThreadViewOptions,
}

/// The views of one snapshot, derived off the main thread.
pub(crate) struct Views {
    /// The snapshot revision they were derived from.
    pub(crate) revision: u64,
    pub(crate) generation: u64,
    pub(crate) now_ms: i64,
    pub(crate) sidebar: SidebarView,
    /// The selected thread's screen.
    pub(crate) thread: Option<ThreadView>,
    /// The new-thread draft while no thread is selected.
    pub(crate) new_thread: Option<NewThreadView>,
}
impl Views {
    fn derive(snapshot: &Snapshot, inputs: &ViewInputs, generation: u64, now_ms: i64) -> Self {
        Self {
            revision: snapshot.revision,
            generation,
            now_ms,
            sidebar: snapshot.sidebar(now_ms, inputs.sidebar.clone()),
            thread: snapshot.selected_thread(now_ms, inputs.thread.clone()),
            new_thread: snapshot
                .selected_thread
                .is_none()
                .then(|| snapshot.new_thread(inputs.thread.composer.clone())),
        }
    }
}

pub(crate) struct Desktop {
    pub(crate) session: Option<StoreSession>,
    pub(crate) snapshot: Arc<Snapshot>,
    pub(crate) views: Arc<Views>,
    pub(crate) runtime: Runtime,
    updates: async_channel::Sender<(u64, Update)>,
    epoch: u64,
    pub(crate) connecting: bool,
    pub(crate) remote: Option<RemoteHost>,
    pub(crate) hosts: Entity<Hosts>,
    pub(crate) route: Route,
    pub(crate) sidebar_hidden: bool,
    generation: u64,
    views_running: bool,
    shown_error: Option<String>,
    pub(crate) sidebar: sidebar::SidebarState,
    pub(crate) header: header::HeaderState,
    pub(crate) timeline: timeline::TimelineState,
    pub(crate) composer: composer::ComposerState,
    pub(crate) panels: panel::PanelState,
    pub(crate) settings: settings::SettingsState,
    pub(crate) menus: menus::MenuState,
    pub(crate) attachments: attachments::AttachmentCache,
    pub(crate) dictation: Option<dictation::Dictation>,
    tick: Option<tokio_util::task::AbortOnDropHandle<()>>,
    _subscriptions: Vec<Subscription>,
}

/// Keys every window binds; screens handle their own focus-specific keys.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "ctrl-v",
        gpui_kit::component::input::Paste,
        Some("ChatComposer > Input"),
    )]);
}

impl Desktop {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = cx.global::<Runtime>().clone();
        let (updates, incoming) = async_channel::bounded(64);
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
        let hosts = cx.new(|cx| Hosts::new(window, cx));
        let mut subscriptions = vec![
            cx.subscribe_in(&hosts, window, |view, _, event, window, cx| {
                match event {
                    HostEvent::Selected(remote) => {
                        if view.remote.as_ref().map(|r| (&r.id, &r.ticket))
                            != remote.as_ref().map(|r| (&r.id, &r.ticket))
                            || !view.snapshot.connected
                        {
                            view.connect(remote.clone(), window, cx);
                        }
                    }
                    HostEvent::Removed(id) if view.remote.as_ref().is_some_and(|r| &r.id == id) => {
                        view.connect(None, window, cx)
                    }
                    _ => {}
                }
                cx.notify();
            }),
            cx.observe_window_appearance(window, |view, window, cx| {
                apply_appearance(window.appearance(), cx);
                view.refresh_views(cx);
            }),
        ];
        let sidebar = sidebar::SidebarState::new(window, cx);
        let header = header::HeaderState::new(window, cx);
        let timeline = timeline::TimelineState::new(window, cx);
        let composer = composer::ComposerState::new(window, cx, &mut subscriptions);
        let panels = panel::PanelState::new(window, cx);
        let settings = settings::SettingsState::new(window, cx);
        let menus = menus::MenuState::new(window, cx);
        let mut view = Self {
            session: None,
            snapshot: Arc::default(),
            views: Arc::new(Views::derive(
                &Snapshot::default(),
                &ViewInputs {
                    sidebar: SidebarOptions::default(),
                    thread: ThreadViewOptions::default(),
                },
                0,
                ui::now_ms(),
            )),
            runtime,
            updates,
            epoch: 0,
            connecting: false,
            remote: None,
            hosts,
            route: Route::Chat,
            sidebar_hidden: false,
            generation: 0,
            views_running: false,
            shown_error: None,
            sidebar,
            header,
            timeline,
            composer,
            panels,
            settings,
            menus,
            attachments: attachments::AttachmentCache::new(),
            dictation: None,
            tick: None,
            _subscriptions: subscriptions,
        };
        if let Some(error) = view.runtime.logging_error.clone() {
            view.show_error(&error, window, cx);
        }
        view.connect(None, window, cx);
        view
    }

    /// Connects to the local Host, or to `remote`, restoring this device's
    /// state and the Host's disk cache.
    pub(crate) fn connect(
        &mut self,
        remote: Option<RemoteHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attachments.clear();
        self.dictation = None;
        self.epoch += 1;
        self.views_running = false;
        self.connecting = true;
        self.remote = remote;
        self.session.take();
        self.snapshot = Arc::default();
        self.disconnected(window, cx);
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
            let state = (|| -> anyhow::Result<(PathBuf, Snapshot, StoreOptions)> {
                let directory = platform::state_dir().map_err(anyhow::Error::msg)?;
                let path = directory.join(format!("device-{name}.json"));
                let bytes = std::fs::read(&path).unwrap_or_default();
                let preferences = std::fs::read(path.with_file_name("model-preferences.json"))
                    .unwrap_or_default();
                let snapshot = agent_core::persistence::recover(&bytes, &preferences);
                let options = StoreOptions {
                    cache_directory: Some(directory.join("cache").join(&name)),
                    ..StoreOptions::default()
                };
                Ok((path, snapshot, options))
            })();
            match state {
                Err(error) => {
                    let _ = updates
                        .send((epoch, Update::Connected(Err(error.to_string()))))
                        .await;
                }
                Ok((path, snapshot, options)) => {
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
                        connections
                            .connect(ticket.as_deref(), snapshot, options)
                            .await,
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

    /// Sends an intent to the Host connection's owner.
    pub(crate) fn perform(&self, intent: Intent) {
        self.dispatch(intent, None);
    }

    /// Sends an intent and runs `done` once it resolves.
    pub(crate) fn perform_then(
        &self,
        intent: Intent,
        done: impl FnOnce(&mut Desktop, &Result<Outcome, String>, &mut Window, &mut Context<Desktop>)
        + Send
        + 'static,
    ) {
        self.dispatch(intent, Some(Box::new(done)));
    }

    fn dispatch(&self, intent: Intent, done: Option<Done>) {
        let Some(session) = &self.session else {
            return;
        };
        let receipt = session.store.dispatch(intent);
        let updates = self.updates.clone();
        let epoch = self.epoch;
        self.runtime.handle.spawn(async move {
            let result = receipt
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r.map_err(|e| e.to_string()));
            let _ = updates.send((epoch, Update::Completed(done, result))).await;
        });
    }

    /// The Host connection's store, once connected.
    pub(crate) fn store(&self) -> Option<Arc<agent_core::connection::Store>> {
        self.session.as_ref().map(|session| session.store.clone())
    }

    /// Spawns `task` on the async runtime and delivers its result to `done`
    /// on the window, unless the connection changed meanwhile.
    pub(crate) fn spawn_task<T: Send + 'static>(
        &self,
        task: impl std::future::Future<Output = T> + Send + 'static,
        done: impl FnOnce(&mut Desktop, T, &mut Window, &mut Context<Desktop>) + Send + 'static,
    ) {
        let updates = self.updates.clone();
        let epoch = self.epoch;
        self.runtime.handle.spawn(async move {
            let value = task.await;
            let done: Done = Box::new(move |view, _, window, cx| done(view, value, window, cx));
            let _ = updates
                .send((epoch, Update::Completed(Some(done), Ok(Outcome::Applied))))
                .await;
        });
    }

    /// An error the user should read, shown once per message.
    pub(crate) fn show_error(&mut self, error: &str, window: &mut Window, cx: &mut Context<Self>) {
        let message = agent_core::presentation::error::error_message(error);
        if message.is_empty() {
            return;
        }
        self.shown_error = Some(message.clone());
        window.push_notification(Notification::error(message), cx);
    }

    /// Derives the views again after a change to what they read besides the
    /// snapshot, such as an expanded row.
    pub(crate) fn refresh_views(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.schedule_views(cx);
    }

    fn view_inputs(&self) -> ViewInputs {
        ViewInputs {
            sidebar: self.sidebar.options(),
            thread: ThreadViewOptions {
                layout: TimelineLayout::Desktop,
                disclosure: self.timeline.disclosure(),
                panels: self.panels.header_panels(),
                composer: self.composer.options(),
                show_scroll_to_end: false,
            },
        }
    }

    fn schedule_views(&mut self, cx: &mut Context<Self>) {
        if self.views_running {
            return;
        }
        if self.views.revision == self.snapshot.revision
            && self.views.generation == self.generation
            && ui::now_ms() - self.views.now_ms < 1_000
        {
            return;
        }
        self.views_running = true;
        let snapshot = self.snapshot.clone();
        let inputs = self.view_inputs();
        let generation = self.generation;
        let epoch = self.epoch;
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            if let Ok(views) = tokio::task::spawn_blocking(move || {
                Views::derive(&snapshot, &inputs, generation, ui::now_ms())
            })
            .await
            {
                let _ = updates.send((epoch, Update::Views(Box::new(views)))).await;
            }
        });
        cx.notify();
    }

    fn receive(&mut self, update: Update, window: &mut Window, cx: &mut Context<Self>) {
        match update {
            Update::Tick => {
                self.schedule_views(cx);
                cx.notify();
                return;
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
                self.perform(Intent::LoadAccounts);
                self.perform(Intent::LoadConversationSettings);
                self.snapshot_changed(window, cx);
            }
            Update::Connected(Err(error)) => {
                self.connecting = false;
                self.show_error(&error, window, cx);
            }
            Update::Snapshot(snapshot) => {
                if !snapshot.accepts_after(&self.snapshot) {
                    return;
                }
                self.snapshot = snapshot;
                if let Some(session) = &self.session {
                    session.save(self.snapshot.clone());
                }
                self.snapshot_changed(window, cx);
            }
            Update::Views(views) => {
                self.views_running = false;
                if views.revision <= self.snapshot.revision {
                    let previous = std::mem::replace(&mut self.views, Arc::new(*views));
                    self.views_changed(&previous, window, cx);
                }
                self.schedule_views(cx);
            }
            Update::Completed(done, result) => {
                if let Some(session) = &self.session {
                    let snapshot = session.store.snapshot();
                    if snapshot.accepts_after(&self.snapshot) {
                        self.snapshot = snapshot;
                    }
                    session.save(self.snapshot.clone());
                }
                match (&result, done.is_some()) {
                    (Ok(outcome), _) => self.outcome(outcome, window, cx),
                    (Err(error), false) => {
                        let error = error.clone();
                        self.show_error(&error, window, cx);
                    }
                    (Err(_), true) => {}
                }
                if let Some(done) = done {
                    done(self, &result, window, cx);
                }
                self.snapshot_changed(window, cx);
            }
            Update::PersistenceError(error) => self.show_error(&error, window, cx),
            Update::Attachments(update) => self.attachments_update(update, window, cx),
            Update::Recording(id, event) => self.recording_update(id, event, window, cx),
            Update::Transcribed(id, result) => {
                if self.dictation.as_ref().is_some_and(|d| d.id == id) {
                    self.dictation = None;
                }
                if let Some(session) = &self.session {
                    self.snapshot = session.store.snapshot();
                }
                if let Err(error) = result {
                    self.show_error(&error, window, cx);
                }
                self.snapshot_changed(window, cx);
            }
        }
        cx.notify();
    }

    fn outcome(&mut self, outcome: &Outcome, window: &mut Window, cx: &mut Context<Self>) {
        self.composer_outcome(outcome, window, cx);
        self.panel_outcome(outcome, window, cx);
    }

    fn snapshot_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(error) = self.snapshot.error.clone()
            && self.shown_error.as_deref()
                != Some(agent_core::presentation::error::error_message(&error).as_str())
        {
            self.show_error(&error, window, cx);
        }
        self.sync_composer(window, cx);
        self.sync_panels(window, cx);
        self.sync_settings(window, cx);
        self.schedule_views(cx);
    }

    fn views_changed(&mut self, previous: &Views, window: &mut Window, cx: &mut Context<Self>) {
        self.timeline_views_changed(previous, window, cx);
        self.sync_composer(window, cx);
        self.sync_panels(window, cx);
    }

    /// The connection went away; screens drop what belonged to it.
    fn disconnected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.timeline.reset();
        self.panels.reset(window, cx);
        self.sync_composer(window, cx);
    }

    /// The selected thread's id.
    pub(crate) fn thread_id(&self) -> Option<String> {
        self.snapshot
            .selected_thread
            .as_ref()
            .map(ToString::to_string)
    }

    /// Opens a thread, leaving settings.
    pub(crate) fn open_thread(&mut self, thread_id: String, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.perform(Intent::OpenThread { thread_id });
        cx.notify();
    }

    /// Starts a new-thread draft in `project_id`, leaving settings.
    pub(crate) fn new_thread(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.perform(Intent::NewThread { project_id });
        cx.notify();
    }

    fn global_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if !modifiers.secondary() {
            return false;
        }
        let key = keystroke.key.as_str();
        match (key, modifiers.shift, modifiers.alt) {
            ("b", false, false) => {
                self.sidebar_hidden = !self.sidebar_hidden;
                cx.notify();
            }
            ("b", false, true) => self.toggle_right_panel(window, cx),
            ("j", false, false) => self.toggle_terminal_drawer(window, cx),
            ("n", false, false) | ("o", true, false) => {
                let project = self
                    .snapshot
                    .selected_thread
                    .as_ref()
                    .and_then(|thread| self.snapshot.thread_project(thread))
                    .map(str::to_owned)
                    .or_else(|| self.snapshot.selected_project.clone());
                self.new_thread(project, cx);
            }
            ("[", true, false) => self.select_adjacent_thread(false, cx),
            ("]", true, false) => self.select_adjacent_thread(true, cx),
            _ => return false,
        }
        true
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = ui::metrics();
        let main = match self.route {
            Route::Settings => self.render_settings(window, cx),
            Route::Chat => {
                let body = if self.snapshot.selected_thread.is_some() {
                    v_flex()
                        .flex_1()
                        .min_h_0()
                        .child(self.render_timeline(window, cx))
                        .child(self.render_composer(window, cx))
                        .into_any_element()
                } else {
                    self.render_new_thread(window, cx)
                };
                let column = v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_header(window, cx))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .child(body)
                                    .children(self.render_terminal_drawer(window, cx)),
                            )
                            .children(self.render_thread_details(window, cx)),
                    );
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(column)
                    .children(self.render_right_panel(window, cx))
                    .into_any_element()
            }
        };
        div()
            .id("desktop")
            .key_context("Desktop")
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if view.global_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .relative()
            .size_full()
            .bg(color("canvas"))
            .text_color(color("text"))
            .text_size(px(metrics.prompt_size))
            .child(
                h_flex()
                    .size_full()
                    .when(!self.sidebar_hidden, |root| {
                        root.child(match self.route {
                            Route::Settings => self.render_settings_nav(window, cx),
                            Route::Chat => self.render_sidebar(window, cx),
                        })
                    })
                    .child(main),
            )
            .children(gpui_kit::component::Root::render_dialog_layer(window, cx))
            .children(gpui_kit::component::Root::render_notification_layer(
                window, cx,
            ))
    }
}

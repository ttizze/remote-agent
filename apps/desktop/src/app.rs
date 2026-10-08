//! The desktop window: one Host connection, the views agent-core derives from
//! its snapshots, and the screens that draw them.
mod attachments;
mod composer;
mod dialogs;
mod dictation;
mod header;
mod hosts;
mod keymap;
mod menus;
mod panel;
mod settings;
mod sidebar;
mod timeline;
mod ui;

pub(crate) use ui::{apply_appearance, color, diff_colors, load_appearance, terminal_font};

/// Whether long lines wrap by default.
pub(crate) fn ui_word_wrap() -> bool {
    ui::appearance().word_wrap
}

use crate::{Runtime, platform, store_session::StoreSession};
use agent_core::{
    connection::{Outcome, StoreOptions},
    environment::{
        EnvironmentInboxView, EnvironmentRegistry, EnvironmentSettingsView, EnvironmentSidebarView,
    },
    state::{Intent, Snapshot},
    view::{
        new_thread::NewThreadView,
        sidebar::{SidebarOptions, SidebarView},
        thread::{ThreadView, ThreadViewOptions},
        thread_menu::ThreadMenuItemId,
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
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// Runs once a dispatched intent resolves.
type Done = Box<
    dyn FnOnce(&mut Desktop, &Result<Outcome, String>, &mut Window, &mut Context<Desktop>) + Send,
>;

enum Update {
    Connected(Result<(StoreSession, PathBuf), String>),
    Snapshot(Arc<Snapshot>),
    EnvironmentConnected {
        profile_id: String,
        result: Result<(StoreSession, PathBuf), String>,
    },
    EnvironmentSnapshot {
        profile_id: String,
        snapshot: Arc<Snapshot>,
    },
    EnvironmentPersistenceError {
        profile_id: String,
        error: String,
    },
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
    /// Combined rows and awareness from every environment owned by this app.
    pub(crate) environment_sidebar: EnvironmentSidebarView,
    pub(crate) environment_inbox: EnvironmentInboxView,
    pub(crate) environment_settings: EnvironmentSettingsView,
    /// The selected thread's screen.
    pub(crate) thread: Option<ThreadView>,
    /// The new-thread draft while no thread is selected.
    pub(crate) new_thread: Option<NewThreadView>,
}
impl Views {
    fn derive(
        snapshot: &Snapshot,
        environments: &EnvironmentRegistry,
        inputs: &ViewInputs,
        generation: u64,
        now_ms: i64,
    ) -> Self {
        Self {
            revision: snapshot.revision,
            generation,
            now_ms,
            sidebar: snapshot.sidebar(now_ms, inputs.sidebar.clone()),
            environment_sidebar: environments.sidebar_filtered(
                now_ms,
                inputs.sidebar.clone(),
                &snapshot.search,
            ),
            environment_inbox: environments.inbox_filtered(
                now_ms,
                inputs.sidebar.clone(),
                &snapshot.search,
            ),
            environment_settings: environments.settings(),
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
    /// Immutable projections for every authenticated Host store.
    pub(crate) environment_registry: EnvironmentRegistry,
    pub(crate) views: Arc<Views>,
    pub(crate) runtime: Runtime,
    updates: async_channel::Sender<(u64, Update)>,
    epoch: u64,
    pub(crate) connecting: bool,
    pub(crate) remote: Option<RemoteHost>,
    /// Secondary Host stores stay alive independently of the selected route.
    background_sessions: BTreeMap<String, StoreSession>,
    background_connecting: BTreeSet<String>,
    background_retry_at: BTreeMap<String, (Instant, u32)>,
    selected_retry_at: Option<(Instant, u32)>,
    profile_environment_ids: BTreeMap<String, String>,
    pending_open: Option<(String, String)>,
    pending_new_thread: Option<(String, Option<String>)>,
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
    /// Decoded project icons, by content hash.
    pub(crate) project_icons: std::cell::RefCell<std::collections::HashMap<String, Arc<Image>>>,
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

fn load_store_state(name: &str) -> anyhow::Result<(PathBuf, Snapshot, StoreOptions)> {
    let directory = platform::state_dir().map_err(anyhow::Error::msg)?;
    let state_file = directory.join(format!("device-{name}.json"));
    let path = directory.join("model-preferences.json");
    let preferences = std::fs::read(&path).unwrap_or_default();
    let snapshot = agent_core::persistence::load(&state_file, &preferences);
    let options = StoreOptions {
        cache_directory: Some(directory.join("cache").join(name)),
        state_file: Some(state_file),
        ..StoreOptions::default()
    };
    Ok((path, snapshot, options))
}

impl Desktop {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let runtime = cx.global::<Runtime>().clone();
        let (updates, incoming) = async_channel::bounded(64);
        StoreSession::on_app_quit(cx, |view| &mut view.session);
        cx.spawn_in(window, async move |view, cx| {
            while let Ok((epoch, update)) = incoming.recv().await {
                let environment_update = matches!(
                    &update,
                    Update::EnvironmentConnected { .. }
                        | Update::EnvironmentSnapshot { .. }
                        | Update::EnvironmentPersistenceError { .. }
                );
                if view
                    .update_in(cx, |view, window, cx| {
                        if view.epoch == epoch || environment_update {
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
            cx.observe_window_activation(window, |view, window, cx| {
                if window.is_window_active() {
                    view.refresh_diff_on_window_activation(cx);
                }
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
                &EnvironmentRegistry::default(),
                &ViewInputs {
                    sidebar: SidebarOptions::default(),
                    thread: ThreadViewOptions::default(),
                },
                0,
                ui::now_ms(),
            )),
            runtime,
            environment_registry: EnvironmentRegistry::default(),
            updates,
            epoch: 0,
            connecting: false,
            remote: None,
            background_sessions: BTreeMap::new(),
            background_connecting: BTreeSet::new(),
            background_retry_at: BTreeMap::new(),
            selected_retry_at: None,
            profile_environment_ids: BTreeMap::new(),
            pending_open: None,
            pending_new_thread: None,
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
            project_icons: Default::default(),
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
        self.selected_retry_at = None;
        self.epoch += 1;
        self.views_running = false;
        self.connecting = true;
        self.remote = remote;
        self.session.take();
        if let Some(remote) = &remote {
            self.background_sessions.remove(&remote.id);
            self.background_connecting.remove(&remote.id);
        }
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
            let state = load_store_state(&name);
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

    /// Starts one supervised Store owner for each saved environment. Each
    /// profile has its own cache directory and event stream; a failed profile
    /// never replaces the selected Host's session.
    fn start_background_connections(&mut self) {
        let selected = self.remote.as_ref().map(|remote| remote.id.as_str());
        let remotes = self.snapshot.remote_hosts.clone();
        let known: BTreeSet<_> = remotes.iter().map(|remote| remote.id.clone()).collect();
        let stale: Vec<_> = self
            .background_sessions
            .keys()
            .chain(self.background_connecting.iter())
            .filter(|profile_id| !known.contains(*profile_id))
            .cloned()
            .collect();
        for profile_id in stale {
            self.background_sessions.remove(&profile_id);
            self.background_connecting.remove(&profile_id);
            self.background_retry_at.remove(&profile_id);
            if let Some(environment_id) = self.profile_environment_ids.remove(&profile_id) {
                self.environment_registry.remove(&environment_id);
            }
        }
        for remote in remotes {
            if self
                .background_retry_at
                .get(&remote.id)
                .is_some_and(|(retry_at, _)| *retry_at > Instant::now())
            {
                continue;
            }
            if selected == Some(remote.id.as_str())
                || self.background_sessions.contains_key(&remote.id)
                || !self.background_connecting.insert(remote.id.clone())
            {
                continue;
            }
            self.connect_background(remote);
        }
    }

    fn background_failed(&mut self, profile_id: &str) {
        let failures = self
            .background_retry_at
            .get(profile_id)
            .map_or(0, |(_, failures)| *failures)
            .saturating_add(1);
        let exponent = failures.min(11);
        let delay = Duration::from_millis(250_u64.saturating_mul(1 << exponent).min(300_000));
        self.background_retry_at
            .insert(profile_id.to_owned(), (Instant::now() + delay, failures));
    }

    fn selected_failed(&mut self) {
        let failures = self
            .selected_retry_at
            .map_or(0, |(_, failures)| failures)
            .saturating_add(1);
        let exponent = failures.min(11);
        let delay = Duration::from_millis(250_u64.saturating_mul(1 << exponent).min(300_000));
        self.selected_retry_at = Some((Instant::now() + delay, failures));
    }

    fn reconnect_selected_if_due(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.connecting
            || self.remote.is_none()
            || self.snapshot.environment.is_none()
            || self.snapshot.connected
        {
            return;
        }
        if self
            .selected_retry_at
            .is_some_and(|(retry_at, _)| retry_at > Instant::now())
        {
            return;
        }
        let remote = self.remote.clone();
        self.selected_retry_at = None;
        if let Some(remote) = remote {
            self.connect(Some(remote), window, cx);
        }
    }

    fn connect_background(&self, remote: RemoteHost) {
        let profile_id = remote.id.clone();
        let updates = self.updates.clone();
        let epoch = self.epoch;
        let runtime = self.runtime.clone();
        let connections = runtime.connections.clone();
        self.runtime.handle.spawn(async move {
            let (path, snapshot, options) = match load_store_state(&profile_id) {
                Ok(state) => state,
                Err(error) => {
                    let _ = updates
                        .send((
                            epoch,
                            Update::EnvironmentConnected {
                                profile_id: profile_id.clone(),
                                result: Err(error.to_string()),
                            },
                        ))
                        .await;
                    return;
                }
            };
            if updates
                .send((
                    epoch,
                    Update::EnvironmentSnapshot {
                        profile_id: profile_id.clone(),
                        snapshot: Arc::new(snapshot.clone()),
                    },
                ))
                .await
                .is_err()
            {
                return;
            }
            let (tx, rx) = async_channel::bounded(8);
            let relay = updates.clone();
            let forward = tokio::spawn(async move {
                while let Ok(event) = rx.recv().await {
                    if relay.send((epoch, event)).await.is_err() {
                        break;
                    }
                }
            });
            let connected_profile = profile_id.clone();
            let snapshot_profile = profile_id.clone();
            StoreSession::publish(
                connections
                    .connect(Some(&remote.ticket), snapshot, options)
                    .await,
                runtime,
                tx,
                move |result| Update::EnvironmentConnected {
                    profile_id: connected_profile,
                    result: result.map(|session| (session, path.clone())),
                },
                move |snapshot| Update::EnvironmentSnapshot {
                    profile_id: snapshot_profile,
                    snapshot,
                },
            )
            .await;
            let _ = forward.await;
        });
    }

    /// Promotes an already-running per-profile Store when navigation targets
    /// one of the combined rows. The Store owner moves with the selected route;
    /// its cache and reconnect loop are otherwise independent.
    pub(crate) fn promote_environment(&mut self, environment_id: &str) -> bool {
        let Some(profile_id) = self
            .profile_environment_ids
            .iter()
            .find_map(|(profile_id, id)| (id == environment_id).then(|| profile_id.clone()))
        else {
            return false;
        };
        let Some(session) = self.background_sessions.remove(&profile_id) else {
            return false;
        };
        self.session.take();
        self.snapshot = session.store.snapshot();
        self.session = Some(session);
        self.remote = self
            .snapshot
            .remote_hosts
            .iter()
            .find(|remote| remote.id == profile_id)
            .cloned();
        self.environment_registry.select(environment_id);
        self.connecting = false;
        true
    }

    fn apply_pending_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((environment_id, project_id)) = self.pending_new_thread.clone() {
            if self.environment_registry.selected() != Some(environment_id.as_str())
                && !self.promote_environment(&environment_id)
            {
                return;
            }
            if self.environment_registry.selected() == Some(environment_id.as_str()) {
                self.pending_new_thread = None;
                self.snapshot_changed(window, cx);
                self.new_thread(project_id, cx);
                return;
            }
        }
        let Some((environment_id, thread_id)) = self.pending_open.clone() else {
            return;
        };
        if self.environment_registry.selected() != Some(environment_id.as_str()) {
            if !self.promote_environment(&environment_id) {
                return;
            }
        }
        if self.environment_registry.selected() == Some(environment_id.as_str()) {
            self.pending_open = None;
            self.snapshot_changed(window, cx);
            self.perform(Intent::OpenThread { thread_id });
        }
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
                panels: self.header_panels(),
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
        let environments = self.environment_registry.clone();
        let inputs = self.view_inputs();
        let generation = self.generation;
        let epoch = self.epoch;
        let updates = self.updates.clone();
        self.runtime.handle.spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            if let Ok(views) = tokio::task::spawn_blocking(move || {
                Views::derive(&snapshot, &environments, &inputs, generation, ui::now_ms())
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
                self.apply_pending_open(window, cx);
                self.reconnect_selected_if_due(window, cx);
                self.start_background_connections();
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
                let profile_id = self
                    .remote
                    .as_ref()
                    .map(|remote| remote.id.clone())
                    .unwrap_or_else(|| "local".into());
                if let Some(environment_id) = self
                    .snapshot
                    .environment
                    .as_ref()
                    .map(|environment| environment.environment_id.clone())
                {
                    self.profile_environment_ids
                        .insert(profile_id, environment_id.clone());
                    self.environment_registry.select(&environment_id);
                }
                self.connecting = false;
                self.selected_retry_at = None;
                self.perform(Intent::LoadAccounts);
                self.perform(Intent::LoadConversationSettings);
                self.snapshot_changed(window, cx);
            }
            Update::Connected(Err(error)) => {
                self.connecting = false;
                if self.remote.is_some() && self.selected_retry_at.is_none() {
                    self.selected_failed();
                }
                self.show_error(&error, window, cx);
            }
            Update::EnvironmentConnected { profile_id, result } => {
                if self
                    .remote
                    .as_ref()
                    .is_some_and(|remote| remote.id == profile_id)
                    || !self
                        .snapshot
                        .remote_hosts
                        .iter()
                        .any(|remote| remote.id == profile_id)
                {
                    return;
                }
                self.background_connecting.remove(&profile_id);
                match result {
                    Ok((mut session, path)) => {
                        self.background_retry_at.remove(&profile_id);
                        let updates = self.updates.clone();
                        let profile_for_errors = profile_id.clone();
                        let (tx, rx) = async_channel::bounded(4);
                        let epoch = self.epoch;
                        self.runtime.handle.spawn(async move {
                            while let Ok(event) = rx.recv().await {
                                if updates.send((epoch, event)).await.is_err() {
                                    break;
                                }
                            }
                        });
                        session.persist(path, tx, move |error| {
                            Update::EnvironmentPersistenceError {
                                profile_id: profile_for_errors.clone(),
                                error,
                            }
                        });
                        let snapshot = session.store.snapshot();
                        if let Some(environment_id) = snapshot
                            .environment
                            .as_ref()
                            .map(|environment| environment.environment_id.clone())
                        {
                            self.profile_environment_ids
                                .insert(profile_id.clone(), environment_id);
                        }
                        self.background_sessions.insert(profile_id.clone(), session);
                        self.environment_registry.update(snapshot);
                        let pending_environment = self
                            .pending_open
                            .as_ref()
                            .map(|(environment_id, _)| environment_id)
                            .or_else(|| {
                                self.pending_new_thread
                                    .as_ref()
                                    .map(|(environment_id, _)| environment_id)
                            });
                        if pending_environment.is_some_and(|environment_id| {
                            self.environment_registry.selected() != Some(environment_id.as_str())
                                && self.profile_environment_ids.get(&profile_id)
                                    == Some(environment_id)
                        }) {
                            if let Some(environment_id) = pending_environment {
                                self.promote_environment(environment_id);
                            }
                        }
                        self.generation += 1;
                        self.schedule_views(cx);
                    }
                    Err(error) => {
                        self.background_failed(&profile_id);
                        if let Some(environment_id) = self.profile_environment_ids.get(&profile_id)
                        {
                            self.environment_registry
                                .mark_disconnected(environment_id, Some(error.clone()));
                            self.generation += 1;
                            self.schedule_views(cx);
                        }
                    }
                }
            }
            Update::EnvironmentSnapshot {
                profile_id,
                snapshot,
            } => {
                if !self.background_sessions.contains_key(&profile_id)
                    && !self.background_connecting.contains(&profile_id)
                {
                    return;
                }
                let disconnected = !snapshot.connected;
                let current = self
                    .profile_environment_ids
                    .get(&profile_id)
                    .and_then(|environment_id| self.environment_registry.snapshot(environment_id));
                if current
                    .as_ref()
                    .is_some_and(|current| !snapshot.accepts_after(current))
                {
                    return;
                }
                if let Some(environment_id) = snapshot
                    .environment
                    .as_ref()
                    .map(|environment| environment.environment_id.clone())
                {
                    self.profile_environment_ids
                        .insert(profile_id.clone(), environment_id);
                }
                if let Some(session) = self.background_sessions.get(&profile_id) {
                    session.save(snapshot.clone());
                }
                self.environment_registry.update(snapshot);
                if disconnected {
                    self.background_sessions.remove(&profile_id);
                    self.background_failed(&profile_id);
                }
                self.generation += 1;
                self.schedule_views(cx);
            }
            Update::EnvironmentPersistenceError { profile_id, error } => {
                if !self
                    .snapshot
                    .remote_hosts
                    .iter()
                    .any(|remote| remote.id == profile_id)
                {
                    return;
                }
                self.show_error(&format!("Environment {profile_id}: {error}"), window, cx);
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
    }

    fn snapshot_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(environment_id) = self.environment_registry.update(self.snapshot.clone()) {
            self.environment_registry.select(&environment_id);
            self.generation += 1;
        }
        self.start_background_connections();
        if self.remote.is_some()
            && self.snapshot.environment.is_some()
            && !self.snapshot.connected
            && self.selected_retry_at.is_none()
        {
            self.selected_failed();
        }
        if let Some(error) = self.snapshot.error.clone()
            && self.shown_error.as_deref()
                != Some(agent_core::presentation::error::error_message(&error).as_str())
        {
            self.show_error(&error, window, cx);
        }
        self.sync_composer(window, cx);
        self.sync_panels(window, cx);
        self.sync_settings(window, cx);
        self.offer_onboarding_import(window, cx);
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

    /// Runs the command the keymap binds to this keystroke where focus is.
    fn global_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keystroke = &event.keystroke;
        if keystroke.key == "escape" && self.cancel_sweep(window, cx) {
            return true;
        }
        if keymap::is_modifier_only(keystroke) || self.settings.recording_shortcut() {
            return false;
        }
        let context = self.key_context(window, cx);
        let Some(command) = self
            .snapshot
            .keymap(keymap::MAC)
            .resolve(&keymap::key_press(keystroke), &context)
        else {
            return false;
        };
        self.run_command(&command, window, cx)
    }

    /// Where focus is, as the keymap's `when` clauses read it.
    pub(crate) fn key_context(
        &self,
        window: &Window,
        cx: &App,
    ) -> agent_core::view::keybindings::KeyContext {
        agent_core::view::keybindings::KeyContext {
            terminal_focus: self.terminal_focused(window, cx),
            terminal_open: self.header_panels().terminal_open,
            composer_focus: self.composer_focused(window, cx),
            model_picker_open: self.model_picker_open(),
            editable_focus: window
                .context_stack()
                .iter()
                .any(|context| context.contains("Input")),
        }
    }

    /// Runs a keyboard command; false when it does nothing here.
    fn run_command(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match command {
            "sidebar.toggle" => {
                self.sidebar_hidden = !self.sidebar_hidden;
                cx.notify();
            }
            "rightPanel.toggle" => self.toggle_right_panel(window, cx),
            "terminal.toggle" => self.toggle_terminal_drawer(window, cx),
            "terminal.split" | "terminal.splitVertical" | "terminal.new" | "terminal.close" => {
                return self.terminal_command(command, window, cx);
            }
            "rightPanel.close" => return self.close_active_surface(window, cx),
            "diff.toggle" => self.toggle_diff(window, cx),
            "composer.stash" => self.composer_stash_shortcut(cx),
            "thread.steerQueuedMessage" => self.steer_first_queued(),
            "thread.editQueuedMessage" => return self.edit_last_queued(),
            "modelPicker.toggle" => self.toggle_model_picker(window, cx),
            "modelPicker.previousProvider" | "modelPicker.nextProvider" => {
                return self.step_model_picker_provider(
                    command == "modelPicker.nextProvider",
                    window,
                    cx,
                );
            }
            "chat.new" => {
                let project = self
                    .snapshot
                    .selected_thread
                    .as_ref()
                    .and_then(|thread| self.snapshot.thread_project(thread))
                    .map(str::to_owned)
                    .or_else(|| self.snapshot.selected_project.clone());
                self.new_thread(project, cx);
            }
            "thread.undo" => return self.undo_thread_action(),
            "thread.previous" => self.select_adjacent_thread(false, cx),
            "thread.next" => self.select_adjacent_thread(true, cx),
            "thread.settle" => self.toggle_open_thread(
                (ThreadMenuItemId::Settle, ThreadMenuItemId::Unsettle),
                window,
                cx,
            ),
            "thread.pin" => self.toggle_open_thread(
                (ThreadMenuItemId::Pin, ThreadMenuItemId::Unpin),
                window,
                cx,
            ),
            other => {
                use agent_core::view::keybindings::{model_jump_index, thread_jump_index};
                if let Some(index) = model_jump_index(other) {
                    return self.jump_model_picker(index, window, cx);
                }
                match thread_jump_index(other) {
                    Some(index) => self.jump_to_thread(index, cx),
                    None => return false,
                }
            }
        }
        true
    }
}

impl Render for Desktop {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_browser(cx);
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
                let body = self.chat_drop_target(body, cx);
                let body = self.details_area(body, window, cx);
                let column = v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_header(window, cx))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .child(body)
                            .children(self.render_terminal_drawer(window, cx)),
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
            .on_modifiers_changed(cx.listener(Self::sidebar_modifiers_changed))
            .on_drop(
                cx.listener(|view, _: &sidebar::SweepDrag, window, cx| {
                    view.finish_sweep(window, cx)
                }),
            )
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

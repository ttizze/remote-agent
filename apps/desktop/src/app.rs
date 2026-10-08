//! The desktop window: one Host connection, the views agent-core derives from
//! its snapshots, and the screens that draw them.
mod attachments;
mod composer;
mod dialogs;
mod dictation;
mod git;
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

use crate::{
    Runtime, platform,
    store_session::{ClientPreferences, StoreSession},
};
use agent_core::{
    connection::{Outcome, Store, StoreOptions},
    environment::{
        EnvironmentInboxView, EnvironmentRegistry, EnvironmentSettingsView, EnvironmentSidebarView,
    },
    state::{Intent, Snapshot},
    view::{
        command_palette::{self, CommandPaletteItem, CommandPaletteItemKind},
        load_balancing::{self, PendingRouteAction},
        new_thread::NewThreadView,
        sidebar::{SidebarOptions, SidebarView},
        thread::{ThreadView, ThreadViewOptions},
        thread_menu::ThreadMenuItemId,
        timeline::rows::TimelineLayout,
    },
};
use agent_protocol::models::RemoteHost;
use futures_util::future::join_all;
use gpui_kit::{
    component::{
        Sizable, WindowExt, h_flex,
        input::{Input, InputEvent, InputState},
        menu::PopupMenuItem,
        notification::Notification,
        v_flex,
    },
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
use tokio_util::sync::CancellationToken;

/// Runs once a dispatched intent resolves.
type Done = Box<
    dyn FnOnce(&mut Desktop, &Result<Outcome, String>, &mut Window, &mut Context<Desktop>) + Send,
>;

enum Update {
    Connected(Result<(StoreSession, PathBuf, bool), String>),
    Snapshot(Arc<Snapshot>),
    EnvironmentConnected {
        profile_id: String,
        result: Result<StoreSession, String>,
    },
    EnvironmentSnapshot {
        profile_id: String,
        snapshot: Arc<Snapshot>,
    },
    Views(Box<Views>),
    Completed(Option<Done>, Result<Outcome, String>),
    PersistenceError(String),
    Attachments(attachments::Update),
    Recording(uuid::Uuid, platform::RecordingEvent),
    Transcribed(uuid::Uuid, Result<Outcome, String>),
    HostPowerSample {
        attempt: u64,
        snapshot: agent_protocol::background::HostPowerSnapshot,
    },
    ExternalSnapshot(String),
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

#[derive(Debug, Clone)]
struct PendingLoadBalancedNewThread {
    project_id: String,
    source_environment_id: String,
    started_at_ms: i64,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutomaticNewThreadResult {
    Started,
    Waiting,
    Unavailable,
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
    /// Last snapshot used as the notification transition baseline.
    notification_snapshot: Arc<Snapshot>,
    /// Native notifications that have actually been posted. The value keeps
    /// the owning environment so a disconnected Host only retracts its own
    /// notices. Inserting an existing tag replaces its notice, matching the
    /// platform notification contract.
    active_notification_tags: BTreeMap<String, Option<String>>,
    /// Immutable projections for every authenticated Host store.
    pub(crate) environment_registry: EnvironmentRegistry,
    pub(crate) views: Arc<Views>,
    pub(crate) runtime: Runtime,
    /// The selected Store owns writes, while this current value seeds every
    /// cached Host Store. Keeping it outside per-Host snapshots prevents a
    /// late offline Store from resurrecting stale client preferences.
    client_preferences: ClientPreferences,
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
    /// Route queued by the platform notification callback. The callback has
    /// no Window handle; the next UI tick consumes it on the owning view.
    pending_notification_route: Option<String>,
    pending_new_thread: Option<(String, Option<String>)>,
    local_host_supervised: bool,
    pending_load_balanced_new_thread: Option<PendingLoadBalancedNewThread>,
    load_balancing_attempt_generation: u64,
    load_balancing_refresh_requested: bool,
    browser_profile_removal_generation: u64,
    pub(crate) hosts: Entity<Hosts>,
    pub(crate) route: Route,
    pub(crate) sidebar_hidden: bool,
    sidebar_animation: PanelAnimationState,
    right_panel_animation: PanelAnimationState,
    terminal_drawer_animation: PanelAnimationState,
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
    command_palette_query: Entity<InputState>,
    command_palette_open: bool,
    /// Number of Shift key presses in the current modifier pair. GPUI exposes
    /// modifier state but not left/right identity, so the desktop surface
    /// keeps this small edge-triggered latch for the both-Shift shortcut.
    snapshot_shift_presses: u8,
    snapshot_feedback_until: Option<std::time::Instant>,
    snapshot_feedback_id: u64,
    snapshot_feedback_animated: bool,
    external_snapshot_ids: BTreeSet<String>,
    pub(crate) attachments: attachments::AttachmentCache,
    pub(crate) dictation: Option<dictation::Dictation>,
    last_host_power_report_ms: Option<i64>,
    last_host_power: Option<agent_protocol::background::HostPowerSnapshot>,
    host_power_attempt: u64,
    host_power_probe: Option<tokio_util::task::AbortOnDropHandle<()>>,
    host_power_probe_stop: Option<CancellationToken>,
    desktop_process_monitor: Arc<host_daemon::DesktopProcessMonitor>,
    /// Decoded project icons, by content hash.
    pub(crate) project_icons: std::cell::RefCell<std::collections::HashMap<String, Arc<Image>>>,
    tick: Option<tokio_util::task::AbortOnDropHandle<()>>,
    _subscriptions: Vec<Subscription>,
}

/// A duration-based transition that can retarget from its current visual
/// value. GPUI's ordinary animation state is keyed by element id and only
/// tracks elapsed time, so changing an open/closed id would restart from an
/// endpoint and visibly jump when a panel is toggled mid-transition.
#[derive(Default)]
struct PanelAnimationState {
    scope: Option<String>,
    from: f32,
    target: f32,
    started: Option<Instant>,
    duration: Duration,
    run: u64,
}

#[derive(Clone, Copy)]
struct PanelAnimation {
    from: f32,
    target: f32,
    run: u64,
    duration: Duration,
}

impl PanelAnimationState {
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Returns the animation inputs for this frame, or `None` when the target
    /// is already settled. A scope change is rendered immediately so restored
    /// panel state does not flash through an enter animation.
    fn prepare(&mut self, scope: &str, target: f32, duration: Duration) -> Option<PanelAnimation> {
        let now = Instant::now();
        if self.scope.as_deref() != Some(scope) {
            self.scope = Some(scope.to_owned());
            self.from = target;
            self.target = target;
            self.started = None;
            self.duration = duration;
            return None;
        }

        if duration.is_zero() {
            self.from = target;
            self.target = target;
            self.started = None;
            self.duration = duration;
            return None;
        }

        if (target - self.target).abs() > f32::EPSILON {
            self.from = self.value_at(now);
            self.target = target;
            self.started = Some(now);
            self.duration = duration;
            self.run = self.run.wrapping_add(1);
        } else if let Some(started) = self.started {
            if now.duration_since(started) >= self.duration {
                self.from = self.target;
                self.started = None;
            }
        }

        self.started.map(|_| PanelAnimation {
            from: self.from,
            target: self.target,
            run: self.run,
            duration: self.duration,
        })
    }

    fn value_at(&self, now: Instant) -> f32 {
        let Some(started) = self.started else {
            return self.target;
        };
        if self.duration.is_zero() {
            return self.target;
        }
        let delta =
            (now.duration_since(started).as_secs_f32() / self.duration.as_secs_f32()).min(1.);
        self.from + (self.target - self.from) * panel_ease_out(delta)
    }
}

/// Match the web panel transitions' CSS `ease-out` curve
/// (`cubic-bezier(0, 0, .58, 1)`).
fn panel_ease_out(time: f32) -> f32 {
    let time = time.clamp(0., 1.);
    let mut parameter = time;
    for _ in 0..5 {
        let x = cubic_bezier(parameter, 0., 0.58);
        let derivative = cubic_bezier_derivative(parameter, 0., 0.58);
        if derivative.abs() < f32::EPSILON {
            break;
        }
        parameter = (parameter - (x - time) / derivative).clamp(0., 1.);
    }
    cubic_bezier(parameter, 0., 1.)
}

fn cubic_bezier(parameter: f32, first_control: f32, second_control: f32) -> f32 {
    let inverse = 1. - parameter;
    3. * inverse * inverse * parameter * first_control
        + 3. * inverse * parameter * parameter * second_control
        + parameter * parameter * parameter
}

fn cubic_bezier_derivative(parameter: f32, first_control: f32, second_control: f32) -> f32 {
    let inverse = 1. - parameter;
    3. * inverse * inverse * first_control
        + 6. * inverse * parameter * (second_control - first_control)
        + 3. * parameter * parameter * (1. - second_control)
}

fn local_host_power_publish_allowed(
    local_host_supervised: bool,
    remote_selected: bool,
    probe_in_flight: bool,
) -> bool {
    local_host_supervised && !remote_selected && !probe_in_flight
}

/// Keys every window binds; screens handle their own focus-specific keys.
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "ctrl-v",
        gpui_kit::component::input::Paste,
        Some("ChatComposer > Input"),
    )]);
}

fn load_store_state(
    name: &str,
    client_preferences: &ClientPreferences,
) -> anyhow::Result<(PathBuf, Snapshot, StoreOptions)> {
    let directory = platform::state_dir().map_err(anyhow::Error::msg)?;
    let state_file = directory.join(format!("device-{name}.json"));
    let path = directory.join("model-preferences.json");
    let preferences = client_preferences.current();
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
                    Update::EnvironmentConnected { .. } | Update::EnvironmentSnapshot { .. }
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
                    view.dismiss_active_notifications(cx);
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
        let command_palette_query =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search commands and threads"));
        subscriptions.push(cx.subscribe_in(
            &command_palette_query,
            window,
            |view, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) && view.command_palette_open {
                    view.reopen_command_palette(window, cx);
                }
            },
        ));
        let mut view = Self {
            session: None,
            snapshot: Arc::default(),
            notification_snapshot: Arc::default(),
            active_notification_tags: BTreeMap::new(),
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
            client_preferences: ClientPreferences::from_disk(),
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
            pending_notification_route: None,
            pending_new_thread: None,
            local_host_supervised: false,
            pending_load_balanced_new_thread: None,
            load_balancing_attempt_generation: 0,
            load_balancing_refresh_requested: false,
            browser_profile_removal_generation: 0,
            hosts,
            route: Route::Chat,
            sidebar_hidden: false,
            sidebar_animation: PanelAnimationState::default(),
            right_panel_animation: PanelAnimationState::default(),
            terminal_drawer_animation: PanelAnimationState::default(),
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
            command_palette_query,
            command_palette_open: false,
            snapshot_shift_presses: 0,
            snapshot_feedback_until: None,
            snapshot_feedback_id: 0,
            snapshot_feedback_animated: true,
            external_snapshot_ids: BTreeSet::new(),
            attachments: attachments::AttachmentCache::new(),
            dictation: None,
            last_host_power_report_ms: None,
            last_host_power: None,
            host_power_attempt: 0,
            host_power_probe: None,
            host_power_probe_stop: None,
            desktop_process_monitor: Arc::new(host_daemon::DesktopProcessMonitor::new()),
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
        let previous_environment_id = self
            .snapshot
            .environment
            .as_ref()
            .map(|environment| environment.environment_id.clone());
        self.attachments.clear();
        self.external_snapshot_ids.clear();
        self.dictation = None;
        self.selected_retry_at = None;
        if let Some(stop) = self.host_power_probe_stop.take() {
            stop.cancel();
        }
        self.host_power_probe.take();
        self.local_host_supervised = false;
        self.last_host_power_report_ms = None;
        self.last_host_power = None;
        self.host_power_attempt = self.host_power_attempt.wrapping_add(1);
        self.epoch += 1;
        self.views_running = false;
        self.connecting = true;
        self.invalidate_load_balancing_attempt();
        self.remote = remote;
        self.session.take();
        if let Some(remote) = &remote {
            self.background_sessions.remove(&remote.id);
            self.background_connecting.remove(&remote.id);
        }
        self.snapshot = Arc::default();
        self.notification_snapshot = self.snapshot.clone();
        if let Some(environment_id) = previous_environment_id {
            self.dismiss_environment_notifications(&environment_id, cx);
        }
        self.disconnected(window, cx);
        let updates = self.updates.clone();
        let epoch = self.epoch;
        let runtime = self.runtime.clone();
        let connections = runtime.connections.clone();
        let client_preferences = self.client_preferences.clone();
        let ticket = self.remote.as_ref().map(|r| r.ticket.clone());
        let name = self
            .remote
            .as_ref()
            .map(|r| r.id.clone())
            .unwrap_or_else(|| "local".into());
        self.runtime.handle.spawn(async move {
            let state = load_store_state(&name, &client_preferences);
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
                    let connected = connections
                        .connect(ticket.as_deref(), snapshot, options)
                        .await;
                    let local_host_supervised = connected
                        .as_ref()
                        .map(|connected| connected.local_host_supervised)
                        .unwrap_or(false);
                    StoreSession::publish(
                        connected.map(|connected| connected.store),
                        runtime.clone(),
                        tx,
                        move |result| {
                            Update::Connected(
                                result
                                    .map(|session| (session, path.clone(), local_host_supervised)),
                            )
                        },
                        Update::Snapshot,
                        Some(client_preferences.clone()),
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
                    for snapshot in platform::pending_snapshots().unwrap_or_default() {
                        if updates
                            .send((epoch, Update::ExternalSnapshot(snapshot.id)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                }
            }),
        ));
    }

    /// Starts one supervised Store owner for each saved environment. Each
    /// profile has its own cache directory and event stream; a failed profile
    /// never replaces the selected Host's session.
    fn start_background_connections(&mut self, cx: &mut Context<Self>) {
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
                self.dismiss_environment_notifications(&environment_id, cx);
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
        let client_preferences = self.client_preferences.clone();
        self.runtime.handle.spawn(async move {
            let (_, snapshot, options) = match load_store_state(&profile_id, &client_preferences) {
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
                    result,
                },
                move |snapshot| Update::EnvironmentSnapshot {
                    profile_id: snapshot_profile,
                    snapshot,
                },
                Some(client_preferences.clone()),
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
        let previous_profile_id = self.remote.as_ref().map(|remote| remote.id.clone());
        if let Some(previous_session) = self.session.take()
            && let Some(previous_profile_id) = previous_profile_id
        {
            self.background_sessions
                .insert(previous_profile_id, previous_session);
        }
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
        self.invalidate_load_balancing_attempt();
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
                self.begin_new_thread(project_id);
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

    /// Dispatches a device-owned preference to the Store for one environment.
    /// Provider and Host mutations remain scoped to the owning Store even
    /// while the settings page is showing a combined environment list.
    pub(crate) fn perform_on_environment(&self, environment_id: &str, intent: Intent) {
        if self
            .snapshot
            .environment
            .as_ref()
            .map(|environment| environment.environment_id.as_str())
            == Some(environment_id)
        {
            self.perform(intent);
            return;
        }
        let Some(profile_id) = self
            .profile_environment_ids
            .iter()
            .find_map(|(profile_id, id)| (id == environment_id).then_some(profile_id))
        else {
            return;
        };
        if let Some(session) = self.background_sessions.get(profile_id) {
            let _ = session.store.dispatch(intent);
        } else {
            // Device preferences remain editable while a cached environment
            // is disconnected; the selected Store persists the scoped key.
            self.perform(intent);
        }
    }

    /// Clears a browser profile in every connected Host before removing the
    /// device-owned profile row. The core plan owns validation and completion
    /// decisions; this owner only supplies the Store for each environment and
    /// commits the existing removal intent after every receipt succeeds.
    pub(crate) fn remove_browser_profile(
        &mut self,
        profile_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browser_profile_removal_generation =
            self.browser_profile_removal_generation.wrapping_add(1);
        let generation = self.browser_profile_removal_generation;
        let profiles = self.snapshot.preferences.browser.resolved().profiles;
        let mut environment_ids = BTreeSet::new();
        for snapshot in self.environment_registry.snapshots() {
            if snapshot.connected
                && let Some(environment) = snapshot.environment.as_ref()
            {
                environment_ids.insert(environment.environment_id.clone());
            }
        }
        let mut stores = BTreeMap::new();
        if let Some(session) = &self.session {
            let snapshot = session.store.snapshot();
            if snapshot.connected
                && let Some(environment_id) = snapshot
                    .environment
                    .as_ref()
                    .map(|environment| environment.environment_id.clone())
            {
                environment_ids.insert(environment_id.clone());
                stores.insert(environment_id, session.store.clone());
            }
        }
        for session in self.background_sessions.values() {
            let snapshot = session.store.snapshot();
            if snapshot.connected
                && let Some(environment_id) = snapshot
                    .environment
                    .as_ref()
                    .map(|environment| environment.environment_id.clone())
            {
                environment_ids.insert(environment_id.clone());
                stores.insert(environment_id, session.store.clone());
            }
        }
        let environment_ids = environment_ids.into_iter().collect::<Vec<_>>();
        let plan = match agent_core::view::browser::begin_browser_profile_removal(
            &profiles,
            profile_id,
            environment_ids,
            generation,
        ) {
            Ok(plan) => plan,
            Err(error) => {
                self.show_error(&error, window, cx);
                return;
            }
        };
        let missing = plan
            .environment_ids
            .iter()
            .filter(|environment_id| !stores.contains_key(*environment_id))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            self.show_error(
                "A connected Host is not ready to clear this browser profile; try again.",
                window,
                cx,
            );
            return;
        }
        let targets = plan
            .environment_ids
            .iter()
            .filter_map(|environment_id| {
                stores
                    .get(environment_id)
                    .map(|store| (environment_id.clone(), store.clone()))
            })
            .collect::<Vec<_>>();
        let clear_targets = targets.clone();
        self.spawn_task(
            async move {
                let cleared = join_all(clear_targets.into_iter().map(|(environment_id, store)| {
                    let profile_id = plan.profile_id.clone();
                    async move {
                        let result = store
                            .dispatch(Intent::PreviewClearProfileData { profile_id })
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|result| {
                                result.map(|_| ()).map_err(|error| error.to_string())
                            });
                        (environment_id, result)
                    }
                }))
                .await;
                let mut cleared_environment_ids = Vec::new();
                let mut failed = false;
                for (environment_id, result) in cleared {
                    if result.is_ok() {
                        cleared_environment_ids.push(environment_id);
                    } else {
                        failed = true;
                    }
                }
                let decision = agent_core::view::browser::browser_profile_removal_decision(
                    &plan,
                    generation,
                    &cleared_environment_ids,
                    failed,
                );
                let removed = if decision
                    == agent_core::view::browser::BrowserProfileRemovalDecision::Ready
                {
                    let profile_id = plan.profile_id.clone();
                    join_all(targets.into_iter().map(|(environment_id, store)| {
                        let profile_id = profile_id.clone();
                        async move {
                            let result = store
                                .dispatch(Intent::RemoveBrowserProfile { profile_id })
                                .await
                                .map_err(|error| error.to_string())
                                .and_then(|result| {
                                    result.map(|_| ()).map_err(|error| error.to_string())
                                });
                            (environment_id, result)
                        }
                    }))
                    .await
                } else {
                    Vec::new()
                };
                (plan, decision, removed)
            },
            move |view, (_plan, decision, removed), window, cx| {
                if view.browser_profile_removal_generation != generation {
                    return;
                }
                match decision {
                    agent_core::view::browser::BrowserProfileRemovalDecision::Ready => {
                        if let Some((environment_id, error)) =
                            removed.into_iter().find(|(_, result)| result.is_err())
                        {
                            let detail = error.unwrap_err();
                            view.show_error(
                                &format!(
                                    "Browser profile could not be removed from Host {environment_id}: {detail}"
                                ),
                                window,
                                cx,
                            );
                        }
                    }
                    agent_core::view::browser::BrowserProfileRemovalDecision::Failed => {
                        view.show_error(
                            "Browser profile data could not be cleared on every connected Host; the profile was kept.",
                            window,
                            cx,
                        );
                    }
                    agent_core::view::browser::BrowserProfileRemovalDecision::Pending
                    | agent_core::view::browser::BrowserProfileRemovalDecision::Stale => {}
                }
            },
        );
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
                if window.is_window_active() {
                    self.dismiss_active_notifications(cx);
                }
                if let Some(route) = self.pending_notification_route.take() {
                    self.open_notification_route(&route, window, cx);
                }
                self.retry_pending_load_balanced_new_thread(window, cx);
                self.apply_pending_open(window, cx);
                self.reconnect_selected_if_due(window, cx);
                self.start_background_connections(cx);
                self.publish_host_power();
                self.schedule_views(cx);
                cx.notify();
                return;
            }
            Update::Connected(Ok((mut session, path, local_host_supervised))) => {
                if !self.selected_store_has_current_client_preferences(&session.store) {
                    let updates = self.updates.clone();
                    let client_preferences = self.client_preferences.clone();
                    let epoch = self.epoch;
                    self.runtime.handle.spawn(async move {
                        let update = match client_preferences.apply_to(&session.store).await {
                            Ok(()) => Update::Connected(Ok((session, path, local_host_supervised))),
                            Err(error) => Update::Connected(Err(format!("{error:#}"))),
                        };
                        let _ = updates.send((epoch, update)).await;
                    });
                    return;
                }
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
                session.persist(
                    path,
                    self.client_preferences.clone(),
                    tx,
                    Update::PersistenceError,
                );
                self.snapshot = session.store.snapshot();
                self.last_host_power_report_ms = None;
                self.last_host_power = self
                    .snapshot
                    .background_policy
                    .as_ref()
                    .map(|snapshot| snapshot.host_power.clone());
                self.local_host_supervised = local_host_supervised;
                self.session = Some(session);
                self.synchronize_client_preferences();
                if let Some(session) = &self.session {
                    session.save(self.snapshot.clone());
                }
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
                self.perform(Intent::LoadSettings);
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
                    Ok(mut session) => {
                        self.background_retry_at.remove(&profile_id);
                        let updates = self.updates.clone();
                        let (tx, rx) = async_channel::bounded(4);
                        let epoch = self.epoch;
                        self.runtime.handle.spawn(async move {
                            while let Ok(event) = rx.recv().await {
                                if updates.send((epoch, event)).await.is_err() {
                                    break;
                                }
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
                        if let Some(environment_id) =
                            self.profile_environment_ids.get(&profile_id).cloned()
                        {
                            self.dismiss_environment_notifications(&environment_id, cx);
                            self.environment_registry
                                .mark_disconnected(&environment_id, Some(error.clone()));
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
                self.environment_registry.update(snapshot.clone());
                if let Some(previous) = current.as_ref() {
                    self.deliver_notification_events(previous, &snapshot, window, cx);
                }
                if disconnected {
                    if let Some(environment_id) =
                        self.profile_environment_ids.get(&profile_id).cloned()
                    {
                        self.dismiss_environment_notifications(&environment_id, cx);
                    }
                    self.background_sessions.remove(&profile_id);
                    self.background_failed(&profile_id);
                }
                self.generation += 1;
                self.schedule_views(cx);
            }
            Update::Snapshot(snapshot) => {
                if self.session.is_none() {
                    return;
                }
                if !snapshot.accepts_after(&self.snapshot) {
                    return;
                }
                let disconnected_environment_id = (self.snapshot.connected && !snapshot.connected)
                    .then(|| {
                        self.snapshot
                            .environment
                            .as_ref()
                            .map(|environment| environment.environment_id.clone())
                    })
                    .flatten();
                if let Some(power) = snapshot
                    .background_policy
                    .as_ref()
                    .map(|background| background.host_power.clone())
                {
                    self.last_host_power = Some(power);
                }
                self.snapshot = snapshot;
                self.synchronize_client_preferences();
                if let Some(session) = &self.session {
                    session.save(self.snapshot.clone());
                }
                self.snapshot_changed(window, cx);
                if let Some(environment_id) = disconnected_environment_id {
                    self.dismiss_environment_notifications(&environment_id, cx);
                }
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
                    self.synchronize_client_preferences();
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
            Update::ExternalSnapshot(id) => {
                if self.session.is_none() {
                    return;
                }
                if self.external_snapshot_ids.insert(id.clone()) {
                    match platform::read_pending_snapshot(&id) {
                        Ok(snapshot) => {
                            let draft_key = self.snapshot.draft_key();
                            self.attach_external_snapshot(draft_key, snapshot);
                        }
                        Err(error) => {
                            self.external_snapshot_ids.remove(&id);
                            self.show_error(&error, window, cx);
                        }
                    }
                }
            }
            Update::HostPowerSample { attempt, snapshot } => {
                if let Some(stop) = self.host_power_probe_stop.take() {
                    stop.cancel();
                }
                self.host_power_probe.take();
                if attempt != self.host_power_attempt || !self.local_host_supervised {
                    return;
                }
                let Some(store) = self.session.as_ref().map(|session| session.store.clone()) else {
                    return;
                };
                self.last_host_power = Some(snapshot.clone());
                let receipt = store.report_host_power(snapshot);
                self.runtime.handle.spawn(async move {
                    let _ = receipt.await;
                });
            }
        }
        cx.notify();
    }

    /// GPUI owns the desktop lifecycle, so its one-second app tick schedules a
    /// local-only power observation. The bounded native probe runs on the
    /// runtime and returns through the epoch-tagged update channel; GPUI only
    /// applies the result and reports it to the connected Host.
    fn publish_host_power(&mut self) {
        if !local_host_power_publish_allowed(
            self.local_host_supervised,
            self.remote.is_some(),
            self.host_power_probe.is_some(),
        ) {
            return;
        }
        if self.session.is_none() {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        let power = self
            .last_host_power
            .clone()
            .or_else(|| {
                self.snapshot
                    .background_policy
                    .as_ref()
                    .map(|snapshot| snapshot.host_power.clone())
            })
            .unwrap_or_else(|| {
                agent_domain::HostPowerSnapshot::unknown(
                    agent_domain::Timestamp::from_millis(now_ms).expect("current timestamp"),
                )
            });
        let interval_ms = self
            .snapshot
            .background_policy
            .as_ref()
            .map(|snapshot| {
                if snapshot.host_power.idle.is_true() {
                    snapshot.policy.host_power_monitor_idle_interval_ms
                } else {
                    snapshot.policy.host_power_monitor_active_interval_ms
                }
            })
            .unwrap_or(30_000)
            .max(1)
            .min(i64::MAX as u64) as i64;
        if self
            .last_host_power_report_ms
            .is_some_and(|last| now_ms >= last && now_ms.saturating_sub(last) < interval_ms)
        {
            return;
        }
        self.last_host_power_report_ms = Some(now_ms);
        self.host_power_attempt = self.host_power_attempt.wrapping_add(1);
        let attempt = self.host_power_attempt;
        let updates = self.updates.clone();
        let epoch = self.epoch;
        let stop = CancellationToken::new();
        let process_monitor = self.desktop_process_monitor.clone();
        self.host_power_probe_stop = Some(stop.clone());
        self.host_power_probe = Some(tokio_util::task::AbortOnDropHandle::new(
            self.runtime.handle.spawn(async move {
                let mut snapshot = host_daemon::sample_desktop_power(&stop).await;
                let process_sample = tokio::task::spawn_blocking(move || process_monitor.sample());
                let processes = tokio::select! {
                    _ = stop.cancelled() => Vec::new(),
                    result = process_sample => result.unwrap_or_default(),
                };
                snapshot.desktop_processes = processes;
                let _ = updates
                    .send((epoch, Update::HostPowerSample { attempt, snapshot }))
                    .await;
            }),
        ));
    }

    fn outcome(&mut self, outcome: &Outcome, window: &mut Window, cx: &mut Context<Self>) {
        self.composer_outcome(outcome, window, cx);
    }

    fn snapshot_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.synchronize_client_preferences();
        self.deliver_snapshot_notifications(window, cx);
        self.start_background_connections(cx);
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

    /// Publishes the selected Host's client-global preferences to the single
    /// native cache and every live background Store. Background snapshots stay
    /// Host-scoped, so an older Store cannot overwrite the current version or
    /// become the source for a later Host switch.
    fn synchronize_client_preferences(&self) {
        let Ok(bytes) = agent_core::persistence::encode_model_preferences(&self.snapshot) else {
            return;
        };
        if !self.client_preferences.replace(bytes.clone()) {
            return;
        }
        for session in self.background_sessions.values() {
            let receipt = session.store.apply_client_preferences(bytes.clone());
            self.runtime.handle.spawn(async move {
                let _ = receipt.await;
            });
        }
    }

    fn selected_store_has_current_client_preferences(&self, store: &Store) -> bool {
        let current = self.client_preferences.current();
        current.is_empty()
            || agent_core::persistence::encode_model_preferences(&store.snapshot())
                .is_ok_and(|bytes| bytes == current)
    }

    fn deliver_snapshot_notifications(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = std::mem::replace(&mut self.notification_snapshot, self.snapshot.clone());
        if let Some(environment_id) = self.environment_registry.update(self.snapshot.clone()) {
            self.environment_registry.select(&environment_id);
            self.generation += 1;
        }
        self.deliver_notification_events(&previous, &self.snapshot, window, cx);
    }

    fn deliver_notification_events(
        &mut self,
        previous: &Snapshot,
        current: &Snapshot,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focused = window.is_window_active();
        let mode_changed =
            previous.preferences.notification_mode != current.preferences.notification_mode;
        if mode_changed {
            // The web coordinator dismisses every posted notice whenever the
            // mode changes. Clear the owner registry before posting events
            // from the new snapshot so stale tags cannot survive a
            // Notifications <-> NotificationsAndSound transition.
            self.dismiss_active_notifications(cx);
        }
        let events = agent_core::view::notifications::between(previous, current, focused, focused);
        for event in events {
            if event.in_app {
                let route = event.deep_link.clone();
                let desktop = cx.entity().downgrade();
                let notification = match event.kind {
                    agent_core::view::notifications::NotificationEventKind::Completion => {
                        Notification::success(event.body.clone())
                    }
                    agent_core::view::notifications::NotificationEventKind::Failed => {
                        Notification::error(event.body.clone())
                    }
                    agent_core::view::notifications::NotificationEventKind::Input
                    | agent_core::view::notifications::NotificationEventKind::Approval
                    | agent_core::view::notifications::NotificationEventKind::Limited => {
                        Notification::warning(event.body.clone())
                    }
                };
                window.push_notification(
                    notification
                        .title(event.title.clone())
                        .on_click(move |_, _, app| {
                            let _ = desktop.update(app, |view, cx| {
                                view.queue_notification_route(route.clone());
                                cx.notify();
                            });
                        }),
                    cx,
                );
            }
            if event.operating_system {
                // GPUI owns the native adapter on every desktop target. The
                // tag is the complete core route, and the registered app
                // callback returns it to this UI owner when the user clicks.
                let tag = event.deep_link.clone();
                cx.show_system_notification(SystemNotification {
                    tag: tag.clone().into(),
                    title: event.title.clone().into(),
                    body: event.body.clone().into(),
                    actions: vec![SystemNotificationAction {
                        id: "open".into(),
                        label: "Open".into(),
                    }],
                });
                self.active_notification_tags
                    .insert(tag, event.environment_id.clone());
            }
            if event.sound {
                let sound_kind = event.sound_kind;
                self.runtime.handle.spawn(async move {
                    let _ = platform::play_notification_sound(sound_kind).await;
                });
            }
        }
        if focused {
            // Focusing the window acknowledges native notices, matching the
            // in-app selection path even when another Host still has pending
            // attention rows.
            self.dismiss_active_notifications(cx);
        } else if !current.preferences.notification_mode.has_notifications() {
            self.dismiss_active_notifications(cx);
        } else {
            self.publish_notification_badge();
        }
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
        self.sidebar_animation.reset();
        self.right_panel_animation.reset();
        self.terminal_drawer_animation.reset();
        self.sync_composer(window, cx);
    }

    fn dismiss_active_notifications(&mut self, cx: &mut Context<Self>) {
        let tags = std::mem::take(&mut self.active_notification_tags);
        for tag in tags.keys() {
            cx.dismiss_system_notification(tag);
        }
        self.publish_notification_badge();
    }

    fn dismiss_environment_notifications(&mut self, environment_id: &str, cx: &mut Context<Self>) {
        let removed = self
            .active_notification_tags
            .iter()
            .filter(|(_, owner)| owner.as_deref() == Some(environment_id))
            .map(|(tag, _)| tag.clone())
            .collect::<Vec<_>>();
        for tag in removed {
            self.active_notification_tags.remove(&tag);
            cx.dismiss_system_notification(&tag);
        }
        self.publish_notification_badge();
    }

    fn publish_notification_badge(&self) {
        platform::set_notification_badge(self.active_notification_tags.len() as u32);
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

    /// Handles the route returned by a native notification adapter on the UI
    /// owner. The callback selects the owning environment before dispatching
    /// the thread intent, so a click never opens the same id on another Host.
    fn open_notification_route(
        &mut self,
        deep_link: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match agent_domain::parse_activity_deep_link(deep_link) {
            Some(agent_domain::ActivityDeepLink::Overview) => {
                self.route = Route::Chat;
                self.perform(Intent::LeaveThread);
                cx.notify();
            }
            Some(agent_domain::ActivityDeepLink::Thread {
                environment_id,
                thread_id,
            }) => {
                self.route = Route::Chat;
                self.pending_open = Some((environment_id, thread_id));
                self.apply_pending_open(window, cx);
            }
            None => {}
        }
    }

    /// Queues a native notification response for the window-owned tick.
    pub(crate) fn queue_notification_route(&mut self, route: String) {
        self.pending_notification_route = Some(route);
    }

    fn begin_new_thread(&self, project_id: Option<String>) {
        self.perform(Intent::NewThread { project_id });
    }

    /// Requests one capacity sample from each live Store. This is called only
    /// for an unresolved automatic draft; idle environments do not poll.
    fn refresh_load_balancing_resources(&self) {
        if let Some(session) = &self.session {
            let _ = session
                .store
                .dispatch(Intent::RefreshLoadBalancingResources);
        }
        for session in self.background_sessions.values() {
            let _ = session
                .store
                .dispatch(Intent::RefreshLoadBalancingResources);
        }
    }

    fn invalidate_load_balancing_attempt(&mut self) {
        self.load_balancing_attempt_generation =
            self.load_balancing_attempt_generation.wrapping_add(1);
        self.pending_load_balanced_new_thread = None;
        self.load_balancing_refresh_requested = false;
    }

    fn automatic_new_thread(
        &mut self,
        project_id: &str,
        allow_refresh: bool,
    ) -> AutomaticNewThreadResult {
        if !self.snapshot.preferences.load_balancing_enabled {
            return AutomaticNewThreadResult::Unavailable;
        }
        let Some(source_environment_id) = self
            .snapshot
            .environment
            .as_ref()
            .map(|environment| environment.environment_id.clone())
        else {
            return AutomaticNewThreadResult::Unavailable;
        };
        let draft = self
            .snapshot
            .new_thread_default_draft_for_project(Some(project_id));
        if draft.instance_id.is_empty() || draft.model.is_empty() {
            return AutomaticNewThreadResult::Unavailable;
        }
        let evaluation = self.environment_registry.evaluate_load_balancing(
            &source_environment_id,
            project_id,
            draft.driver,
            (!draft.instance_id.is_empty()).then_some(draft.instance_id.as_str()),
            &draft.model,
            &draft.options,
            draft.runtime_mode,
            draft.interaction_mode,
            &self.snapshot.preferences.load_balancing_weights,
            ui::now_ms(),
        );
        if evaluation.candidate_count < 2 {
            return AutomaticNewThreadResult::Unavailable;
        }
        if evaluation.pending_resources {
            if allow_refresh && !self.load_balancing_refresh_requested {
                self.pending_load_balanced_new_thread = Some(PendingLoadBalancedNewThread {
                    project_id: project_id.to_owned(),
                    source_environment_id,
                    started_at_ms: ui::now_ms(),
                    generation: self.load_balancing_attempt_generation,
                });
                self.load_balancing_refresh_requested = true;
                self.refresh_load_balancing_resources();
            }
            return AutomaticNewThreadResult::Waiting;
        }
        let Some(route) = evaluation.route else {
            return AutomaticNewThreadResult::Unavailable;
        };
        if self.environment_registry.selected() != Some(route.environment_id.as_str()) {
            if !self.promote_environment(&route.environment_id) {
                return AutomaticNewThreadResult::Unavailable;
            }
        } else {
            self.invalidate_load_balancing_attempt();
        }
        let selection = (
            route.provider_instance,
            route.driver,
            route.model,
            route.options,
            route.runtime_mode,
            route.interaction_mode,
        );
        let target_environment_id = route.environment_id;
        let target_project = route.project_id;
        let source_environment_id_for_fallback = source_environment_id.clone();
        let fallback_project = project_id.to_owned();
        let attempt_generation = self.load_balancing_attempt_generation;
        self.perform_then(
            Intent::NewThread {
                project_id: Some(target_project),
            },
            move |view, result, window, cx| {
                if view.load_balancing_attempt_generation != attempt_generation {
                    return;
                }
                if result.is_err() {
                    if view.environment_registry.selected() != Some(target_environment_id.as_str())
                    {
                        return;
                    }
                    view.begin_new_thread_on_environment(
                        &source_environment_id_for_fallback,
                        &fallback_project,
                        window,
                        cx,
                    );
                    return;
                }
                view.perform(Intent::SetModel {
                    instance_id: selection.0.clone(),
                    driver: selection.1,
                    model: selection.2.clone(),
                    options: selection.3.clone(),
                });
                view.perform(Intent::SetRuntimeMode { mode: selection.4 });
                view.perform(Intent::SetInteractionMode { mode: selection.5 });
            },
        );
        AutomaticNewThreadResult::Started
    }

    fn begin_new_thread_on_environment(
        &mut self,
        environment_id: &str,
        project_id: &str,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) -> bool {
        if self.environment_registry.selected() != Some(environment_id)
            && !self.promote_environment(environment_id)
        {
            return false;
        }
        self.snapshot_changed(window, cx);
        self.begin_new_thread(Some(project_id.to_owned()));
        true
    }

    fn retry_pending_load_balanced_new_thread(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Desktop>,
    ) {
        let Some(pending) = self.pending_load_balanced_new_thread.clone() else {
            return;
        };
        match load_balancing::pending_route_action(
            pending.generation,
            self.load_balancing_attempt_generation,
            &pending.source_environment_id,
            self.environment_registry.selected(),
            pending.started_at_ms,
            ui::now_ms(),
            3_000,
        ) {
            PendingRouteAction::Cancel => {
                self.invalidate_load_balancing_attempt();
                return;
            }
            PendingRouteAction::Fallback => {
                self.invalidate_load_balancing_attempt();
                self.begin_new_thread_on_environment(
                    &pending.source_environment_id,
                    &pending.project_id,
                    window,
                    cx,
                );
                return;
            }
            PendingRouteAction::Retry => {}
        }
        match self.automatic_new_thread(&pending.project_id, false) {
            AutomaticNewThreadResult::Waiting => {}
            AutomaticNewThreadResult::Started => {}
            AutomaticNewThreadResult::Unavailable => {
                self.invalidate_load_balancing_attempt();
                self.begin_new_thread_on_environment(
                    &pending.source_environment_id,
                    &pending.project_id,
                    window,
                    cx,
                );
            }
        }
    }

    /// Starts a new-thread draft in `project_id`, leaving settings.
    pub(crate) fn new_thread(&mut self, project_id: Option<String>, cx: &mut Context<Self>) {
        self.route = Route::Chat;
        self.invalidate_load_balancing_attempt();
        let automatic = project_id
            .as_deref()
            .map_or(AutomaticNewThreadResult::Unavailable, |project_id| {
                self.automatic_new_thread(project_id, true)
            });
        if matches!(automatic, AutomaticNewThreadResult::Unavailable) {
            self.begin_new_thread(project_id);
        }
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
        let capture = self.snapshot.preferences.snapshot_capture.clone();
        if capture.shortcut == agent_core::view::snapshot_capture::SnapshotShortcut::BothShiftKeys
            && keystroke.key.eq_ignore_ascii_case("shift")
        {
            self.snapshot_shift_presses = self.snapshot_shift_presses.saturating_add(1);
        } else if !keystroke.modifiers.shift {
            self.snapshot_shift_presses = 0;
        }
        if capture.enabled
            && agent_core::view::snapshot_capture::shortcut_matches(
                capture.shortcut,
                &keystroke.key,
                keystroke.modifiers.shift,
                keystroke.modifiers.platform,
                keystroke.modifiers.control,
                self.snapshot_shift_presses >= 2,
            )
            && let Some(draft_key) = self.composer_attachment_target()
        {
            self.capture_snapshot(draft_key);
            self.snapshot_shift_presses = 0;
            return true;
        }
        if keystroke.key == "k" && (keystroke.modifiers.platform || keystroke.modifiers.control) {
            self.open_command_palette(window, cx);
            return true;
        }
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

    fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.command_palette_open = false;
        self.command_palette_query
            .update(cx, |query, cx| query.set_value("", window, cx));
        self.open_command_palette_menu(window, cx);
    }

    fn reopen_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_menu(cx);
        self.open_command_palette_menu(window, cx);
    }

    fn open_command_palette_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self.command_palette_items();
        let query = self.command_palette_query.read(cx).value().to_string();
        let items = command_palette::filter(&items, &query, &Default::default());
        let input = self.command_palette_query.clone();
        let owner = cx.entity().downgrade();
        self.open_menu(
            point(px(240.), px(72.)),
            window,
            cx,
            move |mut menu, _, _| {
                menu = menu
                    .label("Command palette")
                    .item(PopupMenuItem::element(move |_, _| {
                        Input::new(&input)
                            .appearance(false)
                            .small()
                            .aria_label("Command palette search")
                    }))
                    .min_w(px(320.));
                for item in items {
                    let key = item.key;
                    let title = item.title;
                    let owner = owner.clone();
                    menu = menu.item(PopupMenuItem::new(title).on_click(move |_, window, cx| {
                        let key = key.clone();
                        let _ = owner.update(cx, |view, cx| {
                            view.close_menu(cx);
                            match key.strip_prefix("thread:") {
                                Some(thread) => view.open_thread(thread.to_owned(), cx),
                                None if key == "action:new" => view.new_thread(None, cx),
                                None if key == "action:settings" => {
                                    view.open_settings(settings::SettingsPage::General, window, cx)
                                }
                                None if key == "action:sidebar" => {
                                    view.sidebar_hidden = !view.sidebar_hidden;
                                    cx.notify();
                                }
                                _ => {}
                            }
                        });
                    }));
                }
                menu
            },
        );
        self.command_palette_open = true;
        self.command_palette_query
            .update(cx, |query, cx| query.focus(window, cx));
    }

    fn command_palette_items(&self) -> Vec<CommandPaletteItem> {
        let mut items = vec![
            CommandPaletteItem {
                key: "action:new".into(),
                kind: CommandPaletteItemKind::Action,
                title: "New thread".into(),
                detail: Some("Start a conversation".into()),
                search_terms: vec!["chat".into()],
            },
            CommandPaletteItem {
                key: "action:settings".into(),
                kind: CommandPaletteItemKind::Action,
                title: "Open settings".into(),
                detail: None,
                search_terms: vec!["preferences".into()],
            },
            CommandPaletteItem {
                key: "action:sidebar".into(),
                kind: CommandPaletteItemKind::Action,
                title: "Toggle sidebar".into(),
                detail: None,
                search_terms: vec!["navigation".into()],
            },
        ];
        items.extend(self.views.sidebar.rows().map(|row| CommandPaletteItem {
            key: format!("thread:{}", row.id),
            kind: CommandPaletteItemKind::Thread,
            title: row.title.clone(),
            detail: row.project_name.clone(),
            search_terms: vec![row.branch.clone().unwrap_or_default()],
        }));
        items
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

impl Desktop {
    fn panel_animation_duration() -> Duration {
        Duration::from_millis(u64::from(ui::appearance().panel_animation_ms))
    }

    fn render_navigation(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let navigation = match self.route {
            Route::Settings => self.render_settings_nav(window, cx),
            Route::Chat => self.render_sidebar(window, cx),
        };
        let width = ui::metrics().sidebar_width;
        let hidden = self.sidebar_hidden;
        let duration = Self::panel_animation_duration();
        let navigation = div()
            .id("desktop-navigation")
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .w(px(if hidden { 0. } else { width }))
            .child(navigation);
        match self
            .sidebar_animation
            .prepare("navigation", if hidden { 0. } else { 1. }, duration)
        {
            Some(animation) => navigation
                .with_animation(
                    ("desktop-navigation-animation", animation.run),
                    Animation::new(animation.duration).with_easing(panel_ease_out),
                    move |navigation, delta| {
                        let progress = animation.from + (animation.target - animation.from) * delta;
                        navigation.w(px(width * progress))
                    },
                )
                .into_any_element(),
            None => navigation.into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::local_host_power_publish_allowed;

    #[test]
    fn desktop_power_requires_a_verified_local_host_without_remote_or_inflight_probe() {
        assert!(local_host_power_publish_allowed(true, false, false));
        assert!(!local_host_power_publish_allowed(false, false, false));
        assert!(!local_host_power_publish_allowed(true, true, false));
        assert!(!local_host_power_publish_allowed(true, false, true));
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
        let snapshot_flash = self
            .snapshot_feedback_until
            .is_some_and(|until| until > std::time::Instant::now());
        let snapshot_overlay = snapshot_flash.then(|| {
            let overlay = div()
                .id(("snapshot-feedback", self.snapshot_feedback_id))
                .absolute()
                .inset_0()
                .bg(ui::tint("text", 0.12));
            if self.snapshot_feedback_animated {
                overlay
                    .with_animation(
                        ("snapshot-feedback-fade", self.snapshot_feedback_id),
                        Animation::new(std::time::Duration::from_millis(220)),
                        |overlay, progress| overlay.opacity(1. - progress),
                    )
                    .into_any_element()
            } else {
                overlay.into_any_element()
            }
        });
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
                    .child(self.render_navigation(window, cx))
                    .child(main),
            )
            .children(snapshot_overlay)
            .children(self.panels.render_preview_mini_player(cx))
            .children(gpui_kit::component::Root::render_dialog_layer(window, cx))
            .children(gpui_kit::component::Root::render_notification_layer(
                window, cx,
            ))
    }
}

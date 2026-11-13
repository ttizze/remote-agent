//! The application owns UI resources shared by native views; each view observes immutable snapshots.
use crate::{
    client::{Client, Update},
    shared::Shared,
};
use agent_core::{
    client::AgentError,
    localization::UiLanguage,
    panels::{PanelKind, PanelScope, Panels},
    presentation::view::Presentation,
    state::{DraftKey, Intent},
    store::SideChatStart,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

mod worktrees;
pub(crate) use worktrees::{WorktreeAction, WorktreeEditor};
mod connections;
pub(crate) use connections::{ListRequest, ListScope, ListSource};
#[cfg(desktop)]
mod onboarding;

#[derive(Clone, PartialEq)]
pub(crate) struct Host {
    pub id: String,
    pub name: String,
}

impl Host {
    /// How the UI names the Host: this machine's own in the UI's language.
    pub(crate) fn label(&self) -> String {
        if self.id == crate::client::LOCAL {
            crate::l10n::tr("このPC").into()
        } else {
            self.name.clone()
        }
    }
}

pub(crate) enum Navigation {
    Settings(bool),
    Provider(Option<agent_protocol::session::ProviderKind>),
    /// The sidebar's toggle, in a window this wide.
    ToggleSidebar(f32),
    /// The reader went somewhere from the sidebar in a window this wide.
    #[cfg(desktop)]
    LeftSidebar(f32),
    /// The sidebar's sheet was dismissed.
    #[cfg(desktop)]
    DismissSidebarSheet,
}

#[derive(Clone)]
pub(crate) struct SideChat {
    pub publication: Option<Shared<Presentation>>,
    pub pending: bool,
    pub failure: Option<agent_core::presentation::error::ErrorText>,
    provider: Option<agent_protocol::session::ProviderKind>,
    start: SideChatStart,
}

#[derive(Clone)]
pub(crate) struct State {
    pub hosts: Vec<Host>,
    pub host: String,
    pub all_hosts: bool,
    pub publications: BTreeMap<String, Shared<Presentation>>,
    pub panels: Arc<Panels>,
    pub failure: Option<String>,
    pub pairing: bool,
    pub settings: bool,
    pub provider: Option<agent_protocol::session::ProviderKind>,
    pub account_pending: bool,
    pub sidebar: agent_core::presentation::sidebar::SidebarState,
    #[cfg(any(desktop, test))]
    pub sidebar_projects: Vec<agent_core::presentation::sidebar::ProjectKey>,
    /// Each Host's observed navigation survives sidebar and language remounts.
    #[cfg(any(desktop, test))]
    sidebar_routes: BTreeMap<String, agent_core::presentation::sidebar::ProjectReveal>,
    pub file_line: Option<(u64, Option<u32>)>,
    pub side_chats: BTreeMap<u64, SideChat>,
    pub browser_renderer: String,
    pub worktrees: Option<WorktreeEditor>,
    pub language: UiLanguage,
    pub contrast: u32,
    /// The width the user gave the file list beside an open file.
    #[cfg(any(desktop, test))]
    pub explorer_width: f32,
    /// This app's own update, on the desktop.
    #[cfg(desktop)]
    pub update: Option<agent_core::updates::UpdateState>,
    /// The update's worker waits for this app to end its event loop.
    #[cfg(desktop)]
    pub quitting: bool,
    /// The desktop's first-run setup and its project import.
    #[cfg(desktop)]
    pub onboarding: agent_core::presentation::onboarding::Onboarding,
    uploads: BTreeMap<uuid::Uuid, (String, DraftKey)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            hosts: Vec::new(),
            host: String::new(),
            all_hosts: false,
            publications: BTreeMap::new(),
            panels: Panels::new(),
            failure: None,
            pairing: false,
            settings: false,
            provider: None,
            account_pending: false,
            sidebar: Default::default(),
            #[cfg(any(desktop, test))]
            sidebar_projects: Vec::new(),
            #[cfg(any(desktop, test))]
            sidebar_routes: BTreeMap::new(),
            file_line: None,
            side_chats: BTreeMap::new(),
            browser_renderer: String::new(),
            worktrees: None,
            language: UiLanguage::default(),
            contrast: agent_core::presentation::appearance::DEFAULT_CONTRAST,
            #[cfg(any(desktop, test))]
            explorer_width: agent_core::presentation::files::EXPLORER_DEFAULT_WIDTH,
            #[cfg(desktop)]
            update: None,
            #[cfg(desktop)]
            quitting: false,
            #[cfg(desktop)]
            onboarding: Default::default(),
            uploads: BTreeMap::new(),
        }
    }
}

impl State {
    pub fn account_locked(&self) -> bool {
        self.account_pending
            || self.publications.get(&self.host).is_some_and(|p| {
                p.settings.account_login().is_some()
                    || self.provider.is_some_and(|provider| {
                        p.snapshot.operation_running(
                            agent_core::state::operations::OperationKey::AccountLogin { provider },
                        )
                    })
            })
    }

    fn receive(&mut self, update: Update) {
        match update {
            Update::AttachmentsFinished(id) => {
                self.uploads.remove(&id);
            }
            Update::Hosts(hosts) => {
                self.hosts = hosts
                    .into_iter()
                    .map(|host| Host {
                        id: host.id,
                        name: host.name,
                    })
                    .collect();
                let hosts = &self.hosts;
                self.publications
                    .retain(|id, _| hosts.iter().any(|host| &host.id == id));
                self.resolve_selection();
            }
            Update::Presentation { host, value } => {
                let before = self.publications.get(&host).map(|p| {
                    p.snapshot
                        .panel_scope(host.clone(), p.context.navigation().draft_key)
                });
                if let Some(before) = before {
                    self.panels = self.panels.clone().inherit_visibility(
                        before,
                        value
                            .snapshot
                            .panel_scope(host.clone(), value.context.navigation().draft_key),
                        value.context.navigation().thread_id.is_none(),
                    );
                }
                self.publications.insert(host, Shared(value));
            }
            Update::Error(reason) => self.failure = Some(reason),
            Update::PanePresentation { id, value } => {
                if let Some(pane) = self.side_chats.get_mut(&id) {
                    pane.publication = Some(Shared(value));
                }
            }
            Update::SideChatReady { id, result } => {
                if let Some(pane) = self.side_chats.get_mut(&id) {
                    pane.pending = false;
                    match result {
                        Ok(value) => {
                            pane.publication = Some(Shared(value));
                            pane.failure = None;
                            // Its first input is in: preparing it again only
                            // reopens its conversation.
                            pane.start = SideChatStart::Quote(String::new());
                        }
                        Err(reason) => {
                            pane.failure = Some(agent_core::presentation::error::error_text(
                                crate::l10n::language(),
                                &reason,
                            ))
                        }
                    }
                }
            }
            Update::Paired(result) => match result {
                Ok(host) => {
                    self.host = host;
                    self.all_hosts = false;
                    self.pairing = false;
                    self.settings = false;
                    self.provider = None;
                }
                Err(reason) => {
                    self.pairing = false;
                    self.failure = Some(reason);
                }
            },
        }
        if self
            .worktrees
            .as_ref()
            .is_some_and(|editor| editor.host != self.host)
        {
            self.worktrees = None;
        }
    }

    pub fn uploading(&self, host: &str, draft: &DraftKey) -> bool {
        self.uploads
            .values()
            .any(|(target, key)| target == host && key == draft)
    }
}

pub(crate) struct Application {
    pub client: Arc<Client>,
    instance: uuid::Uuid,
    state: watch::Sender<Arc<State>>,
    stop: CancellationToken,
    runtime: tokio::runtime::Handle,
    jobs: tokio_util::task::TaskTracker,
    selection_path: Option<PathBuf>,
    #[cfg(desktop)]
    onboarding_path: PathBuf,
    #[cfg(any(desktop, test))]
    explorer_width_path: PathBuf,
    #[cfg(any(desktop, test))]
    sidebar_projects_path: PathBuf,
    #[cfg(desktop)]
    updater: Option<Arc<agent_core::updates::Updater>>,
}

impl Application {
    #[cfg(target_os = "macos")]
    pub fn set_browser_renderer(&self, id: String) {
        self.state
            .send_modify(|state| Arc::make_mut(state).browser_renderer = id);
    }

    /// `local` is this machine's Host scope, which the desktop starts and updates.
    pub fn new(
        root: PathBuf,
        local: Option<agent_local_host::launch::HostScope>,
    ) -> Result<(tokio::runtime::Runtime, Arc<Self>), AgentError> {
        Self::start(root, local, None)
    }

    #[cfg(any(target_os = "ios", test))]
    pub(crate) fn embedded(
        root: PathBuf,
        borrowed: crate::client::BorrowedSession,
    ) -> Result<(tokio::runtime::Runtime, Arc<Self>), AgentError> {
        Self::start(root, None, Some(borrowed))
    }

    fn start(
        root: PathBuf,
        local: Option<agent_local_host::launch::HostScope>,
        borrowed: Option<crate::client::BorrowedSession>,
    ) -> Result<(tokio::runtime::Runtime, Arc<Self>), AgentError> {
        let selection_path = borrowed
            .is_none()
            .then(|| root.join("connection-selection.json"));
        let selection = match &selection_path {
            Some(path) => connections::load_selection(path)?,
            None => agent_core::presentation::connection_list::ConnectionSelection {
                host_id: borrowed.as_ref().map(|borrowed| borrowed.host.clone()),
                all: false,
            },
        };
        let language = borrowed
            .as_ref()
            .map(|borrowed| borrowed.session.snapshot().catalog.ui_language);
        #[cfg(any(desktop, test))]
        let explorer_width_path = root.join("file-explorer-width.json");
        #[cfg(any(desktop, test))]
        let sidebar_projects_path = root.join("sidebar-projects.json");
        #[cfg(any(desktop, test))]
        let sidebar_projects = crate::storage::read(&sidebar_projects_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        // As the language, an unreadable width is the default's.
        #[cfg(any(desktop, test))]
        let explorer_width = crate::storage::read(&explorer_width_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<f32>(&bytes).ok())
            .map_or(
                agent_core::presentation::files::EXPLORER_DEFAULT_WIDTH,
                |width| agent_core::presentation::files::explorer_width(width, f32::INFINITY),
            );
        #[cfg(desktop)]
        let onboarding_path = root.join(onboarding::COMPLETED);
        #[cfg(desktop)]
        let scope = local.clone();
        let (runtime, client, incoming) = Client::start(root, local, borrowed)?;
        // The device's language, which Core's copy follows too; an unreadable
        // choice is the default, as on a first launch.
        let language = language.unwrap_or_else(|| client.device.language());
        crate::l10n::set(language);
        let contrast = client.device.contrast();
        crate::ui::colors::set_contrast(contrast);
        #[cfg(desktop)]
        let updater = scope.map(|scope| {
            agent_core::updates::Updater::start(
                scope,
                agent_core::updates::build_number(),
                runtime.handle().clone(),
            )
        });
        let (state, _) = watch::channel(Arc::new(State {
            host: selection.host_id.clone().unwrap_or_default(),
            all_hosts: selection.all,
            language,
            contrast,
            #[cfg(any(desktop, test))]
            explorer_width,
            #[cfg(any(desktop, test))]
            sidebar_projects,
            #[cfg(desktop)]
            onboarding: agent_core::presentation::onboarding::Onboarding::new(
                onboarding_path.is_file(),
            ),
            ..State::default()
        }));
        let application = Arc::new(Self {
            client,
            instance: uuid::Uuid::new_v4(),
            state,
            stop: CancellationToken::new(),
            runtime: runtime.handle().clone(),
            jobs: Default::default(),
            selection_path,
            #[cfg(desktop)]
            onboarding_path,
            #[cfg(any(desktop, test))]
            explorer_width_path,
            #[cfg(any(desktop, test))]
            sidebar_projects_path,
            #[cfg(desktop)]
            updater,
        });
        application.persist_selection(selection);
        #[cfg(desktop)]
        application.publish_update();
        let owner = Arc::downgrade(&application);
        let stop = application.stop.clone();
        application.jobs.spawn_on(async move {
            loop {
                let update = tokio::select! {
                    _ = stop.cancelled() => break,
                    update = incoming.recv() => match update { Ok(update) => update, Err(_) => break },
                };
                let Some(owner) = owner.upgrade() else { break };
                let listed = matches!(update, Update::Hosts(_));
                owner.receive(update);
                // A chosen WSL distro's Host starts once it is listed; the
                // client starts no distro on its own.
                let host = owner.snapshot().host.clone();
                if listed && agent_local_host::wsl::host_distro(&host).is_some() {
                    owner.client.reconnect(host);
                }
            }
        }, runtime.handle());
        Ok((runtime, application))
    }

    fn receive(&self, update: Update) {
        self.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            #[cfg(any(desktop, test))]
            let published_host = match &update {
                Update::Presentation { host, .. } => Some(host.clone()),
                _ => None,
            };
            state.receive(update);
            #[cfg(any(desktop, test))]
            if let Some(host) = published_host
                && let Some(value) = state.publications.get(&host)
            {
                let route = value
                    .snapshot
                    .sidebar_route(host.clone(), &value.context.navigation());
                let previous = state.sidebar_routes.get(&host).cloned().unwrap_or_default();
                let (observed, reveal) =
                    previous.observe(&route.draft, &route.project, host == state.host);
                state.sidebar_routes.insert(host, observed);
                // Resolve project metadata only for new navigation, never for
                // every streamed update of an already observed conversation.
                let widens = reveal && {
                    let inputs = state.list_inputs();
                    let catalogs = inputs
                        .iter()
                        .map(|(host, list)| (host.as_str(), &**list))
                        .collect::<Vec<_>>();
                    let selected = agent_core::presentation::sidebar::resolve_projects(
                        &state.sidebar_projects,
                        &catalogs,
                        state.sidebar_catalog_ready(),
                    );
                    agent_core::presentation::sidebar::widens_scope(&selected, &route)
                };
                if widens {
                    self.write_sidebar_projects(state, Vec::new());
                }
            }
        });
    }

    /// Views read the update with the rest of the application's state.
    #[cfg(desktop)]
    fn publish_update(self: &Arc<Self>) {
        let Some(mut updates) = self.updater.as_ref().map(|updater| updater.subscribe()) else {
            return;
        };
        let owner = Arc::downgrade(self);
        let stop = self.stop.clone();
        self.jobs.spawn_on(
            async move {
                loop {
                    let value = updates.borrow_and_update().clone();
                    let Some(owner) = owner.upgrade() else { break };
                    owner
                        .state
                        .send_modify(|state| Arc::make_mut(state).update = Some(value));
                    drop(owner);
                    tokio::select! {
                        _ = stop.cancelled() => break,
                        changed = updates.changed() => if changed.is_err() { break },
                    }
                }
            },
            &self.runtime,
        );
    }

    #[cfg(desktop)]
    pub fn select_update_channel(&self, channel: agent_core::updates::Channel) {
        if let Some(updater) = &self.updater {
            updater.select_channel(channel);
        }
    }

    /// Checks, downloads or restarts into a release, as the update's phase allows.
    /// The restart quits once the worker that replaces this app is waiting.
    #[cfg(desktop)]
    pub fn advance_update(self: &Arc<Self>) {
        use agent_core::updates::UpdatePhase;
        let Some(updater) = self.updater.clone() else {
            return;
        };
        let phase = updater.subscribe().borrow().phase.clone();
        match phase {
            UpdatePhase::Current | UpdatePhase::Failed(_) => updater.check(),
            UpdatePhase::Available(_) => updater.download(),
            UpdatePhase::Ready(_) => {
                let owner = Arc::downgrade(self);
                self.runtime.spawn(async move {
                    if let Some(lease) = updater.restart().await {
                        // The worker replaces the app after this process exits
                        // and releases the lock.
                        std::mem::forget(lease);
                        if let Some(owner) = owner.upgrade() {
                            owner
                                .state
                                .send_modify(|state| Arc::make_mut(state).quitting = true);
                        }
                    }
                });
            }
            _ => {}
        }
    }

    pub fn snapshot(&self) -> Arc<State> {
        self.state.borrow().clone()
    }
    pub fn subscribe(&self) -> watch::Receiver<Arc<State>> {
        self.state.subscribe()
    }

    pub fn pair(&self, invitation: agent_protocol::models::Invitation) {
        let start = self.state.send_if_modified(|state| {
            if state.pairing {
                return false;
            }
            Arc::make_mut(state).pairing = true;
            true
        });
        if start {
            self.client.pair(invitation);
        }
    }

    /// Saves the file list's width and publishes it; a width that cannot be
    /// saved still holds until the app ends, and the failure shows.
    #[cfg(any(desktop, test))]
    pub fn set_explorer_width(&self, width: f32) {
        let saved = agent_local_host::save_private_json(&self.explorer_width_path, &width);
        self.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.explorer_width = width;
            if let Err(reason) = saved {
                state.failure = Some(reason.to_string());
            }
        });
    }

    #[cfg(any(desktop, test))]
    pub fn select_sidebar_projects(
        &self,
        selected: Vec<agent_core::presentation::sidebar::ProjectKey>,
    ) {
        if self.snapshot().sidebar_projects == selected {
            return;
        }
        self.state.send_modify(|state| {
            self.write_sidebar_projects(Arc::make_mut(state), selected);
        });
    }

    #[cfg(any(desktop, test))]
    fn write_sidebar_projects(
        &self,
        state: &mut State,
        selected: Vec<agent_core::presentation::sidebar::ProjectKey>,
    ) {
        let saved = agent_local_host::save_private_json(&self.sidebar_projects_path, &selected);
        state.sidebar_projects = selected;
        if let Err(reason) = saved {
            state.failure = Some(reason.to_string());
        }
    }

    /// Saves the UI language, then publishes it so the root is made again in it.
    #[cfg(any(desktop, test))]
    pub fn select_language(&self, language: UiLanguage) {
        if let Err(reason) = self.client.device.select_language(language) {
            self.fail(reason.to_string());
            return;
        }
        crate::l10n::set(language);
        self.state
            .send_modify(|state| Arc::make_mut(state).language = language);
    }

    /// Saves the interface contrast, then publishes it so the root is made again.
    #[cfg(desktop)]
    pub fn select_contrast(&self, contrast: u32) {
        if let Err(reason) = self.client.device.select_contrast(contrast) {
            self.fail(reason.to_string());
            return;
        }
        crate::ui::colors::set_contrast(contrast);
        self.state
            .send_modify(|state| Arc::make_mut(state).contrast = contrast);
    }

    /// Shows `reason` as the application's failure until it is dismissed.
    #[cfg(any(desktop, test))]
    pub fn fail(&self, reason: String) {
        self.state
            .send_modify(|state| Arc::make_mut(state).failure = Some(reason));
    }

    pub fn dismiss_failure(&self) {
        self.state
            .send_modify(|state| Arc::make_mut(state).failure = None);
    }

    pub fn navigate(&self, action: Navigation) {
        self.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            match action {
                Navigation::Settings(open) => {
                    state.settings = open;
                    state.provider = None;
                }
                Navigation::Provider(provider) => state.provider = provider,
                Navigation::ToggleSidebar(width) => state.sidebar = state.sidebar.toggled(width),
                #[cfg(desktop)]
                Navigation::LeftSidebar(width) => state.sidebar = state.sidebar.navigated(width),
                #[cfg(desktop)]
                Navigation::DismissSidebarSheet => {
                    state.sidebar = state.sidebar.sheet_dismissed();
                }
            }
        });
    }

    pub fn account_request(self: &Arc<Self>, host: String, intent: Intent) {
        if self.stop.is_cancelled() {
            return;
        }
        let accepted = self.state.send_if_modified(|state| {
            if state.account_pending || state.host != host {
                return false;
            }
            Arc::make_mut(state).account_pending = true;
            true
        });
        if !accepted {
            return;
        }
        let response = self.client.request(&host, None, intent);
        let owner = Arc::downgrade(self);
        let stop = self.stop.clone();
        self.jobs.spawn_on(
            async move {
                tokio::select! { _ = stop.cancelled() => return, _ = response.recv() => {} }
                if let Some(owner) = owner.upgrade() {
                    owner
                        .state
                        .send_modify(|state| Arc::make_mut(state).account_pending = false);
                }
            },
            &self.runtime,
        );
    }

    #[cfg(any(desktop, test))]
    pub fn set_panel_maximized(&self, scope: PanelScope, maximized: bool) {
        self.state.send_modify(|state| {
            let panels = state.panels.clone().set_maximized(scope, maximized);
            Arc::make_mut(state).panels = panels;
        });
    }

    pub fn attach(
        &self,
        host: String,
        pane: Option<u64>,
        draft: DraftKey,
        sources: Vec<crate::client::AttachmentSource>,
    ) {
        if sources.is_empty()
            || pane.is_some_and(|id| !self.snapshot().side_chats.contains_key(&id))
        {
            return;
        }
        let id = uuid::Uuid::new_v4();
        self.state.send_modify(|state| {
            Arc::make_mut(state)
                .uploads
                .insert(id, (host.clone(), draft.clone()));
        });
        self.client.attach_batch(id, host, pane, draft, sources);
    }

    pub fn open_side_chat(
        &self,
        scope: PanelScope,
        cwd: String,
        provider: Option<agent_protocol::session::ProviderKind>,
        start: SideChatStart,
    ) {
        let mut id = 0;
        self.state.send_modify(|state| {
            let opened = state
                .panels
                .clone()
                .open(scope, PanelKind::SideChat, Some(cwd));
            id = opened.id;
            let state = Arc::make_mut(state);
            state.panels = opened.panels;
            state.side_chats.insert(
                id,
                SideChat {
                    publication: None,
                    pending: false,
                    failure: None,
                    provider,
                    start,
                },
            );
        });
        self.prepare_side_chat(id);
    }

    /// Asks side chat `id` to explain `text` in a conversation of its own.
    #[cfg(any(desktop, target_os = "ios"))]
    pub fn explain_in_side_chat(&self, id: u64, text: String) {
        self.state.send_modify(|state| {
            if let Some(chat) = Arc::make_mut(state).side_chats.get_mut(&id) {
                chat.start = SideChatStart::Explain(text);
            }
        });
        self.prepare_side_chat(id);
    }

    pub fn prepare_side_chat(&self, id: u64) {
        let mut request = None;
        self.state.send_if_modified(|state| {
            let Some(scope) = state.panels.scope(id) else {
                return false;
            };
            let cwd = state
                .panels
                .get(scope.clone())
                .surfaces
                .into_iter()
                .find(|s| s.id == id)
                .and_then(|s| s.path)
                .unwrap_or_default();
            let Some(pane) = Arc::make_mut(state)
                .side_chats
                .get_mut(&id)
                .filter(|pane| !pane.pending)
            else {
                return false;
            };
            pane.pending = true;
            pane.failure = None;
            request = Some((scope.host, cwd, pane.provider, pane.start.clone()));
            true
        });
        if let Some((host, cwd, provider, start)) = request {
            self.client
                .prepare_side_chat(id, host, cwd, provider, start);
        }
    }

    pub fn open_panel(
        &self,
        scope: PanelScope,
        kind: PanelKind,
        path: Option<String>,
        line: Option<u32>,
    ) {
        self.state.send_modify(|state| {
            let next = state.panels.clone().open(scope, kind, path);
            let state = Arc::make_mut(state);
            state.panels = next.panels;
            if kind == PanelKind::File {
                state.file_line = Some((next.id, line));
            }
        });
    }

    #[cfg(desktop)]
    pub fn open_script(&self, scope: PanelScope, cwd: String, command: String) {
        self.state.send_modify(|state| {
            let next = state.panels.clone().open_script(scope, cwd, command);
            Arc::make_mut(state).panels = next.panels;
        });
    }

    pub fn set_panel_open(&self, scope: PanelScope, open: bool) {
        self.state.send_modify(|state| {
            let panels = state.panels.clone().set_open(scope, open);
            Arc::make_mut(state).panels = panels;
        });
    }

    pub fn open_setup_terminal(
        &self,
        scope: PanelScope,
        terminal: agent_core::presentation::conversation::SetupTerminal,
    ) {
        self.state.send_modify(|state| {
            let opened = state.panels.clone().open_setup_terminal(scope, terminal);
            Arc::make_mut(state).panels = opened.panels;
        });
    }

    pub fn activate_panel(&self, scope: PanelScope, id: u64) {
        self.state.send_modify(|state| {
            let panels = state.panels.clone().activate(scope, id).panels;
            Arc::make_mut(state).panels = panels;
        });
    }

    pub fn close_panel(&self, scope: PanelScope, id: u64) {
        let handle = self.terminal_handle(id);
        let mut terminal = false;
        let closed = self.state.send_if_modified(|state| {
            if state.panels.scope(id).as_ref() != Some(&scope) {
                return false;
            }
            terminal = state
                .panels
                .get(scope.clone())
                .surfaces
                .iter()
                .any(|surface| surface.id == id && surface.kind == PanelKind::Terminal);
            let panels = state.panels.clone().close(scope.clone(), id);
            let state = Arc::make_mut(state);
            state.panels = panels;
            state.side_chats.remove(&id);
            if state.file_line.is_some_and(|(file, _)| file == id) {
                state.file_line = None;
            }
            true
        });
        if !closed {
            return;
        }
        self.client.close_pane(id);
        if terminal {
            self.client
                .dispatch(&scope.host, None, Intent::CloseTerminal { handle });
        }
    }

    pub fn terminal_handle(&self, id: u64) -> String {
        let state = self.snapshot();
        state
            .panels
            .scope(id)
            .and_then(|scope| {
                state
                    .panels
                    .get(scope)
                    .surfaces
                    .into_iter()
                    .find(|surface| surface.id == id)
            })
            .and_then(|surface| surface.terminal_handle)
            .unwrap_or_else(|| format!("agent-terminal-{}-{id}", self.instance))
    }

    pub async fn shutdown(&self) -> Result<(), AgentError> {
        self.stop.cancel();
        self.jobs.close();
        self.jobs.wait().await;
        // Producer jobs have stopped; include changes accepted during a save.
        let saved = match &self.selection_path {
            Some(path) => {
                connections::save_selection(path.clone(), self.snapshot().selection()).await
            }
            None => Ok(()),
        };
        let closed = self.client.shutdown().await;
        closed.and(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::state::Snapshot;
    use agent_protocol::session::{ProviderKind, SessionRef};

    #[test]
    fn hiding_a_side_chat_keeps_its_resource_and_late_updates_cannot_reopen_a_closed_tab() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, application) = Application::new(directory.path().into(), None).unwrap();
        let scope =
            Snapshot::default().panel_scope("missing-host".into(), DraftKey::from("parent"));
        application.open_side_chat(
            scope.clone(),
            "/side".into(),
            Some(ProviderKind::Codex),
            SideChatStart::Quote("quote".into()),
        );
        let id = application
            .snapshot()
            .panels
            .get(scope.clone())
            .active
            .unwrap();
        application.set_panel_open(scope.clone(), false);
        assert!(application.snapshot().side_chats.contains_key(&id));
        let mut wrong_scope = scope.clone();
        wrong_scope.host = "another-host".into();
        application.close_panel(wrong_scope, id);
        assert!(application.snapshot().side_chats.contains_key(&id));
        application.close_panel(scope.clone(), id);
        application.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.receive(Update::PanePresentation {
                id,
                value: Presentation::empty(agent_core::localization::UiLanguage::default()),
            });
            state.receive(Update::SideChatReady {
                id,
                result: Ok(Presentation::empty(
                    agent_core::localization::UiLanguage::default(),
                )),
            });
        });
        assert!(!application.snapshot().side_chats.contains_key(&id));
        assert!(application.snapshot().panels.get(scope).surfaces.is_empty());
        runtime.block_on(application.shutdown()).unwrap();
    }

    #[test]
    fn starting_a_new_chat_keeps_the_open_panel_and_the_sidebar() {
        let mut snapshot = Snapshot::default();
        snapshot.connection.storage_scope = "storage".into();
        let mut state = State::default();
        let host = "host".to_owned();
        let before = snapshot.panel_scope(
            host.clone(),
            snapshot.panes.primary().navigation.draft_key.clone(),
        );
        state.receive(Update::Presentation {
            host: host.clone(),
            value: Presentation::project(Arc::new(snapshot.clone()), "", None).unwrap(),
        });
        let opened = state.panels.clone().open(
            before.clone(),
            PanelKind::Terminal,
            Some("/original".into()),
        );
        state.panels = opened.panels;
        snapshot = agent_core::state::reduce(
            &snapshot,
            "",
            agent_core::state::Event::Intent(Intent::NewChat { cwd: "/new".into() }),
        )
        .0;
        let after = snapshot.panel_scope(
            host.clone(),
            snapshot.panes.primary().navigation.draft_key.clone(),
        );
        state.receive(Update::Presentation {
            host,
            value: Presentation::project(Arc::new(snapshot), "", None).unwrap(),
        });
        assert!(state.panels.get(after.clone()).open);
        assert!(state.panels.get(after).surfaces.is_empty());
        assert_eq!(state.panels.get(before).active, Some(opened.id));
        assert_eq!(
            state.sidebar.presentation(1200.),
            agent_core::presentation::sidebar::SidebarPresentation::Beside
        );
    }

    #[test]
    fn submitting_a_draft_keeps_its_panel_scope_and_tabs() {
        for navigate_to_new_chat in [false, true] {
            let mut snapshot = Snapshot::default();
            snapshot.connection.storage_scope = "storage".into();
            let session =
                SessionRef::new(None, "00000000-0000-0000-0000-000000000001".into()).unwrap();
            Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).draft_key =
                DraftKey::from(&session);
            let initial = Presentation::project(Arc::new(snapshot.clone()), "", None).unwrap();
            let draft = initial.context.navigation().draft_key;
            let before = snapshot.panel_scope("host".into(), draft);
            let after = before.clone();
            let mut state = State::default();
            state.receive(Update::Presentation {
                host: "host".into(),
                value: initial,
            });
            let opened = state.panels.clone().open(
                before.clone(),
                PanelKind::Terminal,
                Some("/project".into()),
            );
            state.panels = opened.panels;
            let retained = state.clone();
            Arc::make_mut(&mut snapshot.sessions.conversations).insert(
                session.clone(),
                Arc::new(agent_protocol::models::Thread {
                    id: Some(session.clone()),
                    active_provider: Some(ProviderKind::Codex),
                    ..Default::default()
                }),
            );
            Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).thread_id =
                (!navigate_to_new_chat).then(|| session.clone());
            let selected = if navigate_to_new_chat {
                DraftKey::from("next-draft")
            } else {
                DraftKey::Session { session }
            };
            Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).draft_key =
                selected.clone();
            state.receive(Update::Presentation {
                host: "host".into(),
                value: Presentation::project(Arc::new(snapshot), "", None).unwrap(),
            });
            assert_eq!(state.panels.get(after.clone()).active, Some(opened.id));
            assert_eq!(
                state.publications["host"].pane().navigation.draft_key,
                selected
            );
            if navigate_to_new_chat {
                let panel = state.panels.get(
                    state.publications["host"]
                        .snapshot
                        .panel_scope("host".into(), selected),
                );
                assert!(panel.open);
                assert!(panel.surfaces.is_empty());
            }
            assert_eq!(retained.panels.get(before).active, Some(opened.id));
        }
    }

    #[test]
    fn finishing_one_batch_keeps_other_hosts_and_conversations_uploading() {
        let mut state = State::default();
        let a = DraftKey::from("a");
        let b = DraftKey::from("b");
        let first = uuid::Uuid::new_v4();
        let second = uuid::Uuid::new_v4();
        state.uploads.insert(first, ("host".into(), a.clone()));
        state.uploads.insert(second, ("host".into(), a.clone()));
        state
            .uploads
            .insert(uuid::Uuid::new_v4(), ("other".into(), b.clone()));
        state.receive(Update::AttachmentsFinished(first));
        assert!(state.uploading("host", &a));
        assert!(!state.uploading("host", &b));
        assert!(state.uploading("other", &b));
        state.receive(Update::AttachmentsFinished(second));
        assert!(!state.uploading("host", &a));
        assert!(state.uploading("other", &b));
    }

    #[test]
    fn native_views_observe_the_same_tabs_size_navigation_and_file_target() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, application) = Application::new(directory.path().into(), None).unwrap();
        let mut sidebar = application.subscribe();
        let mut tools = application.subscribe();
        let scope = Snapshot::default().panel_scope("host".into(), DraftKey::from("draft"));
        application.open_panel(
            scope.clone(),
            PanelKind::File,
            Some("/a.rs".into()),
            Some(7),
        );
        let id = application
            .snapshot()
            .panels
            .get(scope.clone())
            .active
            .unwrap();
        application.set_panel_maximized(scope.clone(), true);
        application.navigate(Navigation::Settings(true));
        application.navigate(Navigation::Provider(Some(ProviderKind::Codex)));
        let first = sidebar.borrow_and_update().clone();
        let second = tools.borrow_and_update().clone();
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.file_line, Some((id, Some(7))));
        assert!(first.panels.get(scope.clone()).maximized);
        assert!(first.settings);
        assert_eq!(first.provider, Some(ProviderKind::Codex));
        application.close_panel(scope.clone(), id);
        application.navigate(Navigation::Settings(false));
        let closed = application.snapshot();
        assert!(closed.panels.get(scope).surfaces.is_empty());
        assert!(closed.file_line.is_none());
        assert!(!closed.settings);
        assert!(closed.provider.is_none());
        assert_eq!(
            first.file_line,
            Some((id, Some(7))),
            "mounted views retain immutable snapshots"
        );
        runtime.block_on(application.shutdown()).unwrap();
    }

    #[test]
    fn a_selected_language_is_published_and_restored_on_the_next_launch() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, application) = Application::new(directory.path().into(), None).unwrap();
        assert_eq!(application.snapshot().language, UiLanguage::Japanese);
        application.select_language(UiLanguage::English);
        assert_eq!(application.snapshot().language, UiLanguage::English);
        assert_eq!(crate::l10n::tr("設定"), "Settings");
        runtime.block_on(application.shutdown()).unwrap();
        let (runtime, restored) = Application::new(directory.path().into(), None).unwrap();
        assert_eq!(restored.snapshot().language, UiLanguage::English);
        restored.select_language(UiLanguage::Japanese);
        assert_eq!(crate::l10n::tr("設定"), "設定");
        runtime.block_on(restored.shutdown()).unwrap();
    }

    #[test]
    fn sidebar_projects_survive_relaunch_and_a_failed_save_keeps_the_current_choice() {
        let directory = tempfile::tempdir().unwrap();
        let selected = vec![agent_core::presentation::sidebar::ProjectKey {
            host_id: "host".into(),
            project_id: "repo".into(),
        }];
        let (runtime, application) = Application::new(directory.path().into(), None).unwrap();
        application.select_sidebar_projects(selected.clone());
        assert_eq!(application.snapshot().sidebar_projects, selected);
        runtime.block_on(application.shutdown()).unwrap();
        let (runtime, restored) = Application::new(directory.path().into(), None).unwrap();
        assert_eq!(restored.snapshot().sidebar_projects, selected);
        std::fs::remove_file(&restored.sidebar_projects_path).unwrap();
        std::fs::create_dir(&restored.sidebar_projects_path).unwrap();
        restored.select_sidebar_projects(Vec::new());
        assert!(restored.snapshot().sidebar_projects.is_empty());
        assert!(restored.snapshot().failure.is_some());
        runtime.block_on(restored.shutdown()).unwrap();
    }

    #[test]
    fn sidebar_scope_survives_sheet_and_language_remounts_and_reveals_only_new_outside_routes() {
        use agent_core::{
            presentation::sidebar::ProjectKey,
            state::{Event, reduce},
        };
        let directory = tempfile::tempdir().unwrap();
        let (runtime, application) = Application::new(directory.path().into(), None).unwrap();
        // Exercise publications deterministically without a running Host/receiver.
        runtime.block_on(application.shutdown()).unwrap();
        application.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.host = "host".into();
            state.hosts = vec![Host {
                id: "host".into(),
                name: "Host".into(),
            }];
        });
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        snapshot.lists.threads = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
            "projects": [{"id":"repo", "name":"Repo", "roots":[{"path":"/repo"}]},
                         {"id":"other", "name":"Other", "roots":[{"path":"/other"}]}],
                "data": [], "limit":60, "hasMore":false, "hasMoreProjects":false, "projectPages":{}
            }))
            .unwrap(),
        ));
        snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/repo".into(),
            }),
        )
        .0;
        let publish = |snapshot: &Snapshot| {
            application.receive(Update::Presentation {
                host: "host".into(),
                value: Presentation::project(Arc::new(snapshot.clone()), "", None).unwrap(),
            })
        };
        publish(&snapshot);
        let chosen = vec![ProjectKey {
            host_id: "host".into(),
            project_id: "other".into(),
        }];
        application.select_sidebar_projects(chosen.clone());
        // Opening/closing the narrow sheet and rebuilding localized UI do not navigate.
        for _ in 0..4 {
            application.navigate(Navigation::ToggleSidebar(600.0));
            publish(&snapshot);
            assert_eq!(application.snapshot().sidebar_projects, chosen);
        }
        for language in [UiLanguage::English, UiLanguage::Japanese] {
            application.select_language(language);
            publish(&snapshot);
            assert_eq!(application.snapshot().sidebar_projects, chosen);
        }
        let in_project = vec![ProjectKey {
            host_id: "host".into(),
            project_id: "repo".into(),
        }];
        application.select_sidebar_projects(in_project.clone());
        snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/repo".into(),
            }),
        )
        .0;
        publish(&snapshot);
        assert_eq!(application.snapshot().sidebar_projects, in_project);
        application.select_sidebar_projects(chosen.clone());
        let mut search = snapshot.clone();
        Arc::make_mut(&mut search.lists.list_query).search_term = "no matches".into();
        Arc::make_mut(search.lists.threads.as_mut().unwrap())
            .projects
            .clear();
        publish(&search);
        assert_eq!(application.snapshot().sidebar_projects, chosen);
        Arc::make_mut(&mut search.lists.list_query)
            .search_term
            .clear();
        Arc::make_mut(&mut search.operations).insert(
            agent_core::state::operations::OperationKey::SessionList,
            agent_core::state::operations::OperationState {
                generation: 1,
                phase: agent_core::state::operations::OperationPhase::Running,
            },
        );
        publish(&search);
        assert_eq!(application.snapshot().sidebar_projects, chosen);
        publish(&snapshot);
        assert_eq!(application.snapshot().sidebar_projects, chosen);
        // A hidden choice makes the effective scope All. Navigating while it
        // is hidden must retain the saved choice for a later restore.
        application.select_sidebar_projects(in_project.clone());
        snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::SetProjectHidden {
                project_id: "repo".into(),
                hidden: true,
            }),
        )
        .0;
        publish(&snapshot);
        snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/other".into(),
            }),
        )
        .0;
        publish(&snapshot);
        assert_eq!(application.snapshot().sidebar_projects, in_project);
        let saved: Vec<ProjectKey> =
            serde_json::from_slice(&std::fs::read(&application.sidebar_projects_path).unwrap())
                .unwrap();
        assert_eq!(saved, in_project);
        snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::SetProjectHidden {
                project_id: "repo".into(),
                hidden: false,
            }),
        )
        .0;
        publish(&snapshot);
        assert_eq!(application.snapshot().sidebar_projects, in_project);
        let inputs = application.snapshot().list_inputs();
        let catalogs = inputs
            .iter()
            .map(|(host, list)| (host.as_str(), &**list))
            .collect::<Vec<_>>();
        assert_eq!(
            agent_core::presentation::sidebar::resolve_projects(&in_project, &catalogs, true),
            in_project
        );
        // A complete catalog that loses the project also has no active scope.
        snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/repo".into(),
            }),
        )
        .0;
        publish(&snapshot);
        let projects = snapshot.lists.threads.as_ref().unwrap().projects.clone();
        Arc::make_mut(snapshot.lists.threads.as_mut().unwrap())
            .projects
            .retain(|project| project.id != "repo");
        snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/other".into(),
            }),
        )
        .0;
        publish(&snapshot);
        assert_eq!(application.snapshot().sidebar_projects, in_project);
        let saved: Vec<ProjectKey> =
            serde_json::from_slice(&std::fs::read(&application.sidebar_projects_path).unwrap())
                .unwrap();
        assert_eq!(saved, in_project);
        Arc::make_mut(snapshot.lists.threads.as_mut().unwrap()).projects = projects;
        publish(&snapshot);
        assert_eq!(application.snapshot().sidebar_projects, in_project);
        snapshot = reduce(
            &snapshot,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/outside".into(),
            }),
        )
        .0;
        publish(&snapshot);
        assert!(application.snapshot().sidebar_projects.is_empty());
        let saved: Vec<ProjectKey> =
            serde_json::from_slice(&std::fs::read(&application.sidebar_projects_path).unwrap())
                .unwrap();
        assert!(
            saved.is_empty(),
            "revealing an outside route also clears persisted scope"
        );
    }

    #[test]
    fn sidebar_switching_hosts_ignores_queued_navigation_until_that_hosts_route_changes() {
        use agent_core::{
            presentation::sidebar::ProjectKey,
            state::{Event, operations as op, reduce},
        };
        let directory = tempfile::tempdir().unwrap();
        let (runtime, application) = Application::new(directory.path().into(), None).unwrap();
        runtime.block_on(application.shutdown()).unwrap();
        application.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.host = "a".into();
            state.all_hosts = true;
            state.hosts = vec![
                Host {
                    id: "a".into(),
                    name: "A".into(),
                },
                Host {
                    id: "b".into(),
                    name: "B".into(),
                },
            ];
        });
        let mut base = Snapshot::default();
        base.connection.connected = true;
        base.lists.threads = Some(Arc::new(serde_json::from_value(serde_json::json!({
            "projects": [{"id":"repo", "name":"Repo", "roots":[{"path":"/repo"}]},
                         {"id":"other", "name":"Other", "roots":[{"path":"/other"}]}],
            "data": [{"id":{"provider":"codex", "id":"repo-chat"}, "projectId":"repo", "cwd":"/repo"},
                     {"id":{"provider":"codex", "id":"old-chat"}, "projectId":"other", "cwd":"/other"}],
            "limit":60, "hasMore":false, "hasMoreProjects":false, "projectPages":{}
        })).unwrap()));
        let target = SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "repo-chat".into(),
        };
        let old = SessionRef {
            provider: Some(ProviderKind::Codex),
            id: "old-chat".into(),
        };
        let a = reduce(
            &base,
            "",
            Event::Intent(Intent::ReadThread(op::ReadThread::open(target.clone()))),
        )
        .0;
        let b = reduce(
            &base,
            "",
            Event::Intent(Intent::ReadThread(op::ReadThread::open(old))),
        )
        .0;
        let publish = |host: &str, snapshot: &Snapshot| {
            application.receive(Update::Presentation {
                host: host.into(),
                value: Presentation::project(Arc::new(snapshot.clone()), "", None).unwrap(),
            })
        };
        publish("a", &a);
        // An inactive Host is observed even while search hides its membership.
        let mut searching = b.clone();
        Arc::make_mut(&mut searching.lists.list_query).search_term = "repo".into();
        publish("b", &searching);
        let selected = vec![ProjectKey {
            host_id: "b".into(),
            project_id: "repo".into(),
        }];
        application.select_sidebar_projects(selected.clone());
        // A row selects its destination Host before ReadThread's publication.
        application
            .state
            .send_modify(|state| Arc::make_mut(state).host = "b".into());
        publish("b", &b);
        assert_eq!(
            application.snapshot().sidebar_projects,
            selected,
            "the queued old conversation must not widen scope"
        );
        let opened = reduce(
            &b,
            "",
            Event::Intent(Intent::ReadThread(op::ReadThread::open(target.clone()))),
        )
        .0;
        assert_eq!(
            opened.panes.primary().navigation.thread_id.as_ref(),
            Some(&target)
        );
        publish("b", &opened);
        assert_eq!(application.snapshot().sidebar_projects, selected);
        assert!(application.snapshot().all_hosts);
        let saved: Vec<ProjectKey> =
            serde_json::from_slice(&std::fs::read(&application.sidebar_projects_path).unwrap())
                .unwrap();
        assert_eq!(saved, selected);
    }

    #[test]
    fn the_file_list_width_is_restored_on_the_next_launch_and_a_broken_one_is_the_default() {
        use agent_core::presentation::files::EXPLORER_DEFAULT_WIDTH;
        let directory = tempfile::tempdir().unwrap();
        let (runtime, application) = Application::new(directory.path().into(), None).unwrap();
        assert_eq!(
            application.snapshot().explorer_width,
            EXPLORER_DEFAULT_WIDTH
        );
        application.set_explorer_width(336.0);
        assert_eq!(application.snapshot().explorer_width, 336.0);
        runtime.block_on(application.shutdown()).unwrap();
        let (runtime, restored) = Application::new(directory.path().into(), None).unwrap();
        assert_eq!(restored.snapshot().explorer_width, 336.0);
        runtime.block_on(restored.shutdown()).unwrap();
        std::fs::write(directory.path().join("file-explorer-width.json"), b"broken").unwrap();
        let (runtime, broken) = Application::new(directory.path().into(), None).unwrap();
        assert_eq!(broken.snapshot().explorer_width, EXPLORER_DEFAULT_WIDTH);
        runtime.block_on(broken.shutdown()).unwrap();
    }

    #[test]
    fn pairing_failure_keeps_navigation_and_success_closes_setup_atomically() {
        let mut state = State {
            host: "existing".into(),
            settings: true,
            provider: Some(ProviderKind::Codex),
            pairing: true,
            ..Default::default()
        };
        state.receive(Update::Paired(Err("unavailable".into())));
        assert!(state.settings);
        assert_eq!(state.host, "existing");
        assert!(!state.pairing);
        state.receive(Update::Paired(Ok("paired".into())));
        assert_eq!(state.host, "paired");
        assert!(!state.pairing);
        assert!(!state.settings);
        assert!(state.provider.is_none());
    }

    #[test]
    fn host_updates_preserve_the_selection_until_that_host_is_removed() {
        let host = |id: &str| crate::storage::Host {
            id: id.into(),
            name: id.into(),
            ticket: "offline".into(),
            local_directory: None,
        };
        let mut state = State {
            host: "b".into(),
            ..Default::default()
        };
        state.receive(Update::Hosts(vec![host("a"), host("b")]));
        assert_eq!(state.host, "b");
        state.receive(Update::Hosts(vec![host("a")]));
        assert_eq!(state.host, "a");
        state.receive(Update::Hosts(Vec::new()));
        assert!(state.host.is_empty());
    }

    #[test]
    fn an_account_request_for_an_old_host_is_ignored_and_failure_unlocks_navigation() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, application) = Application::new(directory.path().into(), None).unwrap();
        let mut updates = application.subscribe();
        application.account_request(
            "old-host".into(),
            Intent::ListAccounts(agent_core::state::operations::ListAccounts {}),
        );
        assert!(!application.snapshot().account_pending);
        application.account_request(
            String::new(),
            Intent::ListAccounts(agent_core::state::operations::ListAccounts {}),
        );
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let state = updates.borrow_and_update().clone();
                    if state.failure.is_some() && !state.account_locked() {
                        break;
                    }
                    updates.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            application.shutdown().await.unwrap();
        });
    }

    #[test]
    fn pending_requests_login_prompts_and_the_selected_providers_login_each_lock_navigation() {
        use agent_core::state::operations::{OperationKey, OperationPhase, OperationState};
        let mut state = State {
            host: "host".into(),
            ..Default::default()
        };
        assert!(!state.account_locked());
        state.account_pending = true;
        assert!(
            state.account_locked(),
            "the request is locked before core publishes"
        );
        state.account_pending = false;
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.catalog.account).login =
            Some(Arc::new(agent_protocol::operations::AccountLogin {
                provider: ProviderKind::Codex,
                login_id: "fixture-login".into(),
                requires_code_submission: false,
                user_code: String::new(),
                verification_url: String::new(),
            }));
        state.receive(Update::Presentation {
            host: "host".into(),
            value: Presentation::project(Arc::new(snapshot.clone()), "", None).unwrap(),
        });
        assert!(
            state.account_locked(),
            "an active login keeps navigation locked between polls"
        );
        Arc::make_mut(&mut snapshot.catalog.account).login = None;
        let key = OperationKey::AccountLogin {
            provider: ProviderKind::Codex,
        };
        Arc::make_mut(&mut snapshot.operations).insert(
            key.clone(),
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        state.receive(Update::Presentation {
            host: "host".into(),
            value: Presentation::project(Arc::new(snapshot.clone()), "", None).unwrap(),
        });
        state.provider = Some(ProviderKind::Claude);
        assert!(!state.account_locked());
        state.provider = Some(ProviderKind::Codex);
        assert!(state.account_locked());
        Arc::make_mut(&mut snapshot.operations).insert(
            key,
            OperationState {
                generation: 1,
                phase: OperationPhase::Failed {
                    message: "cancelled".into(),
                },
            },
        );
        state.receive(Update::Presentation {
            host: "host".into(),
            value: Presentation::project(Arc::new(snapshot), "", None).unwrap(),
        });
        assert!(
            !state.account_locked(),
            "failed login operations permit recovery and navigation"
        );
    }
}

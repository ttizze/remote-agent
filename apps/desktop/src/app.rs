use agent_core::state::operations as op;
mod clipboard;
mod completions;
mod dictation;
mod files;
mod forms;
mod hosts;
mod selection;
mod timeline;
mod view;

use crate::{Runtime, appearance, diff::DiffView, platform, store_session::StoreSession};
use agent_core::{
    client::persistence::{Persistence, RestoreError},
    panels::{PanelKind as Panel, PanelLayout, PanelScope, Panels},
    presentation::{
        conversation::{ActivityExpansion, ConversationRowContent},
        error::ErrorScope,
    },
    state::{Attachment, Draft, DraftKey, Intent, Snapshot},
    store::Outcome,
};
use agent_protocol::{
    ids::{ItemId, RequestId, TurnId},
    models::{Item, Model, RemoteHost, Thread, Turn},
    requests::Answer,
    session::SessionRef,
};
use dictation::{Dictation, Phase};
use gpui_kit::{
    component::{
        button::{Button, ButtonVariants},
        input::{Editor, Input, InputEvent, InputState, Textarea, TextareaState},
        menu::{DropdownMenu, PopupMenuItem},
        text::TextView,
        *,
    },
    prelude::FluentBuilder,
    *,
};
use hosts::{ConnectionLayout, HostEvent, Hosts};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Weak},
};
use timeline::{
    ConversationRow, ImageGallery, ImageState, MarkdownContent, Media, RequestInputs, Timeline,
};
use view::icon_button;

struct ProjectEditor {
    id: String,
    scope: String,
    name: Entity<InputState>,
    section: Entity<InputState>,
}

enum OperationCompletion {
    ProjectImport,
    Busy,
    Composer(u64),
    Answer(u64),
    Editor(u64),
    Item { generation: u64, item_id: ItemId },
    WorktreeSettings,
    Request(RequestId),
    Dictation(uuid::Uuid),
    RemoveWorktree,
    Account,
    Gallery(uuid::Uuid),
    FileReference(Box<files::ReferenceRequest>),
}
enum Update {
    InitialHost(Result<Option<RemoteHost>, String>),
    Connected(Result<(StoreSession, Arc<Persistence>, bool), String>),
    Snapshot,
    Completed(OperationCompletion, Result<Outcome, String>),
    Folder(Result<Option<platform::ProjectSelection>, String>),
    Image {
        key: String,
        result: Result<String, String>,
    },
    Recording(uuid::Uuid, platform::RecordingEvent),
    PersistenceError(String),
    LocalCompleted {
        scope: ErrorScope,
        result: Result<(), String>,
    },
    FilePreview {
        generation: u64,
        scope: ErrorScope,
        result: Result<files::DownloadedPreview, String>,
    },
    OnboardingCompleted(Result<(), String>),
}
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Chat,
    Settings,
}
#[derive(Clone, Copy, PartialEq)]
enum SettingsPage {
    General,
    Agents,
    Connections,
    Worktrees,
}
enum WorktreeToggle {
    Create(bool),
    Copy(bool),
    DeleteUnchanged(bool),
    /// `None` returns the project to the rule shared by all projects.
    ProjectDeleteUnchanged(String, Option<bool>),
}
#[derive(Clone, Copy, PartialEq)]
enum ModelPanel {
    Models,
    Accounts,
}
enum SurfaceResource {
    Diff {
        view: Entity<DiffView>,
        source: Option<Weak<agent_protocol::models::WorkspaceReview>>,
    },
    Pending {
        url: Option<String>,
    },
    Browser(Entity<crate::browser::Browser>),
    Terminal(Entity<crate::terminal::Terminal>),
    SideChat(Entity<Desktop>),
}
pub(crate) enum Mode {
    Main,
    SideChat {
        remote: Option<RemoteHost>,
        cwd: String,
    },
}
/// Business state is owned by Store. Everything else here is a widget, render
/// cache, pending UI effect, or immutable snapshot retained for display.
pub(crate) struct Desktop {
    session: Option<StoreSession>,
    snapshot: Arc<Snapshot>,
    runtime: Runtime,
    updates: async_channel::Sender<(u64, Update)>,
    snapshot_pending: Option<u64>,
    epoch: u64,
    connecting: bool,
    local_shell: bool,
    remote: Option<RemoteHost>,
    hosts: Option<Entity<Hosts>>,
    app_updates: Option<Entity<crate::updates::Updates>>,
    side_chat_mode: bool,
    visible: bool,
    onboarding: bool,
    import_projects: bool,
    initial_cwd: Option<String>,
    pending_project: Option<(Option<String>, String)>,
    busy: usize,
    local_errors: BTreeMap<ErrorScope, String>,
    composer: forms::ComposerInput,
    question: forms::QuestionInput,
    selection: Entity<selection::ConversationSelection>,
    pending_quote: Option<String>,
    pending_explanation: Option<String>,
    editor: forms::FileEditor,
    project_editor: Option<ProjectEditor>,
    search: Entity<InputState>,
    file_search: Entity<InputState>,
    file_browser: Option<files::FileBrowser>,
    file_explorer: files::FileExplorer,
    file_open_generation: u64,
    model_controls: forms::ModelControls,
    account: forms::AccountForm,
    worktrees: forms::WorktreeForm,
    tab: Tab,
    settings_page: SettingsPage,
    sidebar: bool,
    /// Whether thread details stay docked beside the chat when they fit.
    details: bool,
    /// Whether thread details are open as a popover while they cannot dock.
    details_popover: bool,
    panels: Panels,
    surface_resources: HashMap<u64, SurfaceResource>,
    panel_focus: FocusHandle,
    dictation: Option<Dictation>,
    review_expanded: bool,
    source_paths: Vec<String>,
    source_items: Vec<Arc<Item>>,
    conversation: Entity<view::ConversationView>,
    composer_view: Entity<view::ComposerView>,
    workbench_view: Entity<view::WorkbenchView>,
    header_view: Entity<view::HeaderView>,
    media: Media,
    /// Absent in side chats, which have no sidebar.
    sidebar_view: Option<Entity<view::SidebarView>>,
    _subscriptions: Vec<Subscription>,
}
// Input text/catalog and terminal updates have their own owners. Other changes still
// reach Desktop observers (including open menus) without comparing conversation,
// file, or diff contents on each keystroke. Exhaustive matching keeps new fields
// from being silently omitted from this routing decision.
fn general_snapshot_changed(before: &Snapshot, after: &Snapshot) -> bool {
    macro_rules! changed {
        (shared: [$($shared:ident),*], optional: [$($optional:ident),*], values: [$($value:ident),*]) => {{
            let Snapshot {
                $($shared: _,)* $($optional: _,)* $($value: _,)*
                drafts: _, file_drafts: _, composer_catalog: _, unconfirmed_lists: _, terminals: _,
            } = after;
            false
                $(|| !Arc::ptr_eq(&before.$shared, &after.$shared))*
                $(|| before.$optional.as_ref().map(Arc::as_ptr) != after.$optional.as_ref().map(Arc::as_ptr))*
                $(|| before.$value != after.$value)*
        }};
    }
    let changed = changed!(
        shared: [project_import, project_preferences, operations,
            archived_scopes, account, conversations, expanded_projects,
            project_threads, models, model_errors, question_drafts, draft_sessions, pending_submissions,
            workspace, workspaces, navigation, activity, management, list_query, subscriptions],
        optional: [permission_settings, task_activity, threads, file_open_capabilities],
        values: [recent_chats, operation_sequence, model_preferences, preferred_editor, file_open_capabilities_requested, host_name, host_ticket, host_home_directory, storage_scope,
            observed_agents, epoch, connected, connection_generation, connection_failure, error]
    );
    let empty = Draft::default();
    macro_rules! controls {
        ($draft:expr) => {{
            let Draft {
                text: _,
                attachments,
                invocations,
                model,
                effort,
                service_tier,
                model_settings_changed,
            } = $draft;
            (
                attachments,
                invocations,
                model,
                effort,
                service_tier,
                model_settings_changed,
            )
        }};
    }
    let before = before
        .drafts
        .get(&before.navigation.draft_key)
        .map_or(&empty, Arc::as_ref);
    let after = after
        .drafts
        .get(&after.navigation.draft_key)
        .map_or(&empty, Arc::as_ref);
    changed || controls!(before) != controls!(after)
}

impl Desktop {
    fn set_error(&mut self, error: String) {
        tracing::error!(target: "agent", operation = "desktop", message = %error);
        self.local_errors.insert(self.local_error_scope(), error);
    }
    pub(crate) fn new(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let language = crate::localization::language(cx);
        StoreSession::on_app_quit(cx, |view| &mut view.session);
        cx.on_release(|view, _| view.close_file_browser()).detach();
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
                    .update(cx, |view, _| {
                        if view.account.polling
                            && !view.account.busy
                            && view.snapshot.connected
                            && let Some(login) = &view.snapshot.account.login
                        {
                            let id = login.login_id.clone();
                            let provider = login.provider;
                            view.account.busy = true;
                            view.perform(
                                Intent::ReadAccountLogin(op::ReadAccountLogin {
                                    provider,
                                    id,
                                    thread_id: view.account.login_draft.clone(),
                                }),
                                OperationCompletion::Account,
                            );
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let composer = forms::ComposerInput::new(window, cx);
        let question = forms::QuestionInput::new(window, cx);
        let editor = forms::FileEditor::new(window, cx);
        let model_controls = forms::ModelControls::new(window, cx);
        let account = forms::AccountForm::new(window, cx);
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(language.text("会話を検索")));
        let file_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(language.text("ファイルを検索")));
        let worktrees = forms::WorktreeForm::new(window, cx);
        let hosts = (!side_chat_mode).then(|| cx.new(|cx| Hosts::new(window, cx)));
        let app_updates = (!side_chat_mode
            && cfg!(any(target_os = "macos", target_os = "windows")))
        .then(|| cx.new(crate::updates::Updates::new));
        let selection = cx.new(|_| selection::ConversationSelection::new(!side_chat_mode));
        let owner = cx.entity();
        let conversation =
            cx.new(|cx| view::ConversationView::new(&owner, selection.clone(), window, cx));
        let composer_view =
            cx.new(|cx| view::ComposerView::new(&owner, composer.input.clone(), cx));
        let file_explorer = files::FileExplorer::new(cx);
        let workbench_view =
            cx.new(|cx| view::WorkbenchView::new(&owner, &file_explorer.state, cx));
        let header_view = cx.new(|cx| view::HeaderView::new(&owner, cx));
        let mut subscriptions = vec![
            cx.observe_global_in::<crate::localization::Language>(window, |view, window, cx| {
                let language = crate::localization::language(cx);
                for (input, source) in [
                    (&view.search, language.text("会話を検索")),
                    (&view.file_search, language.text("ファイルを検索")),
                    (&view.model_controls.search, language.text("モデルを検索")),
                    (
                        &view.account.code,
                        language.text("認証コード・APIキーを貼り付け"),
                    ),
                    (
                        &view.worktrees.directory,
                        language.text("ホスト上の絶対パス（空欄で既定）"),
                    ),
                ] {
                    input.update(cx, |input, cx| {
                        input.set_placeholder(language.text(source), window, cx)
                    });
                }
                view.composer.input.update(cx, |input, cx| {
                    input.set_placeholder(language.text("AI に依頼する"), window, cx)
                });
                if let Some(prompt) = view.question_prompt() {
                    view.question
                        .set_placeholder(language.text(&prompt.placeholder), window, cx);
                }
                cx.notify();
            }),
            cx.observe_in(&owner, window, |view, _, window, cx| {
                view.sync_surfaces(window, cx)
            }),
            cx.observe_window_bounds(window, |view, window, cx| view.sync_surfaces(window, cx)),
            cx.subscribe_in(&conversation, window, |view, _, action, window, cx| {
                view.conversation_action(action, window, cx);
                cx.notify();
            }),
            cx.subscribe_in(
                &selection,
                window,
                |view, _, action, window, cx| match action {
                    selection::SelectionAction::AddToChat(text) => {
                        view.quote_selection(text, window, cx)
                    }
                    selection::SelectionAction::AskSideChat(text) => {
                        view.open_panel(Panel::SideChat, window, cx);
                        if let Some(SurfaceResource::SideChat(chat)) = view
                            .panels
                            .get(view.panel_scope())
                            .active
                            .and_then(|id| view.surface_resources.get(&id))
                        {
                            chat.update(cx, |chat, cx| chat.quote_selection(text, window, cx));
                        }
                    }
                    selection::SelectionAction::Explain(text) => {
                        if view.side_chat_mode {
                            view.explain_selection(text, cx);
                        } else {
                            view.open_panel(Panel::SideChat, window, cx);
                            if let Some(SurfaceResource::SideChat(chat)) = view
                                .panels
                                .get(view.panel_scope())
                                .active
                                .and_then(|id| view.surface_resources.get(&id))
                            {
                                chat.update(cx, |chat, cx| chat.explain_selection(text, cx));
                            }
                        }
                    }
                },
            ),
            cx.subscribe(&composer.input, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    if let Some(revision) = view.composer.typed(value.clone()) {
                        let cursor = input.read(cx).cursor();
                        view.perform(
                            Intent::EditComposer {
                                thread_id: view.draft_key().clone(),
                                text: value.to_string(),
                                cursor: cursor as u32,
                            },
                            OperationCompletion::Composer(revision),
                        );
                    }
                    view.sync_composer(cx);
                }
            }),
            cx.subscribe(&file_search, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    view.file_tree_action(agent_core::presentation::files::FileTreeAction::Query {
                        text: input.read(cx).value().to_string(),
                    });
                    cx.notify();
                }
            }),
            cx.subscribe(&question.input, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    view.answer_typed(input.read(cx).value());
                }
            }),
            cx.subscribe(&question.secret, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    view.answer_typed(input.read(cx).value());
                }
            }),
            cx.subscribe(&editor.input, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    if let Some((path, revision)) = view.editor.typed(value.clone()) {
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
            cx.subscribe(&model_controls.search, |_, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
            cx.subscribe(&account.code, |_, _, event, cx| {
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
            cx.subscribe(&worktrees.copy_paths, |view, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    view.worktree_input_changed(cx);
                    cx.notify();
                }
                if matches!(event, InputEvent::Blur) {
                    view.save_worktree_settings(None, cx);
                }
            }),
            cx.subscribe(&worktrees.directory, |view, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    view.worktree_input_changed(cx);
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
                        HostEvent::Failed(error) => view.set_error(error.clone()),
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
        if let Some(updates) = &app_updates {
            subscriptions.push(
                cx.subscribe(updates, |view, _, _: &crate::updates::RefreshThreads, _| {
                    view.refresh_threads()
                }),
            );
        }
        let mut view = Self {
            session: None,
            snapshot: Arc::default(),
            runtime: cx.global::<Runtime>().clone(),
            updates,
            snapshot_pending: None,
            epoch: 0,
            connecting: false,
            local_shell: false,
            pending_project: None,
            remote,
            hosts,
            app_updates,
            side_chat_mode,
            visible: true,
            onboarding: !side_chat_mode
                && !platform::state_dir()
                    .is_ok_and(|directory| directory.join("onboarding.completed").is_file()),
            import_projects: false,
            initial_cwd,
            busy: 0,
            local_errors: BTreeMap::new(),
            composer,
            question,
            selection,
            pending_quote: None,
            pending_explanation: None,
            editor,
            project_editor: None,
            search,
            file_search,
            file_browser: None,
            file_open_generation: 0,
            file_explorer,
            model_controls,
            account,
            worktrees,
            tab: Tab::Chat,
            settings_page: SettingsPage::General,
            sidebar: true,
            details: true,
            details_popover: false,
            panels: Panels::default(),
            surface_resources: HashMap::new(),
            panel_focus: cx.focus_handle(),
            dictation: None,
            review_expanded: false,
            source_paths: Vec::new(),
            source_items: Vec::new(),
            conversation,
            composer_view,
            workbench_view,
            header_view,
            media: Media::new(),
            sidebar_view: None,
            _subscriptions: subscriptions,
        };
        if !side_chat_mode {
            let desktop = cx.entity();
            let sidebar = cx.new(|cx| view::SidebarView::new(&desktop, &view, cx));
            view.sidebar_view = Some(sidebar);
        }
        if cfg!(windows) && !side_chat_mode {
            view.connecting = true;
            let connections = view.runtime.connections.clone();
            view.effect(
                async move {
                    connections
                        .initial_environment()
                        .await
                        .map_err(|error| format!("{error:#}"))
                },
                Update::InitialHost,
            );
        } else {
            view.connect(None);
        }
        cx.notify();
        view
    }
    fn connect(&mut self, previous: Option<StoreSession>) {
        let previous = previous.and_then(|mut session| session.close());
        self.epoch += 1;
        self.connecting = true;
        self.local_shell = false;
        self.busy = 0;
        self.worktrees.removal = None;
        self.worktrees.removing = false;
        self.account.busy = false;
        self.account.polling = false;
        self.model_controls.provider = None;
        self.account.sign_out = None;
        self.account.login_draft = None;
        self.local_errors.clear();
        if let Some(error) = self.runtime.logging_error.clone() {
            self.local_errors.insert(ErrorScope::Application, error);
        }
        let epoch = self.epoch;
        let remote = self.remote.clone();
        let side = self.side_chat_mode;
        let initial_cwd = self.initial_cwd.take();
        let pending_project = self
            .pending_project
            .as_ref()
            .filter(|(host, _)| host.as_deref() == remote.as_ref().map(|host| host.id.as_str()))
            .map(|(_, cwd)| cwd.clone());
        let updates = self.updates.clone();
        let connections = self.runtime.connections.clone();
        let runtime = self.runtime.clone();
        self.runtime.handle.spawn(async move {
            let result = async {
                if let Some(previous) = previous {
                    let _ = previous.await;
                }
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
                let persistence = Persistence::new(
                    Arc::new(platform::SessionStorage { path }),
                    connections.preferences.clone(),
                );
                let snapshot = persistence
                    .restore(false, Some(platform::conversation_copies(&host)?))
                    .await
                    .map_err(|reason| match reason {
                        RestoreError::InvalidState(reason) => {
                            format!("保存した入力状態を読み込めません: {reason}")
                        }
                        RestoreError::Storage(reason) => reason.to_string(),
                    })?;
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
                if let Some(cwd) = pending_project {
                    store
                        .dispatch(Intent::AddProject(op::AddProject { cwd }))
                        .await
                        .map_err(|error| error.to_string())?;
                }
                if !side {
                    connections
                        .remember_environment(remote.as_ref())
                        .await
                        .map_err(|error| error.to_string())?;
                }
                let local_shell = connections
                    .host_is_local(remote.as_ref().map(|remote| remote.ticket.as_str()))
                    .await;
                Ok::<_, String>((store, persistence, local_shell))
            }
            .await;
            match result {
                Ok((store, persistence, local_shell)) => {
                    StoreSession::publish(
                        Ok(store),
                        runtime,
                        updates,
                        move |session| {
                            (
                                epoch,
                                Update::Connected(
                                    session.map(|session| (session, persistence, local_shell)),
                                ),
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
            Update::InitialHost(result) => self.initial_host(result),
            Update::Connected(result) => {
                self.connecting = false;
                match result {
                    Ok((mut session, persistence, local_shell)) => {
                        self.pending_project = None;
                        self.local_shell = local_shell;
                        session.persist(persistence, self.updates.clone(), move |error| {
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
                        self.dispatch(Intent::ReadFileOpenCapabilities(
                            op::ReadFileOpenCapabilities {},
                        ));
                        if self.import_projects {
                            self.dispatch(Intent::ScanProjects(op::ScanProjects {}));
                        }
                        if self.tab == Tab::Settings {
                            self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
                        }
                    }
                    Err(error) => {
                        self.local_errors.insert(ErrorScope::Application, error);
                    }
                }
            }
            Update::Snapshot => {
                if self.snapshot_pending == Some(epoch) {
                    return;
                }
                self.snapshot_pending = Some(epoch);
                let view = cx.entity().downgrade();
                window.on_next_frame(move |window, cx| {
                    let _ = view.update(cx, |view, cx| {
                        if view.snapshot_pending != Some(epoch) {
                            return;
                        }
                        view.snapshot_pending = None;
                        if view.epoch != epoch {
                            return;
                        }
                        if view.accept_snapshot(window, cx) {
                            cx.notify();
                        } else {
                            view.sync_composer(cx);
                        }
                    });
                });
                return;
            }
            Update::Completed(kind, result) => {
                if !self.operation_completed(kind, result, window, cx) {
                    self.sync_composer(cx);
                    return;
                }
            }
            Update::Recording(id, event) => {
                let level = matches!(event, platform::RecordingEvent::Level(_));
                self.recording_update(id, event);
                if level {
                    self.sync_composer(cx);
                    return;
                }
            }
            Update::LocalCompleted { scope, result } => {
                self.busy = self.busy.saturating_sub(1);
                match result {
                    Ok(()) => {}
                    Err(error) => {
                        self.local_errors.insert(scope, error);
                    }
                }
                self.accept_snapshot(window, cx);
            }
            Update::FilePreview {
                generation,
                scope,
                result,
            } => {
                self.complete_file_preview(generation, scope, result, cx);
            }
            Update::PersistenceError(error) => {
                self.local_errors.insert(ErrorScope::Application, error);
            }
            Update::OnboardingCompleted(result) => match result {
                Ok(()) => {
                    self.onboarding = false;
                    self.import_projects = false;
                }
                Err(error) => self.set_error(error),
            },
            Update::Folder(result) => {
                self.busy = self.busy.saturating_sub(1);
                match result {
                    Ok(Some(platform::ProjectSelection { remote, cwd })) => {
                        self.add_selected_project(remote, cwd, window, cx);
                    }
                    Ok(None) => {}
                    Err(error) => self.set_error(error),
                }
            }
            Update::Image { key, result } => {
                if let Some(image) = self.media.images.get_mut(&key) {
                    match result {
                        Ok(path) => {
                            image.path = Some(if Path::new(&path).is_absolute() {
                                PathBuf::from(path).into()
                            } else {
                                ImageSource::from(path)
                            })
                        }
                        Err(error) => {
                            tracing::error!(target: "agent", operation = "image.load", message = %error);
                            image.error = Some(error);
                        }
                    }
                }
                self.composer_view
                    .read(cx)
                    .content
                    .clone()
                    .update(cx, |_, cx| cx.notify());
                self.conversation.update(cx, |view, cx| {
                    view.timeline.list.remeasure();
                    cx.notify();
                });
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
    ) -> bool {
        let input_save = matches!(
            kind,
            OperationCompletion::Composer(_)
                | OperationCompletion::Answer(_)
                | OperationCompletion::Editor(_)
        );
        match kind {
            OperationCompletion::FileReference(request) => {
                self.complete_reference(*request, result, window, cx);
                return true;
            }
            OperationCompletion::ProjectImport => {
                self.accept_snapshot(window, cx);
                if result.is_ok()
                    && self
                        .snapshot
                        .project_import
                        .result
                        .as_ref()
                        .is_some_and(|result| result.errors.is_empty())
                {
                    self.complete_onboarding();
                }
                return true;
            }
            OperationCompletion::Composer(revision) => self.composer.text.saved(revision),
            OperationCompletion::Answer(revision) => self.question.text.saved(revision),
            OperationCompletion::Editor(revision) => self.editor.text.saved(revision),
            OperationCompletion::Busy => {
                self.busy = self.busy.saturating_sub(1);
            }
            OperationCompletion::Item {
                generation,
                item_id,
            } => {
                if self.snapshot.epoch != generation {
                    return true;
                }
                self.accept_snapshot(window, cx);
                self.conversation.update(cx, |view, cx| {
                    view.timeline.remeasure_item(item_id.as_str());
                    cx.notify();
                });
                return true;
            }
            OperationCompletion::WorktreeSettings => {
                self.worktrees.saving = false;
                self.accept_snapshot(window, cx);
                self.worktrees.dirty = !self
                    .worktrees
                    .matches(self.snapshot.workspace.settings.as_deref(), cx);
                self.worktrees.saved = !self.worktrees.dirty;
                if std::mem::take(&mut self.worktrees.save_pending) {
                    self.save_worktree_settings(None, cx);
                }
                return true;
            }
            OperationCompletion::Request(key) => {
                if result.is_err() {
                    self.conversation.update(cx, |view, cx| {
                        if let Some(inputs) = view.timeline.requests.get_mut(&key) {
                            inputs.sent = false;
                        }
                        cx.notify();
                    });
                }
                self.accept_snapshot(window, cx);
                self.conversation.update(cx, |view, cx| {
                    view.timeline.list.remeasure();
                    cx.notify();
                });
                return true;
            }
            OperationCompletion::Account => {
                self.account.busy = false;
                if result.is_err() {
                    self.account.polling = false;
                    self.dispatch(Intent::ListAccounts(op::ListAccounts {}));
                } else {
                    self.accept_snapshot(window, cx);
                    self.account.polling = self.snapshot.account.login.is_some();
                }
                if self.snapshot.account.login.is_none() {
                    self.account.login_draft = None;
                }
                return true;
            }
            OperationCompletion::RemoveWorktree => {
                self.worktrees.removing = false;
                match result {
                    Ok(_) => self.worktrees.removal = None,
                    Err(_) => {
                        self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
                    }
                }
                self.accept_snapshot(window, cx);
                return true;
            }
            OperationCompletion::Dictation(id) => {
                if self.dictation.as_ref().is_some_and(|state| state.id == id) {
                    self.dictation = None;
                }
            }
            OperationCompletion::Gallery(id) => {
                let Some(gallery) = self
                    .media
                    .gallery
                    .as_mut()
                    .filter(|gallery| gallery.id == id)
                else {
                    return true;
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
                        tracing::error!(target: "agent", operation = "gallery.load", message = %error);
                        gallery.error = error;
                    }
                    _ => unreachable!("session image outcome"),
                }
                return true;
            }
        }
        self.accept_snapshot(window, cx) || !input_save
    }
    fn accept_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let snapshot = session.store.snapshot();
        let changed = !Arc::ptr_eq(&snapshot, &self.snapshot);
        let general_changed = changed && general_snapshot_changed(&self.snapshot, &snapshot);
        let previous = std::mem::replace(&mut self.snapshot, snapshot);
        if !general_changed {
            // A save acknowledgment can make the same snapshot adoptable, so
            // input reconciliation also runs when the snapshot Arc is unchanged.
            self.sync_inputs(window, cx);
            return false;
        }
        if previous.account.login.as_ref().map(|login| &login.login_id)
            != self
                .snapshot
                .account
                .login
                .as_ref()
                .map(|login| &login.login_id)
        {
            self.account
                .code
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
            tracing::error!(target: "agent", operation = "store", message = %error);
        }
        let previous_draft = previous.drafts.get(&previous.navigation.draft_key);
        let sources_changed = previous_draft.map(|draft| &draft.attachments)
            != self
                .snapshot
                .drafts
                .get(self.draft_key())
                .map(|draft| &draft.attachments);
        let navigated = previous.navigation.draft_key != self.snapshot.navigation.draft_key;
        // Store snapshots may coalesce creation and send acceptance. Use the
        // durable core identity transition rather than transient pending sends.
        for (draft, session) in self.snapshot.draft_sessions.iter() {
            if previous.draft_sessions.get(draft) != Some(session) {
                let before = PanelScope {
                    draft: draft.clone(),
                    ..self.panel_scope()
                };
                let after = PanelScope {
                    draft: session.into(),
                    ..self.panel_scope()
                };
                self.panels.rekey(before, after);
            }
        }
        let project_for_selected = |snapshot: &Snapshot| {
            snapshot
                .thread_metadata(snapshot.navigation.thread_id.as_ref()?)
                .filter(|thread| thread.parent_id.is_none())?
                .project_id
                .as_ref()
                .cloned()
        };
        if (navigated || project_for_selected(&previous) != project_for_selected(&self.snapshot))
            && let Some(project) = project_for_selected(&self.snapshot)
        {
            self.dispatch(Intent::SetProjectExpanded {
                project_id: project,
                expanded: true,
            });
        }
        if navigated {
            self.selection
                .update(cx, |selection, cx| selection.clear(cx));
            self.cancel_recording();
            self.composer.text.pending = None;
        }
        self.sync_inputs(window, cx);
        if !self.worktrees.dirty
            && !self.worktrees.saving
            && previous.workspace.settings != self.snapshot.workspace.settings
            && let Some(settings) = &self.snapshot.workspace.settings
        {
            self.worktrees.show(settings, window, cx);
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
        if navigated {
            let panel = self.panels.get(self.panel_scope());
            if panel.open
                && matches!(panel.kind(), Panel::File | Panel::Files)
                && let Some(id) = panel.active
            {
                self.activate_surface(id, window, cx);
            }
        }
        true
    }
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .snapshot
            .drafts
            .get(&self.snapshot.navigation.draft_key)
            .map_or("", |draft| draft.text.as_str());
        if let Some(value) = self.composer.text.adopt(text) {
            self.composer
                .input
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        let prompt = self.question_prompt();
        let answer = self.question.show(prompt.as_ref());
        if let (Some(prompt), Some(answer)) = (prompt, answer) {
            let language = crate::localization::language(cx);
            self.question
                .set_placeholder(language.text(&prompt.placeholder), window, cx);
            self.question.set_value(answer, window, cx);
        }
        let file = self.snapshot.workspace.file.as_ref();
        let text = file.map_or("", |file| {
            self.snapshot
                .file_drafts
                .get(&file.path)
                .filter(|_| !file.truncated)
                .map_or(file.text.as_str(), |draft| draft.text.as_str())
        });
        if let Some(value) = self.editor.show(file.map(|file| file.path.as_str()), text) {
            self.editor
                .input
                .update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if self
            .editor
            .pending_line
            .as_ref()
            .is_some_and(|(path, _)| file.is_some_and(|file| &file.path == path))
        {
            let (_, line) = self.editor.pending_line.take().unwrap();
            let line = (line.saturating_sub(1) as usize)
                .min(text.split('\n').count().saturating_sub(1)) as u32;
            self.editor.input.update(cx, |input, cx| {
                input.set_cursor_position(gpui_kit::base::input::Position::new(line, 0), window, cx)
            });
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
            model_settings_changed: false,
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
    fn new_chat(&mut self, cwd: String, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        self.dispatch(Intent::NewChat { cwd });
        self.composer
            .input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
    }
    fn open_chat(&mut self, id: SessionRef, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        self.dispatch(Intent::ReadThread(op::ReadThread::open(id)));
        self.composer
            .input
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
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
        let item_id = read.item_id.clone();
        self.perform(
            Intent::ReadItem(read),
            OperationCompletion::Item {
                generation,
                item_id,
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
        self.close_file_browser();
        let previous = self.session.take();
        self.remote = remote;
        self.snapshot = Arc::default();
        self.pending_quote = None;
        self.pending_explanation = None;
        self.editor.text.pending = None;
        self.editor.path = None;
        self.worktrees.dirty = false;
        self.worktrees.saving = false;
        self.worktrees.save_pending = false;
        self.media.images.clear();
        self.media.gallery = None;
        self.conversation.update(cx, |view, cx| {
            view.timeline.clear();
            cx.notify();
        });
        self.question.text.pending = None;
        let value = self.composer.text.replace("");
        self.composer
            .input
            .update(cx, |input, cx| input.set_value(value, window, cx));
        self.connect(previous);
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
        if self.composer.input.read(cx).value().trim().is_empty()
            && self.draft().attachments.is_empty()
        {
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
        selection::append_to_composer(&self.composer.input, text, window, cx);
    }

    fn explain_selection(&mut self, text: &str, cx: &mut Context<Self>) {
        let language = crate::localization::language(cx);
        let Some(store) = self.session.as_ref().map(|session| session.store.clone()) else {
            self.pending_explanation = Some(text.into());
            return;
        };
        let cwd = self.snapshot.selected_directory();
        let Some(provider) = self
            .snapshot
            .model_provider_for_draft(self.draft_key().clone())
        else {
            self.set_error(
                language
                    .text("利用可能なエージェントを選択してください。")
                    .into(),
            );
            return;
        };
        let prompt = agent_core::localization::localized_ui_format(
            language,
            "次の選択範囲について詳しく説明してください。\n\n{0}".into(),
            vec![text.to_string()],
        );
        self.busy += 1;
        self.effect(
            async move {
                let Outcome::StartedThread { id } = store
                    .dispatch(Intent::CreateSession(op::CreateSession {
                        initial_input: None,
                        provider,
                        cwd: Some(cwd),
                        model: None,
                    }))
                    .await
                    .map_err(|error| error.to_string())?
                else {
                    return Err(language
                        .text("新しいサイドチャットを作成できませんでした")
                        .into());
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
    fn panel_scope(&self) -> PanelScope {
        self.snapshot.panel_scope(
            self.remote
                .as_ref()
                .map_or_else(|| "local".into(), |host| host.id.clone()),
            self.draft_key().clone(),
        )
    }
    fn open_panel(&mut self, kind: Panel, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.panels.open(self.panel_scope(), kind, None);
        self.tab = Tab::Chat;
        self.activate_surface(id, window, cx);
    }
    fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible != visible {
            self.visible = visible;
            self.sync_surfaces(window, cx);
            cx.notify();
        }
    }
    fn open_browser(&mut self, scope: PanelScope, url: &str, cx: &mut Context<Self>) {
        if let Err(error) = agent_protocol::browser::browser_url(url) {
            self.set_error(error);
            cx.notify();
            return;
        }
        let current = scope == self.panel_scope();
        let id = self.panels.open(scope, Panel::Browser, None);
        self.surface_resources.insert(
            id,
            SurfaceResource::Pending {
                url: Some(url.into()),
            },
        );
        if current {
            self.tab = Tab::Chat;
        }
        cx.notify();
    }
    fn activate_surface(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.panel_scope();
        if !self.panels.activate(key.clone(), id) {
            return;
        }
        let surface = self.panels.get(key).selected().unwrap().clone();
        match surface.kind {
            Panel::Browser => {
                self.surface_resources
                    .entry(id)
                    .or_insert(SurfaceResource::Pending { url: None });
            }
            Panel::Terminal => {
                if !self.surface_resources.contains_key(&id) {
                    let Some(session) = &self.session else {
                        return;
                    };
                    let terminal = crate::terminal::Terminal::new(
                        session.store.clone(),
                        self.snapshot.navigation.cwd.clone(),
                        format!("agent-terminal-{}", uuid::Uuid::new_v4()),
                        window,
                        cx,
                    );
                    self.surface_resources
                        .insert(id, SurfaceResource::Terminal(terminal));
                }
                if let Some(SurfaceResource::Terminal(terminal)) = self.surface_resources.get(&id) {
                    terminal.update(cx, |terminal, cx| terminal.focus(window, cx));
                }
            }
            Panel::SideChat => {
                if !self.surface_resources.contains_key(&id) {
                    let chat = cx.new(|cx| {
                        Self::new(
                            Mode::SideChat {
                                remote: self.remote.clone(),
                                cwd: self.snapshot.selected_directory(),
                            },
                            window,
                            cx,
                        )
                    });
                    self.surface_resources
                        .insert(id, SurfaceResource::SideChat(chat));
                }
            }
            Panel::File => {
                if let Some(path) = surface.path {
                    self.dispatch(Intent::ReadFile(op::ReadFile {
                        path,
                        discard_draft: false,
                    }));
                }
            }
            Panel::Files => (),
            Panel::Diff => self.refresh_review(),
            Panel::Agents | Panel::Home => (),
        }
        cx.notify();
    }
    fn close_surface(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        if self.panels.scope(id) != Some(self.panel_scope()) {
            return;
        }
        self.panels.close(self.panel_scope(), id);
        match self.surface_resources.remove(&id) {
            Some(SurfaceResource::Browser(view)) => {
                view.update(cx, |view, cx| view.set_visible(false, cx));
            }
            Some(SurfaceResource::Terminal(view)) => view.update(cx, |view, _| view.close()),
            Some(SurfaceResource::SideChat(view)) => {
                view.update(cx, |view, cx| view.set_visible(false, window, cx))
            }
            _ => (),
        }
        if let Some(next) = self.panels.get(self.panel_scope()).active {
            self.activate_surface(next, window, cx);
        } else {
            self.panel_focus.focus(window, cx);
        }
        cx.notify();
    }
    fn pick_folder(&mut self) {
        self.busy += 1;
        let initial = self
            .remote
            .as_ref()
            .and_then(|host| host.id.strip_prefix("wsl:"))
            .and_then(|distro| {
                let cwd = if self.snapshot.navigation.cwd.is_empty() {
                    self.snapshot
                        .host_home_directory
                        .as_deref()
                        .unwrap_or("/home")
                } else {
                    self.snapshot.navigation.cwd.as_str()
                };
                agent_local_host::wsl::to_unc(distro, cwd)
                    .ok()
                    .map(PathBuf::from)
            });
        let connections = self.runtime.connections.clone();
        self.effect(
            async move {
                let path = tokio::task::spawn_blocking(move || platform::choose_folder(initial))
                    .await
                    .map_err(|error| error.to_string())?;
                match path {
                    Some(path) => connections
                        .project_target(path)
                        .await
                        .map(Some)
                        .map_err(|error| format!("{error:#}")),
                    None => Ok(None),
                }
            },
            Update::Folder,
        );
    }
    fn initial_host(&mut self, result: Result<Option<RemoteHost>, String>) {
        self.connecting = false;
        match result {
            Ok(remote) => {
                self.remote = remote;
                self.connect(None);
            }
            Err(error) => self.set_error(error),
        }
    }
    fn add_selected_project(
        &mut self,
        remote: Option<RemoteHost>,
        cwd: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.tab = Tab::Chat;
        self.cancel_recording();
        if self.snapshot.connected
            && self.remote.as_ref().map(|host| &host.id) == remote.as_ref().map(|host| &host.id)
        {
            self.dispatch(Intent::AddProject(op::AddProject { cwd }));
        } else {
            self.pending_project = Some((remote.as_ref().map(|host| host.id.clone()), cwd));
            self.switch_host(remote, window, cx);
        }
    }
    fn composer_arrow(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.completion_key(key, window, cx) {
            return;
        }
        let moved = self.composer.input.update(cx, |input, cx| {
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
        let submit = self.composer.input.update(cx, |input, cx| {
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
                    ClipboardEntry::Image(image) => Some(clipboard::save_image(&directory, image)),
                    _ => None,
                })
                .collect()
        });
        cx.notify();
    }
    fn attach(&mut self) {
        self.attach_sources(|| Ok(platform::choose_files().unwrap_or_default()));
    }
    fn attach_sources<P: AsRef<Path> + Send + 'static>(
        &mut self,
        sources: impl FnOnce() -> Result<Vec<P>, String> + Send + 'static,
    ) {
        let Some(store) = self.session.as_ref().map(|session| session.store.clone()) else {
            return;
        };
        let key = self.draft_key().to_owned();
        self.busy += 1;
        let error_scope = ErrorScope::Conversation {
            draft_key: key.clone(),
        };
        self.effect(
            async move {
                let paths = tokio::task::spawn_blocking(sources)
                    .await
                    .map_err(|error| error.to_string())??;
                for source in paths {
                    let path = source.as_ref();
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
                    let intent = Intent::UploadAttachment(op::UploadAttachment {
                        draft_key: key.clone(),
                        attachment,
                    });
                    if store.dispatch(intent).await.is_err() {
                        break;
                    }
                }
                Ok(())
            },
            move |result| Update::LocalCompleted {
                scope: error_scope,
                result,
            },
        );
    }
    fn browse(&mut self, path: String, cx: &mut Context<Self>) {
        self.file_open_generation += 1;
        self.editor.pending_line = None;
        self.reveal_file_in_tree(Some(&path), true);
        self.panels
            .open(self.panel_scope(), Panel::Files, Some(path));
        self.tab = Tab::Chat;
        cx.notify();
    }
    fn open_file(
        &mut self,
        path: String,
        line: Option<u32>,
        discard_draft: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        self.file_open_generation += 1;
        self.editor.pending_line = None;
        if agent_core::presentation::files::file_preview_mime_type(path.clone()).is_some() {
            self.preview_file(path, cx);
            return false;
        }
        self.editor.pending_line = line.map(|line| (path.clone(), line));
        self.panels
            .open(self.panel_scope(), Panel::File, Some(path.clone()));
        self.dispatch(Intent::ReadFile(op::ReadFile {
            path,
            discard_draft,
        }));
        true
    }
    fn save_file(&mut self) {
        let Some(path) = self.editor.path.clone() else {
            return;
        };
        self.busy += 1;
        self.perform(
            Intent::SaveFile(op::SaveFile { path }),
            OperationCompletion::Busy,
        );
    }
    fn worktree_input_changed(&mut self, cx: &App) {
        self.worktrees.dirty = !self
            .worktrees
            .matches(self.snapshot.workspace.settings.as_deref(), cx);
        self.worktrees.saved = false;
    }
    fn save_worktree_settings(&mut self, toggle: Option<WorktreeToggle>, cx: &Context<Self>) {
        let Some(settings) = &self.snapshot.workspace.settings else {
            return;
        };
        let mut settings = self.worktrees.edited(settings, cx);
        if let Some(toggle) = toggle {
            match toggle {
                WorktreeToggle::Create(checked) => settings.create_on_new_session = checked,
                WorktreeToggle::Copy(checked) => settings.copy_on_create = checked,
                WorktreeToggle::DeleteUnchanged(checked) => settings.delete_unchanged = checked,
                WorktreeToggle::ProjectDeleteUnchanged(project, Some(checked)) => {
                    settings.project_delete_unchanged.insert(project, checked);
                }
                WorktreeToggle::ProjectDeleteUnchanged(project, None) => {
                    settings.project_delete_unchanged.remove(&project);
                }
            }
        }
        if self.snapshot.workspace.settings.as_deref() == Some(&settings) {
            return;
        }
        if self.worktrees.saving {
            self.worktrees.save_pending = true;
            return;
        }
        self.worktrees.saving = true;
        self.worktrees.saved = false;
        self.perform(
            Intent::UpdateWorktreeSettings(op::UpdateWorktreeSettings { settings }),
            OperationCompletion::WorktreeSettings,
        );
    }
    fn respond(&mut self, id: RequestId, answer: Answer) {
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
    !action.shift && !action.secondary && input.marked_text_range(window, cx).is_none()
}

#[cfg(test)]
mod snapshot_tests {
    use super::{Snapshot, general_snapshot_changed};
    use std::sync::Arc;

    #[test]
    fn terminal_output_does_not_republish_the_desktop() {
        let previous = Snapshot::default();
        let mut next = previous.clone();
        next.terminals = Arc::new(Default::default());
        assert!(!general_snapshot_changed(&previous, &next));
        next.connected = true;
        assert!(general_snapshot_changed(&previous, &next));
    }
}

#[cfg(test)]
mod composer_tests {
    use super::{TextareaState, composer_should_submit};
    use gpui_kit as gpui;
    use gpui_kit::{EntityInputHandler, TestAppContext};
    #[gpui::test]
    fn enter_submits_committed_text_regardless_of_cursor_or_selection(cx: &mut TestAppContext) {
        cx.update(crate::appearance::init);
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
                assert!(composer_should_submit(input, &enter, window, cx));
                input.set_selected_range(0..end, cx);
                assert!(composer_should_submit(input, &enter, window, cx));
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
mod completion_tests {
    use super::{Arc, Context, Desktop, Mode, OperationCompletion, Outcome, Update, Window};
    use gpui_kit as gpui;
    use gpui_kit::TestAppContext;

    #[gpui::test]
    fn language_switch_updates_existing_inputs_and_preserves_drafts(cx: &mut TestAppContext) {
        use crate::localization::Language;
        use agent_core::localization::UiLanguage;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
            cx.set_global(Language(UiLanguage::Japanese));
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
            view.composer.input.update(cx, |input, cx| {
                input.set_value("ユーザーの入力 {0}", window, cx)
            });
        })
        .unwrap();
        for (language, placeholder) in [
            (UiLanguage::English, "Ask AI"),
            (UiLanguage::Japanese, "AI に依頼する"),
        ] {
            cx.update(|cx| cx.set_global(Language(language)));
            view.update(cx, |view, _, cx| {
                let input = view.composer.input.read(cx);
                assert_eq!(input.presentation().placeholder().as_ref(), placeholder);
                assert_eq!(input.text().to_string(), "ユーザーの入力 {0}");
            })
            .unwrap();
        }
    }

    #[gpui::test]
    fn completion_messages_preserve_newer_host_navigation_and_input(cx: &mut TestAppContext) {
        // Do not drive this runtime: this test exercises UI result delivery, not provisioning.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        cx.update(|cx| {
            crate::appearance::init(cx);
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
            view.composer.text.pending = Some(9);
            view.editor.text.pending = Some(12);
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
            assert!(view.local_errors.is_empty());
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
            assert_eq!(view.composer.text.pending, Some(9));
            assert_eq!(view.editor.text.pending, Some(12));
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
            assert_eq!(view.composer.text.pending, None);
            assert_eq!(view.editor.text.pending, None);
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
}

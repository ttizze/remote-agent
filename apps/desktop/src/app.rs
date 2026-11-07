use agent_core::state::operations as op;
mod clipboard;
mod completions;
mod connections;
mod dictation;
mod files;
mod forms;
mod hosts;
mod preferences;
mod selection;
mod timeline;
mod view;

use crate::{Runtime, appearance, diff::DiffView, platform, store_session::StoreSession};
use agent_core::{
    panels::{PanelKind as Panel, PanelLayout, PanelScope, Panels},
    presentation::{conversation::ConversationRowContent, error::ErrorScope},
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
use preferences::Preferences;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Weak},
};
use timeline::{ConversationRow, MarkdownContent, RequestInputs, Timeline};
use view::icon_button;
use view::media::Media;

struct ProjectEditor {
    id: String,
    host: String,
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
    Request(RequestId),
    Dictation(uuid::Uuid),
    FileReference(Box<files::ReferenceRequest>),
}
enum Update {
    InitialHost(
        Result<
            (
                agent_core::presentation::connection_list::ConnectionSelection,
                Option<RemoteHost>,
            ),
            String,
        >,
    ),
    Connected(Result<(StoreSession, bool), String>),
    Snapshot,
    Completed(OperationCompletion, Result<Outcome, String>),
    Folder(Result<Option<platform::ProjectSelection>, String>),
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
    Providers,
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
        session: StoreSession,
    },
    #[cfg(test)]
    Fixture {
        remote: Option<RemoteHost>,
        cwd: String,
    },
}
/// Business state is owned by Store. Everything else here is a widget, render
/// cache, pending UI effect, or immutable snapshot retained for display.
pub(crate) struct Desktop {
    session: Option<StoreSession>,
    snapshot: Arc<Snapshot>,
    publication: Arc<agent_core::presentation::view::Presentation>,
    runtime: Runtime,
    updates: async_channel::Sender<(u64, Update)>,
    snapshot_pending: Option<u64>,
    epoch: u64,
    connecting: bool,
    local_shell: bool,
    remote: Option<RemoteHost>,
    connection_selection: agent_core::presentation::connection_list::ConnectionSelection,
    connection_list: Option<Arc<agent_core::presentation::list::ThreadList>>,
    connection_names: Vec<(String, String)>,
    connected_hosts: std::collections::BTreeSet<String>,
    hosts: Option<Entity<Hosts>>,
    app_updates: Option<Entity<crate::updates::Updates>>,
    side_chat_mode: bool,
    visible: bool,
    onboarding: bool,
    import_projects: bool,
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
    tab: Tab,
    preferences: Entity<Preferences>,
    sidebar: bool,
    /// Whether thread details stay docked beside the chat when they fit.
    details: bool,
    /// Whether thread details are open as a popover while they cannot dock.
    details_popover: bool,
    panels: Arc<Panels>,
    surface_resources: HashMap<u64, SurfaceResource>,
    panel_focus: FocusHandle,
    dictation: Option<Dictation>,
    review_expanded: bool,
    workspace_view: Entity<view::workspace::WorkspaceView>,
    conversation: Entity<view::ConversationView>,
    composer_view: Entity<view::ComposerView>,
    workbench_view: Entity<view::WorkbenchView>,
    header_view: Entity<view::HeaderView>,
    media: Entity<Media>,
    /// Absent in side chats, which have no sidebar.
    sidebar_view: Option<Entity<view::SidebarView>>,
    _subscriptions: Vec<Subscription>,
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
        let (remote, side_session, side_chat_mode, initial_snapshot) = match mode {
            Mode::Main => (None, None, false, Snapshot::default()),
            Mode::SideChat { remote, session } => {
                (remote, Some(session), true, Snapshot::default())
            }
            #[cfg(test)]
            Mode::Fixture { remote, cwd } => {
                let initial_snapshot = agent_core::state::reduce(
                    &Snapshot::default(),
                    agent_core::state::Event::Intent(Intent::NewChat { cwd }),
                )
                .0;
                (remote, None, true, initial_snapshot)
            }
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
        let composer = forms::ComposerInput::new(window, cx);
        let question = forms::QuestionInput::new(window, cx);
        let editor = forms::FileEditor::new(window, cx);
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(language.text("会話を検索")));
        let file_search =
            cx.new(|cx| InputState::new(window, cx).placeholder(language.text("ファイルを検索")));
        let hosts = (!side_chat_mode).then(|| cx.new(|cx| Hosts::new(window, cx)));
        let app_updates = (!side_chat_mode
            && cfg!(any(target_os = "macos", target_os = "windows")))
        .then(|| cx.new(crate::updates::Updates::new));
        let selection = cx.new(|_| selection::ConversationSelection::new(!side_chat_mode));
        let preferences =
            cx.new(|cx| Preferences::new(hosts.clone(), app_updates.clone(), window, cx));
        let owner = cx.entity();
        let media =
            cx.new(|cx| Media::new(cx.global::<Runtime>().handle.clone(), side_chat_mode, cx));
        let conversation =
            cx.new(|cx| view::ConversationView::new(&owner, selection.clone(), media.clone(), cx));
        let composer_view = cx.new(|cx| {
            view::ComposerView::new(
                &owner,
                media.clone(),
                preferences.clone(),
                composer.input.clone(),
                question.input.clone(),
                question.secret.clone(),
                cx,
            )
        });
        let composer_content = composer_view.read(cx).content.clone();
        let file_explorer = files::FileExplorer::new(cx);
        let workbench_view =
            cx.new(|cx| view::WorkbenchView::new(&owner, &file_explorer.state, cx));
        let workspace_view = cx.new(view::workspace::WorkspaceView::new);
        let header_view = cx.new(|cx| view::HeaderView::new(&owner, cx));
        let mut subscriptions = vec![
            cx.subscribe_in(
                &workspace_view,
                window,
                |view, _, action: &view::workspace::WorkspaceAction, window, cx| {
                    use view::workspace::WorkspaceAction::*;
                    match action {
                        Download(path) => view.download(path.clone()),
                        RefreshReview => view.refresh_review(),
                        Review => view.open_review(None, cx),
                        Attach => view.attach(),
                        PickFolder => view.pick_folder(),
                        Browse(path) => view.browse(path.clone(), cx),
                        OpenEditor { path, editor } => {
                            view.dispatch(Intent::SetPreferredEditor { editor: *editor });
                            view.open_in_editor(path.clone(), None, None, *editor, false, cx);
                        }
                        Notice(action) => view.notice_action(action.clone(), window, cx),
                    }
                    cx.notify();
                },
            ),
            cx.observe(&preferences, |view, preferences, cx| {
                let enabled = preferences.read(cx).model_controls_enabled();
                view.composer_view
                    .read(cx)
                    .content
                    .clone()
                    .update(cx, |content, cx| {
                        content.set_controls_enabled(enabled, cx);
                    });
                if view.onboarding || view.import_projects {
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &preferences,
                window,
                |view, _, event: &preferences::PreferencesEvent, window, cx| {
                    match event {
                        preferences::PreferencesEvent::Closed => {
                            view.tab = Tab::Chat;
                            view.composer
                                .input
                                .read(cx)
                                .focus_handle(cx)
                                .focus(window, cx);
                        }
                        preferences::PreferencesEvent::ImportProjects => view.open_project_import(),
                        preferences::PreferencesEvent::OpenBrowser(url) => {
                            view.open_browser(view.panel_scope(), url, cx)
                        }
                        preferences::PreferencesEvent::OpenChat(id) => {
                            view.open_chat(id.clone(), window, cx)
                        }
                        preferences::PreferencesEvent::Notice(action) => {
                            view.notice_action(action.clone(), window, cx)
                        }
                    }
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &media,
                window,
                |view, _, event: &view::media::MediaEvent, window, cx| {
                    match event {
                        view::media::MediaEvent::Visibility => view.sync_surfaces(window, cx),
                        view::media::MediaEvent::Error(error) => view.set_error(error.clone()),
                    }
                    cx.notify();
                },
            ),
            cx.observe_global_in::<crate::localization::Language>(window, |view, window, cx| {
                let language = crate::localization::language(cx);
                for (input, source) in [
                    (&view.search, language.text("会話を検索")),
                    (&view.file_search, language.text("ファイルを検索")),
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
                view.publish_views(window, cx)
            }),
            cx.observe_window_bounds(window, |view, window, cx| view.sync_surfaces(window, cx)),
            cx.subscribe_in(
                &composer_content,
                window,
                |view, _, action: &view::composer::ComposerAction, window, cx| {
                    use view::composer::ComposerAction::*;
                    match action {
                        NewChat(cwd) => view.new_chat(cwd.clone(), window, cx),
                        PickFolder => view.pick_folder(),
                        Send => view.send(cx),
                        Attach => view.attach(),
                        StartDictation => view.start_dictation(),
                        CancelDictation => view.cancel_recording(),
                        FinishDictation(send) => view.finish_dictation(*send),
                        AddMentions(text) => view.add_file_mentions(text, window, cx),
                        QuestionCollapsed(value) => view.question.collapsed = value.clone(),
                        Respond(id, answer) => view.respond(id.clone(), answer.clone()),
                        Review(path) => view.open_review(path.as_deref(), cx),
                        RefreshReview => view.refresh_review(),
                        ToggleReview => view.review_expanded = !view.review_expanded,
                        Completion(invocation) => {
                            view.insert_completion(invocation.clone(), window, cx)
                        }
                    }
                    cx.notify();
                },
            ),
            cx.subscribe_in(
                &workbench_view,
                window,
                |view, _, action: &view::workbench::WorkbenchAction, window, cx| {
                    view.workbench_action(action, window, cx)
                },
            ),
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
            cx.subscribe(&search, |view, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value();
                    if value.as_ref() != view.snapshot.lists.list_query.search_term {
                        let mut query = (*view.snapshot.lists.list_query).clone();
                        query.search_term = value.to_string();
                        view.dispatch_list(Intent::ListSessions(op::ListSessions::new(query)), cx);
                    }
                }
            }),
        ];
        if let Some(hosts) = &hosts {
            subscriptions.push(cx.observe(hosts, |_, _, cx| cx.notify()));
            subscriptions.push(
                cx.subscribe_in(hosts, window, |view, _, event, window, cx| {
                    match event {
                        HostEvent::Selected(remote) => view.switch_host(remote.clone(), window, cx),
                        HostEvent::Enabled(remote, enabled) => {
                            view.set_connection_enabled(remote.clone(), *enabled)
                        }
                        HostEvent::ListsChanged => view.rebuild_connection_list(cx),
                        HostEvent::Failed(error) => view.set_error(error.clone()),
                        HostEvent::Removed(id) => {
                            let removed = view.runtime.sessions.remove(id);
                            view.effect(removed, |result| Update::LocalCompleted {
                                scope: ErrorScope::Settings,
                                result,
                            });
                            if view.remote.as_ref().is_some_and(|remote| &remote.id == id) {
                                view.switch_host(None, window, cx);
                            }
                        }
                    }
                    cx.notify();
                }),
            );
        }
        if let Some(updates) = &app_updates {
            subscriptions.push(cx.observe(updates, |_, _, cx| cx.notify()));
            subscriptions.push(cx.subscribe(
                updates,
                |view, _, _: &crate::updates::RefreshThreads, cx| view.refresh_threads(cx),
            ));
        }
        let mut view = Self {
            session: None,
            snapshot: Arc::new(initial_snapshot),
            publication: agent_core::presentation::view::Presentation::empty(),
            runtime: cx.global::<Runtime>().clone(),
            updates,
            snapshot_pending: None,
            epoch: 0,
            connecting: false,
            local_shell: false,
            pending_project: None,
            remote,
            connection_selection: agent_core::presentation::connection_list::ConnectionSelection {
                host_id: Some("local".into()),
                all: false,
            },
            connection_list: None,
            connected_hosts: Default::default(),
            connection_names: vec![("local".into(), "このPC".into())],
            hosts,
            app_updates,
            side_chat_mode,
            visible: true,
            onboarding: !side_chat_mode
                && !platform::state_dir()
                    .is_ok_and(|directory| directory.join("onboarding.completed").is_file()),
            import_projects: false,
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
            tab: Tab::Chat,
            preferences,
            sidebar: true,
            details: true,
            details_popover: false,
            panels: Panels::new(),
            surface_resources: HashMap::new(),
            panel_focus: cx.focus_handle(),
            dictation: None,
            review_expanded: false,
            workspace_view,
            conversation,
            composer_view,
            workbench_view,
            header_view,
            media,
            sidebar_view: None,
            _subscriptions: subscriptions,
        };
        if !side_chat_mode {
            let desktop = cx.entity();
            let sidebar = cx.new(|cx| {
                view::SidebarView::new(
                    &desktop,
                    view.sidebar_model(),
                    view.search.clone(),
                    view.app_updates.clone(),
                    view.session.as_ref().map(|s| s.pane.clone()),
                    cx,
                )
            });
            view._subscriptions.push(cx.subscribe_in(
                &sidebar,
                window,
                |view, _, action: &view::SidebarAction, window, cx| {
                    match action {
                        view::SidebarAction::NewChat(host, cwd) => view.navigate_host(
                            host,
                            Intent::NewChat { cwd: cwd.clone() },
                            window,
                            cx,
                        ),
                        view::SidebarAction::OpenChat(host, id) => view.navigate_host(
                            host,
                            Intent::ReadThread(op::ReadThread::open(id.clone())),
                            window,
                            cx,
                        ),
                        view::SidebarAction::Connection(host) => {
                            view.select_connection(host.clone(), cx)
                        }
                        view::SidebarAction::List(host, intent) => match host {
                            Some(host) => view.dispatch_host(host, intent.clone(), cx),
                            None => view.dispatch_list(intent.clone(), cx),
                        },
                        view::SidebarAction::Settings(page) => {
                            view.open_settings(*page, window, cx);
                            if *page == SettingsPage::Connections
                                && let Some(hosts) = &view.hosts
                            {
                                hosts.update(cx, |hosts, _| hosts.refresh());
                            }
                        }
                        view::SidebarAction::Refresh => view.refresh_threads(cx),
                        view::SidebarAction::DismissLocal(op::ErrorId::Application { message })
                            if view.local_errors.get(&ErrorScope::Projects) == Some(message) =>
                        {
                            view.local_errors.remove(&ErrorScope::Projects);
                        }
                        _ => {}
                    }
                    cx.notify();
                },
            ));
            view.sidebar_view = Some(sidebar);
        }
        if let Some(session) = side_session {
            let updates = view.updates.clone();
            let local = view.remote.is_none();
            view.runtime.handle.spawn(StoreSession::observe(
                session,
                updates,
                move |result| (0, Update::Connected(result.map(|session| (session, local)))),
                |_| (0, Update::Snapshot),
            ));
        } else if !side_chat_mode {
            let runtime = view.runtime.clone();
            view.effect(
                async move { Self::restore_connections(runtime).await },
                Update::InitialHost,
            );
        }
        cx.notify();
        view
    }
    fn connect(&mut self, previous: Option<StoreSession>, enabled: Option<bool>) {
        drop(previous);
        self.epoch += 1;
        self.connecting = true;
        self.local_shell = false;
        self.busy = 0;
        self.local_errors.clear();
        if let Some(error) = self.runtime.logging_error.clone() {
            self.local_errors.insert(ErrorScope::Application, error);
        }
        let epoch = self.epoch;
        let remote = self.remote.clone();
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
                let session = runtime
                    .sessions
                    .open(
                        remote.as_ref().map(|remote| remote.ticket.as_str()),
                        runtime.clone(),
                        enabled,
                    )
                    .await?;
                if let Some(cwd) = pending_project {
                    session
                        .pane
                        .dispatch(Intent::AddProject(op::AddProject { cwd }))
                        .await
                        .map_err(|error| error.to_string())?;
                }
                let local_shell = connections
                    .host_is_local(remote.as_ref().map(|remote| remote.ticket.as_str()))
                    .await;
                Ok::<_, String>((session, local_shell))
            }
            .await;
            match result {
                Ok((session, local_shell)) => {
                    StoreSession::observe(
                        session,
                        updates,
                        move |session| {
                            (
                                epoch,
                                Update::Connected(session.map(|session| (session, local_shell))),
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
    fn load_host_configuration(&self) {
        if !self.snapshot.connection.connected {
            return;
        }
        self.dispatch(Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}));
        self.dispatch(Intent::ReadFileOpenCapabilities(
            op::ReadFileOpenCapabilities {},
        ));
        self.dispatch(Intent::ReadProviderSettings(op::ReadProviderSettings {}));
        if self.import_projects {
            self.dispatch(Intent::ScanProjects(op::ScanProjects {}));
        }
        if self.tab == Tab::Settings {
            self.dispatch(Intent::ReadHostSoftware(op::ReadHostSoftware {}));
            self.dispatch(Intent::ListWorktrees(op::ListWorktrees {}));
        }
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
        let Some(store) = self.session.as_ref().map(|session| &session.pane) else {
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
            drop(session.pane.dispatch(intent));
        }
    }
    fn connected(
        &mut self,
        session: StoreSession,
        local_shell: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let epoch = self.epoch;
        self.pending_project = None;
        self.local_shell = local_shell;
        let persistence = session.host.persistence();
        if let Some(persistence) = persistence {
            session.persist(persistence, self.updates.clone(), move |error| {
                (epoch, Update::PersistenceError(error))
            });
        }
        self.session = Some(session);
        self.accept_snapshot(window, cx);
        if let Some(text) = self.pending_quote.take() {
            self.quote_selection(&text, window, cx);
        }
        if let Some(text) = self.pending_explanation.take() {
            self.explain_selection(&text, cx);
        }
        self.load_host_configuration();
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
            Update::InitialHost(result) => {
                self.initial_host(result);
                if let (Some(hosts), Some(remote)) = (&self.hosts, &self.remote) {
                    hosts.update(cx, |hosts, _| hosts.track_remote(remote));
                }
                self.rebuild_connection_list(cx);
            }
            Update::Connected(result) => {
                self.connecting = false;
                match result {
                    Ok((session, local_shell)) => self.connected(session, local_shell, window, cx),
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
                        .is_some_and(|result| {
                            result.errors.is_empty() && result.warnings.is_empty()
                        })
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
                if self.snapshot.connection.epoch != generation {
                    return true;
                }
                self.accept_snapshot(window, cx);
                self.conversation.update(cx, |view, cx| {
                    view.timeline.remeasure_item(item_id.as_str());
                    cx.notify();
                });
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

            OperationCompletion::Dictation(id) => {
                if self.dictation.as_ref().is_some_and(|state| state.id == id) {
                    self.dictation = None;
                }
            }
        }
        self.accept_snapshot(window, cx) || !input_save
    }
    fn accept_snapshot(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        let Ok(publication) = session.pane.presentation() else {
            return false;
        };
        let changes = publication.changes(self.publication.clone());
        let snapshot = publication.snapshot();
        let window_uses_snapshot =
            self.onboarding || self.import_projects || self.project_editor.is_some();
        let navigation_changed = self.snapshot.pane.navigation != snapshot.pane.navigation;
        let connection_changed = changes.groups.connection;
        let application_error_changed = self.snapshot.pane.error != snapshot.pane.error;
        self.publication = publication;
        let previous = std::mem::replace(&mut self.snapshot, snapshot);
        if let Some(request) = self
            .snapshot
            .permission_control(self.draft_key())
            .load_request
        {
            self.dispatch(Intent::ReadPermissionSettings(request));
        }
        if previous.pane.error != self.snapshot.pane.error
            && let Some(error) = &self.snapshot.pane.error
        {
            tracing::error!(target: "agent", operation = "store", message = %error);
        }
        let navigated =
            previous.pane.navigation.draft_key != self.snapshot.pane.navigation.draft_key;
        // Store snapshots may coalesce creation and send acceptance. Use the
        // durable core identity transition rather than transient pending sends.
        for (draft, session) in self.snapshot.sessions.draft_sessions.iter().filter(|_| {
            !Arc::ptr_eq(
                &previous.sessions.draft_sessions,
                &self.snapshot.sessions.draft_sessions,
            )
        }) {
            if previous.sessions.draft_sessions.get(draft) != Some(session) {
                let before = PanelScope {
                    draft: draft.clone(),
                    ..self.panel_scope()
                };
                let after = PanelScope {
                    draft: session.into(),
                    ..self.panel_scope()
                };
                self.panels = self.panels.clone().rekey(before, after);
            }
        }
        let project_for_selected = |snapshot: &Snapshot| {
            snapshot
                .thread_metadata(snapshot.pane.navigation.thread_id.as_ref()?)
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
        if navigated {
            let panel = self.panels.get(self.panel_scope());
            if panel.open
                && matches!(panel.kind(), Panel::File | Panel::Files)
                && let Some(id) = panel.active
            {
                self.activate_surface(id, window, cx);
            }
        }
        self.publish_presentation(changes, window, cx);
        window_uses_snapshot && changes.changed
            || navigation_changed
            || connection_changed
            || application_error_changed
    }
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .snapshot
            .input
            .drafts
            .get(&self.snapshot.pane.navigation.draft_key)
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
        let file = self.snapshot.pane.workspace.file.as_ref();
        let text = file.map_or("", |file| {
            self.snapshot
                .files
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
        &self.snapshot.pane.navigation.draft_key
    }
    fn selected(&self) -> Option<&SessionRef> {
        self.snapshot.pane.navigation.thread_id.as_ref()
    }
    fn thread(&self) -> Option<&Arc<Thread>> {
        self.selected()
            .and_then(|id| self.snapshot.sessions.conversations.get(id))
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
            .input
            .drafts
            .get(self.draft_key())
            .map_or(&EMPTY, Arc::as_ref)
    }
    fn selected_model(&self) -> Option<&Model> {
        selected_model(&self.snapshot.catalog.models, self.draft().model.as_ref())
    }
    fn remote_key(&self) -> &str {
        self.remote.as_ref().map_or("local", |remote| &remote.id)
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
        let generation = self.snapshot.connection.epoch;
        let item_id = read.item_id.clone();
        self.perform(
            Intent::ReadItem(read),
            OperationCompletion::Item {
                generation,
                item_id,
            },
        );
    }
    fn set_connection_enabled(&mut self, remote: Option<RemoteHost>, enabled: bool) {
        let runtime = self.runtime.clone();
        self.effect(
            async move {
                runtime
                    .sessions
                    .open(
                        remote.as_ref().map(|host| host.ticket.as_str()),
                        runtime.clone(),
                        Some(enabled),
                    )
                    .await?;
                Ok(())
            },
            |result| Update::LocalCompleted {
                scope: ErrorScope::Settings,
                result,
            },
        );
    }

    fn switch_host(
        &mut self,
        remote: Option<RemoteHost>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.remote == remote && self.snapshot.connection.connected {
            return;
        }
        self.cancel_recording();
        self.dictation = None;
        self.close_file_browser();
        let previous = self.session.take();
        self.remote = remote;
        self.connection_selection.host_id = Some(self.connection_id());
        self.save_connection_selection();
        self.rebuild_connection_list(cx);
        self.snapshot = Arc::default();
        self.publication = agent_core::presentation::view::Presentation::empty();
        self.pending_quote = None;
        self.pending_explanation = None;
        self.editor.text.pending = None;
        self.editor.path = None;
        self.media.update(cx, |media, cx| media.reset(cx));
        self.conversation.update(cx, |view, cx| {
            view.timeline.clear();
            cx.notify();
        });
        self.question.text.pending = None;
        let value = self.composer.text.replace("");
        self.composer
            .input
            .update(cx, |input, cx| input.set_value(value, window, cx));
        self.connect(previous, None);
    }
    fn send(&mut self, cx: &Context<Self>) {
        if !self.snapshot.connection.connected || self.busy > 0 {
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
        let Some(store) = self.session.as_ref().map(|session| session.pane.clone()) else {
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

    fn refresh_threads(&self, cx: &App) {
        self.dispatch_list(
            Intent::ListSessions(op::ListSessions::new(
                (*self.snapshot.lists.list_query).clone(),
            )),
            cx,
        );
    }
    fn refresh_review(&self) {
        if !self.snapshot.selected_directory().is_empty() {
            self.dispatch(Intent::ReviewWorkspace(op::ReviewWorkspace {
                cwd: self.snapshot.pane.navigation.cwd.clone(),
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
    fn open_surface(&mut self, scope: PanelScope, kind: Panel, path: Option<String>) -> u64 {
        let opened = self.panels.clone().open(scope, kind, path);
        self.panels = opened.panels;
        opened.id
    }
    fn open_panel(&mut self, kind: Panel, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.open_surface(self.panel_scope(), kind, None);
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
        let id = self.open_surface(scope, Panel::Browser, None);
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
        let activation = self.panels.clone().activate(key.clone(), id);
        self.panels = activation.panels;
        if !activation.activated {
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
                        session.host.store.clone(),
                        self.snapshot.pane.navigation.cwd.clone(),
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
                    let Some(owner) = &self.session else {
                        return;
                    };
                    let session = match owner.side(self.snapshot.selected_directory()) {
                        Ok(session) => session,
                        Err(error) => {
                            self.set_error(error);
                            return;
                        }
                    };
                    let chat = cx.new(|cx| {
                        Self::new(
                            Mode::SideChat {
                                remote: self.remote.clone(),
                                session,
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
        self.panels = self.panels.clone().close(self.panel_scope(), id);
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
                let cwd = if self.snapshot.pane.navigation.cwd.is_empty() {
                    self.snapshot
                        .connection
                        .host_home_directory
                        .as_deref()
                        .unwrap_or("/home")
                } else {
                    self.snapshot.pane.navigation.cwd.as_str()
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
    fn initial_host(
        &mut self,
        result: Result<
            (
                agent_core::presentation::connection_list::ConnectionSelection,
                Option<RemoteHost>,
            ),
            String,
        >,
    ) {
        self.connecting = false;
        match result {
            Ok((selection, remote)) => {
                self.connection_selection = selection;
                self.remote = remote;
                self.connect(None, None);
            }
            Err(error) => {
                self.connect(None, None);
                self.set_error(error);
            }
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
        if self.snapshot.connection.connected
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
        if submit && self.snapshot.connection.connected && self.busy == 0 {
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
        let Some(store) = self.session.as_ref().map(|session| session.pane.clone()) else {
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
        self.open_surface(self.panel_scope(), Panel::Files, Some(path));
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
        self.open_surface(self.panel_scope(), Panel::File, Some(path.clone()));
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

fn selected_model<'a>(
    models: &'a [Model],
    selected: Option<&agent_protocol::models::ModelRef>,
) -> Option<&'a Model> {
    models.iter().find(|model| Some(&model.model) == selected)
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
                sessions: Default::default(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let view = cx.add_window(|window, cx| {
            Desktop::new(
                Mode::Fixture {
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
                sessions: Default::default(),
                connections: Arc::new(crate::platform::Connections::default()),
                closing: tokio_util::task::TaskTracker::new(),
                logging_error: None,
            });
        });
        let view = cx.add_window(|window, cx| {
            Desktop::new(
                Mode::Fixture {
                    remote: None,
                    cwd: "/fixture".into(),
                },
                window,
                cx,
            )
        });
        view.update(cx, |view, window, cx| {
            view.epoch = 3;
            Arc::make_mut(&mut view.snapshot).connection.epoch = 5;
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

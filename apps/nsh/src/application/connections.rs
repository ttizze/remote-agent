//! List scope is independent of the concrete destination used by the composer.
use super::*;
use agent_core::presentation::{
    connection_list::{ConnectionSelection, resolve_connection_selection},
    list::ThreadList,
};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ListScope {
    pub host: String,
    pub storage: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListSource {
    pub scope: ListScope,
    pub connected: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum ListRequest {
    Refresh,
    MoreChats,
    MoreProjects,
}

pub(super) fn load_selection(path: &Path) -> Result<ConnectionSelection, AgentError> {
    let bytes = crate::storage::read(path)?;
    if bytes.is_empty() {
        Ok(ConnectionSelection {
            host_id: None,
            all: false,
        })
    } else {
        serde_json::from_slice(&bytes).map_err(crate::storage::error)
    }
}

impl State {
    pub(super) fn selection(&self) -> ConnectionSelection {
        ConnectionSelection {
            host_id: (!self.host.is_empty()).then(|| self.host.clone()),
            all: self.all_hosts,
        }
    }

    pub(super) fn resolve_selection(&mut self) {
        let current = self.selection();
        let selection = resolve_connection_selection(
            self.hosts.iter().map(|host| host.id.clone()).collect(),
            current.host_id,
            current.all,
        );
        self.host = selection.host_id.unwrap_or_default();
        self.all_hosts = selection.all;
    }

    fn list_hosts(&self) -> impl Iterator<Item = &Host> {
        self.hosts
            .iter()
            .filter(|host| self.all_hosts || host.id == self.host)
    }

    fn list_publications(&self) -> impl Iterator<Item = (&str, &Presentation)> {
        self.list_hosts().filter_map(|host| {
            self.publications
                .get(&host.id)
                .map(|value| (host.id.as_str(), &**value))
        })
    }

    pub fn list_sources(&self) -> Vec<ListSource> {
        self.list_publications()
            .map(|(host, value)| ListSource {
                scope: ListScope {
                    host: host.into(),
                    storage: value.snapshot.connection.storage_scope.clone(),
                },
                connected: value.composer.controls.connected,
            })
            .collect()
    }

    pub fn list_inputs(&self) -> Vec<(String, Shared<ThreadList>)> {
        self.list_publications()
            .filter_map(|(host, value)| {
                value
                    .list
                    .as_ref()
                    .map(|list| (host.to_owned(), Shared(list.clone())))
            })
            .collect()
    }

    #[cfg(any(desktop, test))]
    pub fn sidebar_catalog_ready(&self) -> bool {
        self.list_hosts().all(|host| {
            self.publications.get(&host.id).is_some_and(|value| {
                value.composer.controls.connected
                    && value.list.is_some()
                    && value.snapshot.sidebar_catalog_ready()
            })
        })
    }

    pub fn list_scope_matches(&self, scope: &ListScope) -> bool {
        self.list_publications().any(|(host, value)| {
            host == scope.host && value.snapshot.connection.storage_scope == scope.storage
        })
    }
}

pub(super) async fn save_selection(
    path: PathBuf,
    selection: ConnectionSelection,
) -> Result<(), AgentError> {
    tokio::task::spawn_blocking(move || agent_local_host::save_private_json(&path, &selection))
        .await
        .map_err(crate::storage::error)?
        .map_err(crate::storage::error)
}

impl Application {
    pub(super) fn persist_selection(self: &Arc<Self>, mut previous: ConnectionSelection) {
        let path = self.selection_path.clone();
        let mut updates = self.subscribe();
        let owner = Arc::downgrade(self);
        let stop = self.stop.clone();
        self.jobs.spawn_on(
            async move {
                loop {
                    let next = updates.borrow_and_update().selection();
                    if next != previous {
                        previous = next.clone();
                        if let Err(reason) = save_selection(path.clone(), next).await
                            && let Some(owner) = owner.upgrade()
                        {
                            owner.state.send_modify(|state| {
                                Arc::make_mut(state).failure = Some(reason.to_string())
                            });
                        }
                    }
                    if stop.is_cancelled() {
                        break;
                    }
                    tokio::select! {
                        _ = stop.cancelled() => {},
                        changed = updates.changed() => if changed.is_err() { break; },
                    }
                }
            },
            &self.runtime,
        );
    }

    pub fn select_host(&self, host: String) {
        if self.activate_host(&host, false) {
            #[cfg(any(desktop, test))]
            self.select_sidebar_projects(Vec::new());
            self.client.reconnect(host);
        }
    }

    /// A folder picked on this PC inside a WSL distro belongs to that distro's
    /// Host: it becomes the chosen Host, which runs `intent` for the folder's
    /// Linux path once it is reached. Whether the folder was a distro's.
    #[cfg(any(desktop, test))]
    pub fn open_wsl_folder(
        &self,
        path: &Path,
        intent: impl FnOnce(String) -> Intent,
    ) -> Result<bool, String> {
        let Some(folder) = agent_local_host::wsl::folder(&path.to_string_lossy())
            .map_err(|reason| reason.to_string())?
        else {
            return Ok(false);
        };
        let host = agent_local_host::wsl::host_id(&folder.distro);
        if !self.activate_host(&host, true) {
            return Err(crate::l10n::tr_format(
                "{0} が見つかりません。",
                [&agent_local_host::wsl::host_name(&folder.distro)],
            ));
        }
        self.client
            .connect_then(host, Some(intent(folder.linux_path)));
        Ok(true)
    }

    fn activate_host(&self, host: &str, keep_list: bool) -> bool {
        self.state.send_if_modified(|state| {
            if self.stop.is_cancelled()
                || state.account_locked()
                || !state.hosts.iter().any(|candidate| candidate.id == host)
            {
                return false;
            }
            let state = Arc::make_mut(state);
            state.host = host.into();
            if !keep_list {
                state.all_hosts = false;
            }
            state.worktrees = None;
            true
        })
    }

    pub fn select_all_hosts(&self) {
        self.state.send_if_modified(|state| {
            if self.stop.is_cancelled()
                || state.account_locked()
                || state.hosts.is_empty()
                || state.all_hosts
            {
                return false;
            }
            Arc::make_mut(state).all_hosts = true;
            true
        });
    }

    /// A row can belong to another Host without changing the list scope.
    pub fn list_action(&self, scope: &ListScope, intent: Intent, navigate: bool) -> bool {
        if self.stop.is_cancelled() {
            return false;
        }
        let state = self.snapshot();
        // Account work pins its Host, not the conversation within that Host.
        if !state.list_scope_matches(scope)
            || (navigate && state.host != scope.host && !self.activate_host(&scope.host, true))
        {
            return false;
        }
        self.client.dispatch(&scope.host, None, intent);
        true
    }

    pub fn request_list(&self, request: ListRequest) {
        if self.stop.is_cancelled() {
            return;
        }
        let state = self.snapshot();
        for host in state.list_hosts() {
            let intent = match request {
                ListRequest::Refresh => {
                    let Some(value) = state
                        .publications
                        .get(&host.id)
                        .filter(|value| value.composer.controls.connected)
                    else {
                        self.client.reconnect(host.id.clone());
                        continue;
                    };
                    Intent::ListSessions(agent_core::state::operations::ListSessions::new(
                        (*value.snapshot.lists.list_query).clone(),
                    ))
                }
                ListRequest::MoreChats => Intent::ExpandThreadList { project_id: None },
                ListRequest::MoreProjects => Intent::ExpandProjects,
            };
            self.client.dispatch(&host.id, None, intent);
        }
    }

    pub fn search_list(&self, search_term: &str) {
        if self.stop.is_cancelled() {
            return;
        }
        let state = self.snapshot();
        for (host, value) in state.list_publications() {
            if !value.composer.controls.connected
                || value.snapshot.lists.list_query.search_term == search_term
            {
                continue;
            }
            let mut query = (*value.snapshot.lists.list_query).clone();
            query.search_term = search_term.into();
            self.client.dispatch(
                host,
                None,
                Intent::ListSessions(agent_core::state::operations::ListSessions::new(query)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Host as Profile, Storage};
    use agent_core::{
        client::persistence::ClientStorage,
        state::{Event, Snapshot},
    };

    #[test]
    fn sidebar_catalog_waits_for_every_selected_hosts_authoritative_publication() {
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        snapshot.lists.threads = Some(Arc::new(
            serde_json::from_value(serde_json::json!({
                "projects": [], "data": [], "limit":60, "hasMore":false,
                "hasMoreProjects":false, "projectPages":{}
            }))
            .unwrap(),
        ));
        let publication = |snapshot: Snapshot| {
            Shared(Presentation::project(Arc::new(snapshot), "", None).unwrap())
        };
        let mut state = State {
            all_hosts: true,
            host: "one".into(),
            hosts: vec![
                Host {
                    id: "one".into(),
                    name: "One".into(),
                },
                Host {
                    id: "two".into(),
                    name: "Two".into(),
                },
            ],
            ..Default::default()
        };
        state
            .publications
            .insert("one".into(), publication(snapshot.clone()));
        assert!(
            !state.sidebar_catalog_ready(),
            "a missing Host is not a complete catalog"
        );
        state
            .publications
            .insert("two".into(), publication(snapshot.clone()));
        assert!(state.sidebar_catalog_ready());
        let mut offline = snapshot.clone();
        offline.connection.connected = false;
        let mut no_list = snapshot.clone();
        no_list.lists.threads = None;
        let mut searching = snapshot.clone();
        Arc::make_mut(&mut searching.lists.list_query).search_term = "query".into();
        let mut loading = snapshot.clone();
        Arc::make_mut(&mut loading.operations).insert(
            agent_core::state::operations::OperationKey::SessionList,
            agent_core::state::operations::OperationState {
                generation: 1,
                phase: agent_core::state::operations::OperationPhase::Running,
            },
        );
        for pending in [offline, no_list, searching, loading] {
            state
                .publications
                .insert("two".into(), publication(pending));
            assert!(!state.sidebar_catalog_ready());
        }
        state.all_hosts = false;
        assert!(
            state.sidebar_catalog_ready(),
            "a Host outside the list scope does not block it"
        );
    }

    fn profiles(root: &Path) -> Vec<Profile> {
        let profiles: Vec<_> = ["one", "two"]
            .into_iter()
            .map(|id| Profile {
                id: id.into(),
                name: "Same machine".into(),
                ticket: "offline".into(),
                local_directory: None,
            })
            .collect();
        for host in &profiles {
            let mut snapshot = Snapshot::default();
            snapshot.connection.connection_disabled = true;
            snapshot.connection.storage_scope = format!("store-{}", host.id);
            Storage {
                root: root.into(),
                host: host.clone(),
                handed: Default::default(),
            }
            .save(agent_core::persistence::encode(&snapshot).unwrap())
            .unwrap();
        }
        agent_local_host::save_private_json(&root.join("hosts.json"), &profiles).unwrap();
        profiles
    }

    async fn until(app: &Application, ready: impl Fn(&State) -> bool) -> Arc<State> {
        let mut updates = app.subscribe();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let state = updates.borrow_and_update().clone();
                if ready(&state) {
                    return state;
                }
                updates.changed().await.unwrap();
            }
        })
        .await
        .expect("application publication stalled")
    }

    async fn saved_selection(path: &Path, expected: &ConnectionSelection) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let value = tokio::fs::read(path)
                    .await
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<ConnectionSelection>(&bytes).ok());
                if value.as_ref() == Some(expected) {
                    break;
                }
            }
        })
        .await
        .expect("connection selection was not saved while the app was open");
    }

    fn scope(state: &State, host: &str) -> ListScope {
        ListScope {
            host: host.into(),
            storage: state.publications[host]
                .snapshot
                .connection
                .storage_scope
                .clone(),
        }
    }

    fn publish_connected(app: &Application) {
        for host in ["one", "two"] {
            let before = app.snapshot().publications[host].0.clone();
            let mut snapshot = (*before.snapshot).clone();
            snapshot.connection.connected = true;
            let value = Presentation::project(Arc::new(snapshot), "", Some(&before)).unwrap();
            app.state.send_modify(|state| {
                Arc::make_mut(state).receive(Update::Presentation {
                    host: host.into(),
                    value: value.clone(),
                })
            });
        }
    }

    #[test]
    fn an_empty_connection_list_cannot_select_an_unknown_destination_or_all_scope() {
        let root = tempfile::tempdir().unwrap();
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        app.select_all_hosts();
        app.select_host("missing".into());
        assert!(app.snapshot().host.is_empty());
        assert!(!app.snapshot().all_hosts);
        runtime.block_on(app.shutdown()).unwrap();
    }

    #[test]
    fn list_search_refresh_and_paging_apply_to_the_listed_hosts_and_keep_drafts() {
        let root = tempfile::tempdir().unwrap();
        profiles(root.path());
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        runtime.block_on(async {
            let initial = until(&app, |state| state.publications.len() == 2).await;
            let chat_limit = initial.publications["one"].snapshot.lists.list_query.limit;
            let project_limit = initial.publications["one"]
                .snapshot
                .lists
                .list_query
                .project_limit;
            app.select_all_hosts();
            app.request_list(ListRequest::MoreChats);
            let paged = until(&app, |state| {
                ["one", "two"].iter().all(|host| {
                    state.publications[*host].snapshot.lists.list_query.limit > chat_limit
                })
            })
            .await;
            assert_eq!(
                paged.publications["one"]
                    .snapshot
                    .lists
                    .list_query
                    .project_limit,
                project_limit
            );
            assert_eq!(
                paged.publications["two"]
                    .snapshot
                    .lists
                    .list_query
                    .project_limit,
                project_limit
            );
            for host in ["one", "two"] {
                assert!(app.list_action(
                    &scope(&paged, host),
                    Intent::NewChat {
                        cwd: format!("/{host}")
                    },
                    true
                ));
            }
            until(&app, |state| {
                ["one", "two"].iter().all(|host| {
                    state.publications[*host].context.selected_directory() == format!("/{host}")
                })
            })
            .await;
            // The transport can disconnect before the UI receives its next
            // publication. A failed refresh must still keep both drafts open.
            publish_connected(&app);
            app.request_list(ListRequest::Refresh);
            let refreshed = until(&app, |state| {
                ["one", "two"]
                    .iter()
                    .all(|host| !state.publications[*host].composer.controls.connected)
            })
            .await;
            for host in ["one", "two"] {
                assert_eq!(
                    refreshed.publications[host].context.selected_directory(),
                    format!("/{host}")
                );
            }
            publish_connected(&app);
            app.search_list("日本語の検索");
            until(&app, |state| {
                ["one", "two"].iter().all(|host| {
                    state.publications[*host]
                        .snapshot
                        .lists
                        .list_query
                        .search_term
                        == "日本語の検索"
                })
            })
            .await;
            app.select_host("one".into());
            publish_connected(&app);
            app.search_list("only one");
            let filtered = until(&app, |state| {
                state.publications["one"]
                    .snapshot
                    .lists
                    .list_query
                    .search_term
                    == "only one"
            })
            .await;
            assert_eq!(
                filtered.publications["two"]
                    .snapshot
                    .lists
                    .list_query
                    .search_term,
                "日本語の検索"
            );
            app.request_list(ListRequest::MoreProjects);
            let expanded = until(&app, |state| {
                state.publications["one"]
                    .snapshot
                    .lists
                    .list_query
                    .project_limit
                    > project_limit
            })
            .await;
            assert_eq!(
                expanded.publications["two"]
                    .snapshot
                    .lists
                    .list_query
                    .project_limit,
                project_limit
            );
            app.shutdown().await.unwrap();
        });
    }

    #[test]
    fn list_projection_keeps_offline_hosts_and_reuses_lists_for_composer_only_changes() {
        let mut state = State {
            host: "one".into(),
            all_hosts: true,
            ..Default::default()
        };
        for id in ["one", "two"] {
            state.hosts.push(Host {
                id: id.into(),
                name: "Same machine".into(),
            });
            let mut snapshot = Snapshot::default();
            snapshot.connection.storage_scope = format!("store-{id}");
            snapshot.lists.threads = Some(Arc::new(serde_json::from_value(serde_json::json!({
                "data":[{"id":{"provider":"codex","id":"same-task"},"name":"Same task","projectId":"same-project"}],
                "projects":[{"id":"same-project","name":"Same project","roots":[]}],
                "hasMore":false,"hasMoreProjects":false,"projectPages":{},"limit":5
            })).unwrap()));
            state.publications.insert(
                id.into(),
                Shared(Presentation::project(Arc::new(snapshot), "", None).unwrap()),
            );
        }
        let sources = state.list_sources();
        assert_eq!(
            sources
                .iter()
                .map(|source| source.scope.host.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert!(sources.iter().all(|source| !source.connected));
        let inputs = state.list_inputs();
        let combined = agent_core::presentation::connection_list::combine_connection_lists(
            inputs
                .iter()
                .map(|(host, value)| (host.clone(), (**value).clone())),
        )
        .unwrap();
        assert_eq!(
            combined
                .projects
                .iter()
                .map(|project| project.host_id.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(combined.projects[0].id, combined.projects[1].id);
        let previous = state.publications["one"].0.clone();
        let (next, _) = agent_core::state::reduce(
            &previous.snapshot,
            "",
            Event::Intent(Intent::SetDraftText {
                thread_id: previous.context.navigation().draft_key,
                text: "typed".into(),
            }),
        );
        state.receive(Update::Presentation {
            host: "one".into(),
            value: Presentation::project(Arc::new(next), "", Some(&previous)).unwrap(),
        });
        assert!(state.list_inputs() == inputs);
        state.receive(Update::Hosts(vec![Profile {
            id: "one".into(),
            name: "Same machine".into(),
            ticket: "offline".into(),
            local_directory: None,
        }]));
        assert_eq!(state.list_inputs().len(), 1);
        assert!(!state.list_scope_matches(&sources[1].scope));
        state.receive(Update::Hosts(Vec::new()));
        assert!(state.list_inputs().is_empty());
        assert!(!state.all_hosts);
    }

    #[test]
    fn list_operations_reach_the_owner_and_navigation_keeps_the_list_scope() {
        let root = tempfile::tempdir().unwrap();
        profiles(root.path());
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        runtime.block_on(async {
            let state = until(&app, |state| state.publications.len() == 2).await;
            let one = scope(&state, "one");
            let two = scope(&state, "two");
            let draft = state.publications["two"].context.navigation().draft_key;
            let edit = |text: &str| Intent::SetDraftText {
                thread_id: draft.clone(),
                text: text.into(),
            };
            assert!(!app.list_action(&two, edit("hidden Host"), false));
            app.select_all_hosts();
            assert!(app.list_action(&two, edit("for two"), false));
            let state = until(&app, |state| {
                state.publications["two"].composer.draft.text == "for two"
            })
            .await;
            assert!(state.publications["one"].composer.draft.text.is_empty());
            assert_eq!(state.host, "one");
            assert!(app.list_action(&two, Intent::NewChat { cwd: "/two".into() }, true));
            let state = until(&app, |state| {
                state.publications["two"].context.selected_directory() == "/two"
            })
            .await;
            assert_eq!(state.host, "two");
            assert!(state.all_hosts);
            assert!(
                state.publications["one"]
                    .context
                    .selected_directory()
                    .is_empty()
            );
            let stale = ListScope {
                storage: "old-account".into(),
                ..two.clone()
            };
            assert!(!app.list_action(&stale, edit("stale"), false));
            app.select_host("missing".into());
            assert_eq!(app.snapshot().host, "two");
            assert!(app.snapshot().all_hosts);
            app.select_host("one".into());
            assert!(!app.snapshot().all_hosts);
            assert!(!app.list_action(&two, edit("wrong Host"), false));
            app.state
                .send_modify(|state| Arc::make_mut(state).account_pending = true);
            app.select_all_hosts();
            app.select_host("two".into());
            assert!(!app.snapshot().all_hosts);
            assert_eq!(app.snapshot().host, "one");
            app.state
                .send_modify(|state| Arc::make_mut(state).account_pending = false);
            app.shutdown().await.unwrap();
            app.select_all_hosts();
            app.select_host("two".into());
            assert_eq!(app.snapshot().host, "one");
            assert!(!app.snapshot().all_hosts);
            assert!(!app.list_action(&one, edit("closed"), false));
        });
    }

    #[test]
    fn account_work_allows_task_switching_on_its_host_and_preserves_drafts() {
        use agent_core::state::operations::ReadThread;
        use agent_protocol::session::{ProviderKind, SessionRef};

        let root = tempfile::tempdir().unwrap();
        profiles(root.path());
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        runtime.block_on(async {
            until(&app, |state| state.publications.len() == 2).await;
            app.select_all_hosts();
            let state = app.snapshot();
            let one = scope(&state, "one");
            let two = scope(&state, "two");
            app.state
                .send_modify(|state| Arc::make_mut(state).account_pending = true);

            for (id, expected_draft) in
                [("first", ""), ("second", ""), ("first", "draft for first")]
            {
                let session = SessionRef::new(ProviderKind::Codex, id.into()).unwrap();
                assert!(app.list_action(
                    &one,
                    Intent::ReadThread(ReadThread::open(session.clone())),
                    true
                ));
                let state = until(&app, |state| {
                    state.publications["one"]
                        .context
                        .navigation()
                        .thread_id
                        .as_ref()
                        == Some(&session)
                })
                .await;
                assert_eq!(state.host, "one");
                assert!(state.all_hosts);
                assert!(state.account_pending);
                assert_eq!(
                    state.publications["one"].composer.draft.text,
                    expected_draft
                );
                assert!(app.list_action(
                    &one,
                    Intent::SetDraftText {
                        thread_id: session.clone().into(),
                        text: format!("draft for {id}"),
                    },
                    false
                ));
                until(&app, |state| {
                    state.publications["one"].composer.draft.text == format!("draft for {id}")
                })
                .await;
                assert!(!app.list_action(
                    &two,
                    Intent::ReadThread(ReadThread::open(session)),
                    true
                ));
            }
            app.state
                .send_modify(|state| Arc::make_mut(state).account_pending = false);
            assert!(app.list_action(&two, Intent::NewChat { cwd: "/two".into() }, true));
            until(&app, |state| {
                state.host == "two"
                    && state.publications["two"].context.selected_directory() == "/two"
            })
            .await;
            assert_eq!(
                app.snapshot().publications["one"].composer.draft.text,
                "draft for first"
            );
            app.shutdown().await.unwrap();
        });
    }

    #[test]
    fn shutdown_flushes_the_latest_selection_and_removed_hosts_restore_to_an_available_host() {
        let root = tempfile::tempdir().unwrap();
        let profiles = profiles(root.path());
        let path = root.path().join("connection-selection.json");
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        runtime.block_on(async {
            until(&app, |state| state.publications.len() == 2).await;
            app.select_all_hosts();
            saved_selection(
                &path,
                &ConnectionSelection {
                    host_id: Some("one".into()),
                    all: true,
                },
            )
            .await;
            app.select_host("two".into());
            saved_selection(
                &path,
                &ConnectionSelection {
                    host_id: Some("two".into()),
                    all: false,
                },
            )
            .await;
            app.select_host("one".into());
            saved_selection(
                &path,
                &ConnectionSelection {
                    host_id: Some("one".into()),
                    all: false,
                },
            )
            .await;
            for index in 0..64 {
                app.select_host(if index % 2 == 0 { "one" } else { "two" }.into());
            }
            app.select_all_hosts();
            app.shutdown().await.unwrap();
        });
        assert_eq!(
            load_selection(&path).unwrap(),
            ConnectionSelection {
                host_id: Some("two".into()),
                all: true
            }
        );
        let (restored_runtime, restored) = Application::new(root.path().into(), None).unwrap();
        restored_runtime.block_on(async {
            let state = until(&restored, |state| state.publications.len() == 2).await;
            assert_eq!(state.host, "two");
            assert!(state.all_hosts);
            restored.shutdown().await.unwrap();
        });
        agent_local_host::save_private_json(&root.path().join("hosts.json"), &&profiles[..1])
            .unwrap();
        let (fallback_runtime, fallback) = Application::new(root.path().into(), None).unwrap();
        fallback_runtime.block_on(async {
            let state = until(&fallback, |state| state.publications.len() == 1).await;
            assert_eq!(state.host, "one");
            assert!(state.all_hosts);
            fallback.shutdown().await.unwrap();
        });
        assert_eq!(
            load_selection(&path).unwrap().host_id.as_deref(),
            Some("one")
        );
    }

    #[test]
    fn a_failed_selection_save_is_reported_and_the_next_change_can_recover() {
        let root = tempfile::tempdir().unwrap();
        profiles(root.path());
        let path = root.path().join("connection-selection.json");
        agent_local_host::save_private_json(
            &path,
            &ConnectionSelection {
                host_id: Some("one".into()),
                all: false,
            },
        )
        .unwrap();
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        runtime.block_on(async {
            until(&app, |state| state.publications.len() == 2).await;
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            app.select_all_hosts();
            until(&app, |state| state.failure.is_some()).await;
            std::fs::remove_dir(&path).unwrap();
            app.dismiss_failure();
            app.select_host("two".into());
            app.shutdown().await.unwrap();
        });
        assert_eq!(
            load_selection(&path).unwrap(),
            ConnectionSelection {
                host_id: Some("two".into()),
                all: false
            }
        );
    }

    #[test]
    fn a_failed_final_selection_save_still_closes_clients_and_keeps_accepted_input() {
        let root = tempfile::tempdir().unwrap();
        profiles(root.path());
        let path = root.path().join("connection-selection.json");
        agent_local_host::save_private_json(
            &path,
            &ConnectionSelection {
                host_id: Some("one".into()),
                all: false,
            },
        )
        .unwrap();
        let (runtime, app) = Application::new(root.path().into(), None).unwrap();
        runtime.block_on(async {
            let state = until(&app, |state| state.publications.len() == 2).await;
            let draft = state.publications["one"].context.navigation().draft_key;
            app.client
                .request(
                    "one",
                    None,
                    Intent::SetDraftText {
                        thread_id: draft.clone(),
                        text: "keep this draft".into(),
                    },
                )
                .recv()
                .await
                .unwrap()
                .unwrap();
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert!(app.shutdown().await.is_err());
            let late = app.client.request(
                "one",
                None,
                Intent::SetDraftText {
                    thread_id: draft,
                    text: "must be rejected".into(),
                },
            );
            assert!(late.recv().await.unwrap().is_err());
        });
        std::fs::remove_dir(&path).unwrap();
        let (restored_runtime, restored) = Application::new(root.path().into(), None).unwrap();
        restored_runtime.block_on(async {
            until(&restored, |state| {
                state
                    .publications
                    .get("one")
                    .is_some_and(|value| value.composer.draft.text == "keep this draft")
            })
            .await;
            restored.shutdown().await.unwrap();
        });
    }
}

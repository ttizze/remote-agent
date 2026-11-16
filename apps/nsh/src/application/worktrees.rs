//! The application owns Host requests and editor lifetime across native roots.
use super::Application;
use crate::l10n::tr;
use agent_core::{
    panels::PanelKind,
    presentation::worktrees::{normalize_worktree_settings, worktree_removal_reason},
    state::{Intent, operations as op},
};
use agent_protocol::{models::WorktreeSettings, session::SessionRef};
use std::sync::Arc;

#[derive(Clone, PartialEq, serde::Serialize)]
pub(crate) struct WorktreeEditor {
    pub id: String,
    pub host: String,
    pub settings: Option<WorktreeSettings>,
    pub pending: bool,
    pub error: Option<String>,
    pub receipt: Option<SaveReceipt>,
}

#[derive(Clone, PartialEq, serde::Serialize)]
pub(crate) struct SaveReceipt {
    pub id: String,
    pub succeeded: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum WorktreeAction {
    Load {},
    Close {},
    Host {
        id: String,
    },
    Save {
        id: String,
        settings: WorktreeSettings,
    },
    Remove {
        path: String,
    },
    Review {
        path: String,
    },
    Open {
        thread: SessionRef,
    },
}

impl Application {
    pub fn open_worktrees(self: &Arc<Self>) {
        let mut id = None;
        self.state.send_if_modified(|state| {
            if state.host.is_empty()
                || state
                    .worktrees
                    .as_ref()
                    .is_some_and(|editor| editor.pending)
            {
                return false;
            }
            let editor = WorktreeEditor {
                id: uuid::Uuid::new_v4().to_string(),
                host: state.host.clone(),
                settings: None,
                pending: false,
                error: None,
                receipt: None,
            };
            id = Some(editor.id.clone());
            Arc::make_mut(state).worktrees = Some(editor);
            true
        });
        if let Some(id) = id {
            self.worktree_action(&id, WorktreeAction::Load {});
        }
    }

    /// Returns true only when a registered conversation was opened.
    pub fn worktree_action(self: &Arc<Self>, token: &str, action: WorktreeAction) -> bool {
        let state = self.snapshot();
        let Some(editor) = state
            .worktrees
            .as_ref()
            .filter(|editor| editor.id == token && editor.host == state.host)
        else {
            return false;
        };
        let publication = state.publications.get(&editor.host);
        let rows = publication
            .and_then(|p| p.snapshot.files.workspace.worktrees.as_deref())
            .map(Vec::as_slice)
            .unwrap_or_default();
        match action {
            // Closing the settings page must not wait for a Host read to
            // finish. The request job checks the editor id before publishing
            // its result, so dropping the editor safely discards a late
            // response.
            WorktreeAction::Close {} => {
                self.state
                    .send_modify(|state| Arc::make_mut(state).worktrees = None);
            }
            _ if editor.pending => return false,
            WorktreeAction::Host { id } => {
                if state.hosts.iter().any(|host| host.id == id) {
                    self.select_host(id);
                    self.open_worktrees();
                }
            }
            WorktreeAction::Load {} => {
                self.worktree_request(
                    editor,
                    Intent::ReadWorktreeSettings(op::ReadWorktreeSettings {}),
                    None,
                );
            }
            WorktreeAction::Save { id, settings } if editor.settings.is_some() => {
                self.worktree_request(
                    editor,
                    Intent::UpdateWorktreeSettings(op::UpdateWorktreeSettings {
                        settings: normalize_worktree_settings(settings),
                    }),
                    Some(id),
                );
            }
            WorktreeAction::Remove { path } => {
                let registered = rows.iter().find(|row| row.path == path);
                if registered.is_some_and(|row| {
                    worktree_removal_reason(
                        row.blocked_reason.clone(),
                        publication
                            .map(|p| p.context.selected_directory())
                            .unwrap_or_default(),
                        row.path.clone(),
                    )
                    .is_none()
                }) {
                    self.worktree_request(
                        editor,
                        Intent::RemoveWorktree(op::RemoveWorktree { path }),
                        None,
                    );
                }
            }
            WorktreeAction::Review { path } => {
                if let Some(publication) =
                    publication.filter(|_| rows.iter().any(|row| row.path == path))
                {
                    let scope = publication.snapshot.panel_scope(
                        editor.host.clone(),
                        publication.context.navigation().draft_key,
                    );
                    self.open_panel(scope, PanelKind::Diff, Some(path), None);
                    self.state.send_modify(|state| {
                        let state = Arc::make_mut(state);
                        state.worktrees = None;
                        state.settings = false;
                    });
                }
            }
            WorktreeAction::Open { thread } => {
                if rows
                    .iter()
                    .flat_map(|row| &row.threads)
                    .any(|row| row.id == thread)
                {
                    self.client.dispatch(
                        &editor.host,
                        None,
                        Intent::ReadThread(op::ReadThread::open(thread)),
                    );
                    self.state.send_modify(|state| {
                        let state = Arc::make_mut(state);
                        state.worktrees = None;
                        state.settings = false;
                    });
                    return true;
                }
            }
            WorktreeAction::Save { .. } => {}
        }
        false
    }

    fn worktree_request(
        self: &Arc<Self>,
        editor: &WorktreeEditor,
        intent: Intent,
        receipt: Option<String>,
    ) {
        if self.stop.is_cancelled() {
            return;
        }
        let id = editor.id.clone();
        let host = editor.host.clone();
        let accepted = self.state.send_if_modified(|state| {
            if state.host != host {
                return false;
            }
            let Some(editor) = &mut Arc::make_mut(state).worktrees else {
                return false;
            };
            if editor.id != id || editor.host != host || editor.pending {
                return false;
            }
            editor.pending = true;
            editor.error = None;
            true
        });
        if !accepted {
            return;
        }
        let refresh = matches!(&intent, Intent::ReadWorktreeSettings(_));
        let response = self.client.request(&host, None, intent);
        let listing = refresh.then(|| {
            self.client
                .request(&host, None, Intent::ListWorktrees(op::ListWorktrees {}))
        });
        let owner = Arc::downgrade(self);
        let stop = self.stop.clone();
        self.jobs.spawn_on(
            async move {
                let mut failure = None;
                for response in std::iter::once(response).chain(listing) {
                    let result = tokio::select! {
                        _ = stop.cancelled() => return,
                        result = response.recv() => result,
                    };
                    let error = match result {
                        Ok(Ok(_)) => None,
                        Ok(Err(reason)) => Some(
                            agent_core::presentation::error::error_text(
                                crate::l10n::language(),
                                &reason.to_string(),
                            )
                            .to_string(),
                        ),
                        Err(_) => Some(tr("設定の応答が無効です。").into()),
                    };
                    failure = failure.or(error);
                }
                let Some(owner) = owner.upgrade() else { return };
                let settings = owner.client.worktree_settings(&host);
                let failure = failure.or_else(|| {
                    settings
                        .is_none()
                        .then(|| tr("設定の応答が無効です。").into())
                });
                owner.state.send_if_modified(|state| {
                    let Some(editor) = &mut Arc::make_mut(state).worktrees else {
                        return false;
                    };
                    if editor.id != id || editor.host != host {
                        return false;
                    }
                    editor.pending = false;
                    if let Some(id) = receipt {
                        editor.receipt = Some(SaveReceipt {
                            id,
                            succeeded: failure.is_none(),
                        });
                    }
                    if let Some(settings) = settings {
                        editor.settings = Some(settings);
                    }
                    editor.error = failure;
                    true
                });
            },
            &self.runtime,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::{Host, State},
        shared::Shared,
    };
    use agent_core::presentation::view::Presentation;

    fn isolated_application(
        root: std::path::PathBuf,
    ) -> (tokio::runtime::Runtime, Arc<Application>) {
        // These tests publish explicit Host facts. Discovery must not race those facts.
        let selection_path = root.join("connection-selection.json");
        let explorer_width_path = root.join("file-explorer-width.json");
        let sidebar_projects_path = root.join("sidebar-projects.json");
        let file_rendering_path = root.join("file-rendering.json");
        let (runtime, client, _) = crate::client::Client::new(root, None).unwrap();
        let (state, _) = tokio::sync::watch::channel(Arc::new(State::default()));
        let app = Arc::new(Application {
            client,
            instance: uuid::Uuid::new_v4(),
            state,
            stop: tokio_util::sync::CancellationToken::new(),
            runtime: runtime.handle().clone(),
            jobs: Default::default(),
            selection_path,
            #[cfg(desktop)]
            onboarding_path: std::path::PathBuf::new(),
            explorer_width_path,
            file_rendering_path,
            sidebar_projects_path,
            #[cfg(desktop)]
            updater: None,
        });
        (runtime, app)
    }

    async fn idle_editor(mut updates: tokio::sync::watch::Receiver<Arc<State>>) {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while updates
                .borrow_and_update()
                .worktrees
                .as_ref()
                .unwrap()
                .pending
            {
                updates.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[test]
    fn account_pending_worktree_request_can_close_before_late_response() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, mut app) = isolated_application(directory.path().into());
        // Publish responses only when driven below, so Close deterministically
        // precedes the missing Host's response regardless of worker scheduling.
        let responses = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Arc::get_mut(&mut app).unwrap().runtime = responses.handle().clone();
        app.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.host = "missing".into();
            state.account_pending = true;
        });
        assert!(app.snapshot().account_locked());

        app.open_worktrees();
        let editor = app
            .snapshot()
            .worktrees
            .clone()
            .expect("account-pending read should create an editor");
        assert!(
            editor.pending,
            "opening the editor should start its request"
        );

        app.worktree_action(&editor.id, WorktreeAction::Close {});
        assert!(app.snapshot().worktrees.is_none());

        // The missing Host completes the request asynchronously after Close.
        // Its response must be ignored rather than recreating the editor.
        app.jobs.close();
        responses.block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(5), app.jobs.wait())
                .await
                .unwrap();
        });
        assert!(app.snapshot().worktrees.is_none());
        runtime.block_on(app.shutdown()).unwrap();
    }

    #[test]
    fn managed_actions_preserve_current_worktrees_and_open_only_registered_resources() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, app) = isolated_application(directory.path().into());
        let thread =
            SessionRef::new(agent_protocol::session::ProviderKind::Codex, "chat".into()).unwrap();
        let mut snapshot = agent_core::state::Snapshot::default();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).cwd =
            "/managed/tree/src".into();
        Arc::make_mut(&mut snapshot.files.workspace).worktrees =
            Some(Arc::new(vec![agent_protocol::models::Worktree {
                path: "/managed/tree".into(),
                project_path: "/project".into(),
                branch: "session".into(),
                blocked_reason: None,
                threads: vec![agent_protocol::models::WorktreeThread {
                    id: thread.clone(),
                    name: "Work".into(),
                    active: false,
                }],
            }]));
        let p = Presentation::project(Arc::new(snapshot.clone()), "", None).unwrap();
        let scope = p
            .snapshot
            .panel_scope("host".into(), p.context.navigation().draft_key);
        let editor = WorktreeEditor {
            id: "editor".into(),
            host: "host".into(),
            settings: Some(Default::default()),
            pending: false,
            error: None,
            receipt: None,
        };
        app.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.host = "host".into();
            state.settings = true;
            state.publications.insert("host".into(), Shared(p));
            state.worktrees = Some(editor.clone());
        });
        let unchanged = app.subscribe();
        app.worktree_action(
            "editor",
            WorktreeAction::Remove {
                path: "/managed/tree".into(),
            },
        );
        assert!(!unchanged.has_changed().unwrap());
        app.worktree_action(
            "editor",
            WorktreeAction::Review {
                path: "/other".into(),
            },
        );
        assert!(app.snapshot().panels.get(scope.clone()).surfaces.is_empty());
        assert!(!unchanged.has_changed().unwrap());
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).navigation).cwd =
            "/another".into();
        app.state.send_modify(|state| {
            Arc::make_mut(state).publications.insert(
                "host".into(),
                Shared(Presentation::project(Arc::new(snapshot), "", None).unwrap()),
            );
        });
        let removal = app.subscribe();
        app.worktree_action(
            "editor",
            WorktreeAction::Remove {
                path: "/managed/tree".into(),
            },
        );
        assert!(
            removal.has_changed().unwrap(),
            "An available registered checkout must reach the Host request"
        );
        runtime.block_on(idle_editor(app.subscribe()));
        assert!(app.snapshot().worktrees.as_ref().unwrap().error.is_some());

        app.worktree_action(
            "editor",
            WorktreeAction::Review {
                path: "/managed/tree".into(),
            },
        );
        let reviewed = app.snapshot();
        assert!(!reviewed.settings);
        assert!(reviewed.worktrees.is_none());
        let panels = reviewed.panels.get(scope);
        assert!(panels.open);
        assert!(
            panels
                .surfaces
                .iter()
                .any(|panel| panel.kind == PanelKind::Diff
                    && panel.path.as_deref() == Some("/managed/tree"))
        );
        app.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.settings = true;
            state.worktrees = Some(editor);
        });
        let other = SessionRef::new(thread.provider, "unregistered".into()).unwrap();
        assert!(!app.worktree_action("editor", WorktreeAction::Open { thread: other }));
        assert!(app.snapshot().worktrees.is_some());
        assert!(app.worktree_action("editor", WorktreeAction::Open { thread }));
        assert!(!app.snapshot().settings);
        assert!(app.snapshot().worktrees.is_none());
        runtime.block_on(app.shutdown()).unwrap();
    }

    #[test]
    fn worktree_requests_reject_stale_views_and_keep_saved_values_after_a_failed_save() {
        let directory = tempfile::tempdir().unwrap();
        let (runtime, app) = isolated_application(directory.path().into());
        let saved = WorktreeSettings {
            worktree_directory: "/saved".into(),
            ..Default::default()
        };
        app.state.send_modify(|state| {
            let state = Arc::make_mut(state);
            state.host = "missing".into();
            state.hosts = vec![Host {
                id: "missing".into(),
                name: "Host".into(),
            }];
            state.publications.insert(
                "missing".into(),
                Shared(Presentation::empty(
                    agent_core::localization::UiLanguage::default(),
                )),
            );
            state.worktrees = Some(WorktreeEditor {
                id: "editor".into(),
                host: "missing".into(),
                settings: Some(saved.clone()),
                pending: true,
                error: None,
                receipt: None,
            });
        });
        app.worktree_action("editor", WorktreeAction::Close {});
        assert!(app.snapshot().worktrees.is_none());
        app.state.send_modify(|state| {
            Arc::make_mut(state).worktrees = Some(WorktreeEditor {
                id: "editor".into(),
                host: "missing".into(),
                settings: Some(saved.clone()),
                pending: false,
                error: None,
                receipt: None,
            })
        });
        app.worktree_action("old-editor", WorktreeAction::Close {});
        app.worktree_action(
            "editor",
            WorktreeAction::Remove {
                path: "/unknown".into(),
            },
        );
        assert!(!app.snapshot().worktrees.as_ref().unwrap().pending);
        app.state
            .send_modify(|state| Arc::make_mut(state).worktrees.as_mut().unwrap().settings = None);
        let unloaded = app.subscribe();
        app.worktree_action(
            "editor",
            WorktreeAction::Save {
                id: "unloaded".into(),
                settings: saved.clone(),
            },
        );
        assert!(!unloaded.has_changed().unwrap());
        app.worktree_action(
            "editor",
            WorktreeAction::Host {
                id: "unregistered".into(),
            },
        );
        assert!(!unloaded.has_changed().unwrap());
        app.state.send_modify(|state| {
            Arc::make_mut(state).worktrees.as_mut().unwrap().settings = Some(saved.clone())
        });
        app.worktree_action(
            "editor",
            WorktreeAction::Save {
                id: "request".into(),
                settings: WorktreeSettings {
                    copy_paths: vec!["../invalid".into()],
                    ..saved.clone()
                },
            },
        );
        runtime.block_on(idle_editor(app.subscribe()));
        let snapshot = app.snapshot();
        let editor = snapshot.worktrees.as_ref().unwrap();
        assert_eq!(editor.settings.as_ref(), Some(&saved));
        assert!(editor.error.is_some());
        let receipt = editor.receipt.as_ref().unwrap();
        assert_eq!(receipt.id, "request");
        assert!(!receipt.succeeded);
        app.worktree_action(
            "editor",
            WorktreeAction::Host {
                id: "missing".into(),
            },
        );
        runtime.block_on(idle_editor(app.subscribe()));
        let reopened = app.snapshot().worktrees.as_ref().unwrap().id.clone();
        assert_ne!(reopened, "editor");
        assert_eq!(app.snapshot().host, "missing");
        assert!(
            app.snapshot()
                .worktrees
                .as_ref()
                .unwrap()
                .settings
                .is_none()
        );
        app.worktree_action("editor", WorktreeAction::Close {});
        assert!(app.snapshot().worktrees.is_some());
        app.worktree_action(&reopened, WorktreeAction::Close {});
        app.worktree_action("editor", WorktreeAction::Load {});
        assert!(app.snapshot().worktrees.is_none());
        runtime.block_on(app.shutdown()).unwrap();
    }
}

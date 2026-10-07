//! Conversation settings, project scripts and importing agent sessions.
use super::{
    Outcome,
    intents::{Next, invalid},
    owner::{Event, Owner, Waiter, now_ms},
};
use crate::{
    peer::PeerError,
    protocol::Call,
    view::{
        projects::import::{
            default_import_selection, resolve_import_project_id, set_import_selection,
        },
        settings::{
            ConversationSettingChange, ProjectSettingKey, SettingsScope, clear_project_overrides,
            plan_conversation_settings_update,
        },
    },
};
use agent_protocol::{conversation as c, models as m, operations as op};

/// What importing one selected folder did.
pub(super) type ImportStep = (String, Result<(String, c::ImportCounts), PeerError>);

impl Owner {
    fn host_settings(&self) -> Result<&m::ConversationSettings, PeerError> {
        self.state
            .conversation_settings
            .as_ref()
            .ok_or_else(|| invalid("Settings are loading"))
    }

    pub(super) fn update_conversation_settings(
        &mut self,
        scope: &SettingsScope,
        change: &ConversationSettingChange,
    ) -> Result<Next, PeerError> {
        Ok(
            match plan_conversation_settings_update(self.host_settings()?, scope, change) {
                Some(next) => Next::call(Call::UpdateConversationSettings(next), None),
                None => Next::Done,
            },
        )
    }

    pub(super) fn reset_project_settings(&mut self, project_id: &str) -> Result<Next, PeerError> {
        let next = clear_project_overrides(
            self.host_settings()?,
            project_id,
            &[
                ProjectSettingKey::AutoSettle,
                ProjectSettingKey::ContinueAfterRestart,
            ],
        );
        Ok(Next::call(Call::UpdateConversationSettings(next), None))
    }

    pub(super) fn update_project_scripts(
        &mut self,
        project_id: String,
        scripts: Vec<m::ProjectScript>,
    ) -> Next {
        Next::call(
            Call::UpdateProject(op::UpdateProject {
                project_id,
                scripts: Some(scripts),
                favicon_path: None,
            }),
            None,
        )
    }

    /// Saves the project's icon file, or `None` to find it automatically.
    pub(super) fn set_project_icon(&mut self, project_id: String, path: Option<String>) -> Next {
        Next::call(
            Call::UpdateProject(op::UpdateProject {
                project_id,
                scripts: None,
                favicon_path: Some(path),
            }),
            None,
        )
    }

    /// Asks the Host for the icon of every listed project whose saved icon
    /// or update time changed since its icon was read.
    pub(super) fn refresh_project_icons(&mut self) {
        if !self.connected() {
            return;
        }
        let wanted: Vec<(String, String)> = self
            .state
            .shell_projects()
            .iter()
            .map(|project| (project.id.clone(), project_icon_version(project)))
            .filter(|(id, version)| {
                self.state
                    .project_icons
                    .get(id)
                    .is_none_or(|icon| &icon.version != version && !icon.in_flight)
            })
            .collect();
        for (project_id, version) in wanted {
            let entry = self
                .state
                .project_icons
                .entry(project_id.clone())
                .or_default();
            entry.in_flight = true;
            entry.version = version;
            let known_hash = entry.icon.as_ref().map(|icon| icon.hash.clone());
            self.job(
                Call::ProjectFavicon(m::ReadProjectFavicon {
                    project_id,
                    known_hash,
                }),
                None,
                None,
            );
        }
    }

    pub(super) fn project_icon_read(
        &mut self,
        request: &m::ReadProjectFavicon,
        result: Result<Option<m::ProjectFavicon>, ()>,
    ) {
        let Some(entry) = self.state.project_icons.get_mut(&request.project_id) else {
            return;
        };
        entry.in_flight = false;
        match result {
            Ok(None) => entry.icon = None,
            Ok(Some(favicon)) => {
                let unchanged = entry
                    .icon
                    .as_ref()
                    .is_some_and(|icon| icon.hash == favicon.hash);
                if let Some(data) = favicon.data {
                    entry.icon = Some(crate::state::ProjectIcon {
                        hash: favicon.hash,
                        mime_type: favicon.mime_type,
                        data: std::sync::Arc::new(data),
                    });
                } else if !unchanged {
                    entry.icon = None;
                }
            }
            Err(()) => entry.version.clear(),
        }
    }

    pub(super) fn scan_sessions(&mut self) -> Next {
        let import = &mut self.state.session_import;
        import.scan_pending = true;
        import.scan_error = None;
        Next::call(Call::ScanAgentSessions(c::ScanAgentSessions {}), None)
    }

    pub(super) fn select_import_sessions(&mut self, paths: &[String], checked: bool) {
        let import = &mut self.state.session_import;
        let Some(scan) = &import.scan else {
            return;
        };
        let current = import
            .selection
            .clone()
            .unwrap_or_else(|| default_import_selection(scan, now_ms() as i64));
        import.selection = Some(set_import_selection(&current, paths, checked));
    }

    /// Imports the selected folders in order, registering the ones that are
    /// not projects yet.
    pub(super) fn import_sessions(&mut self, complete: Waiter) {
        let import = &self.state.session_import;
        let Some(scan) = import.scan.clone().filter(|_| !import.importing) else {
            let _ = complete.send(Err(invalid("Scan for sessions first")));
            return;
        };
        let selected = import
            .selection
            .clone()
            .unwrap_or_else(|| default_import_selection(&scan, now_ms() as i64));
        let selection: Vec<String> = scan
            .candidates
            .iter()
            .filter(|candidate| selected.contains(&candidate.path))
            .map(|candidate| candidate.path.clone())
            .collect();
        let projects = self.state.shell_projects().to_vec();
        let progress = &mut self.state.session_import.progress;
        progress.begin(selection.clone());
        let work: Vec<_> = scan
            .candidates
            .iter()
            .filter(|candidate| {
                selection.contains(&candidate.path) && progress.needs_import(&candidate.path)
            })
            .map(|candidate| {
                (
                    candidate.path.clone(),
                    resolve_import_project_id(&projects, candidate),
                )
            })
            .collect();
        let sender = self.sender.clone();
        let network = match self.network() {
            Ok(network) => network,
            Err(error) => {
                let _ = complete.send(Err(error));
                return;
            }
        };
        let (peer, epoch) = (network.peer.clone(), network.epoch);
        network.spawn(async move {
            let mut steps: Vec<ImportStep> = vec![];
            for (path, project) in work {
                let result = async {
                    let project_id = match project {
                        Some(id) => id,
                        None => {
                            peer.request::<String>(&Call::AddProject(op::AddProject {
                                cwd: path.clone(),
                            }))
                            .await?
                        }
                    };
                    let counts = peer
                        .request::<c::ImportCounts>(&Call::ImportAgentSessions(
                            c::ImportAgentSessions {
                                project_id: project_id.clone(),
                                expected_root: Some(path.clone()),
                            },
                        ))
                        .await?;
                    Ok((project_id, counts))
                }
                .await;
                steps.push((path, result));
            }
            let _ = sender
                .send(Event::Imported {
                    epoch,
                    steps,
                    complete,
                })
                .await;
        });
        self.state.session_import.importing = true;
    }

    pub(super) fn imported(&mut self, steps: Vec<ImportStep>, complete: Waiter) {
        let import = &mut self.state.session_import;
        import.importing = false;
        let mut failure = None;
        for (path, result) in steps {
            match result {
                Ok((project, counts)) => import.progress.record_import(&path, &project, counts),
                Err(error) => {
                    import.progress.record_failure();
                    failure = Some(error);
                }
            }
        }
        import.progress.finish();
        import.toast = import.progress.completion_toast();
        let landing = import.progress.landing_project().map(str::to_owned);
        let rescan = import.progress.rescan_needed();
        if let Some(project) = landing {
            self.state.selected_project = Some(project);
            self.select_thread(None);
        }
        if rescan {
            let _ = self.prepare_scan();
        }
        let _ = complete.send(match failure {
            Some(error) if self.state.session_import.toast.is_none() => Err(error),
            _ => Ok(Outcome::Applied),
        });
    }

    fn prepare_scan(&mut self) -> Result<(), PeerError> {
        if let Next::Call(call, _) = self.scan_sessions() {
            self.job(*call, None, None);
        }
        Ok(())
    }
}

/// What a project's icon depends on in its shell record.
fn project_icon_version(project: &m::Project) -> String {
    format!(
        "{}|{}",
        project.favicon_path.as_deref().unwrap_or_default(),
        project
            .updated_at
            .as_ref()
            .map_or(0, agent_domain::Timestamp::millis)
    )
}

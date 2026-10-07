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
            }),
            None,
        )
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

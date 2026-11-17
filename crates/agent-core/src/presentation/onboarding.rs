//! The desktop's first-run setup: the environment and its AI, then the projects
//! to bring over from Codex and Claude Code history. Importing projects also
//! opens on its own from the settings once the first run is over.
use super::connections::{AgentAvailability, ConnectionSetup};
use crate::state::{Snapshot, operations::OperationKey};

/// Where the setup stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Onboarding {
    first_run: bool,
    importing: bool,
}

/// The page the setup shows in place of the conversations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnboardingPage {
    /// Step 1 of 2: where to work, and its AI.
    Environment,
    /// Importing projects: step 2 of 2 on a first run, or alone after it.
    Import { first_run: bool },
}

impl Onboarding {
    /// A device that completed the setup before starts on its conversations.
    pub fn new(completed: bool) -> Self {
        Self {
            first_run: !completed,
            importing: false,
        }
    }

    pub fn page(self) -> Option<OnboardingPage> {
        match (self.first_run, self.importing) {
            (first_run, true) => Some(OnboardingPage::Import { first_run }),
            (true, false) => Some(OnboardingPage::Environment),
            (false, false) => None,
        }
    }

    pub fn open_import(self) -> Self {
        Self {
            importing: true,
            ..self
        }
    }

    /// Back from importing: to the environment on a first run, else closed.
    pub fn back(self) -> Self {
        Self {
            importing: false,
            ..self
        }
    }

    /// Skipping or finishing the import. A first run ends only once its
    /// completion is saved (`completed`); otherwise the import just closes.
    pub fn finish(self) -> Finish {
        if self.first_run {
            Finish::Save
        } else {
            Finish::Closed(self.back())
        }
    }

    /// The completion was saved: the setup does not show again.
    pub fn completed(self) -> Self {
        Self::new(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    /// Save the completion, then apply `Onboarding::completed`.
    Save,
    Closed(Onboarding),
}

/// The first step's primary action, by how ready the environment's AI is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStart {
    Connecting,
    /// An AI is signed in: the projects are next.
    Ready,
    Checking,
    LoginRequired,
}

impl ConnectionSetup {
    pub fn start(&self, connected: bool) -> SetupStart {
        if !connected {
            SetupStart::Connecting
        } else if self.can_start {
            SetupStart::Ready
        } else if self
            .agents
            .iter()
            .any(|agent| agent.availability == AgentAvailability::Checking)
        {
            SetupStart::Checking
        } else {
            SetupStart::LoginRequired
        }
    }

    /// Without a ready AI the projects can still come first, once connected.
    pub fn can_skip_agents(&self, connected: bool) -> bool {
        connected && !self.can_start
    }
}

/// The import step as it shows: the scan's projects and what importing them did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectImportView {
    pub scanning: bool,
    pub importing: bool,
    /// `None` until a scan answers.
    pub candidates: Option<Vec<ImportCandidate>>,
    /// Each provider whose history could not be read, with its reason.
    pub warnings: Vec<(String, String)>,
    pub imported: usize,
    /// Each project that could not be imported, with its reason.
    pub errors: Vec<(String, String)>,
    pub selected: usize,
    pub can_import: bool,
}

impl ProjectImportView {
    /// Scanning or importing: the selection and the step's buttons wait.
    pub fn busy(&self) -> bool {
        self.scanning || self.importing
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImportCandidate {
    pub path: String,
    pub name: String,
    /// An imported project shows checked and cannot be chosen again.
    pub registered: bool,
    pub checked: bool,
    pub selectable: bool,
    pub conversations: u32,
    /// The providers whose history names it, as `codex / claude`.
    pub providers: String,
}

impl Snapshot {
    pub fn project_import_view(&self) -> ProjectImportView {
        let state = &self.project_import;
        let busy = self.project_import_busy();
        ProjectImportView {
            scanning: self.operation_running(OperationKey::ProjectScan),
            importing: self.operation_running(OperationKey::ProjectImport),
            candidates: state.scan.as_ref().map(|scan| {
                scan.projects
                    .iter()
                    .map(|project| ImportCandidate {
                        path: project.path.clone(),
                        name: if project.is_scratch {
                            self.catalog.ui_language.text("プロジェクトなし").into()
                        } else {
                            project.name.clone()
                        },
                        registered: project.registered,
                        checked: project.registered || state.selected.contains(&project.path),
                        selectable: !busy && !project.registered,
                        conversations: project.conversation_count,
                        providers: project
                            .providers
                            .iter()
                            .map(|provider| provider.key())
                            .collect::<Vec<_>>()
                            .join(" / "),
                    })
                    .collect()
            }),
            warnings: state.scan.as_ref().map_or_else(Vec::new, |scan| {
                scan.warnings
                    .iter()
                    .map(|(provider, warning)| (provider.clone(), warning.clone()))
                    .collect()
            }),
            imported: state
                .result
                .as_ref()
                .map_or(0, |result| result.imported.len()),
            errors: state.result.as_ref().map_or_else(Vec::new, |result| {
                result
                    .errors
                    .iter()
                    .map(|(path, error)| (path.clone(), error.clone()))
                    .collect()
            }),
            selected: state.selected.len(),
            can_import: !busy && !state.selected.is_empty() && self.connection.connected,
        }
    }

    /// The last import brought every selected project: the setup can end.
    pub fn project_import_succeeded(&self) -> bool {
        self.project_import
            .result
            .as_ref()
            .is_some_and(|result| result.errors.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        presentation::connections::ConnectionAgent,
        session::ProviderKind,
        state::{
            Event, Intent, ProjectImportState,
            operations::{OperationPhase, OperationState},
        },
    };
    use agent_protocol::project_import::{ProjectCandidate, ProjectImportResult, ProjectScan};
    use std::sync::Arc;

    #[test]
    fn a_first_run_shows_both_steps_until_its_completion_is_saved() {
        let first = Onboarding::new(false);
        assert_eq!(first.page(), Some(OnboardingPage::Environment));
        let import = first.open_import();
        assert_eq!(
            import.page(),
            Some(OnboardingPage::Import { first_run: true })
        );
        assert_eq!(import.back(), first);
        assert_eq!(import.finish(), Finish::Save);
        assert_eq!(first.finish(), Finish::Save);
        assert_eq!(import.completed().page(), None);
    }

    #[test]
    fn after_the_first_run_the_import_opens_alone_and_closes_without_saving() {
        let done = Onboarding::new(true);
        assert_eq!(done.page(), None);
        let import = done.open_import();
        assert_eq!(
            import.page(),
            Some(OnboardingPage::Import { first_run: false })
        );
        assert_eq!(import.back().page(), None);
        assert_eq!(import.finish(), Finish::Closed(done));
    }

    fn agent(availability: AgentAvailability) -> ConnectionAgent {
        ConnectionAgent {
            provider: ProviderKind::Codex,
            enabled: true,
            name: "Codex".into(),
            availability,
            label: String::new(),
            error: None,
        }
    }

    #[test]
    fn the_start_waits_for_the_connection_then_names_what_the_ai_needs() {
        let setup = |availability: &[AgentAvailability]| ConnectionSetup {
            agents: availability.iter().copied().map(agent).collect(),
            can_start: availability.contains(&AgentAvailability::Ready),
        };
        let ready = setup(&[AgentAvailability::LoginRequired, AgentAvailability::Ready]);
        assert_eq!(ready.start(false), SetupStart::Connecting);
        assert!(!ready.can_skip_agents(false));
        assert_eq!(ready.start(true), SetupStart::Ready);
        assert!(!ready.can_skip_agents(true));
        let checking = setup(&[AgentAvailability::Checking, AgentAvailability::Unavailable]);
        assert_eq!(checking.start(true), SetupStart::Checking);
        assert!(checking.can_skip_agents(true));
        let login = setup(&[
            AgentAvailability::LoginRequired,
            AgentAvailability::Unavailable,
        ]);
        assert_eq!(login.start(true), SetupStart::LoginRequired);
        assert!(login.can_skip_agents(true));
        assert!(!login.can_skip_agents(false));
    }

    fn candidate(path: &str, registered: bool) -> ProjectCandidate {
        ProjectCandidate {
            path: path.into(),
            name: path.trim_start_matches('/').into(),
            providers: vec![ProviderKind::Codex, ProviderKind::Claude],
            conversation_count: 4,
            last_active_at: None,
            is_repository: true,
            is_scratch: false,
            registered,
        }
    }

    fn scanned() -> Snapshot {
        let mut snapshot = Snapshot::default();
        snapshot.connection.connected = true;
        snapshot.project_import = Arc::new(ProjectImportState {
            scan: Some(ProjectScan {
                projects: vec![candidate("/new", false), candidate("/old", true)],
                warnings: [("claude".into(), "history unreadable".into())].into(),
            }),
            selected: ["/new".into()].into(),
            result: None,
        });
        snapshot
    }

    #[test]
    fn imported_projects_stay_checked_but_cannot_be_chosen_again() {
        let view = scanned().project_import_view();
        let candidates = view.candidates.as_ref().unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|c| (c.checked, c.selectable, c.registered))
                .collect::<Vec<_>>(),
            [(true, true, false), (true, false, true)]
        );
        assert_eq!(candidates[0].providers, "codex / claude");
        assert_eq!(candidates[0].conversations, 4);
        assert_eq!(view.selected, 1);
        assert!(view.can_import);
        assert_eq!(
            view.warnings,
            [("claude".to_owned(), "history unreadable".to_owned())]
        );
        // Deselecting through Core leaves nothing to import.
        let (deselected, _) = crate::state::reduce(
            &scanned(),
            "",
            Event::from(Intent::SelectAllImportProjects { selected: false }),
        );
        let view = deselected.project_import_view();
        assert_eq!(view.selected, 0);
        assert!(!view.can_import);
    }

    #[test]
    fn a_running_scan_or_import_or_a_lost_connection_holds_the_selection() {
        assert_eq!(Snapshot::default().project_import_view().candidates, None);
        for key in [OperationKey::ProjectScan, OperationKey::ProjectImport] {
            let mut snapshot = scanned();
            Arc::make_mut(&mut snapshot.operations).insert(
                key.clone(),
                OperationState {
                    generation: 1,
                    phase: OperationPhase::Running,
                },
            );
            let view = snapshot.project_import_view();
            assert!(view.busy());
            assert!(!view.can_import);
            assert!(view.candidates.unwrap().iter().all(|c| !c.selectable));
        }
        let mut offline = scanned();
        offline.connection.connected = false;
        assert!(!offline.project_import_view().can_import);
    }

    #[test]
    fn only_an_import_without_failures_ends_the_setup() {
        let mut snapshot = scanned();
        assert!(!snapshot.project_import_succeeded());
        let result = |errors: &[(&str, &str)]| ProjectImportResult {
            imported: vec!["/new".into()],
            errors: errors
                .iter()
                .map(|(path, error)| (path.to_string(), error.to_string()))
                .collect(),
        };
        Arc::make_mut(&mut snapshot.project_import).result = Some(result(&[("/other", "denied")]));
        let view = snapshot.project_import_view();
        assert_eq!(view.imported, 1);
        assert_eq!(view.errors, [("/other".to_owned(), "denied".to_owned())]);
        assert!(!snapshot.project_import_succeeded());
        Arc::make_mut(&mut snapshot.project_import).result = Some(result(&[]));
        assert!(snapshot.project_import_succeeded());
    }
}

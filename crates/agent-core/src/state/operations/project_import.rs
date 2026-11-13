use super::*;
pub use agent_protocol::project_import::ScanProjects;

impl Operation for ScanProjects {
    rpc_operation!();
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProjectScan)
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        ensure_idle(snapshot)
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        apply_scan(snapshot, output, now);
        Vec::new()
    }
}

fn apply_scan(
    snapshot: &mut Snapshot,
    scan: agent_protocol::project_import::ProjectScan,
    now: f64,
) {
    let state = Arc::make_mut(&mut snapshot.project_import);
    let known: BTreeSet<_> = state
        .scan
        .iter()
        .flat_map(|scan| scan.projects.iter().map(|project| project.path.as_str()))
        .collect();
    state.selected = scan
        .projects
        .iter()
        .filter(|project| {
            !project.registered
                && if known.contains(project.path.as_str()) {
                    state.selected.contains(&project.path)
                } else {
                    project.is_repository
                        && project.conversation_count >= 3
                        && project
                            .last_active_at
                            .is_some_and(|time| time >= now - 30. * 86400. && time <= now)
                }
        })
        .map(|project| project.path.clone())
        .collect();
    state.scan = Some(scan);
}

fn ensure_idle(snapshot: &Snapshot) -> Result<(), String> {
    if snapshot.project_import_busy() {
        Err("プロジェクトの読み込みが進行中です。".into())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ImportProjects {}

impl Operation for ImportProjects {
    type Input = Vec<String>;
    type Output = agent_protocol::project_import::ProjectImportResult;
    fn capture(&self, snapshot: &Snapshot, _pane_id: &str) -> Result<Self::Input, PeerError> {
        Ok(snapshot.project_import.selected.iter().cloned().collect())
    }
    fn key(&self) -> Option<OperationKey> {
        Some(OperationKey::ProjectImport)
    }
    fn prepare(&mut self, snapshot: &mut Snapshot, _pane_id: &str) -> Result<(), String> {
        ensure_idle(snapshot)?;
        if snapshot.project_import.selected.is_empty() {
            return Err("取り込むプロジェクトを選択してください。".into());
        }
        Arc::make_mut(&mut snapshot.project_import).result = None;
        Ok(())
    }
    async fn run(
        &self,
        paths: Self::Input,
        context: &mut Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        context
            .call(&agent_protocol::project_import::ImportProjects { paths })
            .await
    }
    fn apply(self, snapshot: &mut Snapshot, _pane_id: &str, output: Self::Output) -> Vec<Effect> {
        let state = Arc::make_mut(&mut snapshot.project_import);
        for path in &output.imported {
            state.selected.remove(path);
            if let Some(scan) = &mut state.scan {
                for project in &mut scan.projects {
                    if &project.path == path {
                        project.registered = true;
                    }
                }
            }
        }
        state.result = Some(output);
        vec![Effect::execute(super::super::list_sync::SubscribeCatalog {
            refresh: false,
        })]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::project_import::{ProjectCandidate, ProjectImportResult, ProjectScan};

    fn candidate(path: &str) -> ProjectCandidate {
        ProjectCandidate {
            path: path.into(),
            name: path.into(),
            providers: vec![],
            conversation_count: 3,
            last_active_at: Some(100.),
            is_repository: true,
            registered: false,
        }
    }

    #[test]
    fn retry_recommends_newly_discovered_projects_and_preserves_explicit_deselection() {
        let mut snapshot = Snapshot::default();
        apply_scan(
            &mut snapshot,
            ProjectScan {
                projects: vec![candidate("/a")],
                ..Default::default()
            },
            101.,
        );
        (snapshot, _) = reduce_intent(
            &snapshot,
            "",
            Intent::SelectImportProject {
                path: "/a".into(),
                selected: false,
            },
        );
        apply_scan(
            &mut snapshot,
            ProjectScan {
                projects: vec![candidate("/a"), candidate("/recovered")],
                ..Default::default()
            },
            101.,
        );
        assert_eq!(
            snapshot
                .project_import
                .selected
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["/recovered"]
        );
    }

    proptest::proptest! {
        #[test]
        fn default_selection_only_includes_recent_unregistered_repositories(
            age_days in -10i32..100, count in 0u32..10, repository in proptest::bool::ANY, registered in proptest::bool::ANY, has_activity in proptest::bool::ANY,
        ) {
            let now = 100. * 86400.;
            let scan = ProjectScan { projects: vec![ProjectCandidate {
                path: "/project".into(), name: "project".into(), providers: vec![],
                conversation_count: count, last_active_at: has_activity.then_some(now - f64::from(age_days) * 86400.),
                is_repository: repository, registered,
            }], ..Default::default() };
            let mut snapshot = Snapshot::default();
            apply_scan(&mut snapshot, scan, now);
            proptest::prop_assert_eq!(snapshot.project_import.selected.contains("/project"),
                repository && !registered && count >= 3 && has_activity && (0..=30).contains(&age_days));
        }
    }

    #[test]
    fn selection_rejects_unknown_registered_and_busy_projects_and_resets_for_another_host() {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.project_import).scan = Some(ProjectScan {
            projects: [false, true]
                .into_iter()
                .map(|registered| ProjectCandidate {
                    path: format!("/{registered}"),
                    name: String::new(),
                    providers: vec![],
                    conversation_count: 0,
                    last_active_at: None,
                    is_repository: false,
                    registered,
                })
                .collect(),
            ..Default::default()
        });
        for path in ["/unknown", "/true"] {
            (snapshot, _) = reduce_intent(
                &snapshot,
                "",
                Intent::SelectImportProject {
                    path: path.into(),
                    selected: true,
                },
            );
            assert!(snapshot.project_import.selected.is_empty());
        }
        (snapshot, _) = reduce_intent(
            &snapshot,
            "",
            Intent::SelectAllImportProjects { selected: true },
        );
        assert_eq!(
            ImportProjects {}.capture(&snapshot, "").unwrap(),
            ["/false"]
        );
        for key in [OperationKey::ProjectScan, OperationKey::ProjectImport] {
            Arc::make_mut(&mut snapshot.operations).insert(
                key.clone(),
                OperationState {
                    generation: 1,
                    phase: OperationPhase::Running,
                },
            );
            let (blocked, _) = reduce_intent(
                &snapshot,
                "",
                Intent::SelectAllImportProjects { selected: false },
            );
            assert_eq!(
                blocked.project_import.selected,
                snapshot.project_import.selected
            );
            let (blocked, _) = reduce_intent(
                &snapshot,
                "",
                Intent::SelectImportProject {
                    path: "/false".into(),
                    selected: false,
                },
            );
            assert_eq!(
                blocked.project_import.selected,
                snapshot.project_import.selected
            );
            assert!(ScanProjects {}.prepare(&mut snapshot, "").is_err());
            assert!(ImportProjects {}.prepare(&mut snapshot, "").is_err());
            Arc::make_mut(&mut snapshot.operations).remove(&key);
        }
        let (other, _) = reduce_event(&snapshot, Event::StorageScope("another-host".into()));
        assert_eq!(*other.project_import, Default::default());
        (snapshot, _) = reduce_intent(
            &snapshot,
            "",
            Intent::SelectAllImportProjects { selected: false },
        );
        assert!(ImportProjects {}.prepare(&mut snapshot, "").is_err());
    }

    #[test]
    fn import_refresh_preserves_choices_and_partial_failures_without_changing_the_draft() {
        let mut snapshot = Snapshot::default();
        let scan = ProjectScan {
            projects: vec![candidate("/a"), candidate("/b")],
            ..Default::default()
        };
        apply_scan(&mut snapshot, scan.clone(), 101.);
        assert_eq!(snapshot.project_import.selected.len(), 2);
        Arc::make_mut(&mut snapshot.project_import)
            .selected
            .remove("/b");
        apply_scan(&mut snapshot, scan, 101.);
        assert_eq!(
            snapshot.project_import.selected.iter().collect::<Vec<_>>(),
            [&"/a".to_string()]
        );
        Arc::make_mut(&mut snapshot.project_import)
            .selected
            .insert("/b".into());
        let navigation = snapshot.panes.primary().navigation.clone();
        let drafts = snapshot.input.drafts.clone();
        let result = ProjectImportResult {
            imported: vec!["/a".into()],
            errors: [("/b".into(), "permission denied".into())].into(),
        };
        let effects = ImportProjects {}.apply(&mut snapshot, "", result.clone());
        assert_eq!(snapshot.project_import.result, Some(result));
        assert!(snapshot.project_import.scan.as_ref().unwrap().projects[0].registered);
        assert_eq!(ImportProjects {}.capture(&snapshot, "").unwrap(), ["/b"]);
        assert_eq!(snapshot.panes.primary().navigation, navigation);
        assert_eq!(snapshot.input.drafts, drafts);
        assert_eq!(effects.len(), 1, "refresh the registered project list");
    }
}

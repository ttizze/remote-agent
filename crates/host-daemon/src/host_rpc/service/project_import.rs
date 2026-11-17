use super::*;
use agent_protocol::project_import::{ProjectCandidate, ProjectImportResult, ProjectScan};
use std::collections::HashMap;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

const MAX_SESSIONS_PER_PROVIDER: usize = 5_000;

impl HostRpcService {
    /// Project acquisition has one route, including registration and discovery.
    pub(super) async fn project_request(&self, request: &Call) -> Result<Option<Body>, Failure> {
        Ok(Some(match request {
            Call::AddProject(params) => self
                .inner
                .projects
                .register(Path::new(&params.cwd))
                .await
                .map_err(|error| Failure::new("project_add_failed", error))?
                .into(),
            Call::ScanProjects(_) => self.scan_projects().await?.into(),
            Call::ImportProjects(params) => self.import_projects(&params.paths).await?.into(),
            Call::CloneProject(params) => self
                .inner
                .project_clones
                .start(params.clone())
                .await
                .map_err(|error| Failure::new("project_clone_failed", error))?
                .into(),
            Call::ReadProjectClones(_) => self
                .inner
                .project_clones
                .read()
                .await
                .map_err(|error| Failure::new("project_clone_read_failed", error))?
                .into(),
            Call::ChangeProjectClone(params) => self
                .inner
                .project_clones
                .change(params.clone())
                .await
                .map_err(|error| Failure::new("project_clone_change_failed", error))?
                .into(),
            _ => return Ok(None),
        }))
    }
    pub(super) async fn scan_projects(&self) -> Result<ProjectScan, Failure> {
        let projects = self.project_snapshot().await?;
        let owned = self
            .inner
            .conversation
            .storage
            .active_sources()
            .map_err(super::super::runtime::storage_failure)?;
        let scope = SessionListScope::Roots {
            owned: &owned,
            directories: &projects.scratch_roots,
        };
        let deadline = tokio::time::Instant::now() + SESSION_LIST_TIMEOUT;
        let sources = futures_util::future::join_all(
            self.inner
                .providers
                .all()
                .into_iter()
                .filter(|(provider, _)| {
                    matches!(provider, ProviderKind::Codex | ProviderKind::Claude)
                })
                .map(|(provider, agent)| async move {
                    let pages = native_pages(agent.as_ref(), scope, 100, 100, Some(deadline), None);
                    futures_util::pin_mut!(pages);
                    let mut threads = Vec::new();
                    let mut warning = None;
                    while let Some(page) = pages.next().await {
                        match page {
                            Ok(page) => threads.extend(page),
                            Err(error) => {
                                warning = Some(error.to_string());
                                break;
                            }
                        }
                        if threads.len() >= MAX_SESSIONS_PER_PROVIDER {
                            threads.truncate(MAX_SESSIONS_PER_PROVIDER);
                            warning =
                                Some("履歴の探索上限に達しました。候補は一部のみです。".into());
                            break;
                        }
                    }
                    (provider, threads, warning)
                }),
        )
        .await;
        let mut scan = ProjectScan::default();
        for (provider, error) in self.inner.providers.startup_errors() {
            if matches!(provider, ProviderKind::Codex | ProviderKind::Claude) {
                scan.warnings
                    .insert(provider.key().into(), error.to_string());
            }
        }
        let mut threads = Vec::new();
        for (provider, source, warning) in sources {
            threads.extend(source);
            if let Some(warning) = warning {
                scan.warnings.insert(provider.key().into(), warning);
            }
        }
        let roots = projects.scratch_roots.clone();
        scan.projects = tokio::task::spawn_blocking(move || candidates(threads, &roots, &owned))
            .await
            .map_err(|error| Failure::new("project_scan_failed", error))?;
        for candidate in &mut scan.projects {
            // Scratch remains selectable to recover additional native histories.
            candidate.registered =
                !candidate.is_scratch && projects.project_for_workspace(&candidate.path).is_some();
        }
        Ok(scan)
    }

    pub(super) async fn import_projects(
        &self,
        paths: &[String],
    ) -> Result<ProjectImportResult, Failure> {
        if paths.len() > 200 {
            return Err(Failure::new(
                "invalid_params",
                "一度に取り込めるプロジェクトは200件までです。",
            ));
        }
        let mut result = ProjectImportResult::default();
        for path in paths.iter().collect::<std::collections::BTreeSet<_>>() {
            match self.inner.projects.register(Path::new(path)).await {
                Ok(path) => result.imported.push(path),
                Err(error) => {
                    result.errors.insert(path.clone(), error.to_string());
                }
            }
        }
        let errors =
            super::super::runtime::Runtime::import_project_threads(self, &result.imported).await?;
        result.imported.retain(|path| !errors.contains_key(path));
        result.errors.extend(errors);
        Ok(result)
    }
}

fn workspace(
    cwd: &str,
    scratch_roots: &std::collections::BTreeSet<PathBuf>,
) -> Option<(PathBuf, bool, bool)> {
    if agent_local_host::wsl::folder(cwd).ok()?.is_some() {
        return None;
    }
    let cwd = Path::new(cwd);
    if !cwd.is_absolute() {
        return None;
    }
    if let Some(root) = crate::projects::state::scratch_root(scratch_roots, cwd) {
        // Native transcripts retain cwd even when the catalog or folder is gone.
        return Some((
            dunce::canonicalize(root).unwrap_or_else(|_| root.to_owned()),
            false,
            true,
        ));
    }
    let cwd = dunce::canonicalize(cwd).ok()?;
    if !cwd.is_dir() {
        return None;
    }
    for ancestor in cwd.ancestors() {
        let git = ancestor.join(".git");
        // Linked worktrees belong to the registered source repository.
        if git.is_file() {
            return None;
        }
        if git.is_dir() {
            return Some((ancestor.to_owned(), true, false));
        }
    }
    Some((cwd, false, false))
}

fn candidates(
    threads: Vec<Thread>,
    scratch_roots: &std::collections::BTreeSet<PathBuf>,
    owned: &[agent_protocol::session::SessionRef],
) -> Vec<ProjectCandidate> {
    let owned: HashSet<_> = owned.iter().collect();
    let mut directories = HashMap::new();
    let mut seen = HashSet::new();
    let mut candidates = BTreeMap::<String, ProjectCandidate>::new();
    for thread in threads {
        let (Some(id), Some(cwd)) = (thread.id, thread.cwd) else {
            continue;
        };
        if thread.parent_id.is_some() || !seen.insert(id.clone()) {
            continue;
        }
        let Some((root, repository, scratch)) = directories
            .entry(cwd.clone())
            .or_insert_with(|| workspace(&cwd, scratch_roots))
        else {
            continue;
        };
        if *scratch && owned.contains(&id) {
            continue;
        }
        let Some(path) = root.to_str() else {
            continue;
        };
        let candidate = candidates
            .entry(path.into())
            .or_insert_with(|| ProjectCandidate {
                path: path.into(),
                name: if *scratch {
                    "No project".into()
                } else {
                    root.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.into())
                },
                providers: vec![],
                conversation_count: 0,
                last_active_at: None,
                is_repository: *repository,
                is_scratch: *scratch,
                registered: false,
            });
        candidate.conversation_count += 1;
        if let Some(provider) = thread.active_provider.or(id.provider)
            && !candidate.providers.contains(&provider)
        {
            candidate.providers.push(provider);
            candidate.providers.sort();
        }
        if let Some(time) = thread.updated_at.filter(|time| time.is_finite()) {
            candidate.last_active_at = Some(
                candidate
                    .last_active_at
                    .map_or(time, |previous| previous.max(time)),
            );
        }
    }
    let mut candidates: Vec<_> = candidates.into_values().collect();
    candidates.sort_by(|a, b| {
        b.is_repository
            .cmp(&a.is_repository)
            .then_with(|| {
                b.last_active_at
                    .unwrap_or_default()
                    .total_cmp(&a.last_active_at.unwrap_or_default())
            })
            .then(a.path.cmp(&b.path))
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case(ProviderKind::Codex)]
    #[case(ProviderKind::Claude)]
    #[tokio::test]
    async fn scratch_import_reports_unreadable_history_and_retries_without_duplicates(
        #[case] provider: ProviderKind,
    ) {
        use crate::host_rpc::runtime::native_fixture::NativeAgent;
        use agent_protocol::{
            execution::{ItemStatus, TurnStatus},
            items::{Item, ItemBody},
            models::{Thread, ThreadResponse, Turn},
            session::SessionRef,
        };
        use std::sync::Arc;
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let mut agent = NativeAgent::new(provider);
        Arc::get_mut(&mut agent).unwrap().deferred_history = true;
        let native = SessionRef::new(provider, "source-chat".into()).unwrap();
        agent.records.lock().unwrap().insert(
            native.id.clone(),
            ThreadResponse {
                thread: Thread {
                    id: Some(native.clone()),
                    cwd: Some(root.join("scratch/private-chat").to_str().unwrap().into()),
                    turns: Some(vec![Arc::new(Turn {
                        id: "turn".into(),
                        status: TurnStatus::Completed,
                        items: Some(vec![Arc::new(Item::new(
                            "question".into(),
                            ItemStatus::Completed,
                            ItemBody::UserMessage {
                                text: Some("saved question".into()),
                                content: vec![],
                            },
                        ))]),
                        ..Default::default()
                    })]),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        agent.unreadable.lock().unwrap().insert("question".into());
        let service = HostRpcService::new([agent.backend()], root.join("projects.json"));
        let path = service.scan_projects().await.unwrap().projects[0]
            .path
            .clone();
        let failed = service.import_projects(&[path.clone()]).await.unwrap();
        assert!(failed.imported.is_empty());
        assert!(failed.errors.contains_key(&path));
        assert!(
            service
                .inner
                .conversation
                .storage
                .active_sources()
                .unwrap()
                .is_empty()
        );
        assert!(!service.scan_projects().await.unwrap().projects[0].registered);
        agent.unreadable.lock().unwrap().clear();
        let imported = service.import_projects(&[path.clone()]).await.unwrap();
        assert!(imported.errors.is_empty(), "{imported:?}");
        assert_eq!(imported.imported, [path.clone()]);
        service.import_projects(&[path]).await.unwrap();
        assert_eq!(
            service.inner.conversation.storage.active_sources().unwrap(),
            [native]
        );
    }

    async fn recovery_service(root: &Path) -> HostRpcService {
        let claude = crate::providers::Claude::load(
            root.join("missing-cli"),
            root.join("adapter"),
            Some(root.join("native")),
        )
        .await
        .unwrap();
        HostRpcService::new([claude.into()], root.join("projects.json"))
    }

    #[tokio::test]
    async fn scratch_reimports_sdk_history_after_database_loss_without_creating_projects_per_chat()
    {
        use agent_protocol::models::SCRATCH_PROJECT_ID;
        let directory = tempfile::tempdir().unwrap();
        let root = dunce::canonicalize(directory.path()).unwrap();
        let transcripts = root.join("native/projects/shared");
        std::fs::create_dir_all(&transcripts).unwrap();
        let sources = [uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];
        let folders = [root.join("scratch/first"), root.join("scratch/second")];
        for (id, cwd) in sources.iter().zip(&folders) {
            let user = serde_json::json!({"type":"user","uuid":"question","parentUuid":null,"sessionId":id,"cwd":cwd,"entrypoint":"sdk-ts","promptSource":"sdk","message":{"content":"saved question"}});
            let answer = serde_json::json!({"type":"assistant","uuid":"answer","parentUuid":"question","sessionId":id,"cwd":cwd,"message":{"id":"reply","role":"assistant","content":[{"type":"text","text":"saved answer"}]}});
            std::fs::write(
                transcripts.join(format!("{id}.jsonl")),
                format!("{user}\n{answer}\n"),
            )
            .unwrap();
        }
        // An automated task outside Scratch is still excluded from recovery.
        let other = uuid::Uuid::new_v4();
        let automated = serde_json::json!({"type":"user","sessionId":other,"cwd":root.join("elsewhere"),"entrypoint":"sdk-ts","promptSource":"sdk","message":{"content":"agent task"}});
        std::fs::write(
            transcripts.join(format!("{other}.jsonl")),
            format!("{automated}\n"),
        )
        .unwrap();
        for round in 0..2 {
            let service = recovery_service(&root).await;
            assert!(
                service
                    .inner
                    .projects
                    .load()
                    .await
                    .unwrap()
                    .projects
                    .is_empty()
            );
            let scan = service.scan_projects().await.unwrap();
            assert_eq!(scan.projects.len(), 1);
            let candidate = &scan.projects[0];
            assert!(candidate.is_scratch);
            assert!(!candidate.registered);
            assert_eq!(candidate.conversation_count, 2);
            let result = service
                .import_projects(&[candidate.path.clone()])
                .await
                .unwrap();
            assert!(result.errors.is_empty(), "{result:?}");
            assert_eq!(result.imported, [candidate.path.clone()]);
            let before = service.inner.conversation.storage.active_sources().unwrap();
            assert_eq!(before.len(), 2);
            service
                .import_projects(&[candidate.path.clone()])
                .await
                .unwrap();
            assert_eq!(
                service.inner.conversation.storage.active_sources().unwrap(),
                before
            );
            let projects = service.inner.projects.load().await.unwrap();
            assert_eq!(projects.projects.len(), 1);
            assert_eq!(projects.projects[0].id, SCRATCH_PROJECT_ID);
            for (source, cwd) in sources.iter().zip(&folders) {
                let native = agent_protocol::session::SessionRef::new(
                    ProviderKind::Claude,
                    source.to_string(),
                )
                .unwrap();
                let snapshot = super::super::super::runtime::native_fixture::canonical_snapshot(
                    &service, &native,
                )
                .unwrap();
                assert_eq!(
                    snapshot.projection.thread.project_id.as_deref(),
                    Some(SCRATCH_PROJECT_ID)
                );
                assert_eq!(
                    snapshot.projection.thread.worktree_path.as_deref(),
                    cwd.to_str()
                );
                assert!(snapshot.projection.turn_items.iter().any(|item| matches!(&item.body, agent_protocol::orchestration::TurnItemBody::UserMessage {text,..} if text == "saved question")));
                assert!(snapshot.projection.turn_items.iter().any(|item| matches!(&item.body, agent_protocol::orchestration::TurnItemBody::AssistantMessage {text,..} if text == "saved answer")));
            }
            assert_eq!(
                service
                    .restore_workspace(folders[0].to_str().unwrap())
                    .await
                    .unwrap(),
                round == 0
            );
            let artifact = folders[0].join("result.txt");
            if round == 0 {
                std::fs::write(&artifact, "keep generated files").unwrap();
            }
            assert_eq!(
                std::fs::read_to_string(artifact).unwrap(),
                "keep generated files"
            );
            drop(service);
            if round == 0 {
                // Remove only this test's application DB, retaining native transcripts.
                std::fs::remove_file(
                    root.join(super::super::super::storage::CONVERSATION_DATABASE_FILE),
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn discovery_ignores_unusable_folders_and_deduplicates_native_identities() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let linked = root.path().join("linked");
        let chats = root.path().join("scratch");
        for path in [
            &repository.join(".git"),
            &repository.join("src"),
            &linked,
            &chats,
        ] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(
            linked.join(".git"),
            "gitdir: ../repository/.git/worktrees/linked",
        )
        .unwrap();
        let thread = |provider, id: &str, path: &Path| Thread {
            id: Some(agent_protocol::session::SessionRef::new(provider, id.into()).unwrap()),
            cwd: Some(path.to_string_lossy().into_owned()),
            updated_at: Some(100.),
            ..Default::default()
        };
        let native = thread(ProviderKind::Codex, "same-id", &repository.join("src"));
        let found = candidates(
            vec![
                native.clone(),
                native,
                thread(ProviderKind::Claude, "same-id", &repository),
                thread(ProviderKind::Claude, "linked", &linked),
                thread(ProviderKind::Claude, "chat", &chats),
                thread(
                    ProviderKind::Claude,
                    "missing",
                    &root.path().join("missing"),
                ),
                thread(ProviderKind::Claude, "relative", Path::new("relative")),
            ],
            &[dunce::canonicalize(&chats).unwrap()].into(),
            &[],
        );
        assert_eq!(found.len(), 2);
        let scratch = found.iter().find(|candidate| candidate.is_scratch).unwrap();
        assert_eq!(scratch.conversation_count, 1);
        assert_eq!(scratch.name, "No project");
        assert_eq!(found[0].conversation_count, 2);
        assert_eq!(
            found[0].providers,
            [ProviderKind::Codex, ProviderKind::Claude]
        );
        assert_eq!(
            found[0].path,
            dunce::canonicalize(repository).unwrap().to_str().unwrap()
        );
    }

    #[tokio::test]
    async fn native_project_import_is_selective_durable_and_preserves_session_identity() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let other = root.path().join("other");
        std::fs::create_dir_all(repository.join(".git")).unwrap();
        std::fs::create_dir_all(repository.join("src")).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let native = root.path().join("native");
        let transcripts = native.join("projects/shared");
        std::fs::create_dir_all(&transcripts).unwrap();
        let mut expected = std::collections::HashSet::new();
        for cwd in [
            repository.clone(),
            repository.join("src"),
            repository.clone(),
            other,
        ] {
            let id = uuid::Uuid::new_v4().to_string();
            expected.insert(id.clone());
            let user = serde_json::json!({"type":"user","sessionId":id,"cwd":cwd,"message":{"content":"native conversation"}});
            std::fs::write(transcripts.join(format!("{id}.jsonl")), format!("{user}\n")).unwrap();
        }
        let agent = crate::providers::Claude::load(
            root.path().join("missing-cli"),
            root.path().join("state"),
            Some(native),
        )
        .await
        .unwrap();
        let store_path = root.path().join("bex-worktrees.json");
        let service = HostRpcService::new(
            [
                agent.into(),
                Backend::unavailable(ProviderKind::Codex, "offline"),
            ],
            &store_path,
        );
        let scan = service.scan_projects().await.unwrap();
        assert_eq!(scan.projects.len(), 2);
        assert!(scan.warnings.contains_key("codex"));
        let candidate = &scan.projects[0];
        assert!(candidate.is_repository);
        assert!(!candidate.registered);
        assert_eq!(candidate.conversation_count, 3);
        assert_eq!(candidate.providers, [ProviderKind::Claude]);
        assert!(
            &service
                .inner
                .projects
                .load()
                .await
                .unwrap()
                .projects
                .is_empty()
        );
        let path = candidate.path.clone();
        let missing = root.path().join("deleted").to_string_lossy().into_owned();
        let result = service
            .import_projects(&[path.clone(), missing.clone()])
            .await
            .unwrap();
        assert_eq!(result.imported.as_slice(), std::slice::from_ref(&path));
        assert!(result.errors.contains_key(&missing));
        service
            .import_projects(&[path.clone(), path.clone()])
            .await
            .unwrap();
        let registered = ProjectStore::fixture(&store_path).load().await.unwrap();
        assert_eq!(registered.projects.len(), 1);
        assert_eq!(registered.projects[0].roots[0].path, path);
        let scan = service.scan_projects().await.unwrap();
        assert!(scan.projects[0].registered);
        // Import only registers folders. Native titles and IDs remain readable without a CLI.
        let agent = &service.inner.providers.all()[0].1;
        let page = crate::providers::Discovery::catalog_page(
            agent.as_ref(),
            None,
            SessionListScope::Roots {
                owned: &[],
                directories: &Default::default(),
            },
            100,
        )
        .await
        .unwrap();
        assert_eq!(
            page.data
                .into_iter()
                .map(|entry| entry.id.unwrap().id)
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
    }
}

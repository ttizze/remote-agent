use super::*;
use agent_protocol::project_import::{ProjectCandidate, ProjectImportResult, ProjectScan};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};

const MAX_SESSIONS_PER_PROVIDER: usize = 5_000;

impl HostRpcService {
    pub(super) async fn scan_projects(&self) -> Result<ProjectScan, Failure> {
        let projects = self.project_snapshot().await?;
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
                    let pages = session_pages(
                        agent.as_ref(),
                        "",
                        SessionListScope::Roots,
                        100,
                        100,
                        Some(deadline),
                        None,
                    );
                    futures_util::pin_mut!(pages);
                    let mut threads = Vec::new();
                    let mut warning = None;
                    while let Some(page) = pages.next().await {
                        match page {
                            Ok(page) => {
                                threads.extend(page.into_iter().map(|summary| summary.thread))
                            }
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
        let chat_directory = self.inner.projects.chat_directory();
        scan.projects = tokio::task::spawn_blocking(move || candidates(threads, &chat_directory))
            .await
            .map_err(|error| Failure::new("project_scan_failed", error))?;
        for candidate in &mut scan.projects {
            candidate.registered = projects.project_for_workspace(&candidate.path).is_some();
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
        result.warnings = self.import_conversation_titles(&result.imported).await?;
        self.reimport_transcripts(&result.imported).await?;
        Ok(result)
    }

    /// External discovery belongs to explicit import, never to a list read.
    async fn import_conversation_titles(
        &self,
        roots: &[String],
    ) -> Result<BTreeMap<String, String>, Failure> {
        let mut warnings = BTreeMap::new();
        if roots.is_empty() {
            return Ok(warnings);
        }
        warnings.extend(
            self.inner
                .providers
                .startup_errors()
                .map(|(provider, error)| (provider.key().into(), error.to_string())),
        );
        let projects = self.project_snapshot().await?;
        let selected: HashSet<_> = roots
            .iter()
            .filter_map(|root| projects.project_for_workspace(root))
            .collect();
        for (provider, agent) in self.inner.providers.all() {
            let deadline = tokio::time::Instant::now() + SESSION_LIST_TIMEOUT;
            let pages = session_pages(
                agent.as_ref(),
                "",
                SessionListScope::All,
                100,
                100,
                Some(deadline),
                None,
            );
            futures_util::pin_mut!(pages);
            let mut scanned = 0;
            while let Some(page) = pages.next().await {
                let page = match page {
                    Ok(page) => page,
                    Err(error) => {
                        warnings.insert(provider.key().into(), error.to_string());
                        break;
                    }
                };
                for summary in page {
                    scanned += 1;
                    let Some(mut thread) = self.inner.database.summary(summary.thread)? else {
                        continue;
                    };
                    if thread
                        .cwd
                        .as_deref()
                        .and_then(|cwd| projects.project_for_workspace(cwd))
                        .is_some_and(|project| selected.contains(project))
                    {
                        thread.git_branch = summary.branch.or(thread.git_branch);
                        describe_thread(&mut thread, agent.capabilities(), &projects);
                        self.inner
                            .database
                            .observe(&thread)
                            .map_err(transcripts::storage_failure)?;
                    }
                }
                if scanned >= MAX_SESSIONS_PER_PROVIDER {
                    warnings.insert(
                        provider.key().into(),
                        "一覧が多いため、最初の5000件まで取り込みました。".into(),
                    );
                    break;
                }
            }
        }
        Ok(warnings)
    }

    /// Importing a project again copies native history continued outside the
    /// Host into its conversations' transcripts.
    async fn reimport_transcripts(&self, roots: &[String]) -> Result<(), Failure> {
        let projects = self.project_snapshot().await?;
        let imported: HashSet<_> = roots
            .iter()
            .filter_map(|root| projects.project_for_workspace(root))
            .collect();
        let conversations = self
            .inner
            .database
            .imported()
            .map_err(transcripts::storage_failure)?;
        for (conversation, cwd) in conversations {
            // Registered roots are canonical; a recorded cwd may not be.
            let cwd = cwd.map(|cwd| {
                dunce::canonicalize(&cwd)
                    .ok()
                    .and_then(|path| path.into_os_string().into_string().ok())
                    .unwrap_or(cwd)
            });
            if cwd
                .as_deref()
                .and_then(|cwd| projects.project_for_workspace(cwd))
                .is_some_and(|project| imported.contains(project))
            {
                // Re-imports run in the background.
                drop(self.import_transcript(conversation, true));
            }
        }
        Ok(())
    }
}

fn workspace(cwd: &str, chat_directory: &Path) -> Option<(PathBuf, bool)> {
    let cwd = Path::new(cwd);
    if !cwd.is_absolute() {
        return None;
    }
    let cwd = dunce::canonicalize(cwd).ok()?;
    if !cwd.is_dir() || cwd == chat_directory {
        return None;
    }
    for ancestor in cwd.ancestors() {
        let git = ancestor.join(".git");
        // Linked worktrees belong to the registered source repository.
        if git.is_file() {
            return None;
        }
        if git.is_dir() {
            return Some((ancestor.to_owned(), true));
        }
    }
    Some((cwd, false))
}

fn candidates(threads: Vec<Thread>, chat_directory: &Path) -> Vec<ProjectCandidate> {
    let chat_directory =
        dunce::canonicalize(chat_directory).unwrap_or_else(|_| chat_directory.to_owned());
    let mut directories = HashMap::new();
    let mut seen = HashSet::new();
    let mut candidates = BTreeMap::<String, ProjectCandidate>::new();
    for thread in threads {
        let (Some(id), Some(cwd)) = (thread.id, thread.cwd) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some((root, repository)) = directories
            .entry(cwd.clone())
            .or_insert_with(|| workspace(&cwd, &chat_directory))
        else {
            continue;
        };
        let Some(path) = root.to_str() else {
            continue;
        };
        let candidate = candidates
            .entry(path.into())
            .or_insert_with(|| ProjectCandidate {
                path: path.into(),
                name: root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.into()),
                providers: vec![],
                conversation_count: 0,
                last_active_at: None,
                is_repository: *repository,
                registered: false,
            });
        candidate.conversation_count += 1;
        if !candidate.providers.contains(&id.provider) {
            candidate.providers.push(id.provider);
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

    #[test]
    fn discovery_ignores_unusable_folders_and_deduplicates_native_identities() {
        let root = tempfile::tempdir().unwrap();
        let repository = root.path().join("repository");
        let linked = root.path().join("linked");
        let chats = root.path().join("bex-chats");
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
            &chats,
        );
        assert_eq!(found.len(), 1);
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
            ProjectStore::new(&store_path),
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
            ProjectStore::new(&store_path)
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
        assert!(result.warnings.contains_key("codex"));
        service
            .import_projects(&[path.clone(), path.clone()])
            .await
            .unwrap();
        let registered = ProjectStore::new(&store_path).load().await.unwrap();
        assert_eq!(registered.projects.len(), 1);
        assert_eq!(registered.projects[0].roots[0].path, path);
        let scan = service.scan_projects().await.unwrap();
        assert!(scan.projects[0].registered);
        // Explicit import preserves provider identities, including without a CLI.
        let agent = &service.inner.providers.all()[0].1;
        let page = Agent::list(agent.as_ref(), "", None, SessionListScope::Roots, 100)
            .await
            .unwrap();
        assert_eq!(
            page.data
                .into_iter()
                .map(|entry| entry.thread.id.unwrap().id)
                .collect::<std::collections::HashSet<_>>(),
            expected
        );
    }
}

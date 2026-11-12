use super::*;
use agent_protocol::workspace::{
    SetupPhase, SetupScript, SetupStageId, SetupStageStatus, WorkspaceSetup, WorkspaceStrategy,
};

struct Fixture {
    host: HostRpcService,
    agent: Arc<NativeAgent>,
    repository: std::path::PathBuf,
    directory: tempfile::TempDir,
}
impl Fixture {
    async fn new(script: Option<SetupScript>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repository = dunce::canonicalize(
            crate::host_rpc::service::worktrees::tests::repository(directory.path()),
        )
        .unwrap();
        let agent = NativeAgent::new(ProviderKind::Codex, directory.path().join("native"));
        let host = HostRpcService::new(
            [agent.backend()],
            ProjectStore::new(directory.path().join("projects.json")),
        );
        host.inner
            .worktrees
            .settings(Some(agent_protocol::models::WorktreeSettings {
                setup_script: script,
                ..Default::default()
            }))
            .await
            .unwrap();
        Self {
            host,
            agent,
            repository,
            directory,
        }
    }
    fn request(&self, base: &str, fetch: bool) -> Call {
        let mut input = request(&self.repository);
        let Call::CreateSession(params) = &mut input else {
            unreachable!()
        };
        params.initial_input.as_mut().unwrap().workspace = Some(WorkspaceStrategy::Worktree {
            base_ref: base.into(),
            start_from_origin: fetch,
        });
        input
    }
}
async fn setup_until(
    updates: &mut HostSubscription,
    predicate: impl Fn(&WorkspaceSetup) -> bool,
) -> WorkspaceSetup {
    let change = until(updates, |change| matches!(change, SessionChange::WorkspaceSetup { setup: Some(setup) } if predicate(setup))).await;
    let SessionChange::WorkspaceSetup { setup: Some(setup) } = change else {
        unreachable!()
    };
    setup
}
fn script(command: &str, asynchronous: bool) -> Option<SetupScript> {
    Some(SetupScript {
        name: "Install dependencies".into(),
        command: command.into(),
        asynchronous,
    })
}
fn commit(repository: &Path) {
    crate::git::text(repository, &["add", "--all"]).unwrap();
    crate::git::text(
        repository,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "-m",
            "remote update",
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn remote_start_fetches_before_checkout_and_retains_the_completed_card() {
    let fixture = Fixture::new(None).await;
    let repository = &fixture.repository;
    let old = crate::git::text(repository, &["rev-parse", "HEAD"]).unwrap();
    let remote = fixture.directory.path().join("origin.git");
    crate::git::text(
        repository,
        &[
            "clone",
            "--bare",
            repository.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    )
    .unwrap();
    crate::git::text(
        repository,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    )
    .unwrap();
    std::fs::write(repository.join("latest.txt"), "from remote").unwrap();
    commit(repository);
    crate::git::text(repository, &["push", "origin", "main"]).unwrap();
    crate::git::text(repository, &["reset", "--hard", old.trim()]).unwrap();
    let blocked = fixture.agent.create_gate.acquire().await.unwrap();
    let client = fixture.host.open_session();
    let (opened, mut updates) = admit(
        &fixture.host,
        client.id(),
        &fixture.request("origin/main", true),
    )
    .await;
    assert_eq!(
        opened.response.thread.workspace_setup.unwrap().stages.len(),
        4
    );
    let running = setup_until(&mut updates, |setup| {
        setup.stages[3].status == SetupStageStatus::Running
    })
    .await;
    assert_eq!(
        running
            .stages
            .iter()
            .map(|stage| (stage.id, stage.status))
            .collect::<Vec<_>>(),
        vec![
            (SetupStageId::Fetch, SetupStageStatus::Done),
            (SetupStageId::Checkout, SetupStageStatus::Done),
            (SetupStageId::SetupScript, SetupStageStatus::Skipped),
            (SetupStageId::Agent, SetupStageStatus::Running)
        ]
    );
    assert_eq!(
        std::fs::read_to_string(
            Path::new(running.worktree_path.as_ref().unwrap()).join("latest.txt")
        )
        .unwrap(),
        "from remote"
    );
    assert!(!repository.join("latest.txt").exists());
    assert!(
        call::<Empty>(
            &fixture.host,
            client.id(),
            Call::Interrupt(op::Interrupt {
                thread_id: opened.session.clone(),
                turn_id: "first-input".into(),
            })
        )
        .await
        .is_err(),
        "agent startup has passed the cancellation boundary"
    );
    assert_eq!(
        crate::git::text(
            Path::new(running.worktree_path.as_ref().unwrap()),
            &[
                "config",
                "--get",
                &crate::workspace_review::comparison::base_key(running.branch.as_ref().unwrap())
            ],
        )
        .unwrap()
        .trim(),
        "refs/remotes/origin/main"
    );
    let reopened: OpenedSession = call(
        &fixture.host,
        client.id(),
        Call::OpenSession(OpenSession {
            session: opened.session.clone(),
            limit: 5,
            include_activity: true,
            known_version: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(reopened.response.thread.workspace_setup, Some(running));
    drop(blocked);
    let done = setup_until(&mut updates, |setup| setup.phase == SetupPhase::Done).await;
    assert!(done.ended_at_ms.is_some());
    assert!(!done.can_cancel);
    let row = fixture
        .host
        .inner
        .conversations
        .get(&opened.session)
        .unwrap()
        .unwrap();
    assert!(row.startup.is_none());
    assert_eq!(row.workspace_setup, Some(done));
    assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn local_base_skips_fetch_and_root_selection_overrides_automatic_worktrees() {
    for root in [false, true] {
        let fixture = Fixture::new(None).await;
        fixture
            .host
            .inner
            .worktrees
            .settings(Some(agent_protocol::models::WorktreeSettings {
                create_on_new_session: true,
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut input = fixture.request("main", false);
        if root {
            let Call::CreateSession(params) = &mut input else {
                unreachable!()
            };
            params.initial_input.as_mut().unwrap().workspace = Some(WorkspaceStrategy::Root);
        }
        let blocked = fixture.agent.create_gate.acquire().await.unwrap();
        let client = fixture.host.open_session();
        let (opened, mut updates) = admit(&fixture.host, client.id(), &input).await;
        until(&mut updates, |change| matches!(change, SessionChange::Preparation { preparation: Some(preparation) } if preparation.stage == PreparationStage::Provider)).await;
        let row = fixture
            .host
            .inner
            .conversations
            .get(&opened.session)
            .unwrap()
            .unwrap();
        if root {
            assert!(row.workspace_setup.is_none());
            assert_eq!(Path::new(&row.cwd), fixture.repository.as_path());
        } else {
            assert_eq!(
                row.workspace_setup.unwrap().stages[0].status,
                SetupStageStatus::Skipped
            );
            assert_ne!(Path::new(&row.cwd), fixture.repository.as_path());
        }
        drop(blocked);
        until(&mut updates, |change| {
            matches!(change, SessionChange::Preparation { preparation: None })
        })
        .await;
    }
}

#[cfg(unix)]
#[tokio::test]
async fn project_script_failure_has_real_terminal_output_and_retries_in_the_same_checkout() {
    let fixture = Fixture::new(script("exit 99", false)).await;
    fixture
        .host
        .inner
        .projects
        .register(&fixture.repository)
        .await
        .unwrap();
    let project = fixture.host.project_snapshot().await.unwrap().projects[0]
        .id
        .clone();
    let mut settings = fixture.host.inner.worktrees.settings(None).await.unwrap();
    settings.project_setup_scripts.insert(project, script("if [ ! -f .attempted ]; then touch .attempted; printf 'setup failed here\\n'; exit 7; fi; printf 'recovered\\n'", false).unwrap());
    fixture
        .host
        .inner
        .worktrees
        .settings(Some(settings))
        .await
        .unwrap();
    let client = fixture.host.open_session();
    let (opened, mut updates) =
        admit(&fixture.host, client.id(), &fixture.request("main", false)).await;
    let failed = setup_until(&mut updates, |setup| setup.phase == SetupPhase::Failed).await;
    assert_eq!(failed.stages[2].exit_code, Some(7));
    assert!(
        failed.stages[2]
            .tail
            .iter()
            .any(|line| line.contains("setup failed here"))
    );
    assert_eq!(fixture.agent.next.load(Ordering::SeqCst), 0);
    let viewer = fixture.host.open_session();
    let handle = failed.terminal_handle.clone().unwrap();
    let _: Empty = call(
        &fixture.host,
        viewer.id(),
        Call::StartTerminal(op::StartTerminal {
            handle: handle.clone(),
            cwd: failed.worktree_path.clone().unwrap(),
            command: None,
            size: op::TerminalSize { cols: 80, rows: 24 },
        }),
    )
    .await
    .unwrap();
    assert!(fixture.host.inner.terminals.is_setup(&handle));
    until(&mut updates, |change| matches!(change, SessionChange::Preparation { preparation: Some(preparation) } if matches!(preparation.status, PreparationStatus::Failed { .. }))).await;
    tokio::task::yield_now().await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    until(&mut updates, |change| {
        matches!(change, SessionChange::WorkspaceSetup { setup: None })
    })
    .await;
    tokio::time::resume();
    let retained = fixture
        .host
        .inner
        .conversations
        .get(&opened.session)
        .unwrap()
        .unwrap();
    assert_eq!(
        retained.workspace_setup.as_ref().unwrap().script,
        failed.script
    );
    let _: Empty = call(
        &fixture.host,
        client.id(),
        Call::RetryPreparation(op::RetryPreparation {
            thread_id: opened.session,
            work_locally: false,
        }),
    )
    .await
    .unwrap();
    let done = setup_until(&mut updates, |setup| setup.phase == SetupPhase::Done).await;
    assert_eq!(done.worktree_path, failed.worktree_path);
    assert_eq!(done.branch, failed.branch);
    assert_eq!(done.stages[2].exit_code, Some(0));
    assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn workspace_options_share_the_review_and_existing_worktrees_must_belong_to_the_repository() {
    let fixture = Fixture::new(None).await;
    let client = fixture.host.open_session();
    let review: agent_protocol::models::WorkspaceReview = call(
        &fixture.host,
        client.id(),
        Call::ReviewWorkspace(op::ReviewWorkspace {
            cwd: fixture.repository.to_string_lossy().into_owned(),
            include_options: true,
            base_ref: None,
        }),
    )
    .await
    .unwrap();
    let options = review.options.unwrap();
    assert!(options.is_repository);
    assert_eq!(options.default_base, "main");
    assert!(options.refs.iter().any(|reference| reference == "main"));
    assert!(
        options
            .worktrees
            .iter()
            .any(|path| Path::new(path) == fixture.repository)
    );
    let other = Fixture::new(None).await;
    let mut input = fixture.request("main", false);
    let Call::CreateSession(params) = &mut input else {
        unreachable!()
    };
    params.initial_input.as_mut().unwrap().workspace = Some(WorkspaceStrategy::ExistingWorktree {
        path: other.repository.to_string_lossy().into_owned(),
    });
    let (_, mut updates) = admit(&fixture.host, client.id(), &input).await;
    let failed = setup_until(&mut updates, |setup| setup.phase == SetupPhase::Failed).await;
    assert!(failed.error.unwrap().contains("another repository"));
    assert_eq!(fixture.agent.next.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn asynchronous_failure_does_not_block_or_resend_the_initial_input() {
    let fixture = Fixture::new(script("printf 'waiting\\n'; while [ ! -f .release ]; do sleep 0.02; done; printf 'async failure\\n'; exit 9", true)).await;
    let client = fixture.host.open_session();
    let (_, mut updates) = admit(&fixture.host, client.id(), &fixture.request("main", false)).await;
    let active = setup_until(&mut updates, |setup| {
        setup.stages[3].status == SetupStageStatus::Done
    })
    .await;
    assert_eq!(active.phase, SetupPhase::Running);
    assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
    std::fs::write(
        Path::new(active.worktree_path.as_ref().unwrap()).join(".release"),
        "",
    )
    .unwrap();
    let done = setup_until(&mut updates, |setup| setup.phase == SetupPhase::Done).await;
    assert_eq!(done.stages[2].status, SetupStageStatus::Failed);
    assert_eq!(done.stages[2].exit_code, Some(9));
    assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn closing_the_running_setup_terminal_reports_failure_without_starting_the_agent() {
    let fixture = Fixture::new(script("printf 'waiting\\n'; sleep 120", false)).await;
    let client = fixture.host.open_session();
    let (_, mut updates) = admit(&fixture.host, client.id(), &fixture.request("main", false)).await;
    let running = setup_until(&mut updates, |setup| {
        setup.stages[2]
            .tail
            .iter()
            .any(|line| line.contains("waiting"))
    })
    .await;
    let handle = running.terminal_handle.unwrap();
    let _: Empty = call(
        &fixture.host,
        client.id(),
        Call::StartTerminal(op::StartTerminal {
            handle: handle.clone(),
            cwd: running.worktree_path.unwrap(),
            command: None,
            size: op::TerminalSize { rows: 24, cols: 80 },
        }),
    )
    .await
    .unwrap();
    let _: Empty = call(
        &fixture.host,
        client.id(),
        Call::KillTerminal(op::TerminalKill {
            process_handle: handle,
        }),
    )
    .await
    .unwrap();
    let failed = setup_until(&mut updates, |setup| setup.phase == SetupPhase::Failed).await;
    assert_eq!(failed.stages[2].exit_code, Some(130));
    assert_eq!(fixture.agent.next.load(Ordering::SeqCst), 0);
    assert!(Path::new(failed.worktree_path.as_ref().unwrap()).exists());
}

#[cfg(unix)]
#[tokio::test]
async fn work_locally_cancels_setup_and_continues_the_same_input_at_the_project_root() {
    let fixture = Fixture::new(script("printf 'waiting\\n'; sleep 120", false)).await;
    let client = fixture.host.open_session();
    let (opened, mut updates) =
        admit(&fixture.host, client.id(), &fixture.request("main", false)).await;
    let running = setup_until(&mut updates, |setup| {
        setup.stages[2]
            .tail
            .iter()
            .any(|line| line.contains("waiting"))
    })
    .await;
    assert_eq!(fixture.agent.next.load(Ordering::SeqCst), 0);
    let _: Empty = call(
        &fixture.host,
        client.id(),
        Call::RetryPreparation(op::RetryPreparation {
            thread_id: opened.session.clone(),
            work_locally: true,
        }),
    )
    .await
    .unwrap();
    until(&mut updates, |change| {
        matches!(change, SessionChange::Preparation { preparation: None })
    })
    .await;
    let row = fixture
        .host
        .inner
        .conversations
        .get(&opened.session)
        .unwrap()
        .unwrap();
    assert_eq!(Path::new(&row.cwd), fixture.repository.as_path());
    assert!(row.workspace_setup.is_none());
    assert!(!Path::new(running.worktree_path.as_ref().unwrap()).exists());
    assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
}

use super::*;
use crate::{
    ProjectStore,
    host_rpc::{
        routing::{HostSubscription, SessionId},
        runtime::native_fixture::{NativeAgent, call},
    },
};
use agent_protocol::{
    conversation::{Command, InputContent, records},
    models::{Empty, Thread},
    protocol::{Call, Response},
    session::{PreparationStage, PreparationStatus, ProviderKind, SessionRef},
};
use std::{path::Path, sync::atomic::Ordering, time::Duration};

struct Updates {
    stream: HostSubscription,
    replica: records::Replica,
}
impl Updates {
    fn view(&self) -> Thread {
        records::project(&self.replica).unwrap().thread
    }
}
fn request(cwd: &Path) -> Call {
    Call::CreateConversation(records::Create {
        session: SessionRef::new(None, Uuid::new_v4().to_string()).unwrap(),
        provider: ProviderKind::Codex,
        workspace: None,
        subscription: Uuid::new_v4(),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        selection: Default::default(),
        initial: Some((
            Uuid::from_u128(42).to_string().into(),
            InputContent {
                parts: vec![op::Input::Text {
                    text: "Build the requested feature".into(),
                }],
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
            },
        )),
    })
}
async fn admit(host: &HostRpcService, client: SessionId, request: &Call) -> (SessionRef, Updates) {
    let reply = tokio::time::timeout(Duration::from_secs(5), host.dispatch(client, request))
        .await
        .unwrap()
        .unwrap();
    let Response::Success { result } =
        agent_protocol::protocol::decode::<Response<records::Opened>>(&reply.initial).unwrap()
    else {
        panic!("admission")
    };
    let replica = records::Replica::new(
        agent_protocol::sync::Scope::Conversation(result.session.clone()),
        1,
    )
    .apply(1, result.initial)
    .unwrap();
    (
        result.session,
        Updates {
            stream: reply.updates.unwrap(),
            replica,
        },
    )
}
async fn until(updates: &mut Updates, predicate: impl Fn(&Thread) -> bool) -> Thread {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let thread = updates.view();
            if predicate(&thread) {
                return thread;
            }
            let message =
                agent_protocol::protocol::decode(&updates.stream.recv().await.unwrap()).unwrap();
            updates.replica = updates.replica.apply(1, message).unwrap();
        }
    })
    .await
    .expect("preparation transition must arrive")
}
fn retry(id: SessionRef, work_locally: bool) -> Call {
    Call::ChangeConversation(records::Change {
        session: id,
        operation: Uuid::new_v4(),
        command: Command::RetryInput {
            id: Uuid::from_u128(42).to_string().into(),
            work_locally,
        },
    })
}

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
        let Call::CreateConversation(params) = &mut input else {
            unreachable!()
        };
        params.workspace = Some(WorkspaceStrategy::Worktree {
            base_ref: base.into(),
            start_from_origin: fetch,
        });
        input
    }
}
async fn setup_until(
    updates: &mut Updates,
    predicate: impl Fn(&WorkspaceSetup) -> bool,
) -> WorkspaceSetup {
    until(updates, |thread| {
        thread.workspace_setup.as_ref().is_some_and(&predicate)
    })
    .await
    .workspace_setup
    .unwrap()
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
    assert_eq!(updates.view().workspace_setup.unwrap().stages.len(), 4);
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
    let stopped: records::Acknowledged = call(
        &fixture.host,
        client.id(),
        Call::ChangeConversation(records::Change {
            session: opened.clone(),
            operation: Uuid::new_v4(),
            command: Command::Interrupt {
                turn: Some(Uuid::from_u128(42).to_string().into()),
            },
        }),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            stopped.outcome,
            agent_protocol::conversation::Outcome::Rejected(_)
        ),
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
    let (reopened, _) = admit(
        &fixture.host,
        client.id(),
        &Call::SubscribeConversation(records::Subscribe {
            session: opened.clone(),
            subscription: Uuid::new_v4(),
            cached: None,
            window: None,
        }),
    )
    .await;
    assert_eq!(reopened, opened);
    let reopened = fixture
        .host
        .inner
        .conversation
        .storage
        .history(&opened, 0)
        .unwrap()
        .thread;
    assert_eq!(reopened.workspace_setup, Some(running));
    drop(blocked);
    let done = setup_until(&mut updates, |setup| setup.phase == SetupPhase::Done).await;
    assert!(done.ended_at_ms.is_some());
    assert!(!done.can_cancel);
    let row = fixture
        .host
        .inner
        .conversation
        .storage
        .history(&opened, 0)
        .unwrap()
        .thread;
    assert!(row.preparations.is_empty());
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
            let Call::CreateConversation(params) = &mut input else {
                unreachable!()
            };
            params.workspace = Some(WorkspaceStrategy::Root);
        }
        let blocked = fixture.agent.create_gate.acquire().await.unwrap();
        let client = fixture.host.open_session();
        let (opened, mut updates) = admit(&fixture.host, client.id(), &input).await;
        until(&mut updates, |thread| {
            thread
                .preparations
                .iter()
                .any(|preparation| preparation.stage == PreparationStage::Provider)
        })
        .await;
        let row = fixture
            .host
            .inner
            .conversation
            .storage
            .history(&opened, 0)
            .unwrap()
            .thread;
        if root {
            assert!(row.workspace_setup.is_none());
            assert_eq!(
                Path::new(row.cwd.as_deref().unwrap()),
                fixture.repository.as_path()
            );
        } else {
            assert_eq!(
                row.workspace_setup.unwrap().stages[0].status,
                SetupStageStatus::Skipped
            );
            assert_ne!(
                Path::new(row.cwd.as_deref().unwrap()),
                fixture.repository.as_path()
            );
        }
        drop(blocked);
        until(&mut updates, |change| change.preparations.is_empty()).await;
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
    until(&mut updates, |thread| {
        thread
            .preparations
            .iter()
            .any(|preparation| matches!(preparation.status, PreparationStatus::Failed { .. }))
    })
    .await;
    tokio::task::yield_now().await;
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    until(&mut updates, |change| change.workspace_setup.is_none()).await;
    tokio::time::resume();
    let retained = fixture
        .host
        .inner
        .conversation
        .storage
        .header(&opened)
        .unwrap()
        .details
        .workspace
        .unwrap()
        .setup
        .unwrap();
    assert_eq!(retained.script, failed.script);
    let retried: records::Acknowledged =
        call(&fixture.host, client.id(), retry(opened.clone(), false))
            .await
            .unwrap();
    retried.accepted().unwrap();
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
    let Call::CreateConversation(params) = &mut input else {
        unreachable!()
    };
    params.workspace = Some(WorkspaceStrategy::ExistingWorktree {
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
    for existing in [false, true] {
        let fixture = Fixture::new(script("printf 'waiting\\n'; sleep 120", false)).await;
        let client = fixture.host.open_session();
        let mut creation = fixture.request("main", false);
        if existing {
            let path = fixture.directory.path().join("selected-checkout");
            crate::git::text(
                &fixture.repository,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    path.to_str().unwrap(),
                    "main",
                ],
            )
            .unwrap();
            let Call::CreateConversation(params) = &mut creation else {
                unreachable!()
            };
            params.workspace = Some(WorkspaceStrategy::ExistingWorktree {
                path: path.to_string_lossy().into_owned(),
            });
        }
        let (opened, mut updates) = admit(&fixture.host, client.id(), &creation).await;
        let running = setup_until(&mut updates, |setup| {
            setup.stages[2]
                .tail
                .iter()
                .any(|line| line.contains("waiting"))
        })
        .await;
        assert_eq!(fixture.agent.next.load(Ordering::SeqCst), 0);
        let retried: records::Acknowledged =
            call(&fixture.host, client.id(), retry(opened.clone(), true))
                .await
                .unwrap();
        retried.accepted().unwrap();
        until(&mut updates, |change| change.preparations.is_empty()).await;
        let row = fixture
            .host
            .inner
            .conversation
            .storage
            .history(&opened, 0)
            .unwrap()
            .thread;
        assert_eq!(
            Path::new(row.cwd.as_deref().unwrap()),
            fixture.repository.as_path()
        );
        assert!(row.workspace_setup.is_none());
        assert_eq!(
            Path::new(running.worktree_path.as_ref().unwrap()).exists(),
            existing
        );
        assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn automatic_workspace_preference_selects_the_initial_checkout() {
    for automatic in [false, true] {
        let fixture = Fixture::new(None).await;
        fixture
            .host
            .inner
            .worktrees
            .settings(Some(agent_protocol::models::WorktreeSettings {
                create_on_new_session: automatic,
                ..Default::default()
            }))
            .await
            .unwrap();
        let blocked = fixture.agent.create_gate.acquire().await.unwrap();
        let client = fixture.host.open_session();
        let (id, mut updates) =
            admit(&fixture.host, client.id(), &request(&fixture.repository)).await;
        let thread = until(&mut updates, |thread| {
            thread
                .preparations
                .iter()
                .any(|preparation| preparation.stage == PreparationStage::Provider)
        })
        .await;
        assert_eq!(thread.workspace_setup.is_some(), automatic);
        assert_eq!(
            Path::new(thread.cwd.as_deref().unwrap()) == fixture.repository,
            !automatic
        );
        drop(blocked);
        until(&mut updates, |thread| thread.preparations.is_empty()).await;
        assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
        assert_eq!(
            fixture
                .host
                .inner
                .conversation
                .storage
                .history(&id, 0)
                .unwrap()
                .thread
                .id
                .as_ref(),
            Some(&id)
        );
    }
}

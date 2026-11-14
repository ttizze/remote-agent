use super::*;
use crate::host_rpc::{
    routing::{HostSubscription, SessionId},
    runtime::native_fixture::{NativeAgent, call},
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
                author: None,
                fresh_context: false,
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
    .unwrap_or_else(|error| {
        panic!(
            "preparation transition must arrive: {error}; {:?}",
            updates.view()
        )
    })
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
        let host = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
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
        .history(&opened, None)
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
        .history(&opened, None)
        .unwrap()
        .thread;
    assert!(row.preparations.is_empty());
    assert_eq!(row.workspace_setup, Some(done.clone()));
    assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
    // Rebinding after a rewind uses ordinary input progress, not the finished
    // startup card or its original worktree-generation ownership.
    let finished = until(&mut updates, |thread| {
        thread.turns.as_ref().is_some_and(|turns| {
            turns
                .last()
                .is_some_and(|turn| turn.status == agent_protocol::execution::TurnStatus::Completed)
        })
    })
    .await;
    let turn = finished.turns.as_ref().unwrap().last().unwrap().id.clone();
    let generation = fixture
        .host
        .inner
        .conversation
        .storage
        .header(&opened)
        .unwrap()
        .details
        .workspace
        .unwrap()
        .generation;
    fixture
        .host
        .setup_update(
            &opened,
            generation,
            crate::host_rpc::storage::workspace::SetupChange::Expire {
                ended_at: done.ended_at_ms.unwrap(),
            },
        )
        .await
        .unwrap();
    let rewound: records::Acknowledged = call(
        &fixture.host,
        client.id(),
        Call::ChangeConversation(records::Change {
            session: opened.clone(),
            operation: Uuid::new_v4(),
            command: Command::Rewind(turn.clone()),
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        rewound.outcome,
        agent_protocol::conversation::Outcome::Accepted
    );
    let operation = Uuid::new_v4();
    let next: records::Acknowledged = call(
        &fixture.host,
        client.id(),
        Call::ChangeConversation(records::Change {
            session: opened.clone(),
            operation,
            command: Command::Submit {
                id: operation.to_string().into(),
                content: InputContent {
                    author: None,
                    fresh_context: false,
                    parts: vec![op::Input::Text {
                        text: "continue after rewind".into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
            },
        }),
    )
    .await
    .unwrap();
    assert_eq!(
        next.outcome,
        agent_protocol::conversation::Outcome::Accepted
    );
    let rebound = until(&mut updates, |thread| {
        thread.turns.as_ref().is_some_and(|turns| {
            turns.last().is_some_and(|latest| {
                latest.id != turn
                    && latest.status == agent_protocol::execution::TurnStatus::Completed
            })
        })
    })
    .await;
    assert!(rebound.workspace_setup.is_none());
    assert_eq!(
        fixture
            .host
            .inner
            .conversation
            .storage
            .header(&opened)
            .unwrap()
            .details
            .workspace
            .unwrap()
            .generation,
        generation
    );
    let checkout = Path::new(done.worktree_path.as_ref().unwrap());
    std::fs::write(checkout.join("uncommitted.txt"), "keep local work").unwrap();
    let branch = done.branch.as_ref().unwrap();
    let through = rebound.turns.as_ref().unwrap().last().unwrap().id.clone();
    for fork in [true, false] {
        let target = if fork {
            let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
            fixture
                .host
                .inner
                .conversation
                .storage
                .fork(&opened, &target, &through, 100)
                .unwrap();
            assert!(
                fixture
                    .host
                    .inner
                    .conversation
                    .storage
                    .header(&target)
                    .unwrap()
                    .details
                    .workspace
                    .is_none()
            );
            target
        } else {
            let reply: records::Acknowledged = call(
                &fixture.host,
                client.id(),
                Call::ChangeConversation(records::Change {
                    session: opened.clone(),
                    operation: Uuid::new_v4(),
                    command: Command::Rewind(through.clone()),
                }),
            )
            .await
            .unwrap();
            reply.accepted().unwrap();
            opened.clone()
        };
        let (_, mut updates) = admit(
            &fixture.host,
            client.id(),
            &Call::SubscribeConversation(records::Subscribe {
                session: target.clone(),
                subscription: Uuid::new_v4(),
                cached: None,
                window: None,
            }),
        )
        .await;
        let blocked = fixture.agent.create_gate.acquire().await.unwrap();
        let input = Uuid::new_v4();
        let submitted: records::Acknowledged = call(
            &fixture.host,
            client.id(),
            Call::ChangeConversation(records::Change {
                session: target.clone(),
                operation: input,
                command: Command::Submit {
                    id: input.to_string().into(),
                    content: InputContent {
                        author: None,
                        fresh_context: false,
                        parts: vec![op::Input::Text {
                            text: "prepare another turn".into(),
                        }],
                        model: None,
                        effort: None,
                        service_tier: None,
                        follow_up: Default::default(),
                    },
                },
            }),
        )
        .await
        .unwrap();
        submitted.accepted().unwrap();
        until(&mut updates, |thread| {
            thread
                .preparations
                .iter()
                .any(|preparation| preparation.stage == PreparationStage::Provider)
        })
        .await;
        let stopped: records::Acknowledged = call(
            &fixture.host,
            client.id(),
            Call::ChangeConversation(records::Change {
                session: target,
                operation: Uuid::new_v4(),
                command: Command::Interrupt {
                    turn: Some(input.to_string().into()),
                },
            }),
        )
        .await
        .unwrap();
        stopped.accepted().unwrap();
        until(&mut updates, |thread| {
            thread.preparations.iter().any(|preparation| {
                preparation.submission.client_user_message_id.as_str() == input.to_string()
                    && preparation.status == PreparationStatus::Interrupted
            })
        })
        .await;
        drop(blocked);
        assert_eq!(
            std::fs::read_to_string(checkout.join("uncommitted.txt")).unwrap(),
            "keep local work"
        );
        assert!(
            crate::git::text(
                checkout,
                &["rev-parse", "--verify", &format!("refs/heads/{branch}")]
            )
            .is_ok()
        );
        let saved = fixture
            .host
            .inner
            .conversation
            .storage
            .header(&opened)
            .unwrap()
            .details
            .workspace
            .unwrap();
        assert_eq!(saved.generation, generation);
        assert_eq!(saved.setup.unwrap().phase, SetupPhase::Done);
    }
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
            .history(&opened, None)
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
    for existing in [false, true] {
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
        let mut request = fixture.request("main", false);
        if existing {
            let Call::CreateConversation(params) = &mut request else {
                unreachable!()
            };
            params.workspace = Some(WorkspaceStrategy::ExistingWorktree {
                path: fixture.repository.to_string_lossy().into_owned(),
            });
        }
        let (opened, mut updates) = admit(&fixture.host, client.id(), &request).await;
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
        let expected = if existing {
            SetupStageStatus::Skipped
        } else {
            SetupStageStatus::Done
        };
        for id in [SetupStageId::Fetch, SetupStageId::Checkout] {
            assert_eq!(
                done.stages
                    .iter()
                    .find(|stage| stage.id == id)
                    .unwrap()
                    .status,
                expected
            );
        }
    }
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
    assert!(done.error.is_none());
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
    for (existing, failed) in [(false, false), (true, false), (false, true), (true, true)] {
        let command = if failed {
            "printf 'failed\\n'; exit 1"
        } else {
            "printf 'waiting\\n'; sleep 120"
        };
        let fixture = Fixture::new(script(command, false)).await;
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
            if failed {
                return setup.phase == SetupPhase::Failed;
            }
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
            .history(&opened, None)
            .unwrap()
            .thread;
        assert_eq!(
            Path::new(row.cwd.as_deref().unwrap()),
            fixture.repository.as_path()
        );
        assert!(row.workspace_setup.is_none());
        assert_eq!(
            Path::new(running.worktree_path.as_ref().unwrap()).exists(),
            existing || failed,
            "Work locally preserves a failed checkout, and only cancels a fresh running one"
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
            !automatic,
            "automatic={automatic}; projected={thread:?}; stored={:?}",
            fixture.host.inner.conversation.storage.header(&id).unwrap()
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
                .history(&id, None)
                .unwrap()
                .thread
                .id
                .as_ref(),
            Some(&id)
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn stop_cleanup_allows_retry_to_create_a_fresh_checkout() {
    stopped_checkout(true).await;
}
#[cfg(unix)]
#[tokio::test]
async fn stop_cleanup_allows_a_new_input_without_retry() {
    stopped_checkout(false).await;
}
#[cfg(unix)]
async fn stopped_checkout(retry_existing: bool) {
    let marker = tempfile::tempdir().unwrap();
    let flag = marker.path().join("attempted");
    let fixture = Fixture::new(script(&format!("if [ ! -f '{}' ]; then touch '{}'; printf 'waiting\\n'; sleep 120; fi; printf 'retried\\n'", flag.display(), flag.display()), false)).await;
    let client = fixture.host.open_session();
    let (id, mut updates) =
        admit(&fixture.host, client.id(), &fixture.request("main", false)).await;
    let running = setup_until(&mut updates, |setup| {
        setup.stages[2]
            .tail
            .iter()
            .any(|line| line.contains("waiting"))
    })
    .await;
    let stopped: records::Acknowledged = call(
        &fixture.host,
        client.id(),
        Call::ChangeConversation(records::Change {
            session: id.clone(),
            operation: Uuid::new_v4(),
            command: Command::Interrupt { turn: None },
        }),
    )
    .await
    .unwrap();
    stopped.accepted().unwrap();
    setup_until(&mut updates, |setup| setup.phase == SetupPhase::Cancelled).await;
    let stopped = until(&mut updates, |thread| {
        thread
            .preparations
            .iter()
            .any(|preparation| preparation.status == PreparationStatus::Interrupted)
    })
    .await;
    assert_eq!(stopped.preparations[0].stage, PreparationStage::SetupScript);
    setup_until(&mut updates, |setup| {
        setup.phase == SetupPhase::Cancelled
            && setup.worktree_path.is_none()
            && setup.branch.is_none()
    })
    .await;
    assert!(!Path::new(running.worktree_path.as_deref().unwrap()).exists());
    assert!(
        crate::git::text(
            &fixture.repository,
            &["branch", "--list", running.branch.as_deref().unwrap()]
        )
        .unwrap()
        .is_empty()
    );
    let header = fixture.host.inner.conversation.storage.header(&id).unwrap();
    assert_eq!(header.state.workspace.cwd(), fixture.repository.to_str());
    let setup = header.details.workspace.unwrap().setup.unwrap();
    assert!(setup.worktree_path.is_none());
    assert!(setup.branch.is_none());
    let request = if retry_existing {
        retry(id.clone(), false)
    } else {
        let removed: records::Acknowledged = call(
            &fixture.host,
            client.id(),
            Call::ChangeConversation(records::Change {
                session: id.clone(),
                operation: Uuid::new_v4(),
                command: Command::RemoveInput(Uuid::from_u128(42).to_string().into()),
            }),
        )
        .await
        .unwrap();
        removed.accepted().unwrap();
        let input = Uuid::new_v4();
        Call::ChangeConversation(records::Change {
            session: id.clone(),
            operation: input,
            command: Command::Submit {
                id: input.to_string().into(),
                content: InputContent {
                    author: None,
                    fresh_context: false,
                    parts: vec![op::Input::Text {
                        text: "new input after Stop".into(),
                    }],
                    model: None,
                    effort: None,
                    service_tier: None,
                    follow_up: Default::default(),
                },
            },
        })
    };
    let retried: records::Acknowledged = call(&fixture.host, client.id(), request).await.unwrap();
    retried.accepted().unwrap();
    let done = setup_until(&mut updates, |setup| setup.phase == SetupPhase::Done).await;
    assert!(!Path::new(running.worktree_path.as_deref().unwrap()).exists());
    assert_ne!(done.worktree_path, running.worktree_path);
    assert_ne!(done.branch, running.branch);
    assert_eq!(done.stages[2].exit_code, Some(0));
    assert!(
        done.stages[2]
            .tail
            .iter()
            .any(|line| line.contains("retried"))
    );
    assert_eq!(fixture.agent.inputs.lock().unwrap().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn restarted_host_restores_the_remaining_setup_card_lifetime() {
    let directory = tempfile::tempdir().unwrap();
    let id = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
    let generation = Uuid::new_v4();
    let host = HostRpcService::new([], directory.path().join("projects.json"));
    let ended = now().max(20_000) as u64 - 20_000;
    let mut setup = WorkspaceSetup::new(None, ended - 1);
    setup.finish(SetupPhase::Done, None, ended);
    host.inner
        .conversation
        .storage
        .create(
            &id,
            &records::Header {
                state: agent_protocol::conversation::Thread {
                    context_start: 0,
                    name: "Finished setup".into(),
                    provider: ProviderKind::Codex,
                    workspace: agent_protocol::conversation::Workspace::Ready("/project".into()),
                    queue_held: false,
                },
                details: records::Details {
                    workspace: Some(Box::new(WorkspacePreparation {
                        project_cwd: "/project".into(),
                        strategy: None,
                        generation,
                        created_worktree: false,
                        setup: Some(setup),
                        visible: true,
                    })),
                    ..Default::default()
                },
            },
        )
        .unwrap();
    let mut changes = host.inner.conversation.storage.changes();
    changes.borrow_and_update();
    let started = tokio::time::Instant::now();
    host.start().unwrap();
    tokio::time::timeout(Duration::from_secs(11), async {
        while host
            .inner
            .conversation
            .storage
            .header(&id)
            .unwrap()
            .details
            .workspace
            .as_ref()
            .unwrap()
            .visible
        {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("restart keeps the original expiry deadline");
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "the card keeps its original remaining lifetime"
    );
    assert!(started.elapsed() <= Duration::from_secs(11));
    assert!(
        host.inner
            .conversation
            .storage
            .header(&id)
            .unwrap()
            .details
            .workspace
            .unwrap()
            .setup
            .is_some()
    );
}

#[tokio::test]
async fn retry_after_restart_restores_a_deleted_checkout_before_the_first_provider_is_created() {
    let fixture = Fixture::new(None).await;
    fixture.agent.create_gate.close();
    let creation = fixture.request("main", false);
    let client = fixture.host.open_session();
    let (id, mut updates) = admit(&fixture.host, client.id(), &creation).await;
    let failed = until(&mut updates, |thread| {
        thread
            .preparations
            .iter()
            .any(|preparation| matches!(preparation.status, PreparationStatus::Failed { .. }))
    })
    .await;
    let checkout = failed.cwd.unwrap();
    assert_ne!(Path::new(&checkout), fixture.repository);
    assert_eq!(fixture.agent.next.load(Ordering::SeqCst), 0);
    fixture.host.shutdown_owned_processes().await;
    let agent = fixture.agent.restarted();
    let directory = fixture.directory;
    drop(updates);
    drop(client);
    drop(fixture.host);
    std::fs::remove_dir_all(&checkout).unwrap();
    let host = HostRpcService::new([agent.backend()], directory.path().join("projects.json"));
    let client = host.open_session();
    let (reopened, mut updates) = admit(&host, client.id(), &creation).await;
    assert_eq!(reopened, id);
    let result: records::Acknowledged = call(&host, client.id(), retry(id.clone(), false))
        .await
        .unwrap();
    result.accepted().unwrap();
    let done = until(&mut updates, |thread| {
        thread.preparations.is_empty() && !thread.turns.as_ref().is_none_or(Vec::is_empty)
    })
    .await;
    assert_eq!(done.cwd.as_deref(), Some(checkout.as_str()));
    assert!(Path::new(&checkout).join(".git").exists());
    assert_eq!(*agent.created_in.lock().unwrap(), [true]);
    assert_eq!(agent.inputs.lock().unwrap().len(), 1);
    assert_eq!(done.id, Some(id));
}

#[cfg(not(windows))]
#[tokio::test]
async fn cancellation_between_stages_stops_an_async_script_before_discarding_its_checkout() {
    use agent_protocol::conversation::{self as decision, Event, OperationId};
    use agent_protocol::workspace::WorkspaceSetup;
    tokio::time::timeout(Duration::from_secs(5), async {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("repository");
        std::fs::create_dir(&repository).unwrap();
        for args in [
            &["init", "-b", "main"][..],
            &["config", "user.name", "fixture"],
            &["config", "user.email", "fixture@example.test"],
            &["commit", "--allow-empty", "-m", "initial"],
        ] {
            crate::git::text(&repository, args).unwrap();
        }
        let host = HostRpcService::new([], directory.path().join("projects.json"));
        let checkout = host
            .inner
            .worktrees
            .prepare_selected(
                repository.to_str().unwrap(),
                "main".into(),
                false,
                Default::default(),
            )
            .await
            .unwrap();
        let branch = crate::git::text(&checkout, &["branch", "--show-current"])
            .unwrap()
            .trim()
            .to_owned();
        let generation = Uuid::new_v4();
        let target = SessionRef::new(None, Uuid::new_v4().to_string()).unwrap();
        let mut setup = WorkspaceSetup::new(Some("main".into()), 0);
        setup.worktree_path = Some(checkout.to_str().unwrap().into());
        setup.branch = Some(branch.clone());
        setup.can_cancel = true;
        let header = records::Header {
            state: decision::Thread {
                context_start: 0,
                name: String::new(),
                workspace: decision::Workspace::Ready(checkout.to_str().unwrap().into()),
                provider: ProviderKind::Codex,
                queue_held: false,
            },
            details: records::Details {
                workspace: Some(Box::new(records::WorkspacePreparation {
                    project_cwd: repository.to_str().unwrap().into(),
                    strategy: Some(WorkspaceStrategy::Worktree {
                        base_ref: "main".into(),
                        start_from_origin: false,
                    }),
                    generation,
                    created_worktree: true,
                    setup: Some(setup),
                    visible: true,
                })),
                ..Default::default()
            },
        };
        host.inner
            .conversation
            .storage
            .create(&target, &header)
            .unwrap();
        crate::host_rpc::runtime::Runtime::recover(&host, &target, ProviderKind::Codex)
            .await
            .unwrap();
        let input: agent_protocol::ids::ClientInputId = Uuid::new_v4().to_string().into();
        host.inner
            .conversation
            .storage
            .decide(
                &target,
                Event::Command {
                    id: OperationId::Client(Uuid::parse_str(input.as_str()).unwrap()),
                    receipt: None,
                    command: Command::Submit {
                        id: input.clone(),
                        content: InputContent {
                author: None,
                fresh_context: false,
                            parts: vec![op::Input::Text {
                                text: "do work".into(),
                            }],
                            model: None,
                            effort: None,
                            service_tier: None,
                            follow_up: Default::default(),
                        },
                    },
                },
                decision::Context {
                rewind_context_start: None,preparation:None,rewind_binding:None,imported_turns:&[],external_busy:false,now:100,seed:Uuid::new_v4(),steers:false,steer_ready:true},
                None,
            )
            .unwrap();
        host.run_setup_script(
            &target,
            generation,
            (checkout.to_str().unwrap(), repository.to_str().unwrap()),
            Some(SetupScript {
                name: "async".into(),
                command: "while test ! -f .released; do sleep 0.05; done".into(),
                asynchronous: true,
            }),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let handle = host
            .inner
            .conversation
            .storage
            .header(&target)
            .unwrap()
            .details
            .workspace
            .unwrap()
            .setup
            .unwrap()
            .terminal_handle
            .unwrap();
        assert!(host.inner.terminals.is_setup(&handle));
        let connection = host.open_session();
        let result: records::Acknowledged = call(
            &host,
            connection.id(),
            Call::ChangeConversation(records::Change {
                session: target.clone(),
                operation: Uuid::new_v4(),
                command: Command::Interrupt {
                    turn: Some(input.to_string().into()),
                },
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, decision::Outcome::Accepted);
        let mut changed = host.inner.conversation.storage.changes();
        loop {
            let workspace = host
                .inner
                .conversation
                .storage
                .header(&target)
                .unwrap()
                .details
                .workspace
                .unwrap();
            if !workspace.created_worktree {
                break;
            }
            changed.changed().await.unwrap();
        }
        // Finished setup terminals retain their replay until the card expires,
        // as on main. Attaching must immediately report the actual PTY exit.
        let mut viewer=host.open_session();
        let _:Empty=call(&host,viewer.id(),Call::StartTerminal(op::StartTerminal {
            handle:handle.clone(),cwd:checkout.to_str().unwrap().into(),command:None,size:op::TerminalSize {rows:24,cols:80},
        })).await.unwrap();
        loop {
            let notification:agent_protocol::protocol::Notification=agent_protocol::protocol::decode(&viewer.recv().await.unwrap()).unwrap();
            if matches!(notification,agent_protocol::protocol::Notification::Exited {handle:exited,..} if exited==handle) {break}
        }

        assert!(!checkout.exists());
        assert!(
            crate::git::text(&repository, &["branch", "--list", &branch])
                .unwrap()
                .is_empty()
        );
        assert!(
            host.inner
                .conversation
                .storage
                .state(&target)
                .unwrap()
                .attempts
                .is_empty()
        );
    })
    .await
    .unwrap();
}

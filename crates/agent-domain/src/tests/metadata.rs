use super::*;

fn update() -> Command {
    Command::UpdateMetadata {
        title: None,
        regenerate_title: None,
        branch: None,
        worktree_path: None,
        expected_worktree_path: None,
        expected_empty: false,
        limit_recovery: None,
        linked_pull_request: None,
        project_root: None,
    }
}
fn with(change: impl FnOnce(&mut Command)) -> Command {
    let mut command = update();
    change(&mut command);
    command
}
macro_rules! set {
    ($command:expr, $field:ident, $value:expr) => {
        if let Command::UpdateMetadata { $field, .. } = $command {
            *$field = $value;
        }
    };
}

#[test]
fn metadata_updates_follow_the_reference_guards() {
    let mut s = state();
    let worktree = with(|c| {
        set!(c, worktree_path, Some(Some("/wt/feature".into())));
        set!(c, branch, Some(Some("feature".into())));
        set!(c, expected_empty, true);
    });
    let step = command(&mut s, "worktree", worktree);
    assert_eq!(step.reply, Reply::Accepted);
    let workspace = s.thread.as_ref().unwrap().workspace.clone().unwrap();
    assert_eq!(workspace.cwd, "/wt/feature");
    assert_eq!(workspace.branch.as_deref(), Some("feature"));
    assert!(step.effects.iter().any(|effect| effect.body
        == EffectBody::DetachSessions {
            reason: "Workspace changed.".into(),
            revoke_credentials: false,
            instance: None,
        }));
    assert_eq!(
        command(
            &mut s,
            "stale",
            with(|c| set!(c, expected_worktree_path, Some(Some("/wt/other".into())))),
        )
        .reply,
        Reply::Rejected {
            reason: "worktree-changed".into()
        }
    );
    let branch_only = command(
        &mut s,
        "branch",
        with(|c| {
            set!(c, expected_worktree_path, Some(Some("/wt/feature".into())));
            set!(c, branch, Some(Some("renamed".into())));
        }),
    );
    assert!(branch_only.effects.is_empty());
    running(&mut s, "first");
    assert_eq!(
        command(&mut s, "empty", with(|c| set!(c, expected_empty, true))).reply,
        Reply::Rejected {
            reason: "thread-not-empty".into()
        }
    );
    let pull_request = LinkedPullRequest {
        project: "project".into(),
        repository: "owner/repo".into(),
        number: 7,
        url: "https://example.test/pull/7".into(),
    };
    command(
        &mut s,
        "link",
        with(|c| set!(c, linked_pull_request, Some(Some(pull_request.clone())))),
    );
    assert_eq!(shell(&s).unwrap().linked_pull_request, Some(pull_request));
    command(
        &mut s,
        "regenerate",
        with(|c| set!(c, regenerate_title, Some(true))),
    );
    assert!(shell(&s).unwrap().title_regenerating);
    command(
        &mut s,
        "abandon",
        with(|c| set!(c, regenerate_title, Some(false))),
    );
    assert!(!shell(&s).unwrap().title_regenerating);
    command(
        &mut s,
        "local",
        with(|c| {
            set!(c, worktree_path, Some(None));
            set!(c, project_root, Some("/repo".into()));
        }),
    );
    let workspace = s.thread.as_ref().unwrap().workspace.clone().unwrap();
    assert_eq!(
        (workspace.cwd.as_str(), workspace.worktree_path),
        ("/repo", None)
    );
}

// Automatic settlement loses to any change made after its snapshot and
// otherwise settles like a user settle.
#[test]
fn automatic_settlement_needs_an_unchanged_thread() {
    let mut s = state();
    command(
        &mut s,
        "order",
        Command::ReorderActive { order: "a0".into() },
    );
    let snapshot = s.thread.as_ref().unwrap().updated_at.clone();
    let stale = Timestamp::from_millis(snapshot.millis() - 1).unwrap();
    assert_eq!(
        command(
            &mut s,
            "stale",
            Command::SettleAutomatically {
                snapshot_at: stale,
                settled_at: None,
            },
        )
        .reply,
        Reply::Rejected {
            reason: "thread-changed".into()
        }
    );
    command(
        &mut s,
        "settle",
        Command::SettleAutomatically {
            snapshot_at: snapshot,
            settled_at: None,
        },
    );
    let thread = s.thread.as_ref().unwrap();
    assert_eq!(thread.settled, Some(true));
    assert_eq!(thread.active_order, None);
    command(
        &mut s,
        "unsettle",
        Command::Settle {
            settled: false,
            at: None,
        },
    );
    let snapshot = s.thread.as_ref().unwrap().updated_at.clone();
    assert!(matches!(
        command(
            &mut s,
            "after-unsettle",
            Command::SettleAutomatically {
                snapshot_at: snapshot,
                settled_at: None,
            },
        )
        .reply,
        Reply::Rejected { .. }
    ));
}

// A plan on another thread of the project is completed by the run that
// implements it.
#[test]
fn a_message_can_implement_another_threads_plan() {
    let mut source = state();
    let (_, attempt) = running(&mut source, "planning");
    provider(
        &mut source,
        "plan",
        &attempt,
        ProviderEvent::Plan {
            kind: PlanKind::Proposed,
            key: "plan".into(),
            markdown: "Do it".into(),
            steps: vec![],
        },
    );
    let plan = source.plans[0].id.clone();
    let implement = |resolved: Option<ResolvedPlan>| {
        let Command::Send(mut message) = send_message("implement", DispatchMode::StartImmediately)
        else {
            unreachable!()
        };
        message.source_plan = Some(PlanRef {
            thread: ThreadId::new("thread").unwrap(),
            plan: plan.clone(),
        });
        message.resolved_plan = resolved;
        Command::Send(message)
    };
    let mut target = State::default();
    command(
        &mut target,
        "create",
        Command::Create {
            thread: ThreadId::new("target").unwrap(),
            project: "project".into(),
            title: "Target".into(),
            selection: selection(),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: None,
            created_by: crate::MessageAuthor::User,
            creation_source: "desktop".into(),
        },
    );
    let resolved = |project: &str, implemented| ResolvedPlan {
        project: project.into(),
        kind: PlanKind::Proposed,
        implemented,
    };
    for (key, plan, reason) in [
        ("missing", None, "plan-not-found"),
        (
            "other-project",
            Some(resolved("other", false)),
            "plan-in-another-project",
        ),
        ("done", Some(resolved("project", true)), "plan-not-active"),
    ] {
        assert_eq!(
            command(&mut target, key, implement(plan)).reply,
            Reply::Rejected {
                reason: reason.into()
            }
        );
    }
    let step = command(
        &mut target,
        "implement",
        implement(Some(resolved("project", false))),
    );
    let Reply::Run(run) = step.reply.clone() else {
        panic!("{:?}", step.reply)
    };
    let Some(Command::ImplementPlan {
        plan: marked,
        run: by,
    }) = step.effects.iter().find_map(|effect| match &effect.body {
        EffectBody::SendToThread { thread, command } if thread.as_str() == "thread" => {
            Some(command.as_ref().clone())
        }
        _ => None,
    })
    else {
        panic!("{:?}", step.effects)
    };
    assert_eq!((&marked, &by), (&plan, &run));
    command(
        &mut source,
        "mark",
        Command::ImplementPlan {
            plan: marked,
            run: by,
        },
    );
    assert_eq!(source.plans[0].implemented_by, Some(run));
}

/// The project root the Host fills in on first dispatch is not part of the
/// command's identity, so a resent command returns its first result.
#[test]
fn a_resent_update_without_the_host_filled_root_replays_its_result() {
    let mut s = state();
    let first = command(
        &mut s,
        "clear",
        with(|c| {
            set!(c, worktree_path, Some(None));
            set!(c, project_root, Some("/project".into()));
        }),
    );
    let replay = ThreadMachine::step(
        &s,
        &InputEnvelope {
            at: at(),
            key: "clear".into(),
            input: Input::Command {
                id: CommandId::new("clear").unwrap(),
                command: Box::new(with(|c| set!(c, worktree_path, Some(None)))),
                receipt: first.receipt.clone(),
            },
        },
    );
    assert_eq!(replay.reply, first.reply);
    assert_eq!(replay.receipt, first.receipt);
}

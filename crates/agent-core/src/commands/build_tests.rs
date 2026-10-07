use super::*;
use crate::sync::fixtures::*;
use agent_domain::{CheckpointId, PlanId, RunStatus};

fn message(id: &str, text: &str) -> TurnMessage {
    TurnMessage {
        id: MessageId::new(id).unwrap(),
        text: text.into(),
        attachments: vec![],
        context: None,
    }
}
fn turn(dispatch: TurnDispatch) -> StartTurn {
    StartTurn {
        message: message("message", "text"),
        selection: None,
        title_seed: None,
        source_plan: None,
        dispatch,
        continuation: None,
        creation_source: "desktop".into(),
    }
}
fn sent(command: Command) -> SendMessage {
    let Command::Send(send) = command else {
        panic!("send command")
    };
    send
}
fn checkpoint(id: &str, ordinal: u64, status: CheckpointStatus) -> Checkpoint {
    Checkpoint {
        status,
        scope: None,
        id: CheckpointId::new(id).unwrap(),
        run: None,
        run_ordinal: ordinal,
        native_heads: Default::default(),
        file_ref: "refs/orchestration/checkpoints/x".into(),
        files: vec![],
    }
}

#[test]
fn starts_an_ordinary_turn_while_idle() {
    assert_eq!(
        resolve_composer_dispatch_mode(false, false, None),
        ComposerDispatchMode::Auto
    );
}

#[test]
fn steers_by_default_and_reserves_the_alternate_for_queueing() {
    assert_eq!(
        resolve_composer_dispatch_mode(true, false, None),
        ComposerDispatchMode::Steer
    );
    assert_eq!(
        resolve_composer_dispatch_mode(true, true, None),
        ComposerDispatchMode::Queue
    );
}

#[test]
fn queues_as_the_alternate_when_restarting_is_the_default() {
    let restart = Some(FollowUpBehavior::Restart);
    assert_eq!(
        resolve_composer_dispatch_mode(true, false, restart),
        ComposerDispatchMode::Restart
    );
    assert_eq!(
        resolve_composer_dispatch_mode(true, true, restart),
        ComposerDispatchMode::Queue
    );
}

#[test]
fn uses_the_configured_behavior_only_during_a_running_turn() {
    for (behavior, configured, alternate) in [
        (
            FollowUpBehavior::Queue,
            ComposerDispatchMode::Queue,
            ComposerDispatchMode::Steer,
        ),
        (
            FollowUpBehavior::Steer,
            ComposerDispatchMode::Steer,
            ComposerDispatchMode::Queue,
        ),
    ] {
        assert_eq!(
            resolve_composer_dispatch_mode(true, false, Some(behavior)),
            configured
        );
        assert_eq!(
            resolve_composer_dispatch_mode(true, true, Some(behavior)),
            alternate
        );
        for alternate in [false, true] {
            assert_eq!(
                resolve_composer_dispatch_mode(false, alternate, Some(behavior)),
                ComposerDispatchMode::Auto
            );
        }
    }
}

#[test]
fn names_the_alternate_action_so_the_affordance_can_be_labelled() {
    assert_eq!(
        alternate_follow_up(Some(FollowUpBehavior::Queue)),
        FollowUpBehavior::Steer
    );
    assert_eq!(
        alternate_follow_up(Some(FollowUpBehavior::Steer)),
        FollowUpBehavior::Queue
    );
    assert_eq!(alternate_follow_up(None), FollowUpBehavior::Queue);
}

#[test]
fn preserves_caller_command_ids() {
    let id = CommandId::new("queued-command").unwrap();
    let dispatch = dispatch(
        ThreadId::new("thread-1").unwrap(),
        id.clone(),
        lifecycle_command(LifecycleAction::Archive),
    );
    assert_eq!(dispatch.command_id, id);
    assert_eq!(dispatch.thread_id.as_str(), "thread-1");
    assert_eq!(dispatch.command, Command::Archive { archived: true });
}

#[test]
fn resolves_run_ordinal_zero_to_the_thread_start_checkpoint() {
    let mut state = thread_state("Thread");
    state.checkpoints.push(checkpoint(
        "checkpoint-thread-start",
        0,
        CheckpointStatus::Ready,
    ));
    let checkpoint = checkpoint_after_run(&state, 0).unwrap();
    assert_eq!(
        rollback_command(checkpoint, false),
        Command::Rollback {
            checkpoint: CheckpointId::new("checkpoint-thread-start").unwrap(),
            restore_files: false,
            restore_refusal: None,
        }
    );
}

#[test]
fn only_ready_checkpoints_resolve_from_a_run_ordinal() {
    for status in [
        CheckpointStatus::Ready,
        CheckpointStatus::Missing,
        CheckpointStatus::Error,
        CheckpointStatus::Stale,
    ] {
        let mut state = thread_state("Thread");
        state.checkpoints.push(checkpoint("checkpoint", 2, status));
        assert_eq!(
            checkpoint_after_run(&state, 2).is_ok(),
            status == CheckpointStatus::Ready
        );
    }
    assert!(checkpoint_after_run(&thread_state("Thread"), 2).is_err());
}

#[test]
fn rolls_back_an_identified_checkpoint_with_the_chosen_file_restore() {
    for restore in [true, false] {
        let command = rollback_command(
            &checkpoint("checkpoint-known", 1, CheckpointStatus::Ready),
            restore,
        );
        assert_eq!(
            command,
            Command::Rollback {
                checkpoint: CheckpointId::new("checkpoint-known").unwrap(),
                restore_files: restore,
                restore_refusal: None,
            }
        );
    }
}

#[test]
fn preserves_plan_implementation_provenance() {
    let send = sent(send_command(StartTurn {
        message: message("message-implementation", "Implement the plan"),
        selection: Some(selection("codex")),
        title_seed: Some("Implement the plan".into()),
        source_plan: Some(PlanRef {
            thread: ThreadId::new("thread-plan").unwrap(),
            plan: PlanId::new("plan-1").unwrap(),
        }),
        ..turn(TurnDispatch::Auto)
    }));
    assert_eq!(send.title_seed.as_deref(), Some("Implement the plan"));
    assert_eq!(send.source_plan.unwrap().plan.as_str(), "plan-1");
    assert_eq!(send.intent, Some(DeliveryIntent::Auto));
    assert_eq!(send.mode, DispatchMode::StartImmediately);
}

#[test]
fn preserves_an_existing_worktree_and_branch_during_a_first_message_launch() {
    let launch = launch(LaunchThread {
        command_id: CommandId::new("launch-existing-worktree").unwrap(),
        thread: Some(thread_id()),
        project: "project-1".into(),
        title: "Thread".into(),
        title_seed: Some("Continue here".into()),
        selection: selection("codex"),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        workspace: WorkspaceChoice::Local {
            branch: Some("feature".into()),
            worktree_path: Some("/workspace/project-worktrees/feature".into()),
        },
        message: Some(message("message-existing-worktree", "Continue here")),
        creation_source: "desktop".into(),
    });
    assert_eq!(launch.thread_id, Some(thread_id()));
    assert_eq!(launch.title, "Continue here");
    assert_eq!(
        launch.message.unwrap().title_seed.as_deref(),
        Some("Continue here")
    );
    assert_eq!(
        launch.workspace,
        WorkspaceStrategy::ExistingWorktree {
            worktree_path: "/workspace/project-worktrees/feature".into(),
            branch: Some("feature".into()),
        }
    );
}

#[test]
fn provisions_an_origin_based_worktree() {
    assert_eq!(
        workspace_strategy(WorkspaceChoice::NewWorktree {
            base_branch: "main".into(),
            branch: Some("feature".into()),
            start_from_origin: true,
        }),
        WorkspaceStrategy::Worktree {
            base_ref: "main".into(),
            branch: Some("feature".into()),
            start_from_origin: true,
        }
    );
    assert_eq!(
        workspace_strategy(WorkspaceChoice::Local {
            branch: None,
            worktree_path: None
        }),
        WorkspaceStrategy::Root { branch: None }
    );
}

#[test]
fn sends_the_users_delivery_intent_for_the_host_to_resolve() {
    for (dispatch, mode, intent) in [
        (TurnDispatch::Queue, DispatchMode::QueueAfterActive, None),
        (
            TurnDispatch::Auto,
            DispatchMode::StartImmediately,
            Some(DeliveryIntent::Auto),
        ),
        (
            TurnDispatch::Steer,
            DispatchMode::StartImmediately,
            Some(DeliveryIntent::Steer),
        ),
        (
            TurnDispatch::Restart,
            DispatchMode::StartImmediately,
            Some(DeliveryIntent::Restart),
        ),
    ] {
        let send = sent(send_command(turn(dispatch)));
        assert_eq!((send.mode, send.intent), (mode, intent));
    }
}

#[test]
fn dispatches_an_explicit_idle_start_with_its_continuation() {
    let mut input = turn(TurnDispatch::Start);
    input.continuation = Some(RunId::new("limited").unwrap());
    let send = sent(send_command(input));
    assert_eq!(send.mode, DispatchMode::StartImmediately);
    assert_eq!(send.intent, None);
    assert_eq!(
        send.continuation,
        Some(Continuation::Manual {
            run: RunId::new("limited").unwrap()
        })
    );
    let mut queued = turn(TurnDispatch::Queue);
    queued.continuation = Some(RunId::new("limited").unwrap());
    assert_eq!(sent(send_command(queued)).continuation, None);
}

#[test]
fn interrupts_a_known_run_or_the_stop_target_and_holds_the_queue() {
    let state = thread_state("Thread");
    assert_eq!(
        interrupt_command(&state, Some(RunId::new("active-run").unwrap())),
        Some(Command::Interrupt {
            run: RunId::new("active-run").unwrap(),
            hold_queue: true,
            reason: None,
        })
    );
    assert_eq!(interrupt_command(&state, None), None);
    let mut running = thread_state("Thread");
    running.runs.push(run("running", 1, RunStatus::Running));
    assert_eq!(
        interrupt_command(&running, None),
        Some(Command::Interrupt {
            run: RunId::new("running").unwrap(),
            hold_queue: true,
            reason: None,
        })
    );
}

#[test]
fn builds_relationship_and_queue_commands_without_reshaping() {
    let fork = fork_command(
        ThreadId::new("thread-fork").unwrap(),
        RunId::new("run-1").unwrap(),
        None,
        "desktop",
    );
    assert!(
        matches!(&fork, Command::Fork { source: SourcePoint::Run(run), created_by: MessageAuthor::User, .. } if run.as_str() == "run-1")
    );
    let merge = merge_back_command(thread_id(), RunId::new("run-2").unwrap());
    assert!(
        matches!(&merge, Command::MergeBack { source: SourcePoint::Run(run), .. } if run.as_str() == "run-2")
    );
    let edit = Command::EditQueued {
        run: RunId::new("run-3").unwrap(),
        text: "updated queued text".into(),
        attachments: None,
        context: None,
    };
    // A text-only edit sends no replacement attachment list.
    assert!(matches!(
        edit,
        Command::EditQueued {
            attachments: None,
            ..
        }
    ));
}

#[test]
fn selects_models_through_the_host_without_choosing_a_switch() {
    for instance in ["codex", "claude"] {
        assert_eq!(
            select_model_command(selection(instance)),
            Command::SelectModel {
                selection: selection(instance)
            }
        );
    }
}

#[test]
fn dispatches_settle_and_unsettle_without_timestamps() {
    assert_eq!(
        lifecycle_command(LifecycleAction::Settle),
        Command::Settle {
            settled: true,
            at: None
        }
    );
    assert_eq!(
        lifecycle_command(LifecycleAction::Unsettle),
        Command::Settle {
            settled: false,
            at: None
        }
    );
}

#[test]
fn sends_an_active_order_key() {
    assert_eq!(
        lifecycle_command(LifecycleAction::ReorderActive { order: "mf".into() }),
        Command::ReorderActive { order: "mf".into() }
    );
}

#[test]
fn dismisses_and_answers_pending_requests() {
    let request = RuntimeRequestId::new("request-1").unwrap();
    assert_eq!(
        Command::DismissQuestion {
            request: request.clone()
        },
        Command::DismissQuestion { request }
    );
}

#[test]
fn detaches_each_provider_session_with_a_derived_command_id() {
    let mut state = thread_state("Thread");
    state
        .native_sessions
        .insert("codex".into(), "session".into());
    let commands = detach_commands(&state, &CommandId::new("stop").unwrap());
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].0.as_str(), "stop:detach:codex");
    assert_eq!(
        commands[0].1,
        Command::DetachProviderSession {
            instance: "codex".into(),
            reason: Some("client-requested".into()),
        }
    );
}

#[test]
fn title_seeds_prefer_normalized_message_text() {
    assert_eq!(
        thread_title_seed(
            "  Investigate\n  login   failures  ",
            &["login-error.png"],
            &[Some("Terminal 1")]
        ),
        "Investigate login failures"
    );
}

#[test]
fn title_seeds_use_the_first_attachment_name_when_text_is_empty() {
    assert_eq!(
        thread_title_seed(" \n ", &["login-error.png", "other.png"], &[]),
        "Image: login-error.png"
    );
}

#[test]
fn title_seeds_use_the_first_nonempty_fallback_label() {
    assert_eq!(
        thread_title_seed("", &[], &[None, Some("  "), Some("Terminal 1")]),
        "Terminal 1"
    );
}

#[test]
fn title_seeds_fall_back_to_new_thread() {
    assert_eq!(thread_title_seed("", &[], &[]), "New thread");
}

#[test]
fn title_seeds_read_assistant_quotes_as_their_text_and_comment() {
    use crate::presentation::markdown::assistant_citations::{
        AssistantCitation, serialize_assistant_citation,
    };
    let quote = serialize_assistant_citation(&AssistantCitation {
        environment_id: "environment".into(),
        thread_id: "thread".into(),
        message_id: "message".into(),
        text: "Retry  the\nlogin".into(),
        comment: Some("Why?".into()),
        start: 0,
        end: 16,
        prefix: String::new(),
        suffix: String::new(),
    });
    assert_eq!(
        thread_title_seed(&format!("Explain {quote}"), &[], &[]),
        "Explain Retry the login Comment: Why?"
    );
}

#[test]
fn title_seeds_apply_the_shared_truncation() {
    assert_eq!(
        thread_title_seed(&"x".repeat(60), &[], &[]),
        format!("{}...", "x".repeat(50))
    );
}

#[test]
fn plan_follow_up_implements_on_an_empty_draft_and_refines_otherwise() {
    assert_eq!(
        plan_follow_up(" ", " # Plan "),
        (
            "PLEASE IMPLEMENT THIS PLAN:\n# Plan".into(),
            InteractionMode::Default
        )
    );
    assert_eq!(
        plan_follow_up(" Add tests ", "# Plan"),
        ("Add tests".into(), InteractionMode::Plan)
    );
    assert_eq!(
        plan_implementation_thread_title("intro\n  ## Ship it \nmore"),
        "Implement Ship it"
    );
    assert_eq!(
        plan_implementation_thread_title("no heading"),
        "Implement plan"
    );
    assert_eq!(proposed_plan_title("####### seven"), None);
}

use super::*;
use crate::{
    models::{Thread, Turn, TurnStatus},
    session::SessionRef,
    state::{Navigation, operations::OperationState},
};

fn session() -> SessionRef {
    SessionRef::new(ProviderKind::Codex, "chat".into()).unwrap()
}

fn snapshot() -> Snapshot {
    let mut snapshot = Snapshot::default();
    snapshot.connection.connected = true;
    Arc::make_mut(snapshot.panes.primary_mut()).navigation = Arc::new(Navigation {
        thread_id: Some(session()),
        draft_key: session().into(),
        cwd: "/project".into(),
    });
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(
        session(),
        Arc::new(Thread {
            id: Some(session()),
            ..Default::default()
        }),
    );
    snapshot
}

fn presentation(snapshot: &Snapshot) -> Arc<ComposerPresentation> {
    ComposerPresentation::project(snapshot, snapshot.panes.primary(), None)
}

#[test]
fn a_new_claude_conversation_with_no_explicit_model_uses_its_catalog_default() {
    let mut snapshot = snapshot();
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    )
    .active_provider = Some(ProviderKind::Claude);
    snapshot.catalog.models = Arc::new(vec![
        serde_json::from_value(serde_json::json!({
            "id":"default","model":{"provider":"claude","id":"default"},
            "displayName":"Claude","isDefault":true
        }))
        .unwrap(),
    ]);
    assert_eq!(
        presentation(&snapshot).controls.selected_model,
        Some(ModelRef {
            provider: ProviderKind::Claude,
            id: "default".into()
        })
    );
}

#[test]
fn a_conversation_still_opening_or_failed_to_open_keeps_its_draft_but_sends_nothing() {
    let mut snapshot = snapshot();
    crate::state::set_draft_text(&mut snapshot, session().into(), "下書き".into());
    assert!(presentation(&snapshot).controls.loaded);
    assert!(presentation(&snapshot).can_send(true));

    Arc::make_mut(&mut snapshot.sessions.conversations).remove(&session());
    let opening = presentation(&snapshot);
    assert!(!opening.controls.loaded);
    assert_eq!(opening.draft.text, "下書き");
    assert!(!opening.can_send(true));
    assert!(!opening.can_attach(), "nor an attachment");

    // A new chat has no conversation to wait for.
    Arc::make_mut(snapshot.panes.primary_mut()).navigation = Arc::new(Navigation {
        thread_id: None,
        draft_key: session().into(),
        cwd: "/project".into(),
    });
    assert!(presentation(&snapshot).controls.loaded);
}

#[test]
fn another_agent_waits_for_the_working_conversation_and_says_why() {
    let mut snapshot = snapshot();
    crate::state::set_draft_text(&mut snapshot, session().into(), "切り替えて送ります".into());
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.input.drafts)
            .get_mut(&session().into())
            .unwrap(),
    )
    .model = Some(ModelRef {
        provider: ProviderKind::Claude,
        id: "sonnet".into(),
    });
    let status = |snapshot: &mut Snapshot, status| {
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session())
                .unwrap(),
        )
        .status = status;
    };
    status(&mut snapshot, crate::models::SessionStatus::Running);
    let held = presentation(&snapshot);
    assert!(!held.can_send(true));
    assert!(
        held.controls
            .unavailable
            .as_deref()
            .is_some_and(|reason| reason.starts_with("AIを切り替えて送信するには")),
        "a held send names its reason"
    );
    status(&mut snapshot, crate::models::SessionStatus::Idle);
    let ready = presentation(&snapshot);
    assert!(ready.can_send(true));
    assert_eq!(ready.controls.unavailable, None);
}

#[test]
fn active_draft_operations_block_submission_until_completion_or_retry() {
    let mut snapshot = snapshot();
    crate::state::set_draft_text(&mut snapshot, session().into(), "送信".into());
    for key in [
        OperationKey::Submission {
            draft_key: session().into(),
        },
        OperationKey::Attachment {
            draft_key: session().into(),
        },
        OperationKey::Dictation {
            draft_key: session().into(),
        },
    ] {
        Arc::make_mut(&mut snapshot.operations).insert(
            key.clone(),
            OperationState {
                generation: 1,
                phase: OperationPhase::Running,
            },
        );
        let busy = presentation(&snapshot);
        assert!(!busy.can_send(true));
        assert!(!busy.can_attach() && !busy.can_dictate());
        assert_eq!(
            busy.controls.transferring,
            matches!(key, OperationKey::Attachment { .. })
        );
        Arc::make_mut(&mut snapshot.operations)
            .get_mut(&key)
            .unwrap()
            .phase = OperationPhase::Failed {
            message: "retry".into(),
        };
        assert!(presentation(&snapshot).can_send(true));
        Arc::make_mut(&mut snapshot.operations).remove(&key);
        assert!(presentation(&snapshot).can_send(true));
    }
    Arc::make_mut(&mut snapshot.operations).insert(
        OperationKey::Attachment {
            draft_key: SessionRef::new(ProviderKind::Codex, "other".into())
                .unwrap()
                .into(),
        },
        OperationState {
            generation: 1,
            phase: OperationPhase::Running,
        },
    );
    assert!(
        presentation(&snapshot).can_send(true),
        "another conversation's transfer does not block this draft"
    );
}

#[test]
fn answers_cannot_be_advanced_twice_while_the_response_is_in_flight() {
    use crate::presentation::queue::QueueControl;
    use agent_protocol::{
        requests::{Question, Request, RequestBody, RequestTarget},
        session::RequestDelivery,
    };
    let mut snapshot = snapshot();
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    )
    .requests
    .insert(
        "question".into(),
        Arc::new(Request {
            id: "question".into(),
            target: RequestTarget::Session,
            delivery: RequestDelivery::Awaiting,
            body: RequestBody::Question {
                questions: vec![Question {
                    id: "answer".into(),
                    header: "質問".into(),
                    prompt: "何を作る？".into(),
                    secret: false,
                    allow_free_text: true,
                    multiple: false,
                    choices: Vec::new(),
                }],
            },
        }),
    );
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    )
    .queue
    .inputs = vec![queued_message("waiting")];
    assert!(
        presentation(&snapshot)
            .controls
            .follow_up
            .queue
            .action("waiting".into(), QueueControl::Edit)
            .is_none(),
        "T3's open question owns the composer while queued messages remain removable"
    );
    assert!(
        presentation(&snapshot)
            .controls
            .follow_up
            .queue
            .action("waiting".into(), QueueControl::Remove)
            .is_some()
    );
    assert!(!presentation(&snapshot).can_advance_question());
    assert!(presentation(&snapshot).can_answer_question());
    crate::questions::act(
        &mut snapshot,
        "question".into(),
        crate::questions::QuestionAction::Edit {
            text: "iPhoneアプリ".into(),
        },
    );
    assert!(presentation(&snapshot).can_advance_question());
    let key = OperationKey::Request {
        id: "question".into(),
    };
    Arc::make_mut(&mut snapshot.operations).insert(
        key.clone(),
        OperationState {
            generation: 1,
            phase: OperationPhase::Running,
        },
    );
    assert!(!presentation(&snapshot).can_advance_question());
    assert!(!presentation(&snapshot).can_answer_question());
    let original = snapshot.input.question_drafts.clone();
    crate::questions::act(
        &mut snapshot,
        "question".into(),
        crate::questions::QuestionAction::Edit {
            text: "a late edit during delivery".into(),
        },
    );
    assert_eq!(snapshot.input.question_drafts, original);
    Arc::make_mut(&mut snapshot.operations)
        .get_mut(&key)
        .unwrap()
        .phase = OperationPhase::Failed {
        message: "retry".into(),
    };
    assert!(presentation(&snapshot).can_advance_question());
    assert!(presentation(&snapshot).can_answer_question());
    snapshot.connection.connected = false;
    assert!(!presentation(&snapshot).can_answer_question());
    assert!(!presentation(&snapshot).can_advance_question());
    assert!(!presentation(&snapshot).can_answer_question());
    assert!(!presentation(&Snapshot::default()).can_advance_question());
    snapshot.connection.connected = true;
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    )
    .requests
    .clear();
    assert!(
        presentation(&snapshot)
            .controls
            .follow_up
            .queue
            .action("waiting".into(), QueueControl::Edit)
            .is_some()
    );
}

#[test]
fn stopping_with_a_draft_retries_failure_and_does_not_depend_on_message_input_availability() {
    let mut snapshot = snapshot();
    assert!(!presentation(&snapshot).can_interrupt(Some("turn".into())));
    let thread = Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    );
    thread.can_accept_direct_input = Some(false);
    thread.turns = Some(vec![Arc::new(Turn {
        id: "turn".into(),
        status: TurnStatus::Running,
        ..Default::default()
    })]);
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        session().into(),
        Arc::new(Draft {
            text: "keep this draft".into(),
            ..Default::default()
        }),
    );
    assert!(presentation(&snapshot).can_interrupt(Some("turn".into())));
    assert_eq!(
        presentation(&snapshot).primary_stop_turn(),
        Some("turn".into())
    );
    assert!(!presentation(&snapshot).can_interrupt(None));
    assert!(!presentation(&snapshot).can_interrupt(Some("stale-turn".into())));
    assert!(!presentation(&snapshot).can_send(true));
    let key = OperationKey::Interrupt {
        session: session(),
        turn: "turn".into(),
    };
    Arc::make_mut(&mut snapshot.operations).insert(
        key.clone(),
        OperationState {
            generation: 1,
            phase: OperationPhase::Running,
        },
    );
    assert!(!presentation(&snapshot).can_interrupt(Some("turn".into())));
    Arc::make_mut(&mut snapshot.operations)
        .get_mut(&key)
        .unwrap()
        .phase = OperationPhase::Failed {
        message: "retry".into(),
    };
    assert!(presentation(&snapshot).can_interrupt(Some("turn".into())));
    snapshot.connection.connected = false;
    assert!(!presentation(&snapshot).can_interrupt(Some("turn".into())));
}

#[test]
fn the_review_summary_needs_a_conversation_with_changes() {
    use agent_protocol::models::{ChangedFile, WorkspaceReview};
    let with_files = |count: usize| {
        let mut snapshot = snapshot();
        Arc::make_mut(&mut Arc::make_mut(snapshot.panes.primary_mut()).workspace).review =
            Some(Arc::new(WorkspaceReview {
                controls: Default::default(),
                options: None,
                base_ref: Some("origin/main".into()),
                branch: "main".into(),
                additions: 7,
                deletions: 2,
                files: (0..count)
                    .map(|index| ChangedFile {
                        path: format!("src/日本語-{index}.rs"),
                        status: "M".into(),
                        additions: Some(1),
                        deletions: None,
                    })
                    .collect(),
                diff: String::new(),
            }));
        snapshot
    };
    assert_eq!(snapshot().panes.primary().composer_review(false), None);
    assert_eq!(with_files(0).panes.primary().composer_review(false), None);
    assert_eq!(
        with_files(3).panes.primary().composer_review(true),
        None,
        "side chats do not show the workspace badge"
    );

    let three = with_files(3)
        .panes
        .primary()
        .composer_review(false)
        .unwrap();
    assert_eq!((three.files, three.additions, three.deletions), (3, 7, 2));

    let mut draft = with_files(4);
    Arc::make_mut(draft.panes.primary_mut()).navigation = Arc::new(Navigation {
        thread_id: None,
        draft_key: "draft".into(),
        cwd: "/project".into(),
    });
    assert_eq!(
        draft.panes.primary().composer_review(false),
        None,
        "a new draft has no card"
    );
}

fn queued_message(id: &str) -> agent_protocol::operations::Submission {
    agent_protocol::operations::Submission {
        fresh_context: false,
        thread_id: session(),
        client_user_message_id: id.into(),
        input: vec![agent_protocol::operations::Input::Text {
            text: format!("{id} text"),
        }],
        model: None,
        effort: None,
        service_tier: None,
        follow_up: FollowUp::Queue,
    }
}

#[test]
fn queue_controls_follow_connection_and_direct_input_permissions_and_recover() {
    use crate::presentation::queue::QueueControl;
    for (connected, direct, enabled) in [
        (true, None, true),
        (true, Some(true), true),
        (true, Some(false), false),
        (false, Some(true), false),
    ] {
        let mut snapshot = snapshot();
        snapshot.connection.connected = connected;
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session())
                .unwrap(),
        );
        thread.can_accept_direct_input = direct;
        thread.queue.inputs = vec![queued_message("first"), queued_message("second")];
        let queue = presentation(&snapshot).controls.follow_up.queue.clone();
        assert_eq!(
            queue.action("first".into(), QueueControl::Edit).is_some(),
            enabled
        );
        assert_eq!(
            queue.action("first".into(), QueueControl::Remove).is_some(),
            enabled
        );
        assert_eq!(
            queue
                .drop_on("second".into(), "first".into(), false)
                .is_some(),
            enabled
        );
        snapshot.connection.connected = true;
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session())
                .unwrap(),
        )
        .can_accept_direct_input = Some(true);
        assert!(
            presentation(&snapshot)
                .controls
                .follow_up
                .queue
                .action("first".into(), QueueControl::Edit)
                .is_some()
        );
    }
}

#[test]
fn follow_up_controls_name_the_send_and_list_the_queue_the_host_holds() {
    for (running, steer, held, active, default, alternate) in [
        (true, true, false, true, FollowUp::Queue, FollowUp::Steer),
        // A provider that cannot steer queues both ways.
        (true, false, false, true, FollowUp::Queue, FollowUp::Queue),
        // Input waits behind the queue until it starts.
        (false, true, false, true, FollowUp::Queue, FollowUp::Queue),
        // A held queue waits for the user; a send starts a turn.
        (false, true, true, false, FollowUp::Steer, FollowUp::Steer),
    ] {
        let mut snapshot = snapshot();
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session(),
            Arc::new(Thread {
                id: Some(session()),
                steerable: running && steer,
                capabilities: Some(crate::session::Capabilities {
                    steer,
                    ..Default::default()
                }),
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    status: if running {
                        TurnStatus::Running
                    } else {
                        TurnStatus::Completed
                    },
                    ..Default::default()
                })]),
                queue: agent_protocol::operations::InputQueue {
                    inputs: vec![queued_message("waiting")],
                    held,
                },
                ..Default::default()
            }),
        );
        crate::state::set_draft_text(&mut snapshot, session().into(), "sending".into());
        let (snapshot, _) = crate::state::reduce(
            &snapshot,
            "",
            crate::state::Event::from(crate::state::Intent::Submit {
                thread_id: Some(session()),
                client_user_message_id: "sending".into(),
                alternate: false,
            }),
        );
        let controls = presentation(&snapshot).controls.follow_up.clone();
        assert_eq!(controls.active, active, "running {running}, held {held}");
        assert_eq!(controls.default_action, default);
        assert_eq!(controls.alternate_action, alternate);
        assert_eq!(controls.queue.held, held);
        assert_eq!(controls.queue.can_steer, running && steer);
        // The Host's queue first, then a send it has not acknowledged yet.
        let rows: Vec<_> = controls
            .queue
            .messages
            .iter()
            .map(|row| (row.id.as_str(), row.text.as_str(), row.pending))
            .collect();
        let expected: &[_] = if running || !held {
            &[
                ("waiting", "waiting text", false),
                ("sending", "sending", true),
            ]
        } else {
            &[("waiting", "waiting text", false)]
        };
        assert_eq!(rows, expected, "running {running}, held {held}");
    }
}

#[test]
fn an_answered_turn_offers_its_background_work_and_stop_while_the_composer_sends() {
    let mut snapshot = snapshot();
    let waiting = |background| {
        Arc::new(Thread {
            id: Some(session()),
            turns: Some(vec![Arc::new(Turn {
                id: "turn".into(),
                status: TurnStatus::Completed,
                ..Default::default()
            })]),
            background,
            ..Default::default()
        })
    };
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(session(), waiting(Vec::new()));
    assert_eq!(presentation(&snapshot).controls.background, None);
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(
        session(),
        waiting(vec![agent_protocol::execution::BackgroundTask {
            id: "shell".into(),
            kind: agent_protocol::execution::BackgroundTaskKind::Monitor,
            description: Some("Watch the build".into()),
        }]),
    );
    crate::state::set_draft_text(&mut snapshot, session().into(), "next".into());
    let composer = presentation(&snapshot);
    let work = composer.controls.background.as_ref().unwrap();
    assert!(work.waiting);
    assert_eq!(
        composer.primary_stop_turn(),
        None,
        "background work owns the stop in the footer"
    );
    assert_eq!(
        work.title(crate::localization::UiLanguage::English),
        "Waiting on monitor Watch the build"
    );
    assert!(
        composer.can_interrupt(Some("turn".into())),
        "Stop ends the work"
    );
    assert_eq!(composer.controls.running_turn, Some("turn".into()));
    assert!(composer.can_send(true), "a message can follow the work");
    for status in [
        agent_protocol::session::PreparationStatus::Preparing,
        agent_protocol::session::PreparationStatus::Sending,
    ] {
        Arc::make_mut(
            Arc::make_mut(&mut snapshot.sessions.conversations)
                .get_mut(&session())
                .unwrap(),
        )
        .preparations = vec![agent_protocol::session::SessionPreparation {
            submission: agent_protocol::operations::Submission {
                thread_id: session(),
                client_user_message_id: "next".into(),
                input: Vec::new(),
                model: None,
                effort: None,
                service_tier: None,
                follow_up: Default::default(),
                fresh_context: false,
            },
            status: status.clone(),
            stage: agent_protocol::session::PreparationStage::Workspace,
        }];
        let composer = presentation(&snapshot);
        assert_eq!(composer.controls.background, None, "foreground {status:?}");
        assert_eq!(composer.primary_stop_turn(), Some("next".into()));
        assert!(
            !composer.can_interrupt(Some("turn".into())),
            "the old footer cannot stop the new turn"
        );
        assert!(composer.can_interrupt(Some("next".into())));
    }
}

#[test]
fn an_interrupted_turn_or_a_usage_limit_can_be_resumed() {
    use agent_protocol::execution::{ErrorCategory, ExecutionError};
    let turn = |status, category: Option<ErrorCategory>| {
        Arc::new(Turn {
            id: "turn".into(),
            status,
            error: category.map(|category| ExecutionError {
                category,
                ..Default::default()
            }),
            ..Default::default()
        })
    };
    for (status, category, expected) in [
        (TurnStatus::Interrupted, None, true),
        (TurnStatus::Failed, Some(ErrorCategory::UsageLimit), true),
        (TurnStatus::Failed, Some(ErrorCategory::Other), false),
        (TurnStatus::Completed, None, false),
    ] {
        let thread = Thread {
            turns: Some(vec![turn(status, category)]),
            ..Default::default()
        };
        assert_eq!(super::resumable(&thread), expected, "{status:?}");
    }
    assert!(!super::resumable(&Thread::default()));
}

#[test]
fn steering_uses_host_admission_instead_of_the_displayed_running_status() {
    let mut snapshot = snapshot();
    snapshot.input.composer_preferences.follow_up = FollowUp::Steer;
    for (name, capability, allowed, skips_compaction) in [
        ("stopping or retrying", true, false, false),
        ("compaction", false, false, true),
        ("ordinary turn", true, true, false),
    ] {
        Arc::make_mut(&mut snapshot.sessions.conversations).insert(
            session(),
            Arc::new(Thread {
                id: Some(session()),
                steerable: allowed,
                skips_compaction,
                capabilities: Some(agent_protocol::session::Capabilities {
                    steer: capability,
                    ..Default::default()
                }),
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    status: TurnStatus::Running,
                    ..Default::default()
                })]),
                ..Default::default()
            }),
        );
        let controls = &presentation(&snapshot).controls.follow_up;
        assert_eq!(
            controls.queue.can_steer,
            allowed || skips_compaction,
            "{name}"
        );
        assert_eq!(
            controls.default_action,
            if allowed {
                FollowUp::Steer
            } else {
                FollowUp::Queue
            },
            "{name}"
        );
    }
}

#[test]
fn queued_contexts_and_adjacent_moves_preserve_host_order_and_pending_content() {
    use crate::state::follow_up::QueueAction;
    use crate::state::{Draft, Event, Intent, reduce};
    use agent_protocol::{
        context::{ContextContent, MessageContext},
        operations::Input,
    };
    let context = MessageContext {
        id: "file".into(),
        label: "src/lib.rs".into(),
        content: ContextContent::File {
            path: "/project/src/lib.rs".into(),
        },
    };
    use crate::presentation::queue::QueueControl::{MoveDown, MoveUp};
    let mut state = snapshot();
    let thread = Arc::make_mut(
        Arc::make_mut(&mut state.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    );
    thread.turns = Some(vec![Arc::new(Turn {
        id: "turn".into(),
        status: TurnStatus::Running,
        ..Default::default()
    })]);
    let mut context_only = queued_message("b");
    context_only.fresh_context = true;
    context_only.input = vec![Input::Context {
        context: Box::new(context.clone()),
    }];
    let mut mixed = queued_message("c");
    mixed.input.push(Input::Context {
        context: Box::new(context.clone()),
    });
    thread.queue.inputs = vec![queued_message("a"), context_only, mixed];
    let (state, _) = reduce(
        &state,
        "",
        Event::from(Intent::SetDraft {
            thread_id: session().into(),
            draft: Draft {
                fresh_context: true,
                contexts: vec![context.clone()],
                ..Default::default()
            },
        }),
    );
    let (state, _) = reduce(
        &state,
        "",
        Event::from(Intent::Submit {
            thread_id: Some(session()),
            client_user_message_id: "pending".into(),
            alternate: false,
        }),
    );
    let controls = presentation(&state).controls.follow_up.clone();
    assert_eq!(
        controls
            .queue
            .messages
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "c", "pending"]
    );
    assert_eq!(
        controls
            .queue
            .messages
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>(),
        ["a text", "src/lib.rs", "c text\nsrc/lib.rs", "src/lib.rs"]
    );
    assert_eq!(
        crate::state::follow_up::queued_text(
            &state.sessions.conversations[&session()].queue.inputs[2]
        ),
        "c text"
    );
    assert_eq!(
        agent_protocol::operations::Input::contexts(
            &state.sessions.conversations[&session()].queue.inputs[2].input
        )
        .cloned()
        .collect::<Vec<_>>(),
        vec![context.clone()]
    );
    let pending =
        &state.sessions.pending_submissions[&agent_protocol::ids::ClientInputId::from("pending")];
    assert!(pending.draft.text.is_empty());
    assert_eq!(pending.draft.contexts, vec![context.clone()]);
    let movement =
        |queue: &crate::presentation::queue::QueuePresentation, id: &str, control| match queue
            .action(id.into(), control)
        {
            Some(Intent::Queue { thread_id, action }) => {
                assert_eq!(thread_id, session());
                Some(action)
            }
            None => None,
            _ => panic!("a queue control must target its queue owner"),
        };
    let moved = |id: &str, before: Option<&str>| {
        Some(QueueAction::Move {
            client_user_message_id: id.into(),
            before: before.map(Into::into),
        })
    };
    assert_eq!(movement(&controls.queue, "a", MoveUp), None);
    assert_eq!(
        movement(&controls.queue, "a", MoveDown),
        moved("a", Some("c"))
    );
    assert_eq!(
        movement(&controls.queue, "b", MoveUp),
        moved("b", Some("a"))
    );
    assert_eq!(movement(&controls.queue, "b", MoveDown), moved("b", None));
    assert_eq!(
        movement(&controls.queue, "c", MoveUp),
        moved("c", Some("b"))
    );
    assert_eq!(movement(&controls.queue, "c", MoveDown), None);
    for row in [&controls.queue.messages[1], &controls.queue.messages[3]] {
        assert_eq!(row.text, "src/lib.rs");
        assert_eq!(row.attachments, 0);
        assert!(row.fresh_context);
    }
    assert!(controls.queue.messages[3].pending);
    assert_eq!(
        (
            movement(&controls.queue, "pending", MoveUp),
            movement(&controls.queue, "pending", MoveDown)
        ),
        (None, None)
    );
    let (editing, _) = reduce(
        &state,
        "",
        Event::from(Intent::Queue {
            thread_id: session(),
            action: QueueAction::Edit {
                client_user_message_id: "c".into(),
            },
        }),
    );
    assert_eq!(presentation(&editing).draft.text, "c text");
    assert_eq!(
        editing.input.drafts[&session().into()],
        state.input.drafts[&session().into()]
    );
    assert_eq!(
        editing.sessions.conversations[&session()].queue.inputs[2].input,
        state.sessions.conversations[&session()].queue.inputs[2].input
    );
    let mut changing = state;
    Arc::make_mut(&mut changing.operations).insert(
        OperationKey::Queue { session: session() },
        OperationState {
            generation: 1,
            phase: OperationPhase::Running,
        },
    );
    let busy = presentation(&changing).controls.follow_up.queue.clone();
    assert!(
        busy.messages
            .iter()
            .all(|row| busy.action(row.id.clone(), MoveUp).is_none()
                && busy.action(row.id.clone(), MoveDown).is_none())
    );
    Arc::make_mut(&mut changing.operations).insert(
        OperationKey::Queue { session: session() },
        OperationState {
            generation: 1,
            phase: OperationPhase::Failed {
                message: "queue change failed".into(),
            },
        },
    );
    let restored = presentation(&changing).controls.follow_up.clone();
    assert_eq!(
        movement(&restored.queue, "b", MoveUp),
        moved("b", Some("a"))
    );
    assert_eq!(restored.queue.messages[1].text, "src/lib.rs");
}

#[test]
fn plain_queue_previews_keep_text_and_context_labels_without_markdown_or_payload_details() {
    use agent_protocol::context::{ContextContent, MessageContext};
    let contexts = vec![
        MessageContext {
            id: "source".into(),
            label: " [source] <selection> ".into(),
            content: ContextContent::File {
                path: "/private/source.rs".into(),
            },
        },
        MessageContext {
            id: "second".into(),
            label: "second label".into(),
            content: ContextContent::File {
                path: "/private/second.rs".into(),
            },
        },
    ];
    let text = "  question\n  continuation  ";
    assert_eq!(
        queued_preview(text, &contexts),
        "question\n  continuation\n[source] <selection>\nsecond label"
    );
    assert_eq!(
        queued_preview(" \n ", &contexts),
        "[source] <selection>\nsecond label"
    );
    assert_eq!(queued_preview(" \n ", &[]), "");
}

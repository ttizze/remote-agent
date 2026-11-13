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
fn resume_continues_the_same_interrupted_conversation_without_replacing_a_draft() {
    let mut snapshot = snapshot();
    let thread = Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    );
    thread.turns = Some(vec![Arc::new(Turn {
        id: "interrupted".into(),
        status: TurnStatus::Interrupted,
        ..Default::default()
    })]);
    let composer = presentation(&snapshot);
    assert!(composer.can_resume());
    let intents = composer.resume_intents("continuation".into());
    assert!(
        matches!(&intents[..], [crate::state::Intent::EditComposer {thread_id,text,..},crate::state::Intent::Submit {thread_id:Some(target),client_user_message_id,alternate:false}]
        if thread_id == &session().into() && target == &session() && text == "Continue where you left off." && client_user_message_id.as_str() == "continuation")
    );
    crate::state::set_draft_text(&mut snapshot, session().into(), "Keep this draft".into());
    assert!(!presentation(&snapshot).can_resume());
    assert!(
        presentation(&snapshot)
            .resume_intents("unused".into())
            .is_empty()
    );
}

#[test]
fn resume_releases_held_input_in_order_instead_of_submitting_an_extra_message() {
    let mut snapshot = snapshot();
    let thread = Arc::make_mut(
        Arc::make_mut(&mut snapshot.sessions.conversations)
            .get_mut(&session())
            .unwrap(),
    );
    thread.queue.held = true;
    thread
        .queue
        .inputs
        .push(agent_protocol::operations::Submission {
            thread_id: session(),
            client_user_message_id: "waiting".into(),
            input: vec![],
            model: None,
            effort: None,
            service_tier: None,
            follow_up: FollowUp::Queue,
        });
    let composer = presentation(&snapshot);
    assert!(composer.can_resume());
    assert!(
        matches!(&composer.resume_intents("unused".into())[..],[crate::state::Intent::Queue {thread_id,action:crate::state::follow_up::QueueAction::Resume}] if thread_id == &session())
    );
    snapshot.connection.connected = false;
    assert!(
        presentation(&snapshot)
            .resume_intents("unused".into())
            .is_empty()
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
    assert!(!presentation(&snapshot).can_advance_question());
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
    Arc::make_mut(&mut snapshot.operations)
        .get_mut(&key)
        .unwrap()
        .phase = OperationPhase::Failed {
        message: "retry".into(),
    };
    assert!(presentation(&snapshot).can_advance_question());
    snapshot.connection.connected = false;
    assert!(!presentation(&snapshot).can_advance_question());
    assert!(!presentation(&Snapshot::default()).can_advance_question());
}

#[test]
fn stopping_retries_failure_and_does_not_depend_on_message_input_availability() {
    let mut snapshot = snapshot();
    assert!(!presentation(&snapshot).can_interrupt());
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
    assert!(presentation(&snapshot).can_interrupt());
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
    assert!(!presentation(&snapshot).can_interrupt());
    Arc::make_mut(&mut snapshot.operations)
        .get_mut(&key)
        .unwrap()
        .phase = OperationPhase::Failed {
        message: "retry".into(),
    };
    assert!(presentation(&snapshot).can_interrupt());
    snapshot.connection.connected = false;
    assert!(!presentation(&snapshot).can_interrupt());
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

#[test]
fn follow_up_controls_name_the_send_and_list_the_queue_the_host_holds() {
    let queued = |id: &str| agent_protocol::operations::Submission {
        thread_id: session(),
        client_user_message_id: id.into(),
        input: vec![agent_protocol::operations::Input::Text {
            text: format!("{id} text"),
        }],
        model: None,
        effort: None,
        service_tier: None,
        follow_up: FollowUp::Queue,
    };
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
                    inputs: vec![queued("waiting")],
                    held,
                },
                ..Default::default()
            }),
        );
        crate::state::set_draft_text(&mut snapshot, session().into(), "sending".into());
        let (snapshot, _) = crate::state::reduce(
            &snapshot,
            "",
            crate::state::Event::Intent(crate::state::Intent::Submit {
                thread_id: Some(session()),
                client_user_message_id: "sending".into(),
                alternate: false,
            }),
        );
        let controls = presentation(&snapshot).controls.follow_up.clone();
        assert_eq!(controls.active, active, "running {running}, held {held}");
        assert_eq!(controls.default_action, default);
        assert_eq!(controls.alternate_action, alternate);
        assert_eq!(controls.held, held);
        assert_eq!(controls.can_steer, running && steer);
        // The Host's queue first, then a send it has not acknowledged yet.
        let rows: Vec<_> = controls
            .queue
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
        work.title(crate::localization::UiLanguage::English),
        "Waiting on monitor Watch the build"
    );
    assert!(composer.can_interrupt(), "Stop ends the work");
    assert_eq!(composer.controls.running_turn, Some("turn".into()));
    assert!(composer.can_send(true), "a message can follow the work");
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

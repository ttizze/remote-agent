use agent_core::state::DraftKey;
use agent_core::state::{
    Draft, Effect, Event, Intent, Snapshot, operations as op, operations::Operation,
};
use agent_protocol::models::{
    AssistantPhase, Item, ItemBody, ItemStatus, Thread, ThreadResponse, Turn, TurnStatus,
};
use agent_protocol::session::{ProviderKind, SessionRef};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};

use agent_core::state::reduce;

// Corpus changes are already BEX domain data, scoped to a fixture session.
fn fixture_change(previous: &Snapshot, params: Value) -> (Snapshot, Vec<Effect>) {
    let id: SessionRef = serde_json::from_value(params["session"].clone()).unwrap();
    let change: agent_protocol::session::SessionChange =
        serde_json::from_value(params["change"].clone()).unwrap();
    if !previous.conversations.contains_key(&id) {
        let active = match &change {
            agent_protocol::session::SessionChange::Status { status } => {
                *status == agent_protocol::models::SessionStatus::Running
            }
            agent_protocol::session::SessionChange::Turn { completed, .. } => !completed,
            _ => return (previous.clone(), Vec::new()),
        };
        return agent_core::state::reduce(
            previous,
            Event::Notification(agent_protocol::protocol::Notification::Activity {
                session: id.clone(),
                active,
                finished: matches!(&change, agent_protocol::session::SessionChange::Turn {completed:true, turn} if turn.status == agent_protocol::execution::TurnStatus::Completed),
            }),
        );
    }
    let mut source = previous.clone();
    let subscription = uuid::Uuid::nil();
    Arc::make_mut(&mut source.subscriptions).insert(id.clone(), subscription);
    let (mut next, effects) = agent_core::state::reduce(
        &source,
        Event::SessionUpdate(Box::new(agent_protocol::session::SessionUpdate {
            subscription_id: subscription,
            change,
        })),
    );
    next.subscriptions = previous.subscriptions.clone();
    (next, effects)
}

fn initial(thread: Thread) -> Snapshot {
    Snapshot {
        conversations: Arc::new(BTreeMap::from([(
            thread.id.clone().unwrap(),
            Arc::new(thread),
        )])),
        ..Default::default()
    }
}

fn applied<O: Operation>(
    previous: &Snapshot,
    operation: O,
    output: O::Output,
) -> (Snapshot, Vec<Effect>) {
    let mut next = previous.clone();
    let effects = operation.apply(&mut next, output);
    (next, effects)
}
fn reply(thread: Thread) -> agent_protocol::session::OpenedSession {
    agent_protocol::session::OpenedSession {
        session: thread.id.clone().unwrap(),
        subscription_id: uuid::Uuid::nil(),
        response: ThreadResponse {
            thread,
            model: None,
        },
    }
}

#[test]
fn snapshot_preserves_history_drafts_and_navigation() {
    let mut snapshot = initial(
        serde_json::from_value(json!({
            "id":{"provider":"codex","id":"thread"}, "historyCursor":null,
            "turns":[{"id":"oldest"},{"id":"latest"}]
        }))
        .unwrap(),
    );
    snapshot.drafts = Arc::new(BTreeMap::from([(
        SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }
        .into(),
        Arc::new(agent_core::state::Draft {
            text: "unsent input".into(),
            ..Default::default()
        }),
    )]));
    snapshot.navigation = Arc::new(agent_core::state::Navigation {
        thread_id: Some(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }),
        cwd: "/fixture".into(),
        draft_key: SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }
        .into(),
    });
    snapshot.pending_submissions = Arc::new(BTreeMap::from([(
        "sent".into(),
        Arc::new(agent_core::state::PendingSubmission {
            sequence: 0,
            draft_key: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            draft: snapshot.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            })]
                .clone(),
            turn_id: Some("latest".into()),
            after_item_id: None,
            accepted: true,
            delivery_unknown: false,
        }),
    )]));
    let restored: Snapshot =
        serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    assert_eq!(
        restored, snapshot,
        "persisted snapshots must retain loaded history after reopening"
    );
}

#[test]
fn selected_folder_preserves_explicit_scope_without_losing_the_execution_directory() {
    for (project_id, selected) in [
        (None, "/workspace"),
        (Some(Value::Null), ""),
        (Some(json!("project")), "/workspace"),
    ] {
        let mut thread = json!({"id":{"provider":"codex","id":"thread"},"cwd":"/workspace"});
        if let Some(project_id) = project_id {
            thread["projectId"] = project_id;
        }
        let thread: Thread = serde_json::from_value(thread).unwrap();
        let (snapshot, _) = applied(
            &Snapshot {
                connected: true,
                ..Default::default()
            },
            op::ReadThread::open(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }),
            reply(thread),
        );
        let restored: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        for snapshot in [&snapshot, &restored] {
            assert_eq!(snapshot.navigation.cwd, "/workspace");
            assert_eq!(snapshot.selected_directory(), selected);
            assert_eq!(
                snapshot.workspace.review_cwd.as_deref(),
                (!selected.is_empty()).then_some("/workspace")
            );
            for cwd in ["", "/another-project"] {
                let (next, _) = reduce(
                    snapshot,
                    Event::Intent(agent_core::state::Intent::NewChat { cwd: cwd.into() }),
                );
                assert_eq!(next.selected_directory(), cwd);
            }
        }
    }
}

#[test]
fn pending_submission_reconciles_both_reply_and_echo_orders() {
    use agent_core::state::Intent;
    for echo_first in [false, true] {
        let previous = initial(
            serde_json::from_value(
                json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"turn","items":[],"status":"unknown"}]}),
            )
            .unwrap(),
        );
        let (pending, _) = reduce(
            &previous,
            Event::Intent(Intent::Submit {
                thread_id: Some(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }),
                client_user_message_id: "client".into(),
            }),
        );
        let mut finished = pending.clone();
        for (index, echo) in [echo_first, !echo_first].into_iter().enumerate() {
            if echo {
                finished = fixture_change(&finished, json!({"session":{"provider":"codex","id":"thread"},"change":{"item":{"turnId":"turn","item":{"id":"native","status":"unknown","clientInputId":"client","body":{"inline":{"body":{"userMessage":{"text":null,"content":[]}}}}}}}})).0;
            } else {
                op::SendSubmission {
                    thread_id: SessionRef {
                        provider: ProviderKind::Codex,
                        id: "thread".into(),
                    },
                    client_user_message_id: "client".into(),
                    draft: Arc::default(),
                }
                .apply(
                    &mut finished,
                    op::SubmissionProgress::Sent(Some("turn".into())),
                );
            }
            if index == 0 {
                assert_eq!(
                    finished.pending_submissions.len(),
                    usize::from(!echo),
                    "a Host-delivered native echo proves acceptance even when the RPC reply is lost"
                );
            }
        }
        assert!(finished.pending_submissions.is_empty());
        assert_eq!(pending.pending_submissions.len(), 1);
    }
}

#[test]
fn submission_acknowledgement_moves_to_the_returned_turn() {
    use agent_core::state::Intent;
    let previous = initial(
        serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"old","status":"completed","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"","phase":"unknown"}}}}}]}]}))
        .unwrap(),
    );
    let (mut pending, _) = reduce(
        &previous,
        Event::Intent(Intent::Submit {
            thread_id: Some(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }),
            client_user_message_id: "client".into(),
        }),
    );
    assert_eq!(
        pending.pending_submissions["client"].turn_id.as_deref(),
        Some("old")
    );
    assert_eq!(
        pending.pending_submissions["client"]
            .after_item_id
            .as_deref(),
        Some("answer")
    );
    op::SendSubmission {
        thread_id: SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        },
        client_user_message_id: "client".into(),
        draft: Arc::default(),
    }
    .apply(
        &mut pending,
        op::SubmissionProgress::Sent(Some("new".into())),
    );
    assert_eq!(
        pending.pending_submissions["client"].turn_id.as_deref(),
        Some("new")
    );
    assert!(
        pending.pending_submissions["client"]
            .after_item_id
            .is_none()
    );
}

#[test]
fn model_settings_corpus() {
    use agent_core::state::{Draft, Intent};
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/model-settings.json")).unwrap();
    for case in cases {
        let draft: Draft = serde_json::from_value(case["draft"].clone()).unwrap();
        let models = serde_json::from_value(case["models"].clone()).unwrap();
        let mut previous = Snapshot {
            drafts: Arc::new(BTreeMap::from([(
                SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                Arc::new(draft),
            )])),
            ..Default::default()
        };
        let (next, effects) = if let Ok(model) =
            serde_json::from_value::<agent_protocol::models::ModelRef>(case["select"].clone())
        {
            previous.models = Arc::new(models);
            reduce(
                &previous,
                Event::Intent(Intent::SelectModel {
                    thread_id: SessionRef {
                        provider: ProviderKind::Codex,
                        id: "thread".into(),
                    }
                    .into(),
                    model,
                }),
            )
        } else {
            applied(
                &previous,
                op::LoadModels {},
                serde_json::from_value::<agent_protocol::operations::ModelPage>(
                    json!({"data":models}),
                )
                .unwrap(),
            )
        };
        assert!(effects.is_empty());
        let expected: Draft = serde_json::from_value(case["expected"].clone()).unwrap();
        assert_eq!(
            *next.drafts[&DraftKey::from(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into()
            })],
            expected,
            "{}",
            case["name"]
        );
    }
}
#[test]
fn event_corpus() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/events.json")).unwrap();
    assert_eq!(cases.len(), 7);
    for case in cases {
        let previous: Thread = serde_json::from_value(case["previous"].clone()).unwrap();
        let id = previous.id.clone().unwrap();
        let mut state = initial(previous);
        for event in case["events"].as_array().unwrap() {
            let (next, effects) = fixture_change(&state, event.clone());
            assert!(effects.is_empty());
            assert_eq!(next.error, None, "{}", case["name"]);
            state = next;
        }
        assert_eq!(
            serde_json::to_value(&state.conversations[&id]).unwrap(),
            serde_json::to_value(
                serde_json::from_value::<Thread>(case["expected"].clone()).unwrap()
            )
            .unwrap(),
            "{}",
            case["name"]
        );
    }
}
#[test]
fn delta_copies_only_the_changed_path_and_snapshot_round_trips() {
    let item = |id: &str| {
        Arc::new(Item::new(
            id.into(),
            ItemStatus::Unknown,
            ItemBody::AssistantText {
                text: "before".into(),
                phase: AssistantPhase::Unknown,
            },
        ))
    };
    let thread = Thread {
        id: Some(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }),
        turns: Some(vec![
            Arc::new(Turn {
                id: "old".into(),
                items: Some(vec![item("old-item")]),
                ..Default::default()
            }),
            Arc::new(Turn {
                id: "live".into(),
                status: TurnStatus::Running,
                items: Some(vec![item("unchanged"), item("changed")]),
                ..Default::default()
            }),
        ]),
        ..Default::default()
    };
    let mut state = initial(thread);
    Arc::make_mut(&mut state.conversations).insert(
        SessionRef {
            provider: ProviderKind::Codex,
            id: "other".into(),
        },
        Arc::new(Thread {
            id: Some(SessionRef {
                provider: ProviderKind::Codex,
                id: "other".into(),
            }),
            ..Default::default()
        }),
    );
    let (next, _) = fixture_change(
        &state,
        json!({"session":{"provider":"codex","id":"thread"},"change":{"text":{"turnId":"live","itemId":"changed","delta":" after","field":"assistantText"}}}),
    );
    assert!(Arc::ptr_eq(
        &state.conversations[&SessionRef {
            provider: ProviderKind::Codex,
            id: "other".into()
        }],
        &next.conversations[&SessionRef {
            provider: ProviderKind::Codex,
            id: "other".into()
        }]
    ));
    let old = state.conversations[&SessionRef {
        provider: ProviderKind::Codex,
        id: "thread".into(),
    }]
        .turns
        .as_ref()
        .unwrap();
    let new = next.conversations[&SessionRef {
        provider: ProviderKind::Codex,
        id: "thread".into(),
    }]
        .turns
        .as_ref()
        .unwrap();
    assert!(Arc::ptr_eq(&old[0], &new[0]));
    assert!(Arc::ptr_eq(
        &old[1].items.as_ref().unwrap()[0],
        &new[1].items.as_ref().unwrap()[0]
    ));
    assert_eq!(
        item_text(&(old[1].items.as_ref().unwrap()[1])),
        Some("before")
    );
    assert_eq!(
        item_text(&(new[1].items.as_ref().unwrap()[1])),
        Some("before after")
    );
    let restored: Snapshot = serde_json::from_slice(&serde_json::to_vec(&next).unwrap()).unwrap();
    assert_eq!(restored, next);
}

#[test]
fn activity_corpus_applies_even_without_a_loaded_conversation() {
    let cases: Vec<Value> = serde_json::from_str(include_str!("fixtures/activity.json")).unwrap();
    for case in cases {
        let mut snapshot = Snapshot::default();
        Arc::make_mut(&mut snapshot.navigation).thread_id =
            case["visible"].as_str().map(|id| SessionRef {
                provider: ProviderKind::Codex,
                id: id.into(),
            });
        for event in case["events"].as_array().unwrap() {
            snapshot = fixture_change(&snapshot, event.clone()).0;
        }
        assert_eq!(
            snapshot.activity.active[&SessionRef {
                provider: ProviderKind::Codex,
                id: "a".into()
            }],
            case["active"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        assert_eq!(
            snapshot.activity.unread.contains(&SessionRef {
                provider: ProviderKind::Codex,
                id: "a".into()
            }),
            case["unread"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        assert!(snapshot.conversations.is_empty());
    }
}

#[test]
fn new_chat_selects_catalog_defaults_in_either_load_order() {
    use agent_core::state::Intent;
    let models = serde_json::from_value::<Vec<agent_protocol::models::Model>>(json!([{
        "id":"model", "model":{"provider": "codex", "id": "model"}, "displayName":"Model",
        "defaultReasoningEffort":"high", "supportedReasoningEfforts":[{"reasoningEffort":"high"}],
        "defaultServiceTier":"priority", "serviceTiers":[{"id":"priority","fast":true}], "isDefault":true
    }]))
    .unwrap();
    for catalog_first in [false, true] {
        let mut current = Snapshot::default();
        for load_catalog in [catalog_first, !catalog_first] {
            if load_catalog {
                op::LoadModels {}.apply(
                    &mut current,
                    serde_json::from_value(json!({"data":models})).unwrap(),
                );
            } else {
                current = reduce(
                    &current,
                    Event::Intent(Intent::NewChat {
                        cwd: "/fixture".into(),
                    }),
                )
                .0;
            }
        }
        let draft = current
            .drafts
            .get(&current.navigation.draft_key)
            .expect("new chat draft");
        assert_eq!(
            draft.model.as_ref(),
            Some(&agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "model".into()
            })
        );
        assert_eq!(draft.effort.as_deref(), Some("high"));
        assert_eq!(draft.service_tier.as_deref(), Some("priority"));
        let (edited, _) = reduce(
            &current,
            Event::Intent(Intent::SetDraftText {
                thread_id: current.navigation.draft_key.clone(),
                text: "keep".into(),
            }),
        );
        let (returned, _) = reduce(
            &edited,
            Event::Intent(Intent::NewChat {
                cwd: "/fixture".into(),
            }),
        );
        assert!(Arc::ptr_eq(&edited.drafts, &returned.drafts));
        assert_eq!(returned.drafts[&returned.navigation.draft_key].text, "keep");
    }
}

#[test]
fn changing_workspace_clears_content_and_preserves_file_drafts() {
    use agent_core::state::{FileDraft, Intent, Navigation, Workspace};
    let file: Arc<agent_protocol::models::FileContent> = Arc::new(
        serde_json::from_value(json!({
            "path":"/old/file", "revision":"r1", "text":"saved", "size":5
        }))
        .unwrap(),
    );
    let directory: Arc<agent_protocol::models::FileList> = Arc::new(
        serde_json::from_value(json!({
            "path":"/old", "entries":[], "truncated":false
        }))
        .unwrap(),
    );
    let review: Arc<agent_protocol::models::WorkspaceReview> = Arc::new(
        serde_json::from_value(json!({
            "branch":"main", "additions":1, "deletions":0, "files":[], "diff":"old"
        }))
        .unwrap(),
    );
    let previous = Snapshot {
        navigation: Arc::new(Navigation {
            cwd: "/old".into(),
            ..Default::default()
        }),
        workspace: Arc::new(Workspace {
            file: Some(file.clone()),
            directory: Some(directory.clone()),
            review: Some(review.clone()),
            review_cwd: Some("/old".into()),
            settings: Some(Arc::default()),
            worktrees: None,
        }),
        file_drafts: Arc::new(BTreeMap::from([(
            "/old/file".into(),
            Arc::new(FileDraft {
                revision: "r1".into(),
                text: "unsaved".into(),
            }),
        )])),
        ..Default::default()
    };
    for cwd in ["/old", "/new"] {
        for open_thread in [false, true] {
            let next = if open_thread {
                let mut next = previous.clone();
                op::ReadThread::open(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                })
                .apply(
                    &mut next,
                    reply(
                        serde_json::from_value(
                            json!({"id":{"provider":"codex","id":"thread"}, "cwd":cwd}),
                        )
                        .unwrap(),
                    ),
                );
                next
            } else {
                reduce(
                    &previous,
                    Event::Intent(Intent::NewChat { cwd: cwd.into() }),
                )
                .0
            };
            assert!(Arc::ptr_eq(&previous.file_drafts, &next.file_drafts));
            assert!(next.workspace.settings.is_some());
            if cwd == "/old" {
                assert!(Arc::ptr_eq(&previous.workspace, &next.workspace));
                continue;
            }
            assert!(
                next.workspace.file.is_none(),
                "old file remains after navigation"
            );
            assert!(next.workspace.directory.is_none());
            assert!(next.workspace.review.is_none());
            assert!(next.workspace.review_cwd.is_none());
        }
    }
    let mut unassigned = previous.clone();
    let chat: ThreadResponse = serde_json::from_value(
        json!({"thread":{"id":{"provider":"codex","id":"chat"},"cwd":"/old","projectId":null}}),
    )
    .unwrap();
    Arc::make_mut(&mut unassigned.navigation).thread_id = Some(SessionRef {
        provider: ProviderKind::Codex,
        id: "chat".into(),
    });
    Arc::make_mut(&mut unassigned.conversations).insert(
        SessionRef {
            provider: ProviderKind::Codex,
            id: "chat".into(),
        },
        Arc::new(chat.thread.clone()),
    );
    let mut unassigned: Snapshot =
        serde_json::from_slice(&serde_json::to_vec(&unassigned).unwrap()).unwrap();
    op::ReadThread::open(SessionRef {
        provider: ProviderKind::Codex,
        id: "chat".into(),
    })
    .apply(&mut unassigned, reply(chat.thread));
    assert!(unassigned.workspace.review.is_none());
    assert!(unassigned.workspace.review_cwd.is_none());
    assert_eq!(previous.file_drafts, unassigned.file_drafts);
}

#[test]
fn file_change_delta_appends_to_output_without_inventing_a_diff() {
    for output in ["", "prefix"] {
        let previous = initial(serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"turn","items":[{"id":"file","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"fileChange":{"changes":[],"output":output}}}}}],"status":"unknown"}]})).unwrap());
        let (next, _) = fixture_change(
            &previous,
            json!({"session":{"provider":"codex","id":"thread"},"change":{"text":{"turnId":"turn","itemId":"file","delta":"tail","field":"fileOutput"}}}),
        );
        assert_eq!(next.error, None);
        let item = &next.conversations[&SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }]
            .turns
            .as_ref()
            .unwrap()[0]
            .items
            .as_ref()
            .unwrap()[0];
        assert!(
            matches!(item.body(), agent_protocol::items::ItemBody::FileChange { changes, output: actual } if changes.is_empty() && actual == &format!("{output}tail"))
        );
    }
}

#[test]
fn leaving_conversation_retains_draft_and_marks_later_completion_unread() {
    use agent_core::state::{Draft, Intent, Navigation};
    let previous = Snapshot {
        drafts: Arc::new(BTreeMap::from([(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            Arc::new(Draft {
                text: "下書き".into(),
                ..Default::default()
            }),
        )])),
        navigation: Arc::new(Navigation {
            thread_id: Some(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }),
            draft_key: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (listed, effects) = reduce(&previous, Event::Intent(Intent::ShowThreadList));
    assert!(listed.navigation.thread_id.is_none());
    assert_eq!(listed.epoch, previous.epoch + 1);
    assert!(Arc::ptr_eq(&listed.drafts, &previous.drafts));
    assert!(listed.subscriptions.is_empty());
    assert!(effects.is_empty());
    let (completed, _) = fixture_change(
        &listed,
        json!({"session":{"provider":"codex","id":"thread"},"change":{"turn":{"turn":{"id":"turn","status":"completed","items":[]},"completed":true}}}),
    );
    assert!(completed.activity.unread.contains(&SessionRef {
        provider: ProviderKind::Codex,
        id: "thread".into()
    }));
    assert_eq!(
        completed.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })]
            .text,
        "下書き"
    );
}

#[test]
fn serialized_events_preserve_operation_inputs_and_replay_state() {
    let events = vec![
        Event::Connected,
        Event::Intent(op::Intent::NewChat {
            cwd: "/fixture".into(),
        }),
        Event::Intent(op::Intent::SetDraftText {
            thread_id: "new:/fixture".into(),
            text: "再生する下書き".into(),
        }),
        Event::Intent(op::Intent::ReadFile(op::ReadFile {
            path: "/fixture/file".into(),
            discard_draft: true,
        })),
        Event::Intent(op::Intent::Respond(op::Respond {
            request_id: "request".into(),
            answer: agent_protocol::requests::Answer::Approval {
                choice_id: "fixture-choice".into(),
            },
        })),
        Event::Disconnected("fixture disconnect".into()),
    ];
    let encoded = serde_json::to_vec(&events).unwrap();
    let decoded: Vec<Event> = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        serde_json::to_value(&events).unwrap()
    );
    let replay = |events: Vec<Event>| {
        events
            .into_iter()
            .fold(Snapshot::default(), |snapshot, event| {
                reduce(&snapshot, event).0
            })
    };
    assert_eq!(replay(events.clone()), replay(decoded));
    assert_eq!(
        replay(events).drafts[&DraftKey::from("new:/fixture")].text,
        "再生する下書き"
    );
}

#[test]
fn durable_upload_and_pairing_results_survive_navigation() {
    let mut snapshot = reduce(
        &Snapshot::default(),
        Event::Intent(agent_core::state::Intent::NewChat { cwd: "/new".into() }),
    )
    .0;
    op::UploadAttachment {
        draft_key: "old".into(),
        attachment: agent_core::state::Attachment {
            path: "/local".into(),
            name: "image.png".into(),
            is_image: true,
        },
        directory: "/old".into(),
    }
    .stale(&mut snapshot, "/uploaded".into());
    assert_eq!(
        snapshot.drafts[&DraftKey::from("old")].attachments[0].path,
        "/uploaded"
    );
    for name in ["first", "updated"] {
        op::PairRemoteHost {
            invitation: serde_json::from_value(json!({
                "endpoint":"unused", "invitation":uuid::Uuid::nil(), "expiresAt":0,
                "hostName":"Test PC", "aiRecipients":[], "transcriptionRecipient":null
            }))
            .unwrap(),
            name: name.into(),
        }
        .stale(
            &mut snapshot,
            serde_json::from_value(json!({
                "id":"remote", "name":name, "ticket":"unused"
            }))
            .unwrap(),
        );
        assert_eq!(snapshot.management.remotes.len(), 1);
        assert_eq!(snapshot.management.remotes[0].name, name);
    }
    assert_eq!(snapshot.navigation.cwd, "/new");
    assert!(snapshot.error.is_none());
}

#[test]
fn disconnected_catalog_reads_do_not_apply_or_retry_stale_responses() {
    let mut state = Snapshot::default();
    let before = state.clone();
    assert!(op::LoadModels {}.stale(&mut state, serde_json::from_value(json!({
        "data":[{"id":"old","model":{"provider": "codex", "id": "old"},"displayName":"Old", "defaultReasoningEffort":"medium","supportedReasoningEfforts":[]}]
    })).unwrap()).is_empty());
    assert!(
        op::ListAccounts {}
            .stale(
                &mut state,
                serde_json::from_value(json!({
                    "accounts":[{"provider":"codex","id":"old"}],"selected":{"codex":"old"}
                }))
                .unwrap()
            )
            .is_empty()
    );
    assert_eq!(state, before);
}

#[test]
fn incomplete_model_catalog_preserves_restored_choices_and_defaults_only_new_drafts() {
    use agent_core::state::{Draft, Intent};
    for (restored, failed) in [(false, false), (true, false), (false, true), (true, true)] {
        let draft = Draft {
            model: Some(agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "codex-model".into(),
            }),
            effort: Some("high".into()),
            service_tier: Some("priority".into()),
            text: "keep this input".into(),
            ..Default::default()
        };
        let mut state = Snapshot {
            drafts: Arc::new(BTreeMap::from([("saved".into(), Arc::new(draft.clone()))])),
            ..Default::default()
        };
        if restored {
            state = serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        }
        let errors = if failed {
            json!({"codex":{"message":"offline"}})
        } else {
            json!({})
        };
        op::LoadModels {}.apply(
            &mut state,
            serde_json::from_value(json!({"data":[{
            "id":"claude:default","model":{"provider": "claude", "id": "default"},"displayName":"Claude",
            "defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]
        }],"providerErrors":errors}))
            .unwrap(),
        );
        assert_eq!(*state.drafts[&DraftKey::from("saved")], draft);
        assert_eq!(
            state.model_error_messages(None),
            if failed {
                vec!["codex: offline"]
            } else {
                vec![]
            }
        );
        state = reduce(
            &state,
            Event::Intent(Intent::NewChat {
                cwd: "/fresh".into(),
            }),
        )
        .0;
        assert_eq!(
            state.drafts[&state.navigation.draft_key].model.as_ref(),
            Some(&agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Claude,
                id: "default".into()
            })
        );
        state = reduce(
            &state,
            Event::Intent(Intent::SelectEffort {
                thread_id: "saved".into(),
                effort: "high".into(),
            }),
        )
        .0;
        assert_eq!(*state.drafts[&DraftKey::from("saved")], draft);
        let catalog = serde_json::from_value(json!({"data":[{
            "id":"codex-model","model":{"provider": "codex", "id": "codex-model"},"displayName":"Codex",
            "defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}],
            "serviceTiers":[{"id":"priority","fast":true}]
        }]})).unwrap();
        op::LoadModels {}.apply(&mut state, catalog);
        assert_eq!(*state.drafts[&DraftKey::from("saved")], draft);
        assert!(state.model_errors.is_empty());
    }
}

#[test]
fn completed_commands_refresh_session_metadata_without_waiting_for_the_turn() {
    for connected in [false, true] {
        let mut snapshot = initial(
            serde_json::from_value(json!({"id":{"provider":"codex","id":"task"},"status":"running","turns":[{"id":"turn","status":"running","items":[]}]}))
            .unwrap(),
        );
        snapshot.connected = connected;
        let (next, effects) = fixture_change(
            &snapshot,
            json!({"session":{"provider":"codex","id":"task"},"change":{"item":{"turnId":"turn","item":{"id":"merge","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"git merge task","cwd":null,"output":"","exitCode":0}}}}}}}}),
        );
        assert_eq!(effects.len(), usize::from(connected));
        assert_eq!(
            next.conversations[&SessionRef {
                provider: ProviderKind::Codex,
                id: "task".into()
            }]
                .turns
                .as_ref()
                .unwrap()[0]
                .status,
            TurnStatus::Running
        );
        assert_eq!(next.error, None);
    }
}

#[test]
fn terminal_presentation_only_shows_loading_or_exceptional_status() {
    use agent_core::state::{Terminal, TerminalPhase};
    let mut snapshot = Snapshot::default();
    for (phase, loading, status) in [
        (TerminalPhase::Starting, true, None),
        (TerminalPhase::Running, false, None),
        (TerminalPhase::Exited(0), false, Some("終了 · 0")),
        (
            TerminalPhase::Failed("failure".into()),
            false,
            Some("failure"),
        ),
        (
            TerminalPhase::Suspended,
            false,
            Some("再接続を待っています"),
        ),
        (TerminalPhase::Detached, false, Some("切断済み")),
    ] {
        std::sync::Arc::make_mut(&mut snapshot.terminals).insert(
            "test".into(),
            std::sync::Arc::new(Terminal {
                cwd: "/fixture".into(),
                size: agent_protocol::operations::TerminalSize { cols: 80, rows: 24 },
                phase,
                output: Default::default(),
                sequence: 0,
            }),
        );
        let view = snapshot.terminal_view("test".into()).unwrap();
        assert_eq!(view.loading, loading);
        assert_eq!(view.status.as_deref(), status);
    }
}

#[test]
fn terminal_disconnect_keeps_resumption_and_host_switch_drops_old_handles() {
    use agent_core::state::{Event, Intent, Snapshot, TerminalPhase, operations as op, reduce};
    let snapshot = Snapshot {
        connected: true,
        storage_scope: "first-host".into(),
        ..Default::default()
    };
    let (starting, _) = reduce(
        &snapshot,
        Event::Intent(Intent::StartTerminal(op::StartTerminal {
            handle: "test".into(),
            cwd: "/fixture".into(),
            size: agent_protocol::operations::TerminalSize { cols: 80, rows: 24 },
        })),
    );
    assert!(starting.terminal_view("test".into()).unwrap().accepts_input);
    let (disconnected, _) = reduce(&starting, Event::Disconnected("offline".into()));
    assert!(
        !disconnected
            .terminal_view("test".into())
            .unwrap()
            .accepts_input
    );
    let (late_failure, _) = reduce(
        &disconnected,
        Event::TerminalFailed {
            handle: "test".into(),
            reason: "cancelled".into(),
        },
    );
    assert_eq!(
        late_failure.terminals["test"].phase,
        TerminalPhase::Suspended
    );
    let (other_host, _) = reduce(&late_failure, Event::StorageScope("other-host".into()));
    assert!(other_host.terminals.is_empty());
}

#[test]
fn project_registration_navigates_only_while_current() {
    use agent_core::state::Intent;
    let previous = Snapshot {
        connected: true,
        ..Default::default()
    };
    let operation = op::AddProject {
        cwd: "/new-project".into(),
    };
    let (pending, _) = reduce(
        &previous,
        Event::Intent(Intent::AddProject(operation.clone())),
    );
    assert_eq!(pending.navigation, previous.navigation);
    let (opened, effects) = applied(&pending, operation.clone(), "/resolved-project".into());
    assert_eq!(opened.navigation.cwd, "/resolved-project");
    assert_eq!(
        opened.navigation.draft_key,
        DraftKey::from("new:/resolved-project")
    );
    assert_eq!(effects.len(), 2); // Workspace review and project-list refresh.
    let (mut elsewhere, _) = reduce(
        &pending,
        Event::Intent(Intent::NewChat {
            cwd: "/elsewhere".into(),
        }),
    );
    let navigation = elsewhere.navigation.clone();
    assert_eq!(
        operation
            .stale(&mut elsewhere, "/resolved-project".into())
            .len(),
        1
    );
    assert_eq!(elsewhere.navigation, navigation);
}

#[test]
fn failed_submission_restores_text_and_attachments_without_losing_new_input() {
    use agent_core::state::{Attachment, Draft, Intent};
    let attachment = |path: &str| Attachment {
        path: path.into(),
        name: path.into(),
        is_image: false,
    };
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.drafts).insert(
        SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }
        .into(),
        Arc::new(Draft {
            text: "sent".into(),
            attachments: vec![attachment("/sent")],
            ..Default::default()
        }),
    );
    let (mut snapshot, _) = reduce(
        &snapshot,
        Event::Intent(Intent::Submit {
            thread_id: Some(SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }),
            client_user_message_id: "pending".into(),
        }),
    );
    assert!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })]
            .text
            .is_empty()
    );
    assert!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })]
            .attachments
            .is_empty()
    );
    Arc::make_mut(&mut snapshot.drafts).insert(
        SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }
        .into(),
        Arc::new(Draft {
            text: "next".into(),
            attachments: vec![attachment("/next")],
            model: Some(agent_protocol::models::ModelRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "new-model".into(),
            }),
            ..Default::default()
        }),
    );
    let (snapshot, _) = reduce(&snapshot, Event::SubmissionFailed("pending".into()));
    assert!(snapshot.pending_submissions.is_empty());
    let restored = &snapshot.drafts[&DraftKey::from(SessionRef {
        provider: ProviderKind::Codex,
        id: "thread".into(),
    })];
    assert_eq!(restored.text, "sent\nnext");
    assert_eq!(
        restored.attachments,
        vec![attachment("/next"), attachment("/sent")]
    );
    assert_eq!(
        restored.model.as_ref(),
        Some(&agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "new-model".into()
        })
    );
}

#[test]
fn unknown_submission_can_be_restored_or_discarded() {
    let attachment = |path: &str| agent_core::state::Attachment {
        path: path.into(),
        name: path.into(),
        is_image: false,
    };
    let pending = |text: &str| {
        Arc::new(agent_core::state::PendingSubmission {
            sequence: 0,
            draft_key: SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            draft: Arc::new(Draft {
                text: text.into(),
                attachments: vec![attachment("/sent")],
                ..Default::default()
            }),
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: true,
        })
    };
    let snapshot = Snapshot {
        pending_submissions: Arc::new(BTreeMap::from([
            ("restore".into(), pending("uncertain")),
            ("discard".into(), pending("unwanted")),
        ])),
        drafts: Arc::new(BTreeMap::from([(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            Arc::new(Draft {
                text: "newer".into(),
                attachments: vec![attachment("/newer")],
                ..Default::default()
            }),
        )])),
        ..Default::default()
    };

    let (snapshot, effects) = reduce(
        &snapshot,
        Event::Intent(Intent::RestoreUnknownSubmission {
            client_user_message_id: "restore".into(),
        }),
    );
    assert!(effects.is_empty());
    assert_eq!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })]
            .text,
        "uncertain\nnewer"
    );
    assert!(!snapshot.pending_submissions.contains_key("restore"));
    assert_eq!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })]
            .attachments,
        vec![attachment("/newer"), attachment("/sent")]
    );
    let saved_draft = snapshot.drafts[&DraftKey::from(SessionRef {
        provider: ProviderKind::Codex,
        id: "thread".into(),
    })]
        .clone();

    let (snapshot, effects) = reduce(
        &snapshot,
        Event::Intent(Intent::DiscardUnknownSubmission {
            client_user_message_id: "discard".into(),
        }),
    );
    assert!(effects.is_empty());
    assert!(snapshot.pending_submissions.is_empty());
    assert_eq!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })],
        saved_draft
    );
    assert_eq!(
        snapshot.drafts[&DraftKey::from(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into()
        })]
            .text,
        "uncertain\nnewer"
    );
}

#[test]
fn unresolved_submission_actions_ignore_known_delivery_states() {
    let snapshot = Snapshot {
        pending_submissions: Arc::new(BTreeMap::from([(
            "sending".into(),
            Arc::new(agent_core::state::PendingSubmission {
                sequence: 0,
                draft_key: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                draft: Arc::new(Draft {
                    text: "sending".into(),
                    ..Default::default()
                }),
                turn_id: None,
                after_item_id: None,
                accepted: false,
                delivery_unknown: false,
            }),
        )])),
        ..Default::default()
    };

    for intent in [
        Intent::RestoreUnknownSubmission {
            client_user_message_id: "sending".into(),
        },
        Intent::DiscardUnknownSubmission {
            client_user_message_id: "sending".into(),
        },
    ] {
        let (next, effects) = reduce(&snapshot, Event::Intent(intent));
        assert!(effects.is_empty());
        assert_eq!(next, snapshot);
    }
}

#[test]
fn file_navigation_rejects_empty_and_relative_paths_before_rpc() {
    for path in ["", "nested", "../", "~/project"] {
        let (snapshot, effects) = reduce(
            &Snapshot::default(),
            Event::Intent(agent_core::state::Intent::ListFiles(op::ListFiles {
                path: path.into(),
            })),
        );
        assert!(effects.is_empty(), "sent an invalid path: {path:?}");
        assert_eq!(
            snapshot.error.as_deref(),
            Some("絶対パスを入力してください。")
        );
    }
    let (snapshot, effects) = reduce(
        &Snapshot::default(),
        Event::Intent(agent_core::state::Intent::ListFiles(op::ListFiles {
            path: std::env::temp_dir().to_string_lossy().into_owned(),
        })),
    );
    assert!(snapshot.error.is_none());
    assert_eq!(effects.len(), 1);
}

#[test]
fn editing_a_file_shares_other_drafts_and_preserves_previous_snapshots() {
    use agent_core::state::FileDraft;
    let original = Snapshot {
        file_drafts: Arc::new(BTreeMap::from([
            (
                "/edited".into(),
                Arc::new(FileDraft {
                    revision: "r1".into(),
                    text: "before".into(),
                }),
            ),
            (
                "/other".into(),
                Arc::new(FileDraft {
                    revision: "r2".into(),
                    text: "large unchanged draft".repeat(1024),
                }),
            ),
        ])),
        ..Default::default()
    };
    let (edited, effects) = reduce(
        &original,
        Event::Intent(op::Intent::SetFileDraft {
            path: "/edited".into(),
            text: "after".into(),
        }),
    );
    assert!(effects.is_empty());
    assert_eq!(original.file_drafts["/edited"].text, "before");
    assert_eq!(edited.file_drafts["/edited"].text, "after");
    assert_eq!(edited.file_drafts["/edited"].revision, "r1");
    assert!(Arc::ptr_eq(
        &original.file_drafts["/other"],
        &edited.file_drafts["/other"]
    ));
    assert!(!Arc::ptr_eq(
        &original.file_drafts["/edited"],
        &edited.file_drafts["/edited"]
    ));
}

#[test]
fn host_delivery_replay_resolves_unknown_input_without_overwriting_new_draft() {
    use agent_protocol::session::SubmissionDelivery;
    for delivery in [
        SubmissionDelivery::Sending,
        SubmissionDelivery::Accepted {
            turn_id: Some("live".into()),
        },
        SubmissionDelivery::Rejected,
    ] {
        let mut original = initial(
            serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[]}))
                .unwrap(),
        );
        Arc::make_mut(&mut original.drafts).insert(
            SessionRef {
                provider: ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            Arc::new(Draft {
                text: "sent text".into(),
                ..Default::default()
            }),
        );
        let (pending, _) = reduce(
            &original,
            Event::Intent(Intent::Submit {
                thread_id: Some(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }),
                client_user_message_id: "input".into(),
            }),
        );
        let (pending, _) = reduce(&pending, Event::SubmissionUnknown("input".into()));
        let (mut pending, _) = reduce(
            &pending,
            Event::Intent(Intent::SetDraftText {
                thread_id: SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                text: "new text".into(),
            }),
        );
        let mut thread = (*pending.conversations[&SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        }])
            .clone();
        thread.submissions.insert("input".into(), delivery.clone());
        op::ReadThread::new(SessionRef {
            provider: ProviderKind::Codex,
            id: "thread".into(),
        })
        .apply(&mut pending, reply(thread));
        if delivery == SubmissionDelivery::Rejected {
            assert!(pending.pending_submissions.is_empty());
            assert_eq!(
                pending.drafts[&DraftKey::from(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into()
                })]
                    .text,
                "sent text\nnew text"
            );
        } else {
            let input = &pending.pending_submissions["input"];
            assert!(!input.delivery_unknown);
            assert_eq!(
                input.accepted,
                matches!(delivery, SubmissionDelivery::Accepted { .. })
            );
            assert_eq!(
                pending.drafts[&DraftKey::from(SessionRef {
                    provider: ProviderKind::Codex,
                    id: "thread".into()
                })]
                    .text,
                "new text"
            );
            if matches!(delivery, SubmissionDelivery::Accepted { .. }) {
                let (lost_reply, _) = reduce(&pending, Event::SubmissionUnknown("input".into()));
                assert!(
                    !lost_reply.pending_submissions["input"].delivery_unknown,
                    "a lost RPC reply cannot override Host acceptance already received through the subscription"
                );
            }
        }
    }
}

fn item_text(item: &agent_protocol::items::Item) -> Option<&str> {
    match item.body() {
        agent_protocol::items::ItemBody::AssistantText { text, .. } => Some(text),
        agent_protocol::items::ItemBody::UserMessage { text, .. } => text.as_deref(),
        agent_protocol::items::ItemBody::Reasoning { content, .. } => {
            content.first().map(String::as_str)
        }
        _ => None,
    }
}

#[test]
fn empty_directory_chat_keeps_its_draft_separate_from_the_list() {
    let previous = Snapshot {
        workspace: Arc::new(agent_core::state::Workspace {
            directory: Some(Arc::new(agent_protocol::models::FileList {
                path: "/workspace".into(),
                entries: vec![],
                truncated: false,
            })),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (opened, _) = reduce(
        &previous,
        Event::Intent(op::Intent::NewChat { cwd: String::new() }),
    );
    assert_eq!(opened.workspace.directory, previous.workspace.directory);
    let key = opened.navigation.draft_key.clone();
    assert_ne!(key, Snapshot::default().navigation.draft_key);
    let (edited, _) = reduce(
        &opened,
        Event::Intent(op::Intent::SetDraftText {
            thread_id: key.clone(),
            text: "keep".into(),
        }),
    );
    let (listed, _) = reduce(&edited, Event::Intent(op::Intent::ShowThreadList));
    let (reopened, _) = reduce(
        &listed,
        Event::Intent(op::Intent::NewChat { cwd: String::new() }),
    );
    assert_eq!(reopened.navigation.draft_key, key);
    assert_eq!(reopened.drafts[&key].text, "keep");
}

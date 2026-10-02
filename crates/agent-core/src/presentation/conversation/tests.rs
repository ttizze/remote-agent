use super::*;
use crate::state::{Draft, Snapshot};
use serde_json::json;
use std::collections::HashSet;

fn fixture() -> Snapshot {
    let thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"done","status":"completed","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"earlier","phase":"unknown"}}}}}]},{"id":"live","status":"running","items":[{"id":"user","status":"unknown","clientInputId":"accepted","body":{"inline":{"body":{"userMessage":{"text":"question","content":[]}}}}},{"id":"command","status":"completed","clientInputId":null,"body":{"inline":{"body":{"commandExecution":{"command":"pwd","cwd":null,"output":"/fixture","exitCode":null,"durationMs":null}}}}},{"id":"stream","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"hello","phase":"unknown"}}}}}]}]})).unwrap();
    Snapshot {
        conversations: Arc::new(
            [(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                },
                Arc::new(thread),
            )]
            .into(),
        ),
        ..Default::default()
    }
}
fn project_snapshot(
    snapshot: Snapshot,
    previous: Option<&Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    project_conversation(
        &snapshot,
        snapshot.conversations[&agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "thread".into(),
        }]
            .clone(),
        &previous.cloned(),
    )
}

#[test]
fn session_requests_render_without_history_and_sent_requests_disable_answers() {
    use agent_protocol::{
        requests::{ElicitationInput, RequestBody, RequestTarget},
        session::RequestDelivery,
    };
    let make = |delivery| {
        let request = WireRequest {
            id: "request".into(),
            target: RequestTarget::Session,
            delivery,
            body: RequestBody::Elicitation {
                server: "mcp".into(),
                message: "confirm".into(),
                input: ElicitationInput::Url {
                    url: "https://example.com/".into(),
                },
            },
        };
        Arc::new(models::Thread {
            requests: [(request.id.clone(), Arc::new(request))].into(),
            ..Default::default()
        })
    };
    let awaiting =
        project_conversation(&Snapshot::default(), make(RequestDelivery::Awaiting), &None);
    assert!(awaiting.turns.is_empty());
    assert_eq!(awaiting.request_rows.len(), 1);
    let ConversationRowContent::PendingRequest { request } = &awaiting.request_rows[0].content
    else {
        panic!("request row")
    };
    assert!(request.can_respond);
    let sent = project_conversation(
        &Snapshot::default(),
        make(RequestDelivery::Sent),
        &Some(awaiting.clone()),
    );
    let ConversationRowContent::PendingRequest { request } = &sent.request_rows[0].content else {
        panic!("request row")
    };
    assert!(!request.can_respond);
    assert_ne!(
        &request.title,
        match &awaiting.request_rows[0].content {
            ConversationRowContent::PendingRequest { request } => &request.title,
            _ => unreachable!(),
        }
    );
}

#[test]
fn generated_image_placeholder_yields_to_result_and_stops_on_failure() {
    for (status, saved_path, result, placeholder, sources, title) in [
        ("running", "", "", true, vec![], ""),
        (
            "running",
            "/preview.png",
            "",
            false,
            vec!["/preview.png"],
            "",
        ),
        (
            "completed",
            "/generated.png",
            "",
            false,
            vec!["/generated.png"],
            "生成画像",
        ),
        (
            "completed",
            "",
            "png-data",
            false,
            vec!["data:image/png;base64,png-data"],
            "生成画像",
        ),
        (
            "failed",
            "",
            "",
            false,
            vec![],
            "画像を生成できませんでした",
        ),
    ] {
        let item = Arc::new(
            serde_json::from_value(json!({"id":"image","status":status,"clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":saved_path,"data":result,"revisedPrompt":null}}}}}))
            .unwrap(),
        );
        let projected = RenderedItem::native(
            &item,
            Some(crate::session::ProviderKind::Codex),
            false,
            None,
        );
        assert_eq!(projected.data.image_placeholder, placeholder);
        assert_eq!(projected.data.image_sources, sources);
        assert_eq!(projected.data.title, title);
        assert_eq!(projected.data.body, title);
    }
}

#[test]
fn flat_rows_preserve_history_order_and_only_offer_fork_on_last_completed_response() {
    let mut snapshot = fixture();
    let thread = Arc::make_mut(
        Arc::make_mut(&mut snapshot.conversations)
            .get_mut(&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            })
            .unwrap(),
    );
    thread.capabilities = Some(crate::session::Capabilities {
        fork: true,
        ..Default::default()
    });
    let turn = Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]);
    turn.items_has_more = Some(true);
    turn.opening_user_message = Some(Arc::new(
        serde_json::from_value(json!({"id":"opening","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":"first","content":[]}}}}}))
            .unwrap(),
    ));
    let rendered = project_snapshot(snapshot.clone(), None);
    let rows = rendered.turns[1].conversation_rows();
    let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
    assert!(
        matches!(&rows[0].content, ConversationRowContent::User { item } if item.data.native_id.as_deref() == Some("opening"))
    );
    assert!(
        matches!(&rows[1].content, ConversationRowContent::OlderItems { turn_id } if turn_id.as_str() == "live")
    );
    assert!(
        matches!(&rows[2].content, ConversationRowContent::User { item } if item.data.native_id.as_deref() == Some("user"))
    );
    let accepted_id = ids[2].to_owned();
    assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len());
    assert!(rows.iter().any(|row| matches!(&row.content, ConversationRowContent::Activity { item, .. } if item.data.native_id.as_deref() == Some("command"))));
    assert!(rows.iter().all(|row| !matches!(
        &row.content,
        ConversationRowContent::Response {
            fork_turn_id: Some(_),
            ..
        }
    )));
    assert!(matches!(
        rows.last().unwrap().content,
        ConversationRowContent::InProgress { .. }
    ));
    let thread = Arc::make_mut(
        Arc::make_mut(&mut snapshot.conversations)
            .get_mut(&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            })
            .unwrap(),
    );
    Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1]).status = models::TurnStatus::Completed;
    let completed = project_snapshot(snapshot, Some(&rendered));
    let rows = completed.turns[1].conversation_rows();
    assert!(
        matches!(&rows.last().unwrap().content, ConversationRowContent::Response { item, fork_turn_id: Some(id) } if id.as_str() == "live" && item.data.native_id.as_deref() == Some("stream"))
    );
    assert!(rows.iter().any(|row| row.id == accepted_id));
    for row in rows {
        if let ConversationRowContent::ActivityHeader { activity } = row.content {
            assert!(!activity_is_expanded(&activity, None));
            assert!(activity_is_expanded(
                &activity,
                Some(ActivityExpansion {
                    status: activity.status.clone(),
                    expanded: true
                })
            ));
            assert!(!activity_is_expanded(
                &activity,
                Some(ActivityExpansion {
                    status: "running".into(),
                    expanded: true
                })
            ));
        }
    }
}

#[test]
fn flat_row_ids_distinguish_repeated_items_and_turns() {
    let source = Arc::new(
        serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"turns":[{"id":"first","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"old","phase":"unknown"}}}}},{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"new","phase":"final"}}}}}]},{"id":"second","status":"completed","items":[{"id":"same","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"another turn","phase":"unknown"}}}}}]}]}))
        .unwrap(),
    );
    let rendered = project_conversation(&Snapshot::default(), source, &None);
    let rows: Vec<_> = rendered
        .turns
        .iter()
        .flat_map(|turn| turn.conversation_rows())
        .collect();
    assert_eq!(
        rows.iter().map(|row| &row.id).collect::<HashSet<_>>().len(),
        rows.len()
    );
    let texts: Vec<_> = rows
        .iter()
        .filter_map(|row| match &row.content {
            ConversationRowContent::Activity { item, .. }
            | ConversationRowContent::Response { item, .. } => Some(item.data.body.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["old", "new", "another turn"]);
    let again: Vec<_> = rendered
        .turns
        .iter()
        .flat_map(|turn| turn.conversation_rows())
        .map(|row| row.id)
        .collect();
    assert_eq!(
        again,
        rows.iter().map(|row| row.id.clone()).collect::<Vec<_>>()
    );
    let mut changed = rendered.source.clone();
    Arc::make_mut(&mut Arc::make_mut(&mut changed).turns.as_mut().unwrap()[0]).status =
        models::TurnStatus::Interrupted;
    let updated = project_conversation(&Snapshot::default(), changed, &Some(rendered.clone()));
    assert_eq!(updated.turns[0].items().count(), 2);
    for (before, after) in rendered.turns[0].items().zip(updated.turns[0].items()) {
        assert!(
            Arc::ptr_eq(before, after),
            "unchanged items must retain their cache even when IDs repeat"
        );
    }
}

#[test]
fn unknown_submissions_keep_send_order_and_position_after_reopening() {
    use crate::state::{Event, Intent, reduce};
    for status in ["completed", "running"] {
        let mut snapshot = Snapshot {
            conversations: Arc::new(
                [(
                    agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() },
                    Arc::new(
                        serde_json::from_value(json!({"id":{"provider":"codex","id":"thread"},"capabilities":{"additionalInput":true,"fork":false,"rename":false,"modelChange":false},"turns":[{"id":"before","status":status,"items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"before","phase":"unknown"}}}}}]}]}))
                        .unwrap(),
                    ),
                )]
                .into(),
            ),
            ..Default::default()
        };
        // IDs deliberately disagree with submission order.
        for id in ["z-first", "a-second", "m-third"] {
            Arc::make_mut(&mut snapshot.drafts).insert(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                Arc::new(Draft {
                    text: id.into(),
                    ..Default::default()
                }),
            );
            snapshot = reduce(
                &snapshot,
                Event::Intent(Intent::Submit {
                    thread_id: Some(agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "thread".into(),
                    }),
                    client_user_message_id: id.into(),
                }),
            )
            .0;
            snapshot = reduce(&snapshot, Event::SubmissionUnknown(id.into())).0;
        }
        let first = project_snapshot(snapshot.clone(), None);
        let texts = |rendered: &RenderedConversation| {
            rendered
                .turns
                .iter()
                .flat_map(|turn| turn.items())
                .chain(rendered.queued.iter())
                .map(|item| item.data.body.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(&first), ["before", "z-first", "a-second", "m-third"]);
        assert!(Arc::ptr_eq(
            &first,
            &project_snapshot(snapshot.clone(), Some(&first))
        ));
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.conversations)
                .get_mut(&agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                })
                .unwrap(),
        );
        thread.turns.as_mut().unwrap().push(Arc::new(
            serde_json::from_value(json!({"id":"later","status":"completed","items":[{"id":"later-user","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"userMessage":{"text":"later","content":[]}}}}}]}))
            .unwrap(),
        ));
        let reopened: Snapshot =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let rendered = project_snapshot(reopened, Some(&first));
        assert_eq!(
            texts(&rendered),
            ["before", "z-first", "a-second", "m-third", "later"]
        );
        assert!(rendered.queued.is_empty());
    }
}

#[test]
fn queued_submissions_keep_send_order_without_history() {
    use crate::state::{Event, Intent, reduce};
    let mut snapshot = Snapshot {
        conversations: Arc::new(
            [(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                },
                Arc::new(models::Thread {
                    id: Some(agent_protocol::session::SessionRef {
                        provider: agent_protocol::session::ProviderKind::Codex,
                        id: "thread".into(),
                    }),
                    ..Default::default()
                }),
            )]
            .into(),
        ),
        ..Default::default()
    };
    for id in ["z-first", "a-second", "m-third"] {
        snapshot = reduce(
            &snapshot,
            Event::Intent(Intent::Submit {
                thread_id: Some(agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                }),
                client_user_message_id: id.into(),
            }),
        )
        .0;
        snapshot = reduce(&snapshot, Event::SubmissionUnknown(id.into())).0;
    }
    let rendered = project_snapshot(snapshot.clone(), None);
    assert_eq!(
        rendered
            .queued
            .iter()
            .map(|item| item.data.id.as_str())
            .collect::<Vec<_>>(),
        ["z-first", "a-second", "m-third"]
    );
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.conversations)
            .get_mut(&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            })
            .unwrap(),
    )
    .turns = Some(vec![Arc::new(
        serde_json::from_value(json!({"id":"later","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"later","phase":"unknown"}}}}}],"status":"unknown"}))
        .unwrap(),
    )]);
    let updated = project_snapshot(snapshot, Some(&rendered));
    assert!(updated.queued.is_empty());
    assert_eq!(
        updated.turns[0]
            .items()
            .map(|item| item.data.id.as_str())
            .collect::<Vec<_>>(),
        ["z-first", "a-second", "m-third", "answer"]
    );
}

#[test]
fn pending_echoes_render_once_and_keep_remaining_input_order() {
    let mut snapshot = fixture();
    for (sequence, id) in ["a", "b", "a", "c"].into_iter().enumerate() {
        Arc::make_mut(&mut snapshot.pending_submissions).insert(
            id.into(),
            Arc::new(PendingSubmission {
                sequence: sequence as u64,
                draft_key: agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "thread".into(),
                }
                .into(),
                draft: Arc::new(Draft {
                    text: format!("pending {id} {sequence}"),
                    ..Default::default()
                }),
                turn_id: Some("echo-turn".into()),
                after_item_id: Some("echo".into()),
                accepted: false,
                delivery_unknown: true,
            }),
        );
    }
    Arc::make_mut(Arc::make_mut(&mut snapshot.conversations).get_mut(&agent_protocol::session::SessionRef { provider: agent_protocol::session::ProviderKind::Codex, id: "thread".into() }).unwrap())
        .turns.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({"id":"echo-turn","items":[{"id":"echo","status":"unknown","clientInputId":"b","body":{"inline":{"body":{"userMessage":{"text":"native b","content":[]}}}}}],"status":"unknown"})).unwrap()));
    let rendered = project_snapshot(snapshot.clone(), None);
    let items: Vec<_> = rendered.turns.last().unwrap().items().collect();
    assert_eq!(
        items
            .iter()
            .map(|item| item.data.body.as_str())
            .collect::<Vec<_>>(),
        ["native b", "pending a 2", "pending c 3"]
    );
    assert!(matches!(&items[0].source, ItemSource::Native(_)));
    assert!(rendered.queued.is_empty());
    assert_eq!(snapshot.pending_submissions.len(), 3);
    assert!(Arc::ptr_eq(
        &rendered,
        &project_snapshot(snapshot, Some(&rendered))
    ));
}

#[test]
fn pending_input_remains_visible_until_its_turn_is_loaded() {
    let mut snapshot = fixture();
    Arc::make_mut(&mut snapshot.pending_submissions).insert(
        "pending".into(),
        Arc::new(PendingSubmission {
            sequence: 0,
            draft_key: agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            draft: Arc::new(Draft {
                text: "waiting for history".into(),
                ..Default::default()
            }),
            turn_id: Some("unloaded".into()),
            after_item_id: None,
            accepted: true,
            delivery_unknown: false,
        }),
    );
    let first = project_snapshot(snapshot.clone(), None);
    assert_eq!(first.queued.len(), 1);
    assert_eq!(first.queued[0].data.body, "waiting for history");
    let thread = Arc::make_mut(
        Arc::make_mut(&mut snapshot.conversations)
            .get_mut(&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            })
            .unwrap(),
    );
    thread.turns.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({"id":"unloaded","status":"running","items":[{"id":"echo","status":"unknown","clientInputId":"pending","body":{"inline":{"body":{"userMessage":{"text":"waiting for history","content":[]}}}}}]})).unwrap()));
    let loaded = project_snapshot(snapshot, Some(&first));
    assert!(loaded.queued.is_empty());
    assert_eq!(loaded.turns.last().unwrap().items().count(), 1);
    let echoed = loaded.turns.last().unwrap().items().next().unwrap();
    assert_eq!(echoed.data.id, "pending");
    assert!(matches!(&echoed.source, ItemSource::Native(item) if item.id == "echo".into()));
}

#[test]
fn delta_reuses_untouched_turns_and_items_but_invalidates_deferred_details() {
    let snapshot = fixture();
    let first = project_snapshot(snapshot.clone(), None);
    let same = project_snapshot(snapshot.clone(), Some(&first));
    assert!(Arc::ptr_eq(&first, &same));
    let mut updated = snapshot.clone();
    let thread = crate::session::SessionChange::Text {
        turn_id: "live".into(),
        item_id: "stream".into(),
        field: crate::session::TextField::AssistantText,
        delta: " world".into(),
    }
    .apply(
        &snapshot.conversations[&agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "thread".into(),
        }],
    )
    .unwrap();
    Arc::make_mut(&mut updated.conversations).insert(
        agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "thread".into(),
        },
        Arc::new(thread),
    );
    let second = project_snapshot(updated.clone(), Some(&first));
    assert!(Arc::ptr_eq(&first.turns[0], &second.turns[0]));
    assert!(!Arc::ptr_eq(&first.turns[1], &second.turns[1]));
    for index in [0, 1] {
        assert!(Arc::ptr_eq(
            first.turns[1].items().nth(index).unwrap(),
            second.turns[1].items().nth(index).unwrap()
        ));
    }
    assert_eq!(first.turns[1].items().nth(2).unwrap().data.body, "hello");
    assert_eq!(
        second.turns[1].items().nth(2).unwrap().data.body,
        "hello world"
    );
    let mut deferred = updated;
    let thread = Arc::make_mut(
        Arc::make_mut(&mut deferred.conversations)
            .get_mut(&agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            })
            .unwrap(),
    );
    Arc::make_mut(
        &mut Arc::make_mut(&mut thread.turns.as_mut().unwrap()[1])
            .items
            .as_mut()
            .unwrap()[1],
    )
    .defer();
    let third = project_snapshot(deferred, Some(&second));
    assert!(!Arc::ptr_eq(
        second.turns[1].items().nth(1).unwrap(),
        third.turns[1].items().nth(1).unwrap()
    ));
    assert!(third.turns[1].items().nth(1).unwrap().data.deferred);
    assert!(Arc::ptr_eq(
        second.turns[1].items().nth(2).unwrap(),
        third.turns[1].items().nth(2).unwrap()
    ));
}
#[test]
fn requests_and_pending_submissions_have_one_shared_native_projection() {
    let mut snapshot = fixture();
    for value in [
        json!({"id": "1", "target": {"turn": {"turnId": "done", "itemId": null}}, "delivery": "awaiting", "body": {"approval": {"kind": "command", "description": "run command", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "meaning": "allow", "scope": "once"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "meaning": "allow", "scope": "session"}, {"id": "choice-2", "label": "拒否", "description": "", "meaning": "deny", "scope": "once"}, {"id": "choice-3", "label": "キャンセル", "description": "", "meaning": "cancel", "scope": "once"}]}}}),
        json!({"id": "question", "target": {"turn":{"turnId":"live","itemId":null}}, "delivery": "awaiting", "body": {"question": {"questions": [{"id": "question-0", "header": "", "prompt": "which?", "secret": false, "allowFreeText": true, "multiple": false, "choices": []}]}}}),
        json!({"id": "3", "target": "session", "delivery": "awaiting", "body": {"approval": {"kind": "fileChange", "description": "", "details": "", "choices": [{"id": "choice-0", "label": "承認", "description": "", "meaning": "allow", "scope": "once"}, {"id": "choice-1", "label": "このセッションで承認", "description": "", "meaning": "allow", "scope": "session"}, {"id": "choice-2", "label": "拒否", "description": "", "meaning": "deny", "scope": "once"}, {"id": "choice-3", "label": "キャンセル", "description": "", "meaning": "cancel", "scope": "once"}]}}}),
    ] {
        let request: WireRequest = serde_json::from_value(value).unwrap();
        let request = Arc::new(request);
        let session = agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: if request.id.as_str() == "3" {
                "other"
            } else {
                "thread"
            }
            .into(),
        };
        let thread = Arc::make_mut(
            Arc::make_mut(&mut snapshot.conversations)
                .entry(session.clone())
                .or_insert_with(|| {
                    Arc::new(models::Thread {
                        id: Some(session),
                        ..Default::default()
                    })
                }),
        );
        thread.requests.insert(request.id.clone(), request);
    }
    let pending = |turn_id| {
        Arc::new(PendingSubmission {
            sequence: 0,
            draft_key: agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "thread".into(),
            }
            .into(),
            draft: Arc::new(Draft {
                text: "queued text".into(),
                ..Default::default()
            }),
            turn_id,
            after_item_id: None,
            accepted: true,
            delivery_unknown: false,
        })
    };
    Arc::make_mut(&mut snapshot.pending_submissions)
        .insert("accepted".into(), pending(Some("live".into())));
    Arc::make_mut(&mut snapshot.pending_submissions).insert("queued".into(), pending(None));
    let rendered = project_snapshot(snapshot, None);
    let done: Vec<_> = rendered.turns[0]
        .rows
        .iter()
        .filter_map(|row| match &row.content {
            ConversationRowContent::PendingRequest { request } => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].id.as_str(), "1");
    assert_eq!(done[0].title, "コマンドの承認待ち");
    assert_eq!(done[0].body, "run command");
    assert_eq!(
        done[0]
            .request_body
            .choices()
            .iter()
            .map(|choice| choice.label.as_str())
            .collect::<Vec<_>>(),
        ["承認", "このセッションで承認", "拒否", "キャンセル"]
    );
    let live: Vec<_> = rendered.turns[1]
        .rows
        .iter()
        .filter_map(|row| match &row.content {
            ConversationRowContent::PendingRequest { request } => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].body, "which?");
    assert_eq!(
        rendered.turns[1]
            .items()
            .filter(|item| item.data.id == "accepted")
            .count(),
        1
    );
    assert_eq!(rendered.queued.len(), 1);
    assert_eq!(rendered.queued[0].data.body, "queued text");
}
#[test]
fn retry_error_titles_use_host_evidence_without_reinterpreting_native_codes() {
    let error = turn_error(&models::ExecutionError {
        category: models::ErrorCategory::Network,
        message: "retry".into(),
        retry: Some(models::RetryEvidence {
            retrying: true,
            overloaded: true,
            attempt: Some(2),
            max_attempts: Some(4),
        }),
        ..Default::default()
    });
    assert_eq!(error.title, "サーバーが混み合っています。再接続しています");
    assert!(error.is_reconnecting);
    assert_eq!(error.message, "retry");
}
#[rstest::rstest]
#[case::text_wins(false, "answer", vec!["first", "second"], agent_protocol::requests::QuestionAnswer::FreeText { text: "answer".into() })]
#[case::whitespace_is_preserved(true, " ", vec!["first"], agent_protocol::requests::QuestionAnswer::FreeText { text: " ".into() })]
#[case::single_uses_first(false, "", vec!["second", "first"], agent_protocol::requests::QuestionAnswer::SingleChoice { choice_id: "second".into() })]
#[case::empty_single(false, "", vec![], agent_protocol::requests::QuestionAnswer::SingleChoice { choice_id: String::new() })]
#[case::multiple_keeps_order(true, "", vec!["second", "first"], agent_protocol::requests::QuestionAnswer::MultipleChoices { choice_ids: vec!["second".into(), "first".into()] })]
fn question_editor_preserves_text_and_choice_precedence(
    #[case] multiple: bool,
    #[case] text: &str,
    #[case] choices: Vec<&str>,
    #[case] expected: agent_protocol::requests::QuestionAnswer,
) {
    assert_eq!(
        build_question_answer(
            multiple,
            text.into(),
            choices.into_iter().map(str::to_owned).collect()
        ),
        expected
    );
}

#[test]
fn tool_editor_decodes_content_and_rejects_unknown_fields_and_invalid_images() {
    use agent_protocol::requests::{Answer, RequestBody, ToolContent};
    let body = RequestBody::ToolExecution {
        tool: "render".into(),
        namespace: None,
        arguments: json!({}),
    };
    assert_eq!(
        answer_from_json(
            &body,
            r#"{"success":false,"content":[{"text":{"text":"retry"}}]}"#
        )
        .unwrap(),
        Answer::ToolExecution {
            success: false,
            content: vec![ToolContent::Text {
                text: "retry".into()
            }]
        }
    );
    let defaults = request_input_default(body.clone());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&defaults).unwrap(),
        json!({"success":true,"content":[]})
    );
    assert_eq!(
        answer_from_json(&body, &defaults).unwrap(),
        Answer::ToolExecution {
            success: true,
            content: vec![]
        }
    );
    for invalid in [
        r#"{"success":true,"content":[],"extra":true}"#,
        r#"{"success":true}"#,
        r#"{"success":true,"content":[{"image":{"dataUrl":"https://example.com/image.png"}}]}"#,
    ] {
        assert!(answer_from_json(&body, invalid).is_err(), "{invalid}");
    }
    assert_eq!(
        answer_from_json(&RequestBody::Question { questions: vec![] }, "{}").unwrap_err(),
        "this request requires a typed choice or question answer"
    );
}

#[test]
fn form_editor_keeps_defaults_and_validates_the_submitted_values() {
    use agent_protocol::requests::{
        Answer, ElicitationAnswer, ElicitationInput, FormChoice, FormField, FormInput, RequestBody,
    };
    let field = |name: &str, required, input| FormField {
        name: name.into(),
        title: name.into(),
        description: String::new(),
        required,
        input,
    };
    let body = RequestBody::Elicitation {
        server: "service".into(),
        message: "settings".into(),
        input: ElicitationInput::Form {
            fields: vec![
                field(
                    "name",
                    true,
                    FormInput::String {
                        min_length: Some(1),
                        max_length: None,
                        format: None,
                        default: Some("日本語".into()),
                    },
                ),
                field(
                    "enabled",
                    false,
                    FormInput::Boolean {
                        default: Some(false),
                    },
                ),
                field(
                    "count",
                    false,
                    FormInput::Number {
                        integer: true,
                        minimum: Some(0.),
                        maximum: Some(5.),
                        default: Some(0.),
                    },
                ),
                field(
                    "empty",
                    false,
                    FormInput::Multiple {
                        choices: vec![],
                        min_items: None,
                        max_items: None,
                        default: vec![],
                    },
                ),
                field("unset", false, FormInput::Boolean { default: None }),
                field(
                    "choice",
                    false,
                    FormInput::Choice {
                        choices: vec![FormChoice {
                            value: "a".into(),
                            title: "A".into(),
                        }],
                        default: Some("a".into()),
                    },
                ),
                field(
                    "multiple",
                    false,
                    FormInput::Multiple {
                        choices: vec![
                            FormChoice {
                                value: "b".into(),
                                title: "B".into(),
                            },
                            FormChoice {
                                value: "a".into(),
                                title: "A".into(),
                            },
                        ],
                        min_items: Some(1),
                        max_items: Some(2),
                        default: vec!["b".into(), "a".into()],
                    },
                ),
            ],
        },
    };
    let text = request_input_default(body.clone());
    let expected =
        json!({"name":"日本語","enabled":false,"count":0.0,"choice":"a","multiple":["b","a"]});
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap(),
        expected
    );
    assert_eq!(
        answer_from_json(&body, &text).unwrap(),
        Answer::Elicitation {
            action: ElicitationAnswer::Accept { values: expected }
        }
    );
    for invalid in [
        "{}",
        r#"{"name":"ok","extra":1}"#,
        r#"{"name":"ok","count":6}"#,
        "null",
    ] {
        assert!(answer_from_json(&body, invalid).is_err(), "{invalid}");
    }
    let url = RequestBody::Elicitation {
        server: "service".into(),
        message: "continue".into(),
        input: ElicitationInput::Url {
            url: "https://example.com/".into(),
        },
    };
    assert_eq!(request_input_default(url.clone()), "null");
    assert!(answer_from_json(&url, "null").is_ok());
    assert!(answer_from_json(&url, "{}").is_err());
}

#[test]
fn request_delivery_changes_only_its_turn_and_reuses_queued_items() {
    use agent_protocol::{
        requests::{RequestBody, RequestTarget},
        session::RequestDelivery,
    };
    let mut snapshot = fixture();
    let session = agent_protocol::session::SessionRef {
        provider: agent_protocol::session::ProviderKind::Codex,
        id: "thread".into(),
    };
    let pending = Arc::new(PendingSubmission {
        sequence: 0,
        draft_key: session.clone().into(),
        draft: Arc::new(Draft {
            text: "queued".into(),
            ..Default::default()
        }),
        turn_id: None,
        after_item_id: None,
        accepted: true,
        delivery_unknown: false,
    });
    Arc::make_mut(&mut snapshot.pending_submissions).insert("queued".into(), pending);
    let wire = WireRequest {
        id: "question".into(),
        target: RequestTarget::Turn {
            turn_id: "live".into(),
            item_id: None,
        },
        delivery: RequestDelivery::Awaiting,
        body: RequestBody::Question { questions: vec![] },
    };
    Arc::make_mut(
        Arc::make_mut(&mut snapshot.conversations)
            .get_mut(&session)
            .unwrap(),
    )
    .requests
    .insert(wire.id.clone(), Arc::new(wire));
    let first = project_snapshot(snapshot.clone(), None);
    let mut updated = snapshot;
    let thread = Arc::make_mut(
        Arc::make_mut(&mut updated.conversations)
            .get_mut(&session)
            .unwrap(),
    );
    Arc::make_mut(thread.requests.get_mut("question").unwrap()).delivery = RequestDelivery::Sent;
    let second = project_snapshot(updated, Some(&first));
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(Arc::ptr_eq(&first.turns[0], &second.turns[0]));
    assert!(!Arc::ptr_eq(&first.turns[1], &second.turns[1]));
    assert!(Arc::ptr_eq(&first.turns[1].source, &second.turns[1].source));
    assert!(Arc::ptr_eq(&first.queued[0], &second.queued[0]));
    assert_eq!(first.turns[1].items().count(), 3);
    assert_eq!(second.turns[1].items().count(), 3);
    for (before, after) in first.turns[1].items().zip(second.turns[1].items()) {
        assert!(Arc::ptr_eq(before, after));
    }
    let request_row = |turn: &RenderedTurn| {
        turn.rows
            .iter()
            .find_map(|row| match &row.content {
                ConversationRowContent::PendingRequest { request } => {
                    Some((row.id.clone(), request.can_respond, request.title.clone()))
                }
                _ => None,
            })
            .unwrap()
    };
    let before = request_row(&first.turns[1]);
    let after = request_row(&second.turns[1]);
    assert_eq!(before.0, after.0);
    assert!(before.1);
    assert!(!after.1);
    assert_eq!(after.2, "回答を送信しました");
}

#[rstest::rstest]
#[case::partial(
    crate::session::HistoryReadKind::Partial,
    "履歴の一部を表示しています。"
)]
#[case::incomplete(
    crate::session::HistoryReadKind::Incomplete,
    "履歴の一部を読み取れませんでした。"
)]
#[case::unavailable(
    crate::session::HistoryReadKind::Unavailable,
    "履歴を取得できません。保存済みの表示は最新とは限りません。"
)]
fn history_notices_keep_source_issues_and_bound_the_visible_list(
    #[case] kind: crate::session::HistoryReadKind,
    #[case] heading: &str,
) {
    let mut thread = models::Thread {
        history_read_state: Some(crate::session::HistoryReadState::new(kind, vec![])),
        ..Default::default()
    };
    assert_eq!(history_notice(&thread).as_deref(), Some(heading));
    thread.history_read_state.as_mut().unwrap().issues =
        (0..10).map(|i| format!("issue-{i}")).collect();
    assert_eq!(
        history_notice(&thread).unwrap(),
        format!(
            "{heading}\nissue-0\nissue-1\nissue-2\nissue-3\nissue-4\nissue-5\nissue-6\nissue-7"
        )
    );
    assert_eq!(thread.history_read_state.as_ref().unwrap().issues.len(), 10);
    assert_eq!(history_notice(&models::Thread::default()), None);
    for kind in [
        crate::session::HistoryReadKind::Complete,
        crate::session::HistoryReadKind::Other,
    ] {
        thread.history_read_state.as_mut().unwrap().kind = kind;
        assert_eq!(history_notice(&thread), None);
    }
}

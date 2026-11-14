use super::*;
use crate::state::operations::Operation;
use agent_protocol::{
    conversation::Outcome,
    orchestration::{self as model, projection::ThreadStreamItem},
};

#[test]
fn unsubmitted_project_drafts_keep_their_address_across_navigation_and_restoration() {
    let new_chat = |snapshot: &Snapshot, cwd: &str| {
        crate::state::reduce(
            snapshot,
            "",
            crate::state::Event::Intent(op::Intent::NewChat { cwd: cwd.into() }),
        )
        .0
    };
    let first = new_chat(&Snapshot::default(), "/one");
    let key = first.panes.primary().navigation.draft_key.clone();
    let DraftKey::Session { session } = &key else {
        panic!("draft requires its conversation address")
    };
    assert_eq!(session.provider, None);
    assert!(uuid::Uuid::parse_str(&session.id).is_ok());
    let mut edited = first;
    Arc::make_mut(&mut edited.input.drafts).insert(
        key.clone(),
        Arc::new(Draft {
            text: "keep me".into(),
            ..Default::default()
        }),
    );
    let elsewhere = new_chat(&edited, "/two");
    assert_ne!(elsewhere.panes.primary().navigation.draft_key, key);
    let restored =
        crate::persistence::decode(&crate::persistence::encode(&elsewhere).unwrap()).unwrap();
    let returned = new_chat(&restored, "/one");
    assert_eq!(returned.panes.primary().navigation.draft_key, key);
    assert_eq!(returned.input.drafts[&key].text, "keep me");
    let mut split = returned;
    split.open_pane("side".into(), "/one".into());
    assert_ne!(split.panes.get("side").unwrap().navigation.draft_key, key);
}

#[test]
fn only_finished_workspace_tools_refresh_the_tree_during_a_turn() {
    use crate::models::{Item, ItemBody, ItemStatus};
    for body in [
        ItemBody::FileChange {
            changes: vec![],
            output: String::new(),
        },
        ItemBody::CommandExecution {
            output_omitted: false,
            command: "touch new.txt".into(),
            cwd: None,
            output: String::new(),
            exit_code: None,
        },
        ItemBody::ToolCall {
            kind: crate::models::ToolKind::Dynamic,
            tool: "edit".into(),
            server: None,
            namespace: None,
            resource_uri: None,
            plugin_id: None,
            arguments: serde_json::Value::Null,
            result: None,
            error: None,
            content: vec![],
            success: None,
            duration_ms: None,
        },
        ItemBody::Subagent {
            tool: "agent".into(),
            prompt: None,
            model: None,
            effort: None,
            sender: None,
            receivers: vec![],
            states: vec![],
            agent_id: None,
            result: None,
        },
    ] {
        for (status, expected) in [
            (ItemStatus::Running, false),
            (ItemStatus::Completed, true),
            (ItemStatus::Failed, true),
        ] {
            assert_eq!(
                finished_workspace_item(&Item::new("item".into(), status, body.clone())),
                expected
            );
        }
    }
    let prose = Item::new(
        "prose".into(),
        ItemStatus::Completed,
        ItemBody::AssistantText {
            text: "hello".into(),
            phase: crate::models::AssistantPhase::Final,
        },
    );
    assert!(!finished_workspace_item(&prose));
}

fn pending() -> (Snapshot, crate::session::SessionRef, uuid::Uuid, uuid::Uuid) {
    let id =
        crate::session::SessionRef::new(crate::session::ProviderKind::Codex, "conversation".into())
            .unwrap();
    let mut snapshot = Snapshot::default();
    crate::test_conversations::open(
        &mut snapshot,
        crate::models::ThreadResponse {
            thread: Thread {
                id: Some(id.clone()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let operation = uuid::Uuid::new_v4();
    let key = DraftKey::from(&id);
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        key.clone(),
        Arc::new(Draft {
            text: "new text".into(),
            ..Default::default()
        }),
    );
    Arc::make_mut(&mut snapshot.sessions.pending_submissions).insert(
        operation.to_string().into(),
        Arc::new(PendingSubmission {
            new_draft_slot: None,
            cursor: None,
            sequence: 0,
            submitted_at_ms: None,
            draft_key: key,
            draft: Arc::new(Draft {
                text: "sent text".into(),
                ..Default::default()
            }),
            turn_id: None,
            after_item_id: None,
            accepted: false,
            delivery_unknown: true,
            queued: false,
        }),
    );
    let scope = read_scope(&snapshot, &id).unwrap();
    (snapshot, id, operation, scope)
}

#[test]
fn durable_receipts_restore_rejected_input_and_preserve_unknown_input_and_newer_edits() {
    for outcome in [
        Outcome::Rejected("refused".into()),
        Outcome::DeliveryUnknown,
        Outcome::Accepted,
    ] {
        let (mut snapshot, id, operation, scope) = pending();
        let cursor = snapshot.sessions.projections[&id].snapshot_sequence;
        let rejected = matches!(outcome, Outcome::Rejected(_));
        let unknown = outcome == Outcome::DeliveryUnknown;
        let through = cursor + 1;
        CheckReceipts {
            session: id.clone(),
            operations: vec![operation],
        }
        .apply(
            &mut snapshot,
            "",
            (
                scope,
                if unknown {
                    vec![]
                } else {
                    vec![(
                        operation,
                        records::Acknowledged {
                            error: match outcome {
                                Outcome::Rejected(reason) => Some(reason),
                                _ => None,
                            },
                            sequence: through,
                            accepted_at: String::new(),
                        },
                    )]
                },
            ),
        );
        assert_eq!(
            snapshot.input.drafts[&DraftKey::from(&id)].text,
            if rejected {
                "sent text\nnew text"
            } else {
                "new text"
            }
        );
        assert_eq!(snapshot.sessions.pending_submissions.is_empty(), rejected);
        if !rejected {
            let input = &snapshot.sessions.pending_submissions[operation.to_string().as_str()];
            assert_eq!(input.delivery_unknown, unknown);
            assert_eq!(input.accepted, !unknown);
            let mut next = snapshot.sessions.projections[&id].as_ref().clone();
            next.snapshot_sequence = through;
            apply(&mut snapshot, &id, vec![ThreadStreamItem::Snapshot(next)]).unwrap();
            assert_eq!(snapshot.sessions.pending_submissions.is_empty(), !unknown);
        }
    }
}

#[test]
fn receipt_from_a_replaced_subscription_cannot_restore_or_discard_new_input() {
    let (mut snapshot, id, operation, _) = pending();
    let cursor = snapshot.sessions.projections[&id].snapshot_sequence;
    let scope = uuid::Uuid::new_v4();
    let before = snapshot.clone();
    CheckReceipts {
        session: id,
        operations: vec![operation],
    }
    .apply(
        &mut snapshot,
        "",
        (
            scope,
            vec![(
                operation,
                records::Acknowledged {
                    error: Some("old".into()),
                    sequence: cursor,
                    accepted_at: String::new(),
                },
            )],
        ),
    );
    assert_eq!(snapshot, before);
}

#[test]
fn an_authoritative_t3_snapshot_replaces_the_captured_sequence_without_a_store_identity() {
    let (snapshot, id, _, scope) = pending();
    let base = capture(&snapshot, &id).unwrap();
    let mut initial = base.projection.as_deref().unwrap().clone();
    initial.snapshot_sequence = 0;
    let result = opened(
        &base,
        records::Opened {
            session: id,
            subscription_id: scope,
            initial: vec![ThreadStreamItem::Snapshot(initial)],
            provider: crate::session::ProviderKind::Codex,
            capabilities: Default::default(),
            project_root: None,
        },
    )
    .unwrap();
    assert_eq!(result.projection.snapshot_sequence, 0);
    assert!(result.replaced);
}

#[test]
fn streaming_a_conversation_without_a_folder_keeps_the_panes_context_and_error() {
    let (mut snapshot, id, _, _) = pending();
    let mut next = snapshot.sessions.projections[&id].as_ref().clone();
    next.snapshot_sequence += 1;
    let pane = Arc::make_mut(snapshot.panes.primary_mut());
    let navigation = Arc::make_mut(&mut pane.navigation);
    navigation.thread_id = Some(id.clone());
    navigation.cwd = "/previous-folder".into();
    pane.error = Some("Keep this operation error".into());
    let epoch = pane.epoch;
    apply(&mut snapshot, &id, vec![ThreadStreamItem::Snapshot(next)]).unwrap();
    assert_eq!(snapshot.panes.primary().epoch, epoch);
    assert_eq!(snapshot.panes.primary().navigation.cwd, "/previous-folder");
    assert_eq!(
        snapshot.panes.primary().error.as_deref(),
        Some("Keep this operation error")
    );
}

#[test]
fn details_finish_after_unrelated_live_updates_without_mutating_the_canonical_stream() {
    use agent_protocol::{
        execution::{ItemStatus, TurnStatus},
        items::{Item, ItemBody},
        models::Turn,
    };
    let id = crate::session::SessionRef::new(None, "detail-thread".into()).unwrap();
    let mut snapshot = Snapshot::default();
    crate::test_conversations::open(
        &mut snapshot,
        crate::models::ThreadResponse {
            thread: Thread {
                id: Some(id.clone()),
                active_provider: Some(crate::session::ProviderKind::Codex),
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    status: TurnStatus::Completed,
                    items: Some(vec![Arc::new(Item::new(
                        "command".into(),
                        ItemStatus::Completed,
                        ItemBody::CommandExecution {
                            command: "echo result".into(),
                            cwd: None,
                            output: "full output".into(),
                            exit_code: Some(0),
                            output_omitted: false,
                        },
                    ))]),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let full = snapshot.sessions.projections[&id].projection.turn_items[0]
        .as_ref()
        .clone();
    let mut wire = snapshot.sessions.projections[&id].as_ref().clone();
    let projected = Arc::new(model::wire::item(&full));
    wire.projection.turn_items[0] = projected.clone();
    wire.projection.visible_turn_items[0].item = projected;
    wire.snapshot_sequence += 1;
    apply(
        &mut snapshot,
        &id,
        vec![ThreadStreamItem::Snapshot(wire.clone())],
    )
    .unwrap();
    let query = || crate::state::operations::ReadItem {
        thread_id: id.clone(),
        turn_id: "turn".into(),
        item_id: "command".into(),
    };
    let (subscription, _) = query().capture(&snapshot, "").unwrap();
    wire.snapshot_sequence += 1;
    wire.projection.thread.title = "unrelated update".into();
    apply(&mut snapshot, &id, vec![ThreadStreamItem::Snapshot(wire)]).unwrap();
    let canonical = snapshot.sessions.projections[&id].clone();
    query()
        .complete(&mut snapshot, "", (subscription, Some(full.clone())), true)
        .unwrap();
    assert!(Arc::ptr_eq(&canonical, &snapshot.sessions.projections[&id]));
    assert_eq!(snapshot.sessions.projections[&id].snapshot_sequence, 3);
    assert_eq!(
        snapshot.sessions.item_details[&(id.application_id(), full.id.clone())].as_ref(),
        &full
    );
    assert!(
        matches!(snapshot.sessions.conversations[&id].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0].body(),ItemBody::CommandExecution {output,..} if output=="full output")
    );
    let mut revised = canonical.as_ref().clone();
    revised.snapshot_sequence += 1;
    Arc::make_mut(&mut revised.projection.turn_items[0]).updated_at =
        "2026-11-14T00:00:01.000Z".into();
    revised.projection.visible_turn_items[0].item = revised.projection.turn_items[0].clone();
    apply(
        &mut snapshot,
        &id,
        vec![ThreadStreamItem::Snapshot(revised)],
    )
    .unwrap();
    let before = snapshot.clone();
    query()
        .complete(&mut snapshot, "", (subscription, Some(full)), true)
        .unwrap();
    assert_eq!(
        snapshot, before,
        "an obsolete body cannot change the current detail revision or stream"
    );
}

#[test]
fn new_chat_after_admitting_the_first_input_allocates_an_independent_draft() {
    use crate::state::{Event, Intent, reduce};
    let first = reduce(
        &Snapshot::default(),
        "",
        Event::Intent(Intent::NewChat {
            cwd: "/project".into(),
        }),
    )
    .0;
    let key = first.panes.primary().navigation.draft_key.clone();
    let draft = Arc::new(Draft {
        text: "first".into(),
        model: Some(crate::models::ModelRef {
            provider: crate::session::ProviderKind::Codex,
            id: "model".into(),
        }),
        ..Default::default()
    });
    let destination = super::super::Destination {
        thread_id: None,
        cwd: Some("/project".into()),
        draft_key: key.clone(),
    };
    let (submitted, effects) = super::super::submission(
        &first,
        destination,
        draft,
        uuid::Uuid::new_v4().to_string().into(),
        None,
        Default::default(),
    );
    assert_eq!(effects.len(), 1);
    assert_eq!(submitted.sessions.pending_submissions.len(), 1);
    let next = reduce(
        &submitted,
        "",
        Event::Intent(Intent::NewChat {
            cwd: "/project".into(),
        }),
    )
    .0;
    assert_ne!(next.panes.primary().navigation.draft_key, key);
    assert!(next.panes.primary().navigation.thread_id.is_none());
    assert_eq!(next.sessions.pending_submissions.len(), 1);
}

#[test]
fn a_rejected_first_message_returns_to_its_folder_and_merges_a_newer_draft() {
    use crate::state::{Event, Intent, reduce};
    for occupied in [false, true] {
        let first = reduce(
            &Snapshot::default(),
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/project".into(),
            }),
        )
        .0;
        let original = first.panes.primary().navigation.draft_key.clone();
        let input: agent_protocol::ids::ClientInputId = uuid::Uuid::new_v4().to_string().into();
        let draft = Arc::new(Draft {
            text: "recover me".into(),
            model: Some(crate::models::ModelRef {
                provider: crate::session::ProviderKind::Codex,
                id: "model".into(),
            }),
            ..Default::default()
        });
        let (mut pending, _) = super::super::submission(
            &first,
            super::super::Destination {
                thread_id: None,
                cwd: Some("/project".into()),
                draft_key: original,
            },
            draft,
            input.clone(),
            None,
            Default::default(),
        );
        if occupied {
            pending = reduce(
                &pending,
                "",
                Event::Intent(Intent::NewChat {
                    cwd: "/project".into(),
                }),
            )
            .0;
            let key = pending.panes.primary().navigation.draft_key.clone();
            Arc::make_mut(&mut pending.input.drafts).insert(
                key,
                Arc::new(Draft {
                    text: "new typing".into(),
                    ..Default::default()
                }),
            );
        }
        let failed = reduce(&pending, "", Event::SubmissionFailed(input)).0;
        let elsewhere = reduce(
            &failed,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/else".into(),
            }),
        )
        .0;
        let saved =
            crate::persistence::decode(&crate::persistence::encode(&elsewhere).unwrap()).unwrap();
        let returned = reduce(
            &saved,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/project".into(),
            }),
        )
        .0;
        assert_eq!(
            returned.input.drafts[&returned.panes.primary().navigation.draft_key].text,
            if occupied {
                "recover me\nnew typing"
            } else {
                "recover me"
            }
        );
    }
}

#[test]
fn a_rejected_welcome_send_restores_reachable_user_work_and_newer_typing() {
    use crate::state::{Event, Intent, reduce};
    for occupied in [false, true] {
        let mut first = Snapshot::default();
        let placeholder = first.panes.primary().navigation.draft_key.clone();
        let draft = Arc::new(Draft {
            text: "welcome work".into(),
            model: Some(crate::models::ModelRef {
                provider: crate::session::ProviderKind::Codex,
                id: "model".into(),
            }),
            attachments: vec![Attachment {
                path: "/picture.png".into(),
                name: "picture.png".into(),
                is_image: true,
            }],
            invocations: vec![agent_protocol::composer::Invocation {
                provider: crate::session::ProviderKind::Codex,
                kind: agent_protocol::composer::InvocationKind::Skill,
                name: "kept".into(),
                path: "/SKILL.md".into(),
            }],
            ..Default::default()
        });
        first = reduce(
            &first,
            "",
            Event::Intent(Intent::SetDraft {
                thread_id: placeholder,
                draft: (*draft).clone(),
            }),
        )
        .0;
        let key = first.panes.primary().navigation.draft_key.clone();
        let input: agent_protocol::ids::ClientInputId = uuid::Uuid::new_v4().to_string().into();
        let (mut pending, _) = super::super::submission(
            &first,
            super::super::Destination {
                thread_id: None,
                cwd: None,
                draft_key: key.clone(),
            },
            draft.clone(),
            input.clone(),
            None,
            Default::default(),
        );
        assert_eq!(pending.panes.primary().navigation.draft_key, key);
        if occupied {
            Arc::make_mut(&mut pending.input.drafts).insert(
                key.clone(),
                Arc::new(Draft {
                    text: "new typing".into(),
                    ..Default::default()
                }),
            );
        }
        let pending = reduce(&pending, "", Event::Intent(Intent::ShowThreadList)).0;
        assert!(pending.panes.primary().navigation.draft_key.is_empty());
        let failed = reduce(&pending, "", Event::SubmissionFailed(input)).0;
        assert_eq!(failed.panes.primary().navigation.draft_key, key);
        let elsewhere = reduce(
            &failed,
            "",
            Event::Intent(Intent::NewChat {
                cwd: "/else".into(),
            }),
        )
        .0;
        let restored =
            crate::persistence::decode(&crate::persistence::encode(&elsewhere).unwrap()).unwrap();
        let returned = reduce(&restored, "", Event::Intent(Intent::ShowThreadList)).0;
        assert_eq!(returned.panes.primary().navigation.draft_key, key);
        let mut returned = returned;
        let pane = Arc::make_mut(returned.panes.get_mut("").unwrap());
        Arc::make_mut(&mut pane.workspace).review_cwd = Some("/temporary-review".into());
        let returned = reduce(&returned, "", Event::Intent(Intent::ShowThreadList)).0;
        assert!(returned.panes.primary().workspace.review_cwd.is_none());
        assert_eq!(returned.panes.primary().navigation.draft_key, key);
        let recovered = &returned.input.drafts[&key];
        assert_eq!(
            recovered.text,
            if occupied {
                "welcome work\nnew typing"
            } else {
                "welcome work"
            }
        );
        assert_eq!(recovered.attachments, draft.attachments);
        assert_eq!(recovered.invocations, draft.invocations);
    }
}

use super::*;
use crate::state::operations::Operation;
use agent_protocol::{
    conversation::Outcome,
    sync::{Cursor, Message},
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

fn pending() -> (Snapshot, crate::session::SessionRef, uuid::Uuid, ReadScope) {
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
        let rejected = matches!(outcome, Outcome::Rejected(_));
        let unknown = outcome == Outcome::DeliveryUnknown;
        let cursor = Cursor {
            sequence: scope.cursor.sequence + 1,
            ..scope.cursor
        };
        CheckReceipts {
            session: id.clone(),
            operations: vec![operation],
        }
        .apply(
            &mut snapshot,
            "",
            (
                scope,
                vec![(operation, records::Acknowledged { outcome, cursor })],
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
            apply(
                &mut snapshot,
                &id,
                vec![Message::Changes {
                    from: scope.cursor,
                    through: cursor,
                    rows: vec![],
                    removed: vec![],
                }],
            )
            .unwrap();
            assert_eq!(snapshot.sessions.pending_submissions.is_empty(), !unknown);
        }
    }
}

#[test]
fn receipt_from_a_replaced_subscription_cannot_restore_or_discard_new_input() {
    let (mut snapshot, id, operation, mut scope) = pending();
    scope.subscription = uuid::Uuid::new_v4();
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
                    outcome: Outcome::Rejected("old".into()),
                    cursor: scope.cursor,
                },
            )],
        ),
    );
    assert_eq!(snapshot, before);
}

#[test]
fn older_same_store_snapshot_cannot_replace_the_captured_conversation() {
    let (snapshot, id, _, scope) = pending();
    let base = &snapshot.sessions.replicas[&id];
    let mut initial = base.snapshot().unwrap();
    if let Message::Snapshot { cursor, .. } = &mut initial {
        cursor.sequence -= 1;
    }
    assert!(
        opened(
            base,
            records::Opened {
                session: id,
                subscription_id: scope.subscription,
                initial
            }
        )
        .is_err()
    );
}

#[test]
fn obsolete_reads_preserve_the_new_cursor_and_never_surface_their_error() {
    let (mut snapshot, id, _, scope) = pending();
    let next = Cursor {
        sequence: scope.cursor.sequence + 1,
        ..scope.cursor
    };
    apply(
        &mut snapshot,
        &id,
        vec![Message::Changes {
            from: scope.cursor,
            through: next,
            rows: vec![],
            removed: vec![],
        }],
    )
    .unwrap();
    let before = snapshot.clone();
    assert!(matches!(
        finish_read(
            &mut snapshot,
            &id,
            ReadRows {
                scope,
                result: Err(PeerError::InvalidMessage("old error".into()))
            },
            true
        ),
        Ok(ReadResult::Retry)
    ));
    assert_eq!(snapshot, before);
}

#[test]
fn streaming_a_conversation_without_a_folder_keeps_the_panes_context_and_error() {
    let (mut snapshot, id, _, scope) = pending();
    let pane = Arc::make_mut(snapshot.panes.primary_mut());
    let navigation = Arc::make_mut(&mut pane.navigation);
    navigation.thread_id = Some(id.clone());
    navigation.cwd = "/previous-folder".into();
    pane.error = Some("Keep this operation error".into());
    let epoch = pane.epoch;
    apply(
        &mut snapshot,
        &id,
        vec![Message::Changes {
            from: scope.cursor,
            through: Cursor {
                sequence: scope.cursor.sequence + 1,
                ..scope.cursor
            },
            rows: vec![],
            removed: vec![],
        }],
    )
    .unwrap();
    assert_eq!(snapshot.panes.primary().epoch, epoch);
    assert_eq!(snapshot.panes.primary().navigation.cwd, "/previous-folder");
    assert_eq!(
        snapshot.panes.primary().error.as_deref(),
        Some("Keep this operation error")
    );
}

#[test]
fn hydration_finishes_after_unrelated_live_updates_without_restarting_the_read() {
    use agent_protocol::{
        execution::ItemStatus,
        items::{AssistantPhase, Item, ItemBody},
        models::Turn,
    };
    let id = crate::session::SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let mut snapshot = Snapshot::default();
    crate::test_conversations::open(
        &mut snapshot,
        crate::models::ThreadResponse {
            thread: Thread {
                id: Some(id.clone()),
                active_provider: Some(crate::session::ProviderKind::Codex),
                turns: Some(vec![Arc::new(Turn {
                    id: "turn".into(),
                    items: Some(vec![Arc::new(Item::new(
                        "answer".into(),
                        ItemStatus::Completed,
                        ItemBody::AssistantText {
                            text: "full answer".into(),
                            phase: AssistantPhase::Final,
                        },
                    ))]),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let scope = read_scope(&snapshot, &id).unwrap();
    let Message::Snapshot {
        cursor,
        window,
        mut rows,
    } = snapshot.sessions.replicas[&id].snapshot().unwrap()
    else {
        panic!("snapshot")
    };
    let full = rows.iter().find(|row| matches!(&row.key,agent_protocol::sync::Key::Item {id,..} if id.as_str()=="answer")).unwrap().clone();
    records::defer(rows.iter_mut().find(|row| row.key == full.key).unwrap());
    let generation = snapshot.sessions.replicas[&id].subscription();
    let deferred = records::Replica::new(
        agent_protocol::sync::Scope::Conversation(id.clone()),
        generation,
    )
    .apply(
        generation,
        Message::Snapshot {
            cursor,
            window,
            rows,
        },
    )
    .unwrap();
    Arc::make_mut(&mut snapshot.sessions.replicas).insert(id.clone(), Arc::new(deferred));
    let latest = Cursor {
        sequence: cursor.sequence + 1,
        ..cursor
    };
    apply(
        &mut snapshot,
        &id,
        vec![Message::Changes {
            from: cursor,
            through: latest,
            rows: vec![],
            removed: vec![],
        }],
    )
    .unwrap();
    assert!(matches!(
        finish_read(
            &mut snapshot,
            &id,
            ReadRows {
                scope,
                result: Ok(vec![Message::Hydrate {
                    cursor,
                    row: full.clone()
                }])
            },
            true
        ),
        Ok(ReadResult::Applied(_))
    ));
    assert_eq!(snapshot.sessions.replicas[&id].rows()[&full.key], full);
    assert_eq!(snapshot.sessions.replicas[&id].cursor(), Some(latest));
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

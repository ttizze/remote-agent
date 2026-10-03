use agent_protocol::{
    models::{
        AssistantPhase, ErrorCategory, ExecutionError, Item, ItemBody, ItemStatus, RetryEvidence,
        Thread, Turn, TurnStatus,
    },
    session::{ProviderKind, SessionChange, SessionRef, TextField},
};
use serde_json::json;
use std::sync::Arc;

fn conversation() -> Thread {
    serde_json::from_value(json!({"id":{"provider":"codex","id":"native"},"turns":[{"id":"run","status":"running","items":[{"id":"answer","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"partial","phase":"unknown"}}}}}]}]})).unwrap()
}

#[rstest::rstest]
#[case::minimum(0, None, 0, 5)]
#[case::recorded_window(5, Some(24), 1, 24)]
#[case::loaded_window(5, None, 8, 8)]
#[case::larger_request(40, Some(24), 8, 40)]
#[case::bounded_wire_limit(5, Some(u64::MAX), 1, u32::MAX)]
fn history_refresh_preserves_the_requested_and_loaded_windows(
    #[case] requested: u32,
    #[case] history_limit: Option<u64>,
    #[case] loaded_turns: usize,
    #[case] expected: u32,
) {
    use agent_core::state::{
        Snapshot,
        operations::{Operation, ReadThread},
    };
    let id = SessionRef::new(ProviderKind::Codex, "native".into()).unwrap();
    let mut cached = conversation();
    cached.history_limit = history_limit;
    cached.turns = Some(
        (0..loaded_turns)
            .map(|index| {
                Arc::new(Turn {
                    id: format!("turn-{index}").into(),
                    ..Default::default()
                })
            })
            .collect(),
    );
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.conversations).insert(id.clone(), Arc::new(cached));
    let mut read = ReadThread {
        limit: requested,
        ..ReadThread::new(id)
    };
    read.prepare(&mut snapshot).unwrap();
    assert_eq!(read.limit, expected);
}

#[test]
fn session_error_survives_binary_transport_and_updates_the_turn() {
    for retrying in [false, true] {
        let error = ExecutionError {
            category: ErrorCategory::RateLimited,
            message: "provider failed".into(),
            details: Some("Retry after 1 second".into()),
            retry: Some(RetryEvidence {
                retrying,
                overloaded: false,
                attempt: Some(2),
                max_attempts: Some(4),
            }),
            ..Default::default()
        };
        let change = SessionChange::Error {
            turn_id: "run".into(),
            error: error.clone(),
        };
        let bytes = agent_protocol::protocol::encode(&change).unwrap();
        let decoded: SessionChange = agent_protocol::protocol::decode(&bytes).unwrap();
        assert_eq!(decoded, change);
        let updated = decoded.apply(&conversation()).unwrap();
        assert_eq!(
            updated.turns.as_ref().unwrap()[0].error.as_ref(),
            Some(&error)
        );
    }
}

#[test]
fn unavailable_history_preserves_live_turn_requests_and_subsequent_text() {
    use agent_core::state::Event;
    use agent_core::state::Snapshot;
    use agent_core::state::operations::Operation;
    use agent_core::state::operations::ReadThread;
    use agent_core::state::reduce;
    use agent_protocol::session::OpenedSession;
    let cached: Thread = serde_json::from_value(json!({"id":{"provider":"codex","id":"native"},"turns":[{"id":"A","status":"completed","items":[{"id":"past","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"cached history","phase":"unknown"}}}}}]},{"id":"stale","status":"running"}]})).unwrap();
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.conversations).insert(
        agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "native".into(),
        },
        Arc::new(cached),
    );
    let subscription = uuid::Uuid::new_v4();
    let response = serde_json::from_value(json!({"thread":{"id":{"provider":"codex","id":"native"},"status":"running","historyReadState":{"type":"unavailable"},"turns":[{"id":"B","status":"running","items":[{"id":"latest","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"assistantText":{"text":"live","phase":"unknown"}}}}}]}],"requests":{"approval":{"id":"approval","target":{"turn":{"turnId":"B","itemId":null}},"delivery":"awaiting","body":{"approval":{"kind":"command","description":"run","details":"","choices":[]}}}}}})).unwrap();
    ReadThread::new(agent_protocol::session::SessionRef {
        provider: agent_protocol::session::ProviderKind::Codex,
        id: "native".into(),
    })
    .apply(
        &mut snapshot,
        OpenedSession {
            session: SessionRef::new(
                agent_protocol::session::ProviderKind::Codex,
                "native".to_string(),
            )
            .unwrap(),
            subscription_id: subscription,
            response,
        },
    );
    let change = SessionChange::Text {
        turn_id: "B".into(),
        item_id: "latest".into(),
        field: TextField::AssistantText,
        delta: " updated".into(),
    };
    let (snapshot, _) = reduce(
        &snapshot,
        Event::SessionUpdate(Box::new(agent_protocol::session::SessionUpdate {
            subscription_id: subscription,
            change,
        })),
    );
    let thread = &snapshot.conversations[&agent_protocol::session::SessionRef {
        provider: agent_protocol::session::ProviderKind::Codex,
        id: "native".into(),
    }];
    let turns = thread.turns.as_ref().unwrap();
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert_eq!(
        item_text(&(turns[0].items.as_ref().unwrap()[0])),
        Some("cached history")
    );
    assert_eq!(
        item_text(&(turns[1].items.as_ref().unwrap()[0])),
        Some("live updated")
    );
    assert_eq!(
        thread.status,
        agent_protocol::models::SessionStatus::Running
    );
    assert!(thread.requests.contains_key("approval"));
    assert!(snapshot.request("approval").is_some());
}

#[test]
fn provider_identity_keeps_the_complete_native_id() {
    let native = "claude:01234567-89ab-cdef-0123-456789abcdef";
    let claude = SessionRef::new(ProviderKind::Claude, native.into()).unwrap();
    let codex = SessionRef::new(ProviderKind::Codex, native.into()).unwrap();
    assert_eq!(claude.id, native);
    assert_ne!(claude, codex);
    assert_eq!(claude.to_string().parse::<SessionRef>().unwrap(), claude);
    for id in ["", " native", "native "] {
        assert!(SessionRef::new(ProviderKind::Codex, id.into()).is_err());
    }
}

#[test]
fn a_final_item_replaces_streamed_text_without_mutating_the_input() {
    let original = conversation();
    let streamed = SessionChange::Text {
        turn_id: "run".into(),
        item_id: "answer".into(),
        field: TextField::AssistantText,
        delta: " draft".into(),
    }
    .apply(&original)
    .unwrap();
    let completed = SessionChange::Item {
        turn_id: "run".into(),
        item: Item::new(
            "answer".into(),
            ItemStatus::Unknown,
            ItemBody::AssistantText {
                citation: None,
                text: "final answer".into(),
                phase: AssistantPhase::Final,
            },
        )
        .into(),
    }
    .apply(&streamed)
    .unwrap();
    let items = completed.turns.as_ref().unwrap()[0].items.as_ref().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(item_text(&(items[0])), Some("final answer"));
    assert_eq!(original, conversation());
}

#[test]
fn completion_preserves_tool_relationships_and_missing_timing_fields() {
    let mut original = conversation();
    let turn = Arc::make_mut(&mut original.turns.as_mut().unwrap()[0]);
    turn.started_at = Some(123.);
    turn.error = Some(ExecutionError {
        message: "retrying".into(),
        retry: Some(RetryEvidence {
            retrying: true,
            overloaded: false,
            attempt: None,
            max_attempts: None,
        }),
        ..Default::default()
    });
    turn.items.as_mut().unwrap().push(Arc::new(serde_json::from_value(json!({"id":"tool","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"toolCall":{"kind":"mcp","tool":"","server":null,"namespace":null,"arguments":null,"result":{"content":[{"type":"image","data":"fixture"}]},"error":null,"content":[],"success":null,"durationMs":null}}}}})).unwrap()));
    let final_turn = SessionChange::Turn {
        turn: Turn {
            id: "run".into(),
            status: TurnStatus::Completed,
            items: Some(vec![]),
            started_at: None,
            ..Default::default()
        },
        completed: true,
    };
    let completed = final_turn.apply(&original).unwrap();
    let turn = &completed.turns.as_ref().unwrap()[0];
    assert_eq!(turn.started_at, Some(123.));
    assert_eq!(turn.error, None);
    assert_eq!(
        turn.items.as_ref().unwrap()[1],
        original.turns.as_ref().unwrap()[0].items.as_ref().unwrap()[1]
    );
    let late = SessionChange::Turn {
        turn: Turn {
            id: "run".into(),
            ..Default::default()
        },
        completed: false,
    };
    assert_eq!(late.apply(&completed).unwrap(), completed);
}

#[test]
fn updates_target_the_latest_occurrence_without_changing_older_turns() {
    let mut original = conversation();
    let first = original.turns.as_ref().unwrap()[0].clone();
    original.turns.as_mut().unwrap().push(first.clone());
    let updated = SessionChange::Text {
        turn_id: "run".into(),
        item_id: "answer".into(),
        field: TextField::AssistantText,
        delta: " new".into(),
    }
    .apply(&original)
    .unwrap();
    assert!(Arc::ptr_eq(&updated.turns.as_ref().unwrap()[0], &first));
    assert_eq!(
        item_text(&(updated.turns.as_ref().unwrap()[1].items.as_ref().unwrap()[0])),
        Some("partial new")
    );
}

#[test]
fn storage_changes_keep_drafts_separate_and_restore_the_original_area() {
    use agent_core::state::{Draft, Event, Snapshot, reduce};
    let mut original = Snapshot {
        storage_scope: "host-key:area-a".into(),
        ..Default::default()
    };
    Arc::make_mut(&mut original.conversations).insert(
        agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "native".into(),
        },
        Arc::new(conversation()),
    );
    Arc::make_mut(&mut original.drafts).insert(
        agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "native".into(),
        }
        .into(),
        Arc::new(Draft {
            text: "area A draft".into(),
            ..Default::default()
        }),
    );
    let (mut next, _) = reduce(&original, Event::StorageScope("host-key:area-b".into()));
    assert!(next.conversations.is_empty());
    assert!(next.drafts.is_empty());
    assert_eq!(
        next.archived_scopes["host-key:area-a"].drafts[&agent_core::state::DraftKey::from(
            agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "native".into()
            }
        )]
            .text,
        "area A draft"
    );
    Arc::make_mut(&mut next.drafts).insert(
        agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "native".into(),
        }
        .into(),
        Arc::new(Draft {
            text: "area B draft".into(),
            ..Default::default()
        }),
    );
    let saved: Snapshot =
        agent_core::persistence::decode(&agent_core::persistence::encode(&next).unwrap()).unwrap();
    let (restored, _) = reduce(&saved, Event::StorageScope("host-key:area-a".into()));
    assert_eq!(
        restored.drafts[&agent_core::state::DraftKey::from(agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "native".into()
        })]
            .text,
        "area A draft"
    );
    assert_eq!(
        restored.archived_scopes["host-key:area-b"].drafts[&agent_core::state::DraftKey::from(
            agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "native".into()
            }
        )]
            .text,
        "area B draft"
    );
    assert!(restored.conversations.is_empty());
    assert_eq!(
        original.drafts[&agent_core::state::DraftKey::from(agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "native".into()
        })]
            .text,
        "area A draft"
    );
}

#[test]
fn late_completion_does_not_make_a_newer_execution_idle() {
    let old = conversation();
    let running = SessionChange::Turn {
        turn: Turn {
            id: "next".into(),
            ..Default::default()
        },
        completed: false,
    }
    .apply(&old)
    .unwrap();
    let late = SessionChange::Turn {
        turn: Turn {
            id: "run".into(),
            ..Default::default()
        },
        completed: true,
    }
    .apply(&running)
    .unwrap();
    assert_eq!(late.status, agent_protocol::models::SessionStatus::Running);
    assert_eq!(
        late.turns.as_ref().unwrap().last().unwrap().id,
        "next".into()
    );
}

#[test]
fn large_images_are_deferred_without_truncating_base64_or_mutating_native_data() {
    let path = format!("/native/{}/image.png", "a".repeat(300));
    let item: Item = serde_json::from_value(json!({"id":"image","status":"unknown","clientInputId":null,"body":{"inline":{"body":{"imageGeneration":{"savedPath":path,"data":"a".repeat(5 * 1024 * 1024),"revisedPrompt":null}}}}})).unwrap();
    let mut thread = Thread {
        turns: Some(vec![Arc::new(Turn {
            id: "turn".into(),
            items: Some(vec![Arc::new(item.clone())]),
            ..Default::default()
        })]),
        ..Default::default()
    };
    thread.defer_item_details(agent_protocol::models::MAX_INLINE_ITEM_BYTES);
    let turn = &thread.turns.as_ref().unwrap()[0];
    let deferred = &turn.items.as_ref().unwrap()[0];
    assert!(deferred.is_deferred());
    let ItemBody::ImageGeneration {
        saved_path, data, ..
    } = deferred.body()
    else {
        panic!("image body")
    };
    assert_eq!(saved_path.as_deref(), Some(path.as_str()));
    assert!(data.is_none());
    let ItemBody::ImageGeneration { data, .. } = item.body() else {
        panic!("image body")
    };
    assert_eq!(data.as_ref().unwrap().len(), 5 * 1024 * 1024);
}

#[test]
fn native_session_ids_round_trip_without_provider_collisions() {
    for id in ["same-native-id", "claude:native", "codex:native"] {
        let codex = SessionRef {
            provider: ProviderKind::Codex,
            id: id.into(),
        };
        let claude = SessionRef {
            provider: ProviderKind::Claude,
            id: id.into(),
        };
        assert_ne!(codex.to_string(), claude.to_string());
        for session in [codex, claude] {
            assert_eq!(session.to_string().parse::<SessionRef>().unwrap(), session);
        }
    }
}

#[test]
fn local_storage_keeps_user_work_without_host_caches() {
    use agent_core::state::{Draft, FileDraft, Navigation, PendingSubmission, Snapshot};
    let draft = Arc::new(Draft {
        text: "unsent message".into(),
        attachments: vec![agent_core::state::Attachment {
            path: "/uploaded/image.png".into(),
            name: "image.png".into(),
            is_image: true,
        }],
        model: Some(agent_protocol::models::ModelRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "chosen-model".into(),
        }),
        ..Default::default()
    });
    let mut original = Snapshot {
        storage_scope: "host:area".into(),
        conversations: Arc::new(
            [(
                agent_protocol::session::SessionRef {
                    provider: agent_protocol::session::ProviderKind::Codex,
                    id: "native".into(),
                },
                Arc::new(conversation()),
            )]
            .into(),
        ),
        drafts: Arc::new(
            [(
                SessionRef {
                    provider: ProviderKind::Codex,
                    id: "native".into(),
                }
                .into(),
                draft.clone(),
            )]
            .into(),
        ),
        pending_submissions: Arc::new(
            [(
                "send-id".into(),
                Arc::new(PendingSubmission {
                    sequence: 0,
                    draft_key: SessionRef {
                        provider: ProviderKind::Codex,
                        id: "native".into(),
                    }
                    .into(),
                    draft,
                    turn_id: Some("turn".into()),
                    after_item_id: None,
                    accepted: false,
                    delivery_unknown: true,
                }),
            )]
            .into(),
        ),
        file_drafts: Arc::new(
            [(
                "/file.txt".into(),
                Arc::new(FileDraft {
                    revision: "revision".into(),
                    text: "unsaved edit".into(),
                }),
            )]
            .into(),
        ),
        navigation: Arc::new(Navigation {
            thread_id: Some(agent_protocol::session::SessionRef {
                provider: agent_protocol::session::ProviderKind::Codex,
                id: "native".into(),
            }),
            cwd: "/project".into(),
            draft_key: SessionRef {
                provider: ProviderKind::Codex,
                id: "native".into(),
            }
            .into(),
        }),
        epoch: 99,
        ..Default::default()
    };
    Arc::make_mut(&mut original.activity)
        .unread
        .insert(agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "native".into(),
        });
    Arc::make_mut(&mut original.activity).active.insert(
        agent_protocol::session::SessionRef {
            provider: agent_protocol::session::ProviderKind::Codex,
            id: "native".into(),
        },
        true,
    );
    let bytes = agent_core::persistence::encode(&original).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mut keys: Vec<_> = saved
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "activity",
            "archived_scopes",
            "drafts",
            "file_drafts",
            "model_defaults",
            "navigation",
            "pending_submissions",
            "scoped_model_defaults",
            "storage_scope"
        ]
    );
    assert_eq!(
        saved["activity"],
        json!({"unread":[{"provider":"codex","id":"native"}]})
    );
    let restored = agent_core::persistence::decode(&bytes).unwrap();
    assert_eq!(restored.drafts, original.drafts);
    assert_eq!(restored.pending_submissions, original.pending_submissions);
    assert_eq!(restored.file_drafts, original.file_drafts);
    assert_eq!(restored.navigation, original.navigation);
    assert_eq!(restored.activity.unread, original.activity.unread);
    assert!(restored.activity.active.is_empty());
    assert!(restored.conversations.is_empty());
    assert!(restored.threads.is_none());
    assert!(restored.models.is_empty());
    assert_eq!(restored.epoch, 0);
    assert!(!restored.connected);
    assert!(!original.conversations.is_empty());
}

proptest::proptest! {
    #[test]
    fn providers_with_the_same_native_id_keep_independent_history_and_drafts(
        native in "[A-Za-z0-9:_-]{1,80}",
        codex_text in "[^\\p{C}]{0,40}",
        claude_text in "[^\\p{C}]{0,40}",
        delta in "[^\\p{C}]{0,40}",
    ) {
        use agent_core::state::{Draft, Event, Snapshot, reduce};
        use agent_protocol::session::SessionUpdate;
        let codex = SessionRef::new(ProviderKind::Codex, native.clone()).unwrap();
        let claude = SessionRef::new(ProviderKind::Claude, native).unwrap();
        let codex_subscription = uuid::Uuid::new_v4();
        let claude_subscription = uuid::Uuid::new_v4();
        let make_thread = |session: &SessionRef, text: &str| Arc::new(Thread {
            id: Some(session.clone()),
            turns: Some(vec![Arc::new(Turn {
                id: "same-turn".into(),
                items: Some(vec![Arc::new(Item::new("same-item".into(), ItemStatus::Unknown, ItemBody::AssistantText {citation: None, text: text.into(), phase: AssistantPhase::Unknown }))]), ..Default::default()
            })]), ..Default::default()
        });
        let original = Snapshot {
            conversations: Arc::new([
                (codex.clone(), make_thread(&codex, &codex_text)),
                (claude.clone(), make_thread(&claude, &claude_text)),
            ].into()),
            subscriptions: Arc::new([(codex.clone(), codex_subscription), (claude.clone(), claude_subscription)].into()),
            drafts: Arc::new([
                (codex.clone().into(), Arc::new(Draft {text:codex_text.clone(), ..Default::default()})),
                (claude.clone().into(), Arc::new(Draft {text:claude_text.clone(), ..Default::default()})),
            ].into()),
            ..Default::default()
        };
        let (changed, _) = reduce(&original, Event::SessionUpdate(Box::new(SessionUpdate {
            subscription_id: claude_subscription,
            change: SessionChange::Text {
                turn_id:"same-turn".into(), item_id:"same-item".into(),
                field:TextField::AssistantText, delta:delta.clone(),
            },
        })));
        proptest::prop_assert!(Arc::ptr_eq(&original.conversations[&codex], &changed.conversations[&codex]));
        let expected_text = format!("{claude_text}{delta}");
        proptest::prop_assert_eq!(item_text(&(changed.conversations[&claude].turns.as_ref().unwrap()[0].items.as_ref().unwrap()[0])), Some(expected_text.as_str()));
        proptest::prop_assert_eq!(&changed.drafts, &original.drafts);
        let restored = agent_core::persistence::decode(&agent_core::persistence::encode(&changed).unwrap()).unwrap();
        proptest::prop_assert_eq!(restored.drafts.len(), 2);
        proptest::prop_assert_eq!(&restored.drafts, &original.drafts);
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

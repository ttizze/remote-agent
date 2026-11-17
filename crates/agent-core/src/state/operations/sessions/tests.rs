use super::*;
use crate::session::{ProviderKind, SessionRef};

#[test]
fn recalled_prompt_preserves_live_text_and_uses_the_sources_empty_string_boundary() {
    for (current, restored, expected) in [
        ("", "", ""),
        ("", "old", "old"),
        ("live", "", "live"),
        ("live ", "old", "live \n\nold"),
        (" ", "old", " \n\nold"),
        ("日本語", "🧪", "日本語\n\n🧪"),
    ] {
        assert_eq!(recalled_prompt(current, restored), expected);
    }
}

#[rstest::rstest]
fn history_projection_failure_keeps_the_page_retryable(
    #[values(None, Some("oldest"))] next_cursor: Option<&str>,
) {
    use agent_protocol::orchestration::{self as model, projection::ThreadHistoryPage};
    let id = SessionRef::new(None, "history-retry".into()).unwrap();
    let mut snapshot = Snapshot::default();
    open_thread(
        &mut snapshot,
        "",
        serde_json::from_value(serde_json::json!({
            "id":id,"historyCursor":"older","historyHasMore":true,
            "turns":[{"id":"turn","status":"completed","items":[{
                "id":"latest","body":{"inline":{"body":{"assistantText":{"text":"Latest answer","phase":"final"}}}}
            }]}]
        }))
        .unwrap(),
        None,
        None,
        None,
    );
    let request = || ReadHistory {
        session: id.clone(),
    };
    let input = request().capture(&snapshot, "").unwrap();
    let before = snapshot.clone();
    let mut row = snapshot.sessions.projections[&id]
        .projection
        .visible_turn_items[0]
        .clone();
    row.source_item_id = "older-question".into();
    let item = Arc::make_mut(&mut row.item);
    item.id = row.source_item_id.clone();
    item.body = model::TurnItemBody::UserMessage {
        message_id: "older-question".into(),
        created_by: model::Actor::User,
        creation_source: model::CreationSource::Mobile,
        scheduled_task_id: None,
        input_intent: "turn_start".into(),
        text: "Earlier question".into(),
        attachments: vec![serde_json::json!({"id":"attachment"})],
        context: None,
    };
    let mut page = ThreadHistoryPage {
        nodes: vec![],
        snapshot_sequence: 10,
        items: vec![row],
        next_cursor: next_cursor.map(str::to_owned),
        has_more_history: next_cursor.is_some(),
    };
    let error = request()
        .complete(&mut snapshot, "", (input.clone(), page.clone()), true)
        .unwrap_err();
    assert!(error.to_string().contains("attachment name is missing"));
    assert_eq!(
        snapshot, before,
        "A failed page must not change the cursor or displayed history"
    );
    assert_eq!(request().capture(&snapshot, "").unwrap(), input);
    let (_, effects) = super::super::super::reduce(
        &snapshot,
        "",
        Event::Intent(Box::new(Intent::ReadOlder {
            thread_id: id.clone(),
        })),
    );
    assert_eq!(
        effects.len(),
        1,
        "Load earlier must still request the failed page"
    );

    let model::TurnItemBody::UserMessage { attachments, .. } =
        &mut Arc::make_mut(&mut page.items[0].item).body
    else {
        unreachable!()
    };
    attachments[0]["name"] = serde_json::json!("notes.txt");
    request()
        .complete(&mut snapshot, "", (input, page), true)
        .unwrap();
    let thread = &snapshot.sessions.conversations[&id];
    assert_eq!(thread.history_cursor.as_deref(), next_cursor);
    assert_eq!(thread.history_has_more, Some(next_cursor.is_some()));
    let items = thread.turns.as_ref().unwrap()[0].items.as_ref().unwrap();
    assert_eq!(items.len(), 2);
    assert!(
        matches!(items[0].body(), crate::models::ItemBody::UserMessage { text: Some(text), .. } if text == "Earlier question")
    );
    assert_eq!(items[1].id.as_str(), "latest");
}

fn open_thread(
    snapshot: &mut Snapshot,
    pane: &str,
    thread: Thread,
    model: Option<crate::models::ModelRef>,
    effort: Option<String>,
    service_tier: Option<String>,
) -> Vec<Effect> {
    let id = thread.id.clone().unwrap();
    let output = crate::test_conversations::opened(
        crate::models::ThreadResponse {
            thread,
            model,
            effort,
            service_tier,
        },
        snapshot,
    );
    ReadThread::open(id).apply(snapshot, pane, output)
}

fn refresh_thread(snapshot: &mut Snapshot, pane: &str, thread: Thread) {
    let id = thread.id.clone().unwrap();
    let output = crate::test_conversations::opened(
        crate::models::ThreadResponse {
            thread,
            ..Default::default()
        },
        snapshot,
    );
    ReadThread::new(id).apply(snapshot, pane, output);
}

#[test]
fn item_reads_require_the_items_actual_turn() {
    let id = SessionRef::new(ProviderKind::Codex, "details".into()).unwrap();
    let mut snapshot = Snapshot::default();
    crate::test_conversations::open(
        &mut snapshot,
        crate::models::ThreadResponse {
            thread: Thread {
                id: Some(id.clone()),
                turns: Some(vec![Arc::new(crate::models::Turn {
                    id: "correct".into(),
                    items: Some(vec![Arc::new(Item::new(
                        "item".into(),
                        crate::models::ItemStatus::Completed,
                        crate::models::ItemBody::ImageView {
                            path: "image.png".into(),
                        },
                    ))]),
                    ..Default::default()
                })]),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let request = ReadItem {
        thread_id: id,
        turn_id: "wrong".into(),
        item_id: "item".into(),
    };
    assert!(
        matches!(request.capture(&snapshot,""),Err(PeerError::InvalidMessage(message)) if message=="item does not belong to the requested turn")
    );
    assert!(
        ReadItem {
            turn_id: "correct".into(),
            ..request
        }
        .capture(&snapshot, "")
        .is_ok()
    );
}

#[rstest::rstest]
fn missing_native_options_keep_saved_options_only_for_the_same_model(
    #[values(None, Some("saved"), Some("changed"))] native_model: Option<&str>,
) {
    let id = SessionRef::new(ProviderKind::Codex, "missing-options".into()).unwrap();
    let key = DraftKey::from(id.clone());
    let model = |name: &str| crate::models::ModelRef {
        provider: id.provider.unwrap(),
        id: name.into(),
    };
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        key.clone(),
        Arc::new(Draft {
            model: Some(model("saved")),
            effort: Some("high".into()),
            service_tier: Some("priority".into()),
            ..Default::default()
        }),
    );
    super::open_thread(
        &mut snapshot,
        "",
        id.clone(),
        String::new(),
        native_model.map(model),
        None,
        None,
    );
    assert_eq!(
        snapshot.input.drafts[&key].model.as_ref().unwrap().id,
        native_model.unwrap_or("saved")
    );
    let same_model = native_model != Some("changed");
    assert_eq!(
        snapshot.input.drafts[&key].effort.as_deref(),
        same_model.then_some("high")
    );
    assert_eq!(
        snapshot.input.drafts[&key].service_tier.as_deref(),
        same_model.then_some("priority")
    );
}

#[rstest::rstest]
fn reopening_preserves_only_unsubmitted_local_settings(
    #[values(false, true)] accepted: bool,
    #[values(false, true)] edited_during_send: bool,
) {
    let id = SessionRef::new(ProviderKind::Codex, "local-settings".into()).unwrap();
    let key = DraftKey::from(id.clone());
    let model = crate::models::ModelRef {
        provider: id.provider.unwrap(),
        id: "model".into(),
    };
    let mut snapshot = Snapshot::default();
    open_thread(
        &mut snapshot,
        "",
        Thread {
            id: Some(id.clone()),
            ..Default::default()
        },
        Some(model.clone()),
        Some("low".into()),
        None,
    );
    let (snapshot, _) = reduce_intent(
        &snapshot,
        "",
        Intent::SelectEffort {
            thread_id: key.clone(),
            effort: "high".into(),
        },
    );
    let (snapshot, _) = reduce_intent(
        &snapshot,
        "",
        Intent::SetDraftText {
            thread_id: key.clone(),
            text: "Continue".into(),
        },
    );
    let (mut snapshot, _) = reduce_intent(
        &snapshot,
        "",
        Intent::Submit {
            thread_id: Some(id.clone()),
            client_user_message_id: "input".into(),
            alternate: false,
        },
    );
    if edited_during_send {
        snapshot = reduce_intent(
            &snapshot,
            "",
            Intent::SelectEffort {
                thread_id: key.clone(),
                effort: "max".into(),
            },
        )
        .0;
    }
    if accepted {
        let pending = snapshot.sessions.pending_submissions["input"].clone();
        SendSubmission {
            thread_id: id.clone(),
            client_user_message_id: "input".into(),
            draft: snapshot.sessions.pending_submissions["input"].draft.clone(),
            follow_up: Default::default(),
        }
        .apply(&mut snapshot, "", 7);
        let mut response = crate::models::ThreadResponse {
            thread: Thread {
                id: Some(id.clone()),
                ..Default::default()
            },
            ..Default::default()
        };
        response
            .thread
            .queue
            .inputs
            .push(agent_protocol::operations::Submission {
                fresh_context: false,
                thread_id: id.clone(),
                client_user_message_id: "input".into(),
                input: vec![],
                model: pending.draft.model.clone(),
                effort: pending.draft.effort.clone(),
                service_tier: pending.draft.service_tier.clone(),
                follow_up: Default::default(),
            });
        let opened = crate::test_conversations::opened(response, &mut snapshot);
        super::super::conversations::install(&mut snapshot, opened);
    } else {
        snapshot = reduce(&snapshot, "", Event::SubmissionFailed("input".into())).0;
        assert_eq!(snapshot.input.drafts[&key].text, "Continue");
    }
    // Persistence must retain an unsent choice, including one made while
    // another submission was awaiting its acknowledgement.
    let mut snapshot =
        crate::persistence::decode(&crate::persistence::encode(&snapshot).unwrap()).unwrap();
    open_thread(
        &mut snapshot,
        "",
        Thread {
            id: Some(id),
            ..Default::default()
        },
        Some(model),
        Some("low".into()),
        None,
    );
    let expected = if edited_during_send {
        "max"
    } else if accepted {
        "low"
    } else {
        "high"
    };
    assert_eq!(
        snapshot.input.drafts[&key].effort.as_deref(),
        Some(expected)
    );
    assert_eq!(
        snapshot.input.drafts[&key].model_settings_changed,
        edited_during_send || !accepted
    );
}

#[rstest::rstest]
fn reopening_uses_latest_settings_despite_cached_defaults(
    #[values("before", "during", "after")] catalog_at: &str,
    #[values(false, true)] cached: bool,
    #[values(ProviderKind::Codex, ProviderKind::Claude, ProviderKind::DeepSeek)]
    provider: ProviderKind,
) {
    let id = SessionRef::new(provider, "reopened".into()).unwrap();
    let key = DraftKey::from(id.clone());
    let models = || {
        serde_json::from_value(serde_json::json!({"data":[
        {"id":"default","model":{"provider":provider,"id":"default"},"isDefault":true,"displayName":"Default"},
        {"id":"saved","model":{"provider":provider,"id":"saved"},"displayName":"Saved","settings":{
            "defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}],
            "serviceTiers":[{"id":"priority","fast":true}]
        }}
    ]})).unwrap()
    };
    let mut snapshot = Snapshot::default();
    if cached {
        refresh_thread(
            &mut snapshot,
            "",
            Thread {
                id: Some(id.clone()),
                ..Default::default()
            },
        );
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                text: "Unsent text".into(),
                model: Some(crate::models::ModelRef {
                    provider,
                    id: "default".into(),
                }),
                effort: Some("medium".into()),
                service_tier: Some("default".into()),
                ..Default::default()
            }),
        );
    }
    if catalog_at == "before" {
        LoadModels { provider }.apply(&mut snapshot, "", models());
    }
    let mut open = ReadThread::open(id.clone());
    open.prepare(&mut snapshot, "").unwrap();
    if catalog_at == "during" {
        LoadModels { provider }.apply(&mut snapshot, "", models());
    }
    let model = crate::models::ModelRef {
        provider,
        id: "saved".into(),
    };
    crate::test_conversations::open(
        &mut snapshot,
        crate::models::ThreadResponse {
            thread: Thread {
                id: Some(id.clone()),
                ..Default::default()
            },
            model: Some(model.clone()),
            effort: Some("high".into()),
            service_tier: Some("priority".into()),
        },
    );
    if catalog_at == "after" {
        LoadModels { provider }.apply(&mut snapshot, "", models());
    }
    let draft = &snapshot.input.drafts[&key];
    assert_eq!(draft.model.as_ref(), Some(&model));
    assert_eq!(draft.effort.as_deref(), Some("high"));
    assert_eq!(draft.service_tier.as_deref(), Some("priority"));
    assert_eq!(draft.text, if cached { "Unsent text" } else { "" });
    let controls = snapshot.model_quick_controls(key.clone());
    assert_eq!(controls.effort, "high");
    assert!(controls.fast);
    let (snapshot, _) = reduce_intent(
        &snapshot,
        "",
        Intent::SetDraftText {
            thread_id: key,
            text: "Continue".into(),
        },
    );
    let (submitted, effects) = reduce_intent(
        &snapshot,
        "",
        Intent::Submit {
            thread_id: Some(id),
            client_user_message_id: "next".into(),
            alternate: false,
        },
    );
    assert_eq!(effects.len(), 1);
    let next = &submitted.sessions.pending_submissions["next"].draft;
    assert_eq!(next.model.as_ref(), Some(&model));
    assert_eq!(next.effort.as_deref(), Some("high"));
    assert_eq!(next.service_tier.as_deref(), Some("priority"));
}

#[rstest::rstest]
fn reopening_preserves_the_saved_model_on_every_provider(
    #[values(ProviderKind::Codex, ProviderKind::Claude, ProviderKind::DeepSeek)]
    provider: ProviderKind,
) {
    let id = SessionRef {
        provider: Some(provider),
        id: "existing".into(),
    };
    let key = DraftKey::from(id.clone());
    let draft = Arc::new(Draft {
        text: "keep input".into(),
        model: Some(crate::models::ModelRef {
            provider,
            id: "local-choice".into(),
        }),
        effort: Some("high".into()),
        model_settings_changed: true,
        ..Default::default()
    });
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.input.drafts).insert(key.clone(), draft.clone());
    open_thread(
        &mut snapshot,
        "",
        Thread {
            id: Some(id.clone()),
            ..Default::default()
        },
        Some(crate::models::ModelRef {
            provider,
            id: "native-choice".into(),
        }),
        None,
        None,
    );
    assert_eq!(snapshot.input.drafts[&key], draft);
    assert_eq!(
        snapshot.panes.primary().navigation.thread_id.as_ref(),
        Some(&id)
    );
}

#[test]
fn navigating_back_keeps_an_unsent_provider_change() {
    let id = SessionRef {
        provider: Some(ProviderKind::Codex),
        id: "existing".into(),
    };
    let key = DraftKey::from(id.clone());
    let mut snapshot = Snapshot {
        catalog: crate::state::Catalog {
            models: Arc::new(serde_json::from_value(serde_json::json!([
                {"id":"gpt","model":{"provider":"codex","id":"gpt"},"displayName":"GPT","isDefault":true},
                {"id":"sonnet","model":{"provider":"claude","id":"sonnet"},"displayName":"Claude","isDefault":true}
            ])).unwrap()),
            ..Default::default()
        },
        ..Default::default()
    };
    let model = snapshot.catalog.models[1].model.clone();
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        key.clone(),
        Arc::new(Draft {
            model: Some(model.clone()),
            text: "continue with Claude".into(),
            ..Default::default()
        }),
    );
    select_thread(&mut snapshot, "", id.clone(), "/workspace".into());
    assert_eq!(snapshot.input.drafts[&key].model.as_ref(), Some(&model));
    assert_eq!(snapshot.input.drafts[&key].text, "continue with Claude");
    assert_eq!(
        snapshot.panes.primary().navigation.thread_id.as_ref(),
        Some(&id)
    );
}

#[test]
fn opening_an_old_project_task_uses_metadata_before_history_and_clears_cached_status_on_disconnect()
{
    let id = SessionRef::new(ProviderKind::Codex, "old".into()).unwrap();
    let page: crate::models::ThreadList = serde_json::from_value(serde_json::json!({
        "data":[{"id":id,"projectId":"p","cwd":"/old-project","status":"running","canAcceptDirectInput":false}],
        "projects":[{"id":"p","name":"P","roots":[]}],"hasMore":false,"hasMoreProjects":false,"limit":5,
    })).unwrap();
    let mut snapshot = Snapshot {
        connection: crate::state::Connection {
            connected: true,
            ..Default::default()
        },
        lists: crate::state::Lists {
            expanded_projects: Arc::new([("p".into(), 5)].into()),
            project_threads: Arc::new([("p".into(), Arc::new(page))].into()),
            ..Default::default()
        },
        ..Default::default()
    };
    ReadThread::open(id.clone())
        .prepare(&mut snapshot, "")
        .unwrap();
    assert_eq!(
        snapshot.panes.primary().navigation.thread_id.as_ref(),
        Some(&id)
    );
    assert_eq!(snapshot.panes.primary().navigation.cwd, "/old-project");
    assert_eq!(
        snapshot.selected_directory(snapshot.panes.primary()),
        "/old-project"
    );
    assert_eq!(
        snapshot
            .thread_metadata(&id)
            .unwrap()
            .can_accept_direct_input,
        Some(false)
    );
    let (disconnected, _) = reduce(&snapshot, "", Event::Disconnected);
    assert_eq!(
        disconnected.thread_metadata(&id).unwrap().status,
        crate::models::SessionStatus::Unknown
    );
    assert_eq!(
        snapshot.thread_metadata(&id).unwrap().status,
        crate::models::SessionStatus::Running
    );
}

#[rstest::rstest]
#[case::openrouter("openrouter")]
#[case::groq("groq")]
fn reopening_old_official_session_preserves_selected_gateway_for_next_submission(
    #[case] route: &str,
) {
    let official = crate::models::ModelRef {
        provider: ProviderKind::DeepSeek,
        id: r#"["deepseek-official","deepseek-flash"]"#.into(),
    };
    let selected = crate::models::ModelRef {
        provider: ProviderKind::DeepSeek,
        id: serde_json::to_string(&[route, "test/model"]).unwrap(),
    };
    let session = SessionRef::new(ProviderKind::DeepSeek, "selected-gateway".into()).unwrap();
    let key: DraftKey = session.clone().into();
    let mut snapshot = Snapshot {
        connection: crate::state::Connection {
            connected: true,
            ..Default::default()
        },
        catalog: crate::state::Catalog {
            models: Arc::new(serde_json::from_value(serde_json::json!([
            {"id":"official","model":official,"displayName":"Official","isDefault":true, "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}},
            {"id":"gateway","model":selected,"displayName":"Gateway", "settings": {"defaultReasoningEffort":"","supportedReasoningEfforts":[]}}
        ])).unwrap()),
            ..Default::default()
        },
        ..Default::default()
    };
    Arc::make_mut(&mut snapshot.input.drafts).insert(
        key.clone(),
        Arc::new(Draft {
            text: "Keep my unfinished request".into(),
            model: Some(selected.clone()),
            model_settings_changed: true,
            ..Default::default()
        }),
    );
    ReadThread::open(session.clone())
        .prepare(&mut snapshot, "")
        .unwrap();
    open_thread(
        &mut snapshot,
        "",
        Thread {
            id: Some(session.clone()),
            ..Default::default()
        },
        Some(official),
        None,
        None,
    );
    assert_eq!(snapshot.input.drafts[&key].model, Some(selected.clone()));
    assert_eq!(
        snapshot.input.drafts[&key].text,
        "Keep my unfinished request"
    );
    let (submitted, effects) = reduce_intent(
        &snapshot,
        "",
        Intent::Submit {
            thread_id: Some(session),
            client_user_message_id: "gateway-input".into(),
            alternate: false,
        },
    );
    assert_eq!(effects.len(), 1);
    assert_eq!(
        submitted.sessions.pending_submissions["gateway-input"]
            .draft
            .model,
        Some(selected)
    );
}

#[rstest::rstest]
#[case::new_draft(false, false)]
#[case::existing_empty_draft(true, false)]
#[case::saved_settings(true, true)]
fn opening_restores_model_metadata_or_uses_the_provider_catalog(
    #[values("before", "during", "after")] catalog_at: &str,
    #[values(true, false)] model_metadata: bool,
    #[values(ProviderKind::Codex, ProviderKind::Claude)] provider: ProviderKind,
    #[case] existing_draft: bool,
    #[case] saved_choice: bool,
) {
    let models: Vec<Model> = serde_json::from_value(serde_json::json!([
        {"id":"codex-default","model":{"provider":"codex","id":"default"},"displayName":"Codex default",
         "isDefault":true, "settings": {"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"}]}},
        {"id":"claude-default","model":{"provider":"claude","id":"default"},"displayName":"Claude default",
         "isDefault":true, "settings": {"defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"high"}]}},
        {"id":"codex-saved","model":{"provider":"codex","id":"saved"},"displayName":"Codex saved", "settings": {
         "defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"}]}},
        {"id":"claude-saved","model":{"provider":"claude","id":"saved"},"displayName":"Claude saved", "settings": {
         "defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}]}}
    ])).unwrap();
    let session = SessionRef::new(provider, "external".into()).unwrap();
    let key: DraftKey = session.clone().into();
    let mut snapshot = Snapshot::default();
    let catalog = || agent_protocol::operations::ModelPage {
        data: models.clone(),
        next_cursor: None,
        provider_errors: None,
    };
    if catalog_at == "before" {
        LoadModels { provider }.apply(&mut snapshot, "", catalog());
    }
    if existing_draft {
        Arc::make_mut(&mut snapshot.input.drafts).insert(
            key.clone(),
            Arc::new(Draft {
                text: "Unsent input".into(),
                model: saved_choice.then(|| crate::models::ModelRef {
                    provider,
                    id: "saved".into(),
                }),
                effort: saved_choice.then(|| "low".into()),
                model_settings_changed: saved_choice,
                ..Default::default()
            }),
        );
    }
    ReadThread::open(session.clone())
        .prepare(&mut snapshot, "")
        .unwrap();
    if catalog_at == "during" {
        LoadModels { provider }.apply(&mut snapshot, "", catalog());
    }
    open_thread(
        &mut snapshot,
        "",
        Thread {
            id: Some(session),
            ..Default::default()
        },
        Some(crate::models::ModelRef {
            provider,
            id: if model_metadata { "saved" } else { "default" }.into(),
        }),
        None,
        None,
    );
    if catalog_at == "after" {
        LoadModels { provider }.apply(&mut snapshot, "", catalog());
    }
    let draft = &snapshot.input.drafts[&key];
    assert_eq!(draft.text, if existing_draft { "Unsent input" } else { "" });
    assert_eq!(
        draft.model,
        Some(crate::models::ModelRef {
            provider,
            id: if saved_choice || model_metadata {
                "saved"
            } else {
                "default"
            }
            .into(),
        })
    );
    let effort = if saved_choice {
        "low"
    } else if provider == ProviderKind::Codex {
        "medium"
    } else {
        "high"
    };
    // The composer and the next submission must use the same settings.
    assert_eq!(draft.effort.as_deref(), Some(effort));
    assert_eq!(draft.service_tier.as_deref(), Some("default"));
    assert_eq!(snapshot.model_quick_controls(key.clone()).effort, effort);
    let expected_model = draft.model.clone();
    let (snapshot, _) = reduce_intent(
        &snapshot,
        "",
        Intent::SetDraftText {
            thread_id: key,
            text: "Next input".into(),
        },
    );
    let (submitted, effects) = reduce_intent(
        &snapshot,
        "",
        Intent::Submit {
            thread_id: None,
            client_user_message_id: "next-input".into(),
            alternate: false,
        },
    );
    assert_eq!(effects.len(), 1);
    assert_eq!(
        submitted.sessions.pending_submissions["next-input"]
            .draft
            .model,
        expected_model
    );
}

#[test]
fn a_paged_rewind_restores_only_the_selected_conversations_original_input() {
    use agent_protocol::orchestration::{
        TurnItemBody,
        projection::{ProjectedTurnItem, Visibility},
    };
    let id = SessionRef::new(ProviderKind::Codex, "paged-source".into()).unwrap();
    let mut state = Snapshot::default();
    let response=serde_json::from_value(serde_json::json!({"thread":{"id":id,"turns":[
        {"id":"old-run","status":"completed","items":[{"id":"old-input","status":"completed","body":{"inline":{"body":{"userMessage":{"text":"owned paged input","content":[]}}}}}]}
    ]}})).unwrap();
    let mut canonical = crate::test_conversations::opened(response, &mut state).projection;
    let projection = &mut canonical.projection;
    let own = projection
        .turn_items
        .iter()
        .find(|item| matches!(item.body, TurnItemBody::UserMessage { .. }))
        .unwrap()
        .as_ref()
        .clone();
    projection.turn_items.clear();
    let mut foreign = own.clone();
    foreign.thread_id = "another-conversation".into();
    if let TurnItemBody::UserMessage { text, .. } = &mut foreign.body {
        *text = "foreign input with colliding raw IDs".into();
    }
    projection.visible_turn_items.insert(
        0,
        ProjectedTurnItem {
            position: 0,
            visibility: Visibility::Inherited,
            source_thread_id: foreign.thread_id.clone(),
            source_item_id: foreign.id.clone(),
            item: Arc::new(foreign),
        },
    );
    Arc::make_mut(&mut state.sessions.projections).insert(id.clone(), Arc::new(canonical));
    assert_eq!(
        RewindSession {
            thread_id: id.clone(),
            turn_id: "old-run".into(),
            storage_scope: state.connection.storage_scope.clone()
        }
        .capture(&state, "")
        .unwrap()
        .original
        .unwrap()
        .text,
        "owned paged input"
    );
}

#[test]
fn a_confirmed_rewind_restores_duplicate_attachments_as_distinct_occurrences() {
    // T3 rewindToMessage adds files with allowDuplicates, including copies
    // already in the latest draft. Native success is covered by store tests;
    // this boundary checks the actual operation's draft update and removal.
    let session = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
    let file = Attachment {
        path: "/fixture/shared.png".into(),
        name: "shared.png".into(),
        is_image: true,
    };
    let mut state = Snapshot::default();
    Arc::make_mut(&mut state.input.drafts).insert(
        session.clone().into(),
        Arc::new(crate::state::Draft {
            attachments: vec![file.clone()],
            ..Default::default()
        }),
    );
    let capture = RewindCapture {
        draft_key: session.clone().into(),
        scope: state.connection.storage_scope.clone(),
        epoch: state.connection.epoch,
        original: Some(Recalled {
            text: String::new(),
            attachments: vec![file.clone(), file.clone()],
            contexts: Vec::new(),
            fresh_context: false,
        }),
    };
    RewindSession::new(
        session.clone(),
        "cut".into(),
        state.connection.storage_scope.clone(),
    )
    .complete(&mut state, "", (capture, 1), false)
    .unwrap();
    assert_eq!(
        state.input.drafts[&session.clone().into()].attachments,
        vec![file.clone(), file.clone(), file.clone()]
    );
    let (after, _) = crate::state::reduce(
        &state,
        "",
        crate::state::Event::from(crate::state::Intent::RemoveAttachment {
            draft_key: session.clone().into(),
            index: 1,
        }),
    );
    assert_eq!(
        after.input.drafts[&session.into()].attachments,
        vec![file.clone(), file]
    );
}

#[test]
fn rewind_never_recalls_an_omitted_or_stale_input_preview() {
    use agent_protocol::orchestration::TurnItemBody;
    let id = SessionRef::new(ProviderKind::Codex, "large-owned-input".into()).unwrap();
    let mut state = Snapshot::default();
    let text = "Complete user input😀".repeat(90_000);
    let response=serde_json::from_value(serde_json::json!({"thread":{"id":id,"turns":[
        {"id":"large-run","status":"completed","items":[{"id":"large-input","status":"completed","body":{"inline":{"body":{"userMessage":{"text":text,"content":[]}}}}}]}
    ]}})).unwrap();
    let mut canonical = crate::test_conversations::opened(response, &mut state).projection;
    let mut full = canonical
        .projection
        .turn_items
        .iter()
        .find(|item| matches!(item.body, TurnItemBody::UserMessage { .. }))
        .unwrap()
        .clone();
    let complete = Arc::make_mut(&mut full);
    complete.detail_omitted = false;
    if let TurnItemBody::UserMessage { text: body, .. } = &mut complete.body {
        *body = text.clone();
    }
    for item in canonical.projection.turn_items.iter_mut().chain(
        canonical
            .projection
            .visible_turn_items
            .iter_mut()
            .map(|row| &mut row.item),
    ) {
        if item.id == full.id {
            let item = Arc::make_mut(item);
            item.detail_omitted = true;
            if let TurnItemBody::UserMessage { text, .. } = &mut item.body {
                *text = "transport preview".into();
            }
        }
    }
    Arc::make_mut(&mut state.sessions.projections).insert(id.clone(), Arc::new(canonical));
    let operation = RewindSession {
        thread_id: id,
        turn_id: "large-run".into(),
        storage_scope: state.connection.storage_scope.clone(),
    };
    assert!(
        operation.capture(&state, "").is_err(),
        "native rewind cannot start while the original input is only a preview"
    );
    let key = (full.thread_id.clone(), full.id.clone());
    let mut stale = full.as_ref().clone();
    stale.updated_at = "2099-01-01T00:00:00.000Z".into();
    Arc::make_mut(&mut state.sessions.item_details).insert(key.clone(), Arc::new(stale));
    assert!(
        operation.capture(&state, "").is_err(),
        "a stale hydration cannot authorize recalling the preview"
    );
    Arc::make_mut(&mut state.sessions.item_details).insert(key, full);
    assert_eq!(
        operation
            .capture(&state, "")
            .unwrap()
            .original
            .unwrap()
            .text,
        text,
        "rewind must recall the complete original input"
    );
}

#[test]
fn deferred_system_notices_request_their_complete_body_without_refetching_complete_notices() {
    let session = SessionRef::new(None, "notice-owner".into()).unwrap();
    let turn = "turn".into();
    let mut item = Item::new(
        "notice".into(),
        crate::models::ItemStatus::Completed,
        crate::models::ItemBody::SystemNotice {
            message: "History omitted".into(),
        },
    );
    assert!(item_detail(&session, &turn, &item).is_none());
    item.defer();
    assert!(
        item_detail(&session, &turn, &item).is_some(),
        "an omitted notice body still has to be hydrated"
    );
}

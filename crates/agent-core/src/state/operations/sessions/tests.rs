use super::*;
use crate::session::{ProviderKind, SessionRef};

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

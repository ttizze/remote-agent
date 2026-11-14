use super::*;

fn context_record(id: &str) -> agent_protocol::context::MessageContext {
    agent_protocol::context::MessageContext {
        id: id.into(),
        label: "Build output".into(),
        content: agent_protocol::context::ContextContent::Terminal {
            terminal_id: "terminal".into(),
            terminal_label: "Build".into(),
            line_start: 1,
            line_end: 4,
            text: "build failed".into(),
        },
    }
}

#[test]
fn context_edits_keep_draft_settings_and_reject_old_host_updates() {
    let mut previous = Snapshot::default();
    previous.connection.storage_scope = "scope".into();
    let key = previous.panes.primary().navigation.draft_key.clone();
    let original = Draft {
        text: "question".into(),
        effort: Some("high".into()),
        ..Default::default()
    };
    Arc::make_mut(&mut previous.input.drafts).insert(key.clone(), Arc::new(original.clone()));
    let action = |scope: &str| {
        Event::Intent(Intent::ChangeContext {
            storage_scope: scope.into(),
            thread_id: key.clone(),
            action: crate::composer::context::ContextAction::Add {
                context: Box::new(context_record("output")),
            },
        })
    };
    let (stale, effects) = reduce(&previous, "", action("other"));
    assert_eq!(stale.input.drafts[&key].as_ref(), &original);
    assert!(effects.is_empty());
    let (next, effects) = reduce(&previous, "", action("scope"));
    assert_eq!(next.input.drafts[&key].text, original.text);
    assert_eq!(next.input.drafts[&key].effort, original.effort);
    assert_eq!(next.input.drafts[&key].contexts, [context_record("output")]);
    let stored = serde_json::to_vec(&next.input.drafts[&key]).unwrap();
    assert_eq!(
        serde_json::from_slice::<Draft>(&stored).unwrap(),
        *next.input.drafts[&key]
    );
    assert!(effects.is_empty());
}

#[test]
fn context_only_input_is_captured_and_failed_sends_restore_it_beside_new_input() {
    let mut previous = Snapshot::default();
    let key = previous.panes.primary().navigation.draft_key.clone();
    let draft = Arc::new(Draft {
        model: Some(crate::models::ModelRef {
            provider: crate::session::ProviderKind::Codex,
            id: "model".into(),
        }),
        contexts: vec![context_record("sent")],
        ..Default::default()
    });
    Arc::make_mut(&mut previous.input.drafts).insert(key.clone(), draft.clone());
    let (mut sending, effects) = submission(
        &previous,
        Destination {
            thread_id: None,
            cwd: Some("/repo".into()),
            draft_key: key.clone(),
        },
        draft.clone(),
        "input".into(),
        None,
        agent_protocol::operations::FollowUp::Queue,
    );
    assert_eq!(effects.len(), 1);
    assert!(sending.input.drafts[&key].contexts.is_empty());
    assert_eq!(sending.sessions.pending_submissions["input"].draft, draft);
    let current = Arc::make_mut(
        Arc::make_mut(&mut sending.input.drafts)
            .get_mut(&key)
            .unwrap(),
    );
    current.text = "new question".into();
    current.contexts.push(context_record("new"));
    let (restored, _) = reduce(&sending, "", Event::SubmissionFailed("input".into()));
    assert_eq!(restored.input.drafts[&key].text, "new question");
    assert_eq!(
        restored.input.drafts[&key].contexts,
        [context_record("new"), context_record("sent")]
    );
    assert_eq!(draft.contexts, [context_record("sent")]);
    let invalid = Arc::new(Draft {
        contexts: vec![context_record("")],
        ..draft.as_ref().clone()
    });
    let (rejected, effects) = submission(
        &previous,
        Destination {
            thread_id: None,
            cwd: Some("/repo".into()),
            draft_key: key.clone(),
        },
        invalid,
        "invalid".into(),
        None,
        agent_protocol::operations::FollowUp::Queue,
    );
    assert!(effects.is_empty());
    assert_eq!(rejected.input.drafts[&key], draft);
    assert!(
        !rejected
            .sessions
            .pending_submissions
            .contains_key("invalid")
    );
}

#[rstest::rstest]
#[case("", "音声🎤", "音声🎤")]
#[case("入力", "音声🎤", "入力\n音声🎤")]
#[case("入力\n", "音声🎤", "入力\n音声🎤")]
#[case("入力 ", "音声🎤", "入力 音声🎤")]
#[case("入力", "", "入力")]
fn transcript_appends_to_the_latest_target_without_changing_selection(
    #[case] existing: &str,
    #[case] spoken: &str,
    #[case] expected: &str,
) {
    let target = DraftKey::from("recorded");
    let selected = DraftKey::from("selected");
    let mut previous = Snapshot::default();
    set_draft_text(&mut previous, target.clone(), existing.into());
    set_draft_text(&mut previous, selected.clone(), "keep this draft".into());
    Arc::make_mut(&mut Arc::make_mut(previous.panes.primary_mut()).navigation).draft_key =
        selected.clone();
    let (next, effects) = reduce(
        &previous,
        "",
        Event::Intent(Intent::AppendTranscript {
            thread_id: target.clone(),
            text: spoken.into(),
        }),
    );
    assert_eq!(
        next.input
            .drafts
            .get(&target)
            .map(|draft| draft.text.as_str())
            .unwrap_or_default(),
        expected
    );
    assert_eq!(
        previous
            .input
            .drafts
            .get(&target)
            .map(|draft| draft.text.as_str())
            .unwrap_or_default(),
        existing
    );
    assert!(Arc::ptr_eq(
        &next.panes.primary().navigation,
        &previous.panes.primary().navigation
    ));
    assert!(Arc::ptr_eq(
        &next.input.drafts[&selected],
        &previous.input.drafts[&selected]
    ));
    assert!(effects.is_empty());
}

#[test]
fn transcript_uses_the_utf8_end_when_refreshing_completions() {
    let mut snapshot = Snapshot::default();
    snapshot.connection.connected = true;
    let key = DraftKey::from("draft");
    Arc::make_mut(snapshot.panes.primary_mut()).navigation = Arc::new(Navigation {
        draft_key: key.clone(),
        cwd: "/code".into(),
        ..Default::default()
    });
    Arc::make_mut(&mut snapshot.catalog.composer_catalogs).insert(
        "/code".into(),
        Arc::new(agent_protocol::composer::ComposerCatalog {
            cwd: "/code".into(),
            loading: false,
            candidates: Vec::new(),
            errors: Default::default(),
        }),
    );
    set_draft_text(&mut snapshot, key.clone(), "日本語".into());
    let (next, effects) = reduce(
        &snapshot,
        "",
        Event::Intent(Intent::AppendTranscript {
            thread_id: key.clone(),
            text: "/".into(),
        }),
    );
    assert_eq!(next.input.drafts[&key].text, "日本語\n/");
    assert!(
        next.catalog
            .composer_catalogs
            .get(&next.panes.primary().navigation.cwd)
            .unwrap()
            .loading
    );
    assert_eq!(effects.len(), 1);
}

#[test]
fn a_recovery_click_cannot_retry_a_replaced_operation() {
    let mut snapshot = Snapshot::default();
    Arc::make_mut(&mut snapshot.operations).insert(
        op::OperationKey::Models {
            provider: crate::session::ProviderKind::Codex,
        },
        op::OperationState {
            generation: 9,
            phase: op::OperationPhase::Failed {
                message: "offline".into(),
            },
        },
    );
    let recover = |generation| {
        Event::Intent(Intent::RecoverError {
            id: op::ErrorId::Operation {
                key: op::OperationKey::Models {
                    provider: crate::session::ProviderKind::Codex,
                },
                generation,
            },
        })
    };
    let (stale, effects) = reduce(&snapshot, "", recover(8));
    assert!(effects.is_empty());
    assert!(stale.operations.same(&snapshot.operations));
    let (_, effects) = reduce(&snapshot, "", recover(9));
    assert_eq!(effects.len(), 1);
}

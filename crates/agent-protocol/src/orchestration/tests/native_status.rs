use super::*;

fn external(status: ProviderThreadStatus) -> State {
    let mut state = initial();
    let mut native:ProviderThread=serde_json::from_value(serde_json::json!({
        "id":"native","driver":"codex","providerInstanceId":"codex","providerSessionId":null,
        "appThreadId":state.thread.id,"ownerNodeId":null,
        "nativeThreadRef":{"driver":"codex","nativeId":"external","strength":"strong"},
        "nativeConversationHeadRef":null,"status":"idle","firstRunOrdinal":null,"lastRunOrdinal":null,
        "handoffIds":[],"forkedFrom":null,"createdAt":NOW,"updatedAt":NOW
    })).unwrap();
    native.status = status;
    state.thread.active_provider_thread_id = Some(native.id.clone());
    state.provider_threads.insert(native.id.clone(), native);
    state
}

#[test]
fn only_observed_native_activity_blocks_new_input_while_unknown_activity_does_not_imply_an_uncertain_write()
 {
    for (status, queued) in [
        (ProviderThreadStatus::Active, true),
        (ProviderThreadStatus::Unknown, false),
        (ProviderThreadStatus::NotLoaded, false),
        (ProviderThreadStatus::Idle, false),
    ] {
        let state = external(status);
        let decision = admit(&state, input(1), Admission::Queue, context("admit", &state)).unwrap();
        let state = state.apply(&decision);
        let run = state.runs.values().next().unwrap();
        assert_eq!(
            run.status,
            if queued {
                RunStatus::Queued
            } else {
                RunStatus::Starting
            }
        );
        assert_eq!(decision.effects.len(), if queued { 0 } else { 1 });
        assert_eq!(
            state.provider_threads["native"]
                .native_thread_ref
                .as_ref()
                .unwrap()
                .native_id
                .as_deref(),
            Some("external")
        );
    }
}

#[test]
fn stale_native_identity_cannot_release_a_queued_input() {
    for (instance, driver, native) in [
        ("claude", "codex", "external"),
        ("codex", "claude", "external"),
        ("codex", "codex", "old-source"),
    ] {
        let state = external(ProviderThreadStatus::Active);
        let state = state.apply(
            &admit(
                &state,
                input(1),
                Admission::Queue,
                context("queued", &state),
            )
            .unwrap(),
        );
        assert_eq!(
            observe_native_status(
                &state,
                instance,
                driver,
                native,
                ProviderThreadStatus::Idle,
                context("foreign", &state)
            )
            .unwrap(),
            Decision::default()
        );
    }
}

#[test]
fn unchanged_native_status_does_not_publish_events_or_effects_and_idle_does_not_release_a_held_queue()
 {
    let state = external(ProviderThreadStatus::Active);
    assert_eq!(
        observe_native_status(
            &state,
            "codex",
            "codex",
            "external",
            ProviderThreadStatus::Active,
            context("same", &state)
        )
        .unwrap(),
        Decision::default()
    );
    let state = state.apply(
        &admit(
            &state,
            input(1),
            Admission::Queue,
            context("queued", &state),
        )
        .unwrap(),
    );
    let state = state.apply(&hold_queue(&state, true));
    let observed_at = "2026-11-14T00:00:02.000Z";
    let idle = observe_native_status(
        &state,
        "codex",
        "codex",
        "external",
        ProviderThreadStatus::Idle,
        Context {
            now: observed_at,
            ..context("idle", &state)
        },
    )
    .unwrap();
    assert_eq!(idle.changes.len(), 1);
    assert!(idle.effects.is_empty());
    let state = state.apply(&idle);
    assert_eq!(state.provider_threads["native"].updated_at, observed_at);
    let run = state.runs.values().next().unwrap();
    assert_eq!(run.status, RunStatus::Queued);
    assert!(run.queue_held);
    assert_eq!(
        observe_native_status(
            &state,
            "codex",
            "codex",
            "external",
            ProviderThreadStatus::Idle,
            context("same-idle", &state)
        )
        .unwrap(),
        Decision::default()
    );
}

#[test]
fn a_native_status_change_releases_only_the_first_of_multiple_queued_messages() {
    let state = external(ProviderThreadStatus::Active);
    let state =
        state.apply(&admit(&state, input(1), Admission::Queue, context("first", &state)).unwrap());
    let state = state.apply(
        &admit(
            &state,
            input(2),
            Admission::Queue,
            context("second", &state),
        )
        .unwrap(),
    );
    assert_eq!(
        observe_native_status(
            &state,
            "codex",
            "codex",
            "external",
            ProviderThreadStatus::Active,
            context("still-busy", &state)
        )
        .unwrap(),
        Decision::default()
    );
    let queued = state.next_queued_run().unwrap();
    assert!(activate_queued(&state, &queued.id, context("force-start", &state)).is_err());
    assert!(promote_queued(&state, &queued.id, context("send-now", &state)).is_err());
    for released in [
        ProviderThreadStatus::Idle,
        ProviderThreadStatus::Unknown,
        ProviderThreadStatus::NotLoaded,
    ] {
        let release = observe_native_status(
            &state,
            "codex",
            "codex",
            "external",
            released,
            context("idle", &state),
        )
        .unwrap();
        let state = state.apply(&release);
        assert_eq!(release.effects.len(), 1);
        assert_eq!(
            state.active_run().unwrap().user_message_id,
            input(1).message_id
        );
        assert_eq!(
            state.next_queued_run().unwrap().user_message_id,
            input(2).message_id
        );
        assert_eq!(
            observe_native_status(
                &state,
                "codex",
                "codex",
                "external",
                released,
                context("duplicate-idle", &state)
            )
            .unwrap(),
            Decision::default()
        );
    }
}

#[test]
fn an_inactive_native_source_does_not_resolve_a_transport_write_with_an_unknown_receipt() {
    let state = external(ProviderThreadStatus::Idle);
    let state =
        state.apply(&admit(&state, input(1), Admission::Queue, context("first", &state)).unwrap());
    let mut state = state.apply(
        &admit(
            &state,
            input(2),
            Admission::Queue,
            context("queued", &state),
        )
        .unwrap(),
    );
    let first = state.active_run().unwrap().clone();
    let effect=state.effects.values_mut().find(|effect|matches!(&effect.request,EffectRequest::ProviderTurnStart {run_id} if run_id==&first.id)).unwrap();
    // The transport owns this lease and cannot establish whether its write
    // was consumed. A separate failure can settle the app run, not that receipt.
    effect.status = EffectStatus::Running;
    let effect_id = effect.id.clone();
    let state =
        state.apply(&uncertain_delivery(&state, &effect_id, "acknowledgement lost", NOW).unwrap());
    let state = state.apply(
        &fail_start(
            &state,
            first.active_attempt_id.as_deref().unwrap(),
            ProviderFailure {
                class: "provider_error".into(),
                message: "native connection closed".into(),
                code: None,
                retryable: None,
                reset_at: None,
            },
            context("failed", &state),
        )
        .unwrap(),
    );
    let state = state.apply(&hold_queue(&state, false));
    assert!(state.active_run().is_none());
    assert!(state.has_uncertain_input());
    let queued = state.next_queued_run().unwrap();
    assert!(activate_queued(&state, &queued.id, context("manual-resume", &state)).is_err());
    for status in [ProviderThreadStatus::Idle, ProviderThreadStatus::Unknown] {
        let observation = observe_native_status(
            &state,
            "codex",
            "codex",
            "external",
            status,
            context("native-status", &state),
        )
        .unwrap();
        assert!(observation.effects.is_empty());
        assert_eq!(
            state.apply(&observation).runs[&queued.id].status,
            RunStatus::Queued
        );
    }
}

fn native_turn(id: &str) -> ProviderRef {
    ProviderRef {
        driver: "codex".into(),
        native_id: Some(id.into()),
        strength: NativeRefStrength::Strong,
        fingerprint: None,
        ordinal: None,
    }
}

#[test]
fn external_turns_have_real_runless_roots_and_cannot_reopen_or_settle_another_running_native_turn()
{
    let initial = external(ProviderThreadStatus::Idle);
    let observed_at = "2026-11-14T00:00:02.000Z";
    let finished_at = "2026-11-14T00:00:04.000Z";
    let first = observe_external_turn(
        &initial,
        "native",
        "external",
        native_turn("first"),
        ProviderTurnStatus::Running,
        observed_at,
    )
    .unwrap();
    assert!(first.effects.is_empty());
    let state = initial.apply(&first);
    assert_eq!(state.provider_threads["native"].updated_at, observed_at);
    assert!(state.runs.is_empty());
    assert!(state.run_attempts.is_empty());
    assert!(state.effects.is_empty());
    let turn = state.provider_turns.values().next().unwrap();
    assert!(turn.run_attempt_id.is_none());
    let root = &state.nodes[&turn.node_id];
    assert!(root.run_id.is_none());
    assert!(!root.counts_for_run);
    assert_eq!(
        observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Running,
            NOW
        )
        .unwrap(),
        Decision::default()
    );
    let state = state.apply(
        &observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("second"),
            ProviderTurnStatus::Running,
            NOW,
        )
        .unwrap(),
    );
    let state = state.apply(
        &observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Completed,
            finished_at,
        )
        .unwrap(),
    );
    assert_eq!(
        state
            .provider_turns
            .values()
            .find(
                |turn| turn.native_turn_ref.as_ref().unwrap().native_id.as_deref() == Some("first")
            )
            .unwrap()
            .completed_at
            .as_deref(),
        Some(finished_at)
    );
    assert_eq!(
        state.provider_threads["native"].status,
        ProviderThreadStatus::Active
    );
    assert_eq!(
        observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Running,
            NOW
        )
        .unwrap(),
        Decision::default()
    );
    assert_eq!(
        observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Completed,
            NOW
        )
        .unwrap(),
        Decision::default()
    );
    let state = state.apply(
        &observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("second"),
            ProviderTurnStatus::Failed,
            NOW,
        )
        .unwrap(),
    );
    assert_eq!(
        state.provider_threads["native"].status,
        ProviderThreadStatus::Idle
    );
    assert_eq!(
        state
            .provider_turns
            .values()
            .filter(|turn| turn.status == ProviderTurnStatus::Completed)
            .count(),
        1
    );
    assert_eq!(
        state
            .nodes
            .values()
            .filter(|node| node.status == NodeStatus::Failed)
            .count(),
        1
    );
}

#[test]
fn external_observations_cannot_adopt_an_application_write_or_an_uncertain_delivery_receipt() {
    let (mut active, run, _) = running();
    for status in [RunStatus::Running, RunStatus::Waiting] {
        active.runs.get_mut(&run).unwrap().status = status;
        assert_eq!(
            observe_external_turn(
                &active,
                "native-thread",
                "native",
                native("unexpected-report", None),
                ProviderTurnStatus::Running,
                NOW,
            )
            .unwrap(),
            Decision::default(),
            "an app response waiting for permission still owns its native write"
        );
    }
    let initial = external(ProviderThreadStatus::Idle);
    assert_eq!(
        observe_external_turn(
            &initial,
            "foreign",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Running,
            NOW
        )
        .unwrap(),
        Decision::default()
    );
    let mut wrong = native_turn("first");
    wrong.driver = "claudeAgent".into();
    assert!(
        observe_external_turn(
            &initial,
            "native",
            "external",
            wrong,
            ProviderTurnStatus::Running,
            NOW
        )
        .is_err()
    );
    assert!(
        observe_external_turn(
            &initial,
            "native",
            "external",
            native_turn(""),
            ProviderTurnStatus::Running,
            NOW
        )
        .is_err()
    );
    let mut state = initial.apply(
        &admit(
            &initial,
            input(1),
            Admission::Queue,
            context("app-write", &initial),
        )
        .unwrap(),
    );
    assert_eq!(
        observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Running,
            NOW
        )
        .unwrap(),
        Decision::default()
    );
    let effect = state.effects.values_mut().next().unwrap();
    effect.status = EffectStatus::Running;
    effect.last_error = Some("receipt missing".into());
    let attempt = state
        .active_run()
        .unwrap()
        .active_attempt_id
        .clone()
        .unwrap();
    let failure = ProviderFailure {
        class: "provider".into(),
        message: "unknown delivery".into(),
        code: None,
        retryable: None,
        reset_at: None,
    };
    let state = state
        .apply(&fail_start(&state, &attempt, failure, context("start-error", &state)).unwrap());
    assert!(state.active_run().is_none());
    assert!(state.has_uncertain_input());
    assert_eq!(
        observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Running,
            NOW
        )
        .unwrap(),
        Decision::default()
    );
}

#[test]
fn external_item_identity_is_owned_by_its_actual_native_turn_and_provider_selection() {
    let state = external(ProviderThreadStatus::Idle);
    let mut state = state.apply(
        &observe_external_turn(
            &state,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Running,
            NOW,
        )
        .unwrap(),
    );
    let turn = state.provider_turns.values().next().unwrap().id.clone();
    let root = state.nodes[&state.provider_turns[&turn].node_id].clone();
    state.nodes.insert(
        "native-message-node".into(),
        ExecutionNode {
            id: "native-message-node".into(),
            parent_node_id: Some(root.id.clone()),
            kind: NodeKind::AssistantMessage,
            ..root
        },
    );
    let item:TurnItem=serde_json::from_value(serde_json::json!({
        "id":"native-item","threadId":state.thread.id,"runId":null,"nodeId":"native-message-node","providerThreadId":"native","providerTurnId":turn,
        "nativeItemRef":null,"parentItemId":null,"ordinal":0,"status":"running","title":null,"startedAt":NOW,"completedAt":null,"updatedAt":NOW,
        "type":"assistant_message","messageId":"message","text":"actual native output","attachments":[],"streaming":true
    })).unwrap();
    for part in 0..4 {
        let mut wrong = item.clone();
        match part {
            0 => wrong.thread_id = "foreign".into(),
            1 => wrong.run_id = Some("invented-app-run".into()),
            2 => wrong.provider_thread_id = Some("foreign".into()),
            _ => wrong.provider_turn_id = Some("foreign".into()),
        }
        assert!(observe_external_item(&state, &turn, "external", wrong).is_err());
    }
    let saved =
        state.apply(&observe_external_item(&state, &turn, "external", item.clone()).unwrap());
    for status in [
        TurnItemStatus::Idle,
        TurnItemStatus::Pending,
        TurnItemStatus::Waiting,
    ] {
        let mut waiting = item.clone();
        waiting.status = status;
        let waiting =
            saved.apply(&observe_external_item(&saved, &turn, "external", waiting).unwrap());
        assert!(
            !observe_external_item(&waiting, &turn, "external", item.clone())
                .unwrap()
                .changes
                .is_empty(),
            "{status:?} is resumable, not terminal"
        );
    }
    for status in [
        TurnItemStatus::Completed,
        TurnItemStatus::Failed,
        TurnItemStatus::Cancelled,
        TurnItemStatus::Interrupted,
    ] {
        let mut terminal = item.clone();
        terminal.status = status;
        let terminal =
            saved.apply(&observe_external_item(&saved, &turn, "external", terminal).unwrap());
        assert_eq!(
            observe_external_item(&terminal, &turn, "external", item.clone()).unwrap(),
            Decision::default(),
            "{status:?} cannot be reopened"
        );
    }
    let mut update = item.clone();
    if let TurnItemBody::AssistantMessage { text, .. } = &mut update.body {
        *text = "new native text".into();
    }
    assert!(
        !observe_external_item(&saved, &turn, "external", update)
            .unwrap()
            .changes
            .is_empty()
    );
    let finished_at = "2026-11-14T00:00:04.000Z";
    let finished = saved.apply(
        &observe_external_turn(
            &saved,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Completed,
            finished_at,
        )
        .unwrap(),
    );
    assert_eq!(
        finished.turn_items["native-item"].status,
        TurnItemStatus::Completed
    );
    assert!(
        matches!(&finished.turn_items["native-item"].body,TurnItemBody::AssistantMessage {text,streaming:false,..} if text=="actual native output")
    );
    assert_eq!(
        finished.nodes["native-message-node"].status,
        NodeStatus::Completed
    );
    assert_eq!(
        finished.nodes["native-message-node"]
            .completed_at
            .as_deref(),
        Some(finished_at)
    );
    assert_eq!(
        observe_external_item(&finished, &turn, "external", item.clone()).unwrap(),
        Decision::default()
    );
    let completed_item = finished.turn_items["native-item"].clone();
    assert!(
        !observe_external_item(&saved, &turn, "external", completed_item)
            .unwrap()
            .changes
            .is_empty()
    );
    let mut replaced = saved.clone();
    replaced
        .provider_threads
        .get_mut("native")
        .unwrap()
        .native_thread_ref
        .as_mut()
        .unwrap()
        .native_id = Some("replacement".into());
    assert_eq!(
        observe_external_item(&replaced, &turn, "external", item.clone()).unwrap(),
        Decision::default()
    );
    assert_eq!(
        observe_external_turn(
            &replaced,
            "native",
            "external",
            native_turn("first"),
            ProviderTurnStatus::Completed,
            finished_at
        )
        .unwrap(),
        Decision::default()
    );
    let unknown = saved.apply(
        &observe_native_status(
            &saved,
            "codex",
            "codex",
            "external",
            ProviderThreadStatus::Unknown,
            context("unknown", &saved),
        )
        .unwrap(),
    );
    let mut app = unknown.apply(
        &admit(
            &unknown,
            input(1),
            Admission::Queue,
            context("app-after-native", &unknown),
        )
        .unwrap(),
    );
    assert!(app.active_run().is_some());
    assert_eq!(
        observe_external_item(&app, &turn, "external", item.clone()).unwrap(),
        Decision::default()
    );
    let effect = app.effects.values_mut().next().unwrap();
    effect.status = EffectStatus::Running;
    effect.last_error = Some("receipt missing".into());
    let attempt = app.active_run().unwrap().active_attempt_id.clone().unwrap();
    let failure = ProviderFailure {
        class: "provider".into(),
        message: "unknown delivery".into(),
        code: None,
        retryable: None,
        reset_at: None,
    };
    let uncertain =
        app.apply(&fail_start(&app, &attempt, failure, context("failed", &app)).unwrap());
    assert!(uncertain.active_run().is_none());
    assert!(uncertain.has_uncertain_input());
    assert_eq!(
        observe_external_item(&uncertain, &turn, "external", item.clone()).unwrap(),
        Decision::default()
    );
    let second = saved.apply(
        &observe_external_turn(
            &saved,
            "native",
            "external",
            native_turn("second"),
            ProviderTurnStatus::Running,
            NOW,
        )
        .unwrap(),
    );
    let other = second
        .provider_turns
        .values()
        .find(|saved| saved.id != turn)
        .unwrap()
        .id
        .clone();
    let mut reused = item.clone();
    reused.provider_turn_id = Some(other.clone());
    assert!(observe_external_item(&second, &other, "external", reused).is_err());
    let mut deleted = saved.clone();
    deleted.thread.deleted_at = Some(NOW.into());
    assert_eq!(
        observe_external_item(&deleted, &turn, "external", item.clone()).unwrap(),
        Decision::default()
    );
    let mut moved = saved.clone();
    moved.thread.active_provider_thread_id = None;
    assert_eq!(
        observe_external_item(&moved, &turn, "external", item).unwrap(),
        Decision::default()
    );
}

#[test]
fn external_requests_cannot_adopt_an_application_or_a_replaced_native_source_or_reopen_a_resolved_card()
 {
    let (owned, _, _) = approval();
    let mut request = owned.runtime_requests["request"].clone();
    request.provider_turn_id = None;
    request.created_at = "2026-11-15T00:00:12.000Z".into();
    let mut item = owned.request_item("request").unwrap().clone();
    item.run_id = None;
    item.provider_thread_id = Some("native".into());
    item.provider_turn_id = None;
    let state = external(ProviderThreadStatus::Idle);
    let observe = |state: &State, native: &str, request: RuntimeRequest, item: TurnItem| {
        observe_external_request(state, "native", native, "session", request, item)
    };
    let decision = observe(&state, "external", request.clone(), item.clone()).unwrap();
    assert!(decision.effects.is_empty());
    let saved = state.apply(&decision);
    assert_eq!(saved.runtime_requests["request"], request);
    assert_eq!(
        saved.provider_threads["native"].updated_at,
        "2026-11-15T00:00:12.000Z"
    );
    assert_eq!(
        saved.provider_threads["native"]
            .provider_session_id
            .as_deref(),
        Some("session")
    );

    assert_eq!(saved.turn_items[&item.id], item);
    assert!(saved.runs.is_empty());
    assert!(saved.run_attempts.is_empty());
    assert!(saved.provider_turns.is_empty());
    let repeated = observe(&saved, "external", request.clone(), item.clone()).unwrap();
    assert!(repeated.effects.is_empty());
    assert_eq!(saved.apply(&repeated), saved);
    for field in 0..3 {
        let mut collision = saved.clone();
        let previous = collision.turn_items.get_mut(&item.id).unwrap();
        match field {
            0 => previous.run_id = Some("another-application-run".into()),
            1 => previous.provider_turn_id = Some("another-native-turn".into()),
            _ => previous.provider_thread_id = Some("another-provider-thread".into()),
        }
        assert!(observe(&collision, "external", request.clone(), item.clone()).is_err());
    }
    assert_eq!(
        observe(&state, "old-native-source", request.clone(), item.clone()).unwrap(),
        Decision::default()
    );
    let mut deleted = state.clone();
    deleted.thread.deleted_at = Some(NOW.into());
    assert_eq!(
        observe(&deleted, "external", request.clone(), item.clone()).unwrap(),
        Decision::default()
    );
    let mut active = state.clone();
    active.runs = owned.runs.clone();
    assert!(active.active_run().is_some());
    assert_eq!(
        observe(&active, "external", request.clone(), item.clone()).unwrap(),
        Decision::default()
    );
    let mut uncertain = state.clone();
    uncertain.effects = owned.effects.clone();
    let effect = uncertain
        .effects
        .values_mut()
        .find(|effect| matches!(effect.request, EffectRequest::ProviderTurnStart { .. }))
        .unwrap();
    effect.status = EffectStatus::Running;
    effect.last_error = Some("native receipt lost".into());
    assert!(uncertain.has_uncertain_input());
    assert_eq!(
        observe(&uncertain, "external", request.clone(), item.clone()).unwrap(),
        Decision::default()
    );
    let mut resolved = saved.clone();
    resolved.runtime_requests.get_mut("request").unwrap().status = RequestStatus::Resolved;
    assert_eq!(
        observe(&resolved, "external", request.clone(), item.clone()).unwrap(),
        Decision::default()
    );
    for field in 0..4 {
        let mut wrong = item.clone();
        match field {
            0 => wrong.thread_id = "foreign".into(),
            1 => wrong.run_id = Some("invented-run".into()),
            2 => wrong.provider_thread_id = Some("foreign".into()),
            _ => wrong.node_id = Some("foreign-node".into()),
        }
        assert!(observe(&state, "external", request.clone(), wrong).is_err());
    }
    let mut wrong_session = request.clone();
    wrong_session.response_capability = ResponseCapability::Live {
        provider_session_id: "another-session".into(),
        supports_feedback: None,
    };
    assert!(observe(&state, "external", wrong_session, item.clone()).is_err());
    let mut taken = state.clone();
    let original = owned.request_item("request").unwrap().clone();
    taken.turn_items.insert(original.id.clone(), original);
    assert!(
        observe(&taken, "external", request, item).is_err(),
        "a session-scoped card cannot overwrite an application's item"
    );
}

#[test]
fn a_native_turn_expires_only_its_pending_requests_while_session_and_other_turn_requests_remain_live()
 {
    let (owned, _, _) = approval();
    for status in [
        ProviderTurnStatus::Completed,
        ProviderTurnStatus::Failed,
        ProviderTurnStatus::Interrupted,
        ProviderTurnStatus::Cancelled,
    ] {
        let mut state = external(ProviderThreadStatus::Idle);
        for id in ["ending", "other"] {
            state = state.apply(
                &observe_external_turn(
                    &state,
                    "native",
                    "external",
                    native_turn(id),
                    ProviderTurnStatus::Running,
                    NOW,
                )
                .unwrap(),
            );
        }
        for id in ["ending", "other", "session"] {
            let turn_id = state
                .provider_turns
                .values()
                .find(|turn| {
                    turn.native_turn_ref.as_ref().unwrap().native_id.as_deref() == Some(id)
                })
                .map(|turn| turn.id.clone());
            let mut request = owned.runtime_requests["request"].clone();
            request.id = id.into();
            request.node_id = format!("request-node-{id}");
            request.provider_turn_id = turn_id.clone();
            let mut item = owned.request_item("request").unwrap().clone();
            item.id = format!("request-item-{id}");
            item.run_id = None;
            item.node_id = Some(request.node_id.clone());
            item.provider_thread_id = Some("native".into());
            item.provider_turn_id = turn_id;
            if let TurnItemBody::ApprovalRequest { request_id, .. } = &mut item.body {
                *request_id = id.into();
            }
            let mut node = owned.nodes["approval-item"].clone();
            node.id = request.node_id.clone();
            node.run_id = None;
            node.provider_thread_id = item.provider_thread_id.clone();
            node.provider_turn_id = item.provider_turn_id.clone();
            node.runtime_request_id = Some(id.into());
            state = state.apply(
                &observe_external_request(&state, "native", "external", "session", request, item)
                    .unwrap(),
            );
            state.nodes.insert(node.id.clone(), node);
        }
        let mut image = state.turn_items["request-item-ending"].clone();
        image.id = "background-image".into();
        image.node_id = Some("background-image-node".into());
        image.status = TurnItemStatus::Running;
        image.body = TurnItemBody::DynamicTool {
            tool_name: Some("image_gen.imagegen".into()),
            input: serde_json::json!({"prompt":"Generate an image"}),
            output: None,
            output_omitted: None,
            viewed_image_path: None,
        };
        let mut image_node = state.nodes["request-node-ending"].clone();
        image_node.id = "background-image-node".into();
        image_node.status = NodeStatus::Running;
        image_node.kind = NodeKind::ToolCall;
        image_node.runtime_request_id = None;
        state.nodes.insert(image_node.id.clone(), image_node);
        state.turn_items.insert(image.id.clone(), image);
        // A previously resolved request on this same turn is never overwritten.
        let mut resolved = state.runtime_requests["ending"].clone();
        resolved.id = "already-resolved".into();
        resolved.status = RequestStatus::Resolved;
        resolved.resolved_at = Some(NOW.into());
        state
            .runtime_requests
            .insert(resolved.id.clone(), resolved.clone());
        let completed_at = "2026-11-14T00:00:08.000Z";
        let next = state.apply(
            &observe_external_turn(
                &state,
                "native",
                "external",
                native_turn("ending"),
                status,
                completed_at,
            )
            .unwrap(),
        );
        assert_eq!(
            next.runtime_requests["ending"].status,
            if status == ProviderTurnStatus::Interrupted {
                RequestStatus::Cancelled
            } else {
                RequestStatus::Expired
            }
        );
        assert_eq!(
            next.runtime_requests["ending"].resolved_at.as_deref(),
            Some(completed_at)
        );
        assert_eq!(
            next.runtime_requests["session"],
            state.runtime_requests["session"]
        );
        assert_eq!(
            next.runtime_requests["other"],
            state.runtime_requests["other"]
        );
        assert_eq!(next.runtime_requests["already-resolved"], resolved);
        let (item_status, node_status) = match status {
            ProviderTurnStatus::Failed => (TurnItemStatus::Failed, NodeStatus::Failed),
            ProviderTurnStatus::Interrupted => {
                (TurnItemStatus::Interrupted, NodeStatus::Interrupted)
            }
            ProviderTurnStatus::Cancelled => (TurnItemStatus::Cancelled, NodeStatus::Cancelled),
            _ => (TurnItemStatus::Completed, NodeStatus::Completed),
        };
        assert_eq!(next.turn_items["request-item-ending"].status, item_status);
        assert_eq!(next.nodes["request-node-ending"].status, node_status);
        assert_eq!(
            next.turn_items["request-item-ending"]
                .completed_at
                .as_deref(),
            Some(completed_at)
        );
        assert_eq!(
            next.nodes["request-node-ending"].completed_at.as_deref(),
            Some(completed_at)
        );
        for other in ["session", "other"] {
            let item = format!("request-item-{other}");
            let node = format!("request-node-{other}");
            assert_eq!(next.turn_items[&item], state.turn_items[&item]);
            assert_eq!(next.nodes[&node], state.nodes[&node]);
        }
        assert_eq!(
            next.turn_items["background-image"].status,
            if status == ProviderTurnStatus::Completed {
                TurnItemStatus::Running
            } else {
                item_status
            }
        );
        assert_eq!(
            next.nodes["background-image-node"].status,
            if status == ProviderTurnStatus::Completed {
                NodeStatus::Running
            } else {
                node_status
            }
        );

        assert!(next.runs.is_empty());
        assert!(next.run_attempts.is_empty());
        assert!(next.effects.is_empty());
    }
}

fn native_request_card(body: TurnItemBody, kind: &str) -> (State, RuntimeRequest, TurnItem) {
    let (owned, _, _) = approval();
    let mut state = external(ProviderThreadStatus::Idle);
    let mut request = owned.runtime_requests["request"].clone();
    request.kind = kind.into();
    request.provider_turn_id = None;
    let mut item = owned.request_item("request").unwrap().clone();
    item.run_id = None;
    item.provider_thread_id = Some("native".into());
    item.provider_turn_id = None;
    item.body = body;
    let mut node = owned.nodes["approval-item"].clone();
    node.run_id = None;
    node.parent_node_id = None;
    node.root_node_id = node.id.clone();
    node.provider_thread_id = item.provider_thread_id.clone();
    node.provider_turn_id = None;
    node.kind = match item.body {
        TurnItemBody::UserInputRequest { .. } => NodeKind::UserInputRequest,
        TurnItemBody::ApprovalRequest { .. } => NodeKind::ApprovalRequest,
        _ => NodeKind::ToolCall,
    };
    state.nodes.insert(node.id.clone(), node);
    (state, request, item)
}

fn question_card() -> TurnItemBody {
    TurnItemBody::UserInputRequest {
        request_id: "request".into(),
        questions: vec![UserInputQuestion {
            id: "choose".into(),
            header: "Choice".into(),
            question: "Choose now".into(),
            is_secret: None,
            options: vec![],
            multi_select: Some(false),
            allow_custom_answer: Some(true),
            initial_answer: None,
            required: Some(true),
        }],
    }
}

#[test]
fn native_request_cards_require_the_matching_kind_prompt_identity_and_pending_disposition() {
    let (owned, _, _) = approval();
    let approval_body = owned.request_item("request").unwrap().body.clone();
    let tool = TurnItemBody::DynamicTool {
        tool_name: Some("test".into()),
        input: serde_json::json!({}),
        output: None,
        output_omitted: None,
        viewed_image_path: None,
    };
    for (body, kind) in [
        (question_card(), "user_input"),
        (approval_body, "command"),
        (tool.clone(), "dynamic_tool_call"),
        (tool, "mcp-elicitation"),
    ] {
        let (state, request, item) = native_request_card(body, kind);
        let observed = observe_external_request(
            &state,
            "native",
            "external",
            "session",
            request.clone(),
            item.clone(),
        )
        .unwrap();
        let saved = state.apply(&observed);
        assert_eq!(saved.runtime_requests["request"], request);
        assert_eq!(saved.turn_items[&item.id], item);
        for status in [
            RequestStatus::Resolved,
            RequestStatus::Expired,
            RequestStatus::Cancelled,
        ] {
            let mut terminal = request.clone();
            terminal.status = status;
            assert!(
                observe_external_request(
                    &state,
                    "native",
                    "external",
                    "session",
                    terminal,
                    item.clone()
                )
                .is_err()
            );
        }
        let mut wrong_kind = request.clone();
        wrong_kind.kind = "unrelated-request-kind".into();
        assert!(
            observe_external_request(
                &state,
                "native",
                "external",
                "session",
                wrong_kind,
                item.clone()
            )
            .is_err()
        );
        if let TurnItemBody::UserInputRequest { request_id, .. }
        | TurnItemBody::ApprovalRequest { request_id, .. } = &item.body
        {
            assert_eq!(request_id, "request");
            let mut wrong = item.clone();
            match &mut wrong.body {
                TurnItemBody::UserInputRequest { request_id, .. }
                | TurnItemBody::ApprovalRequest { request_id, .. } => {
                    *request_id = "another-request".into()
                }
                _ => unreachable!(),
            }
            assert!(
                observe_external_request(&state, "native", "external", "session", request, wrong)
                    .is_err()
            );
        }
    }
}

#[test]
fn losing_a_native_question_callback_keeps_the_prompt_and_requires_an_explicit_message_answer() {
    let (state, request, item) = native_request_card(question_card(), "user_input");
    let state = state.apply(
        &observe_external_request(
            &state,
            "native",
            "external",
            "session",
            request.clone(),
            item.clone(),
        )
        .unwrap(),
    );
    let closed = request_source_closed(&state, &request.id, NOW).unwrap();
    assert!(closed.effects.is_empty());
    let unavailable = state.apply(&closed);
    assert_eq!(
        unavailable.runtime_requests["request"].response_capability,
        ResponseCapability::Message
    );
    assert_eq!(
        unavailable.runtime_requests["request"].status,
        RequestStatus::Pending
    );
    assert_eq!(unavailable.turn_items[&item.id], item);
    assert_eq!(
        unavailable.nodes["approval-item"].status,
        NodeStatus::Waiting
    );
    assert!(unavailable.runs.is_empty());
    let skipped = RequestResponse::UserInput {
        answers: BTreeMap::new(),
    };
    assert!(
        respond(
            &unavailable,
            &request.id,
            skipped,
            "codex",
            context("skip", &unavailable)
        )
        .is_err()
    );
    let response = RequestResponse::UserInput {
        answers: BTreeMap::from([("choose".into(), serde_json::json!("The explicit answer"))]),
    };
    let answered = respond(
        &unavailable,
        &request.id,
        response,
        "codex",
        context("answer", &unavailable),
    )
    .unwrap();
    assert_eq!(answered.effects.len(), 1);
    let next = unavailable.apply(&answered);
    assert_eq!(next.runs.len(), 1);
    assert_eq!(
        next.runtime_requests["request"].status,
        RequestStatus::Resolved
    );
    assert!(next.turn_items.values().any(|item|matches!(&item.body,TurnItemBody::UserMessage {text,..} if text=="Choose now\nThe explicit answer")));
}

#[test]
fn an_attempt_retains_its_bound_native_thread_when_the_selected_reference_changes() {
    let (mut state, run, attempt) = running();
    assert_eq!(
        state.run_attempts[&attempt]
            .native_thread_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_deref()),
        Some("native")
    );
    let provider = state.run_attempts[&attempt].provider_thread_id.clone();
    state
        .provider_threads
        .get_mut(&provider)
        .unwrap()
        .native_thread_ref = Some(native_turn("replacement"));
    assert!(state.native_thread_matches(&attempt, "codex", "native"));
    assert!(!state.native_thread_matches(&attempt, "codex", "replacement"));
    assert!(!state.native_thread_matches(&attempt, "claudeAgent", "native"));
    assert!(!state.native_thread_matches("another-attempt", "codex", "native"));
    let point = state.context_source_point(&state.runs[&run]);
    assert_eq!(
        point
            .provider_thread_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_deref()),
        Some("native")
    );
    assert_eq!(
        point.provider_thread_ref.as_ref().unwrap().strength,
        NativeRefStrength::Strong
    );

    state
        .run_attempts
        .get_mut(&attempt)
        .unwrap()
        .native_thread_ref = None;
    assert!(!state.native_thread_matches(&attempt, "codex", "native"));
    assert!(!state.native_thread_matches(&attempt, "codex", "replacement"));
}

#[test]
fn rebinding_a_native_resource_keeps_turn_identity_and_late_items_in_their_physical_scope() {
    let initial = external(ProviderThreadStatus::Idle);
    let mut state = initial.apply(
        &observe_external_turn(
            &initial,
            "native",
            "external",
            native_turn("reused-turn"),
            ProviderTurnStatus::Running,
            NOW,
        )
        .unwrap(),
    );
    let old = state.provider_turns.values().next().unwrap().clone();
    let thread = state.provider_threads.get_mut("native").unwrap();
    thread.native_thread_ref.as_mut().unwrap().native_id = Some("replacement".into());
    thread.status = ProviderThreadStatus::Idle;
    let state = state.apply(
        &observe_external_turn(
            &state,
            "native",
            "replacement",
            native_turn("reused-turn"),
            ProviderTurnStatus::Running,
            NOW,
        )
        .unwrap(),
    );
    assert_eq!(
        state.provider_turns.len(),
        2,
        "a copied native turn ID is scoped to its physical conversation"
    );
    assert_eq!(
        state.provider_turns[&old.id], old,
        "rebinding cannot rewrite the old turn"
    );
    let item:TurnItem=serde_json::from_value(serde_json::json!({
        "id":"late-item","threadId":state.thread.id,"runId":null,"nodeId":old.node_id,"providerThreadId":"native","providerTurnId":old.id,
        "nativeItemRef":null,"parentItemId":null,"ordinal":0,"status":"running","title":null,"startedAt":NOW,"completedAt":null,"updatedAt":NOW,
        "type":"assistant_message","messageId":"message","text":"output from the replaced source","attachments":[],"streaming":true
    })).unwrap();
    assert_eq!(
        observe_external_item(&state, &old.id, "replacement", item).unwrap(),
        Decision::default()
    );
    let finished = state.apply(
        &observe_external_turn(
            &state,
            "native",
            "replacement",
            native_turn("reused-turn"),
            ProviderTurnStatus::Completed,
            NOW,
        )
        .unwrap(),
    );
    assert_eq!(
        finished.provider_threads["native"].status,
        ProviderThreadStatus::Idle,
        "old resource activity cannot keep the replacement busy"
    );
    assert_eq!(finished.provider_turns[&old.id], old);
    assert!(
        finished.runs.is_empty() && finished.run_attempts.is_empty() && finished.effects.is_empty()
    );
}

#[test]
fn unloading_a_physical_resource_clears_its_aliases_without_detaching_other_resources_or_conversations()
 {
    let mut state = initial();
    state.attached_provider_sessions.insert("session".into());
    let original = state.provider_threads["native-thread"].clone();
    let native = original.native_thread_ref.clone().unwrap();
    let mut alias = original.clone();
    alias.id = "alias".into();
    state.provider_threads.insert(alias.id.clone(), alias);
    let mut other = original.clone();
    other.id = "other".into();
    other.native_thread_ref.as_mut().unwrap().native_id = Some("other-physical-resource".into());
    state
        .provider_threads
        .insert(other.id.clone(), other.clone());
    let mut foreign = original.clone();
    foreign.id = "foreign".into();
    foreign.app_thread_id = Some("another-conversation".into());
    state
        .provider_threads
        .insert(foreign.id.clone(), foreign.clone());
    assert_eq!(
        native_resource_unloaded(&state, "claude", &native, NOW),
        Decision::default()
    );
    let released = state.apply(&native_resource_unloaded(&state, "codex", &native, NOW));
    for id in ["native-thread", "alias"] {
        assert_eq!(
            released.provider_threads[id].status,
            ProviderThreadStatus::NotLoaded
        );
        assert!(released.provider_threads[id].provider_session_id.is_none());
        assert_eq!(
            released.provider_threads[id].native_thread_ref,
            Some(native.clone())
        );
    }
    assert_eq!(released.provider_threads["other"], other);
    assert_eq!(released.provider_threads["foreign"], foreign);
    assert!(
        released.attached_provider_sessions.contains("session"),
        "another resource in this conversation still uses the shared session"
    );
    assert_eq!(released.runs, state.runs);
    assert_eq!(released.run_attempts, state.run_attempts);
    assert_eq!(
        native_resource_unloaded(&released, "codex", &native, NOW),
        Decision::default()
    );
    let finished = released.apply(&native_resource_unloaded(
        &released,
        "codex",
        other.native_thread_ref.as_ref().unwrap(),
        NOW,
    ));
    assert!(!finished.attached_provider_sessions.contains("session"));
    assert_eq!(finished.provider_threads["foreign"], foreign);
    assert_eq!(
        finished.provider_sessions, state.provider_sessions,
        "detaching this conversation never stops a session shared with another conversation"
    );
    let stopped = provider_stopped(
        &finished,
        "codex",
        &ProviderFailure {
            class: "provider_error".into(),
            message: "process stopped".into(),
            code: None,
            retryable: None,
            reset_at: None,
        },
        context("stopped", &finished),
    )
    .unwrap();
    assert!(!stopped.changes.iter().any(|change|matches!(change,Change::ProviderThread(thread) if thread.id=="native-thread" || thread.id=="alias" || thread.id=="other")),"an unloaded historical resource never becomes a new failure after process loss");
}

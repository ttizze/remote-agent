use super::*;
use uuid::Uuid;

mod native_status;
mod recovery;

const NOW: &str = "2026-11-14T00:00:00.000Z";

#[test]
fn a_native_start_can_fail_before_session_attachment_and_cannot_overwrite_cancellation() {
    let state = initial();
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
    let run = state.active_run().unwrap();
    let attempt = run.active_attempt_id.as_deref().unwrap();
    let failure = ProviderFailure {
        class: "provider_error".into(),
        message: "provider could not start".into(),
        code: None,
        retryable: Some(true),
        reset_at: None,
    };
    let failed = state
        .apply(&fail_start(&state, attempt, failure.clone(), context("failure", &state)).unwrap());
    assert_eq!(failed.runs[&run.id].status, RunStatus::Failed);
    assert_eq!(
        failed.run_attempts[attempt].status,
        RunAttemptStatus::Failed
    );
    assert!(
        failed
            .runs
            .values()
            .filter(|run| run.status == RunStatus::Queued)
            .all(|run| run.queue_held)
    );
    assert!(failed.turn_items.values().any(|item|matches!(&item.body,TurnItemBody::Error {failure,..} if failure.message=="provider could not start")));
    assert!(
        fail_start(&failed, attempt, failure.clone(), context("again", &failed))
            .unwrap()
            .changes
            .is_empty()
    );
    let stopped = state.apply(&interrupt(&state, &run.id, context("stop", &state)).unwrap());
    assert!(
        fail_start(&stopped, attempt, failure, context("late", &stopped))
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn unconfirmed_native_writes_block_new_input_until_the_effect_is_resolved() {
    let state = initial();
    let accepted = admit(
        &state,
        input(1),
        Admission::Queue,
        context("original", &state),
    )
    .unwrap();
    let mut state = state.apply(&accepted);
    let template = state.effects.values().next().unwrap().clone();
    for request in [
        EffectRequest::ProviderTurnStart {
            run_id: "run".into(),
        },
        EffectRequest::ProviderTurnSteer {
            provider_session_id: "session".into(),
            provider_thread_id: "native-thread".into(),
            provider_turn_id: "turn".into(),
            message_id: "message".into(),
            model_selection: None,
        },
        EffectRequest::ProviderTurnRestart {
            provider_session_id: "session".into(),
            provider_thread_id: "native-thread".into(),
            provider_turn_id: "turn".into(),
            interrupted_attempt_id: "attempt".into(),
            run_id: "run".into(),
        },
    ] {
        state.effects.clear();
        state.effects.insert(
            template.id.clone(),
            Effect {
                request,
                status: EffectStatus::Running,
                last_error: Some("write acknowledgement lost".into()),
                lease_owner: None,
                ..template.clone()
            },
        );
        assert!(
            admit(&state, input(2), Admission::Queue, context("next", &state))
                .unwrap_err()
                .contains("uncertain input")
        );
    }
    state.effects.values_mut().next().unwrap().status = EffectStatus::Succeeded;
    assert!(admit(&state, input(2), Admission::Queue, context("next", &state)).is_ok());
}

fn context<'a>(seed: &'a str, state: &State) -> Context<'a> {
    Context {
        command_id: seed,
        now: NOW,
        can_steer: true,
        next_run_ordinal: state
            .runs
            .values()
            .map(|run| run.ordinal)
            .max()
            .unwrap_or(0)
            + 1,
    }
}

fn model() -> ModelSelection {
    ModelSelection {
        instance_id: "codex".into(),
        model: "example".into(),
        options: Vec::new(),
    }
}

fn initial() -> State {
    let thread:Thread=serde_json::from_value(serde_json::json!({
        "id":"thread","createdBy":"user","creationSource":"mobile","projectId":null,
        "title":"Example","providerInstanceId":"codex","modelSelection":model(),
        "runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":"/project",
        "activeProviderThreadId":"native-thread","lineage":{"parentThreadId":null,"relationshipToParent":null,"rootThreadId":"thread","sideChat":null},
        "forkedFrom":null,"createdAt":NOW,"updatedAt":NOW,"archivedAt":null,"settledOverride":null,
        "settledAt":null,"lastVisitedAt":null,"deletedAt":null,"rollbackRequestId":null,"rollbackFailure":null
    })).unwrap();
    let state = State::new(thread);
    state.apply(&Decision {
        changes: vec![
            Change::ProviderSessionAttached(ProviderSession {
                id: "session".into(),
                driver: "codex".into(),
                provider_instance_id: "codex".into(),
                status: ProviderSessionStatus::Ready,
                cwd: "/project".into(),
                model: None,
                capabilities: Default::default(),
                created_at: NOW.into(),
                updated_at: NOW.into(),
                last_error: None,
            }),
            Change::ProviderThread(ProviderThread {
                id: "native-thread".into(),
                driver: "codex".into(),
                provider_instance_id: "codex".into(),
                provider_session_id: Some("session".into()),
                app_thread_id: Some("thread".into()),
                owner_node_id: None,
                native_thread_ref: Some(native("native", None)),
                native_conversation_head_ref: None,
                native_metadata: None,
                status: ProviderThreadStatus::Idle,
                first_run_ordinal: None,
                last_run_ordinal: None,
                handoff_ids: Vec::new(),
                forked_from: None,
                pending_background_tasks: Vec::new(),
                created_at: NOW.into(),
                updated_at: NOW.into(),
            }),
        ],
        ..Default::default()
    })
}

fn native(id: &str, ordinal: Option<u64>) -> ProviderRef {
    ProviderRef {
        driver: "codex".into(),
        native_id: Some(id.into()),
        strength: NativeRefStrength::Strong,
        fingerprint: None,
        ordinal,
    }
}

fn input(seed: u128) -> Input {
    Input {
        message_id: Uuid::from_u128(seed).to_string(),
        created_by: Actor::User,
        creation_source: CreationSource::Mobile,
        text: format!("input {seed}"),
        attachments: Vec::new(),
        context: None,
        model_selection: model(),
        driver: "codex".into(),
        workspace_preparation: None,
        fork_origin: None,
    }
}

fn accepted(state: &State, seed: u128, admission: Admission) -> State {
    state.apply(
        &admit(
            state,
            input(seed),
            admission,
            context(&seed.to_string(), state),
        )
        .unwrap(),
    )
}

fn running() -> (State, String, String) {
    let state = accepted(&initial(), 1, Admission::Queue);
    let run = state.runs.values().next().unwrap().id.clone();
    let attempt = state.runs[&run].active_attempt_id.clone().unwrap();
    let state = state.apply(
        &started(
            &state,
            &attempt,
            "session",
            native("turn-1", None),
            context("3", &state),
        )
        .unwrap(),
    );
    (state, run, attempt)
}

#[test]
fn completed_foreground_keeps_known_background_work_and_process_loss_fences_its_late_results() {
    let (mut state, run, attempt) = running();
    let root = state.nodes[&state.runs[&run].root_node_id.clone().unwrap()].clone();
    for id in ["server", "foreground"] {
        let node = ExecutionNode {
            id: id.into(),
            parent_node_id: Some(root.id.clone()),
            kind: NodeKind::ToolCall,
            counts_for_run: false,
            ..root.clone()
        };
        let item = TurnItem {
            id: id.into(),
            thread_id: state.thread.id.clone(),
            run_id: Some(run.clone()),
            node_id: Some(id.into()),
            provider_thread_id: Some("native-thread".into()),
            provider_turn_id: state.run_attempts[&attempt].provider_turn_id.clone(),
            native_item_ref: Some(native(id, None)),
            parent_item_id: None,
            ordinal: 10,
            status: TurnItemStatus::Running,
            title: None,
            started_at: Some(NOW.into()),
            completed_at: None,
            updated_at: NOW.into(),
            body: TurnItemBody::CommandExecution {
                input: id.into(),
                output: Some("started".into()),
                exit_code: None,
                output_omitted: None,
                output_indicates_failure: None,
            },
        };
        state.nodes.insert(node.id.clone(), node);
        state.turn_items.insert(item.id.clone(), item);
    }
    state.provider_threads.get_mut("native-thread").unwrap().pending_background_tasks = vec![serde_json::from_value(serde_json::json!({"kind":"command","taskId":"server","itemId":"server","nativeItemId":"server"})).unwrap()];
    let done = state.apply(
        &complete(
            &state,
            &attempt,
            "session",
            Completion::Completed,
            context("done", &state),
        )
        .unwrap(),
    );
    assert_eq!(done.runs[&run].status, RunStatus::Completed);
    assert_eq!(done.turn_items["server"].status, TurnItemStatus::Running);
    assert_eq!(done.nodes["server"].status, NodeStatus::Running);
    assert_eq!(
        done.turn_items["foreground"].status,
        TurnItemStatus::Completed
    );
    assert_eq!(done.nodes["foreground"].status, NodeStatus::Completed);
    let mut late = done.turn_items["server"].clone();
    late.status = TurnItemStatus::Completed;
    late.completed_at = Some(NOW.into());
    let settled = done.apply(&observe_item(&done, &attempt, "session", late.clone()).unwrap());
    assert!(
        settled.provider_threads["native-thread"]
            .pending_background_tasks
            .is_empty()
    );
    // A roster can arrive before its item. Until it records a canonical item
    // ID, its native reference must match exactly, never a different task.
    let mut native_only = done.clone();
    native_only
        .provider_threads
        .get_mut("native-thread")
        .unwrap()
        .pending_background_tasks[0]
        .item_id = None;
    assert!(
        observe_item(&native_only, &attempt, "session", late.clone())
            .unwrap()
            .changes
            .iter()
            .any(|change| matches!(change,Change::TurnItem(item) if item.id=="server"))
    );
    let mut foreign = late.clone();
    foreign.id = "foreign-task".into();
    foreign.native_item_ref = Some(native("foreign-task", None));
    assert!(
        observe_item(&native_only, &attempt, "session", foreign)
            .unwrap()
            .changes
            .is_empty()
    );
    late.id = "unknown".into();
    assert!(
        observe_item(&done, &attempt, "session", late.clone())
            .unwrap()
            .changes
            .is_empty()
    );
    let mut child = late.clone();
    child.status = TurnItemStatus::Running;
    child.native_item_ref = Some(native("unknown-child", None));
    child.body = TurnItemBody::Subagent {
        subagent_id: "unknown-child".into(),
        origin: "native".into(),
        driver: "codex".into(),
        provider_instance_id: "codex".into(),
        child_thread_id: None,
        prompt: "late child".into(),
        result: None,
        progress: None,
    };
    assert!(
        observe_item(&done, &attempt, "session", child)
            .unwrap()
            .changes
            .is_empty()
    );
    let mut restarted =
        done.apply(&runtime_restarted(&done, false, context("restart", &done)).unwrap());
    assert!(
        restarted.provider_threads["native-thread"]
            .pending_background_tasks
            .is_empty()
    );
    assert_eq!(
        restarted.turn_items["server"].status,
        TurnItemStatus::Interrupted
    );
    restarted
        .provider_sessions
        .get_mut("session")
        .unwrap()
        .status = ProviderSessionStatus::Ready;
    let mut old = late.clone();
    old.id = "server".into();
    assert!(
        observe_item(&restarted, &attempt, "session", old)
            .unwrap()
            .changes
            .is_empty()
    );
    let failure = ProviderFailure {
        class: "provider_error".into(),
        message: "process stopped".into(),
        code: None,
        retryable: None,
        reset_at: None,
    };
    let mut stopped =
        done.apply(&provider_stopped(&done, "codex", &failure, context("stop", &done)).unwrap());
    assert!(
        stopped.provider_threads["native-thread"]
            .pending_background_tasks
            .is_empty()
    );
    assert_eq!(
        stopped.turn_items["server"].status,
        TurnItemStatus::Interrupted
    );
    assert_eq!(stopped.nodes["server"].status, NodeStatus::Interrupted);
    // The provider's process identifier can be reused after restart. A cleared
    // roster must still prevent an old completed attempt from reviving work.
    stopped.provider_sessions.get_mut("session").unwrap().status = ProviderSessionStatus::Ready;
    late.id = "server".into();
    assert!(
        observe_item(&stopped, &attempt, "session", late)
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn pending_generated_images_outlive_foreground_but_not_their_native_process() {
    for restart in [false, true] {
        let (mut state, run, attempt) = running();
        let mut image = state.turn_items.values().next().unwrap().clone();
        image.id = "imagegen".into();
        image.run_id = Some(run.clone());
        image.node_id = None;
        image.provider_thread_id = Some("native-thread".into());
        image.provider_turn_id = state.run_attempts[&attempt].provider_turn_id.clone();
        image.native_item_ref = Some(native("imagegen", None));
        image.status = TurnItemStatus::Running;
        image.body = TurnItemBody::DynamicTool {
            tool_name: Some("image_gen.imagegen".into()),
            input: serde_json::json!({}),
            output: None,
            output_omitted: None,
            viewed_image_path: None,
        };
        state = state.apply(&observe_item(&state, &attempt, "session", image.clone()).unwrap());
        let done = state.apply(
            &complete(
                &state,
                &attempt,
                "session",
                Completion::Completed,
                context("done", &state),
            )
            .unwrap(),
        );
        assert_eq!(done.runs[&run].status, RunStatus::Completed);
        assert_eq!(done.turn_items["imagegen"].status, TurnItemStatus::Running);
        let mut final_image = image.clone();
        final_image.status = TurnItemStatus::Completed;
        if let TurnItemBody::DynamicTool { output, .. } = &mut final_image.body {
            *output = Some(serde_json::json!({"status":"completed","savedPath":"/image.png"}));
        }
        assert!(
            observe_item(&done, &attempt, "session", final_image.clone())
                .unwrap()
                .changes
                .iter()
                .any(|change| matches!(change,Change::TurnItem(item) if item.id=="imagegen"))
        );
        let failure = ProviderFailure {
            class: "provider_error".into(),
            message: "process stopped".into(),
            code: None,
            retryable: None,
            reset_at: None,
        };
        let decision = if restart {
            runtime_restarted(&done, false, context("restart", &done)).unwrap()
        } else {
            provider_stopped(&done, "codex", &failure, context("stop", &done)).unwrap()
        };
        let mut lost = done.apply(&decision);
        assert_eq!(
            lost.turn_items["imagegen"].status,
            TurnItemStatus::Interrupted
        );
        lost.provider_sessions.get_mut("session").unwrap().status = ProviderSessionStatus::Ready;
        assert!(
            observe_item(&lost, &attempt, "session", final_image)
                .unwrap()
                .changes
                .is_empty()
        );
    }
}

#[test]
fn io_owner_cancels_a_claimed_start_only_after_stop_and_late_native_evidence_cannot_revive_it() {
    let mut state = accepted(&initial(), 1, Admission::Queue);
    let effect = state.effects.values_mut().next().unwrap();
    effect.status = EffectStatus::Running;
    effect.lease_owner = Some("worker".into());
    let effect_id = effect.id.clone();
    let run = state.active_run().unwrap().clone();
    let attempt = run.active_attempt_id.as_deref().unwrap();
    assert!(
        cancel_start(&state, attempt, context("before-stop", &state))
            .unwrap()
            .changes
            .is_empty()
    );
    state = state.apply(&interrupt(&state, &run.id, context("stop", &state)).unwrap());
    assert_eq!(state.runs[&run.id].status, RunStatus::Starting);
    let cancelled =
        state.apply(&cancel_start(&state, attempt, context("before-write", &state)).unwrap());
    assert_eq!(cancelled.runs[&run.id].status, RunStatus::Cancelled);
    assert_eq!(
        cancelled.run_attempts[attempt].status,
        RunAttemptStatus::Cancelled
    );
    assert_eq!(
        cancelled.effects[&effect_id].status,
        EffectStatus::Cancelled
    );
    assert_eq!(cancelled.effects[&effect_id].lease_owner, None);
    assert!(
        started(
            &cancelled,
            attempt,
            "session",
            native("late", None),
            context("late", &cancelled)
        )
        .unwrap()
        .changes
        .is_empty()
    );
}

#[test]
fn explicit_cancellation_fences_unknown_delivery_and_releases_the_lane_without_resending() {
    let mut state = accepted(&initial(), 1, Admission::Queue);
    let effect = state.effects.values_mut().next().unwrap();
    effect.status = EffectStatus::Running;
    effect.lease_owner = Some("worker".into());
    let effect_id = effect.id.clone();
    let run = state.active_run().unwrap().clone();
    assert!(cancel_uncertain(&state, &run.user_message_id, context("too-early", &state)).is_err());
    let uncertain = state.apply(&uncertain_delivery(&state, &effect_id, "ack lost", NOW).unwrap());
    assert!(uncertain.has_uncertain_input());
    let error=uncertain.turn_items.values().find(|item|matches!(&item.body,TurnItemBody::Error {failure,..} if failure.class=="delivery_uncertain")).unwrap();
    assert_eq!(
        error.parent_item_id.as_deref(),
        Some(user_item_id(&state.thread.id, &run.user_message_id).as_str())
    );
    let cancelled = uncertain.apply(
        &cancel_uncertain(
            &uncertain,
            &run.user_message_id,
            context("cancel", &uncertain),
        )
        .unwrap(),
    );
    assert!(!cancelled.has_uncertain_input());
    assert_eq!(cancelled.runs[&run.id].status, RunStatus::Cancelled);
    assert_eq!(
        cancelled.effects[&effect_id].status,
        EffectStatus::Cancelled
    );
    assert!(!cancelled.attached_provider_sessions.contains("session"));
    assert!(cancelled.effects.values().any(|effect| matches!(
        effect.request,
        EffectRequest::ProviderSessionDetach { .. }
    ) && effect.status == EffectStatus::Pending));
    assert_eq!(
        cancelled
            .effects
            .values()
            .filter(|effect| matches!(effect.request, EffectRequest::ProviderTurnStart { .. }))
            .count(),
        1
    );
    assert!(
        started(
            &cancelled,
            run.active_attempt_id.as_deref().unwrap(),
            "session",
            native("late", None),
            context("late", &cancelled)
        )
        .unwrap()
        .changes
        .is_empty()
    );
}

#[test]
fn input_is_a_run_at_acceptance_and_concurrent_input_queues_before_native_start() {
    let state = accepted(&initial(), 1, Admission::Queue);
    let second = accepted(&state, 2, Admission::Queue);
    assert_eq!(second.runs.len(), 2);
    let first = second.runs.values().find(|run| run.ordinal == 1).unwrap();
    let queued = second.next_queued_run().unwrap();
    assert_eq!(first.status, RunStatus::Starting);
    for run in second.runs.values() {
        assert!(run.root_node_id.is_some());
        assert!(run.active_attempt_id.is_some());
        assert!(run.provider_thread_id.is_some());
    }
    assert_eq!(second.run_attempts.len(), 2);
    assert_eq!((queued.ordinal, queued.status), (2, RunStatus::Queued));
    assert_eq!(second.turn_items.len(), 2);
    assert_eq!(second.nodes.len(), 2);
    assert_eq!(second.effects.len(), 1);
    assert!(
        second.turn_items.values().all(
            |item| matches!(&item.body,TurnItemBody::UserMessage {text,..} if !text.is_empty())
        )
    );
}

#[test]
fn shared_provider_sessions_do_not_allow_another_native_thread_to_own_a_callback() {
    let (state, _, attempt) = running();
    assert!(state.native_thread_matches(&attempt, "codex", "native"));
    assert!(!state.native_thread_matches(&attempt, "codex", "another-native"));
    assert!(!state.native_thread_matches(&attempt, "claudeAgent", "native"));
    assert!(!state.native_thread_matches("another-attempt", "codex", "native"));
}

#[test]
fn provider_owned_subagents_are_read_only_while_application_owned_subagents_accept_input() {
    let mut state = initial();
    state.thread.lineage.relationship_to_parent = Some("subagent".into());
    state.thread.creation_source = CreationSource::Provider;
    assert!(admit(&state, input(1), Admission::Queue, context("1", &state)).is_err());
    state.thread.creation_source = CreationSource::Mobile;
    assert!(admit(&state, input(1), Admission::Queue, context("1", &state)).is_ok());
}

#[test]
fn steer_appends_a_message_to_the_existing_run_without_allocating_a_run() {
    let (state, run, attempt) = running();
    let next = accepted(&state, 4, Admission::Steer);
    assert_eq!(next.runs.len(), 1);
    let item=next.turn_items.values().find(|item|matches!(&item.body,TurnItemBody::UserMessage {input_intent,..} if input_intent=="steer")).unwrap();
    assert_eq!(item.run_id.as_deref(), Some(run.as_str()));
    assert_eq!(
        item.provider_turn_id,
        next.run_attempts[&attempt].provider_turn_id
    );
    assert!(
        next.effects
            .values()
            .any(|effect| matches!(effect.request, EffectRequest::ProviderTurnSteer { .. }))
    );
}

#[test]
fn native_evidence_before_an_unknown_ack_releases_the_start_lane() {
    let (mut state, run, attempt) = running();
    let effect = state.effects.values_mut().find(|effect|matches!(&effect.request,EffectRequest::ProviderTurnStart {run_id} if run_id==&run)).unwrap();
    effect.status = EffectStatus::Running;
    effect.lease_owner = Some("worker".into());
    let effect_id = effect.id.clone();
    let next = state.apply(&uncertain_delivery(&state, &effect_id, "ack lost", NOW).unwrap());
    assert!(!next.has_uncertain_input());
    assert_eq!(next.effects[&effect_id].status, EffectStatus::Succeeded);
    assert_eq!(next.runs[&run].status, RunStatus::Running);
    assert!(
        cancel_uncertain(
            &next,
            &next.runs[&run].user_message_id,
            context("cancel", &next)
        )
        .is_err()
    );

    // A repeated native start is also proof for a previously unowned write.
    state.effects.get_mut(&effect_id).unwrap().lease_owner = None;
    state.effects.get_mut(&effect_id).unwrap().last_error = Some("lost".into());
    let turn = state.provider_turns[state.run_attempts[&attempt]
        .provider_turn_id
        .as_ref()
        .unwrap()]
    .native_turn_ref
    .clone()
    .unwrap();
    let observed = state.apply(
        &started(
            &state,
            &attempt,
            "session",
            turn,
            context("duplicate", &state),
        )
        .unwrap(),
    );
    assert!(!observed.has_uncertain_input());
    assert_eq!(observed.provider_turns.len(), state.provider_turns.len());
}

#[test]
fn cancelling_an_uncertain_steer_does_not_cancel_or_resend_its_running_turn() {
    let (state, run, attempt) = running();
    let mut state = accepted(&state, 4, Admission::Steer);
    let effect = state
        .effects
        .values_mut()
        .find(|effect| matches!(effect.request, EffectRequest::ProviderTurnSteer { .. }))
        .unwrap();
    effect.status = EffectStatus::Running;
    effect.lease_owner = Some("worker".into());
    let effect_id = effect.id.clone();
    let message = match &effect.request {
        EffectRequest::ProviderTurnSteer { message_id, .. } => message_id.clone(),
        _ => unreachable!(),
    };
    let unknown =
        state.apply(&uncertain_delivery(&state, &effect_id, "steer ack lost", NOW).unwrap());
    assert!(
        !unknown.has_uncertain_input(),
        "an unknown steer must release control/approval/checkpoint work"
    );
    assert_eq!(unknown.effects[&effect_id].status, EffectStatus::Failed);
    let cancelled = unknown
        .apply(&cancel_uncertain(&unknown, &message, context("cancel-steer", &unknown)).unwrap());
    assert!(!cancelled.has_uncertain_input());
    assert_eq!(cancelled.runs[&run].status, RunStatus::Running);
    assert_eq!(
        cancelled.run_attempts[&attempt].status,
        RunAttemptStatus::Running
    );
    assert_eq!(cancelled.effects[&effect_id].status, EffectStatus::Failed);
    assert_eq!(cancelled.effects.len(), unknown.effects.len());
    assert!(
        cancelled
            .turn_items
            .values()
            .any(|item| item.parent_item_id.as_deref()
                == Some(user_item_id("thread", &message).as_str())
                && item.status == TurnItemStatus::Cancelled)
    );
}

#[test]
fn late_native_acceptance_resolves_the_public_uncertainty_error_with_the_outbox() {
    const LATER: &str = "2026-11-14T00:00:01.000Z";
    let mut state = accepted(&initial(), 1, Admission::Queue);
    let effect = state.effects.values_mut().next().unwrap();
    effect.status = EffectStatus::Running;
    effect.lease_owner = Some("worker".into());
    let effect_id = effect.id.clone();
    let attempt = state
        .active_run()
        .unwrap()
        .active_attempt_id
        .clone()
        .unwrap();
    let state = state.apply(&uncertain_delivery(&state, &effect_id, "ack lost", NOW).unwrap());
    assert!(
        state
            .turn_items
            .values()
            .any(|item| item.status == TurnItemStatus::Failed)
    );
    let accepted = state.apply(
        &started(
            &state,
            &attempt,
            "session",
            native("accepted", None),
            Context {
                now: LATER,
                ..context("evidence", &state)
            },
        )
        .unwrap(),
    );
    assert!(!accepted.has_uncertain_input());
    assert_eq!(accepted.effects[&effect_id].status, EffectStatus::Succeeded);
    assert!(
        !accepted
            .turn_items
            .values()
            .any(|item| item.status == TurnItemStatus::Failed)
    );
    assert_eq!(
        accepted.turn_items[&format!("turn-item:delivery-uncertain:{effect_id}")].status,
        TurnItemStatus::Cancelled
    );
    assert_eq!(
        accepted.turn_items[&format!("turn-item:delivery-uncertain:{effect_id}")].updated_at,
        LATER
    );
}

#[test]
fn uncertainty_resolution_requires_the_exact_unresolved_input_and_releases_no_other_delivery() {
    const LATER: &str = "2026-11-14T00:00:01.000Z";
    let (state, _, _) = running();
    let mut state = accepted(&accepted(&state, 4, Admission::Steer), 5, Admission::Steer);
    let effect=state.effects.values_mut().find(|effect|matches!(&effect.request,EffectRequest::ProviderTurnSteer {message_id,..} if message_id==&input(4).message_id)).unwrap();
    effect.status = EffectStatus::Running;
    effect.lease_owner = Some("worker".into());
    let effect_id = effect.id.clone();
    let state = state.apply(&uncertain_delivery(&state, &effect_id, "lost", NOW).unwrap());
    assert!(
        cancel_uncertain(&state, &input(5).message_id, context("wrong-input", &state)).is_err(),
        "another input's uncertainty cannot be selected"
    );
    let mut unrelated = state.clone();
    let error = unrelated
        .turn_items
        .get_mut(&format!("turn-item:delivery-uncertain:{effect_id}"))
        .unwrap();
    let TurnItemBody::Error { failure, .. } = &mut error.body else {
        panic!("error")
    };
    failure.class = "provider".into();
    assert!(
        cancel_uncertain(
            &unrelated,
            &input(4).message_id,
            context("ordinary-error", &unrelated)
        )
        .is_err(),
        "an ordinary failed tool/provider result is not unknown input"
    );
    let cancelled = state.apply(
        &cancel_uncertain(
            &state,
            &input(4).message_id,
            Context {
                now: LATER,
                ..context("cancel", &state)
            },
        )
        .unwrap(),
    );
    assert!(
        cancel_uncertain(
            &cancelled,
            &input(4).message_id,
            context("repeated", &cancelled)
        )
        .is_err(),
        "a resolved delivery cannot be resolved again"
    );
    assert_eq!(
        cancelled.turn_items[&format!("turn-item:delivery-uncertain:{effect_id}")].updated_at,
        LATER
    );

    let mut pending = accepted(&initial(), 1, Admission::Queue);
    pending.effects.values_mut().next().unwrap().last_error =
        Some("known not sent; retry pending".into());
    assert!(
        cancel_uncertain(
            &pending,
            &input(1).message_id,
            context("not-unknown", &pending)
        )
        .is_err(),
        "a scheduled unsent retry is not a delivery-unknown operation"
    );
}

#[test]
fn steer_before_native_turn_is_ready_queues_with_the_target_run() {
    let state = accepted(&initial(), 1, Admission::Queue);
    let next = accepted(&state, 2, Admission::Steer);
    assert_eq!(next.runs.len(), 2);
    let queued = next.next_queued_run().unwrap();
    assert_eq!(
        queued.steer_when_ready_run_id.as_deref(),
        Some(state.active_run().unwrap().id.as_str())
    );
    assert_eq!(next.effects.len(), 1);
}

#[test]
fn stop_before_native_start_cancels_preparation_and_its_durable_start_effect() {
    let state = accepted(&initial(), 1, Admission::Queue);
    let run = state.active_run().unwrap().id.clone();
    let state = accepted(&state, 2, Admission::Queue);
    let stopped = state.apply(&interrupt(&state, &run, context("3", &state)).unwrap());
    assert!(stopped.active_run().is_none());
    assert_eq!(stopped.runs[&run].status, RunStatus::Cancelled);
    assert!(
        stopped
            .effects
            .values()
            .all(|effect| effect.status == EffectStatus::Cancelled)
    );
    assert!(stopped.next_queued_run().is_none());
}

#[test]
fn stop_during_native_submission_waits_for_the_turn_address_and_then_interrupts_it() {
    let state = accepted(&initial(), 1, Admission::Queue);
    let run = state.active_run().unwrap().id.clone();
    let mut state = state;
    let action = state.effects.values_mut().next().unwrap();
    action.status = EffectStatus::Running;
    action.lease_owner = Some("worker".into());
    action.attempt_count = 1;
    let attempt = state.runs[&run].active_attempt_id.clone().unwrap();
    let stopped = state.apply(&interrupt(&state, &run, context("3", &state)).unwrap());
    assert!(
        !stopped
            .effects
            .values()
            .any(|effect| matches!(effect.request, EffectRequest::ProviderTurnInterrupt { .. }))
    );
    let started = stopped.apply(
        &started(
            &stopped,
            &attempt,
            "session",
            native("native-turn", None),
            context("4", &stopped),
        )
        .unwrap(),
    );
    let native = started.run_attempts[&attempt]
        .provider_turn_id
        .as_ref()
        .unwrap();
    assert!(started.effects.values().any(|effect|matches!(&effect.request,EffectRequest::ProviderTurnInterrupt {provider_turn_id,..} if provider_turn_id==native)));
}

#[test]
fn queue_reordering_preserves_run_ordinals_and_stop_holds_each_queued_run() {
    let (state, run, _) = running();
    let state = accepted(&accepted(&state, 4, Admission::Queue), 5, Admission::Queue);
    let ordered = state
        .runs
        .values()
        .filter(|run| run.status == RunStatus::Queued)
        .map(|run| (run.id.clone(), run.ordinal))
        .collect::<BTreeMap<_, _>>();
    let first = state.next_queued_run().unwrap().id.clone();
    let last = ordered.keys().find(|id| **id != first).unwrap();
    let state = state.apply(&move_queued(&state, last, Some(&first)).unwrap());
    assert_eq!(&state.next_queued_run().unwrap().id, last);
    for (id, ordinal) in ordered {
        assert_eq!(state.runs[&id].ordinal, ordinal);
    }
    let state = state.apply(&interrupt(&state, &run, context("6", &state)).unwrap());
    assert!(state.next_queued_run().is_none());
    assert!(
        state
            .turn_items
            .values()
            .any(|item| matches!(item.body, TurnItemBody::RunInterruptRequest { .. }))
    );
}

#[test]
fn successful_completion_advances_only_the_first_unheld_run() {
    let (state, _, attempt) = running();
    let state = accepted(&accepted(&state, 4, Admission::Queue), 5, Admission::Queue);
    let next_id = state.next_queued_run().unwrap().id.clone();
    let next = state.apply(
        &complete(
            &state,
            &attempt,
            "session",
            Completion::Completed,
            context("6", &state),
        )
        .unwrap(),
    );
    assert_eq!(next.active_run().unwrap().id, next_id);
    assert_eq!(next.active_run().unwrap().status, RunStatus::Starting);
    assert_eq!(
        next.runs
            .values()
            .filter(|run| run.status == RunStatus::Queued)
            .count(),
        1
    );
}

#[test]
fn attempt_can_contain_multiple_native_turns_and_reused_native_ids_keep_occurrence() {
    let (state, run, attempt) = running();
    let first = state.run_attempts[&attempt]
        .provider_turn_id
        .clone()
        .unwrap();
    let next = state.apply(
        &started(
            &state,
            &attempt,
            "session",
            native("turn-1", Some(2)),
            context("4", &state),
        )
        .unwrap(),
    );
    assert_eq!(next.runs.len(), 1);
    assert_eq!(next.run_attempts.len(), 1);
    assert_eq!(next.provider_turns.len(), 2);
    assert_eq!(
        next.provider_turns[&first].status,
        ProviderTurnStatus::Running
    );
    let latest = next.run_attempts[&attempt]
        .provider_turn_id
        .as_ref()
        .unwrap();
    assert_ne!(&first, latest);
    assert_eq!(
        next.provider_turns[latest]
            .native_turn_ref
            .as_ref()
            .unwrap()
            .ordinal,
        Some(2)
    );
    assert_eq!(next.runs[&run].status, RunStatus::Running);
    assert!(
        started(
            &next,
            &attempt,
            "session",
            native("turn-1", Some(2)),
            context("5", &next)
        )
        .unwrap()
        .changes
        .is_empty()
    );
}

#[test]
fn detaching_the_process_fences_its_callbacks_without_deleting_native_history() {
    let (state, run, attempt) = running();
    let next = state.apply(&Decision {
        changes: vec![Change::ProviderSessionDetached(ProviderSessionDetached {
            provider_session_id: "session".into(),
            detached_at: NOW.into(),
        })],
        ..Default::default()
    });
    assert!(!next.accepts_observation(&attempt, "session"));
    assert!(
        complete(
            &next,
            &attempt,
            "session",
            Completion::Completed,
            context("4", &next)
        )
        .unwrap()
        .changes
        .is_empty()
    );
    assert_eq!(next.runs[&run].status, RunStatus::Running);
    assert_eq!(next.provider_turns, state.provider_turns);
}

// T3 checkpoint.rollback accepts a request first; the native result owns the
// visible cut. These fixtures contain the capabilities and checkpoint records
// that Host normally saves, rather than assuming rewind can skip those checks.
fn rewind_ready(mut state: State) -> State {
    let capabilities = &mut state
        .provider_sessions
        .get_mut("session")
        .unwrap()
        .capabilities;
    capabilities.threads.can_rollback_thread = true;
    capabilities
        .checkpointing
        .provider_can_rollback_conversation = true;
    capabilities
        .checkpointing
        .provider_rollback_returns_snapshot = true;
    let runs: Vec<_> = state
        .runs
        .values()
        .filter(|run| run.status != RunStatus::Queued)
        .cloned()
        .collect();
    for run in &runs {
        state = state.apply(&prepare_checkpoint_scope(&state, &run.id, "/project", NOW).unwrap());
    }
    let scope = state.checkpoint_scopes.values().next().unwrap().clone();
    for ordinal in std::iter::once(0).chain(runs.iter().map(|run| run.ordinal)) {
        let checkpoint = Checkpoint {
            id: format!("checkpoint:{ordinal}"),
            thread_id: "thread".into(),
            scope_id: scope.id.clone(),
            run_id: runs
                .iter()
                .find(|run| run.ordinal == ordinal)
                .map(|run| run.id.clone()),
            node_id: scope.node_id.clone(),
            parent_checkpoint_id: None,
            ordinal_within_scope: ordinal,
            app_run_ordinal: (ordinal != 0).then_some(ordinal),
            reference: format!("checkpoint-ref:{ordinal}"),
            status: CheckpointStatus::Missing,
            files: Vec::new(),
            files_total: None,
            captured_at: NOW.into(),
        };
        state.checkpoints.insert(checkpoint.id.clone(), checkpoint);
    }
    state
}

fn confirmed_rewind(state: &State) -> State {
    let effect = state
        .effects
        .values()
        .find(|effect| {
            matches!(effect.request, EffectRequest::ProviderThreadRollback { .. })
                && effect.status == EffectStatus::Pending
        })
        .unwrap();
    state.apply(
        &rollback_completed(
            state,
            &effect.id,
            native("native", None),
            None,
            context("confirmed", state),
        )
        .unwrap(),
    )
}

#[test]
fn rolled_back_runs_remain_addressable_and_their_ordinals_are_not_reused() {
    let (state, run, attempt) = running();
    let state = rewind_ready(
        state.apply(
            &complete(
                &state,
                &attempt,
                "session",
                Completion::Completed,
                context("4", &state),
            )
            .unwrap(),
        ),
    );
    let requested = state.apply(&rewind(&state, &run, context("5", &state)).unwrap());
    assert_eq!(
        requested.runs[&run].status,
        RunStatus::Completed,
        "acceptance is not native confirmation"
    );
    let next = confirmed_rewind(&requested);
    assert_eq!(next.runs[&run].status, RunStatus::RolledBack);
    assert_eq!(next.turn_items, state.turn_items);
    let next = accepted(&next, 6, Admission::Queue);
    assert_eq!(next.active_run().unwrap().ordinal, 2);
}

#[test]
fn rewind_holds_unsent_runs_without_rolling_back_or_deleting_their_messages() {
    let (state, run, attempt) = running();
    let state = accepted(&accepted(&state, 4, Admission::Queue), 5, Admission::Queue);
    let state = rewind_ready(
        state.apply(
            &complete(
                &state,
                &attempt,
                "session",
                Completion::Interrupted,
                context("6", &state),
            )
            .unwrap(),
        ),
    );
    let queued: Vec<_> = state
        .runs
        .values()
        .filter(|run| run.status == RunStatus::Queued)
        .map(|run| run.id.clone())
        .collect();
    let requested = state.apply(&rewind(&state, &run, context("7", &state)).unwrap());
    assert_eq!(requested.runs[&run].status, RunStatus::Interrupted);
    let unheld = requested.apply(&hold_queue(&requested, false));
    assert!(
        activate_queued(&unheld, &queued[0], context("early", &unheld)).is_err(),
        "native rollback still owns the lane"
    );
    let rewound = confirmed_rewind(&requested);
    assert_eq!(rewound.runs[&run].status, RunStatus::RolledBack);
    for id in &queued {
        assert_eq!(rewound.runs[id].status, RunStatus::Queued);
        assert!(rewound.runs[id].queue_held);
    }
    assert_eq!(rewound.turn_items, state.turn_items);
    let ready = rewound.apply(&hold_queue(&rewound, false));
    let activation = activate_queued(&ready, &queued[0], context("8", &ready)).unwrap();
    let message=activation.changes.iter().position(|change|matches!(change,Change::TurnItem(item) if item.run_id.as_ref()==Some(&queued[0]))).unwrap();
    let run = activation
        .changes
        .iter()
        .position(|change| matches!(change,Change::Run(run) if run.id==queued[0]))
        .unwrap();
    assert!(message < run);
    let active = ready.apply(&activation);
    let run = &active.runs[&queued[0]];
    assert_eq!(
        active.nodes[run.root_node_id.as_ref().unwrap()].provider_thread_id,
        run.provider_thread_id
    );
    assert_eq!(
        run.provider_thread_id,
        ready.thread.active_provider_thread_id
    );
}

#[test]
fn rewind_without_native_rollback_is_rejected_without_mutating_history() {
    let (state, run, attempt) = running();
    let mut state = rewind_ready(
        state.apply(
            &complete(
                &state,
                &attempt,
                "session",
                Completion::Completed,
                context("4", &state),
            )
            .unwrap(),
        ),
    );
    state
        .provider_sessions
        .get_mut("session")
        .unwrap()
        .capabilities
        .threads
        .can_rollback_thread = false;
    let before = state.clone();
    assert_eq!(
        rewind(&state, &run, context("5", &state)).unwrap_err(),
        "provider conversation rollback is unavailable"
    );
    assert_eq!(state, before);
}

#[test]
fn rewind_publishes_a_correlated_request_and_clears_the_previous_failure() {
    let (state, run, attempt) = running();
    let mut state = rewind_ready(
        state.apply(
            &complete(
                &state,
                &attempt,
                "session",
                Completion::Completed,
                context("done", &state),
            )
            .unwrap(),
        ),
    );
    let mut thread = serde_json::to_value(&state.thread).unwrap();
    thread["rollbackFailure"] =
        serde_json::json!({"requestId":"older-request","message":"older failure"});
    state.thread = serde_json::from_value(thread).unwrap();
    let requested = state.apply(&rewind(&state, &run, context("current-request", &state)).unwrap());
    let thread = serde_json::to_value(&requested.thread).unwrap();
    assert_eq!(thread["rollbackRequestId"], "current-request");
    assert_eq!(
        thread.get("rollbackFailure"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(requested.runs[&run].status, RunStatus::Completed);
}

#[test]
fn rewind_refuses_missing_or_foreign_native_turn_boundaries() {
    let (state, run, attempt) = running();
    let state = rewind_ready(
        state.apply(
            &complete(
                &state,
                &attempt,
                "session",
                Completion::Completed,
                context("done", &state),
            )
            .unwrap(),
        ),
    );
    for reference in [
        None,
        Some(native("", None)),
        Some(ProviderRef {
            driver: "claudeAgent".into(),
            ..native("foreign", None)
        }),
    ] {
        let mut invalid = state.clone();
        invalid
            .provider_turns
            .values_mut()
            .next()
            .unwrap()
            .native_turn_ref = reference;
        assert!(
            rewind(&invalid, &run, context("rewind", &invalid)).is_err(),
            "a missing reference cannot become a successful zero-turn rollback"
        );
    }
    let mut invalid = state.clone();
    invalid
        .provider_turns
        .values_mut()
        .next()
        .unwrap()
        .native_thread_ref = Some(native("another-physical-thread", None));
    assert!(rewind(&invalid, &run, context("rewind", &invalid)).is_err());
}

#[test]
fn rewind_refuses_native_work_even_without_an_active_application_run() {
    let (state, run, attempt) = running();
    let mut state = rewind_ready(
        state.apply(
            &complete(
                &state,
                &attempt,
                "session",
                Completion::Completed,
                context("done", &state),
            )
            .unwrap(),
        ),
    );
    state
        .provider_threads
        .get_mut("native-thread")
        .unwrap()
        .status = ProviderThreadStatus::Active;
    assert!(state.active_run().is_none());
    assert!(rewind(&state, &run, context("rewind", &state)).is_err());
}

#[test]
fn a_provider_exit_during_rollback_publishes_only_the_matching_failure() {
    let (state, run, attempt) = running();
    let state = rewind_ready(
        state.apply(
            &complete(
                &state,
                &attempt,
                "session",
                Completion::Completed,
                context("done", &state),
            )
            .unwrap(),
        ),
    );
    let requested =
        state.apply(&rewind(&state, &run, context("rollback-request", &state)).unwrap());
    let failure = ProviderFailure {
        class: "provider_error".into(),
        message: "provider ended during rewind".into(),
        code: None,
        retryable: None,
        reset_at: None,
    };
    let stopped = requested.apply(
        &provider_stopped(
            &requested,
            "codex",
            &failure,
            context("stopped", &requested),
        )
        .unwrap(),
    );
    let thread = serde_json::to_value(&stopped.thread).unwrap();
    assert_eq!(thread["rollbackFailure"]["requestId"], "rollback-request");
    assert_eq!(thread["rollbackFailure"]["message"], failure.message);
    assert_eq!(stopped.runs[&run].status, RunStatus::Completed);
    let mut newer = requested.clone();
    let mut thread = serde_json::to_value(&newer.thread).unwrap();
    thread["rollbackRequestId"] = "newer-request".into();
    newer.thread = serde_json::from_value(thread).unwrap();
    let stopped = newer.apply(
        &provider_stopped(&newer, "codex", &failure, context("late-stopped", &newer)).unwrap(),
    );
    assert_eq!(
        serde_json::to_value(&stopped.thread)
            .unwrap()
            .get("rollbackFailure"),
        Some(&serde_json::Value::Null),
        "an older rollback failure cannot overwrite the newer request"
    );
}

#[test]
fn a_fork_does_not_inherit_its_parents_rollback_request_or_failure() {
    let (state, run, attempt) = running();
    let mut state = state.apply(
        &complete(
            &state,
            &attempt,
            "session",
            Completion::Completed,
            context("done", &state),
        )
        .unwrap(),
    );
    let mut thread = serde_json::to_value(&state.thread).unwrap();
    thread["rollbackRequestId"] = "parent-request".into();
    thread["rollbackFailure"] =
        serde_json::json!({"requestId":"parent-request","message":"parent failure"});
    state.thread = serde_json::from_value(thread).unwrap();
    let run = &state.runs[&run];
    let (child, _) = fork_thread(
        &state.thread,
        run,
        state.context_source_point(run),
        "child",
        "transfer",
        NOW,
    )
    .unwrap();
    let child = serde_json::to_value(child).unwrap();
    assert_eq!(
        child.get("rollbackRequestId"),
        Some(&serde_json::Value::Null)
    );
    assert_eq!(child.get("rollbackFailure"), Some(&serde_json::Value::Null));
}

#[test]
fn proposal_body_has_one_owner_and_its_lifecycle_outlives_the_stream() {
    let (state, run, attempt) = running();
    let attempt_row = &state.run_attempts[&attempt];
    let item = TurnItem {
        id: "plan-item".into(),
        thread_id: state.thread.id.clone(),
        run_id: Some(run),
        node_id: Some("plan-node".into()),
        provider_thread_id: Some(attempt_row.provider_thread_id.clone()),
        provider_turn_id: attempt_row.provider_turn_id.clone(),
        native_item_ref: None,
        parent_item_id: None,
        ordinal: 20,
        status: TurnItemStatus::Running,
        title: None,
        started_at: Some(NOW.into()),
        completed_at: None,
        updated_at: NOW.into(),
        body: TurnItemBody::ProposedPlan {
            plan_id: "plan".into(),
            markdown: "Only one copy".into(),
            streaming: true,
        },
    };
    let plan = Plan {
        id: "plan".into(),
        thread_id: state.thread.id.clone(),
        run_id: item.run_id.clone(),
        node_id: item.node_id.clone().unwrap(),
        kind: PlanKind::ProposedPlan,
        status: PlanStatus::Active,
        detail_in_turn_item: true,
    };
    let root = &state.nodes[&attempt_row.root_node_id];
    let node = ExecutionNode {
        id: "plan-node".into(),
        parent_node_id: Some(root.id.clone()),
        kind: NodeKind::Plan,
        counts_for_run: false,
        ..root.clone()
    };
    let state = state.apply(&Decision {
        changes: vec![Change::Node(node)],
        ..Default::default()
    });
    let state = state.apply(&observe_plan(&state, &attempt, "session", plan, item).unwrap());
    let metadata = serde_json::to_string(&state.plans["plan"]).unwrap();
    assert!(!metadata.contains("Only one copy"));
    assert_eq!(state.plans["plan"].node_id, "plan-node");
    let next = state.apply(
        &complete(
            &state,
            &attempt,
            "session",
            Completion::Interrupted,
            context("4", &state),
        )
        .unwrap(),
    );
    assert_eq!(next.plans["plan"].status, PlanStatus::Active);
    assert_eq!(
        next.turn_items["plan-item"].status,
        TurnItemStatus::Interrupted
    );
}

fn approval() -> (State, String, String) {
    let (state, run, attempt) = running();
    let native = &state.run_attempts[&attempt];
    let item = TurnItem {
        id: "approval-item".into(),
        thread_id: state.thread.id.clone(),
        run_id: Some(run),
        node_id: Some("approval-item".into()),
        provider_thread_id: Some(native.provider_thread_id.clone()),
        provider_turn_id: native.provider_turn_id.clone(),
        native_item_ref: None,
        parent_item_id: None,
        ordinal: 20,
        status: TurnItemStatus::Waiting,
        title: None,
        started_at: Some(NOW.into()),
        completed_at: None,
        updated_at: NOW.into(),
        body: TurnItemBody::ApprovalRequest {
            request_id: "request".into(),
            request_kind: "command".into(),
            prompt: Some("Run command?".into()),
            reason: None,
            options: vec![ApprovalOption {
                decision: ApprovalDecision::Accept,
                label: "Allow once".into(),
                warning: None,
            }],
        },
    };
    let request = RuntimeRequest {
        id: "request".into(),
        node_id: item.node_id.clone().unwrap(),
        provider_turn_id: item.provider_turn_id.clone(),
        kind: "command".into(),
        native_request_ref: Some(native_ref()),
        status: RequestStatus::Pending,
        response_capability: ResponseCapability::Live {
            provider_session_id: "session".into(),
            supports_feedback: None,
        },
        created_at: NOW.into(),
        resolved_at: None,
        decision: None,
        feedback: None,
        answers: None,
    };
    let root = &state.nodes[&native.root_node_id];
    let node = ExecutionNode {
        id: item.node_id.clone().unwrap(),
        parent_node_id: Some(root.id.clone()),
        kind: NodeKind::ApprovalRequest,
        status: NodeStatus::Waiting,
        counts_for_run: false,
        runtime_request_id: Some(request.id.clone()),
        ..root.clone()
    };
    let state = state.apply(&Decision {
        changes: vec![Change::Node(node)],
        ..Default::default()
    });
    let state = state.apply(&observe_request(&state, &attempt, "session", request, item).unwrap());
    (state, attempt, "request".into())
}

fn native_ref() -> ProviderRef {
    native("native-request", None)
}

#[test]
fn approval_body_is_saved_once_and_in_flight_response_cannot_be_sent_twice() {
    let (state, _, request) = approval();
    let json = serde_json::to_string(&state.runtime_requests[&request]).unwrap();
    assert!(!json.contains("Run command?"));
    let response = RequestResponse::Approval {
        decision: ApprovalDecision::Accept,
        feedback: None,
    };
    let decision = respond(
        &state,
        &request,
        response.clone(),
        "codex",
        Context {
            now: "2026-11-15T00:00:10.000Z",
            ..context("4", &state)
        },
    )
    .unwrap();
    let next = state.apply(&decision);
    assert_eq!(next.nodes["approval-item"].status, NodeStatus::Completed);
    assert_eq!(
        next.nodes["approval-item"].completed_at.as_deref(),
        Some("2026-11-15T00:00:10.000Z")
    );
    assert_eq!(
        next.turn_items["approval-item"].completed_at.as_deref(),
        Some("2026-11-15T00:00:10.000Z")
    );
    assert_eq!(
        next.turn_items["approval-item"].updated_at,
        "2026-11-15T00:00:10.000Z"
    );
    assert!(respond(&next, &request, response, "codex", context("5", &next)).is_err());
    assert_eq!(
        next.runtime_requests[&request].status,
        RequestStatus::Resolved
    );
    assert_eq!(
        next.turn_items["approval-item"].status,
        TurnItemStatus::Completed
    );
}

#[test]
fn a_lost_approval_callback_expires_without_making_it_answerable_again() {
    let (state, _, request) = approval();
    let expired = state.apply(&request_source_closed(&state, &request, NOW).unwrap());
    assert_eq!(
        expired.runtime_requests[&request].status,
        RequestStatus::Expired
    );
    assert!(
        expired
            .request_item(&request)
            .is_some_and(|item| item.status == TurnItemStatus::Cancelled)
    );
    assert!(
        request_source_closed(&expired, &request, NOW)
            .unwrap()
            .changes
            .is_empty()
    );
    assert!(
        respond(
            &expired,
            &request,
            RequestResponse::Approval {
                decision: ApprovalDecision::Accept,
                feedback: None
            },
            "codex",
            context("late-answer", &expired)
        )
        .is_err()
    );
}

#[test]
fn a_recorded_answer_survives_a_failed_callback_without_reopening_the_question() {
    let (state, _, request) = approval();
    let response = RequestResponse::Approval {
        decision: ApprovalDecision::Accept,
        feedback: Some("Proceed".into()),
    };
    let next = state.apply(&resolve_request(&state, &request, Some(&response), NOW).unwrap());
    assert_eq!(
        next.runtime_requests[&request].status,
        RequestStatus::Resolved
    );
    assert_eq!(
        next.runtime_requests[&request].decision,
        Some(ApprovalDecision::Accept)
    );
    assert_eq!(
        next.runtime_requests[&request].feedback.as_deref(),
        Some("Proceed")
    );
    assert_eq!(
        next.turn_items["approval-item"].status,
        TurnItemStatus::Completed
    );
    let failed = next.apply(&response_failed(&next, &request, "connection lost", NOW));
    assert_eq!(failed.runtime_requests, next.runtime_requests);
    assert!(
        failed
            .turn_items
            .values()
            .any(|item| item.title.as_deref() == Some("Answer delivery not confirmed"))
    );
}

#[test]
fn a_failed_answer_delivery_cannot_resend_and_stop_cancels_unanswered_requests() {
    let (state, attempt, request) = approval();
    let response = RequestResponse::Approval {
        decision: ApprovalDecision::Accept,
        feedback: None,
    };
    let next = state.apply(
        &respond(
            &state,
            &request,
            response.clone(),
            "codex",
            context("4", &state),
        )
        .unwrap(),
    );
    let next = next.apply(&response_failed(
        &next,
        &request,
        "Delivery result is unknown",
        NOW,
    ));
    assert!(respond(&next, &request, response, "codex", context("5", &next)).is_err());
    let next = next.apply(&resolve_request(&next, &request, None, NOW).unwrap());
    assert_eq!(
        next.runtime_requests[&request].status,
        RequestStatus::Resolved
    );
    let stopped = state.apply(
        &complete(
            &state,
            &attempt,
            "session",
            Completion::Interrupted,
            context("5", &state),
        )
        .unwrap(),
    );
    assert_eq!(
        stopped.runtime_requests[&request].status,
        RequestStatus::Cancelled
    );
    assert_eq!(
        stopped.turn_items["approval-item"].status,
        TurnItemStatus::Interrupted
    );
}

#[test]
fn t3_model_options_are_typed_selections_in_an_array() {
    let value = serde_json::json!({"instanceId":"codex","model":"example","options":[{"id":"reasoningEffort","value":"high"},{"id":"webSearch","value":true}]});
    let selection: ModelSelection = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(selection).unwrap(), value);
    assert!(serde_json::from_value::<ModelSelection>(serde_json::json!({"instanceId":"codex","model":"example","options":{"reasoningEffort":"high"}})).is_err());
}

#[test]
fn queued_attempts_cannot_accept_provider_callbacks_and_inherit_a_queue_hold() {
    let (state, _, _) = running();
    let state = accepted(&state, 4, Admission::Queue);
    let queued = state.next_queued_run().unwrap();
    assert!(
        started(
            &state,
            queued.active_attempt_id.as_deref().unwrap(),
            "session",
            native("invalid", None),
            context("5", &state)
        )
        .unwrap()
        .changes
        .is_empty()
    );
    let state = state.apply(&hold_queue(&state, true));
    let state = accepted(&state, 6, Admission::Queue);
    assert!(
        state
            .runs
            .values()
            .filter(|run| run.status == RunStatus::Queued)
            .all(|run| run.queue_held)
    );
}

#[test]
fn queued_activation_reuses_execution_ids_and_moves_after_work_started_ahead() {
    let (state, _, attempt) = running();
    let state = accepted(&state, 4, Admission::Queue);
    let held = state.next_queued_run().unwrap().clone();
    let state = state.apply(&hold_queue(&state, true));
    let state = state.apply(
        &complete(
            &state,
            &attempt,
            "session",
            Completion::Completed,
            context("5", &state),
        )
        .unwrap(),
    );
    let state = accepted(&state, 6, Admission::Queue);
    let newer = state.active_run().unwrap().clone();
    let state = state.apply(
        &started(
            &state,
            newer.active_attempt_id.as_deref().unwrap(),
            "session",
            native("newer", None),
            context("7", &state),
        )
        .unwrap(),
    );
    let state = state.apply(
        &complete(
            &state,
            newer.active_attempt_id.as_deref().unwrap(),
            "session",
            Completion::Completed,
            context("8", &state),
        )
        .unwrap(),
    );
    let state = state.apply(&hold_queue(&state, false));
    let active = state.apply(&activate_queued(&state, &held.id, context("9", &state)).unwrap());
    let run = &active.runs[&held.id];
    assert!(run.ordinal > newer.ordinal);
    assert_eq!(run.active_attempt_id, held.active_attempt_id);
    assert_eq!(run.root_node_id, held.root_node_id);
}

#[test]
fn a_late_native_allocation_keeps_only_its_resource_and_cannot_replace_a_new_binding() {
    let state = initial();
    let state = state.apply(
        &admit(
            &state,
            input(1),
            Admission::Queue,
            context("accepted", &state),
        )
        .unwrap(),
    );
    let run = state.active_run().unwrap().clone();
    let stopped = state.apply(&interrupt(&state, &run.id, context("stop", &state)).unwrap());
    let thread_id = run.provider_thread_id.as_ref().unwrap();
    let expected = stopped.provider_threads[thread_id]
        .native_thread_ref
        .clone();
    let session = stopped.provider_sessions.values().next().unwrap().clone();
    let allocated = native("late-native-session", None);
    let decision = native_resource_loaded(
        &stopped,
        thread_id,
        expected.as_ref(),
        session.clone(),
        allocated.clone(),
        false,
        NOW,
    )
    .unwrap();
    let loaded = stopped.apply(&decision);
    assert_eq!(loaded.runs, stopped.runs);
    assert_eq!(loaded.run_attempts, stopped.run_attempts);
    assert_eq!(loaded.nodes, stopped.nodes);
    assert_eq!(loaded.turn_items, stopped.turn_items);
    assert_eq!(loaded.effects, stopped.effects);
    assert!(loaded.provider_turns.is_empty());
    assert_eq!(
        loaded.provider_threads[thread_id]
            .native_thread_ref
            .as_ref(),
        Some(&allocated)
    );
    let mut replaced = stopped.clone();
    replaced
        .provider_threads
        .get_mut(thread_id)
        .unwrap()
        .native_thread_ref = Some(native("newer native resource", None));
    assert!(
        native_resource_loaded(
            &replaced,
            thread_id,
            expected.as_ref(),
            session.clone(),
            allocated.clone(),
            false,
            NOW
        )
        .unwrap()
        .changes
        .is_empty()
    );
    let mut foreign = session.clone();
    foreign.provider_instance_id = "claude".into();
    assert!(
        native_resource_loaded(
            &stopped,
            thread_id,
            expected.as_ref(),
            foreign,
            allocated.clone(),
            false,
            NOW
        )
        .is_err()
    );
    let mut foreign = allocated.clone();
    foreign.driver = "claudeAgent".into();
    assert!(
        native_resource_loaded(
            &stopped,
            thread_id,
            expected.as_ref(),
            session.clone(),
            foreign,
            false,
            NOW
        )
        .is_err()
    );
    let mut foreign = stopped.clone();
    foreign
        .provider_threads
        .get_mut(thread_id)
        .unwrap()
        .app_thread_id = Some("another conversation".into());
    assert!(
        native_resource_loaded(
            &foreign,
            thread_id,
            expected.as_ref(),
            session.clone(),
            allocated.clone(),
            false,
            NOW
        )
        .is_err()
    );
    let mut foreign = session.clone();
    foreign.driver = "claudeAgent".into();
    assert!(
        native_resource_loaded(
            &stopped,
            thread_id,
            expected.as_ref(),
            foreign,
            allocated.clone(),
            false,
            NOW
        )
        .is_err()
    );
    let mut missing = allocated.clone();
    missing.native_id = None;
    assert!(
        native_resource_loaded(
            &stopped,
            thread_id,
            expected.as_ref(),
            session.clone(),
            missing,
            false,
            NOW
        )
        .is_err()
    );
    let mut deleted = stopped.clone();
    deleted.thread.deleted_at = Some(NOW.into());
    assert!(
        native_resource_loaded(
            &deleted,
            thread_id,
            expected.as_ref(),
            session,
            allocated,
            false,
            NOW
        )
        .unwrap()
        .changes
        .is_empty()
    );
}

#[test]
fn removing_an_unsent_stopped_input_preserves_the_queue_and_cannot_remove_real_native_work() {
    let state = initial();
    let state =
        state.apply(&admit(&state, input(1), Admission::Queue, context("first", &state)).unwrap());
    let original = state.active_run().unwrap().clone();
    let state = state.apply(
        &admit(
            &state,
            input(2),
            Admission::Queue,
            context("queued", &state),
        )
        .unwrap(),
    );
    let stopped = state.apply(&interrupt(&state, &original.id, context("stop", &state)).unwrap());
    let queued = stopped
        .runs
        .values()
        .find(|run| run.status == RunStatus::Queued)
        .unwrap()
        .clone();
    let removed = stopped.apply(&remove_input(&stopped, &original.id, NOW).unwrap());
    assert_eq!(removed.runs[&queued.id], queued);
    assert_eq!(removed.effects, stopped.effects);
    assert!(removed.provider_turns.is_empty());
    assert!(removed.turn_items.values().any(|item|item.status==TurnItemStatus::Cancelled && matches!(&item.body,TurnItemBody::UserMessage{message_id,..} if message_id==&original.user_message_id)));
    assert!(
        retry_input(
            &removed,
            &original.id,
            false,
            context("retry removed", &removed)
        )
        .is_err()
    );
    let (running, run_id, attempt_id) = running();
    let failed = running.apply(
        &complete(
            &running,
            &attempt_id,
            "session",
            Completion::Failed(ProviderFailure {
                class: "provider".into(),
                message: "native failure".into(),
                code: None,
                retryable: None,
                reset_at: None,
            }),
            context("failure", &running),
        )
        .unwrap(),
    );
    assert!(remove_input(&failed, &run_id, NOW).is_err());
    let mut missing_pointer = failed.clone();
    missing_pointer
        .run_attempts
        .get_mut(&attempt_id)
        .unwrap()
        .provider_turn_id = None;
    assert!(
        remove_input(&missing_pointer, &run_id, NOW).is_err(),
        "a real saved provider turn remains proof of delivery"
    );
}

#[test]
fn workspace_resource_callbacks_cannot_restart_stopped_or_replaced_launches() {
    use crate::workspace::{
        SetupChange, WorkspacePreparation as Launch, WorkspaceSetup, WorkspaceStrategy,
    };
    let mut state = initial();
    state.thread.workspace_launch = Some(Box::new(Launch {
        run_id: None,
        project_cwd: "/project".into(),
        strategy: Some(WorkspaceStrategy::Worktree {
            base_ref: "main".into(),
            start_from_origin: false,
        }),
        generation: Uuid::nil(),
        created_worktree: false,
        setup: Some(WorkspaceSetup::new(Some("main".into()), 0)),
        visible: true,
    }));
    let mut request = input(1);
    request.workspace_preparation = Some(WorkspacePreparation::Worktree {
        base_ref: "main".into(),
        start_from_origin: false,
        branch: None,
    });
    state =
        state.apply(&admit(&state, request, Admission::Queue, context("launch", &state)).unwrap());
    let run = state.active_run().unwrap().clone();
    let launch = state.thread.workspace_launch.clone().unwrap();
    assert_eq!(launch.run_id.as_ref(), Some(&run.id));
    let generation = launch.generation;
    assert!(
        workspace_progress(
            &state,
            generation,
            SetupChange::Begin {
                workspace: launch.clone()
            },
            NOW
        )
        .unwrap()
        .changes
        .is_empty()
    );
    assert!(
        workspace_progress(
            &state,
            Uuid::nil(),
            SetupChange::Created {
                path: "/wrong".into(),
                branch: "wrong".into()
            },
            NOW
        )
        .is_err()
    );
    assert!(
        workspace_progress(
            &state,
            Uuid::nil(),
            SetupChange::Expire { ended_at: 0 },
            NOW
        )
        .unwrap()
        .changes
        .is_empty()
    );
    let mut foreign = launch.clone();
    foreign.run_id = Some("another accepted input".into());
    assert!(
        workspace_progress(
            &state,
            generation,
            SetupChange::Begin { workspace: foreign },
            NOW
        )
        .is_err()
    );
    let stopped = state.apply(&interrupt(&state, &run.id, context("stop", &state)).unwrap());
    assert!(
        workspace_progress(
            &stopped,
            generation,
            SetupChange::Begin { workspace: launch },
            NOW
        )
        .is_err()
    );
    assert!(
        workspace_progress(
            &stopped,
            generation,
            SetupChange::Script {
                handle: "must not launch a PTY".into()
            },
            NOW
        )
        .is_err()
    );
    assert!(
        workspace_progress(
            &stopped,
            generation,
            SetupChange::Created {
                path: "/late".into(),
                branch: "late".into()
            },
            NOW
        )
        .is_err()
    );
    assert_eq!(stopped.thread.worktree_path.as_deref(), Some("/project"));
    assert!(stopped.provider_turns.is_empty());
    assert!(stopped.effects.is_empty());
}

#[test]
fn workspace_preparation_failure_retry_and_release_keep_the_reserved_run_and_attempt() {
    let mut input = input(1);
    input.workspace_preparation = Some(WorkspacePreparation::Worktree {
        base_ref: "main".into(),
        branch: None,
        start_from_origin: false,
    });
    let state = initial();
    let state = state.apply(&admit(&state, input, Admission::Queue, context("1", &state)).unwrap());
    let run = state.active_run().unwrap().clone();
    assert_eq!(run.status, RunStatus::Preparing);
    assert!(state.effects.is_empty());
    assert!(
        started(
            &state,
            run.active_attempt_id.as_deref().unwrap(),
            "session",
            native("early", None),
            context("2", &state)
        )
        .unwrap()
        .changes
        .is_empty()
    );
    let state = state
        .apply(&preparation_progress(&state, &run.id, PreparationPhase::SetupScript, NOW).unwrap());
    let state = state.apply(
        &preparation_fail(
            &state,
            &run.id,
            ProviderFailure {
                class: "provider_error".into(),
                message: "setup failed".into(),
                code: None,
                retryable: Some(true),
                reset_at: None,
            },
            context("3", &state),
        )
        .unwrap(),
    );
    assert_eq!(state.runs[&run.id].status, RunStatus::Failed);
    let state = state
        .apply(&retry_input(&state, &run.id, false, context("preparation-retry", &state)).unwrap());
    assert_eq!(state.runs[&run.id].active_attempt_id, run.active_attempt_id);
    assert_eq!(state.runs[&run.id].root_node_id, run.root_node_id);
    let state = state.apply(
        &preparation_release(
            &state,
            &run.id,
            "/worktree",
            Some("topic"),
            context("4", &state),
        )
        .unwrap(),
    );
    assert_eq!(state.thread.worktree_path.as_deref(), Some("/worktree"));
    assert_eq!(state.runs[&run.id].status, RunStatus::Starting);
    assert_eq!(state.effects.len(), 1);
    assert!(preparation_release(&state, &run.id, "/other", None, context("5", &state)).is_err());
}

#[test]
fn application_completion_does_not_rewrite_an_already_failed_native_turn() {
    let (state, _, attempt) = running();
    let turn_id = state.run_attempts[&attempt]
        .provider_turn_id
        .clone()
        .unwrap();
    let state = state.apply(&finish_provider_turn(
        &state,
        &turn_id,
        "session",
        Completion::Failed(ProviderFailure {
            class: "provider_error".into(),
            message: "failed native turn".into(),
            code: None,
            retryable: None,
            reset_at: None,
        }),
        NOW,
    ));
    let state = state.apply(
        &started(
            &state,
            &attempt,
            "session",
            native("retry-native", None),
            context("4", &state),
        )
        .unwrap(),
    );
    let state = state.apply(
        &complete(
            &state,
            &attempt,
            "session",
            Completion::Completed,
            context("5", &state),
        )
        .unwrap(),
    );
    assert_eq!(
        state.provider_turns[&turn_id].status,
        ProviderTurnStatus::Failed
    );
}

#[test]
fn promoting_a_queue_entry_moves_its_original_message_and_cancels_only_its_reserved_execution() {
    let state = accepted(&initial(), 1, Admission::Queue);
    let state = accepted(&state, 2, Admission::Queue);
    let queued = state.next_queued_run().unwrap().clone();
    let run = state.active_run().unwrap().clone();
    let deferred = state.apply(&promote_queued(&state, &queued.id, context("3", &state)).unwrap());
    assert_eq!(
        deferred.runs[&queued.id].steer_when_ready_run_id.as_ref(),
        Some(&run.id)
    );
    let state = deferred.apply(
        &started(
            &deferred,
            run.active_attempt_id.as_deref().unwrap(),
            "session",
            native("native-turn", None),
            context("4", &deferred),
        )
        .unwrap(),
    );
    let promoted = state.apply(&promote_queued(&state, &queued.id, context("5", &state)).unwrap());
    assert_eq!(promoted.runs[&queued.id].status, RunStatus::Cancelled);
    assert_eq!(
        promoted.run_attempts[queued.active_attempt_id.as_ref().unwrap()].status,
        RunAttemptStatus::Cancelled
    );
    assert_eq!(
        promoted.nodes[queued.root_node_id.as_ref().unwrap()].status,
        NodeStatus::Cancelled
    );
    let messages:Vec<_>=promoted.turn_items.values().filter(|item|matches!(&item.body,TurnItemBody::UserMessage {message_id,..} if message_id==&queued.user_message_id)).collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].run_id.as_ref(), Some(&run.id));
    assert!(
        matches!(&messages[0].body,TurnItemBody::UserMessage {input_intent,..} if input_intent=="promoted_queued_to_steer")
    );
    assert!(promoted.effects.values().any(|effect|matches!(&effect.request,EffectRequest::ProviderTurnSteer {message_id,..} if message_id==&queued.user_message_id)));
}

#[test]
fn successful_native_completion_waits_for_checkpoint_capture_before_starting_queued_work() {
    let (state, run, attempt) = running();
    let state = state.apply(&prepare_checkpoint_scope(&state, &run, "/project", NOW).unwrap());
    let state = accepted(&state, 4, Admission::Queue);
    let next = state.next_queued_run().unwrap().id.clone();
    let root_id = state.runs[&run].root_node_id.clone().unwrap();
    let waiting = state.apply(
        &complete(
            &state,
            &attempt,
            "session",
            Completion::Completed,
            context("5", &state),
        )
        .unwrap(),
    );
    assert_eq!(waiting.runs[&run].status, RunStatus::Waiting);
    assert_eq!(waiting.nodes[&root_id].status, NodeStatus::Waiting);
    let native_turn = &waiting.provider_turns[waiting.run_attempts[&attempt]
        .provider_turn_id
        .as_ref()
        .unwrap()];
    assert_eq!(native_turn.status, ProviderTurnStatus::Completed);
    assert_eq!(native_turn.completed_at.as_deref(), Some(NOW));
    assert_eq!(
        waiting.run_attempts[&attempt].status,
        RunAttemptStatus::Completed
    );
    assert_eq!(waiting.runs[&next].status, RunStatus::Queued);
    let scope = waiting.nodes[&root_id].checkpoint_scope_id.clone().unwrap();
    assert!(waiting.effects.values().any(|effect|matches!(&effect.request,EffectRequest::CheckpointCapture {scope_id,run_id} if scope_id==&scope && run_id==&run)));
    let checkpoint = Checkpoint {
        id: "checkpoint".into(),
        thread_id: waiting.thread.id.clone(),
        scope_id: scope,
        run_id: Some(run.clone()),
        node_id: root_id.clone(),
        parent_checkpoint_id: None,
        ordinal_within_scope: waiting.runs[&run].ordinal,
        app_run_ordinal: Some(waiting.runs[&run].ordinal),
        reference: "refs/t3/orchestration-v2/checkpoints/example/ordinal/1".into(),
        status: CheckpointStatus::Missing,
        files: Vec::new(),
        files_total: None,
        captured_at: NOW.into(),
    };
    let completed = waiting.apply(
        &checkpoint_captured(&waiting, &run, checkpoint.clone(), context("6", &waiting)).unwrap(),
    );
    assert_eq!(completed.runs[&run].status, RunStatus::Completed);
    assert_eq!(completed.nodes[&root_id].status, NodeStatus::Completed);
    assert_eq!(completed.active_run().unwrap().id, next);
    assert!(
        checkpoint_captured(&completed, &run, checkpoint, context("7", &completed))
            .unwrap()
            .changes
            .is_empty()
    );
}

#[test]
fn callbacks_require_the_exact_attached_session_and_a_live_provider_process() {
    let (state, _, attempt) = running();
    for (session_id, status) in [
        ("other-session", ProviderSessionStatus::Ready),
        ("session", ProviderSessionStatus::Starting),
        ("session", ProviderSessionStatus::Stopped),
        ("session", ProviderSessionStatus::Error),
    ] {
        let mut state = state.clone();
        state.provider_sessions.get_mut("session").unwrap().status = status;
        let late = started(
            &state,
            &attempt,
            session_id,
            native("late", None),
            context("20", &state),
        )
        .unwrap();
        assert!(late.changes.is_empty());
        assert!(late.effects.is_empty());
        assert!(
            complete(
                &state,
                &attempt,
                session_id,
                Completion::Completed,
                context("21", &state)
            )
            .unwrap()
            .changes
            .is_empty()
        );
    }
}

mod fork_tests {
    use super::*;

    #[test]
    fn native_forks_require_a_finished_owned_turn_and_keep_the_exact_item_boundary() {
        let state = initial();
        let provider = &state.provider_threads["native-thread"];
        let point = ContextSourcePoint {
            thread_id: state.thread.id.clone(),
            run_id: None,
            checkpoint_id: None,
            turn_item_id: Some("real-last-source-item".into()),
            provider_thread_ref: provider.native_thread_ref.clone(),
            provider_turn_ref: Some(native("actual-raw-turn", None)),
        };
        let turn = ProviderTurn {
            id: "observed-native-turn".into(),
            provider_thread_id: provider.id.clone(),
            node_id: "real-root".into(),
            run_attempt_id: None,
            native_thread_ref: point.provider_thread_ref.clone(),
            native_turn_ref: point.provider_turn_ref.clone(),
            ordinal: 1,
            status: ProviderTurnStatus::Completed,
            started_at: Some(NOW.into()),
            completed_at: Some(NOW.into()),
            token_usage: None,
            model: None,
        };
        for (status, allowed) in [
            (ProviderTurnStatus::Pending, false),
            (ProviderTurnStatus::Running, false),
            (ProviderTurnStatus::Completed, true),
            (ProviderTurnStatus::Failed, true),
            (ProviderTurnStatus::Interrupted, true),
            (ProviderTurnStatus::Cancelled, true),
        ] {
            let turn = ProviderTurn {
                status,
                ..turn.clone()
            };
            let result = fork_native_thread(
                &state.thread,
                provider,
                Some(&turn),
                point.clone(),
                "target",
                "transfer",
                NOW,
            );
            assert_eq!(result.is_ok(), allowed, "{status:?}");
            if let Ok((thread, transfer)) = result {
                assert_eq!(
                    thread.forked_from,
                    Some(ThreadForkSource::ProviderThread {
                        provider_thread_id: provider.id.clone(),
                        provider_turn_id: Some(turn.id)
                    })
                );
                assert_eq!(transfer.source_point, point);
                assert!(transfer.target_run_id.is_none());
                assert!(thread.active_provider_thread_id.is_none());
            }
        }
        for invalid in [
            "foreign-turn",
            "app-attempt",
            "foreign-point",
            "wrong-raw-turn",
            "no-item",
            "app-run",
            "foreign-provider",
            "deleted",
            "same-thread",
        ] {
            let mut thread = state.thread.clone();
            let mut provider = provider.clone();
            let mut turn = turn.clone();
            let mut point = point.clone();
            match invalid {
                "foreign-turn" => turn.provider_thread_id = "other-provider-thread".into(),
                "app-attempt" => turn.run_attempt_id = Some("owned-attempt".into()),
                "foreign-point" => point.thread_id = "other-thread".into(),
                "wrong-raw-turn" => point.provider_turn_ref = Some(native("later-turn", None)),
                "no-item" => point.turn_item_id = None,
                "app-run" => point.run_id = Some("invented-run".into()),
                "foreign-provider" => provider.app_thread_id = Some("other-thread".into()),
                "deleted" => thread.deleted_at = Some(NOW.into()),
                _ => {}
            }
            let target = if invalid == "same-thread" {
                thread.id.as_str()
            } else {
                "target"
            };
            assert!(
                fork_native_thread(
                    &thread,
                    &provider,
                    Some(&turn),
                    point,
                    target,
                    "transfer",
                    NOW
                )
                .is_err(),
                "{invalid}"
            );
        }
        let mut imported = state.thread.clone();
        imported.history_origin = Some(ThreadHistoryOrigin::V1Import);
        let point = ContextSourcePoint {
            provider_turn_ref: None,
            ..point
        };
        assert!(
            fork_native_thread(
                &imported,
                provider,
                None,
                point.clone(),
                "target",
                "transfer",
                NOW
            )
            .is_ok()
        );
        assert!(
            fork_native_thread(
                &state.thread,
                provider,
                None,
                point,
                "target",
                "transfer",
                NOW
            )
            .is_err()
        );
    }

    fn undelivered_fork() -> (State, String) {
        let (source, mut run, point) = fork_source_fixture();
        run.status = RunStatus::Completed;
        let (target, transfer) =
            fork_thread(&source, &run, point, "target", "transfer", NOW).unwrap();
        let state = State::new(target).apply(&Decision {
            changes: vec![Change::ContextTransfer(transfer)],
            effects: vec![],
        });
        let mut first = input(100);
        first.fork_origin = Some(ProviderThreadFork {
            provider_thread_id: "source-provider-thread".into(),
            provider_turn_id: Some("source-turn".into()),
            checkpoint_id: None,
        });
        let state = state.apply(
            &admit(
                &state,
                first,
                Admission::Queue,
                context("fork-first", &state),
            )
            .unwrap(),
        );
        let id = state.active_run().unwrap().id.clone();
        (state, id)
    }

    #[test]
    fn undelivered_forks_are_reclaimed_only_after_the_assigned_run_finishes() {
        let (state, id) = undelivered_fork();
        for (status, claimable) in [
            (RunStatus::Preparing, false),
            (RunStatus::Queued, false),
            (RunStatus::Starting, false),
            (RunStatus::Running, false),
            (RunStatus::Waiting, false),
            (RunStatus::Completed, true),
            (RunStatus::Failed, true),
            (RunStatus::Interrupted, true),
            (RunStatus::Cancelled, true),
            (RunStatus::RolledBack, true),
        ] {
            let mut state = state.clone();
            state.runs.get_mut(&id).unwrap().status = status;
            assert_eq!(state.pending_fork().is_some(), claimable, "{status:?}");
        }
        let mut state = state;
        state.runs.get_mut(&id).unwrap().status = RunStatus::Failed;
        state
            .context_transfers
            .get_mut("transfer")
            .unwrap()
            .target_thread_id = "foreign".into();
        assert!(state.pending_fork().is_none());
        state
            .context_transfers
            .get_mut("transfer")
            .unwrap()
            .target_thread_id = "target".into();
        state.context_transfers.get_mut("transfer").unwrap().status = TransferStatus::Consumed;
        assert!(state.pending_fork().is_none());
    }

    #[test]
    fn a_queued_input_claims_undelivered_fork_context_when_activated() {
        let (state, first) = undelivered_fork();
        let mut state = accepted(&state, 101, Admission::Queue);
        let queued = state.next_queued_run().unwrap().id.clone();
        state.runs.get_mut(&first).unwrap().status = RunStatus::Failed;
        let activated =
            state.apply(&activate_queued(&state, &queued, context("resume-fork", &state)).unwrap());
        assert_eq!(
            activated.context_transfers["transfer"]
                .target_run_id
                .as_ref(),
            Some(&queued)
        );
        assert_eq!(
            activated.context_transfers["transfer"].status,
            TransferStatus::Pending
        );
    }
    fn fork_source_fixture() -> (Thread, Run, ContextSourcePoint) {
        let state = accepted(&initial(), 1, Admission::Queue);
        let run = state.runs.into_values().next().unwrap();
        let mut thread = state.thread;
        thread.title = "Source".into();
        let point = ContextSourcePoint {
            thread_id: thread.id.clone(),
            run_id: Some(run.id.clone()),
            checkpoint_id: None,
            turn_item_id: None,
            provider_thread_ref: None,
            provider_turn_ref: None,
        };
        (thread, run, point)
    }
    // ThreadForkService.ts36-45: waiting is provider-finished/checkpoint pending.
    #[test]
    fn only_finished_source_runs_can_be_forked() {
        for (status, allowed) in [
            (RunStatus::Preparing, false),
            (RunStatus::Queued, false),
            (RunStatus::Starting, false),
            (RunStatus::Running, false),
            (RunStatus::RolledBack, false),
            (RunStatus::Waiting, true),
            (RunStatus::Completed, true),
            (RunStatus::Failed, true),
            (RunStatus::Interrupted, true),
            (RunStatus::Cancelled, true),
        ] {
            let (thread, mut run, point) = fork_source_fixture();
            run.status = status;
            let fork = fork_thread(&thread, &run, point.clone(), "target", "transfer", NOW);
            assert_eq!(fork.is_ok(), allowed);
            if let Ok((target, transfer)) = fork {
                assert_eq!(target.lineage.parent_thread_id.as_ref(), Some(&thread.id));
                assert_eq!(
                    target.lineage.relationship_to_parent.as_deref(),
                    Some("fork")
                );
                assert_eq!(transfer.source_point, point);
                assert_eq!(transfer.status, TransferStatus::Pending);
                assert!(target.active_provider_thread_id.is_none());
                assert!(transfer.target_run_id.is_none());
                assert!(
                    transfer.error.is_some(),
                    "portable context is required without a strong native source"
                );
            }
        }
    }

    #[test]
    fn an_invalid_source_cannot_create_a_fork() {
        for change in [
            "same-thread",
            "foreign-run",
            "foreign-point",
            "wrong-run",
            "deleted",
        ] {
            let (mut thread, mut run, mut point) = fork_source_fixture();
            run.status = RunStatus::Completed;
            let target = if change == "same-thread" {
                thread.id.clone()
            } else {
                "target".into()
            };
            match change {
                "foreign-run" => run.thread_id = "other".into(),
                "foreign-point" => point.thread_id = "other".into(),
                "wrong-run" => point.run_id = Some("other".into()),
                "deleted" => thread.deleted_at = Some(NOW.into()),
                _ => {}
            }
            assert!(fork_thread(&thread, &run, point, &target, "transfer", NOW).is_err());
        }
    }

    #[test]
    fn a_strong_native_source_needs_no_portable_context_warning() {
        for strength in [NativeRefStrength::Strong, NativeRefStrength::Weak] {
            let (thread, mut run, mut point) = fork_source_fixture();
            run.status = RunStatus::Completed;
            point.provider_thread_ref = Some(ProviderRef {
                driver: "codex".into(),
                native_id: Some("native".into()),
                strength,
                fingerprint: None,
                ordinal: None,
            });
            let (_, transfer) =
                fork_thread(&thread, &run, point, "target", "transfer", NOW).unwrap();
            assert_eq!(
                transfer.error.is_none(),
                strength == NativeRefStrength::Strong
            );
        }
    }

    #[test]
    fn a_fork_starts_its_own_lifecycle_and_keeps_the_source_provenance() {
        let (mut source, mut run, point) = fork_source_fixture();
        run.status = RunStatus::Completed;
        source.created_by = Actor::System;
        source.creation_source = CreationSource::Provider;
        source.active_provider_thread_id = Some("parent-native-thread".into());
        source.created_at = "2026-10-01T00:00:00.000Z".into();
        source.updated_at = source.created_at.clone();
        source.archived_at = Some(source.created_at.clone());
        source.settled_override = Some("settled".into());
        source.settled_at = Some(source.created_at.clone());
        source.last_visited_at = Some(source.created_at.clone());
        let (target, transfer) =
            fork_thread(&source, &run, point, "target", "transfer", NOW).unwrap();
        assert_eq!(target.id, "target");
        assert_eq!(target.title, "Source fork");
        assert_eq!(target.created_by, Actor::User);
        assert_eq!(target.creation_source, CreationSource::Mobile);
        assert_eq!(target.created_at, NOW);
        assert_eq!(target.updated_at, NOW);
        assert!(target.archived_at.is_none());
        assert!(target.settled_override.is_none());
        assert!(target.settled_at.is_none());
        assert!(target.last_visited_at.is_none());
        assert!(target.active_provider_thread_id.is_none());
        assert!(target.deleted_at.is_none());
        assert_eq!(
            target.forked_from,
            Some(ThreadForkSource::Run {
                thread_id: source.id.clone(),
                run_id: run.id.clone()
            })
        );
        assert_eq!(target.model_selection, source.model_selection);
        assert_eq!(target.worktree_path, source.worktree_path);
        assert_eq!(transfer.target_thread_id, target.id);
        assert_eq!(transfer.source_thread_id, source.id);
    }
}

mod handoff_tests {
    use super::super::handoffs::*;
    use super::*;
    fn text(id: &str, role: &str, body: &str) -> HistoricalText {
        HistoricalText {
            message: HistoricalMessage {
                role: role.into(),
                run_status: Some("completed".into()),
                thread_id: "source".into(),
                run_id: Some("run".into()),
                item_id: id.into(),
                provider_thread_id: Some("native".into()),
                status: "completed".into(),
                kind: if role == "user" {
                    "user_message"
                } else {
                    "assistant_message"
                }
                .into(),
            },
            text: body.into(),
        }
    }
    #[test]
    fn intact_history_keeps_priority_order_and_omitted_source_ids() {
        let messages = [
            text("first", "user", "original \"request\" 🦀"),
            text("large", "assistant", &"x".repeat(40_000)),
            text("latest-user", "user", "newest question"),
            text("latest-assistant", "assistant", "newest answer"),
        ];
        let selected = select_history(&messages, "source reader", 0, &[], 2_000).unwrap();
        assert_eq!(
            selected
                .messages
                .iter()
                .map(|message| message.message.item_id.as_str())
                .collect::<Vec<_>>(),
            ["first", "latest-user", "latest-assistant"]
        );
        assert_eq!(selected.omitted_item_ids, ["large"]);
        assert_eq!(selected.omitted_items, 1);
        assert!(
            selected
                .context
                .contains("Omitted item IDs, newest last: large.")
        );
        assert!(history_cost(&selected.messages, &selected.context) <= 2_000);
        let rendered = render_history(&selected.messages, &selected.context);
        assert!(rendered.contains("[Historical user; user_message; thread=source; run=run; item=first; provider-thread=native; status=completed; run-status=completed]"));
        assert!(rendered.contains("original \"request\" 🦀"));
    }
    #[test]
    fn native_context_budget_charges_input_attachments_and_output_reserve() {
        let image = serde_json::json!({"type":"image"});
        assert_eq!(handoff_budget("x", &[], 0, None), 16_000);
        assert_eq!(handoff_budget("x", &[], 86_000, Some(128_000)), 9_997);
        assert_eq!(handoff_budget("x", &[image], 86_000, Some(128_000)), 1_805);
        assert_eq!(handoff_budget("x", &[], 128_000, Some(128_000)), 0);
    }
    #[test]
    fn history_selection_matches_pinned_t3_reference_outputs() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../scripts/t3-port-check/handoff-fixtures.json"
        )))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let input = &case["input"];
            let messages: Vec<_> = input["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| HistoricalText {
                    message: serde_json::from_value(value.clone()).unwrap(),
                    text: value["text"].as_str().unwrap().into(),
                })
                .collect();
            let prior: Vec<String> = input
                .get("omittedItemIds")
                .map(|value| serde_json::from_value(value.clone()).unwrap())
                .unwrap_or_default();
            let budget = input["budget"].as_u64().unwrap() as usize;
            let selected = select_history(
                &messages,
                input["coverage"].as_str().unwrap(),
                input
                    .get("omittedItems")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize,
                &prior,
                budget,
            );
            if case["cost"].as_u64().unwrap() > budget as u64 {
                assert!(
                    selected.is_err(),
                    "{} must fail before delivery",
                    case["name"]
                );
                continue;
            }
            let selected = selected.unwrap_or_else(|error| panic!("{}: {error}", case["name"]));
            let actual = serde_json::json!({"messages":selected.messages,"context":selected.context,"omittedItemIds":selected.omitted_item_ids,"omittedItems":selected.omitted_items});
            assert_eq!(actual, case["expected"], "reference case {}", case["name"]);
        }
    }

    #[test]
    fn portable_fork_resolution_rebinds_only_the_target_and_preserves_its_source() {
        let old = "2026-11-13T00:00:00.000Z";
        let original:ContextTransfer=serde_json::from_value(serde_json::json!({"id":"transfer","type":"fork","sourceThreadId":"source","targetThreadId":"target","sourcePoint":{"threadId":"source","runId":"source-run"},"basePoint":null,"sourceProviderInstanceId":"codex","targetProviderInstanceId":"codex","targetRunId":"failed-run","status":"pending","resolution":null,"createdBy":"user","error":"weak native source","createdAt":old,"updatedAt":old,"consumedAt":null})).unwrap();
        let handoff:ContextHandoff=serde_json::from_value(serde_json::json!({"id":"handoff","transferId":"transfer","threadId":"target","targetRunId":"retry-run","fromProviderThreadIds":["source-provider-thread"],"toProviderThreadId":"target-provider-thread","coveredRunOrdinals":{"from":1,"to":1},"strategy":"full_thread_summary","status":"ready","summaryMessageId":null,"detailInTurnItem":true,"createdByProviderInstanceId":null,"createdAt":NOW,"updatedAt":NOW})).unwrap();
        let change = provider_transfer(
            &handoff,
            original.source_point.clone(),
            None,
            Some("codex".into()),
            "claude",
            Actor::Agent,
            Some(&original),
            false,
            NOW,
        )
        .unwrap();
        let Change::ContextTransfer(updated) = change else {
            panic!("existing fork transfer is updated");
        };
        assert_eq!(updated.id, original.id);
        assert_eq!(updated.source_point, original.source_point);
        assert_eq!(
            updated.source_provider_instance_id,
            original.source_provider_instance_id
        );
        assert_eq!(updated.created_at, old);
        assert_eq!(updated.created_by, Actor::User);
        assert_eq!(updated.target_run_id.as_deref(), Some("retry-run"));
        assert_eq!(
            updated.target_provider_instance_id.as_deref(),
            Some("claude")
        );
        assert_eq!(updated.status, TransferStatus::ResolvedPortable);
        assert_eq!(
            updated.resolution,
            Some(ContextTransferResolution::PortableContext {
                context_handoff_id: "handoff".into()
            })
        );
        assert!(updated.error.is_none() && updated.consumed_at.is_none());
        assert_eq!(updated.updated_at, NOW);
        assert_eq!(original.target_run_id.as_deref(), Some("failed-run"));
    }

    proptest::proptest! {
        #[test]
        fn selected_history_fits_serialized_native_and_inline_payloads(data in proptest::collection::vec((proptest::prelude::any::<bool>(),".{0,300}"),0..20),budget in 1500usize..12_000) {
            let messages:Vec<_>=data.iter().enumerate().map(|(index,(user,body))|text(&index.to_string(),if *user {"user"} else {"assistant"},body)).collect();
            let selected=select_history(&messages,"reader",0,&[],budget).unwrap();
            proptest::prop_assert!(history_cost(&selected.messages,&selected.context)<=budget);
            proptest::prop_assert_eq!(selected.messages.len()+selected.omitted_items,messages.len());
            let mut source=messages.iter();
            for message in selected.messages {
                proptest::prop_assert!(source.any(|original| original.message.item_id==message.message.item_id && original.text==message.text));
            }
        }
    }
}

#[test]
fn context_transfer_types_follow_t3_contract_instead_of_id_allocator_tags() {
    // orchestrationV2.ts179-185 defines five types. The allocator's purpose
    // string provider_resume_fallback is only part of an ID, never a row type.
    for kind in [
        "fork",
        "provider_handoff",
        "merge_back",
        "subagent_spawn",
        "subagent_result",
    ] {
        let value = serde_json::Value::String(kind.into());
        let kind: TransferType = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(kind).unwrap(), value);
    }
    assert!(
        serde_json::from_value::<TransferType>(serde_json::json!("provider_resume_fallback"))
            .is_err()
    );
}

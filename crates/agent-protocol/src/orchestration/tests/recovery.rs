use super::*;

#[test]
fn explicit_retry_keeps_one_original_input_and_requires_proof_that_native_input_was_not_sent() {
    let accepted = accepted(&initial(), 1, Admission::Queue);
    let run = accepted.active_run().unwrap();
    let run_id = run.id.clone();
    let attempt = run.active_attempt_id.clone().unwrap();
    let root = accepted.run_attempts[&attempt].root_node_id.clone();
    let error = ProviderFailure {
        class: "provider".into(),
        message: "native source is busy".into(),
        code: Some("provider_switch_busy".into()),
        retryable: Some(true),
        reset_at: None,
    };
    let mut failed = accepted.apply(
        &fail_start(
            &accepted,
            &attempt,
            error.clone(),
            context("failed-start", &accepted),
        )
        .unwrap(),
    );
    let effect = failed.effects.values_mut().next().unwrap();
    effect.status = EffectStatus::Failed;
    effect.attempt_count = 5;
    effect.last_error = Some("native source is busy".into());
    let effect_id = effect.id.clone();
    let mut other_input = input(2);
    other_input.workspace_preparation = Some(WorkspacePreparation::Worktree {
        base_ref: "main".into(),
        start_from_origin: false,
        branch: None,
    });
    let next = failed.apply(
        &admit(
            &failed,
            other_input,
            Admission::Queue,
            context("other-input", &failed),
        )
        .unwrap(),
    );
    let other_run = next.active_run().unwrap().clone();
    failed = next.apply(
        &preparation_fail(&next, &other_run.id, error, context("other-failure", &next)).unwrap(),
    );
    for effect in failed.effects.values_mut() {
        effect.status = EffectStatus::Failed;
    }
    let other_run = failed.runs[&other_run.id].clone();
    let original_input=failed.turn_items.values().find(|item|matches!(&item.body,TurnItemBody::UserMessage {message_id,..} if message_id==&run.user_message_id)).unwrap().clone();
    let other_items: Vec<_> = failed
        .turn_items
        .values()
        .filter(|item| item.run_id.as_ref() == Some(&other_run.id))
        .cloned()
        .collect();
    let retry_context = Context {
        now: "2026-11-14T00:01:00.000Z",
        ..context("manual-retry", &failed)
    };
    let decision = retry_input(&failed, &run_id, false, retry_context).unwrap();
    let retry = failed.apply(&decision);
    assert_eq!(retry.runs.len(), 2);
    assert_eq!(retry.run_attempts.len(), 2);
    assert_eq!(retry.runs[&run_id].status, RunStatus::Starting);
    assert_eq!(
        retry.run_attempts[&attempt].status,
        RunAttemptStatus::Pending
    );
    assert_eq!(retry.nodes[&root].status, NodeStatus::Pending);
    assert_eq!(
        retry
            .turn_items
            .values()
            .filter(|item| matches!(item.body, TurnItemBody::UserMessage { .. }))
            .count(),
        2
    );
    assert_eq!(retry.turn_items[&original_input.id], original_input);
    assert_eq!(retry.runs[&other_run.id], other_run);
    for item in &other_items {
        assert_eq!(
            &retry.turn_items[&item.id], item,
            "another input's failure stays intact"
        );
    }
    assert!(
        retry
            .turn_items
            .values()
            .filter(|item| item.run_id.as_ref() == Some(&run_id)
                && matches!(item.body, TurnItemBody::Error { .. }))
            .all(|item| item.status == TurnItemStatus::Cancelled
                && item.updated_at == "2026-11-14T00:01:00.000Z")
    );
    assert_eq!(decision.effects.len(), 1);
    assert_ne!(
        decision.effects[0].id, effect_id,
        "the explicit retry owns a new outbox operation"
    );
    assert_eq!(
        retry.effects[&effect_id].status,
        EffectStatus::Failed,
        "the original failure remains recorded"
    );
    assert_eq!(decision.effects[0].status, EffectStatus::Pending);
    assert_eq!(decision.effects[0].attempt_count, 0);
    assert!(retry.provider_turns.is_empty());
    for invalid in [
        "archived",
        "deleted",
        "unknown-write",
        "delivered-pointer",
        "delivered-record",
        "no-failed-effect",
        "no-failure",
        "no-root",
        "active-run",
        "native-work",
        "foreign-failure",
    ] {
        let mut state = failed.clone();
        match invalid {
            "archived" => state.thread.archived_at = Some(NOW.into()),
            "deleted" => state.thread.deleted_at = Some(NOW.into()),
            "unknown-write" => {
                state.effects.get_mut(&effect_id).unwrap().status = EffectStatus::Running
            }
            "delivered-pointer" => {
                state
                    .run_attempts
                    .get_mut(&attempt)
                    .unwrap()
                    .provider_turn_id = Some("actual-native-turn".into())
            }
            "delivered-record" => {
                let (running, _, _) = running();
                let mut turn = running.provider_turns.values().next().unwrap().clone();
                turn.run_attempt_id = Some(attempt.clone());
                state.provider_turns.insert(turn.id.clone(), turn);
            }
            "no-failed-effect" => {
                state.effects.get_mut(&effect_id).unwrap().status = EffectStatus::Succeeded
            }
            "no-failure" => state
                .turn_items
                .retain(|_, item| !matches!(item.body, TurnItemBody::Error { .. })),
            "no-root" => state.nodes.clear(),
            "native-work" => {
                state
                    .provider_threads
                    .get_mut("native-thread")
                    .unwrap()
                    .status = ProviderThreadStatus::Active
            }
            "foreign-failure" => state.turn_items.retain(|_, item| {
                item.run_id.as_ref() != Some(&run_id)
                    || !matches!(item.body, TurnItemBody::Error { .. })
            }),
            "active-run" => {
                state = state.apply(
                    &retry_input(&state, &other_run.id, false, context("other-retry", &state))
                        .unwrap(),
                );
            }
            _ => unreachable!(),
        }
        assert!(
            retry_input(&state, &run_id, false, context("manual-retry", &state)).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn a_prepared_continuation_can_recover_again_without_repeating_the_original_input() {
    let (state, original, _) = running();
    let recovered =
        state.apply(&runtime_restarted(&state, true, context("restart", &state)).unwrap());
    let prepared = recovered.apply(
        &continue_restarted(
            &recovered,
            &original,
            "continue",
            context("continue", &recovered),
        )
        .unwrap(),
    );
    let run = prepared.active_run().unwrap();
    let continuation = run.id.clone();
    assert_eq!(
        run.work_started_at, state.runs[&original].work_started_at,
        "T3 wakeWorkStartedAt carries the accepted work's start time into a prepared continuation"
    );
    let second_restart =
        runtime_restarted(&prepared, true, context("restart-again", &prepared)).unwrap();
    assert!(second_restart.effects.iter().any(|effect| matches!(&effect.request, EffectRequest::ProviderRuntimeContinue {source_run_id} if source_run_id == &continuation)));
    let interrupted = prepared.apply(&second_restart);
    let resumed = interrupted.apply(
        &continue_restarted(
            &interrupted,
            &continuation,
            "continue",
            context("continue-again", &interrupted),
        )
        .unwrap(),
    );
    assert_eq!(
        resumed
            .active_run()
            .unwrap()
            .restart_continuation_of_run_id
            .as_ref(),
        Some(&continuation)
    );
    assert_eq!(resumed.turn_items.values().filter(|item| matches!(&item.body, TurnItemBody::UserMessage {text,..} if text == "input 1")).count(), 1);
}

#[test]
fn a_restart_expires_live_approval_without_continuing_or_reopening_it() {
    let (state, _, request) = approval();
    let decision = runtime_restarted(&state, true, context("restart", &state)).unwrap();
    assert!(decision.effects.iter().all(|effect| !matches!(
        effect.request,
        EffectRequest::ProviderRuntimeContinue { .. }
    )));
    let recovered = state.apply(&decision);
    assert_eq!(
        recovered.runtime_requests[&request].status,
        RequestStatus::Expired
    );
    assert!(matches!(
        recovered.runtime_requests[&request].response_capability,
        ResponseCapability::NotResumable { .. }
    ));
    assert!(
        respond(
            &recovered,
            &request,
            RequestResponse::Approval {
                decision: ApprovalDecision::Accept,
                feedback: None
            },
            "codex",
            context("late-approval", &recovered)
        )
        .is_err()
    );
}

#[test]
fn restart_preserves_native_identity_and_admits_one_continuation_without_replaying_input() {
    let (state, source, _) = running();
    let original =
        state.turn_items[&user_item_id("thread", &state.runs[&source].user_message_id)].clone();
    let native = state.provider_threads["native-thread"]
        .native_thread_ref
        .clone();
    let recovered =
        state.apply(&runtime_restarted(&state, true, context("restart", &state)).unwrap());
    assert_eq!(recovered.runs[&source].status, RunStatus::Cancelled);
    assert_eq!(
        recovered.provider_sessions["session"].status,
        ProviderSessionStatus::Stopped
    );
    assert_eq!(
        recovered.provider_threads["native-thread"].native_thread_ref,
        native
    );
    let continuations: Vec<_> = recovered
        .effects
        .values()
        .filter(|effect| {
            matches!(
                effect.request,
                EffectRequest::ProviderRuntimeContinue { .. }
            )
        })
        .collect();
    assert_eq!(continuations.len(), 1);
    assert_eq!(
        continuations[0].id,
        format!("effect:restart-continuation:{source}")
    );
    let admitted = recovered.apply(
        &continue_restarted(
            &recovered,
            &source,
            "continue-current-work",
            context("continue", &recovered),
        )
        .unwrap(),
    );
    let run = admitted.active_run().unwrap();
    assert_eq!(run.restart_continuation_of_run_id.as_ref(), Some(&source));
    assert_eq!(run.provider_thread_id.as_deref(), Some("native-thread"));
    assert_eq!(run.model_selection, state.runs[&source].model_selection);
    assert_eq!(admitted.turn_items[&original.id].body, original.body);
    assert!(
        matches!(&admitted.turn_items[&user_item_id("thread", &run.user_message_id)].body,
        TurnItemBody::UserMessage {text,created_by:Actor::Agent,creation_source:CreationSource::Server,attachments,..}
        if text == "continue-current-work" && attachments.is_empty())
    );
    assert!(
        continue_restarted(
            &admitted,
            &source,
            "duplicate",
            context("duplicate", &admitted)
        )
        .unwrap()
        .changes
        .is_empty()
    );
    assert!(
        runtime_restarted(&recovered, true, context("repeat-recovery", &recovered))
            .unwrap()
            .effects
            .is_empty()
    );
}

#[test]
fn a_restart_keeps_unknown_native_input_blocked_without_an_automatic_retry() {
    let mut state = accepted(&initial(), 1, Admission::Queue);
    let effect = state.effects.values_mut().next().unwrap();
    effect.status = EffectStatus::Running;
    effect.lease_owner = None;
    effect.last_error = Some("acknowledgement lost".into());
    let effect_id = effect.id.clone();
    let run = state.active_run().unwrap().id.clone();
    let recovered =
        state.apply(&runtime_restarted(&state, true, context("restart", &state)).unwrap());
    assert_eq!(recovered.runs[&run].status, RunStatus::Starting);
    assert_eq!(recovered.effects[&effect_id].status, EffectStatus::Running);
    assert!(recovered.effects.values().all(|effect| !matches!(
        effect.request,
        EffectRequest::ProviderRuntimeContinue { .. }
    )));
    assert!(
        admit(
            &recovered,
            input(2),
            Admission::Queue,
            context("new-input", &recovered)
        )
        .unwrap_err()
        .contains("uncertain input")
    );
}

#[test]
fn a_restart_requires_live_owned_work_and_respects_conversation_boundaries() {
    for condition in [
        "disabled",
        "archived",
        "deleted",
        "settled",
        "child",
        "stopped",
        "error",
        "weak",
        "no-native",
        "no-turn",
        "another-provider",
        "owned-node",
    ] {
        let (mut state, _, _) = running();
        match condition {
            "disabled" => {}
            "archived" => state.thread.archived_at = Some(NOW.into()),
            "deleted" => state.thread.deleted_at = Some(NOW.into()),
            "settled" => state.thread.settled_override = Some("settled".into()),
            "child" => state.thread.lineage.parent_thread_id = Some("parent".into()),
            "stopped" => {
                state.provider_sessions.get_mut("session").unwrap().status =
                    ProviderSessionStatus::Stopped
            }
            "error" => {
                state.provider_sessions.get_mut("session").unwrap().status =
                    ProviderSessionStatus::Error
            }
            "weak" => {
                state
                    .provider_threads
                    .get_mut("native-thread")
                    .unwrap()
                    .native_thread_ref
                    .as_mut()
                    .unwrap()
                    .strength = NativeRefStrength::Weak
            }
            "no-native" => {
                state
                    .provider_threads
                    .get_mut("native-thread")
                    .unwrap()
                    .native_thread_ref = None
            }
            "no-turn" => state.provider_turns.clear(),
            "another-provider" => state.thread.provider_instance_id = "claude".into(),
            "owned-node" => {
                state
                    .provider_threads
                    .get_mut("native-thread")
                    .unwrap()
                    .owner_node_id = Some("node".into())
            }
            _ => unreachable!(),
        }
        let decision =
            runtime_restarted(&state, condition != "disabled", context("restart", &state)).unwrap();
        assert!(
            decision.effects.iter().all(|effect| !matches!(
                effect.request,
                EffectRequest::ProviderRuntimeContinue { .. }
            )),
            "{condition}"
        );
    }
}

#[test]
fn continuations_respect_newer_input_stop_and_native_maintenance_commands() {
    let (state, source, _) = running();
    let recovered =
        state.apply(&runtime_restarted(&state, true, context("restart", &state)).unwrap());
    let newer = accepted(&recovered, 2, Admission::Queue);
    assert!(
        continue_restarted(&newer, &source, "continue", context("late", &newer))
            .unwrap()
            .changes
            .is_empty()
    );
    let stopped = state.apply(&interrupt(&state, &source, context("user-stop", &state)).unwrap());
    assert!(
        runtime_restarted(&stopped, true, context("restart", &stopped))
            .unwrap()
            .effects
            .iter()
            .all(|effect| !matches!(
                effect.request,
                EffectRequest::ProviderRuntimeContinue { .. }
            ))
    );
    for text in ["/compact", "  /LOGOUT  "] {
        let mut maintenance = state.clone();
        let item = maintenance
            .turn_items
            .get_mut(&user_item_id(
                "thread",
                &state.runs[&source].user_message_id,
            ))
            .unwrap();
        let TurnItemBody::UserMessage { text: body, .. } = &mut item.body else {
            unreachable!()
        };
        *body = text.into();
        assert!(
            runtime_restarted(&maintenance, true, context("restart", &maintenance))
                .unwrap()
                .effects
                .iter()
                .all(|effect| !matches!(
                    effect.request,
                    EffectRequest::ProviderRuntimeContinue { .. }
                ))
        );
    }
}

#[test]
fn provider_failure_preserves_other_processes_and_holds_queued_work() {
    let (mut state, source, attempt) = running();
    state = accepted(&state, 2, Admission::Queue);
    let mut foreign = state.provider_sessions["session"].clone();
    foreign.id = "foreign-session".into();
    foreign.provider_instance_id = "claude".into();
    foreign.driver = "claudeAgent".into();
    state.provider_sessions.insert(foreign.id.clone(), foreign);
    let failure = ProviderFailure {
        class: "provider_error".into(),
        message: "process ended".into(),
        code: None,
        retryable: None,
        reset_at: None,
    };
    let foreign_failed = state.apply(
        &provider_stopped(&state, "claude", &failure, context("foreign-stop", &state)).unwrap(),
    );
    assert_eq!(foreign_failed.runs, state.runs);
    assert_eq!(foreign_failed.effects, state.effects);
    assert_eq!(
        foreign_failed.provider_sessions["session"].status,
        ProviderSessionStatus::Ready
    );
    let failed = foreign_failed.apply(
        &provider_stopped(
            &foreign_failed,
            "codex",
            &failure,
            context("own-stop", &foreign_failed),
        )
        .unwrap(),
    );
    assert_eq!(failed.runs[&source].status, RunStatus::Failed);
    assert_eq!(
        failed.provider_threads["native-thread"].status,
        ProviderThreadStatus::Error
    );
    assert!(
        failed
            .runs
            .values()
            .filter(|run| run.status == RunStatus::Queued)
            .all(|run| run.queue_held)
    );
    assert!(
        complete(
            &failed,
            &attempt,
            "session",
            Completion::Completed,
            context("late", &failed)
        )
        .unwrap()
        .changes
        .is_empty()
    );
    let repeated =
        provider_stopped(&failed, "codex", &failure, context("duplicate", &failed)).unwrap();
    assert!(repeated.changes.is_empty() && repeated.effects.is_empty());
}

#[test]
fn continuation_loop_limit_uses_the_pinned_five_minute_window() {
    let (mut state, mut source, _) = running();
    for round in 0..3 {
        state = state.apply(&runtime_restarted(&state, true, context("restart", &state)).unwrap());
        let seed = format!("continue-{round}");
        state = state.apply(
            &continue_restarted(&state, &source, "continue", context(&seed, &state)).unwrap(),
        );
        source = state.active_run().unwrap().id.clone();
        state.provider_sessions.get_mut("session").unwrap().status = ProviderSessionStatus::Ready;
        let attempt = state.runs[&source].active_attempt_id.clone().unwrap();
        state = state.apply(
            &started(
                &state,
                &attempt,
                "session",
                native(&format!("turn-{round}"), None),
                context(&format!("started-{round}"), &state),
            )
            .unwrap(),
        );
    }
    state = state.apply(&runtime_restarted(&state, true, context("restart", &state)).unwrap());
    assert!(
        continue_restarted(&state, &source, "loop", context("loop", &state))
            .unwrap()
            .changes
            .is_empty()
    );
    state.runs.get_mut(&source).unwrap().requested_at = "2026-11-13T23:55:00.000Z".into();
    assert!(
        continue_restarted(&state, &source, "boundary", context("boundary", &state))
            .unwrap()
            .changes
            .is_empty()
    );
    state.runs.get_mut(&source).unwrap().requested_at = "2026-11-13T23:54:59.999Z".into();
    assert!(
        !continue_restarted(
            &state,
            &source,
            "outside-window",
            context("outside-window", &state)
        )
        .unwrap()
        .changes
        .is_empty()
    );
}

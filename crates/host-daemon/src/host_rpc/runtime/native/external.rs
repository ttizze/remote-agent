use super::*;

pub(in crate::host_rpc::runtime) fn provider<'a>(
    state: &'a model::State,
    native: &SessionRef,
) -> Option<&'a model::ProviderThread> {
    state.selected_native_thread().filter(|thread| {
        native.provider.is_some_and(|provider| {
            provider.key() == thread.provider_instance_id && provider.driver() == thread.driver
        }) && thread
            .native_thread_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_deref())
            == Some(&native.id)
    })
}

pub(in crate::host_rpc::runtime) fn context(
    state: &model::State,
    native: &SessionRef,
    turn: &agent_protocol::ids::TurnId,
    now: &str,
    completed: bool,
) -> Option<Context> {
    let provider_thread = provider(state, native)?;
    let turn = state.provider_turns.values().find(|saved| {
        saved.run_attempt_id.is_none()
            && saved.provider_thread_id == provider_thread.id
            && saved.native_thread_ref.as_ref() == provider_thread.native_thread_ref.as_ref()
            && saved
                .control_ref()
                .and_then(|reference| reference.native_id.as_deref())
                == Some(turn.as_str())
    })?;
    Some(Context {
        thread_id: state.thread.id.clone(),
        run_id: None,
        root_node_id: turn.node_id.clone(),
        provider_thread_id: provider_thread.id.clone(),
        provider_turn_id: turn.id.clone(),
        native_turn_id: turn.control_ref()?.native_id.clone()?,
        native_thread_id: native.id.clone(),
        provider: native.provider?,
        prompt_actor: state.thread.native_prompt_actor(),
        now: now.into(),
        completed,
    })
}

impl Runtime {
    pub(in crate::host_rpc::runtime) async fn external_change(
        service: &HostRpcService,
        source: &Arc<resources::Source>,
        native: &SessionRef,
        change: SessionChange,
    ) -> Result<(), String> {
        let storage = &service.inner.conversation.storage;
        let now = commands::at();
        let Some(turn_id) = change.turn_id().cloned() else {
            return Ok(());
        };
        let target = source.target.application_id();
        storage.observe_thread(&target, &[], &now, |state| {
            let Some(provider_thread) = provider(state, native) else {
                return Ok(model::Decision::default());
            };
            model::observe_external_turn(
                state,
                &provider_thread.id,
                &native.id,
                control_reference(
                    native.native_provider().map_err(str::to_owned)?,
                    turn_id.as_str(),
                ),
                model::ProviderTurnStatus::Running,
                &now,
            )
        })?;
        let state = storage.thread_state(&target, &[])?;
        let completed = matches!(
            &change,
            SessionChange::Turn {
                completed: true,
                ..
            }
        );
        let Some(context) = context(&state, native, &turn_id, &now, completed) else {
            return Ok(());
        };
        // Late items still carry results for known background work. The pure
        // item boundary checks that ownership before committing any change.
        let status = state.provider_turns[&context.provider_turn_id].status;
        if (state.active_run().is_some()
            || state.has_uncertain_input()
            || status != model::ProviderTurnStatus::Running)
            && !(status == model::ProviderTurnStatus::Completed
                && matches!(&change, SessionChange::Item { .. }))
        {
            return Ok(());
        }
        match change {
            SessionChange::Turn { turn, completed } => {
                if completed {
                    Self::flush_text(service, source)?;
                }
                for item in turn.items.iter().flatten() {
                    Self::canonical_native_item(service, source, native, None, &context, item)
                        .await?;
                }
                if completed {
                    let status = completed_turn_status(turn.status);
                    let item_ids = storage
                        .unfinished_turn_item_ids(&source.target, &context.provider_turn_id)?;
                    let tails = service
                        .inner
                        .conversation
                        .outputs
                        .checkpoints(&source.target, &context.provider_turn_id);
                    let events = storage.observe_thread(&target, &item_ids, &now, |state| {
                        let saved = &state.provider_turns[&context.provider_turn_id];
                        let mut decision = model::observe_external_turn(
                            state,
                            &context.provider_thread_id,
                            &native.id,
                            saved
                                .control_ref()
                                .cloned()
                                .ok_or("native turn reference missing")?,
                            status,
                            &now,
                        )?;
                        decision = decision.with_command_outputs(Some(&tails));
                        let next = state.apply(&decision);
                        let provider = &next.provider_threads[&context.provider_thread_id];
                        let command = Uuid::new_v4().to_string();
                        let released = model::observe_native_status(
                            &next,
                            &provider.provider_instance_id,
                            &provider.driver,
                            &native.id,
                            provider.status,
                            executor::context(&next, &command, &now),
                        )?;
                        decision.changes.extend(released.changes);
                        decision.effects.extend(released.effects);
                        Ok(decision)
                    })?;
                    Self::finish_output_events(service, &events);
                    service.inner.conversation.prune_requests(&source.target);
                }
            }
            SessionChange::Item { item, .. } => {
                Self::canonical_native_item(service, source, native, None, &context, &item).await?
            }
            SessionChange::Text {
                item_id,
                field,
                delta,
                ..
            } => {
                Self::ensure_text_item(service, source, None, &context, item_id.as_str(), field)?;
                Self::text_delta(
                    service,
                    source,
                    native,
                    super::super::text::Owner::NativeTurn {
                        id: context.provider_turn_id.clone(),
                        native_id: native.id.clone(),
                    },
                    super::super::text::Delta {
                        turn: &context.native_turn_id,
                        item: item_id.as_str(),
                        field,
                        text: &delta,
                    },
                )?;
            }
            SessionChange::Error { error, .. } => {
                let artifact = context.artifact(
                    &format!("error:{}", Uuid::new_v4()),
                    None,
                    model::TurnItemStatus::Failed,
                    model::NodeKind::System,
                    model::TurnItemBody::Error {
                        failure: model::ProviderFailure {
                            class: model::ProviderFailure::class_for(&error.category).into(),
                            message: error.message,
                            code: error.provider_code,
                            retryable: None,
                            reset_at: None,
                        },
                        retry: None,
                    },
                );
                Self::commit_artifacts(service, source, None, &native.id, vec![artifact], &now)?;
            }
            _ => {}
        }
        Ok(())
    }
}

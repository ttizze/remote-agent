use super::*;

impl Runtime {
    pub(in crate::host_rpc::runtime) async fn observe_native_cursor(
        service: &HostRpcService,
        source: &Arc<resources::Source>,
        native: &SessionRef,
        turn: &agent_protocol::ids::TurnId,
        head: &str,
        execution: Option<Uuid>,
    ) -> Result<(), String> {
        let _updates = source.updates.lock().await;
        let now = commands::at();
        let command = Uuid::new_v4().to_string();
        let provider = native.native_provider().map_err(str::to_owned)?;
        let events = service.inner.conversation.storage.observe_thread(
            &source.target.application_id(),
            &[],
            &now,
            |state| {
                let reference = control_reference(provider, turn.as_str());
                let mut decision = if let Some(execution) = execution {
                    let attempt = execution.to_string();
                    if !state.native_thread_matches(&attempt, provider.driver(), &native.id)
                        || !state.accepts_observation(&attempt, &source.session_id)
                    {
                        return Ok(model::Decision::default());
                    }
                    model::started(
                        state,
                        &attempt,
                        &source.session_id,
                        reference,
                        executor::context(state, &command, &now),
                    )?
                } else {
                    let Some(provider_thread) = external::provider(state, native) else {
                        return Ok(model::Decision::default());
                    };
                    model::observe_external_turn(
                        state,
                        &provider_thread.id,
                        &native.id,
                        reference,
                        model::ProviderTurnStatus::Running,
                        &now,
                    )?
                };
                let next = state.apply(&decision);
                let context = if let Some(execution) = execution {
                    Some(observations::native_context(
                        &next,
                        &execution.to_string(),
                        turn,
                        &now,
                        false,
                    )?)
                } else {
                    external::context(&next, native, turn, &now, false)
                };
                if let Some(context) = context {
                    let cursor = model::observe_native_cursor(
                        &next,
                        &context.provider_turn_id,
                        &native.id,
                        head,
                        &now,
                    );
                    decision.changes.extend(cursor.changes);
                    decision.effects.extend(cursor.effects);
                }
                Ok(decision)
            },
        )?;
        service.schedule_setup_events(&events);
        Ok(())
    }
}

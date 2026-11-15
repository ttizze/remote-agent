use super::*;

#[cfg(test)]
mod tests;

impl Runtime {
    /// Full native tool details are a read, not a new timeline revision. A
    /// missing transcript leaves the saved partial body explicitly partial.
    pub(super) async fn read_canonical_item(
        service: &HostRpcService,
        target: &SessionRef,
        id: &str,
    ) -> Result<Option<agent_protocol::orchestration::TurnItem>, Failure> {
        let storage = &service.inner.conversation.storage;
        let Some(mut saved) = storage.thread_item(target, id).map_err(storage_failure)? else {
            return Ok(None);
        };
        if !saved.body.has_omitted_detail() {
            return Ok(Some(saved));
        }
        let Some(raw_item) = saved
            .native_item_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_deref())
        else {
            return Ok(Some(saved));
        };
        let state = storage
            .thread_state(&saved.thread_id, &[])
            .map_err(storage_failure)?;
        let turn = saved
            .provider_turn_id
            .as_ref()
            .and_then(|id| state.provider_turns.get(id))
            .ok_or_else(|| Failure::new("incomplete_item", "native item turn is unavailable"))?;
        let provider = state
            .provider_threads
            .get(&turn.provider_thread_id)
            .ok_or_else(|| {
                Failure::new("incomplete_item", "native item provider is unavailable")
            })?;
        let attempt = turn
            .run_attempt_id
            .as_ref()
            .and_then(|id| state.run_attempts.get(id));
        let reference = turn
            .native_thread_ref
            .as_ref()
            .ok_or_else(|| Failure::new("incomplete_item", "native item source is unavailable"))?;
        let native_thread = reference
            .native_id
            .as_ref()
            .ok_or_else(|| Failure::new("incomplete_item", "native item source is unavailable"))?;
        let native_turn = turn
            .native_turn_ref
            .as_ref()
            .and_then(|reference| reference.native_id.as_ref())
            .ok_or_else(|| Failure::new("incomplete_item", "native item turn is unavailable"))?;
        let provider_kind = ProviderKind::from_instance_id(&provider.provider_instance_id)
            .map_err(storage_failure)?;
        if reference.driver != provider_kind.driver() {
            return Err(Failure::new(
                "incomplete_item",
                "native item source changed provider",
            ));
        }
        if saved
            .native_item_ref
            .as_ref()
            .is_none_or(|reference| reference.driver != provider_kind.driver())
            || turn
                .native_turn_ref
                .as_ref()
                .is_none_or(|reference| reference.driver != provider_kind.driver())
        {
            return Err(Failure::new(
                "incomplete_item",
                "native item identity changed provider",
            ));
        }
        let full = Self::read_native_body(
            service,
            &op::ReadItem {
                thread_id: SessionRef::new(provider_kind, native_thread.clone())
                    .map_err(|error| storage_failure(error.into()))?,
                turn_id: native_turn.clone().into(),
                item_id: raw_item.into(),
            },
        )
        .await?;
        let context = super::native::Context {
            thread_id: saved.thread_id.clone(),
            run_id: saved.run_id.clone(),
            root_node_id: attempt.map_or_else(
                || turn.node_id.clone(),
                |attempt| attempt.root_node_id.clone(),
            ),
            provider_thread_id: provider.id.clone(),
            provider_turn_id: turn.id.clone(),
            native_turn_id: native_turn.clone(),
            native_thread_id: native_thread.clone(),
            provider: provider_kind,
            now: saved.updated_at.clone(),
            completed: saved.status.is_terminal(),
        };
        let item = context
            .item(&full)
            .map_err(storage_failure)?
            .into_iter()
            .find(|artifact| artifact.item.id == saved.id)
            .ok_or_else(|| Failure::new("incomplete_item", "native item identity changed"))?;
        if std::mem::discriminant(&item.item.body) != std::mem::discriminant(&saved.body) {
            return Err(Failure::new("incomplete_item", "native item kind changed"));
        }
        saved.body = item.item.body;
        Ok(Some(saved))
    }

    pub(super) async fn read_native_body(
        service: &HostRpcService,
        request: &op::ReadItem,
    ) -> Result<agent_protocol::items::Item, Failure> {
        let provider = request
            .thread_id
            .native_provider()
            .map_err(|error| Failure::new("invalid_session", error))?;
        let detail = service
            .inner
            .providers
            .discovery(provider)?
            .read_item(request)
            .await?;
        if detail.id != request.item_id || detail.is_deferred() {
            return Err(Failure::new(
                "incomplete_item",
                "native item body is unavailable",
            ));
        }
        Ok(detail)
    }
}

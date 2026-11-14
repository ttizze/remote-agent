use super::*;

#[cfg(test)]
mod tests;

impl Runtime {
    /// Return hydrated history and whether any required message body is still missing.
    pub(in crate::host_rpc) async fn handoff_history(
        service: &HostRpcService,
        target: &SessionRef,
        covered: u64,
    ) -> Result<(agent_protocol::models::ThreadResponse, bool), Failure> {
        let storage = &service.inner.conversation.storage;
        let history = storage
            .history(target, Some(covered))
            .map_err(storage_failure)?;
        for turn in history.thread.turns.iter().flatten() {
            for item in turn.items.iter().flatten().filter(|item| {
                item.is_deferred()
                    && agent_protocol::handoff::Message::from_item(turn, item).is_some()
            }) {
                if let Err(error) = Self::hydrate_native(service, target, &item.id).await {
                    tracing::warn!(target:"agent",operation="conversation.handoff.body",%error);
                }
            }
        }
        let mut history = storage
            .history(target, Some(covered))
            .map_err(storage_failure)?;
        let missing_message = history.thread.turns.iter().flatten().any(|turn| {
            turn.items.iter().flatten().any(|item| {
                item.is_deferred()
                    && agent_protocol::handoff::Message::from_item(turn, item).is_some()
            })
        });
        if missing_message {
            use agent_protocol::session::{HistoryReadKind, HistoryReadState};
            let state = history
                .thread
                .history_read_state
                .get_or_insert_with(|| HistoryReadState::new(HistoryReadKind::Partial, vec![]));
            if state.kind == HistoryReadKind::Complete {
                state.kind = HistoryReadKind::Partial;
            }
            let issue = "一部のメッセージ本文を読み込めませんでした。".to_owned();
            if !state.issues.contains(&issue) {
                state.issues.push(issue);
            }
        }
        Ok((history, missing_message))
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

    pub(super) async fn hydrate_native(
        service: &HostRpcService,
        target: &SessionRef,
        id: &agent_protocol::ids::ItemId,
    ) -> Result<(), Failure> {
        let runtime = &service.inner.conversation;
        if let Some(native) = runtime
            .storage
            .native_body(target, id)
            .map_err(storage_failure)?
        {
            let item = Self::read_native_body(service, &native.request).await?;
            runtime
                .owners
                .write(
                    target,
                    Write::Hydrate {
                        id: id.clone(),
                        version: native.version,
                        body: item.body().clone(),
                    },
                )
                .await
                .map_err(storage_failure)?;
        }
        Ok(())
    }
}

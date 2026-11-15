use super::*;

#[cfg(test)]
mod tests;

impl Runtime {
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

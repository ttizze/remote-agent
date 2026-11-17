use super::*;
use agent_protocol::orchestration as model;

impl Runtime {
    pub(super) async fn generate_title(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        message_id: &str,
    ) -> Result<(), String> {
        let storage = &service.inner.conversation.storage;
        let Some(state) = storage.effect_state(effect, worker)? else {
            return Ok(());
        };
        if state.thread.archived_at.is_some() || state.thread.deleted_at.is_some() {
            return Ok(());
        }
        let item_id = model::user_item_id(&effect.thread_id, message_id);
        let Some(model::TurnItem {
            body:
                model::TurnItemBody::UserMessage {
                    text, attachments, ..
                },
            ..
        }) = state.turn_items.get(&item_id)
        else {
            return Ok(());
        };
        let prompt = model::titles::prompt(text, attachments);
        let preferred = ProviderKind::from_instance_id(&state.thread.provider_instance_id)?;
        let mut providers = service.inner.providers.all();
        providers.sort_by_key(|(provider, _)| (*provider != preferred, *provider));
        for (_, provider) in providers {
            let Some(title) = provider
                .generate_title(&prompt)
                .await
                .map_err(|error| error.to_string())?
            else {
                continue;
            };
            let now = commands::at();
            storage.observe_effect(effect, worker, &now, |state| {
                Ok(model::titles::complete(state, &effect.id, &title, &now))
            })?;
            break;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

//! The provider confirms the native cut before the canonical write. The outbox
//! lease fences publication; source I/O serializes native resource operations.
use super::*;
use agent_protocol::orchestration as model;

impl Runtime {
    pub(super) async fn rollback_thread(
        service: &HostRpcService,
        effect: &model::Effect,
        worker: &str,
        provider_thread_id: &str,
        checkpoint_id: &str,
        scope_id: &str,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let Some(initial) = runtime.storage.effect_state(effect, worker)? else {
            return Ok(());
        };
        let provider_thread = initial
            .provider_threads
            .get(provider_thread_id)
            .ok_or("rewind provider thread is unavailable")?;
        let provider = ProviderKind::from_instance_id(&provider_thread.provider_instance_id)?;
        let native = provider_thread
            .native_thread_ref
            .as_ref()
            .filter(|reference| {
                reference.driver == provider.driver() && provider_thread.driver == provider.driver()
            })
            .and_then(|reference| reference.native_id.as_ref())
            .filter(|id| !id.is_empty())
            .ok_or("rewind native resource is unavailable")?;
        let native = SessionRef::new(provider, native.clone()).map_err(str::to_owned)?;
        let public = SessionRef::new(None, initial.thread.id.clone()).map_err(str::to_owned)?;
        let _workspace = service.inner.worktree_access.read().await;
        let _retire = scopeguard::guard(native.clone(), |native| runtime.retire_source(&native));
        let source = runtime.register_native(&public, &native);
        let _io = source.io.lock().await;
        let Some(state) = runtime.storage.effect_state(effect, worker)? else {
            return Ok(());
        };
        if state
            .provider_threads
            .get(provider_thread_id)
            .and_then(|thread| thread.native_thread_ref.as_ref())
            != provider_thread.native_thread_ref.as_ref()
        {
            return Err("rewind native source changed".into());
        }
        let target = model::rollback_target(&state, provider_thread_id, checkpoint_id, scope_id)?;
        let cwd = state
            .thread
            .worktree_path
            .as_deref()
            .ok_or("rewind workspace is unavailable")?;
        let restored = service
            .inner
            .providers
            .get(provider)
            .map_err(|error| error.to_string())?
            .rollback(
                &native.id,
                target
                    .retained_turn
                    .as_ref()
                    .and_then(|reference| reference.native_id.as_deref()),
                target
                    .first_removed_turn
                    .as_ref()
                    .and_then(|reference| reference.native_id.as_deref()),
                cwd,
                service
                    .browser_config(&public.to_string())
                    .map_err(|error| error.to_string())?,
            )
            .await
            .map_err(|error| error.to_string())?;
        let native = super::executor::native_ref(provider, &restored.native_id);
        let head = restored
            .head_id
            .as_deref()
            .map(|id| super::executor::native_ref(provider, id));
        let now = commands::at();
        runtime
            .storage
            .observe_effect(effect, worker, &now, |state| {
                model::rollback_completed(
                    state,
                    &effect.id,
                    native,
                    head,
                    super::executor::context(state, &effect.id, &now),
                )
            })?;
        Ok(())
    }
}

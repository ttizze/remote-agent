use super::*;

pub(in crate::host_rpc) const CONTINUE: &str = "Bexの更新に伴う再起動で作業が中断されました。この同じ会話の履歴から進捗と未完了の作業を確認し、中断したところから続けてください。元の依頼を最初からやり直したり、代わりの会話を作成したりしないでください。";

impl Runtime {
    pub(super) async fn stopped(
        service: &HostRpcService,
        provider: ProviderKind,
        error: &agent_protocol::execution::ExecutionError,
    ) -> Result<(), String> {
        let runtime = &service.inner.conversation;
        let _requests = scopeguard::guard(provider, |provider| {
            runtime
                .requests
                .lock()
                .unwrap()
                .retain(|_, request| request.native.provider != Some(provider));
        });
        let mut sources: Vec<_> = runtime
            .resources
            .lock()
            .unwrap()
            .iter()
            .filter(|(native, _)| native.provider == Some(provider))
            .map(|(native, source)| (native.clone(), source.clone()))
            .collect();
        // Stop can arrive while a source still has buffered deltas. Drain them
        // before settling the run; terminal attempts reject later observations.
        sources.sort_by(|(left, _), (right, _)| left.id.cmp(&right.id));
        let mut updates = Vec::with_capacity(sources.len());
        for (_, source) in &sources {
            updates.push(source.updates.lock().await);
            Self::flush_text(service, source)?;
        }
        // A source can retire after its last callback while its command still
        // has live output. Output ownership outlives that I/O resource.
        let tails = runtime.outputs.checkpoints_by_thread();
        let events = runtime.storage.provider_stopped(
            provider.key(),
            &agent_protocol::orchestration::ProviderFailure {
                class: agent_protocol::orchestration::ProviderFailure::class_for(&error.category)
                    .into(),
                message: error.message.clone(),
                code: error.provider_code.clone(),
                retryable: None,
                reset_at: None,
            },
            &tails,
            &commands::at(),
        )?;
        Self::finish_output_events(service, &events);
        drop(updates);
        runtime
            .resources
            .lock()
            .unwrap()
            .retain(|native, _| native.provider != Some(provider));
        Ok(())
    }
}

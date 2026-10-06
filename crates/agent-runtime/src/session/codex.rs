use super::*;

impl SessionManager {
    pub(super) async fn start_codex(
        &self,
        target: &LaunchTarget,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
        images: &[PreparedImage],
    ) -> Result<(), Failure> {
        let context = self
            .host
            .codex_context(target.clone())
            .await
            .map_err(ExecError::Retry)?;
        let entry = self.codex_session(target, &context).await?;
        self.settled(&entry, attempt).await?;
        if !self.still_current(&target.key.thread, attempt).await? {
            return Ok(());
        }
        self.bind(attempt, &entry);
        let (command, images) = (command.clone(), images.to_vec());
        let operation = operation(&command);
        let sent = self
            .send(
                &entry,
                Request::new(move |p| p.codex()?.command(&command, &context, &images))
                    .owner(attempt),
            )
            .await;
        settle_sent(sent, attempt, operation, None).map(|_| ())
    }

    pub(super) async fn codex_session(
        &self,
        target: &LaunchTarget,
        context: &WireContext,
    ) -> Result<Entry, Failure> {
        if let Some(entry) = self.entry(&target.key) {
            return Ok(entry);
        }
        let entry = self.spawn(target, None).await?;
        let context = context.clone();
        let initialized = self
            .request_reply(
                &entry,
                Request::new(move |p| Ok(frames(vec![p.codex()?.initialize(&context)])))
                    .handshake(),
            )
            .await;
        if let Err(message) = initialized {
            self.close_entry(&entry, false, false).await;
            return Err(ExecError::Retry(format!("Codex did not initialize: {message}")).into());
        }
        Ok(entry)
    }

    pub(super) async fn fork_codex(
        &self,
        target: &LaunchTarget,
        provider: &ProviderCommand,
    ) -> Result<String, ForkError> {
        self.keys
            .with_lock(target.key.clone(), async {
                let context = self
                    .host
                    .codex_context(target.clone())
                    .await
                    .map_err(ForkError::Retry)?;
                let entry = match self.codex_session(target, &context).await {
                    Ok(entry) => entry,
                    Err(Failure::Exec(ExecError::Retry(message))) => {
                        return Err(ForkError::Retry(message));
                    }
                    Err(Failure::Exec(ExecError::Settle(result))) => {
                        return Err(ForkError::Rejected(format!("{result:?}")));
                    }
                    Err(Failure::Gone) => {
                        return Err(ForkError::Retry("The provider session closed.".into()));
                    }
                };
                let forwarded = provider.clone();
                match self
                    .request_completion(
                        &entry,
                        Request::new(move |p| p.codex()?.command(&forwarded, &context, &[])),
                    )
                    .await
                {
                    Ok(Completion::Forked { native_thread }) => Ok(native_thread),
                    Ok(other) => Err(ForkError::Rejected(format!(
                        "Codex answered a fork with {other:?}"
                    ))),
                    Err(message) => Err(ForkError::Rejected(message)),
                }
            })
            .await
    }
}

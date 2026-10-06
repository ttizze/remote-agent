use super::*;

impl SessionManager {
    pub(super) async fn start_codex(
        &self,
        target: &LaunchTarget,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
        images: &[PreparedImage],
    ) -> Result<(), Failure> {
        let thread = &target.key.thread;
        let (entry, context) = self.codex_entry(target).await?;
        self.settled(&entry, thread, attempt).await?;
        if !self.still_current(thread, attempt).await? {
            return Ok(());
        }
        self.bind(attempt, &entry);
        let (command, images) = (command.clone(), images.to_vec());
        let operation = operation(&command);
        let sent = self
            .send(
                &entry,
                Request::new(thread, move |p| {
                    p.codex()?.command(&command, &context, &images)
                })
                .owner(attempt),
            )
            .await;
        settle_sent(sent, attempt, operation, None).map(|_| ())
    }

    /// The instance's shared app-server with the target's thread attached, and
    /// the thread's wire context on it.
    pub(super) async fn codex_entry(
        &self,
        target: &LaunchTarget,
    ) -> Result<(Entry, WireContext), Failure> {
        let mut context = self
            .host
            .codex_context(target.clone())
            .await
            .map_err(ExecError::Retry)?;
        context.route = target.key.thread.as_str().to_owned();
        let entry = self.codex_session(target, &context).await?;
        self.attach(&entry, target)?;
        Ok((entry, context))
    }

    /// Opens the instance's app-server once: initialize, then the managed account.
    async fn codex_session(
        &self,
        target: &LaunchTarget,
        context: &WireContext,
    ) -> Result<Entry, Failure> {
        let slot = Slot::Shared(target.key.instance.clone());
        self.opening
            .with_lock(slot.clone(), async {
                if let Some(entry) = self.entry(&slot) {
                    let pending = entry
                        .members
                        .lock()
                        .expect("session members")
                        .account_pending;
                    if pending {
                        self.codex_sign_in(&entry, &target.key.thread)
                            .await
                            .map_err(ExecError::Retry)?;
                        entry
                            .members
                            .lock()
                            .expect("session members")
                            .account_pending = false;
                    }
                    return Ok(entry);
                }
                let entry = self.spawn(target, None).await?;
                let thread = &target.key.thread;
                let context = context.clone();
                let initialized = self
                    .request_reply(
                        &entry,
                        Request::new(thread, move |p| {
                            Ok(frames(vec![p.codex()?.initialize(&context)]))
                        })
                        .handshake(),
                    )
                    .await;
                let signed_in = match initialized {
                    Ok(_) => self.codex_sign_in(&entry, thread).await,
                    Err(message) => Err(format!("Codex did not initialize: {message}")),
                };
                if let Err(message) = signed_in {
                    self.close_entry(&entry, false).await;
                    return Err(ExecError::Retry(message).into());
                }
                Ok(entry)
            })
            .await
    }

    /// A new app-server runs as the selected managed account.
    pub(super) async fn codex_sign_in(
        &self,
        entry: &Entry,
        thread: &ThreadId,
    ) -> Result<(), String> {
        let Some(params) = self
            .host
            .codex_account(entry.slot.instance().to_owned())
            .await?
        else {
            return Ok(());
        };
        self.request_reply(
            entry,
            Request::new(thread, move |p| Ok(frames(vec![p.codex()?.login(params)]))),
        )
        .await
        .map(|_| ())
        .map_err(|message| format!("Codex did not accept the selected account: {message}"))
    }

    pub(super) async fn fork_codex(
        &self,
        target: &LaunchTarget,
        provider: &ProviderCommand,
    ) -> Result<String, ForkError> {
        self.keys
            .with_lock(target.key.clone(), async {
                let _reserved = self.reserve(&target.key);
                let (entry, context) = match self.codex_entry(target).await {
                    Ok(opened) => opened,
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
                        Request::new(&target.key.thread, move |p| {
                            p.codex()?.command(&forwarded, &context, &[])
                        }),
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

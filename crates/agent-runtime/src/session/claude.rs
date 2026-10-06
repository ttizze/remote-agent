use super::*;

/// T3 ClaudeBackgroundWorkBlocksQueryReplacementError.
pub(crate) const BACKGROUND_BLOCKS_REPLACEMENT: &str = "Claude is still running background agents or commands, and this model or setting change would end them. Wait for them to finish, or press Stop, then send the message again.";

pub(crate) struct ClaudeProcess {
    pub(super) launch: ClaudeLaunch,
    pub(crate) native: Option<String>,
    pub(super) model: String,
    pub(super) permission_mode: String,
}

impl SessionManager {
    pub(super) async fn start_claude(
        &self,
        target: &LaunchTarget,
        state: &State,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
        prompt: String,
        images: &[PreparedImage],
    ) -> Result<(), Failure> {
        let (native, resume_at) = match command {
            ProviderCommand::Start {
                native_thread,
                resume_at,
                ..
            } => (native_thread.clone(), resume_at.clone()),
            _ => (
                command_native(command),
                state
                    .native_heads
                    .get(&target.key.instance)
                    .cloned()
                    .flatten(),
            ),
        };
        let settings = self
            .host
            .claude_settings(target.clone())
            .await
            .map_err(ExecError::Retry)?;
        let new_session = native.is_none().then(|| self.host.session_uuid(attempt));
        let launch = claude_launch(
            target,
            &settings,
            native.clone(),
            resume_at,
            new_session.clone(),
        );
        let entry = self
            .claude_session(target, state, attempt, command, &settings, launch)
            .await?;
        if !self.still_current(&target.key.thread, attempt).await? {
            return Ok(());
        }
        // The CLI writes a transcript for a new session once it has the prompt;
        // binding first keeps an import from adopting that session.
        if let Some(session) = new_session {
            self.provider_event(
                &target.key.thread,
                attempt,
                ProviderEvent::SessionReady {
                    native_thread: session,
                },
            )
            .await
            .map_err(|error| ExecError::Retry(error.to_string()))?;
        }
        self.bind(attempt, &entry);
        let (command, images, skills) = (command.clone(), images.to_vec(), settings.skills);
        let operation = operation(&command);
        let sent = self
            .send(
                &entry,
                Request::new(&target.key.thread, move |p| {
                    let claude = p.claude()?;
                    claude.set_skills(skills);
                    claude.command(&command, &prompt, &images)
                })
                .owner(attempt),
            )
            .await;
        settle_sent(sent, attempt, operation, None).map(|_| ())
    }

    /// Reuses the process when it holds the thread's native session and its launch
    /// flags still apply, after aligning model and permission mode; otherwise respawns.
    pub(super) async fn claude_session(
        &self,
        target: &LaunchTarget,
        state: &State,
        attempt: &RunAttemptId,
        command: &ProviderCommand,
        settings: &ClaudeSettings,
        launch: ClaudeLaunch,
    ) -> Result<Entry, Failure> {
        if let Some(entry) = self.entry(&Slot::Thread(target.key.clone())) {
            let reusable = entry.claude.as_ref().is_some_and(|process| {
                let process = process.lock().expect("claude process");
                launch.native_session.is_some()
                    && process.native == launch.native_session
                    && compatible(&process.launch, &launch)
            });
            if reusable {
                self.restore_mode(&entry, &target.key.thread, &launch)
                    .await
                    .map_err(ExecError::Retry)?;
                return Ok(entry);
            }
            // Background agents and shells live in the process; replacing it for the
            // same native session would end them (T3 refuses until they finish or Stop).
            let same_session = launch.native_session.is_some()
                && entry.claude.as_ref().is_some_and(|process| {
                    process.lock().expect("claude process").native == launch.native_session
                });
            if same_session
                && holds_background(
                    &*self.state(&target.key.thread).await?,
                    &self.entry_attempts(&entry),
                )
            {
                return Err(ExecError::Settle(Box::new(EffectResult::ProviderFailed {
                    attempt: attempt.clone(),
                    operation: operation(command).unwrap_or(ProviderOperation::Start),
                    message: BACKGROUND_BLOCKS_REPLACEMENT.into(),
                    message_id: None,
                    turn_completed: false,
                    session_lost: false,
                }))
                .into());
            }
            self.close_entry(&entry, true).await;
        }
        let resumed = launch.native_session.is_some();
        let entry = self.spawn(target, Some(launch)).await?;
        let skills = settings.skills.clone();
        let prompt = settings.append_system_prompt.clone();
        let initialized = self
            .request_reply(
                &entry,
                Request::new(&target.key.thread, move |p| {
                    let claude = p.claude()?;
                    claude.set_skills(skills);
                    Ok(frames(vec![claude.control.initialize(&prompt)]))
                })
                .owner(attempt)
                .handshake(),
            )
            .await;
        if let Err(message) = initialized {
            self.close_entry(&entry, false).await;
            return Err(if resumed {
                ExecError::Settle(Box::new(EffectResult::ProviderFailed {
                    attempt: attempt.clone(),
                    operation: operation(command).unwrap_or(ProviderOperation::Start),
                    message,
                    message_id: None,
                    turn_completed: false,
                    session_lost: true,
                }))
            } else {
                ExecError::Retry(format!("Claude did not initialize: {message}"))
            }
            .into());
        }
        let opened = agent_domain::Input::RuntimeOpened {
            instance: target.key.instance.clone(),
            attempt: Some(attempt.clone()),
        };
        self.input(&target.key.thread, opened)
            .await
            .map_err(|error| ExecError::Retry(error.to_string()))?;
        let routes = self.task_routes(state).await;
        self.send(
            &entry,
            Request::new(&target.key.thread, move |p| {
                let claude = p.claude()?;
                for (task, tool, parent, agent) in &routes {
                    claude.restore_task_route(task, tool, parent.as_deref(), *agent);
                }
                Ok(Translation::default())
            }),
        )
        .await
        .map_err(|error| match error {
            SessionError::Gone => Failure::Gone,
            other => ExecError::Retry(other.to_string()).into(),
        })?;
        Ok(entry)
    }

    /// T3 openQuery: Claude can switch its own mode (EnterPlanMode), so the
    /// reused process is put back in the thread's mode before the next prompt.
    pub(super) async fn restore_mode(
        &self,
        entry: &Entry,
        thread: &ThreadId,
        launch: &ClaudeLaunch,
    ) -> Result<(), String> {
        let Some(process) = &entry.claude else {
            return Ok(());
        };
        let mode = {
            let process = process.lock().expect("claude process");
            (process.permission_mode != launch.policy.permission_mode)
                .then(|| launch.policy.permission_mode.clone())
        };
        if let Some(mode) = mode {
            let payload = json!({ "mode": mode });
            self.request_reply(
                entry,
                Request::new(thread, move |p| {
                    Ok(frames(vec![
                        p.claude()?.control.request("set_permission_mode", payload),
                    ]))
                }),
            )
            .await?;
            process.lock().expect("claude process").permission_mode = mode;
        }
        Ok(())
    }

    /// Native task routes held by the root and its native descendants.
    pub(super) async fn task_routes(
        &self,
        root: &State,
    ) -> Vec<(String, String, Option<String>, bool)> {
        let mut states = vec![Arc::new(root.clone())];
        let mut seen = HashSet::new();
        let mut index = 0;
        while index < states.len() {
            let children: Vec<ThreadId> = states[index]
                .tasks
                .iter()
                .map(|task| task.child_thread.clone())
                .collect();
            for child in children {
                if seen.insert(child.clone())
                    && let Ok(state) = self.registry.state(&child).await
                {
                    states.push(state);
                }
            }
            index += 1;
        }
        let tasks: Vec<_> = states.iter().flat_map(|state| &state.tasks).collect();
        let mut routes = vec![];
        for state in &states {
            for task in &state.tasks {
                if let Some(native) = &task.native_task {
                    let parent = task.parent_task.as_ref().and_then(|id| {
                        tasks
                            .iter()
                            .find(|candidate| &candidate.id == id)
                            .map(|parent| parent.native_key.clone())
                    });
                    routes.push((native.clone(), task.native_key.clone(), parent, true));
                }
            }
            for (key, work) in &state.background_work {
                routes.push((key.clone(), work.tool.clone(), None, false));
            }
        }
        routes
    }

    /// The fork's session is reserved on the source thread before its transcript
    /// exists, so an import never adopts it; `None` once the fork is no longer pending.
    pub(super) async fn fork_claude(
        &self,
        target: &LaunchTarget,
        effect_id: &str,
        command: &CommandId,
        provider: &ProviderCommand,
    ) -> Result<Option<String>, ForkError> {
        let directive = ClaudeProtocol::default()
            .command(provider, "", &[])
            .map_err(|error| ForkError::Rejected(error.to_string()))?
            .process;
        let Some(ProcessDirective::Fork {
            native_thread,
            through_head,
        }) = directive
        else {
            return Err(ForkError::Rejected("not a native fork".into()));
        };
        self.keys
            .with_lock(
                target.key.clone(),
                self.close_fork_source(target, &native_thread),
            )
            .await?;
        let source = self
            .host
            .read_claude_session(target.clone(), native_thread.clone())
            .await
            .map_err(|error| ForkError::Retry(error.to_string()))?;
        let mut serial = 0;
        let now = self.registry.context().clock.now();
        let forked = claude_fork_session(
            &source,
            &native_thread,
            through_head.as_deref(),
            None,
            || {
                serial += 1;
                derived_uuid("fork", &format!("{effect_id}:{serial}"))
            },
            now.as_str(),
        )
        .map_err(|error| ForkError::Rejected(error.to_string()))?;
        let reserved = self
            .input(
                &target.key.thread,
                agent_domain::Input::NativeForkReserved {
                    command: command.clone(),
                    native_thread: forked.session_id.clone(),
                },
            )
            .await
            .map_err(|error| ForkError::Retry(error.to_string()))?;
        if reserved != Reply::Accepted {
            return Ok(None);
        }
        self.host
            .write_claude_session(target.clone(), forked.session_id.clone(), forked.transcript)
            .await
            .map_err(|error| ForkError::Retry(error.to_string()))?;
        Ok(Some(forked.session_id))
    }
}

impl SessionManager {
    /// T3 forkThread: no fork while a turn of the source runs, and the source's
    /// live process closes before its transcript is read.
    async fn close_fork_source(
        &self,
        target: &LaunchTarget,
        native_thread: &str,
    ) -> Result<(), ForkError> {
        let state = self
            .state(&target.key.thread)
            .await
            .map_err(|error| ForkError::Retry(format!("{error:?}")))?;
        if let Some(run) = state.runs.iter().find(|run| {
            run.selection.instance == target.key.instance
                && matches!(run.status, RunStatus::Starting | RunStatus::Running)
        }) {
            return Err(ForkError::Rejected(format!(
                "Cannot fork Claude provider thread {native_thread} while provider turn {} is active.",
                run.id
            )));
        }
        if let Some(entry) = self.entry(&Slot::Thread(target.key.clone()))
            && entry.claude.as_ref().is_some_and(|process| {
                process.lock().expect("claude process").native.as_deref() == Some(native_thread)
            })
        {
            self.close_entry(&entry, true).await;
        }
        Ok(())
    }
}

pub(super) fn claude_launch(
    target: &LaunchTarget,
    settings: &ClaudeSettings,
    native_session: Option<String>,
    resume_at: Option<String>,
    new_session: Option<String>,
) -> ClaudeLaunch {
    let options = claude_model_options(&target.selection);
    let mut merged = settings.settings.clone().unwrap_or_else(|| json!({}));
    if let (Some(merged), Some(model)) = (merged.as_object_mut(), options.settings.as_object()) {
        merged.extend(model.clone());
    }
    let settings_value = (merged != json!({})).then_some(merged);
    let mut policy = claude_runtime_query_policy(
        target.runtime_mode,
        target.interaction_mode,
        settings.approval_policy.as_deref(),
        settings.sandbox_kind.as_deref(),
        settings.read_only_allows_global_reads,
    );
    if !settings.mcp_servers.is_empty() && !settings.mcp_allowed_tools.is_empty() {
        let mut allowed = policy.allowed_tools.take().unwrap_or_default();
        for tool in &settings.mcp_allowed_tools {
            if !allowed.contains(tool) {
                allowed.push(tool.clone());
            }
        }
        policy.allowed_tools = Some(allowed);
    }
    ClaudeLaunch {
        model: options.model,
        policy,
        resume_at: resume_at.filter(|_| native_session.is_some()),
        native_session,
        new_session,
        fork: false,
        additional_directories: settings.additional_directories.clone(),
        effort: options.effort,
        disallowed_tools: settings.disallowed_tools.clone(),
        mcp_servers: settings.mcp_servers.clone(),
        settings: settings_value,
        extra_args: settings.extra_args.clone(),
    }
}

/// T3 reuses a live query only for the same query policy and model selection;
/// any other launch replaces the process.
fn compatible(current: &ClaudeLaunch, wanted: &ClaudeLaunch) -> bool {
    let key = |launch: &ClaudeLaunch| {
        (
            launch.model.clone(),
            launch.policy.clone(),
            launch.additional_directories.clone(),
            launch.effort.clone(),
            launch.disallowed_tools.clone(),
            launch.mcp_servers.clone(),
            launch.settings.clone(),
            launch.extra_args.clone(),
        )
    };
    key(current) == key(wanted)
}

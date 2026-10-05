use super::*;

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
        let launch = claude_launch(target, &settings, native.clone(), resume_at, new_session);
        let entry = self
            .claude_session(target, state, attempt, command, &settings, launch)
            .await?;
        self.bind(attempt, &entry);
        let (command, images) = (command.clone(), images.to_vec());
        let operation = operation(&command);
        let sent = self
            .send(
                &entry,
                Request::new(move |p| p.claude()?.command(&command, &prompt, &images))
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
        if let Some(entry) = self.entry(&target.key) {
            let reusable = entry.claude.as_ref().is_some_and(|process| {
                let process = process.lock().expect("claude process");
                launch.native_session.is_some()
                    && process.native == launch.native_session
                    && compatible(&process.launch, &launch)
            });
            if reusable {
                match self.align(&entry, &launch).await {
                    Ok(()) => return Ok(entry),
                    Err(message) => {
                        tracing::warn!(thread = %target.key.thread, %message,
                            "respawning a Claude session that could not be reconfigured");
                    }
                }
            }
            self.close_entry(&entry, true, false).await;
        }
        let resumed = launch.native_session.is_some();
        let entry = self.spawn(target, Some(launch)).await?;
        let skills = settings.skills.clone();
        let prompt = settings.append_system_prompt.clone();
        let initialized = self
            .request_reply(
                &entry,
                Request::new(move |p| {
                    let claude = p.claude()?;
                    claude.set_skills(skills);
                    Ok(frames(vec![claude.control.initialize(&prompt)]))
                })
                .owner(attempt)
                .handshake(),
            )
            .await;
        if let Err(message) = initialized {
            self.close_entry(&entry, false, false).await;
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
            Request::new(move |p| {
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

    pub(super) async fn align(&self, entry: &Entry, launch: &ClaudeLaunch) -> Result<(), String> {
        let Some(process) = &entry.claude else {
            return Ok(());
        };
        let (model, mode) = {
            let process = process.lock().expect("claude process");
            (
                (process.model != launch.model).then(|| launch.model.clone()),
                (process.permission_mode != launch.policy.permission_mode)
                    .then(|| launch.policy.permission_mode.clone()),
            )
        };
        if let Some(model) = model {
            let payload = json!({ "model": model });
            self.request_reply(
                entry,
                Request::new(move |p| {
                    Ok(frames(vec![
                        p.claude()?.control.request("set_model", payload),
                    ]))
                }),
            )
            .await?;
            process.lock().expect("claude process").model = model;
        }
        if let Some(mode) = mode {
            let payload = json!({ "mode": mode });
            self.request_reply(
                entry,
                Request::new(move |p| {
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

    pub(super) async fn fork_claude(
        &self,
        target: &LaunchTarget,
        effect_id: &str,
        provider: &ProviderCommand,
    ) -> Result<String, ForkError> {
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
        self.host
            .write_claude_session(target.clone(), forked.session_id.clone(), forked.transcript)
            .await
            .map_err(|error| ForkError::Retry(error.to_string()))?;
        Ok(forked.session_id)
    }
}

fn claude_launch(
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
    ClaudeLaunch {
        model: options.model,
        policy: claude_runtime_query_policy(
            target.runtime_mode,
            target.interaction_mode,
            settings.approval_policy.as_deref(),
            settings.sandbox_kind.as_deref(),
            settings.read_only_allows_global_reads,
        ),
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

/// Launch flags that a live process cannot change.
fn compatible(current: &ClaudeLaunch, wanted: &ClaudeLaunch) -> bool {
    let fixed = |launch: &ClaudeLaunch| {
        (
            launch.policy.tools.clone(),
            launch.policy.allowed_tools.clone(),
            launch.policy.allow_dangerously_skip_permissions,
            launch.policy.install_permission_callback,
            launch.additional_directories.clone(),
            launch.effort.clone(),
            launch.disallowed_tools.clone(),
            launch.mcp_servers.clone(),
            launch.settings.clone(),
            launch.extra_args.clone(),
        )
    };
    fixed(current) == fixed(wanted)
}

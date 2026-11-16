use super::*;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Queue,
    Steer,
}

#[derive(Debug, Clone)]
pub struct Input {
    pub message_id: String,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    /// T3 scheduledTaskId: the Host automation that sent this message.
    pub scheduled_task_id: Option<String>,
    pub text: String,
    pub attachments: Vec<Value>,
    pub context: Option<Value>,
    pub model_selection: ModelSelection,
    pub driver: String,
    pub workspace_preparation: Option<WorkspacePreparation>,
    /// Fork provenance resolved by the Host from the selected source run.
    pub fork_origin: Option<ProviderThreadFork>,
    /// T3 message.dispatch freshContext. Never steers.
    pub fresh_context: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    pub command_id: &'a str,
    pub now: &'a str,
    pub next_run_ordinal: u64,
    pub can_steer: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Decision {
    pub changes: Vec<Change>,
    pub effects: Vec<Effect>,
}

impl Decision {
    /// The I/O owner supplies its last observed bytes. Finalization transfers
    /// them to terminal command rows in the same commit as their outcome;
    /// still-running background items retain their live stream ownership.
    pub fn with_command_outputs(
        mut self,
        tails: Option<&std::collections::HashMap<String, crate::sync::Output>>,
    ) -> Self {
        let Some(tails) = tails else {
            return self;
        };
        for change in &mut self.changes {
            let Change::TurnItem(item) = change else {
                continue;
            };
            if !item.status.is_terminal() {
                continue;
            }
            let Some(tail) = tails.get(&item.id) else {
                continue;
            };
            if let TurnItemBody::CommandExecution {
                output,
                output_omitted,
                ..
            } = &mut item.body
            {
                *output = Some(tail.text.clone());
                *output_omitted = Some(tail.truncated);
            }
        }
        self
    }
}

pub(super) fn id(command: &str, kind: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("{command}:{kind}").as_bytes()).to_string()
}

pub(super) fn effect(context: Context<'_>, thread_id: &str, request: EffectRequest) -> Effect {
    let identity = serde_json::to_string(&request).expect("typed effect requests serialize");
    Effect {
        id: id(context.command_id, &identity),
        command_id: context.command_id.to_string(),
        thread_id: thread_id.to_owned(),
        request,
        status: EffectStatus::Pending,
        attempt_count: 0,
        available_at: context.now.to_owned(),
        lease_owner: None,
        lease_expires_at: None,
        created_at: context.now.to_owned(),
        updated_at: context.now.to_owned(),
        completed_at: None,
        last_error: None,
    }
}

pub fn user_item_id(thread_id: &str, message_id: &str) -> String {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("turn-item:{thread_id}:{message_id}").as_bytes(),
    )
    .to_string()
}

fn user_item(
    thread_id: &str,
    run_id: &str,
    input: &Input,
    intent: &str,
    context: Context<'_>,
) -> TurnItem {
    TurnItem {
        detail_omitted: false,
        id: user_item_id(thread_id, &input.message_id),
        thread_id: thread_id.to_owned(),
        run_id: Some(run_id.to_owned()),
        node_id: None,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: 0,
        status: TurnItemStatus::Completed,
        title: None,
        started_at: Some(context.now.to_owned()),
        completed_at: Some(context.now.to_owned()),
        updated_at: context.now.to_owned(),
        body: TurnItemBody::UserMessage {
            created_by: input.created_by,
            creation_source: input.creation_source,
            scheduled_task_id: input.scheduled_task_id.clone(),
            message_id: input.message_id.clone(),
            input_intent: intent.to_owned(),
            text: input.text.clone(),
            attachments: input.attachments.clone(),
            context: input.context.clone(),
        },
    }
}

fn steer(state: &State, input: &Input, context: Context<'_>) -> Option<Decision> {
    if state.turn_items.values().any(|item| {
        matches!(item.body, TurnItemBody::RunInterruptRequest { .. })
            && item.status == TurnItemStatus::Pending
    }) {
        return None;
    }
    let run = state
        .active_run()
        .filter(|run| run.provider_instance_id == input.model_selection.instance_id)?;
    let attempt = state
        .active_attempt(run.active_attempt_id.as_deref())
        .filter(|attempt| attempt.status == RunAttemptStatus::Running)?;
    let native = attempt
        .provider_turn_id
        .as_ref()
        .and_then(|id| state.provider_turns.get(id))
        .filter(|turn| turn.status == ProviderTurnStatus::Running)?;
    let session = state.provider_session(&attempt.provider_thread_id)?;
    let mut item = user_item(&state.thread.id, &run.id, input, "steer", context);
    item.node_id = Some(attempt.root_node_id.clone());
    item.provider_thread_id = Some(attempt.provider_thread_id.clone());
    item.provider_turn_id = Some(native.id.clone());
    let action = effect(
        context,
        &state.thread.id,
        EffectRequest::ProviderTurnSteer {
            provider_session_id: session.id.clone(),
            provider_thread_id: attempt.provider_thread_id.clone(),
            provider_turn_id: native.id.clone(),
            message_id: input.message_id.clone(),
            model_selection: Some(input.model_selection.clone()),
        },
    );
    Some(Decision {
        changes: vec![Change::TurnItem(item)],
        effects: vec![action],
    })
}

/// A steer joins the active run. Before its native turn is ready it remains a
/// queued run with steerWhenReadyRunId, as in T3's run admission.
pub fn admit(
    state: &State,
    input: Input,
    admission: Admission,
    context: Context<'_>,
) -> Result<Decision, String> {
    if context.next_run_ordinal == 0 {
        return Err("run ordinal must be positive".into());
    }
    if state.thread.archived_at.is_some() || state.thread.deleted_at.is_some() {
        return Err("thread is not active".into());
    }
    if state.thread.is_provider_subagent() {
        return Err("provider-native subagent threads are read-only".into());
    }
    if state.has_uncertain_input() {
        return Err("resolve the uncertain input before sending".into());
    }
    if input.text.trim().is_empty()
        && input.attachments.is_empty()
        && !input
            .context
            .as_ref()
            .and_then(|context| context.get("records"))
            .and_then(Value::as_array)
            .is_some_and(|records| !records.is_empty())
    {
        return Err("input is empty".into());
    }
    if admission == Admission::Steer
        && state.active_run().is_some()
        && input.text.trim().eq_ignore_ascii_case("/compact")
        && input.attachments.is_empty()
    {
        return Err("context compaction must run as a separate turn".into());
    }
    if state
        .runs
        .values()
        .any(|run| run.user_message_id == input.message_id)
        || state.turn_items.values().any(|item| {
            matches!(&item.body,
        TurnItemBody::UserMessage {message_id,..} if message_id==&input.message_id)
        })
    {
        return Err("message is already accepted".into());
    }
    let fork = state.pending_fork();
    if input.fresh_context && fork.is_some() {
        return Err(
            "A fresh session can't start while a fork or merge is still being applied.".into(),
        );
    }
    // T3 turns a fresh start's steer into a queue after the active run.
    let admission = if input.fresh_context {
        Admission::Queue
    } else {
        admission
    };
    if admission == Admission::Steer
        && context.can_steer
        && let Some(decision) = steer(state, &input, context)
    {
        return Ok(decision);
    }
    let active = state.active_run();
    let queued =
        active.is_some() || state.native_work_blocks_delivery() || state.has_pending_rollback();
    let run_id = id(context.command_id, "run");
    let mut item = user_item(
        &state.thread.id,
        &run_id,
        &input,
        if queued { "queued_turn" } else { "turn_start" },
        context,
    );
    // A fresh start never reuses a provider thread; others continue the
    // latest conversation since the context boundary.
    let boundary = state.context_boundary_ordinal();
    let instance = input.model_selection.instance_id.as_str();
    let mut provider_thread = (!input.fresh_context)
        .then(|| {
            state
                .thread
                .active_provider_thread_id
                .as_ref()
                .and_then(|id| state.provider_threads.get(id))
                .filter(|thread| {
                    thread.provider_instance_id == instance && !thread.predates(boundary)
                })
                .or_else(|| state.root_provider_thread(instance, boundary))
        })
        .flatten()
        .cloned()
        .unwrap_or_else(|| ProviderThread {
            id: id(context.command_id, "provider-thread"),
            driver: input.driver.clone(),
            provider_instance_id: input.model_selection.instance_id.clone(),
            provider_session_id: None,
            app_thread_id: Some(state.thread.id.clone()),
            owner_node_id: None,
            native_thread_ref: None,
            native_conversation_head_ref: None,
            native_metadata: None,
            status: ProviderThreadStatus::NotLoaded,
            first_run_ordinal: None,
            last_run_ordinal: None,
            handoff_ids: Vec::new(),
            forked_from: None,
            pending_background_tasks: Vec::new(),
            created_at: context.now.into(),
            updated_at: context.now.into(),
        });
    if let Some(fork) = fork {
        provider_thread.forked_from = input.fork_origin.clone().filter(|_| {
            fork.source_provider_instance_id.as_deref()
                == Some(input.model_selection.instance_id.as_str())
        });
    }
    let root_id = id(&run_id, "root-node");
    let attempt_id = id(&run_id, "attempt:1");
    item.node_id = Some(root_id.clone());
    item.provider_thread_id = Some(provider_thread.id.clone());
    if queued {
        item.status = TurnItemStatus::Pending;
        item.started_at = None;
        item.completed_at = None;
    }
    let run = Run {
        id: run_id.clone(),
        thread_id: state.thread.id.clone(),
        ordinal: context.next_run_ordinal,
        provider_instance_id: input.model_selection.instance_id.clone(),
        model_selection: input.model_selection,
        provider_thread_id: Some(provider_thread.id.clone()),
        user_message_id: input.message_id,
        root_node_id: Some(root_id.clone()),
        active_attempt_id: Some(attempt_id.clone()),
        status: if queued {
            RunStatus::Queued
        } else if input.workspace_preparation.is_some() {
            RunStatus::Preparing
        } else {
            RunStatus::Starting
        },
        queue_position: queued.then(|| {
            state
                .runs
                .values()
                .filter(|run| run.status == RunStatus::Queued)
                .map(|run| run.queue_position.unwrap_or(run.ordinal))
                .max()
                .unwrap_or(0)
                + 1
        }),
        queue_held: queued
            && state
                .runs
                .values()
                .any(|run| run.status == RunStatus::Queued && run.queue_held),
        steer_when_ready_run_id: if admission == Admission::Steer && context.can_steer {
            active.map(|run| run.id.clone())
        } else {
            None
        },
        requested_at: context.now.to_owned(),
        started_at: None,
        completed_at: None,
        checkpoint_id: None,
        context_handoff_id: None,
        restart_continuation_of_run_id: None,
        usage_limit_continuation_of_run_id: None,
        work_started_at: None,
        workspace_preparation: input.workspace_preparation,
        fresh_context: input.fresh_context,
    };
    let attempt = RunAttempt {
        id: attempt_id,
        native_thread_ref: provider_thread.native_thread_ref.clone(),
        run_id: run.id.clone(),
        attempt_ordinal: 1,
        root_node_id: root_id.clone(),
        provider_instance_id: run.provider_instance_id.clone(),
        provider_thread_id: provider_thread.id.clone(),
        provider_turn_id: None,
        reason: AttemptReason::Initial,
        status: RunAttemptStatus::Pending,
        started_at: None,
        completed_at: None,
    };
    let root = ExecutionNode {
        id: root_id.clone(),
        thread_id: state.thread.id.clone(),
        run_id: Some(run.id.clone()),
        parent_node_id: None,
        root_node_id: root_id,
        kind: NodeKind::RootTurn,
        status: NodeStatus::Pending,
        counts_for_run: true,
        provider_thread_id: Some(provider_thread.id.clone()),
        provider_turn_id: None,
        native_item_ref: None,
        runtime_request_id: None,
        checkpoint_scope_id: None,
        started_at: None,
        completed_at: None,
    };
    let preparing = run.status == RunStatus::Preparing;
    let selection = run.model_selection.clone();
    let preparation = preparing.then(|| preparation::item(&run, &root, context.now));
    let mut changes = vec![
        Change::ProviderThread(provider_thread.clone()),
        Change::RunCreated(run),
        Change::RunAttemptCreated(attempt),
        Change::Node(root),
        Change::TurnItem(item),
    ];
    changes.extend(fork.map(|transfer| {
        Change::ContextTransfer(transfer.for_run(&run_id, &selection.instance_id, context.now))
    }));
    changes.extend(preparation.map(Change::TurnItem));
    if !queued {
        changes.push(Change::Thread(Thread {
            active_provider_thread_id: Some(provider_thread.id),
            provider_instance_id: selection.instance_id.clone(),
            model_selection: selection,
            workspace_launch: if preparing {
                state.thread.workspace_launch.as_ref().map(|workspace| {
                    let mut workspace = workspace.clone();
                    workspace.generation = uuid::Uuid::new_v5(
                        &uuid::Uuid::NAMESPACE_URL,
                        context.command_id.as_bytes(),
                    );
                    workspace.visible = true;
                    workspace.run_id = Some(run_id.clone());
                    workspace
                })
            } else {
                state.thread.workspace_launch.clone()
            },
            updated_at: context.now.into(),
            ..state.thread.clone()
        }));
    }
    let effects = if !queued && !preparing {
        vec![effect(
            context,
            &state.thread.id,
            EffectRequest::ProviderTurnStart { run_id },
        )]
    } else {
        Vec::new()
    };
    Ok(Decision { changes, effects })
}

pub fn hold_queue(state: &State, held: bool) -> Decision {
    Decision {
        changes: state
            .runs
            .values()
            .filter(|run| run.status == RunStatus::Queued && run.queue_held != held)
            .map(|run| {
                Change::Run(Run {
                    queue_held: held,
                    ..run.clone()
                })
            })
            .collect(),
        ..Default::default()
    }
}

pub fn edit_queued(
    state: &State,
    run_id: &str,
    text: String,
    attachments: Vec<Value>,
    message_context: Option<Value>,
    now: &str,
) -> Result<Decision, String> {
    let run = state
        .runs
        .get(run_id)
        .filter(|run| run.status == RunStatus::Queued)
        .ok_or("run is no longer queued")?;
    if text.trim().is_empty() {
        return Err("queued messages need nonempty text".into());
    }
    let item = state
        .user_message_item(&run.user_message_id)
        .ok_or("queued message is missing")?;
    let mut item = item.clone();
    if let TurnItemBody::UserMessage {
        text: old,
        attachments: files,
        context,
        ..
    } = &mut item.body
    {
        *old = text;
        *files = attachments;
        *context = message_context;
    }
    item.updated_at = now.into();
    Ok(Decision {
        changes: vec![Change::TurnItem(item)],
        ..Default::default()
    })
}

pub fn remove_input(state: &State, run_id: &str, now: &str) -> Result<Decision, String> {
    let run = state
        .runs
        .get(run_id)
        .filter(|run| {
            matches!(
                run.status,
                RunStatus::Queued | RunStatus::Failed | RunStatus::Cancelled
            )
        })
        .ok_or("input is in flight or already accepted by the provider")?;
    let attempt = state
        .active_attempt(run.active_attempt_id.as_deref())
        .ok_or("queued run has no attempt")?;
    if attempt.provider_turn_id.is_some()
        || state.provider_turns.values().any(|turn| {
            turn.run_attempt_id.as_ref().is_some_and(|id| {
                state
                    .run_attempts
                    .get(id)
                    .is_some_and(|attempt| attempt.run_id == run_id)
            })
        }) || state.effects.values().any(|effect| {
        matches!(&effect.request,EffectRequest::ProviderTurnStart{run_id:target} if target==run_id)
            && matches!(effect.status, EffectStatus::Pending | EffectStatus::Running)
    }) {
        return Err("input may already have been accepted by the provider".into());
    }
    let mut changes = vec![
        Change::Run(Run {
            status: RunStatus::Cancelled,
            queue_position: None,
            queue_held: false,
            steer_when_ready_run_id: None,
            completed_at: Some(now.into()),
            ..run.clone()
        }),
        Change::RunAttempt(RunAttempt {
            status: RunAttemptStatus::Cancelled,
            completed_at: Some(now.into()),
            ..attempt.clone()
        }),
    ];
    changes.extend(
        state
            .nodes
            .values()
            .filter(|node| node.run_id.as_deref() == Some(run_id))
            .map(|node| {
                Change::Node(ExecutionNode {
                    status: NodeStatus::Cancelled,
                    completed_at: Some(now.into()),
                    ..node.clone()
                })
            }),
    );
    changes.extend(
        state
            .turn_items
            .values()
            .filter(|item| item.run_id.as_deref() == Some(run_id))
            .map(|item| {
                Change::TurnItem(TurnItem {
                    status: TurnItemStatus::Cancelled,
                    completed_at: Some(now.into()),
                    updated_at: now.into(),
                    ..item.clone()
                })
            }),
    );
    Ok(Decision {
        changes,
        ..Default::default()
    })
}

/// T3 keeps a not-yet-deliverable steer in the queue, then moves its original
/// message to the active run once a native turn is addressable.
pub fn promote_queued(
    state: &State,
    run_id: &str,
    context: Context<'_>,
) -> Result<Decision, String> {
    let run = state
        .runs
        .get(run_id)
        .filter(|run| run.status == RunStatus::Queued)
        .ok_or("run is no longer queued")?;
    let active = state.active_run().ok_or("there is no active run")?;
    if !context.can_steer || active.provider_instance_id != run.provider_instance_id {
        return Err("active run cannot receive this queued message".into());
    }
    let item = state
        .user_message_item(&run.user_message_id)
        .ok_or("queued message is missing")?;
    let TurnItemBody::UserMessage {
        created_by,
        creation_source,
        text,
        attachments,
        context: message_context,
        ..
    } = &item.body
    else {
        unreachable!()
    };
    let provider_thread = run
        .provider_thread_id
        .as_ref()
        .and_then(|id| state.provider_threads.get(id))
        .ok_or("queued provider thread is missing")?;
    let input = Input {
        message_id: run.user_message_id.clone(),
        created_by: *created_by,
        creation_source: *creation_source,
        scheduled_task_id: None,
        text: text.clone(),
        attachments: attachments.clone(),
        context: message_context.clone(),
        model_selection: run.model_selection.clone(),
        driver: provider_thread.driver.clone(),
        workspace_preparation: None,
        fork_origin: None,
        fresh_context: false,
    };
    if input.text.trim().eq_ignore_ascii_case("/compact") && input.attachments.is_empty() {
        return Err("context compaction must run as a separate turn".into());
    }
    let Some(mut steered) = steer(state, &input, context) else {
        return Ok(Decision {
            changes: vec![Change::Run(Run {
                steer_when_ready_run_id: Some(active.id.clone()),
                ..run.clone()
            })],
            ..Default::default()
        });
    };
    for change in &mut steered.changes {
        if let Change::TurnItem(TurnItem {
            body: TurnItemBody::UserMessage { input_intent, .. },
            ..
        }) = change
        {
            *input_intent = "promoted_queued_to_steer".into();
        }
    }
    let mut decision = remove_input(state, run_id, context.now)?;
    decision.changes.extend(steered.changes);
    decision.effects = steered.effects;
    Ok(decision)
}

pub fn move_queued(state: &State, run_id: &str, before: Option<&str>) -> Result<Decision, String> {
    let mut queued = state
        .runs
        .values()
        .filter(|run| run.status == RunStatus::Queued)
        .collect::<Vec<_>>();
    queued.sort_by_key(|run| (run.queue_position.unwrap_or(run.ordinal), run.ordinal));
    let index = queued
        .iter()
        .position(|run| run.id == run_id)
        .ok_or("run is no longer queued")?;
    if before == Some(run_id) {
        return Ok(Decision::default());
    }
    let selected = queued.remove(index);
    let insertion = match before {
        Some(before) => queued
            .iter()
            .position(|run| run.id == before)
            .ok_or("target run is no longer queued")?,
        None => queued.len(),
    };
    queued.insert(insertion, selected);
    Ok(Decision {
        changes: queued
            .into_iter()
            .enumerate()
            .filter_map(|(index, run)| {
                let position = index as u64 + 1;
                (run.queue_position != Some(position)).then(|| {
                    Change::Run(Run {
                        queue_position: Some(position),
                        ..run.clone()
                    })
                })
            })
            .collect(),
        ..Default::default()
    })
}

use crate::*;
pub fn now() -> Timestamp {
    Timestamp::parse("2026-10-05T00:00:00Z").unwrap()
}
pub fn turns() -> TurnCapabilities {
    TurnCapabilities {
        exposes_native_turn_id: true,
        emits_turn_started: true,
        emits_turn_completed: true,
        supports_interrupt: true,
        supports_active_steering: true,
        supports_steering_by_interrupt_restart: true,
        supports_queued_messages: true,
        terminal_status_quality: Strength::Strong,
    }
}
pub fn command(id: &str, body: CommandBody) -> Command {
    Command {
        command_id: CommandId::new(id).unwrap(),
        thread_id: ThreadId::new("thread").unwrap(),
        body,
    }
}
pub fn create() -> Command {
    command(
        "create",
        CommandBody::ThreadCreate {
            created_by: CreatedBy::User,
            creation_source: CreationSource::Desktop,
            project_id: ProjectId::new("project").unwrap(),
            title: "Build Bex".into(),
            model_selection: ModelSelection {
                instance_id: ProviderInstanceId::new("codex").unwrap(),
                model: "test-model".into(),
                options: Default::default(),
            },
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            branch: None,
            worktree_path: None,
        },
    )
}
pub fn send(id: &str, mode: DispatchMode) -> Command {
    command(
        id,
        CommandBody::MessageDispatch(MessageDispatch {
            source_plan_ref: None,
            created_by: CreatedBy::User,
            creation_source: CreationSource::Desktop,
            message_id: MessageId::new(format!("message:{id}")).unwrap(),
            text: format!("Input {id}"),
            context: None,
            attachments: vec![],
            model_selection: None,
            delivery_intent: None,
            dispatch_mode: mode,
        }),
    )
}
fn project(projection: Option<&ThreadProjection>, decision: &Decision) -> Option<ThreadProjection> {
    decision
        .events
        .iter()
        .fold(projection.cloned(), |current, event| {
            projector::apply(
                current.as_ref(),
                event,
                projector::ProjectionOptions::default(),
            )
        })
}
pub fn projection() -> ThreadProjection {
    project(
        None,
        &decider::decide(&create(), None, &now(), &turns(), Driver::Codex).unwrap(),
    )
    .unwrap()
}
pub fn apply(projection: &ThreadProjection, command: &Command) -> (ThreadProjection, Decision) {
    let decision =
        decider::decide(command, Some(projection), &now(), &turns(), Driver::Codex).unwrap();
    (project(Some(projection), &decision).unwrap(), decision)
}
pub fn running() -> ThreadProjection {
    let (mut projection, _) = apply(
        &projection(),
        &send("start", DispatchMode::StartImmediately),
    );
    let run = &mut projection.runs[0];
    run.status = RunStatus::Running;
    let attempt = &mut projection.attempts[0];
    attempt.status = AttemptStatus::Running;
    let id = ProviderTurnId::new("native-turn").unwrap();
    attempt.provider_turn_id = Some(id.clone());
    projection.provider_turns.push(ProviderTurn {
        id,
        provider_thread_id: attempt.provider_thread_id.clone(),
        node_id: attempt.root_node_id.clone(),
        run_attempt_id: Some(attempt.id.clone()),
        native_turn_ref: None,
        ordinal: 1,
        status: TurnStatus::Running,
        started_at: Some(now()),
        completed_at: None,
        token_usage: None,
        turn_token_usage: None,
    });
    projection
}

pub fn checkpoint_scope(run: &Run) -> CheckpointScope {
    CheckpointScope {
        id: CheckpointScopeId::new("root-scope").unwrap(),
        thread_id: run.thread_id.clone(),
        run_id: Some(run.id.clone()),
        node_id: run.root_node_id.clone().unwrap(),
        parent_scope_id: None,
        provider_thread_id: run.provider_thread_id.clone(),
        kind: ScopeKind::RootRun,
        ordinal_within_parent: 0,
        advances_app_run_count: true,
        cwd: "/workspace".into(),
        created_at: now(),
    }
}
pub fn checkpoint(run: &Run, status: CheckpointStatus) -> Checkpoint {
    let scope = checkpoint_scope(run);
    Checkpoint {
        id: CheckpointId::new(format!("checkpoint:{}", run.id)).unwrap(),
        thread_id: run.thread_id.clone(),
        scope_id: scope.id,
        run_id: Some(run.id.clone()),
        node_id: scope.node_id,
        parent_checkpoint_id: None,
        ordinal_within_scope: run.ordinal,
        app_run_ordinal: Some(run.ordinal),
        reference: CheckpointRef::new(format!("refs/t3/test/{}", run.id)).unwrap(),
        status,
        files: vec![],
        captured_at: now(),
    }
}

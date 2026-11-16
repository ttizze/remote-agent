//! Canonical provider threads select the active source. Accepted attempts keep
//! previous native identities private when recovery replaces that selection.
//! Observed turns retain the same ownership for work without app attempts.
use super::*;
use agent_protocol::orchestration as model;

const OWNERS: &str = "WITH native_owners AS (
    SELECT json_extract(payload_json,'$.appThreadId') AS thread_id,
           provider_instance_id, driver,
           json_extract(payload_json,'$.nativeThreadRef.nativeId') AS native_id
    FROM orchestration_v2_projection_provider_threads
    WHERE json_extract(payload_json,'$.appThreadId') IS NOT NULL
    UNION
    SELECT a.thread_id, a.provider_instance_id,
           json_extract(a.payload_json,'$.nativeThreadRef.driver'),
           json_extract(a.payload_json,'$.nativeThreadRef.nativeId')
    FROM orchestration_v2_projection_run_attempts a
    UNION
    SELECT t.thread_id,p.provider_instance_id,
           json_extract(t.payload_json,'$.nativeThreadRef.driver'),
           json_extract(t.payload_json,'$.nativeThreadRef.nativeId')
    FROM orchestration_v2_projection_provider_turns t
    JOIN orchestration_v2_projection_provider_threads p ON p.provider_thread_id=t.provider_thread_id
)";

fn source(instance: &str, driver: &str, native_id: String) -> Result<SessionRef, String> {
    let kind = ProviderKind::from_instance_id(instance)?;
    if kind.driver() != driver {
        return Err("native binding uses another provider".into());
    }
    SessionRef::new(kind, native_id).map_err(str::to_owned)
}

pub(super) fn header_source(
    thread: &model::Thread,
    native: &model::ProviderThread,
) -> Result<SessionRef, String> {
    let reference = native
        .native_thread_ref
        .as_ref()
        .ok_or("native header reference is missing")?;
    let kind = ProviderKind::from_instance_id(&native.provider_instance_id)?;
    if thread.provider_instance_id != kind.key()
        || native.driver != kind.driver()
        || reference.driver != kind.driver()
        || native.app_thread_id.as_deref() != Some(&thread.id)
        || thread.active_provider_thread_id.as_deref() != Some(&native.id)
    {
        return Err("native header identity changed".into());
    }
    SessionRef::new(
        kind,
        reference
            .native_id
            .clone()
            .ok_or("native header identity is missing")?,
    )
    .map_err(str::to_owned)
}

pub(super) fn owner(
    connection: &Connection,
    native: &SessionRef,
) -> Result<Option<SessionRef>, String> {
    let Some(kind) = native.provider else {
        return Ok(None);
    };
    let mut query = connection
        .prepare(&format!(
            "{OWNERS}
        SELECT DISTINCT thread_id FROM native_owners
        WHERE provider_instance_id=?1 AND driver=?2 AND native_id=?3"
        ))
        .map_err(failure)?;
    let owners = query
        .query_map(params![kind.key(), kind.driver(), native.id], |row| {
            row.get::<_, String>(0)
        })
        .map_err(failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(failure)?;
    if owners.len() > 1 {
        return Err("native identity belongs to multiple app threads".into());
    }
    owners
        .into_iter()
        .next()
        .map(|id| SessionRef::new(None, id).map_err(str::to_owned))
        .transpose()
}

impl Storage {
    pub(in crate::host_rpc) fn native_owner(
        &self,
        native: &SessionRef,
    ) -> Result<Option<SessionRef>, String> {
        self.with(|connection| owner(connection, native))
    }

    pub(in crate::host_rpc) fn native_owners<'a>(
        &self,
        links: impl IntoIterator<Item = &'a SessionRef>,
    ) -> Result<std::collections::HashMap<SessionRef, SessionRef>, String> {
        let links = links.into_iter().collect::<std::collections::BTreeSet<_>>();
        self.with(|connection| {
            let mut owners = std::collections::HashMap::new();
            for native in links {
                if let Some(target) = owner(connection, native)? {
                    owners.insert(native.clone(), target);
                }
            }
            Ok(owners)
        })
    }

    /// Every owned reference remains private, including inactive providers and
    /// superseded native conversations named by accepted attempts.
    pub(in crate::host_rpc) fn active_sources(&self) -> Result<Vec<SessionRef>, String> {
        self.with(|connection| {
            let mut query=connection.prepare(&format!("{OWNERS}
                SELECT DISTINCT provider_instance_id,driver,native_id FROM native_owners WHERE native_id IS NOT NULL ORDER BY provider_instance_id,native_id")).map_err(failure)?;
            query.query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?)))
                .map_err(failure)?.map(|row| {
                    let (instance,driver,native)=row.map_err(failure)?;
                    source(&instance,&driver,native)
                }).collect()
        })
    }

    pub(in crate::host_rpc) fn active_source(
        &self,
        id: &SessionRef,
    ) -> Result<Option<SessionRef>, String> {
        self.with(|connection| active_source(connection, id))
    }
}

pub(super) fn active_source(
    connection: &Connection,
    id: &SessionRef,
) -> Result<Option<SessionRef>, String> {
    let Some(thread) = projections::read::<model::Thread>(connection, &id.application_id())? else {
        return Ok(None);
    };
    selected_source(
        connection,
        &thread.id,
        &thread.provider_instance_id,
        thread.active_provider_thread_id.as_deref(),
        thread.deleted_at.is_some(),
    )
}

pub(super) fn selected_source(
    connection: &Connection,
    thread: &str,
    instance: &str,
    active: Option<&str>,
    deleted: bool,
) -> Result<Option<SessionRef>, String> {
    if deleted {
        return Ok(None);
    }
    let Some(provider) = active
        .map(|id| projections::read::<model::ProviderThread>(connection, id))
        .transpose()?
        .flatten()
        .filter(|provider| {
            provider.app_thread_id.as_deref() == Some(thread)
                && provider.provider_instance_id == instance
        })
    else {
        return Ok(None);
    };
    let Some(reference) = provider.native_thread_ref else {
        return Ok(None);
    };
    if reference.driver != provider.driver {
        return Err("native binding uses another provider".into());
    }
    reference
        .native_id
        .map(|native| source(&provider.provider_instance_id, &provider.driver, native))
        .transpose()
}

impl Storage {
    /// Child turn facts update the established parent's task in one writer
    /// transaction. They never borrow the parent's application execution ID.
    pub(in crate::host_rpc) fn observe_native_child_turn(
        &self,
        native: &SessionRef,
        native_turn: &str,
        new_turn: bool,
        now: &str,
    ) -> Result<(), String> {
        self.with(|connection| {
            let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(failure)?;
            let Some(child)=owner(&tx,native)? else {return Ok(());};
            let child=projections::metadata(&tx,&child.application_id())?;
            if child.thread.deleted_at.is_some() || child.thread.created_by!=model::Actor::Agent
                || child.thread.creation_source!=model::CreationSource::Provider
                || child.thread.lineage.relationship_to_parent.as_deref()!=Some("subagent") {return Ok(());}
            let Some(provider)=child.selected_native_thread() else {return Ok(());};
            if header_source(&child.thread,provider)?!=*native {return Ok(());}
            let Some(turn)=child.provider_turns.values().filter(|turn|turn.provider_thread_id==provider.id
                && turn.run_attempt_id.is_none()).max_by_key(|turn|turn.ordinal)
                .filter(|turn|turn.control_id()==Some(native_turn)) else {return Ok(());};
            let Some(parent_id)=child.thread.lineage.parent_thread_id.as_ref() else {return Ok(());};
            let parent=projections::metadata(&tx,parent_id)?;
            if parent.thread.deleted_at.is_some() {return Ok(());}
            let Some(task)=parent.subagents.values().find(|task|task.origin=="provider_native"
                && task.child_thread_id.as_ref()==Some(&child.thread.id)
                && task.provider_thread_id.as_ref()==Some(&provider.id)) else {return Ok(());};
            let ids=tx.prepare("SELECT turn_item_id FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND node_id=?2 AND type='subagent'").map_err(failure)?
                .query_map(params![parent_id,task.id],|row|row.get::<_,String>(0)).map_err(failure)?
                .collect::<Result<Vec<_>,_>>().map_err(failure)?;
            let parent=projections::load(&tx,parent_id,&ids)?;
            let status=match turn.status {
                model::ProviderTurnStatus::Pending=>model::TurnItemStatus::Pending,
                model::ProviderTurnStatus::Running=>model::TurnItemStatus::Running,
                model::ProviderTurnStatus::Completed=>model::TurnItemStatus::Completed,
                model::ProviderTurnStatus::Failed=>model::TurnItemStatus::Failed,
                model::ProviderTurnStatus::Interrupted=>model::TurnItemStatus::Interrupted,
                model::ProviderTurnStatus::Cancelled=>model::TurnItemStatus::Cancelled,
            };
            let result=if turn.status==model::ProviderTurnStatus::Completed {
                tx.query_row("SELECT json_extract(payload_json,'$.text') FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND provider_turn_id=?2 AND type='assistant_message' ORDER BY ordinal DESC LIMIT 1",params![child.thread.id,turn.id],|row|row.get::<_,String>(0)).optional().map_err(failure)?
            } else {None};
            let decision=model::observe_subagent_progress(&parent,&task.id,status,result.as_deref(),new_turn && turn.status==model::ProviderTurnStatus::Running,turn.started_at.as_deref(),now)?;
            commands::append_decision(&tx,&parent,decision,None,now)?;
            tx.commit().map_err(failure)
        })
    }

    /// A native parent announcement owns lineage, not an invented child turn.
    /// The concrete parent binding is checked again after metadata preparation.
    pub(in crate::host_rpc) fn observe_native_child(
        &self,
        parent: &SessionRef,
        execution: Option<uuid::Uuid>,
        child: &model::Thread,
        native_child: &model::ProviderThread,
        reported_model: Option<&str>,
        now: &str,
    ) -> Result<Option<model::ProviderThread>, String> {
        self.with(|connection| {
            let tx = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(failure)?;
            let Some(target) = owner(&tx, parent)? else {
                return Ok(None);
            };
            let state = projections::metadata(&tx, &target.application_id())?;
            if state.thread.deleted_at.is_some() || child.id == state.thread.id {
                return Ok(None);
            }
            let provider = parent.native_provider().map_err(str::to_owned)?;
            let attempt = execution
                .as_ref()
                .and_then(|id| state.run_attempts.get(&id.to_string()))
                .filter(|attempt| {
                    state.native_thread_matches(&attempt.id, provider.driver(), &parent.id)
                        && matches!(
                            attempt.status,
                            model::RunAttemptStatus::Pending
                                | model::RunAttemptStatus::Running
                                | model::RunAttemptStatus::Completed
                        )
                });
            let selected = state.selected_native_thread().is_some_and(|thread| {
                thread.driver == provider.driver()
                    && thread
                        .native_thread_ref
                        .as_ref()
                        .and_then(|reference| reference.native_id.as_deref())
                        == Some(parent.id.as_str())
            });
            if attempt.is_none() && !selected {
                return Ok(None);
            }
            let source = header_source(child, native_child)?;
            if source.provider != parent.provider
                || owner(&tx, &source)?.is_some_and(|owner| owner.application_id() != child.id)
            {
                return Ok(None);
            }
            let mut child = child.clone();
            child.created_by = model::Actor::Agent;
            child.creation_source = model::CreationSource::Provider;
            child.lineage.parent_thread_id = Some(state.thread.id.clone());
            child.lineage.relationship_to_parent = Some("subagent".into());
            child.lineage = native_threads::resolve_lineage(&tx, &child.id, &child.lineage)?;
            native_threads::write_header(&tx, &child, native_child, &source)?;
            let saved = projections::read::<model::Thread>(&tx, &child.id)?
                .ok_or("native child header is missing")?;
            let task = state
                .subagents
                .values()
                .find(|task| task.child_thread_id.as_ref() == Some(&child.id));
            let model =
                reported_model.filter(|_| task.is_none_or(|task| task.native_task_ref.is_none()));
            if matches!(saved.created_by, model::Actor::System | model::Actor::Agent)
                && (saved.lineage != child.lineage
                    || saved.created_by != model::Actor::Agent
                    || saved.creation_source != model::CreationSource::Provider
                    || model.is_some_and(|model| model != saved.model_selection.model))
            {
                if saved
                    .lineage
                    .parent_thread_id
                    .as_ref()
                    .is_some_and(|parent| parent != &state.thread.id)
                {
                    return Err("native child belongs to another parent".into());
                }
                let child = model::Thread {
                    lineage: child.lineage.clone(),
                    created_by: model::Actor::Agent,
                    creation_source: model::CreationSource::Provider,
                    model_selection: model::ModelSelection {
                        model: model
                            .map_or_else(|| saved.model_selection.model.clone(), str::to_owned),
                        ..saved.model_selection.clone()
                    },
                    updated_at: now.into(),
                    ..saved
                };
                commands::append_decision(
                    &tx,
                    &model::State::new(child.clone()),
                    model::Decision {
                        changes: vec![model::Change::Thread(child)],
                        ..Default::default()
                    },
                    None,
                    now,
                )?;
            }
            if task.is_none()
                && let Some(attempt) = attempt
                && state.nodes.contains_key(&attempt.root_node_id)
            {
                let id = model::derived_id(
                    "node",
                    &["provider", provider.driver(), "native-child", &child.id],
                );
                let node = model::ExecutionNode {
                    id: id.clone(),
                    thread_id: state.thread.id.clone(),
                    run_id: Some(attempt.run_id.clone()),
                    parent_node_id: Some(attempt.root_node_id.clone()),
                    root_node_id: attempt.root_node_id.clone(),
                    kind: model::NodeKind::Subagent,
                    status: model::NodeStatus::Idle,
                    counts_for_run: false,
                    provider_thread_id: Some(native_child.id.clone()),
                    provider_turn_id: None,
                    native_item_ref: None,
                    runtime_request_id: None,
                    checkpoint_scope_id: None,
                    started_at: None,
                    completed_at: None,
                };
                let task = model::Subagent {
                    id,
                    thread_id: state.thread.id.clone(),
                    run_id: Some(attempt.run_id.clone()),
                    parent_node_id: attempt.root_node_id.clone(),
                    origin: "provider_native".into(),
                    created_by: "agent".into(),
                    driver: provider.driver().into(),
                    provider_instance_id: provider.key().into(),
                    provider_thread_id: Some(native_child.id.clone()),
                    child_thread_id: Some(child.id.clone()),
                    native_task_ref: None,
                    title: (!child.title.is_empty()).then(|| child.title.clone()),
                    model: None,
                    status: model::SubagentStatus::Idle,
                    started_at: None,
                    completed_at: None,
                    updated_at: now.into(),
                };
                commands::append_decision(
                    &tx,
                    &state,
                    model::Decision {
                        changes: vec![model::Change::Node(node), model::Change::Subagent(task)],
                        ..Default::default()
                    },
                    None,
                    now,
                )?;
            }
            let saved = projections::read::<model::Thread>(&tx, &child.id)?
                .ok_or("native child header is missing")?;
            let provider = saved
                .active_provider_thread_id
                .as_ref()
                .ok_or("native child source is missing")?;
            let provider = projections::read::<model::ProviderThread>(&tx, provider)?
                .ok_or("native child source is missing")?;
            if header_source(&saved, &provider)? != source {
                return Ok(None);
            }
            tx.commit().map_err(failure)?;
            Ok(Some(provider))
        })
    }
}

//! Cross-thread native output has a narrower admission path than root output.
use super::*;

fn entity<T: DeserializeOwned>(
    connection: &Connection,
    table: &str,
    column: &str,
    thread: &ThreadId,
    id: &str,
) -> Result<Option<T>> {
    let json: Option<String> = connection.query_row(&format!("SELECT payload_json FROM orchestration_v2_projection_{table} WHERE thread_id=?1 AND {column}=?2"),params![thread.as_str(),id],|row|row.get(0)).optional()?;
    json.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
        .transpose()
}
fn thread(connection: &Connection, id: &ThreadId) -> Result<Option<AppThread>> {
    let json: Option<String> = connection
        .query_row(
            "SELECT payload_json FROM orchestration_v2_projection_threads WHERE thread_id=?1",
            [id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    json.map(|json| serde_json::from_str(&json).map_err(StoreError::from))
        .transpose()
}
fn terminal_item(status: ItemStatus) -> bool {
    matches!(
        status,
        ItemStatus::Completed
            | ItemStatus::Interrupted
            | ItemStatus::Failed
            | ItemStatus::Cancelled
    )
}
fn terminal_node(status: NodeStatus) -> bool {
    matches!(
        status,
        NodeStatus::Completed
            | NodeStatus::Interrupted
            | NodeStatus::Failed
            | NodeStatus::Cancelled
    )
}
pub(super) fn stop_children(
    connection: &Connection,
    parent: &ThreadProjection,
    run_ids: Option<&BTreeSet<RunId>>,
    now: &Timestamp,
    visited: &mut BTreeSet<ThreadId>,
) -> Result<Vec<StoredEvent>> {
    if !visited.insert(parent.thread.id.clone()) || visited.len() > 128 {
        return Ok(vec![]);
    }
    let mut committed = vec![];
    for task in parent.subagents.iter().filter(|t| {
        t.origin == SubagentOrigin::ProviderNative
            && !terminal_node(t.status)
            && run_ids.is_none_or(|ids| t.run_id.as_ref().is_some_and(|id| ids.contains(id)))
    }) {
        let mut stopped = task.clone();
        stopped.status = NodeStatus::Interrupted;
        stopped.completed_at = Some(now.clone());
        stopped.updated_at = now.clone();
        let mut payloads = vec![EventPayload::SubagentUpdated(stopped)];
        for node in parent.nodes.iter().filter(|n| n.id == task.id) {
            let mut n = node.clone();
            n.status = NodeStatus::Interrupted;
            n.completed_at = Some(now.clone());
            payloads.push(EventPayload::NodeUpdated(n));
        }
        for item in parent
            .turn_items
            .iter()
            .filter(|i| i.node_id.as_ref() == Some(&task.id))
        {
            payloads.push(EventPayload::TurnItemUpdated(projector::finished_item(
                item,
                ItemStatus::Interrupted,
                now,
            )));
        }
        committed.extend(commit_decision(
            connection,
            Decision {
                events: crate::events(
                    &parent.thread.id,
                    &format!("native-stop:{}:{}", task.id, now.as_str()),
                    payloads,
                    now,
                ),
                ..Default::default()
            },
            None,
            now,
        )?);
        if let Some(child) = task
            .child_thread_id
            .as_ref()
            .map(|id| load_projection(connection, id))
            .transpose()?
            .flatten()
        {
            let mut payloads = vec![];
            for node in child
                .nodes
                .iter()
                .filter(|n| n.run_id.is_none() && !terminal_node(n.status))
            {
                let mut n = node.clone();
                n.status = NodeStatus::Interrupted;
                n.completed_at = Some(now.clone());
                payloads.push(EventPayload::NodeUpdated(n));
            }
            for item in child
                .turn_items
                .iter()
                .filter(|i| i.run_id.is_none() && !terminal_item(i.status))
            {
                payloads.push(EventPayload::TurnItemUpdated(projector::finished_item(
                    item,
                    ItemStatus::Interrupted,
                    now,
                )));
            }
            for message in child
                .messages
                .iter()
                .filter(|m| m.run_id.is_none() && m.streaming)
            {
                let mut m = message.clone();
                m.streaming = false;
                m.updated_at = now.clone();
                payloads.push(EventPayload::MessageUpdated(m));
            }
            for turn in child.provider_turns.iter().filter(|t| {
                t.run_attempt_id.is_none()
                    && matches!(t.status, TurnStatus::Pending | TurnStatus::Running)
            }) {
                let mut t = turn.clone();
                t.status = TurnStatus::Interrupted;
                t.completed_at = Some(now.clone());
                payloads.push(EventPayload::ProviderTurnUpdated(t));
            }
            for request in child.runtime_requests.iter().filter(|r| {
                r.status == RequestStatus::Pending
                    && child
                        .nodes
                        .iter()
                        .any(|n| n.id == r.node_id && n.run_id.is_none())
            }) {
                let mut r = request.clone();
                r.status = RequestStatus::Cancelled;
                r.resolved_at = Some(now.clone());
                payloads.push(EventPayload::RuntimeRequestUpdated(r));
            }
            committed.extend(commit_decision(
                connection,
                Decision {
                    events: crate::events(
                        &child.thread.id,
                        &format!("native-stop-child:{}:{}", task.id, now.as_str()),
                        payloads,
                        now,
                    ),
                    ..Default::default()
                },
                None,
                now,
            )?);
            // A later user-created run in this child has independent ownership.
            let mut original = child.clone();
            original.subagents.retain(|t| t.run_id.is_none());
            committed.extend(stop_children(connection, &original, None, now, visited)?);
        }
    }
    Ok(committed)
}
impl Store {
    pub fn ingest_native(
        &self,
        events: Vec<DomainEvent>,
        controller_thread: &ThreadId,
        run_id: &RunId,
        attempt: &RunAttemptId,
        owner: &NativeSubagentOwner,
        now: &Timestamp,
    ) -> Result<Commit> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let empty = || {
            Ok(Commit {
                sequence: latest_sequence(&tx, None)?,
                events: vec![],
                replayed: false,
            })
        };
        let Some(root) = thread(&tx, controller_thread)? else {
            return empty();
        };
        let Some(run): Option<Run> =
            entity(&tx, "runs", "run_id", controller_thread, run_id.as_str())?
        else {
            return empty();
        };
        if root.deleted_at.is_some()
            || root.rollback_request_id.is_some()
            || run.active_attempt_id.as_ref() != Some(attempt)
            || run.status == RunStatus::RolledBack
        {
            return empty();
        }
        let Some(task) = events
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::SubagentUpdated(s)
                    if s.id == owner.task_id && s.thread_id == owner.parent_thread_id =>
                {
                    Some(s)
                }
                _ => None,
            })
            .cloned()
            .or(entity(
                &tx,
                "subagents",
                "subagent_id",
                &owner.parent_thread_id,
                owner.task_id.as_str(),
            )?)
        else {
            return Err(StoreError::InvalidEvent("native owner has no task".into()));
        };
        if task.origin != SubagentOrigin::ProviderNative
            || task.thread_id != owner.parent_thread_id
            || task
                .native_task_ref
                .as_ref()
                .is_none_or(|r| r.strength != Strength::Strong)
        {
            return Err(StoreError::InvalidEvent(
                "native owner is not provider owned".into(),
            ));
        }
        let Some(child_id) = &task.child_thread_id else {
            return Err(StoreError::InvalidEvent("native owner has no child".into()));
        };
        let closing = run
            .delegated_completion
            .as_ref()
            .is_some_and(|c| c.disposition != CohortDisposition::Open)
            || !matches!(
                run.status,
                RunStatus::Starting
                    | RunStatus::Running
                    | RunStatus::Waiting
                    | RunStatus::Completed
            );
        if closing
            && (task.status != NodeStatus::Interrupted
                || events.iter().any(|e| match &e.payload {
                    EventPayload::SubagentUpdated(s) => s.status != NodeStatus::Interrupted,
                    EventPayload::NodeUpdated(n) => !terminal_node(n.status),
                    EventPayload::TurnItemUpdated(i) => !terminal_item(i.status),
                    EventPayload::MessageUpdated(m) => m.streaming,
                    EventPayload::ProviderTurnUpdated(t) => !matches!(
                        t.status,
                        TurnStatus::Interrupted
                            | TurnStatus::Cancelled
                            | TurnStatus::Failed
                            | TurnStatus::Completed
                    ),
                    EventPayload::ProviderThreadUpdated(_)
                    | EventPayload::ProviderSessionUpdated(_)
                    | EventPayload::RuntimeRequestUpdated(_) => false,
                    _ => true,
                }))
        {
            return empty();
        }
        let Some(parent) = thread(&tx, &owner.parent_thread_id)? else {
            return empty();
        };
        if parent.deleted_at.is_some() || parent.rollback_request_id.is_some() {
            return empty();
        }
        let mut ancestor = parent.clone();
        let mut visited = BTreeSet::new();
        while ancestor.id != root.id {
            if !visited.insert(ancestor.id.clone())
                || visited.len() > 128
                || ancestor.lineage.relationship_to_parent != Some(Relationship::Subagent)
            {
                return Err(StoreError::InvalidEvent(
                    "native task is outside its controller".into(),
                ));
            }
            let Some(id) = &ancestor.lineage.parent_thread_id else {
                return Err(StoreError::InvalidEvent("native ancestor missing".into()));
            };
            let Some(next) = thread(&tx, id)? else {
                return empty();
            };
            ancestor = next;
        }
        if let Some(previous) = entity::<Subagent>(
            &tx,
            "subagents",
            "subagent_id",
            &owner.parent_thread_id,
            owner.task_id.as_str(),
        )? {
            if previous.origin != task.origin
                || previous.child_thread_id != task.child_thread_id
                || previous.parent_node_id != task.parent_node_id
                || previous.native_task_ref != task.native_task_ref
            {
                return Err(StoreError::InvalidEvent(
                    "native task identity changed".into(),
                ));
            }
            if closing && terminal_node(previous.status) {
                return empty();
            }
            if terminal_node(previous.status)
                && !terminal_node(task.status)
                && !run.status.is_blocking()
            {
                return empty();
            }
        } else if closing || task.run_id.as_ref().is_some_and(|id| id != run_id) {
            return empty();
        }
        let created = events.iter().find_map(|e| match &e.payload {
            EventPayload::ThreadCreated(t) if &t.id == child_id => Some(t.clone()),
            _ => None,
        });
        let Some(child) = thread(&tx, child_id)?.or(created) else {
            return Err(StoreError::InvalidEvent("native child missing".into()));
        };
        if child.deleted_at.is_some() || child.rollback_request_id.is_some() {
            return empty();
        }
        if child.project_id != parent.project_id
            || child.lineage.parent_thread_id.as_ref() != Some(&parent.id)
            || child.lineage.relationship_to_parent != Some(Relationship::Subagent)
        {
            return Err(StoreError::InvalidEvent(
                "native child lineage changed".into(),
            ));
        }
        for event in &events {
            let valid = if event.thread_id == parent.id {
                match &event.payload {
                    EventPayload::SubagentUpdated(s) => s.id == task.id && s.thread_id == parent.id,
                    EventPayload::NodeUpdated(n) => {
                        n.id == task.id
                            && n.kind == NodeKind::Subagent
                            && !n.counts_for_run
                            && n.thread_id == parent.id
                            && n.run_id == task.run_id
                    }
                    EventPayload::TurnItemUpdated(i) => {
                        i.thread_id == parent.id
                            && i.node_id.as_ref() == Some(&task.id)
                            && i.run_id == task.run_id
                            && matches!(&i.body,TurnItemBody::Subagent { subagent_id,origin,.. } if subagent_id==&task.id && *origin==SubagentOrigin::ProviderNative)
                    }
                    _ => false,
                }
            } else if &event.thread_id == child_id {
                match &event.payload {
                    EventPayload::ThreadCreated(t) => t.id == child.id && t == &child,
                    EventPayload::NodeUpdated(n) => {
                        n.thread_id == child.id && n.run_id.is_none() && !n.counts_for_run
                    }
                    EventPayload::TurnItemUpdated(i) => {
                        i.thread_id == child.id && i.run_id.is_none()
                    }
                    EventPayload::TurnItemTextDelta(d) => d.run_id.is_none(),
                    EventPayload::MessageUpdated(m) => {
                        m.thread_id == child.id && m.run_id.is_none()
                    }
                    EventPayload::PlanUpdated(p) => p.thread_id == child.id && p.run_id.is_none(),
                    EventPayload::ProviderTurnUpdated(t) => t.run_attempt_id.is_none(),
                    EventPayload::ProviderThreadUpdated(p) => {
                        p.app_thread_id.as_ref() == Some(child_id) && p.driver == task.driver
                    }
                    EventPayload::RuntimeRequestUpdated(_) => true,
                    _ => false,
                }
            } else {
                false
            };
            if !valid {
                return Err(StoreError::InvalidEvent(
                    "native event crossed its task boundary".into(),
                ));
            }
        }
        let mut stored = commit_decision(
            &tx,
            Decision {
                events,
                ..Default::default()
            },
            None,
            now,
        )?;
        if let Some(child) = load_projection(&tx, child_id)? {
            stored.extend(commit_decision(
                &tx,
                decider::after_terminal(
                    &child,
                    now,
                    &format!("native:{}:{}", task.id, now.as_str()),
                ),
                None,
                now,
            )?);
        }
        if terminal_node(task.status) {
            stored.extend(reconcile_delegations(&tx, controller_thread, now)?);
        }
        let sequence = latest_sequence(&tx, None)?;
        tx.commit()?;
        self.publish(&stored);
        Ok(Commit {
            sequence,
            events: stored,
            replayed: false,
        })
    }
}

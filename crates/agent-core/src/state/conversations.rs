//! The client owns one replica per conversation. Display models are disposable
//! projections; only ordered rows can change saved conversation state.
use super::*;
use crate::{peer::PeerError, session::SessionRef};
use agent_protocol::conversation::records;

#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct Opened {
    pub replica: records::Replica,
    pub subscription: uuid::Uuid,
}

pub(crate) fn opened(
    base: &records::Replica,
    response: records::Opened,
) -> Result<Opened, PeerError> {
    records::validate_message(&response.initial)
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    if let records::Message::Snapshot { cursor, .. } = &response.initial
        && base
            .cursor()
            .is_some_and(|current| cursor.precedes(current))
    {
        return Err(PeerError::InvalidMessage(
            "conversation snapshot precedes its cached cursor".into(),
        ));
    }
    let replica = base
        .apply(base.subscription(), response.initial)
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    let agent_protocol::sync::Scope::Conversation(id) = replica.scope() else {
        return Err(PeerError::InvalidMessage(
            "expected a conversation scope".into(),
        ));
    };
    if !replica
        .rows()
        .contains_key(&agent_protocol::sync::Key::Thread(id.clone()))
    {
        return Err(PeerError::InvalidMessage(
            "conversation header is missing".into(),
        ));
    }
    Ok(Opened {
        replica,
        subscription: response.subscription_id,
    })
}

#[derive(Debug)]
struct CheckReceipts {
    session: crate::session::SessionRef,
    operations: Vec<uuid::Uuid>,
}

pub(crate) fn receipt_check(
    snapshot: &Snapshot,
    session: &crate::session::SessionRef,
) -> Option<Effect> {
    let key = DraftKey::from(session);
    let operations: Vec<_> = snapshot
        .sessions
        .pending_submissions
        .iter()
        .filter(|(_, pending)| pending.draft_key == key)
        .filter_map(|(id, _)| uuid::Uuid::parse_str(id.as_str()).ok())
        .collect();
    (!operations.is_empty()).then(|| {
        Effect::execute(CheckReceipts {
            session: session.clone(),
            operations,
        })
    })
}

impl op::Operation for CheckReceipts {
    const BACKGROUND: bool = true;
    const REPORT_ERROR: bool = false;
    fn navigation_bound(&self) -> bool {
        false
    }
    type Input = uuid::Uuid;
    fn capture(&self, snapshot: &Snapshot, _pane: &str) -> Result<Self::Input, PeerError> {
        read_scope(snapshot, &self.session)
    }
    type Output = (uuid::Uuid, records::Receipts);
    async fn run(
        &self,
        scope: Self::Input,
        context: &mut crate::store::Execution<'_>,
    ) -> Result<Self::Output, PeerError> {
        let mut receipts = Vec::new();
        for operations in self
            .operations
            .chunks(records::CheckCommands::MAX_OPERATIONS)
        {
            receipts.extend(
                context
                    .call(&records::CheckCommands {
                        session: self.session.clone(),
                        operations: operations.to_vec(),
                    })
                    .await?,
            );
        }
        Ok((scope, receipts))
    }
    fn apply(
        self,
        snapshot: &mut Snapshot,
        _pane: &str,
        (scope, receipts): Self::Output,
    ) -> Vec<Effect> {
        if snapshot.sessions.subscriptions.get(&self.session) != Some(&scope) {
            return Vec::new();
        }
        let key = DraftKey::from(&self.session);
        for (operation, receipt) in receipts {
            let id = snapshot
                .sessions
                .pending_submissions
                .iter()
                .find(|(id, pending)| {
                    pending.draft_key == key && uuid::Uuid::parse_str(id.as_str()) == Ok(operation)
                })
                .map(|(id, _)| id.clone());
            let Some(id) = id else {
                continue;
            };
            if receipt.outcome == agent_protocol::conversation::Outcome::DeliveryUnknown {
                if let Some(pending) = shared_mut(&mut snapshot.sessions.pending_submissions, &id) {
                    pending.delivery_unknown = true;
                }
            } else if let agent_protocol::conversation::Outcome::Rejected(reason) = receipt.outcome
            {
                *snapshot = super::reduce_event(snapshot, Event::SubmissionFailed(id)).0;
                snapshot.fail_operation(
                    op::OperationKey::Submission {
                        draft_key: key.clone(),
                    },
                    reason,
                );
            } else if let Some(pending) =
                shared_mut(&mut snapshot.sessions.pending_submissions, &id)
            {
                pending.cursor = Some(receipt.cursor);
                pending.accepted = true;
                pending.delivery_unknown = false;
            }
        }
        super::reconcile_pending(snapshot, &self.session);
        Vec::new()
    }
}

pub(crate) fn read_scope(
    snapshot: &Snapshot,
    id: &crate::session::SessionRef,
) -> Result<uuid::Uuid, PeerError> {
    let subscription = snapshot
        .sessions
        .subscriptions
        .get(id)
        .copied()
        .ok_or_else(|| PeerError::InvalidMessage("conversation is not subscribed".into()))?;
    snapshot
        .sessions
        .replicas
        .get(id)
        .and_then(|replica| replica.cursor())
        .ok_or_else(|| PeerError::InvalidMessage("conversation cursor is missing".into()))?;
    Ok(subscription)
}

pub(crate) fn hydrate_body(
    snapshot: &mut Snapshot,
    id: &SessionRef,
    version: u64,
    item: crate::models::Item,
) -> Result<Vec<Effect>, PeerError> {
    let Some(replica) = snapshot.sessions.replicas.get(id) else {
        return Ok(Vec::new());
    };
    let Some(row) = agent_protocol::conversation::records::hydrate_body(replica, version, item)
    else {
        return Ok(Vec::new());
    };
    let cursor = replica
        .cursor()
        .ok_or_else(|| PeerError::InvalidMessage("body has no conversation cursor".into()))?;
    apply(
        snapshot,
        id,
        vec![records::Message::Hydrate { cursor, row }],
    )
    .map_err(|error| PeerError::InvalidMessage(error.to_string()))
}

pub(super) fn promote_draft(snapshot: &mut Snapshot, id: &SessionRef, cwd: Option<&str>) {
    snapshot.input.retire_new_chat(id);
    let draft = DraftKey::from(id);
    if !snapshot
        .panes
        .0
        .values()
        .any(|pane| pane.navigation.thread_id.is_none() && pane.navigation.draft_key == draft)
    {
        return;
    }
    for pane in Arc::make_mut(&mut snapshot.panes.0).values_mut() {
        if pane.navigation.thread_id.is_none() && pane.navigation.draft_key == DraftKey::from(id) {
            let navigation = Arc::make_mut(&mut Arc::make_mut(pane).navigation);
            navigation.thread_id = Some(id.clone());
            if let Some(cwd) = cwd {
                navigation.cwd = cwd.to_owned();
            }
        }
    }
}

pub(crate) fn install(
    snapshot: &mut Snapshot,
    output: Opened,
) -> Option<crate::models::ThreadResponse> {
    let agent_protocol::sync::Scope::Conversation(id) = output.replica.scope() else {
        return None;
    };
    let id = id.clone();
    let previous = snapshot.sessions.replicas.get(&id)?;
    if previous.subscription() != output.replica.subscription() {
        return None;
    }
    // Keep live rows received while the replacement subscription was opening.
    // Its queued changes still start at the captured cursor and harmlessly
    // catch up; older stream messages are fenced by the new subscription ID.
    let replica = match (previous.cursor(), output.replica.cursor()) {
        (Some(current), Some(opened)) if opened.precedes(current) => previous.as_ref().clone(),
        _ => output.replica,
    };
    let mut projected = records::project(&replica)?;
    projected.thread = super::outputs::overlay(projected.thread, &snapshot.sessions.live_outputs);
    promote_draft(snapshot, &id, projected.thread.cwd.as_deref());
    Arc::make_mut(&mut snapshot.sessions.replicas).insert(id.clone(), Arc::new(replica));
    Arc::make_mut(&mut snapshot.sessions.conversations)
        .insert(id.clone(), Arc::new(projected.thread.clone()));
    Arc::make_mut(&mut snapshot.sessions.subscriptions).insert(id.clone(), output.subscription);
    super::reconcile_pending(snapshot, &id);
    Some(projected)
}

pub(crate) fn received(
    previous: &Snapshot,
    subscription_id: uuid::Uuid,
    message: records::Message,
) -> (Snapshot, Vec<Effect>) {
    let Some(id) = previous
        .sessions
        .subscriptions
        .iter()
        .find_map(|(id, subscription)| (*subscription == subscription_id).then_some(id.clone()))
    else {
        return (previous.clone(), Vec::new());
    };
    let mut next = previous.clone();
    let result = apply(&mut next, &id, vec![message]);
    match result {
        Ok(effects) => (next, effects),
        Err(_) => {
            Arc::make_mut(&mut next.sessions.subscriptions).remove(&id);
            (next, vec![Effect::execute(op::ReadThread::new(id))])
        }
    }
}

pub(crate) fn apply(
    snapshot: &mut Snapshot,
    id: &crate::session::SessionRef,
    messages: Vec<records::Message>,
) -> Result<Vec<Effect>, agent_protocol::sync::ApplyError> {
    let mut details = Vec::new();
    for message in &messages {
        records::validate_message(message)?;
        let rows = match message {
            records::Message::Snapshot { rows, .. }
            | records::Message::Changes { rows, .. }
            | records::Message::Extend { rows, .. } => rows.as_slice(),
            records::Message::Hydrate { row, .. } => std::slice::from_ref(row),
            _ => &[],
        };
        for row in rows {
            let value = match &row.content {
                agent_protocol::sync::Content::Full(value)
                | agent_protocol::sync::Content::Summary(value) => value,
            };
            if let records::Record::Item(item) = value.as_ref()
                && item.content.is_deferred()
            {
                details.extend(op::item_detail(id, &item.turn, &item.content));
            }
        }
    }
    let Some(held) = snapshot.sessions.replicas.get(id) else {
        return Ok(Vec::new());
    };
    let before = snapshot.sessions.conversations.get(id).cloned();
    let mut replica = held.as_ref().clone();
    for message in messages {
        if let records::Message::Snapshot { cursor, .. } = &message
            && replica
                .cursor()
                .is_some_and(|current| cursor.precedes(current))
        {
            return Err(agent_protocol::sync::ApplyError::Version);
        }
        replica = replica.apply(replica.subscription(), message)?;
    }
    let projected = records::project(&replica);
    Arc::make_mut(&mut snapshot.sessions.replicas).insert(id.clone(), Arc::new(replica));
    let Some(mut projected) = projected else {
        Arc::make_mut(&mut snapshot.sessions.conversations).remove(id);
        return Ok(Vec::new());
    };
    projected.thread = super::outputs::overlay(projected.thread, &snapshot.sessions.live_outputs);
    if let (Some(previous), Some(turns)) = (before.as_ref(), projected.thread.turns.as_mut()) {
        for turn in turns {
            if let Some(old) = previous
                .turns
                .iter()
                .flatten()
                .find(|old| old.id == turn.id)
                && old.as_ref() == turn.as_ref()
            {
                *turn = old.clone();
            }
        }
    }
    promote_draft(snapshot, id, projected.thread.cwd.as_deref());
    let thread = Arc::new(projected.thread);
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(id.clone(), thread.clone());
    super::reconcile_pending(snapshot, id);
    super::outputs::retire(snapshot);
    let completed = thread.turns.iter().flatten().any(|turn| {
        turn.status != crate::models::TurnStatus::Running
            && before
                .as_ref()
                .and_then(|before| before.turns.as_ref())
                .and_then(|turns| turns.iter().find(|old| old.id == turn.id))
                .is_some_and(|old| old.status == crate::models::TurnStatus::Running)
    });
    let mut effects = details;
    if let Some(before) = before {
        let previous_items: HashMap<_, _> = before
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .map(|item| (&item.id, item))
            .collect();
        let changed_items: Vec<_> = thread
            .turns
            .iter()
            .flatten()
            .flat_map(|turn| turn.items.iter().flatten())
            .filter(|item| previous_items.get(&item.id).is_none_or(|old| *old != *item))
            .collect();
        let work_finished = changed_items
            .iter()
            .any(|item| finished_workspace_item(item));
        let discovered_agents = changed_items.iter().any(|item| matches!(item.body(), crate::models::ItemBody::Subagent { receivers, .. } if receivers.iter().any(|receiver| !snapshot.sessions.conversations.contains_key(receiver))));
        effects.extend(context_effects(
            snapshot,
            id,
            before.cwd.as_deref(),
            thread.cwd.as_deref(),
            completed
                || op::preparing_workspace(&before.preparations, before.workspace_setup.as_ref())
                    && !op::preparing_workspace(
                        &thread.preparations,
                        thread.workspace_setup.as_ref(),
                    )
                || work_finished,
            discovered_agents,
        ));
        if (completed || work_finished)
            && let Some(cwd) = &thread.cwd
        {
            effects.extend(op::refresh_file_trees(snapshot, cwd));
        }
    }
    if completed {
        snapshot.prune_subscriptions();
    }
    Ok(effects)
}

fn finished_workspace_item(item: &crate::models::Item) -> bool {
    !matches!(
        item.status,
        crate::models::ItemStatus::Running | crate::models::ItemStatus::Unknown
    ) && matches!(
        item.body(),
        crate::models::ItemBody::FileChange { .. }
            | crate::models::ItemBody::CommandExecution { .. }
    )
}

fn context_effects(
    snapshot: &mut Snapshot,
    id: &crate::session::SessionRef,
    previous_cwd: Option<&str>,
    cwd: Option<&str>,
    review: bool,
    discovered_agents: bool,
) -> Vec<Effect> {
    let mut effects = Vec::new();
    let panes = snapshot.panes.0.clone();
    for (pane_id, pane) in panes.iter() {
        let moved = pane.navigation.thread_id.as_ref() == Some(id)
            && previous_cwd != cwd
            && cwd.unwrap_or_default() != pane.navigation.cwd;
        if moved {
            let navigation = Navigation {
                cwd: cwd.unwrap_or_default().into(),
                ..pane.navigation.as_ref().clone()
            };
            snapshot.invalidate_pane(pane_id);
            super::navigate(snapshot, pane_id, navigation);
        }
        if discovered_agents
            && fleet_contains(
                pane.observed_agents.as_ref(),
                id,
                &snapshot.sessions.conversations,
            )
        {
            effects.extend(
                op::refresh_agents(snapshot.connection.connected, pane.observed_agents.as_ref())
                    .into_iter()
                    .map(|effect| effect.in_pane(pane_id)),
            );
        }
        if moved || review && previous_cwd == Some(pane.navigation.cwd.as_str()) {
            effects.extend(
                op::review_workspace(snapshot, pane_id)
                    .into_iter()
                    .map(|effect| effect.in_pane(pane_id)),
            );
        }
    }
    effects
}

fn fleet_contains(
    root: Option<&SessionRef>,
    session: &SessionRef,
    conversations: &BTreeMap<SessionRef, Arc<Thread>>,
) -> bool {
    let Some(root) = root else {
        return false;
    };
    let mut ancestor = session;
    // The number of known sessions also bounds malformed parent cycles.
    for _ in 0..=conversations.len() {
        if ancestor == root {
            return true;
        }
        let Some(parent) = conversations
            .get(ancestor)
            .and_then(|thread| thread.parent_id.as_ref())
        else {
            return false;
        };
        ancestor = parent;
    }
    false
}

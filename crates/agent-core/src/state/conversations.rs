//! Each conversation holds T3's canonical snapshot. Display DTOs and queried
//! details are disposable; only the thread stream advances its saved sequence.
use super::*;
use crate::{peer::PeerError, session::SessionRef};
use agent_protocol::conversation::records;

#[cfg(test)]
mod context_tests;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone)]
pub struct Capture {
    pub generation: u64,
    pub projection: Option<Arc<agent_protocol::orchestration::projection::ThreadSnapshot>>,
}

pub(crate) fn capture(snapshot: &Snapshot, id: &SessionRef) -> Result<Capture, PeerError> {
    let generation = snapshot
        .sessions
        .subscription_generations
        .get(id)
        .copied()
        .ok_or_else(|| {
            PeerError::InvalidMessage("conversation subscription was not scheduled".into())
        })?;
    Ok(Capture {
        generation,
        projection: snapshot.sessions.projections.get(id).cloned(),
    })
}

#[derive(Debug)]
pub struct Opened {
    pub session: SessionRef,
    pub projection: agent_protocol::orchestration::projection::ThreadSnapshot,
    pub subscription: uuid::Uuid,
    pub generation: u64,
    pub provider: crate::session::ProviderKind,
    pub capabilities: crate::session::Capabilities,
    pub project_root: Option<String>,
    pub replaced: bool,
}

pub(crate) fn opened(base: &Capture, response: records::Opened) -> Result<Opened, PeerError> {
    let replaced = response.initial.iter().any(|item| {
        matches!(
            item,
            agent_protocol::orchestration::projection::ThreadStreamItem::Snapshot(_)
        )
    });
    let mut projection = base.projection.as_deref().cloned();
    for message in response.initial {
        projection = crate::orchestration::receive(
            &response.session.application_id(),
            projection.as_ref(),
            message,
        )
        .map_err(|error| PeerError::InvalidMessage(error.into()))?;
    }
    Ok(Opened {
        session: response.session,
        projection: projection
            .ok_or_else(|| PeerError::InvalidMessage("conversation snapshot is missing".into()))?,
        subscription: response.subscription_id,
        generation: base.generation,
        provider: response.provider,
        capabilities: response.capabilities,
        project_root: response.project_root,
        replaced,
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
            if let Some(reason) = receipt.error {
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
                pending.cursor = Some(receipt.sequence);
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
    if !snapshot.sessions.projections.contains_key(id) {
        return Err(PeerError::InvalidMessage(
            "conversation snapshot is missing".into(),
        ));
    }
    Ok(subscription)
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
    let id = output.session;
    if snapshot.sessions.subscription_generations.get(&id) != Some(&output.generation) {
        return None;
    }
    if output.replaced {
        Arc::make_mut(&mut snapshot.sessions.expanded_history).remove(&id);
    }
    let projected = crate::orchestration::project(
        &output.projection,
        &id,
        output.provider,
        output.capabilities,
        output.project_root.as_deref(),
        &snapshot.sessions.item_details,
    )
    .ok()?;
    Arc::make_mut(&mut snapshot.sessions.metadata).insert(
        id.clone(),
        ConversationMetadata {
            provider: output.provider,
            capabilities: output.capabilities,
            project_id: output.projection.projection.thread.project_id.clone(),
            project_root: output.project_root,
        },
    );
    Arc::make_mut(&mut snapshot.sessions.projections)
        .insert(id.clone(), Arc::new(output.projection));
    let mut projected = projected;
    projected.thread = super::outputs::overlay(projected.thread, &snapshot.sessions.live_outputs);
    if let Some(turns) = projected.thread.turns.take() {
        projected.thread.turns = Some(share_turns(
            snapshot
                .sessions
                .conversations
                .get(&id)
                .and_then(|thread| thread.turns.as_deref())
                .unwrap_or_default(),
            turns,
        ));
    }
    promote_draft(snapshot, &id, projected.thread.cwd.as_deref());
    Arc::make_mut(&mut snapshot.sessions.conversations)
        .insert(id.clone(), Arc::new(projected.thread.clone()));
    Arc::make_mut(&mut snapshot.sessions.subscriptions).insert(id.clone(), output.subscription);
    super::follow_up::reconcile_edit(snapshot, &id);
    super::reconcile_pending(snapshot, &id);
    Some(projected)
}

pub(crate) fn received(
    previous: &Snapshot,
    subscription_id: uuid::Uuid,
    message: agent_protocol::orchestration::projection::ThreadStreamItem,
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
    match apply(&mut next, &id, vec![message]) {
        Ok(effects) => (next, effects),
        Err(_) => {
            Arc::make_mut(&mut next.sessions.subscriptions).remove(&id);
            (next, vec![Effect::execute(op::ReadThread::new(id))])
        }
    }
}

fn share_turns(
    previous: &[Arc<crate::models::Turn>],
    turns: Vec<Arc<crate::models::Turn>>,
) -> Vec<Arc<crate::models::Turn>> {
    turns
        .into_iter()
        .map(|mut turn| {
            let Some(old) = previous
                .iter()
                .rev()
                .find(|old| old.matches_occurrence(&turn))
            else {
                return turn;
            };
            if old == &turn {
                return old.clone();
            }
            if let Some(items) = Arc::make_mut(&mut turn).items.as_mut() {
                for item in items {
                    if let Some(old) = old.items.iter().flatten().find(|old| *old == item) {
                        *item = old.clone();
                    }
                }
            }
            turn
        })
        .collect()
}

/// A moved thread must not inherit its previous project's subscription root.
pub(crate) fn project_root<'a>(
    metadata: Option<&'a ConversationMetadata>,
    projects: &'a [crate::models::Project],
    project_id: Option<&str>,
) -> Option<&'a str> {
    metadata
        .filter(|metadata| metadata.project_id.as_deref() == project_id)
        .and_then(|metadata| metadata.project_root.as_deref())
        .or_else(|| {
            project_id
                .and_then(|id| projects.iter().find(|project| project.id == id))
                .filter(|project| project.roots.len() == 1)
                .and_then(|project| project.roots.first())
                .map(|root| root.path.as_str())
        })
}

pub(crate) fn refresh(snapshot: &mut Snapshot, id: &SessionRef) -> Result<Vec<Effect>, PeerError> {
    let held = snapshot
        .sessions
        .projections
        .get(id)
        .ok_or_else(|| PeerError::InvalidMessage("conversation snapshot is missing".into()))?;
    let before = snapshot.sessions.conversations.get(id).cloned();
    let provider = crate::session::ProviderKind::from_instance_id(
        &held.projection.thread.provider_instance_id,
    )
    .map_err(PeerError::InvalidMessage)?;
    let metadata = snapshot.sessions.metadata.get(id);
    let capabilities = metadata
        .map(|metadata| metadata.capabilities)
        .unwrap_or_default();
    let mut projected = crate::orchestration::project(
        held,
        id,
        metadata.map_or(provider, |metadata| metadata.provider),
        capabilities,
        project_root(
            metadata,
            snapshot
                .lists
                .threads
                .as_ref()
                .map_or(&[], |list| list.projects.as_slice()),
            held.projection.thread.project_id.as_deref(),
        ),
        &snapshot.sessions.item_details,
    )
    .map_err(PeerError::InvalidMessage)?;
    projected.thread = super::outputs::overlay(projected.thread, &snapshot.sessions.live_outputs);
    if let Some(turns) = projected.thread.turns.take() {
        projected.thread.turns = Some(share_turns(
            before
                .as_ref()
                .and_then(|thread| thread.turns.as_deref())
                .unwrap_or_default(),
            turns,
        ));
    }
    promote_draft(snapshot, id, projected.thread.cwd.as_deref());
    let thread = Arc::new(projected.thread);
    Arc::make_mut(&mut snapshot.sessions.conversations).insert(id.clone(), thread.clone());
    // Cached queues are cleared while restoring. Only a live subscription's
    // projection can establish that an edited message has left the queue.
    if snapshot.sessions.subscriptions.contains_key(id) {
        super::follow_up::reconcile_edit(snapshot, id);
    }
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
    let mut effects = Vec::new();
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
        let discovered_agents = changed_items.iter().any(|item| matches!(item.body(), crate::models::ItemBody::Subagent { receivers, .. } if receivers.iter().any(|receiver| !snapshot.sessions.conversations.keys().any(|id|id.application_id()==receiver.application_id()))));
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

pub(crate) fn apply(
    snapshot: &mut Snapshot,
    id: &SessionRef,
    messages: Vec<agent_protocol::orchestration::projection::ThreadStreamItem>,
) -> Result<Vec<Effect>, PeerError> {
    let mut current = snapshot
        .sessions
        .projections
        .get(id)
        .map(|snapshot| snapshot.as_ref().clone());
    for message in messages {
        current = crate::orchestration::receive(&id.application_id(), current.as_ref(), message)
            .map_err(|error| PeerError::InvalidMessage(error.into()))?;
    }
    let Some(current) = current else {
        return Ok(Vec::new());
    };
    Arc::make_mut(&mut snapshot.sessions.projections).insert(id.clone(), Arc::new(current));
    refresh(snapshot, id)
}

fn finished_workspace_item(item: &crate::models::Item) -> bool {
    !matches!(
        item.status,
        crate::models::ItemStatus::Running | crate::models::ItemStatus::Unknown
    ) && matches!(
        item.body(),
        crate::models::ItemBody::FileChange { .. }
            | crate::models::ItemBody::CommandExecution { .. }
            | crate::models::ItemBody::ToolCall { .. }
            | crate::models::ItemBody::Subagent { .. }
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
        if ancestor.application_id() == root.application_id() {
            return true;
        }
        let Some(parent) = conversations
            .iter()
            .find(|(id, _)| id.application_id() == ancestor.application_id())
            .and_then(|(_, thread)| thread.parent_id.as_ref())
        else {
            return false;
        };
        ancestor = parent;
    }
    false
}

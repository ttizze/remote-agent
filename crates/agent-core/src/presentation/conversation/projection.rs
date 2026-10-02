//! Reconcile snapshots and reuse unchanged conversation, turn, and queued-item identities.
use super::{
    ConversationRow, ConversationRowContent, PendingItems, RenderedConversation, RenderedItem,
    request, rows::render_turn,
};
use crate::{
    models,
    state::{PendingSubmission, Snapshot},
};
use std::{collections::HashMap, sync::Arc};

pub(super) fn project_conversation(
    snapshot: &Snapshot,
    source: Arc<models::Thread>,
    previous: &Option<Arc<RenderedConversation>>,
) -> Arc<RenderedConversation> {
    let draft_key = source
        .id
        .as_ref()
        .map(crate::state::DraftKey::from)
        .unwrap_or_else(|| snapshot.navigation.draft_key.clone());
    let mut pending: PendingItems = snapshot
        .pending_submissions
        .iter()
        .filter(|(_, pending)| pending.draft_key == draft_key)
        .map(|(id, pending)| (id.clone(), pending.clone()))
        .collect();
    pending.sort_by_key(|(_, pending)| pending.sequence);
    let requests = &source.requests;
    if let Some(previous) = previous
        && Arc::ptr_eq(&source, &previous.source)
        && pending
            .iter()
            .map(|(id, pending)| (id, Arc::as_ptr(pending)))
            .eq(previous
                .pending
                .iter()
                .map(|(id, pending)| (id, Arc::as_ptr(pending))))
    {
        return previous.clone();
    }
    let previous = previous.as_ref().filter(|old| source.id == old.source.id);
    let cached: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|old| &old.turns)
        .map(|turn| (turn.source.id.as_str(), turn))
        .collect();
    let native = source.turns.as_deref().unwrap_or_default();
    // A submission made before any history belongs before the first turn once
    // it arrives. An acknowledged queue entry still waits for its assigned turn.
    let pending_turn = |pending: &PendingSubmission| match pending.turn_id.as_deref() {
        Some(id) => native.iter().rposition(|turn| turn.id.as_str() == id),
        None if !pending.accepted && !native.is_empty() => Some(0),
        None => None,
    };
    let turns = native
        .iter()
        .enumerate()
        .map(|(index, turn)| {
            let pending = pending
                .iter()
                .filter(|(_, p)| pending_turn(p) == Some(index));
            let requests = requests.values().filter(|r| matches!(&r.target, agent_protocol::requests::RequestTarget::Turn { turn_id, .. } if turn_id == &turn.id));
            let old = cached.get(turn.id.as_str()).copied();
            if let Some(old) = old
                && Arc::ptr_eq(turn, &old.source)
                && pending
                    .clone()
                    .map(|(id, p)| (id, Arc::as_ptr(p)))
                    .eq(old.pending.iter().map(|(id, p)| (id, Arc::as_ptr(p))))
                && requests
                    .clone()
                    .map(Arc::as_ptr)
                    .eq(old.requests.iter().map(Arc::as_ptr))
            {
                return old.clone();
            }
            render_turn(
                source.id.as_ref().map(|id| id.provider),
                source.capabilities.unwrap_or_default().fork,
                turn.clone(),
                pending.cloned().collect(),
                requests.cloned().collect(),
                old,
            )
        })
        .collect();
    let queued: HashMap<_, _> = previous
        .into_iter()
        .flat_map(|old| &old.queued)
        .map(|item| (item.key(), item))
        .collect();
    let queued = pending
        .iter()
        .filter(|(_, p)| pending_turn(p).is_none())
        .map(|(id, p)| {
            RenderedItem::pending(
                id,
                p,
                queued
                    .get(&(Some(id.as_str()), Arc::as_ptr(p).cast()))
                    .copied(),
            )
        })
        .collect();
    let request_rows = requests
        .values()
        .filter(|request| match &request.target {
            agent_protocol::requests::RequestTarget::Session => true,
            agent_protocol::requests::RequestTarget::Turn { turn_id, .. } => {
                !native.iter().any(|turn| &turn.id == turn_id)
            }
        })
        .map(|source| ConversationRow {
            id: format!("request:{}", source.id),
            content: ConversationRowContent::PendingRequest {
                request: Box::new(request(source)),
            },
        })
        .collect();
    Arc::new(RenderedConversation {
        source,
        pending,
        turns,
        queued,
        request_rows,
    })
}

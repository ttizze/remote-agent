//! Client updates for T3's thread projection. Parent-thread events never mutate
//! inherited fork history: a later snapshot or history page refreshes that prefix.
use agent_protocol::orchestration::{self as model, projection::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub mod shell;
mod view;
pub(crate) use view::fork_point;
pub use view::project;
pub(crate) use view::{attachment_parts, attachments, context_input, context_parts};

/// T3 queries running item details under one `live` revision and settled
/// details under updatedAt. Query results never change the thread projection.
pub fn same_detail_revision(first: &model::TurnItem, second: &model::TurnItem) -> bool {
    let live = |status| {
        matches!(
            status,
            model::TurnItemStatus::Idle
                | model::TurnItemStatus::Pending
                | model::TurnItemStatus::Running
                | model::TurnItemStatus::Waiting
        )
    };
    first.thread_id == second.thread_id
        && first.id == second.id
        && if first.detail_omitted {
            // A complete authored-body hydration must describe this preview's
            // revision, including while its assistant is still streaming.
            first.updated_at == second.updated_at && live(first.status) == live(second.status)
        } else if live(first.status) {
            live(second.status)
        } else {
            !live(second.status) && first.updated_at == second.updated_at
        }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyOptions {
    pub partial_timeline: bool,
    pub latest_local_turn_ordinal: Option<u64>,
}

macro_rules! upsert {
    ($rows:expr, $value:expr) => {{
        let rows = &mut $rows;
        if let Some(saved) = rows.iter_mut().find(|item| item.id == $value.id) {
            *saved = $value.clone();
        } else {
            rows.push($value.clone());
        }
    }};
}

/// The client owner fences subscription generations before applying messages.
/// A replacement snapshot can reset a pruned cursor; replay events are deduped
/// only within that snapshot's stream. No row/window replica is involved.
pub fn receive(
    thread_id: &str,
    current: Option<&ThreadSnapshot>,
    message: ThreadStreamItem,
) -> Result<Option<ThreadSnapshot>, &'static str> {
    match message {
        ThreadStreamItem::Snapshot(snapshot) => {
            if snapshot.projection.thread.id != thread_id {
                return Err("conversation identity changed");
            }
            Ok(Some(snapshot))
        }
        ThreadStreamItem::Event { sequence, event } => {
            if event.thread_id != thread_id {
                return Err("conversation event belongs to another thread");
            }
            let current = current.ok_or("conversation snapshot is missing")?;
            if current.projection.thread.id != thread_id {
                return Err("conversation identity changed");
            }
            if sequence <= current.snapshot_sequence {
                return Ok(Some(current.clone()));
            }
            let partial = current.has_more_history || current.history_cursor.is_some();
            let projection = apply_event(
                &current.projection,
                &event,
                ApplyOptions {
                    partial_timeline: partial,
                    latest_local_turn_ordinal: current.latest_local_turn_ordinal,
                },
            );
            let latest_local_turn_ordinal = match &event.change {
                model::Change::TurnItem(item) if partial => Some(
                    current
                        .latest_local_turn_ordinal
                        .unwrap_or(0)
                        .max(item.ordinal),
                ),
                _ => current.latest_local_turn_ordinal,
            };
            Ok(Some(ThreadSnapshot {
                snapshot_sequence: sequence,
                projection,
                latest_local_turn_ordinal,
                ..current.clone()
            }))
        }
        ThreadStreamItem::Synchronized => Ok(current.cloned()),
    }
}

/// Older pages do not acknowledge live events. Their sequence describes the
/// page read, while the subscription alone advances the client's resume cursor.
pub fn receive_history(
    current: &ThreadSnapshot,
    requested_cursor: &str,
    page: ThreadHistoryPage,
) -> Option<ThreadSnapshot> {
    if current.history_cursor.as_deref() != Some(requested_cursor) {
        return None;
    }
    let mut projection = merge_older_history(&current.projection, &page.items);
    // A page cannot replace a newer live node observed by this subscription.
    for node in page.nodes {
        if !projection.nodes.iter().any(|saved| saved.id == node.id) {
            projection.nodes.push(node);
        }
    }
    Some(ThreadSnapshot {
        projection,
        history_cursor: page.next_cursor,
        has_more_history: page.has_more_history,
        ..current.clone()
    })
}

pub fn apply_event(
    projection: &ThreadProjection,
    event: &model::Event,
    options: ApplyOptions,
) -> ThreadProjection {
    if event.thread_id != projection.thread.id {
        return projection.clone();
    }
    if let model::Change::TurnItem(item) = &event.change
        && !projection
            .turn_items
            .iter()
            .any(|saved| saved.id == item.id)
        && options.partial_timeline
        && drop_missing_item(projection, item, options.latest_local_turn_ordinal)
    {
        return projection.clone();
    }
    let mut next = projection.clone();
    next.updated_at = event.occurred_at.clone();
    next.thread.updated_at = event.occurred_at.clone();
    use model::Change;
    match &event.change {
        Change::ThreadCreated(value) | Change::Thread(value) => next.thread = value.clone(),
        Change::RunCreated(value) | Change::Run(value) => {
            let was_queued = projection
                .runs
                .iter()
                .any(|run| run.id == value.id && run.status == model::RunStatus::Queued);
            upsert!(next.runs, value);
            retain_visible(&mut next);
            if was_queued && value.status != model::RunStatus::Queued {
                restore_queued(&mut next, &value.id);
            }
        }
        Change::RunAttemptCreated(value) | Change::RunAttempt(value) => {
            upsert!(next.attempts, value);
            retain_visible(&mut next);
        }
        Change::Node(value) => upsert!(next.nodes, value),
        Change::Subagent(value) => upsert!(next.subagents, value),
        Change::ProviderSessionAttached(value) | Change::ProviderSession(value) => {
            upsert!(next.provider_sessions, value)
        }
        Change::ProviderSessionDetached(value) => next
            .provider_sessions
            .retain(|session| session.id != value.provider_session_id),
        Change::ProviderThread(value) => upsert!(next.provider_threads, value),
        Change::ProviderTurn(value) => {
            let mut value = value.clone();
            if value.token_usage.is_none() {
                value.token_usage = projection
                    .provider_turns
                    .iter()
                    .find(|turn| turn.id == value.id)
                    .and_then(|turn| turn.token_usage.clone());
            }
            upsert!(next.provider_turns, value);
        }
        Change::RuntimeRequest(value) => upsert!(next.runtime_requests, value),
        Change::Plan(value) => upsert!(next.plans, value),
        Change::TurnItem(value) => apply_item(&mut next, value),
        Change::CheckpointScope(value) => upsert!(next.checkpoint_scopes, value),
        Change::Checkpoint(value) => upsert!(next.checkpoints, value),
        Change::ContextHandoff(value) => upsert!(next.context_handoffs, value),
        Change::ContextTransferCreated(value) | Change::ContextTransfer(value) => {
            upsert!(next.context_transfers, value)
        }
    }
    next
}

fn renumber(rows: &mut [ProjectedTurnItem]) {
    for (position, row) in rows.iter_mut().enumerate() {
        row.position = position as u64;
    }
}

fn retain_visible(projection: &mut ThreadProjection) {
    let visibility = TurnItemVisibility::new(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    );
    projection
        .visible_turn_items
        .retain(|row| row.visibility != Visibility::Local || visibility.visible(&row.item));
    renumber(&mut projection.visible_turn_items);
}

fn restore_queued(projection: &mut ThreadProjection, run_id: &str) {
    let visibility = TurnItemVisibility::new(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    );
    let items:Vec<_>=projection.turn_items.iter().filter(|item|item.run_id.as_deref()==Some(run_id)
        && matches!(&item.body,model::TurnItemBody::UserMessage {input_intent,..} if input_intent=="queued_turn")
        && visibility.visible(item)).cloned().collect();
    // The Host writes the starting run before its input's transcript position.
    // Until that update, the input stays after everything already shown.
    let last = projection
        .visible_turn_items
        .iter()
        .filter(|row| row.visibility == Visibility::Local)
        .map(|row| row.item.ordinal)
        .max()
        .unwrap_or(0);
    for item in items {
        let item = if item.ordinal == 0 {
            Arc::new(model::TurnItem {
                ordinal: last + 1,
                ..(*item).clone()
            })
        } else {
            item
        };
        upsert_visible(&mut projection.visible_turn_items, item);
    }
}

fn apply_item(projection: &mut ThreadProjection, item: &model::TurnItem) {
    let previous = projection
        .turn_items
        .iter()
        .find(|saved| saved.id == item.id);
    let recheck = matches!(item.body, model::TurnItemBody::RunInterruptRequest { .. })
        || previous.is_some_and(|saved| {
            matches!(saved.body, model::TurnItemBody::RunInterruptRequest { .. })
        });
    let item = Arc::new(item.clone());
    upsert!(projection.turn_items, item);
    if recheck {
        retain_visible(projection);
    }
    let visible = TurnItemVisibility::new(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    )
    .visible(&item);
    if visible {
        upsert_visible(&mut projection.visible_turn_items, item);
    } else {
        projection
            .visible_turn_items
            .retain(|row| row.source_item_id != item.id);
        renumber(&mut projection.visible_turn_items);
    }
}

fn upsert_visible(rows: &mut Vec<ProjectedTurnItem>, item: Arc<model::TurnItem>) {
    let previous = rows.iter().position(|row| row.source_item_id == item.id);
    let mut next = ProjectedTurnItem {
        position: 0,
        visibility: Visibility::Local,
        source_thread_id: item.thread_id.clone(),
        source_item_id: item.id.clone(),
        item,
    };
    if let Some(index) = previous {
        let saved = &rows[index];
        if saved.visibility == Visibility::Local
            && saved.source_thread_id == next.source_thread_id
            && saved.item.ordinal == next.item.ordinal
        {
            next.position = saved.position;
            rows[index] = next;
            return;
        }
        rows.remove(index);
    }
    let insertion = rows
        .iter()
        .position(|row| {
            row.visibility == Visibility::Local
                && (row.item.ordinal, row.item.id.as_str())
                    > (next.item.ordinal, next.item.id.as_str())
        })
        .unwrap_or(rows.len());
    rows.insert(insertion, next);
    renumber(rows);
}

fn drop_missing_item(
    projection: &ThreadProjection,
    item: &model::TurnItem,
    latest: Option<u64>,
) -> bool {
    if projection
        .visible_turn_items
        .iter()
        .any(|row| row.source_item_id == item.id)
    {
        return false;
    }
    if let Some(run_id) = &item.run_id {
        if projection.runs.iter().any(|run| {
            &run.id == run_id
                && matches!(
                    run.status,
                    model::RunStatus::Preparing
                        | model::RunStatus::Queued
                        | model::RunStatus::Starting
                        | model::RunStatus::Running
                        | model::RunStatus::Waiting
                )
        }) {
            return false;
        }
        if projection.visible_turn_items.iter().any(|row| {
            row.visibility == Visibility::Local
                && row.item.run_id.as_ref() == Some(run_id)
                && row.item.ordinal <= item.ordinal
        }) {
            return false;
        }
    }
    latest.is_some_and(|ordinal| item.ordinal <= ordinal)
        || projection
            .visible_turn_items
            .iter()
            .filter(|row| row.visibility == Visibility::Local)
            .map(|row| row.item.ordinal)
            .min()
            .is_some_and(|oldest| item.ordinal < oldest)
}

/// A delayed page must never replace a newer streamed body or revive an item
/// hidden by a live rewind/queue update. T3's optional deferred-work mode is not used.
pub fn merge_older_history(
    projection: &ThreadProjection,
    older: &[ProjectedTurnItem],
) -> ThreadProjection {
    let current: BTreeMap<_, _> = projection
        .visible_turn_items
        .iter()
        .map(|row| {
            (
                (row.source_thread_id.as_str(), row.source_item_id.as_str()),
                row,
            )
        })
        .collect();
    let mut items: BTreeMap<_, _> = projection
        .turn_items
        .iter()
        .map(|item| (item.id.clone(), item.clone()))
        .collect();
    let mut turn_items = projection.turn_items.clone();
    let mut keys = BTreeSet::new();
    let mut page = Vec::new();
    let visibility = TurnItemVisibility::new(
        &projection.runs,
        &projection.attempts,
        &projection.turn_items,
    );
    for incoming in older {
        let key = (
            incoming.source_thread_id.as_str(),
            incoming.source_item_id.as_str(),
        );
        if keys.contains(&key) {
            continue;
        }
        let mut row = current.get(&key).copied().unwrap_or(incoming).clone();
        if !current.contains_key(&key)
            && (row.visibility == Visibility::Local || row.source_thread_id == projection.thread.id)
            && let Some(item) = items.get(&row.source_item_id)
        {
            if !matches!(
                item.body,
                model::TurnItemBody::RunInterruptRequest { .. }
                    | model::TurnItemBody::ApprovalRequest { .. }
                    | model::TurnItemBody::UserInputRequest { .. }
            ) || !visibility.visible(item)
            {
                continue;
            }
            row.item = item.clone();
        }
        keys.insert(key);
        if (row.visibility == Visibility::Local || row.source_thread_id == projection.thread.id)
            && let std::collections::btree_map::Entry::Vacant(entry) =
                items.entry(row.source_item_id.clone())
        {
            entry.insert(row.item.clone());
            turn_items.push(row.item.clone());
        }
        page.push(row);
    }
    if page.is_empty() {
        return projection.clone();
    }
    page.extend(
        projection
            .visible_turn_items
            .iter()
            .filter(|row| {
                !keys.contains(&(row.source_thread_id.as_str(), row.source_item_id.as_str()))
            })
            .cloned(),
    );
    renumber(&mut page);
    ThreadProjection {
        turn_items,
        visible_turn_items: page,
        ..projection.clone()
    }
}

#[cfg(test)]
mod tests;

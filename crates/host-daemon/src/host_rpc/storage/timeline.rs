//! T3's inherited timeline, snapshot windows and opaque identity cursors.
//! Only the selected page's bodies are deserialized, including on fork chains.
use super::projections::ReadError;
use super::*;
use agent_protocol::orchestration::{self as model, projection::*};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

struct IndexedItem {
    source_thread_id: String,
    id: String,
    kind: String,
    input_intent: Option<String>,
    created_by: Option<String>,
    provider_turn_id: Option<String>,
    position: u64,
    visibility: Visibility,
    synthetic: Option<Arc<model::TurnItem>>,
}

struct Timeline {
    state: model::State,
    items: Vec<IndexedItem>,
}

fn local_items(
    connection: &Connection,
    thread_id: &str,
    inherited: Option<&[String]>,
    cutoff: Option<u64>,
) -> Result<Vec<IndexedItem>, ReadError> {
    let inherited_json = inherited
        .map(serde_json::to_string)
        .transpose()
        .map_err(ReadError::from)?;
    let mut query=connection.prepare("SELECT i.turn_item_id,i.type,
        i.input_intent,i.created_by,i.provider_turn_id
        FROM orchestration_v2_projection_turn_items i
        LEFT JOIN orchestration_v2_projection_runs r ON r.run_id=i.run_id AND r.thread_id=i.thread_id
        WHERE i.thread_id=?1
          AND (?2 IS NULL OR r.run_id IN (SELECT value FROM json_each(?2)) OR i.run_id IS NULL)
          AND (?3 IS NULL OR i.ordinal<=?3)
          AND (r.status IS NULL OR r.status!='rolled_back' OR ?2 IS NOT NULL)
          AND NOT(i.type='user_message' AND i.input_intent='queued_turn'
            AND coalesce((r.status IN ('queued','cancelled') OR (r.status='rolled_back' AND EXISTS(
              SELECT 1 FROM orchestration_v2_projection_run_attempts a WHERE a.run_id=r.run_id AND a.status='cancelled'))),0))
          AND NOT(i.type='run_interrupt_result' AND EXISTS(
            SELECT 1 FROM orchestration_v2_projection_run_attempts a
            WHERE a.run_id=i.run_id AND a.root_node_id=i.node_id AND a.status='superseded') AND NOT EXISTS(
            SELECT 1 FROM orchestration_v2_projection_turn_items request
            WHERE request.thread_id=i.thread_id AND request.run_id=i.run_id AND request.type='run_interrupt_request'))
        ORDER BY i.ordinal,i.turn_item_id").map_err(ReadError::from)?;
    let rows = query
        .query_map(
            params![
                thread_id,
                inherited_json,
                cutoff.map(integer).transpose().map_err(ReadError::from)?
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .map_err(ReadError::from)?;
    rows.map(|row| {
        let (id, kind, input_intent, created_by, provider_turn_id) =
            row.map_err(ReadError::from)?;
        Ok(IndexedItem {
            source_thread_id: thread_id.into(),
            id,
            kind,
            input_intent,
            created_by,
            provider_turn_id,
            position: 0,
            visibility: if inherited.is_some() {
                Visibility::Inherited
            } else {
                Visibility::Local
            },
            synthetic: None,
        })
    })
    .collect()
}

fn read(
    connection: &Connection,
    thread_id: &str,
    ancestors: &mut BTreeSet<String>,
) -> Result<Timeline, ReadError> {
    ancestors.insert(thread_id.into());
    let state = projections::metadata(connection, thread_id)?;
    let mut items = Vec::new();
    if state.thread.lineage.side_chat != Some(true)
        && let Some(model::ThreadForkSource::Run {
            thread_id: source_id,
            run_id,
        }) = &state.thread.forked_from
        && !ancestors.contains(source_id)
        && projections::read::<model::Thread>(connection, source_id)?.is_some()
    {
        let source = read(connection, source_id, ancestors)?;
        if let Some(run) = source.state.runs.get(run_id) {
            let retained = inherited_run_ids(
                source.state.runs.values(),
                run.ordinal,
                &state.thread.created_at,
            )?;
            items.extend(
                source
                    .items
                    .into_iter()
                    .filter(|item| item.source_thread_id != *source_id || item.kind == "fork")
                    .map(|mut item| {
                        item.visibility = Visibility::Inherited;
                        item
                    }),
            );
            let cutoff=connection.query_row("SELECT max(ordinal) FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND run_id=?2",params![source_id,run.id],|row|row.get::<_,Option<i64>>(0)).map_err(ReadError::from)?
                .ok_or("fork source run has no items")?.try_into().map_err(failure)?;
            items.extend(local_items(
                connection,
                source_id,
                Some(&retained),
                Some(cutoff),
            )?);
        }
        items.push(fork_marker(&state.thread, source_id));
    } else if state.thread.lineage.side_chat != Some(true)
        && matches!(
            state.thread.forked_from,
            Some(
                model::ThreadForkSource::ProviderThread { .. }
                    | model::ThreadForkSource::Node { .. }
            )
        )
        && let Some(transfer) = state.context_transfers.values().find(|transfer| {
            transfer.kind == model::TransferType::Fork
                && transfer.target_thread_id == state.thread.id
        })
        && !ancestors.contains(&transfer.source_thread_id)
        && projections::read::<model::Thread>(connection, &transfer.source_thread_id)?.is_some()
    {
        let source_id = &transfer.source_thread_id;
        let source = read(connection, source_id, ancestors)?;
        let item = transfer
            .source_point
            .turn_item_id
            .as_ref()
            .ok_or("native fork has no item boundary")?;
        let cutoff=connection.query_row("SELECT ordinal FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND turn_item_id=?2",params![source_id,item],|row|row.get::<_,i64>(0)).map_err(ReadError::from)?.try_into().map_err(failure)?;
        let retained = inherited_run_ids(
            source.state.runs.values(),
            u64::MAX,
            &state.thread.created_at,
        )?;
        items.extend(
            source
                .items
                .into_iter()
                .filter(|item| item.source_thread_id != *source_id || item.kind == "fork")
                .map(|mut item| {
                    item.visibility = Visibility::Inherited;
                    item
                }),
        );
        items.extend(local_items(
            connection,
            source_id,
            Some(&retained),
            Some(cutoff),
        )?);
        items.push(fork_marker(&state.thread, source_id));
    }
    items.extend(local_items(connection, thread_id, None, None)?);
    for (position, item) in items.iter_mut().enumerate() {
        item.position = position as u64;
    }
    ancestors.remove(thread_id);
    Ok(Timeline { state, items })
}

/// Sidebar counts use this same visibility index as history, without loading
/// any message or tool body. Fork prefixes retain their original cutoff.
pub(super) fn item_counts(
    connection: &Connection,
    thread_id: &str,
) -> Result<(u64, u64), ReadError> {
    let timeline = read(connection, thread_id, &mut BTreeSet::new())?;
    let local = timeline
        .items
        .iter()
        .filter(|item| item.visibility == Visibility::Local)
        .count();
    Ok((local as u64, timeline.items.len() as u64))
}

/// Portable context uses the same visible inherited timeline as the client,
/// with full source bodies rather than the reduced wire projection.
pub(super) fn handoff_items(
    connection: &Connection,
    thread_id: &str,
    after: u64,
    through: u64,
    inherited: bool,
) -> Result<Vec<(model::TurnItem, Option<model::RunStatus>)>, ReadError> {
    let timeline = read(connection, thread_id, &mut BTreeSet::new())?;
    let mut sources = std::collections::BTreeMap::new();
    sources.insert(thread_id.to_owned(), timeline.state);
    let mut items = Vec::new();
    for index in timeline
        .items
        .into_iter()
        .filter(|index| index.synthetic.is_none())
    {
        if index.source_thread_id != thread_id && !inherited {
            continue;
        }
        if !sources.contains_key(&index.source_thread_id) {
            sources.insert(
                index.source_thread_id.clone(),
                projections::metadata(connection, &index.source_thread_id)?,
            );
        }
        let source = &sources[&index.source_thread_id];
        let item = projections::read::<model::TurnItem>(connection, &index.id)?
            .ok_or("handoff source item is missing")?;
        let run = item.run_id.as_ref().and_then(|id| source.runs.get(id));
        if index.source_thread_id == thread_id
            && run.is_some_and(|run| run.ordinal <= after || run.ordinal > through)
        {
            continue;
        }
        if run.is_none() && after > 0 {
            continue;
        }
        items.push((item, run.map(|run| run.status)));
    }
    Ok(items)
}

/// Excerpts use the same inherited timeline as snapshots, but read full bodies
/// only for the requested historical items rather than their wire previews.
pub(super) fn context_history(
    connection: &Connection,
    params: &agent_protocol::handoff::ReadHistory,
) -> Result<agent_protocol::handoff::Page, String> {
    use agent_protocol::{handoff, orchestration::handoffs as history};
    let timeline = read(
        connection,
        &params.session.application_id(),
        &mut BTreeSet::new(),
    )?;
    if timeline.state.thread.deleted_at.is_some() {
        return Err("conversation does not exist".into());
    }
    let indices: Vec<_> = timeline
        .items
        .iter()
        .filter(|index| history::historical_role(&index.kind).is_some())
        .collect();
    let offset = params.item_id.as_ref().map_or(params.offset, |id| {
        indices
            .iter()
            .position(|index| &index.id == id)
            .unwrap_or(indices.len())
    });
    let limit = if params.item_id.is_some() {
        1
    } else {
        params.limit.clamp(1, 20)
    };
    let mut messages = Vec::new();
    for index in indices.iter().skip(offset).take(limit) {
        let item = projections::read::<model::TurnItem>(connection, &index.id)?
            .filter(|item| item.thread_id == index.source_thread_id)
            .ok_or("history item disappeared within snapshot")?;
        let run = item
            .run_id
            .as_ref()
            .map(|id| projections::read::<model::Run>(connection, id))
            .transpose()?
            .flatten();
        let message = history::historical_message(&item, run.map(|run| run.status))
            .ok_or("history item has no historical text")?;
        messages.push(handoff::Message {
            role: if message.message.role == "user" {
                handoff::Role::User
            } else {
                handoff::Role::Assistant
            },
            source: message.source(),
            text: message.text,
        });
    }
    let next = offset.saturating_add(messages.len());
    let mut page = handoff::page(messages, 0, limit, params.text_offset);
    for item in &mut page.items {
        item.position += offset;
    }
    page.next_offset = (params.item_id.is_none() && next < indices.len()).then_some(next);
    Ok(page)
}

fn inherited_run_ids<'a>(
    runs: impl Iterator<Item = &'a model::Run>,
    through: u64,
    fork_at: &str,
) -> Result<Vec<String>, String> {
    let fork_at = chrono::DateTime::parse_from_rfc3339(fork_at).map_err(failure)?;
    runs.filter(|run| run.ordinal <= through)
        .filter_map(|run| {
            if run.status == model::RunStatus::RolledBack
                && let Some(at) = &run.completed_at
            {
                match chrono::DateTime::parse_from_rfc3339(at) {
                    Ok(at) if at < fork_at => return None,
                    Err(error) => return Some(Err(failure(error))),
                    _ => {}
                }
            }
            Some(Ok(run.id.clone()))
        })
        .collect()
}

fn fork_marker(thread: &model::Thread, source_thread_id: &str) -> IndexedItem {
    let marker = Arc::new(model::TurnItem {
        detail_omitted: false,
        id: format!("turn-item:fork:{}", thread.id),
        thread_id: thread.id.clone(),
        run_id: None,
        node_id: None,
        provider_thread_id: None,
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: 0,
        status: model::TurnItemStatus::Completed,
        title: Some("Forked from conversation".into()),
        started_at: None,
        completed_at: Some(thread.created_at.clone()),
        updated_at: thread.created_at.clone(),
        body: model::TurnItemBody::Fork {
            source: thread
                .forked_from
                .clone()
                .expect("fork header has its source"),
            target_thread_id: thread.id.clone(),
        },
    });
    IndexedItem {
        source_thread_id: source_thread_id.into(),
        id: marker.id.clone(),
        kind: "fork".into(),
        input_intent: None,
        created_by: None,
        provider_turn_id: None,
        position: 0,
        visibility: Visibility::Synthetic,
        synthetic: Some(marker),
    }
}

#[derive(Serialize, Deserialize)]
struct HistoryCursor {
    v: u8,
    seq: u64,
    st: String,
    si: String,
    p: u64,
}

fn cursor_end(items: &[IndexedItem], encoded: &str) -> Result<usize, String> {
    if encoded.is_empty() || encoded.len() > 4096 {
        return Err("invalid thread history cursor".into());
    }
    let cursor: HistoryCursor = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(failure)
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(failure))?;
    if cursor.v != 1 || cursor.st.is_empty() || cursor.si.is_empty() {
        return Err("invalid thread history cursor".into());
    }
    Ok(items
        .iter()
        .position(|item| item.source_thread_id == cursor.st && item.id == cursor.si)
        .or_else(|| items.iter().position(|item| item.position >= cursor.p))
        .unwrap_or(items.len()))
}

fn materialize(connection: &Connection, index: &IndexedItem) -> Result<ProjectedTurnItem, String> {
    let item = match &index.synthetic {
        Some(item) => item.clone(),
        None => Arc::new(
            projections::read::<model::TurnItem>(connection, &index.id)?
                .filter(|item| item.thread_id == index.source_thread_id)
                .ok_or("history item disappeared within snapshot")?,
        ),
    };
    Ok(ProjectedTurnItem {
        position: index.position,
        visibility: index.visibility,
        source_thread_id: index.source_thread_id.clone(),
        source_item_id: index.id.clone(),
        item: Arc::new(model::wire::item(&item)),
    })
}

fn is_user_turn(item: &IndexedItem) -> bool {
    is_turn_start(item) && item.created_by.as_deref() == Some("user")
}

fn is_turn_start(item: &IndexedItem) -> bool {
    item.kind == "user_message"
        && matches!(
            item.input_intent.as_deref(),
            Some("turn_start" | "queued_turn")
        )
}

fn select_page(
    connection: &Connection,
    items: &[IndexedItem],
    end: usize,
    sequence: u64,
    user_turn_limit: usize,
    byte_budget: usize,
    snapshot_thread: Option<&str>,
) -> Result<ThreadHistoryPage, String> {
    let candidates = &items[..end.min(items.len())];
    let has_user = candidates.iter().any(is_user_turn);
    let item_limit = if has_user { 500 } else { 75 };
    let mut selected = Vec::new();
    let mut bytes = 0;
    let mut user_turns = 0;
    let mut raw_turns = 0;
    for index in candidates.iter().rev() {
        if selected.len() >= item_limit
            || (has_user && (user_turns >= user_turn_limit || raw_turns >= 150))
        {
            break;
        }
        let row = materialize(connection, index)?;
        let mut cost = serde_json::to_vec(&row).map_err(failure)?.len();
        if snapshot_thread.is_some_and(|thread| {
            row.visibility == Visibility::Local || row.source_thread_id == thread
        }) {
            cost = cost.saturating_add(serde_json::to_vec(&row.item).map_err(failure)?.len());
        }
        if !selected.is_empty() && bytes + cost > byte_budget {
            break;
        }
        bytes = bytes.saturating_add(cost);
        user_turns += usize::from(is_user_turn(index));
        raw_turns += usize::from(is_turn_start(index));
        selected.push(row);
    }
    if user_turns == 0 {
        selected.truncate(75);
    }
    selected.reverse();
    let has_more_history = selected.first().is_some_and(|row| row.position > 0);
    let next_cursor = selected
        .first()
        .filter(|_| has_more_history)
        .map(|row| {
            serde_json::to_vec(&HistoryCursor {
                v: 1,
                seq: sequence,
                st: row.source_thread_id.clone(),
                si: row.source_item_id.clone(),
                p: row.position,
            })
            .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
            .map_err(failure)
        })
        .transpose()?;
    for (position, row) in selected.iter_mut().enumerate() {
        row.position = position as u64;
    }
    Ok(ThreadHistoryPage {
        snapshot_sequence: sequence,
        items: selected,
        next_cursor,
        has_more_history,
    })
}

pub(super) fn history(
    connection: &Connection,
    thread_id: &str,
    cursor: &str,
) -> Result<ThreadHistoryPage, String> {
    let timeline = read(connection, thread_id, &mut BTreeSet::new())?;
    if timeline.state.thread.deleted_at.is_some() {
        return Err("conversation does not exist".into());
    }
    let end = cursor_end(&timeline.items, cursor)?;
    let mut page = select_page(
        connection,
        &timeline.items,
        end,
        event_store::head(connection, Some(thread_id))?,
        20,
        1_048_576,
        None,
    )?;
    let mut requests = Vec::new();
    let mut seen = BTreeSet::new();
    for row in &page.items {
        if row.source_thread_id != thread_id {
            continue;
        }
        if !matches!(
            row.item.body,
            model::TurnItemBody::RunInterruptResult { .. }
        ) {
            continue;
        }
        let Some(run_id) = &row.item.run_id else {
            continue;
        };
        if !seen.insert((row.source_thread_id.as_str(), run_id.as_str())) {
            continue;
        }
        if page.items.iter().any(|candidate| {
            candidate.source_thread_id == row.source_thread_id
                && candidate.item.run_id.as_ref() == Some(run_id)
                && matches!(
                    candidate.item.body,
                    model::TurnItemBody::RunInterruptRequest { .. }
                )
        }) {
            continue;
        }
        let mut query=connection.prepare("SELECT payload_json FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND run_id=?2 AND type='run_interrupt_request' ORDER BY ordinal,turn_item_id").map_err(failure)?;
        let saved = query
            .query_map(params![row.source_thread_id, run_id], |saved| {
                saved.get::<_, String>(0)
            })
            .map_err(failure)?;
        for saved in saved {
            let item: model::TurnItem =
                serde_json::from_str(&saved.map_err(failure)?).map_err(failure)?;
            requests.push(ProjectedTurnItem {
                position: 0,
                visibility: row.visibility,
                source_thread_id: row.source_thread_id.clone(),
                source_item_id: item.id.clone(),
                item: Arc::new(item),
            });
        }
    }
    requests.append(&mut page.items);
    for (position, row) in requests.iter_mut().enumerate() {
        row.position = position as u64;
    }
    page.items = requests;
    Ok(page)
}

pub(super) fn snapshot(connection: &Connection, thread_id: &str) -> Result<ThreadSnapshot, String> {
    let Timeline { state, items } = read(connection, thread_id, &mut BTreeSet::new())?;
    if state.thread.deleted_at.is_some() {
        return Err("conversation does not exist".into());
    }
    let latest_local_turn_ordinal = connection
        .query_row(
            "SELECT max(ordinal) FROM orchestration_v2_turn_item_positions WHERE thread_id=?1",
            [thread_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(failure)?
        .map(|ordinal| ordinal.try_into().map_err(failure))
        .transpose()?;
    let mut projection = ThreadProjection {
        updated_at: state.thread.updated_at.clone(),
        thread: state.thread,
        runs: state.runs.into_values().collect(),
        attempts: state.run_attempts.into_values().collect(),
        nodes: state.nodes.into_values().collect(),
        subagents: state.subagents.into_values().collect(),
        provider_sessions: state
            .provider_sessions
            .into_values()
            .filter(|session| state.attached_provider_sessions.contains(&session.id))
            .collect(),
        provider_threads: state.provider_threads.into_values().collect(),
        provider_turns: state.provider_turns.into_values().collect(),
        runtime_requests: state.runtime_requests.into_values().collect(),
        plans: state.plans.into_values().collect(),
        turn_items: Vec::new(),
        checkpoint_scopes: state.checkpoint_scopes.into_values().collect(),
        checkpoints: state.checkpoints.into_values().collect(),
        context_handoffs: state.context_handoffs.into_values().collect(),
        context_transfers: state.context_transfers.into_values().collect(),
        visible_turn_items: Vec::new(),
    };
    let plans = std::mem::take(&mut projection.plans);
    let latest_todo = plans
        .iter()
        .filter(|plan| {
            plan.kind == model::PlanKind::TodoList && plan.status != model::PlanStatus::Superseded
        })
        .filter_map(|plan| {
            projection
                .runs
                .iter()
                .find(|run| Some(&run.id) == plan.run_id.as_ref())
                .map(|run| (run.ordinal, plan.id.as_str()))
        })
        .max()
        .map(|(_, id)| id);
    projection.plans = plans
        .iter()
        .filter(|plan| {
            plan.status == model::PlanStatus::Active || Some(plan.id.as_str()) == latest_todo
        })
        .cloned()
        .collect();
    let handoffs = std::mem::take(&mut projection.context_handoffs);
    projection.context_handoffs = handoffs
        .iter()
        .filter(|handoff| {
            matches!(
                handoff.status,
                model::HandoffStatus::Pending | model::HandoffStatus::Ready
            )
        })
        .cloned()
        .collect();
    let mut query=connection.prepare("SELECT i.payload_json FROM orchestration_v2_projection_turn_items i WHERE i.thread_id=?1 AND (
        i.type='run_interrupt_request'
        OR (i.type='error' AND i.status='failed' AND json_extract(i.payload_json,'$.failure.class')='delivery_uncertain')
        OR i.turn_item_id IN (SELECT parent_item_id FROM orchestration_v2_projection_turn_items WHERE thread_id=?1 AND type='error' AND status='failed' AND json_extract(payload_json,'$.failure.class')='delivery_uncertain')
        OR (i.type='user_message' AND EXISTS(SELECT 1 FROM orchestration_v2_projection_runs r WHERE r.run_id=i.run_id AND r.status IN ('preparing','queued','starting','running','waiting')))
        OR EXISTS(
          SELECT 1 FROM orchestration_v2_projection_runtime_requests r WHERE r.thread_id=i.thread_id AND r.node_id=i.node_id
            AND r.status='pending'))").map_err(failure)?;
    let dependencies = query
        .query_map([thread_id], |row| row.get::<_, String>(0))
        .map_err(failure)?
        .map(|row| {
            serde_json::from_str::<model::TurnItem>(&row.map_err(failure)?)
                .map(Arc::new)
                .map_err(failure)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let dependencies: Vec<_> = dependencies
        .into_iter()
        .map(|item| Arc::new(model::wire::item(&item)))
        .collect();
    inherited_turns(connection, &items, &mut projection)?;
    model::wire::projection(&mut projection);
    let reserved = serde_json::to_vec(&dependencies).map_err(failure)?.len();
    let control = serde_json::to_vec(&projection).map_err(failure)?.len();
    let budget = (524_288usize.saturating_sub(reserved))
        .max(1_048_576usize.saturating_sub(reserved + control + 1024));
    let page = select_page(
        connection,
        &items,
        items.len(),
        event_store::head(connection, Some(thread_id))?,
        10,
        budget,
        Some(thread_id),
    )?;
    let mut seen = BTreeSet::new();
    projection.turn_items = page
        .items
        .iter()
        .filter(|row| row.visibility == Visibility::Local || row.source_thread_id == thread_id)
        .map(|row| {
            seen.insert(row.source_item_id.clone());
            row.item.clone()
        })
        .collect();
    for item in dependencies {
        if seen.insert(item.id.clone()) {
            projection.turn_items.push(item);
        }
    }
    projection.visible_turn_items = page.items;
    let visible_plans: BTreeSet<_> = projection
        .turn_items
        .iter()
        .filter_map(|item| match &item.body {
            model::TurnItemBody::ProposedPlan { plan_id, .. }
            | model::TurnItemBody::TodoList { plan_id, .. } => Some(plan_id.as_str()),
            _ => None,
        })
        .collect();
    let retained_plans: BTreeSet<_> = projection
        .plans
        .iter()
        .map(|plan| plan.id.clone())
        .collect();
    projection.plans = plans
        .into_iter()
        .filter(|plan| {
            retained_plans.contains(&plan.id) || visible_plans.contains(plan.id.as_str())
        })
        .collect();
    let visible_handoffs: BTreeSet<_> = projection
        .turn_items
        .iter()
        .filter_map(|item| match &item.body {
            model::TurnItemBody::Handoff {
                context_handoff_id, ..
            } => Some(context_handoff_id.as_str()),
            _ => None,
        })
        .collect();
    let retained_handoffs: BTreeSet<_> = projection
        .context_handoffs
        .iter()
        .map(|handoff| handoff.id.clone())
        .collect();
    projection.context_handoffs = handoffs
        .into_iter()
        .filter(|handoff| {
            retained_handoffs.contains(&handoff.id)
                || visible_handoffs.contains(handoff.id.as_str())
        })
        .collect();
    Ok(ThreadSnapshot {
        snapshot_sequence: page.snapshot_sequence,
        projection,
        history_cursor: page.next_cursor,
        has_more_history: page.has_more_history,
        latest_local_turn_ordinal,
    })
}

/// Inherited items keep the source turn's actual timing and outcome. These
/// referenced native resources do not become application runs of the fork.
fn inherited_turns(
    connection: &Connection,
    items: &[IndexedItem],
    projection: &mut ThreadProjection,
) -> Result<(), String> {
    let references = items
        .iter()
        .filter(|row| row.visibility == Visibility::Inherited)
        .filter_map(|row| {
            row.provider_turn_id
                .as_ref()
                .map(|turn| (turn.clone(), row.source_thread_id.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    for (id, owner) in references {
        let json=connection.query_row("SELECT payload_json FROM orchestration_v2_projection_provider_turns WHERE provider_turn_id=?1 AND thread_id=?2",params![id,owner],|row|row.get::<_,String>(0)).optional().map_err(failure)?.ok_or("inherited provider turn is missing or changed owner")?;
        let turn = serde_json::from_str(&json).map_err(failure)?;
        if !projection.provider_turns.iter().any(|turn| turn.id == id) {
            projection.provider_turns.push(turn);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

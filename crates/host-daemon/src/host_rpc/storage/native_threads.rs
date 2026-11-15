//! Metadata for observed native resources shares canonical ownership checks.
use super::*;
use agent_protocol::orchestration as model;

const UNTOUCHED:&str="NOT EXISTS(SELECT 1 FROM orchestration_command_receipts c WHERE c.aggregate_kind='thread' AND c.aggregate_id=t.thread_id)
    AND NOT EXISTS(SELECT 1 FROM orchestration_v2_projection_nodes n WHERE n.thread_id=t.thread_id)
    AND NOT EXISTS(SELECT 1 FROM orchestration_v2_projection_subagents s WHERE s.child_thread_id=t.thread_id AND json_extract(s.payload_json,'$.nativeTaskRef.nativeId') IS NOT NULL)";

/// Discovery can refresh native-owned metadata, but a user's durable command
/// owns the app header after interaction. Historical provider identities stay
/// private even if their active selection has since moved to another source.
pub(super) fn write_header(
    tx: &Transaction<'_>,
    incoming: &model::Thread,
    native: &model::ProviderThread,
    id: &SessionRef,
) -> Result<(), String> {
    if native_bindings::owner(tx, id)?.is_some_and(|owner| owner.application_id() != incoming.id) {
        return Ok(());
    }
    let existing = projections::read::<model::Thread>(tx, &incoming.id)?;
    let mut changes = Vec::new();
    let mut thread = incoming.clone();
    thread.lineage = resolve_lineage(tx, &thread.id, &thread.lineage)?;
    if let Some(previous) = &existing {
        let untouched:bool=tx.query_row(&format!("SELECT {UNTOUCHED} FROM orchestration_v2_projection_threads t WHERE t.thread_id=?1"),[&thread.id],|row|row.get(0)).map_err(failure)?;
        if !untouched || previous.history_origin.is_some() {
            return Ok(());
        }
        thread.history_origin = previous.history_origin;
        thread.created_at = previous.created_at.clone();
        thread.last_visited_at = previous.last_visited_at.clone();
        thread.settled_override = previous.settled_override.clone();
        thread.settled_at = previous.settled_at.clone();
        if native
            .native_metadata
            .as_ref()
            .and_then(|metadata| metadata.updated_at.as_ref())
            .is_none()
        {
            thread.updated_at = previous.updated_at.clone();
        }
        if previous != &thread {
            changes.push(model::Change::Thread(thread.clone()));
        }
    } else {
        changes.push(model::Change::ThreadCreated(thread.clone()));
    }
    let mut native = native.clone();
    if let Some(previous) = projections::read::<model::ProviderThread>(tx, &native.id)? {
        native.created_at = previous.created_at.clone();
        native.updated_at = thread.updated_at.clone();
        native.pending_background_tasks = previous.pending_background_tasks.clone();
        if previous != native {
            changes.push(model::Change::ProviderThread(native));
        }
    } else {
        changes.push(model::Change::ProviderThread(native));
    }
    commands::append_decision(
        tx,
        &model::State::new(thread.clone()),
        model::Decision {
            changes,
            effects: vec![],
        },
        None,
        &thread.updated_at,
    )?;
    Ok(())
}

pub(super) fn resolve_lineage(
    connection: &Connection,
    thread_id: &str,
    lineage: &model::Lineage,
) -> Result<model::Lineage, String> {
    let Some(parent) = lineage.parent_thread_id.as_deref() else {
        return Ok(lineage.clone());
    };
    let native = model::shell::public_session_ref(parent)?;
    let parent = native_bindings::owner(connection, &native)?
        .unwrap_or(native)
        .application_id();
    let mut cursor = Some(parent.clone());
    let mut root = parent.clone();
    let mut visited = std::collections::BTreeSet::from([thread_id.to_owned()]);
    while let Some(id) = cursor {
        if !visited.insert(id.clone()) {
            return Err("catalog ancestry is cyclic".into());
        }
        root = id.clone();
        cursor = projections::read::<model::Thread>(connection, &id)?
            .and_then(|thread| thread.lineage.parent_thread_id);
    }
    Ok(model::Lineage {
        parent_thread_id: Some(parent),
        root_thread_id: root,
        ..lineage.clone()
    })
}

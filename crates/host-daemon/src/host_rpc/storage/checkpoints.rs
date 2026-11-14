use super::*;
use agent_protocol::orchestration as model;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::collections::BTreeMap;

fn reference(scope_id: &str, ordinal: u64) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, scope_id.as_bytes());
    // T3 base64url-encodes the first 32 ASCII hexadecimal characters, rather
    // than the first 16 digest bytes (CheckpointService.ts).
    let hex: String = digest.as_ref()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!(
        "refs/t3/orchestration-v2/checkpoints/{}/ordinal/{ordinal}",
        URL_SAFE_NO_PAD.encode(hex)
    )
}

/// Bex currently has no filesystem checkpoint capture capability. T3's missing
/// checkpoints preserve its baseline and rollback metadata without claiming a
/// file snapshot exists. Real filesystem capture belongs to the resource owner.
pub(in crate::host_rpc) fn missing(
    scope: &model::CheckpointScope,
    run: &model::Run,
    preserved: &BTreeMap<String, model::Checkpoint>,
    now: &str,
) -> Result<Vec<model::Checkpoint>, String> {
    let node = run.root_node_id.as_deref().ok_or("run root is missing")?;
    if scope.thread_id != run.thread_id {
        return Err("checkpoint scope belongs to another thread".into());
    }
    let previous = run
        .ordinal
        .checked_sub(1)
        .ok_or("run ordinal must be positive")?;
    let mut records = Vec::new();
    for ordinal in [0, previous, run.ordinal] {
        if records
            .iter()
            .any(|checkpoint: &model::Checkpoint| checkpoint.ordinal_within_scope == ordinal)
            || preserved.values().any(|checkpoint| {
                checkpoint.scope_id == scope.id && checkpoint.ordinal_within_scope == ordinal
            })
        {
            continue;
        }
        let current = ordinal == run.ordinal;
        let parent_checkpoint_id = if current {
            preserved
                .values()
                .chain(records.iter())
                .find(|checkpoint| {
                    checkpoint.scope_id == scope.id && checkpoint.ordinal_within_scope == previous
                })
                .map(|checkpoint| checkpoint.id.clone())
        } else {
            None
        };
        records.push(model::Checkpoint {
            id: Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("checkpoint:{}:{ordinal}", scope.id).as_bytes(),
            )
            .to_string(),
            thread_id: run.thread_id.clone(),
            scope_id: scope.id.clone(),
            run_id: current.then(|| run.id.clone()),
            node_id: node.into(),
            parent_checkpoint_id,
            ordinal_within_scope: ordinal,
            app_run_ordinal: current.then_some(run.ordinal),
            reference: reference(&scope.id, ordinal),
            status: model::CheckpointStatus::Missing,
            files: Vec::new(),
            files_total: None,
            captured_at: if current {
                now.into()
            } else {
                run.requested_at.clone()
            },
        });
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checkpoint_refs_match_t3_ascii_hex_base64url_encoding() {
        assert_eq!(
            reference("hello", 3),
            "refs/t3/orchestration-v2/checkpoints/MmNmMjRkYmE1ZmIwYTMwZTI2ZTgzYjJhYzViOWUyOWU/ordinal/3"
        );
    }
    #[test]
    fn missing_checkpoints_materialize_only_required_baselines_and_keep_the_actual_run_root() {
        let scope = model::CheckpointScope {
            id: "scope".into(),
            thread_id: "thread".into(),
            run_id: Some("newer-queued-run".into()),
            node_id: "newer-root".into(),
            parent_scope_id: None,
            provider_thread_id: None,
            kind: "root_run".into(),
            ordinal_within_parent: 0,
            advances_app_run_count: true,
            cwd: "/project".into(),
            created_at: "requested".into(),
        };
        let run:model::Run=serde_json::from_value(serde_json::json!({
            "id":"run","threadId":"thread","ordinal":5,"providerInstanceId":"codex",
            "modelSelection":{"instanceId":"codex","model":"example"},"providerThreadId":null,
            "userMessageId":"message","rootNodeId":"actual-root","activeAttemptId":null,"status":"waiting",
            "queuePosition":null,"queueHeld":false,"requestedAt":"requested","startedAt":null,"completedAt":null,
            "checkpointId":null,"contextHandoffId":null
        })).unwrap();
        let records = missing(&scope, &run, &BTreeMap::new(), "captured").unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.ordinal_within_scope)
                .collect::<Vec<_>>(),
            [0, 4, 5]
        );
        assert!(records.iter().all(|record| record.node_id == "actual-root"
            && record.files.is_empty()
            && record.status == model::CheckpointStatus::Missing));
        assert!(records[..2].iter().all(|record| record.run_id.is_none()
            && record.app_run_ordinal.is_none()
            && record.captured_at == "requested"));
        assert_eq!(
            records[2].parent_checkpoint_id.as_ref(),
            Some(&records[1].id)
        );
        assert_eq!(records[2].run_id.as_deref(), Some("run"));
        assert_eq!(records[2].app_run_ordinal, Some(5));
        let preserved = records[..2]
            .iter()
            .map(|record| (record.id.clone(), record.clone()))
            .collect();
        assert_eq!(
            missing(&scope, &run, &preserved, "captured").unwrap(),
            [records[2].clone()]
        );
    }
}

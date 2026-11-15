//! Native discovery and lazy history import share one header projection.
use super::*;
use agent_protocol::orchestration as model;

pub(super) fn timestamp(milliseconds: Option<u64>, fallback: &str) -> String {
    milliseconds
        .and_then(|at| i64::try_from(at).ok())
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| fallback.into())
}

pub(in crate::host_rpc) fn observed_status(
    status: agent_protocol::execution::SessionStatus,
) -> model::ProviderThreadStatus {
    use agent_protocol::execution::SessionStatus;
    match status {
        SessionStatus::Running | SessionStatus::Preparing => model::ProviderThreadStatus::Active,
        SessionStatus::Unknown | SessionStatus::Unavailable => model::ProviderThreadStatus::Unknown,
        SessionStatus::Idle => model::ProviderThreadStatus::Idle,
    }
}

/// Discovery is read-only and must not depend on a working provider process or
/// model-list RPC. Known source metadata wins. Absent native metadata follows
/// the pinned importer's provider default; opening resolves the live selection
/// before admitting any input. DSH uses its own route, never a Codex model ID.
pub(super) fn selection(
    provider: ProviderKind,
    model: Option<&agent_protocol::models::ModelRef>,
) -> model::ModelSelection {
    model::ModelSelection {
        instance_id: provider.key().into(),
        model: model
            .filter(|model| model.provider == provider && !model.id.trim().is_empty())
            .map(|model| model.id.clone())
            .unwrap_or_else(|| {
                match provider {
                    ProviderKind::Codex => "gpt-6-astra",
                    ProviderKind::Claude => "claude-opus-5-5",
                    ProviderKind::DeepSeek => "[\"deepseek-official\",\"deepseek-flash\"]",
                }
                .into()
            }),
        options: vec![],
    }
}

pub(in crate::host_rpc) fn records(
    summary: &agent_protocol::models::Thread,
    selected: model::ModelSelection,
    project_id: Option<String>,
    now: &str,
) -> Result<(model::Thread, model::ProviderThread), Failure> {
    let native = summary
        .id
        .as_ref()
        .ok_or_else(|| Failure::new("invalid_thread", "native header has no identity"))?;
    let provider = native
        .native_provider()
        .map_err(|error| Failure::new("invalid_thread", error))?;
    let id = native.application_id();
    let provider_thread_id = model::derived_id(
        "provider-thread",
        &[
            "provider",
            provider.driver(),
            "provider-instance",
            provider.key(),
            "native-thread",
            &native.id,
        ],
    );
    let updated = summary
        .updated_at
        .filter(|at| at.is_finite() && *at >= 0.0)
        .map(|at| timestamp(Some((at * 1000.0) as u64), &now))
        .unwrap_or_else(|| now.to_owned());
    let created = summary.turns.iter().flatten().find_map(|turn| {
        turn.started_at_ms
            .or_else(|| turn.started_at.map(|at| (at * 1000.0) as u64))
    });
    let created = timestamp(created, &updated);
    let thread = model::Thread {
        id: id.clone(),
        history_origin: None,
        workspace_launch: None,
        created_by: model::Actor::System,
        creation_source: if summary.parent_id.is_some() {
            model::CreationSource::Provider
        } else {
            model::CreationSource::Server
        },
        project_id: project_id,
        title: summary
            .name
            .clone()
            .or(summary.preview.clone())
            .unwrap_or_default(),
        provider_instance_id: provider.key().into(),
        model_selection: selected,
        runtime_mode: "full-access".into(),
        interaction_mode: "default".into(),
        branch: summary.git_branch.clone(),
        worktree_path: summary.cwd.clone(),
        active_provider_thread_id: Some(provider_thread_id.clone()),
        lineage: model::Lineage {
            parent_thread_id: summary.parent_id.as_ref().map(SessionRef::application_id),
            relationship_to_parent: summary.parent_id.as_ref().map(|_| "subagent".into()),
            root_thread_id: summary
                .parent_id
                .as_ref()
                .map(SessionRef::application_id)
                .unwrap_or_else(|| id.clone()),
            side_chat: None,
        },
        forked_from: None,
        created_at: created.clone(),
        updated_at: updated.clone(),
        archived_at: None,
        settled_override: Some("settled".into()),
        settled_at: Some(updated.clone()),
        last_visited_at: None,
        deleted_at: None,
    };
    let provider_thread = model::ProviderThread {
        id: provider_thread_id,
        driver: provider.driver().into(),
        provider_instance_id: provider.key().into(),
        provider_session_id: None,
        app_thread_id: Some(id.clone()),
        owner_node_id: None,
        native_thread_ref: Some(model::ProviderRef {
            driver: provider.driver().into(),
            native_id: Some(native.id.clone()),
            strength: if provider == ProviderKind::DeepSeek {
                model::NativeRefStrength::Weak
            } else {
                model::NativeRefStrength::Strong
            },
            fingerprint: None,
            ordinal: None,
        }),
        native_conversation_head_ref: None,
        native_metadata: Some(model::ProviderThreadNativeMetadata {
            updated_at: summary
                .updated_at
                .filter(|at| at.is_finite() && *at >= 0.0)
                .map(|at| timestamp(Some((at * 1000.0) as u64), now)),
            ..Default::default()
        }),
        status: observed_status(summary.status),
        first_run_ordinal: None,
        last_run_ordinal: None,
        handoff_ids: vec![],
        forked_from: None,
        pending_background_tasks: vec![],
        created_at: created,
        updated_at: updated,
    };
    Ok((thread, provider_thread))
}

#[cfg(test)]
mod tests;

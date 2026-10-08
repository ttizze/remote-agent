//! Pure environment identity, capability, and cross-environment view helpers.
use agent_protocol::models::{
    AgentActivityPhase, AwarenessActivity, AwarenessSnapshot, EnvironmentCapabilities,
    EnvironmentDescriptor,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedProjectRef {
    pub environment_id: String,
    pub project_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedThreadRef {
    pub environment_id: String,
    pub thread_id: String,
}

/// Builds the key used when project or thread identities are shared across Hosts.
pub fn scoped_key(environment_id: &str, local_id: &str) -> Option<String> {
    (!environment_id.is_empty() && !local_id.is_empty())
        .then(|| format!("{environment_id}:{local_id}"))
}

pub fn scoped_project_key(reference: &ScopedProjectRef) -> Option<String> {
    scoped_key(&reference.environment_id, &reference.project_id)
}

pub fn scoped_thread_key(reference: &ScopedThreadRef) -> Option<String> {
    scoped_key(&reference.environment_id, &reference.thread_id)
}

/// Splits on the first separator so a local identifier may contain `:`.
pub fn parse_scoped_key(key: &str) -> Option<(&str, &str)> {
    let (environment_id, local_id) = key.split_once(':')?;
    (!environment_id.is_empty() && !local_id.is_empty()).then_some((environment_id, local_id))
}

pub fn parse_scoped_project_key(key: &str) -> Option<ScopedProjectRef> {
    let (environment_id, project_id) = parse_scoped_key(key)?;
    Some(ScopedProjectRef {
        environment_id: environment_id.to_owned(),
        project_id: project_id.to_owned(),
    })
}

pub fn parse_scoped_thread_key(key: &str) -> Option<ScopedThreadRef> {
    let (environment_id, thread_id) = parse_scoped_key(key)?;
    Some(ScopedThreadRef {
        environment_id: environment_id.to_owned(),
        thread_id: thread_id.to_owned(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentCapability {
    RepositoryIdentity,
    ConnectionProbe,
    AttachmentUploads,
    QuestionAttachments,
    FileAttachments,
    ThreadSettlement,
    ThreadAutoSettlement,
    ThreadSnooze,
    StorageCleanup,
    ProjectWorktreeCleanup,
    ThreadRestartContinuation,
    ProjectSettingsOverrides,
    EnvironmentThemes,
    UsageLimitSources,
    UsagePriceOverrides,
    UsageModelAliases,
    ThreadPinning,
    ThreadPinReorder,
    ThreadActiveReorder,
    ThreadAutoSettleOptOut,
    ThreadTitleRegeneration,
    ThreadVisitedTracking,
    ThreadPullRequestLinking,
    ServerResolvedCommandContext,
    ThreadPullRequests,
    ThreadPullRequestWatch,
    PullRequestStackActions,
    ServerSelfUpdate,
    ServerSelfUpdateProgress,
    ServerUpdateThreadContinuation,
    ProjectCloneTracking,
    EnvironmentIcon,
    DesktopAppUpdate,
    AgentActivityPublishing,
}

pub fn supports_capability(
    capabilities: &EnvironmentCapabilities,
    capability: EnvironmentCapability,
) -> bool {
    match capability {
        EnvironmentCapability::RepositoryIdentity => capabilities.repository_identity,
        EnvironmentCapability::ConnectionProbe => capabilities.connection_probe,
        EnvironmentCapability::AttachmentUploads => capabilities.attachment_uploads,
        EnvironmentCapability::QuestionAttachments => capabilities.question_attachments,
        EnvironmentCapability::FileAttachments => capabilities.file_attachments.is_some(),
        EnvironmentCapability::ThreadSettlement => capabilities.thread_settlement,
        EnvironmentCapability::ThreadAutoSettlement => capabilities.thread_auto_settlement,
        EnvironmentCapability::ThreadSnooze => capabilities.thread_snooze,
        EnvironmentCapability::StorageCleanup => capabilities.storage_cleanup,
        EnvironmentCapability::ProjectWorktreeCleanup => capabilities.project_worktree_cleanup,
        EnvironmentCapability::ThreadRestartContinuation => {
            capabilities.thread_restart_continuation
        }
        EnvironmentCapability::ProjectSettingsOverrides => capabilities.project_settings_overrides,
        EnvironmentCapability::EnvironmentThemes => capabilities.environment_themes,
        EnvironmentCapability::UsageLimitSources => capabilities.usage_limit_sources,
        EnvironmentCapability::UsagePriceOverrides => capabilities.usage_price_overrides,
        EnvironmentCapability::UsageModelAliases => capabilities.usage_model_aliases,
        EnvironmentCapability::ThreadPinning => capabilities.thread_pinning,
        EnvironmentCapability::ThreadPinReorder => capabilities.thread_pin_reorder,
        EnvironmentCapability::ThreadActiveReorder => capabilities.thread_active_reorder,
        EnvironmentCapability::ThreadAutoSettleOptOut => capabilities.thread_auto_settle_opt_out,
        EnvironmentCapability::ThreadTitleRegeneration => capabilities.thread_title_regeneration,
        EnvironmentCapability::ThreadVisitedTracking => capabilities.thread_visited_tracking,
        EnvironmentCapability::ThreadPullRequestLinking => capabilities.thread_pull_request_linking,
        EnvironmentCapability::ServerResolvedCommandContext => {
            capabilities.server_resolved_command_context
        }
        EnvironmentCapability::ThreadPullRequests => capabilities.thread_pull_requests,
        EnvironmentCapability::ThreadPullRequestWatch => capabilities.thread_pull_request_watch,
        EnvironmentCapability::PullRequestStackActions => capabilities.pull_request_stack_actions,
        EnvironmentCapability::ServerSelfUpdate => capabilities.server_self_update.is_some(),
        EnvironmentCapability::ServerSelfUpdateProgress => capabilities.server_self_update_progress,
        EnvironmentCapability::ServerUpdateThreadContinuation => {
            capabilities.server_update_thread_continuation
        }
        EnvironmentCapability::ProjectCloneTracking => capabilities.project_clone_tracking,
        EnvironmentCapability::EnvironmentIcon => capabilities.environment_icon,
        EnvironmentCapability::DesktopAppUpdate => capabilities.desktop_app_update,
        EnvironmentCapability::AgentActivityPublishing => capabilities.agent_activity_publishing,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentConnectionState {
    Connected,
    Connecting,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSummary {
    pub descriptor: EnvironmentDescriptor,
    pub connection: EnvironmentConnectionState,
    pub reconnect_reason: Option<String>,
    pub active_activity_count: usize,
}

pub fn summarize_snapshot(snapshot: &crate::state::Snapshot) -> Option<EnvironmentSummary> {
    let descriptor = snapshot.environment.clone()?;
    let connection = if snapshot.connected {
        EnvironmentConnectionState::Connected
    } else if snapshot.error.is_some() {
        EnvironmentConnectionState::Disconnected
    } else {
        EnvironmentConnectionState::Connecting
    };
    Some(EnvironmentSummary {
        descriptor,
        connection,
        reconnect_reason: snapshot.error.clone(),
        active_activity_count: snapshot
            .awareness
            .as_ref()
            .map_or(0, |awareness| awareness.activities.len()),
    })
}

/// Stable ordering keeps the active environment and activity lists consistent
/// when several Hosts have the same display label.
pub fn sort_environment_summaries(summaries: &mut [EnvironmentSummary]) {
    summaries.sort_by(|left, right| {
        left.descriptor
            .label
            .to_lowercase()
            .cmp(&right.descriptor.label.to_lowercase())
            .then_with(|| {
                left.descriptor
                    .environment_id
                    .cmp(&right.descriptor.environment_id)
            })
    });
}

pub fn filter_environment_summaries(
    summaries: &[EnvironmentSummary],
    query: &str,
) -> Vec<EnvironmentSummary> {
    let query = query.trim().to_lowercase();
    summaries
        .iter()
        .filter(|summary| {
            query.is_empty()
                || summary.descriptor.label.to_lowercase().contains(&query)
                || summary
                    .descriptor
                    .environment_id
                    .to_lowercase()
                    .contains(&query)
                || summary
                    .descriptor
                    .platform
                    .os
                    .to_lowercase()
                    .contains(&query)
                || summary
                    .descriptor
                    .platform
                    .arch
                    .to_lowercase()
                    .contains(&query)
                || summary
                    .reconnect_reason
                    .as_deref()
                    .is_some_and(|reason| reason.to_lowercase().contains(&query))
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AggregatedActivity {
    pub environment: EnvironmentDescriptor,
    pub activity: AwarenessActivity,
}

pub fn aggregate_activities(
    snapshots: &[AwarenessSnapshot],
    query: &str,
) -> Vec<AggregatedActivity> {
    let query = query.trim().to_lowercase();
    let mut activities: Vec<_> = snapshots
        .iter()
        .flat_map(|snapshot| {
            snapshot.activities.iter().filter_map(|activity| {
                let searchable = [
                    snapshot.environment.label.as_str(),
                    snapshot.environment.environment_id.as_str(),
                    activity.project_title.as_str(),
                    activity.thread_title.as_str(),
                    activity.headline.as_str(),
                    activity.detail.as_deref().unwrap_or_default(),
                ];
                (query.is_empty()
                    || searchable
                        .iter()
                        .any(|value| value.to_lowercase().contains(&query)))
                .then(|| AggregatedActivity {
                    environment: snapshot.environment.clone(),
                    activity: activity.clone(),
                })
            })
        })
        .collect();
    activities.sort_by(|left, right| {
        left.environment
            .label
            .to_lowercase()
            .cmp(&right.environment.label.to_lowercase())
            .then_with(|| {
                left.environment
                    .environment_id
                    .cmp(&right.environment.environment_id)
            })
            .then_with(|| {
                right
                    .activity
                    .updated_at_ms
                    .cmp(&left.activity.updated_at_ms)
            })
            .then_with(|| left.activity.thread_id.cmp(&right.activity.thread_id))
    });
    activities
}

pub fn activity_is_live(phase: &AgentActivityPhase) -> bool {
    matches!(
        phase,
        AgentActivityPhase::Starting
            | AgentActivityPhase::Running
            | AgentActivityPhase::WaitingApproval
            | AgentActivityPhase::WaitingInput
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_protocol::models::{
        EnvironmentCapabilities, EnvironmentFileAttachments, EnvironmentInstallation,
        EnvironmentPlatform,
    };
    use proptest::prelude::*;

    fn descriptor(id: &str, label: &str) -> EnvironmentDescriptor {
        EnvironmentDescriptor {
            environment_id: id.into(),
            label: label.into(),
            platform: EnvironmentPlatform {
                os: "linux".into(),
                arch: "x64".into(),
                machine: None,
            },
            server_version: "test".into(),
            orchestration_protocol_version: Some(2),
            capabilities: EnvironmentCapabilities::default(),
        }
    }

    proptest! {
        #[test]
        fn scoped_keys_round_trip(environment_id in "[a-z][a-z0-9-]{0,8}", local_id in "[a-z][a-z0-9:-]{0,12}") {
            let key = scoped_key(&environment_id, &local_id).unwrap();
            prop_assert_eq!(parse_scoped_key(&key), Some((environment_id.as_str(), local_id.as_str())));
        }
    }

    #[test]
    fn scoped_keys_reject_missing_parts_and_preserve_colons() {
        assert_eq!(parse_scoped_key(":thread"), None);
        assert_eq!(parse_scoped_key("environment:"), None);
        assert_eq!(
            parse_scoped_key("environment:thread:run"),
            Some(("environment", "thread:run"))
        );
    }

    #[test]
    fn environment_filter_and_sort_are_deterministic() {
        let mut summaries = vec![
            EnvironmentSummary {
                descriptor: descriptor("z", "Local"),
                connection: EnvironmentConnectionState::Connected,
                reconnect_reason: None,
                active_activity_count: 0,
            },
            EnvironmentSummary {
                descriptor: descriptor("a", "local"),
                connection: EnvironmentConnectionState::Disconnected,
                reconnect_reason: Some("network timeout".into()),
                active_activity_count: 1,
            },
        ];
        sort_environment_summaries(&mut summaries);
        assert_eq!(summaries[0].descriptor.environment_id, "a");
        assert_eq!(filter_environment_summaries(&summaries, "timeout").len(), 1);
    }

    #[test]
    fn capability_checks_and_wire_names_are_explicit() {
        let mut capabilities = EnvironmentCapabilities::default();
        capabilities.agent_activity_publishing = true;
        assert!(supports_capability(
            &capabilities,
            EnvironmentCapability::AgentActivityPublishing
        ));
        let value = serde_json::to_value(descriptor("one", "One")).unwrap();
        assert_eq!(value["environmentId"], "one");
        assert!(value["serverVersion"].is_string());
        assert!(value["capabilities"]["fileAttachments"].is_null());
        capabilities.file_attachments = Some(EnvironmentFileAttachments {
            max_upload_bytes: 42,
        });
        assert!(supports_capability(
            &capabilities,
            EnvironmentCapability::FileAttachments
        ));
        let mut descriptor = descriptor("one", "One");
        descriptor.capabilities = capabilities;
        let value = serde_json::to_value(descriptor).unwrap();
        assert_eq!(
            value["capabilities"]["fileAttachments"]["maxUploadBytes"],
            42
        );
        capabilities.server_installation = Some(EnvironmentInstallation::NpmGlobal {
            prefix: "/usr/local".into(),
        });
        let mut descriptor = descriptor("one", "One");
        descriptor.capabilities = capabilities;
        let value = serde_json::to_value(descriptor).unwrap();
        assert_eq!(
            value["capabilities"]["serverInstallation"]["kind"],
            "npm-global"
        );
        assert_eq!(
            value["capabilities"]["serverInstallation"]["prefix"],
            "/usr/local"
        );
    }

    #[test]
    fn snapshot_summary_preserves_connection_reason_and_activity_count() {
        let mut snapshot = crate::state::Snapshot::default();
        snapshot.environment = Some(descriptor("one", "One"));
        snapshot.error = Some("network timeout".into());
        snapshot.awareness = Some(AwarenessSnapshot {
            environment: snapshot.environment.clone().unwrap(),
            activities: vec![],
            updated_at_ms: 0,
        });
        let summary = summarize_snapshot(&snapshot).unwrap();
        assert_eq!(summary.connection, EnvironmentConnectionState::Disconnected);
        assert_eq!(summary.reconnect_reason.as_deref(), Some("network timeout"));
    }

    #[test]
    fn activity_aggregation_filters_and_orders_by_environment_then_recency() {
        let environment = descriptor("one", "One");
        let snapshot = AwarenessSnapshot {
            environment,
            activities: vec![AwarenessActivity {
                environment_id: "one".into(),
                thread_id: "thread".into(),
                project_title: "Project".into(),
                thread_title: "Build".into(),
                phase: AgentActivityPhase::Running,
                headline: "Working".into(),
                detail: None,
                model_title: None,
                updated_at_ms: 4,
            }],
            updated_at_ms: 4,
        };
        let activities = aggregate_activities(&[snapshot], "build");
        assert_eq!(activities.len(), 1);
        assert!(activity_is_live(&activities[0].activity.phase));
    }

    #[test]
    fn activity_aggregation_breaks_same_label_ties_by_environment_id() {
        let mut first = descriptor("z", "Shared");
        first.capabilities.agent_activity_publishing = true;
        let mut second = descriptor("a", "Shared");
        second.capabilities.agent_activity_publishing = true;
        let activity = |environment_id: &str| AwarenessActivity {
            environment_id: environment_id.into(),
            thread_id: "same-thread".into(),
            project_title: "Project".into(),
            thread_title: "Build".into(),
            phase: AgentActivityPhase::Running,
            headline: "Working".into(),
            detail: None,
            model_title: None,
            updated_at_ms: 4,
        };
        let activities = aggregate_activities(
            &[
                AwarenessSnapshot {
                    environment: first,
                    activities: vec![activity("z")],
                    updated_at_ms: 4,
                },
                AwarenessSnapshot {
                    environment: second,
                    activities: vec![activity("a")],
                    updated_at_ms: 4,
                },
            ],
            "",
        );
        assert_eq!(
            activities
                .iter()
                .map(|activity| activity.environment.environment_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
    }
}

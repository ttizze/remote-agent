//! Pure environment identity, capability, and cross-environment view helpers.
use crate::{
    state::{ModelOption, Snapshot},
    view::load_balancing::{self, Candidate as LoadBalancingCandidate},
    view::{
        composer::controls::{compatible_runtime_mode, runtime_mode_choices},
        models::options::normalize_model_options,
        settings::{SettingsScope, SettingsView},
        sidebar::{SidebarDraftRow, SidebarItem, SidebarOptions, SidebarSection, SidebarThreadRow},
        thread_list::{
            PendingTaskKind, PendingTaskRow, ThreadListItem, ThreadListOptions, ThreadRow,
        },
        thread_menu::{ThreadMenuAction, ThreadMenuChild, ThreadMenuItem},
    },
};
use agent_domain::{Driver, InteractionMode, RuntimeMode, ThreadId};
use agent_protocol::models::{
    AgentActivityPhase, AwarenessActivity, AwarenessSnapshot, EnvironmentCapabilities,
    EnvironmentDescriptor, ProviderInstance,
};
use agent_protocol::{operations::Account, provider::ProviderKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
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
    PullRequests,
    PullRequestChecks,
    InlineMessageContext,
    RequiredWorktreeBootstrap,
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
        EnvironmentCapability::PullRequests => capabilities.pull_requests,
        EnvironmentCapability::PullRequestChecks => capabilities.pull_request_checks,
        EnvironmentCapability::InlineMessageContext => capabilities.inline_message_context,
        EnvironmentCapability::RequiredWorktreeBootstrap => {
            capabilities.required_worktree_bootstrap
        }
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

pub fn capability_names(capabilities: &EnvironmentCapabilities) -> Vec<&'static str> {
    [
        (
            EnvironmentCapability::RepositoryIdentity,
            "repositoryIdentity",
        ),
        (EnvironmentCapability::ConnectionProbe, "connectionProbe"),
        (
            EnvironmentCapability::AttachmentUploads,
            "attachmentUploads",
        ),
        (
            EnvironmentCapability::QuestionAttachments,
            "questionAttachments",
        ),
        (EnvironmentCapability::FileAttachments, "fileAttachments"),
        (EnvironmentCapability::PullRequests, "pullRequests"),
        (
            EnvironmentCapability::PullRequestChecks,
            "pullRequestChecks",
        ),
        (
            EnvironmentCapability::InlineMessageContext,
            "inlineMessageContext",
        ),
        (
            EnvironmentCapability::RequiredWorktreeBootstrap,
            "requiredWorktreeBootstrap",
        ),
        (EnvironmentCapability::ThreadSettlement, "threadSettlement"),
        (
            EnvironmentCapability::ThreadAutoSettlement,
            "threadAutoSettlement",
        ),
        (EnvironmentCapability::ThreadSnooze, "threadSnooze"),
        (EnvironmentCapability::StorageCleanup, "storageCleanup"),
        (
            EnvironmentCapability::ProjectWorktreeCleanup,
            "projectWorktreeCleanup",
        ),
        (
            EnvironmentCapability::ThreadRestartContinuation,
            "threadRestartContinuation",
        ),
        (
            EnvironmentCapability::ProjectSettingsOverrides,
            "projectSettingsOverrides",
        ),
        (
            EnvironmentCapability::EnvironmentThemes,
            "environmentThemes",
        ),
        (
            EnvironmentCapability::UsageLimitSources,
            "usageLimitSources",
        ),
        (
            EnvironmentCapability::UsagePriceOverrides,
            "usagePriceOverrides",
        ),
        (
            EnvironmentCapability::UsageModelAliases,
            "usageModelAliases",
        ),
        (EnvironmentCapability::ThreadPinning, "threadPinning"),
        (EnvironmentCapability::ThreadPinReorder, "threadPinReorder"),
        (
            EnvironmentCapability::ThreadActiveReorder,
            "threadActiveReorder",
        ),
        (
            EnvironmentCapability::ThreadAutoSettleOptOut,
            "threadAutoSettleOptOut",
        ),
        (
            EnvironmentCapability::ThreadTitleRegeneration,
            "threadTitleRegeneration",
        ),
        (
            EnvironmentCapability::ThreadVisitedTracking,
            "threadVisitedTracking",
        ),
        (
            EnvironmentCapability::ThreadPullRequestLinking,
            "threadPullRequestLinking",
        ),
        (
            EnvironmentCapability::ServerResolvedCommandContext,
            "serverResolvedCommandContext",
        ),
        (
            EnvironmentCapability::ThreadPullRequests,
            "threadPullRequests",
        ),
        (
            EnvironmentCapability::ThreadPullRequestWatch,
            "threadPullRequestWatch",
        ),
        (
            EnvironmentCapability::PullRequestStackActions,
            "pullRequestStackActions",
        ),
        (EnvironmentCapability::ServerSelfUpdate, "serverSelfUpdate"),
        (
            EnvironmentCapability::ServerSelfUpdateProgress,
            "serverSelfUpdateProgress",
        ),
        (
            EnvironmentCapability::ServerUpdateThreadContinuation,
            "serverUpdateThreadContinuation",
        ),
        (
            EnvironmentCapability::ProjectCloneTracking,
            "projectCloneTracking",
        ),
        (EnvironmentCapability::EnvironmentIcon, "environmentIcon"),
        (EnvironmentCapability::DesktopAppUpdate, "desktopAppUpdate"),
        (
            EnvironmentCapability::AgentActivityPublishing,
            "agentActivityPublishing",
        ),
    ]
    .into_iter()
    .filter_map(|(capability, name)| supports_capability(capabilities, capability).then_some(name))
    .collect()
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

/// A thread row projected into the client-wide environment namespace.
///
/// The row remains the normal Host-owned sidebar row so native clients can
/// reuse their existing renderer. Its id and project id are scoped before the
/// row leaves this registry, which makes selection and routing unambiguous when
/// two environments expose the same local ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentThreadRow {
    pub environment_id: String,
    pub environment_label: String,
    pub row: SidebarThreadRow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSidebarSection {
    pub summary: EnvironmentSummary,
    pub drafts: Vec<SidebarDraftRow>,
    pub rows: Vec<EnvironmentThreadRow>,
}

/// The combined sidebar projection. Shelves stay grouped by environment so a
/// client can retain the existing row actions while making the owning Host
/// visible at the point of selection.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentSidebarView {
    pub sections: Vec<EnvironmentSidebarSection>,
    pub activities: Vec<AggregatedActivity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentInboxItem {
    pub environment_id: String,
    pub environment_label: String,
    pub row: SidebarThreadRow,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentInboxView {
    pub items: Vec<EnvironmentInboxItem>,
}

/// Settings projected for every registered environment. The settings rows are
/// still produced by each environment's Snapshot; this wrapper only supplies
/// the environment identity required to route an edit back to its Store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentSettingsEntry {
    pub summary: EnvironmentSummary,
    pub settings: SettingsView,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EnvironmentSettingsView {
    pub entries: Vec<EnvironmentSettingsEntry>,
}

/// The Host-owned account/provider inputs used by a client-wide usage widget.
/// Account ids are scoped before aggregation so two Hosts can expose the same
/// local id without one overwriting the other. The usage renderer remains the
/// owner of display policy and never receives private account credentials.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentUsageInput {
    pub environment: EnvironmentDescriptor,
    pub accounts: Vec<Account>,
    /// Provider drivers configured on this Host and usable for subscription
    /// usage. Eligibility is evaluated per instance before environments are
    /// combined, so enabled and installed flags from different Hosts cannot
    /// be paired accidentally.
    pub configured_provider_kinds: Vec<ProviderKind>,
}

/// The combined account/provider snapshot consumed by a client-wide usage
/// widget. Account ids have already been scoped by `EnvironmentRegistry`, so
/// duplicate local ids from different Hosts remain distinct while provider
/// availability is unioned once for the whole workspace.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentUsageSnapshot {
    pub accounts: Vec<Account>,
    pub configured_provider_kinds: Vec<ProviderKind>,
}

/// The target Host/project/provider selected for a new-thread draft. The
/// caller promotes the owning Store before creating the draft, so all
/// subsequent mutations stay with that Host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentLoadBalancedRoute {
    pub environment_id: String,
    pub project_id: String,
    pub provider_instance: String,
    pub driver: Driver,
    pub model: String,
    pub options: Vec<ModelOption>,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvironmentLoadBalancingEvaluation {
    pub candidate_count: usize,
    /// At least one matching connected Host still needs a resource reply.
    pub pending_resources: bool,
    pub route: Option<EnvironmentLoadBalancedRoute>,
}

/// A project row in the client-wide project picker. The project id is scoped
/// to the environment that owns it, so two Hosts may expose the same local id.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EnvironmentProjectRow {
    pub environment_id: String,
    pub environment_label: String,
    pub project_id: String,
    pub title: String,
    pub subtitle: String,
}

/// A thread row together with the Host that must receive its next action.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EnvironmentThreadListRow {
    pub environment_id: String,
    pub environment_label: String,
    pub row: ThreadRow,
}

/// A queued or draft task together with its owning Host.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EnvironmentPendingTaskRow {
    pub environment_id: String,
    pub environment_label: String,
    pub task: PendingTaskRow,
}

/// Mobile clients render these rows with their existing Host-local components.
/// Shelves are intentionally omitted here: the rows already carry their
/// lifecycle variant and the combined list must not duplicate a shelf header
/// for every environment.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EnvironmentThreadListView {
    pub rows: Vec<EnvironmentThreadListRow>,
    pub pending_tasks: Vec<EnvironmentPendingTaskRow>,
    pub has_threads: bool,
}

/// Settings for one Host, retaining the environment id used to route edits.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct EnvironmentSettingsEntryView {
    pub environment_id: String,
    pub environment_label: String,
    pub settings: SettingsView,
}

#[derive(Debug, Clone)]
struct EnvironmentEntry {
    snapshot: Arc<Snapshot>,
    summary: EnvironmentSummary,
}

/// Client-owned projections for all authenticated Host stores.
///
/// A Store remains the resource owner for one Host's transport, cache and
/// mutations. This registry owns only the immutable snapshots and the combined
/// projections, so disconnecting one entry never clears another entry's cache
/// or reconnect state.
#[derive(Debug, Clone, Default)]
pub struct EnvironmentRegistry {
    entries: BTreeMap<String, EnvironmentEntry>,
    selected: Option<String>,
}

impl EnvironmentRegistry {
    /// Publishes a newer snapshot and returns its stable environment id.
    pub fn update(&mut self, snapshot: Arc<Snapshot>) -> Option<String> {
        let descriptor = snapshot.environment.as_ref()?;
        let id = descriptor.environment_id.clone();
        let summary = summarize_snapshot(&snapshot)?;
        if let Some(previous) = self.entries.get(&id)
            && !snapshot.accepts_after(&previous.snapshot)
        {
            return Some(id);
        }
        self.entries
            .insert(id.clone(), EnvironmentEntry { snapshot, summary });
        self.selected.get_or_insert_with(|| id.clone());
        Some(id)
    }

    /// Removes a profile only when the native owner explicitly unregisters it.
    pub fn remove(&mut self, environment_id: &str) -> Option<Arc<Snapshot>> {
        let removed = self
            .entries
            .remove(environment_id)
            .map(|entry| entry.snapshot);
        if self.selected.as_deref() == Some(environment_id) {
            self.selected = self.entries.keys().next().cloned();
        }
        removed
    }

    /// Retains a cached projection while its transport is being retried.
    pub fn mark_disconnected(&mut self, environment_id: &str, reason: Option<String>) -> bool {
        let Some(entry) = self.entries.get(environment_id) else {
            return false;
        };
        let mut snapshot = entry.snapshot.as_ref().clone();
        snapshot.connected = false;
        snapshot.error = reason;
        snapshot.revision = snapshot.revision.saturating_add(1);
        self.update(Arc::new(snapshot));
        true
    }

    pub fn select(&mut self, environment_id: &str) -> bool {
        if self.entries.contains_key(environment_id) {
            self.selected = Some(environment_id.to_owned());
            true
        } else {
            false
        }
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub fn snapshot(&self, environment_id: &str) -> Option<Arc<Snapshot>> {
        self.entries
            .get(environment_id)
            .map(|entry| entry.snapshot.clone())
    }

    pub fn summaries(&self) -> Vec<EnvironmentSummary> {
        let mut summaries: Vec<_> = self
            .entries
            .values()
            .map(|entry| entry.summary.clone())
            .collect();
        sort_environment_summaries(&mut summaries);
        summaries
    }

    pub fn filter_summaries(&self, query: &str) -> Vec<EnvironmentSummary> {
        filter_environment_summaries(&self.summaries(), query)
    }

    pub fn activities(&self, query: &str) -> Vec<AggregatedActivity> {
        let snapshots: Vec<_> = self
            .entries
            .values()
            .filter_map(|entry| entry.snapshot.awareness.clone())
            .collect();
        aggregate_activities(&snapshots, query)
    }

    /// Builds the combined sidebar from each registered Host projection.
    pub fn sidebar(&self, now_ms: i64, options: SidebarOptions) -> EnvironmentSidebarView {
        self.sidebar_filtered(now_ms, options, "")
    }

    /// Builds the combined sidebar after applying an environment/activity
    /// query. The local Snapshot still owns thread search results; this query
    /// narrows the Host sections before they are combined so a client cannot
    /// accidentally mix duplicate local identities.
    pub fn sidebar_filtered(
        &self,
        now_ms: i64,
        options: SidebarOptions,
        query: &str,
    ) -> EnvironmentSidebarView {
        let mut sections = Vec::with_capacity(self.entries.len());
        let ordered_summaries = filter_environment_summaries(&self.summaries(), query);
        for summary in ordered_summaries {
            let Some(entry) = self.entries.get(&summary.descriptor.environment_id) else {
                continue;
            };
            let Some(environment) = entry.snapshot.environment.as_ref() else {
                continue;
            };
            let mut local_options = options.clone();
            local_options.selection = options
                .selection
                .iter()
                .filter_map(|key| {
                    let reference = parse_scoped_thread_key(key)?;
                    (reference.environment_id == environment.environment_id)
                        .then_some(reference.thread_id)
                })
                .collect();
            let view = entry.snapshot.sidebar(now_ms, local_options);
            let drafts = view
                .drafts
                .into_iter()
                .map(|mut draft| {
                    draft.draft_key = scoped_key(&environment.environment_id, &draft.draft_key)
                        .unwrap_or(draft.draft_key);
                    draft.project_id = scoped_project_key(&ScopedProjectRef {
                        environment_id: environment.environment_id.clone(),
                        project_id: draft.project_id,
                    })
                    .unwrap_or_default();
                    draft
                })
                .collect();
            let rows = view
                .items
                .into_iter()
                .filter_map(|item| match item {
                    SidebarItem::Thread { mut row } => {
                        let local_id = row.id.clone();
                        row.id = scoped_thread_key(&ScopedThreadRef {
                            environment_id: environment.environment_id.clone(),
                            thread_id: local_id,
                        })?;
                        row.project_id = scoped_project_key(&ScopedProjectRef {
                            environment_id: environment.environment_id.clone(),
                            project_id: row.project_id,
                        })
                        .unwrap_or_default();
                        row.accessibility_label =
                            format!("{}, {}", environment.label, row.accessibility_label);
                        Some(EnvironmentThreadRow {
                            environment_id: environment.environment_id.clone(),
                            environment_label: environment.label.clone(),
                            row,
                        })
                    }
                    _ => None,
                })
                .collect();
            sections.push(EnvironmentSidebarSection {
                summary: entry.summary.clone(),
                drafts,
                rows,
            });
        }
        EnvironmentSidebarView {
            sections,
            activities: self.activities(query),
        }
    }

    /// Returns the active and working rows used by an all-environments inbox.
    pub fn inbox(&self, now_ms: i64, options: SidebarOptions) -> EnvironmentInboxView {
        self.inbox_filtered(now_ms, options, "")
    }

    pub fn inbox_filtered(
        &self,
        now_ms: i64,
        options: SidebarOptions,
        query: &str,
    ) -> EnvironmentInboxView {
        let sidebar = self.sidebar_filtered(now_ms, options, query);
        let mut items: Vec<_> = sidebar
            .sections
            .into_iter()
            .flat_map(|section| {
                section.rows.into_iter().filter_map(|item| {
                    matches!(
                        item.row.section,
                        SidebarSection::Pinned | SidebarSection::Active | SidebarSection::Working
                    )
                    .then_some(EnvironmentInboxItem {
                        environment_id: item.environment_id,
                        environment_label: item.environment_label,
                        row: item.row,
                    })
                })
            })
            .collect();
        items.sort_by(|left, right| {
            left.row
                .title
                .to_lowercase()
                .cmp(&right.row.title.to_lowercase())
                .then_with(|| left.environment_id.cmp(&right.environment_id))
        });
        EnvironmentInboxView { items }
    }

    pub fn settings(&self) -> EnvironmentSettingsView {
        let mut entries: Vec<_> = self
            .entries
            .values()
            .map(|entry| EnvironmentSettingsEntry {
                summary: entry.summary.clone(),
                settings: entry.snapshot.settings(SettingsScope::Host),
            })
            .collect();
        entries.sort_by(|left, right| {
            left.summary
                .descriptor
                .label
                .to_lowercase()
                .cmp(&right.summary.descriptor.label.to_lowercase())
                .then_with(|| {
                    left.summary
                        .descriptor
                        .environment_id
                        .cmp(&right.summary.descriptor.environment_id)
                })
        });
        EnvironmentSettingsView { entries }
    }

    /// Projects from every registered Host, ordered by environment and then
    /// the Host's project picker order. A scoped id is the only identifier
    /// clients should retain for a selection.
    pub fn project_rows(&self, query: &str) -> Vec<EnvironmentProjectRow> {
        let mut rows = Vec::new();
        for summary in self.summaries() {
            let Some(entry) = self.entries.get(&summary.descriptor.environment_id) else {
                continue;
            };
            rows.extend(
                entry
                    .snapshot
                    .project_picker(query.to_owned())
                    .rows
                    .into_iter()
                    .filter_map(|row| {
                        let project_id =
                            scoped_key(&summary.descriptor.environment_id, &row.project_id)?;
                        Some(EnvironmentProjectRow {
                            environment_id: summary.descriptor.environment_id.clone(),
                            environment_label: summary.descriptor.label.clone(),
                            project_id,
                            title: row.title,
                            subtitle: row.subtitle,
                        })
                    }),
            );
        }
        rows
    }

    /// Combines the mobile thread rows from every Host. The snapshots are
    /// cloned only to apply the aggregate query and scoped selection; all
    /// rendering and lifecycle decisions still come from the Host-owned core
    /// view function.
    pub fn thread_list(
        &self,
        now_ms: i64,
        options: ThreadListOptions,
        query: &str,
        selected_project: Option<&str>,
        selected_thread: Option<&str>,
    ) -> EnvironmentThreadListView {
        let mut rows = Vec::new();
        let mut pending_tasks = Vec::new();
        let mut has_threads = false;
        for summary in self.summaries() {
            let Some(entry) = self.entries.get(&summary.descriptor.environment_id) else {
                continue;
            };
            let environment_id = summary.descriptor.environment_id.as_str();
            let mut local = entry.snapshot.as_ref().clone();
            local.search = query.to_owned();
            local.search_matches.clear();
            local.search_request = None;
            local.selected_project = selected_project
                .and_then(parse_scoped_project_key)
                .filter(|reference| reference.environment_id == environment_id)
                .map(|reference| reference.project_id);
            if selected_project.is_some() && local.selected_project.is_none() {
                continue;
            }
            local.selected_thread = selected_thread
                .and_then(parse_scoped_thread_key)
                .filter(|reference| reference.environment_id == environment_id)
                .and_then(|reference| ThreadId::new(reference.thread_id.clone()).ok());
            let view = local.thread_list(now_ms, options);
            has_threads |= view.has_threads;
            for item in view.items {
                match item {
                    ThreadListItem::Thread { mut row } => {
                        let local_id = row.id.clone();
                        row.id = scoped_key(environment_id, &local_id).unwrap_or(local_id);
                        row.key = scoped_key(environment_id, &row.key).unwrap_or(row.key);
                        row.project_id =
                            scoped_key(environment_id, &row.project_id).unwrap_or_default();
                        scope_thread_menu(environment_id, &mut row.menu);
                        rows.push(EnvironmentThreadListRow {
                            environment_id: environment_id.to_owned(),
                            environment_label: summary.descriptor.label.clone(),
                            row,
                        });
                    }
                    ThreadListItem::PendingTask { mut task } => {
                        task.key = scoped_key(environment_id, &task.key).unwrap_or(task.key);
                        task.project_id =
                            scoped_key(environment_id, &task.project_id).unwrap_or_default();
                        task.kind = scope_pending_task(environment_id, task.kind);
                        pending_tasks.push(EnvironmentPendingTaskRow {
                            environment_id: environment_id.to_owned(),
                            environment_label: summary.descriptor.label.clone(),
                            task,
                        });
                    }
                    ThreadListItem::WorkingShelf { .. }
                    | ThreadListItem::SnoozedShelf { .. }
                    | ThreadListItem::SettledShelf { .. } => {}
                }
            }
        }
        EnvironmentThreadListView {
            rows,
            pending_tasks,
            has_threads,
        }
    }

    /// Settings entries retain the same rows each Host exposes while adding
    /// the environment identity needed to route an edit.
    pub fn settings_entries(&self) -> Vec<EnvironmentSettingsEntryView> {
        self.summaries()
            .into_iter()
            .filter_map(|summary| {
                let entry = self.entries.get(&summary.descriptor.environment_id)?;
                Some(EnvironmentSettingsEntryView {
                    environment_id: summary.descriptor.environment_id,
                    environment_label: summary.descriptor.label,
                    settings: entry.snapshot.settings(SettingsScope::Host),
                })
            })
            .collect()
    }

    /// Returns one immutable usage input per registered Host. Native clients
    /// can concatenate these records and pass the account/provider values to a
    /// shared usage widget builder without re-deriving Host state themselves.
    pub fn usage_inputs(&self) -> Vec<EnvironmentUsageInput> {
        let mut inputs: Vec<_> = self
            .entries
            .values()
            .filter_map(|entry| {
                let environment = entry.snapshot.environment.clone()?;
                let accounts = entry
                    .snapshot
                    .accounts
                    .as_ref()
                    .map_or_else(Vec::new, |accounts| {
                        accounts
                            .accounts
                            .iter()
                            .cloned()
                            .map(|mut account| {
                                account.id = scoped_key(&environment.environment_id, &account.id)
                                    .unwrap_or(account.id);
                                account
                            })
                            .collect()
                    });
                let providers = entry.snapshot.providers.as_deref().unwrap_or_default();
                let configured_provider_kinds = providers
                    .iter()
                    .filter(|provider| {
                        provider.enabled
                            && provider.installed
                            && provider.unavailable_reason.as_deref() != Some("unsupported")
                    })
                    .map(provider_kind)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                Some(EnvironmentUsageInput {
                    environment,
                    accounts,
                    configured_provider_kinds,
                })
            })
            .collect();
        inputs.sort_by(|left, right| {
            left.environment
                .label
                .to_lowercase()
                .cmp(&right.environment.label.to_lowercase())
                .then_with(|| {
                    left.environment
                        .environment_id
                        .cmp(&right.environment.environment_id)
                })
        });
        inputs
    }

    /// Aggregates the registry-owned usage inputs into the explicit values
    /// accepted by the shared usage widget builder. Native clients do not
    /// need to reconstruct account or provider state from individual Stores.
    pub fn usage_snapshot(&self) -> EnvironmentUsageSnapshot {
        let inputs = self.usage_inputs();
        let mut accounts = Vec::new();
        let mut configured_provider_kinds = BTreeSet::new();
        for input in inputs {
            accounts.extend(input.accounts);
            configured_provider_kinds.extend(input.configured_provider_kinds);
        }
        EnvironmentUsageSnapshot {
            accounts,
            configured_provider_kinds: configured_provider_kinds.into_iter().collect(),
        }
    }

    /// Evaluates automatic routing for a new draft whose source project lives
    /// on `source_environment_id`. Only connected Hosts that expose the same
    /// repository identity and selected provider participate. The returned
    /// route carries the target's local project id because equal project ids
    /// across Hosts are not interchangeable.
    pub fn evaluate_load_balancing(
        &self,
        source_environment_id: &str,
        project_id: &str,
        driver: Driver,
        provider_instance: Option<&str>,
        model: &str,
        options: &[ModelOption],
        runtime_mode: RuntimeMode,
        interaction_mode: InteractionMode,
        weights: &BTreeMap<String, u8>,
        now_ms: i64,
    ) -> EnvironmentLoadBalancingEvaluation {
        let Some(source) = self.entries.get(source_environment_id) else {
            return EnvironmentLoadBalancingEvaluation {
                candidate_count: 0,
                pending_resources: false,
                route: None,
            };
        };
        let Some(source_project) = source
            .snapshot
            .shell_projects()
            .iter()
            .find(|project| project.id == project_id)
        else {
            return EnvironmentLoadBalancingEvaluation {
                candidate_count: 0,
                pending_resources: false,
                route: None,
            };
        };
        let Some(repository_key) = source_project
            .repository_identity
            .as_ref()
            .map(|identity| identity.canonical_key.as_str())
            .filter(|key| !key.is_empty())
        else {
            return EnvironmentLoadBalancingEvaluation {
                candidate_count: 0,
                pending_resources: false,
                route: None,
            };
        };

        struct RouteCandidate {
            route: EnvironmentLoadBalancedRoute,
            capacity: LoadBalancingCandidate,
        }
        let mut route_candidates = Vec::new();
        for summary in self.summaries() {
            let Some(entry) = self.entries.get(&summary.descriptor.environment_id) else {
                continue;
            };
            if !entry.snapshot.connected {
                continue;
            }
            let Some(target_project) = entry.snapshot.shell_projects().iter().find(|project| {
                project
                    .repository_identity
                    .as_ref()
                    .is_some_and(|identity| identity.canonical_key == repository_key)
            }) else {
                continue;
            };
            let Some(provider) = entry
                .snapshot
                .providers
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|provider| {
                    provider.driver == driver
                        && provider_instance.is_none_or(|instance| {
                            instance.is_empty() || provider.instance == instance
                        })
                        && provider.enabled
                        && provider.installed
                        && !matches!(
                            provider.status,
                            agent_protocol::models::ProviderStatus::Error
                                | agent_protocol::models::ProviderStatus::Disabled
                        )
                        && provider.unavailable_reason.is_none()
                })
            else {
                continue;
            };
            let environment_id = summary.descriptor.environment_id.clone();
            let target_weight = entry
                .snapshot
                .preferences
                .load_balancing_weights
                .get(&environment_id)
                .copied()
                .or_else(|| weights.get(&environment_id).copied());
            let weight = load_balancing::preference_for_weight(target_weight);
            if weight == 0 {
                continue;
            }
            let Some(selected_model) = provider
                .models
                .iter()
                .find(|candidate| candidate.slug == model)
                .or_else(|| {
                    provider
                        .models
                        .iter()
                        .find(|candidate| candidate.is_default)
                })
                .or_else(|| provider.models.first())
            else {
                continue;
            };
            let runtime_mode = compatible_runtime_mode(
                runtime_mode,
                &runtime_mode_choices(&provider.supported_runtime_modes),
            );
            let interaction_mode = if provider.show_interaction_mode_toggle {
                interaction_mode
            } else {
                InteractionMode::Default
            };
            route_candidates.push(RouteCandidate {
                route: EnvironmentLoadBalancedRoute {
                    environment_id: environment_id.clone(),
                    project_id: target_project.id.clone(),
                    provider_instance: provider.instance.clone(),
                    driver,
                    model: selected_model.slug.clone(),
                    options: normalize_model_options(&selected_model.option_descriptors, options),
                    runtime_mode,
                    interaction_mode,
                },
                capacity: LoadBalancingCandidate {
                    environment_id,
                    resources: entry.snapshot.host_resources.clone(),
                    received_at_ms: entry.snapshot.host_resources_received_at_ms,
                    weight,
                },
            });
        }
        if route_candidates.len() < 2 {
            return EnvironmentLoadBalancingEvaluation {
                candidate_count: route_candidates.len(),
                pending_resources: false,
                route: None,
            };
        }
        let pending_resources = route_candidates.iter().any(|candidate| {
            !load_balancing::resource_sample_is_fresh(
                candidate.capacity.resources.is_some(),
                candidate.capacity.received_at_ms,
                now_ms,
            )
        });
        let capacities: Vec<_> = route_candidates
            .iter()
            .map(|candidate| candidate.capacity.clone())
            .collect();
        let route = load_balancing::select_environment(&capacities, now_ms).and_then(|id| {
            route_candidates
                .iter()
                .find(|candidate| candidate.route.environment_id == id)
                .map(|candidate| candidate.route.clone())
        });
        EnvironmentLoadBalancingEvaluation {
            candidate_count: route_candidates.len(),
            pending_resources,
            route,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

fn scope_pending_task(environment_id: &str, kind: PendingTaskKind) -> PendingTaskKind {
    match kind {
        PendingTaskKind::Queued {
            command_id,
            thread_id,
        } => PendingTaskKind::Queued {
            command_id,
            thread_id: scoped_key(environment_id, &thread_id).unwrap_or(thread_id),
        },
        PendingTaskKind::Draft { draft_key } => PendingTaskKind::Draft {
            draft_key: scoped_key(environment_id, &draft_key).unwrap_or(draft_key),
        },
    }
}

fn scope_thread_menu(environment_id: &str, items: &mut [ThreadMenuItem]) {
    for item in items {
        item.action = item
            .action
            .take()
            .map(|action| scope_thread_menu_action(environment_id, action));
        for child in &mut item.children {
            scope_thread_menu_child(environment_id, child);
        }
    }
}

fn scope_thread_menu_child(environment_id: &str, child: &mut ThreadMenuChild) {
    child.action = scope_thread_menu_action(environment_id, child.action.clone());
}

fn scope_thread_menu_action(environment_id: &str, action: ThreadMenuAction) -> ThreadMenuAction {
    match action {
        ThreadMenuAction::Thread { action } => ThreadMenuAction::Thread { action },
        ThreadMenuAction::FilterProject { project_id } => ThreadMenuAction::FilterProject {
            project_id: project_id.and_then(|id| scoped_key(environment_id, &id)),
        },
        ThreadMenuAction::NewThreadOnBranch {
            project_id,
            branch,
            worktree_path,
        } => ThreadMenuAction::NewThreadOnBranch {
            project_id: scoped_key(environment_id, &project_id).unwrap_or(project_id),
            branch,
            worktree_path,
        },
        ThreadMenuAction::OpenProjectSettings { project_id } => {
            ThreadMenuAction::OpenProjectSettings {
                project_id: scoped_key(environment_id, &project_id).unwrap_or(project_id),
            }
        }
        ThreadMenuAction::CopyThreadId { thread_id } => ThreadMenuAction::CopyThreadId {
            thread_id: scoped_key(environment_id, &thread_id).unwrap_or(thread_id),
        },
        other => other,
    }
}

fn provider_kind(provider: &ProviderInstance) -> ProviderKind {
    match provider.driver {
        agent_domain::Driver::Codex => ProviderKind::Codex,
        agent_domain::Driver::Claude => ProviderKind::Claude,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::ShellCache;
    use agent_domain::ThreadId;
    use agent_protocol::conversation::ShellSnapshot;
    use agent_protocol::models::{
        EnvironmentCapabilities, EnvironmentFileAttachments, EnvironmentInstallation,
        EnvironmentPlatform, Model, Project, ProjectRoot, ProviderInstance, ProviderStatus,
        RepositoryIdentity, RepositoryLocator,
    };
    use proptest::prelude::*;

    fn descriptor(id: &str, label: &str) -> EnvironmentDescriptor {
        EnvironmentDescriptor {
            environment_id: id.into(),
            label: label.into(),
            cwd: "/repo".into(),
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
        assert_eq!(capability_names(&capabilities), ["agentActivityPublishing"]);
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
        let mut file_descriptor = descriptor("one", "One");
        file_descriptor.capabilities = capabilities.clone();
        let value = serde_json::to_value(file_descriptor).unwrap();
        assert_eq!(
            value["capabilities"]["fileAttachments"]["maxUploadBytes"],
            42
        );
        capabilities.server_installation = Some(EnvironmentInstallation::NpmGlobal {
            prefix: "/usr/local".into(),
        });
        let mut installation_descriptor = descriptor("one", "One");
        installation_descriptor.capabilities = capabilities;
        let value = serde_json::to_value(installation_descriptor).unwrap();
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

    #[test]
    fn environment_registry_keeps_cached_hosts_and_projects_consumers() {
        let snapshot = |id: &str, label: &str| {
            let environment = descriptor(id, label);
            let activity = AwarenessActivity {
                environment_id: id.into(),
                thread_id: "same-thread".into(),
                project_title: "Project".into(),
                thread_title: format!("{label} thread"),
                phase: AgentActivityPhase::Running,
                headline: "Working".into(),
                detail: Some("Building".into()),
                model_title: None,
                updated_at_ms: 4,
            };
            Arc::new(Snapshot {
                store_id: id.into(),
                revision: 1,
                connected: true,
                environment: Some(environment.clone()),
                awareness: Some(AwarenessSnapshot {
                    environment,
                    activities: vec![activity],
                    updated_at_ms: 4,
                }),
                ..Snapshot::default()
            })
        };

        let mut registry = EnvironmentRegistry::default();
        registry.update(snapshot("z", "Shared"));
        registry.update(snapshot("a", "Shared"));
        let mut account_snapshot = (*snapshot("z", "Shared")).clone();
        account_snapshot.accounts = Some(agent_protocol::operations::Accounts {
            accounts: vec![agent_protocol::operations::Account {
                id: "account".into(),
                provider: ProviderKind::Codex,
                email: Some("user@example.com".into()),
                plan_type: None,
                usage: None,
            }],
            selected: std::collections::HashMap::new(),
            error: None,
        });
        account_snapshot.providers = Some(vec![
            ProviderInstance {
                instance: "codex".into(),
                driver: agent_domain::Driver::Codex,
                display_name: "Codex".into(),
                accent_color: None,
                enabled: true,
                installed: true,
                version: None,
                version_advisory: None,
                status: ProviderStatus::Ready,
                message: None,
                unavailable_reason: None,
                show_interaction_mode_toggle: false,
                reports_context_window: false,
                supported_runtime_modes: Vec::new(),
                models: Vec::new(),
            },
            ProviderInstance {
                instance: "claude".into(),
                driver: agent_domain::Driver::Claude,
                display_name: "Claude".into(),
                accent_color: None,
                enabled: false,
                installed: true,
                version: None,
                version_advisory: None,
                status: ProviderStatus::Disabled,
                message: None,
                unavailable_reason: None,
                show_interaction_mode_toggle: false,
                reports_context_window: false,
                supported_runtime_modes: Vec::new(),
                models: Vec::new(),
            },
        ]);
        registry.update(Arc::new(account_snapshot));
        assert_eq!(registry.len(), 2);
        assert_eq!(registry.selected(), Some("z"));
        assert_eq!(
            registry
                .summaries()
                .iter()
                .map(|summary| summary.descriptor.environment_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "z"]
        );
        assert_eq!(registry.filter_summaries("shared").len(), 2);
        assert_eq!(registry.activities("building").len(), 2);
        assert_eq!(registry.usage_inputs()[1].accounts[0].id, "z:account");
        let usage = registry.usage_snapshot();
        assert_eq!(usage.accounts[0].id, "z:account");
        assert_eq!(usage.configured_provider_kinds, vec![ProviderKind::Codex]);

        let provider = |driver, enabled, installed, unavailable_reason| ProviderInstance {
            instance: format!("{driver:?}"),
            driver,
            display_name: format!("{driver:?}"),
            accent_color: None,
            enabled,
            installed,
            version: None,
            version_advisory: None,
            status: if enabled && installed {
                ProviderStatus::Ready
            } else {
                ProviderStatus::Disabled
            },
            message: None,
            unavailable_reason,
            show_interaction_mode_toggle: false,
            reports_context_window: false,
            supported_runtime_modes: Vec::new(),
            models: Vec::new(),
        };
        let mut split = EnvironmentRegistry::default();
        let mut enabled_only = (*snapshot("enabled", "Enabled")).clone();
        enabled_only.providers = Some(vec![provider(
            agent_domain::Driver::Codex,
            true,
            false,
            None,
        )]);
        split.update(Arc::new(enabled_only));
        let mut installed_only = (*snapshot("installed", "Installed")).clone();
        installed_only.providers = Some(vec![provider(
            agent_domain::Driver::Codex,
            false,
            true,
            None,
        )]);
        split.update(Arc::new(installed_only));
        assert!(split.usage_snapshot().configured_provider_kinds.is_empty());

        let mut unsupported = (*snapshot("unsupported", "Unsupported")).clone();
        unsupported.providers = Some(vec![provider(
            agent_domain::Driver::Claude,
            true,
            true,
            Some("unsupported".into()),
        )]);
        split.update(Arc::new(unsupported));
        assert!(split.usage_snapshot().configured_provider_kinds.is_empty());
        assert_eq!(
            registry
                .sidebar_filtered(0, SidebarOptions::default(), "a")
                .sections
                .len(),
            1
        );
        assert_eq!(registry.inbox(0, SidebarOptions::default()).items.len(), 0);
        assert_eq!(registry.settings().entries.len(), 2);

        assert!(registry.mark_disconnected("z", Some("network timeout".into())));
        assert_eq!(
            registry.summaries()[1].connection,
            EnvironmentConnectionState::Disconnected
        );
        assert_eq!(registry.filter_summaries("timeout").len(), 1);
        assert_eq!(
            registry.snapshot("z").unwrap().environment_display_label(),
            Some("Shared")
        );
    }

    #[test]
    fn aggregate_row_actions_preserve_their_environment_namespace() {
        let action = scope_thread_menu_action(
            "host-a",
            ThreadMenuAction::NewThreadOnBranch {
                project_id: "same-project".into(),
                branch: "main".into(),
                worktree_path: None,
            },
        );
        assert_eq!(
            action,
            ThreadMenuAction::NewThreadOnBranch {
                project_id: "host-a:same-project".into(),
                branch: "main".into(),
                worktree_path: None,
            }
        );
        assert_eq!(
            scope_pending_task(
                "host-b",
                PendingTaskKind::Queued {
                    command_id: "command".into(),
                    thread_id: "same-thread".into(),
                }
            ),
            PendingTaskKind::Queued {
                command_id: "command".into(),
                thread_id: "host-b:same-thread".into(),
            }
        );
    }

    #[test]
    fn aggregate_thread_rows_scope_duplicate_local_ids_per_host() {
        let snapshot = |environment_id: &str| {
            let mut thread = agent_domain::shell(&crate::sync::fixtures::thread_state("same"))
                .expect("fixture shell row");
            thread.id = ThreadId::new("same-thread").unwrap();
            Arc::new(Snapshot {
                environment: Some(descriptor(environment_id, environment_id)),
                connected: true,
                shell: Arc::new(ShellCache::from_cache(ShellSnapshot {
                    snapshot_sequence: 1,
                    projects: vec![],
                    threads: vec![thread],
                })),
                ..Snapshot::default()
            })
        };
        let mut registry = EnvironmentRegistry::default();
        registry.update(snapshot("host-a"));
        registry.update(snapshot("host-b"));
        let rows = registry
            .thread_list(0, ThreadListOptions::default(), "", None, None)
            .rows;
        assert_eq!(
            rows.iter()
                .map(|row| row.row.id.as_str())
                .collect::<Vec<_>>(),
            ["host-a:same-thread", "host-b:same-thread"]
        );
    }

    #[test]
    fn load_balancing_routes_same_repository_to_the_best_connected_host() {
        let project = |id: &str| Project {
            id: id.into(),
            name: "Shared project".into(),
            roots: vec![ProjectRoot {
                path: format!("/work/{id}"),
            }],
            repository_identity: Some(RepositoryIdentity {
                canonical_key: "github.com/example/shared".into(),
                locator: RepositoryLocator {
                    source: "git-remote".into(),
                    remote_name: "origin".into(),
                    remote_url: "https://github.com/example/shared.git".into(),
                },
                web_url: None,
                root_path: None,
                display_name: Some("Shared".into()),
                provider: Some("github".into()),
                owner: Some("example".into()),
                name: Some("shared".into()),
            }),
            ..Project::default()
        };
        let provider = ProviderInstance {
            instance: "codex".into(),
            driver: Driver::Codex,
            display_name: "Codex".into(),
            accent_color: None,
            enabled: true,
            installed: true,
            version: None,
            version_advisory: None,
            status: ProviderStatus::Ready,
            message: None,
            unavailable_reason: None,
            show_interaction_mode_toggle: false,
            reports_context_window: true,
            supported_runtime_modes: vec![RuntimeMode::ApprovalRequired],
            models: vec![Model {
                slug: "shared".into(),
                name: "Shared".into(),
                aliases: vec![],
                badge: None,
                is_default: true,
                is_legacy: false,
                option_descriptors: vec![],
            }],
        };
        let snapshot = |environment_id: &str, project_id: &str, cpu_count: u64| {
            Arc::new(Snapshot {
                environment: Some(descriptor(environment_id, environment_id)),
                connected: true,
                shell: Arc::new(ShellCache::from_cache(ShellSnapshot {
                    snapshot_sequence: 1,
                    projects: vec![project(project_id)],
                    threads: vec![],
                })),
                providers: Some(vec![provider.clone()]),
                host_resources: Some(agent_protocol::background::HostResourcesSnapshot {
                    sampled_at: 100_000,
                    cpu_utilization: Some(0.2),
                    cpu_count,
                    available_memory_bytes: 8_000,
                    total_memory_bytes: 16_000,
                }),
                host_resources_received_at_ms: Some(100_000),
                ..Snapshot::default()
            })
        };
        let mut registry = EnvironmentRegistry::default();
        registry.update(snapshot("host-a", "project-a", 2));
        registry.update(snapshot("host-b", "project-b", 8));
        let evaluation = registry.evaluate_load_balancing(
            "host-a",
            "project-a",
            Driver::Codex,
            Some("codex"),
            "shared",
            &[],
            RuntimeMode::FullAccess,
            InteractionMode::Plan,
            &BTreeMap::new(),
            100_000,
        );
        assert_eq!(evaluation.candidate_count, 2);
        assert!(!evaluation.pending_resources);
        assert_eq!(
            evaluation.route,
            Some(EnvironmentLoadBalancedRoute {
                environment_id: "host-b".into(),
                project_id: "project-b".into(),
                provider_instance: "codex".into(),
                driver: Driver::Codex,
                model: "shared".into(),
                options: vec![],
                runtime_mode: RuntimeMode::ApprovalRequired,
                interaction_mode: InteractionMode::Default,
            })
        );
        let weights = BTreeMap::from([(String::from("host-b"), 0)]);
        let manual_only = registry.evaluate_load_balancing(
            "host-a",
            "project-a",
            Driver::Codex,
            Some("codex"),
            "shared",
            &[],
            RuntimeMode::FullAccess,
            InteractionMode::Plan,
            &weights,
            100_000,
        );
        assert_eq!(manual_only.candidate_count, 1);
        assert!(manual_only.route.is_none());

        let mut missing_resources = (*snapshot("host-b", "project-b", 8)).clone();
        missing_resources.host_resources = None;
        missing_resources.host_resources_received_at_ms = None;
        let mut pending_registry = EnvironmentRegistry::default();
        pending_registry.update(snapshot("host-a", "project-a", 2));
        pending_registry.update(Arc::new(missing_resources));
        let pending = pending_registry.evaluate_load_balancing(
            "host-a",
            "project-a",
            Driver::Codex,
            Some("codex"),
            "shared",
            &[],
            RuntimeMode::FullAccess,
            InteractionMode::Plan,
            &BTreeMap::new(),
            100_000,
        );
        assert_eq!(pending.candidate_count, 2);
        assert!(pending.pending_resources);
        assert!(pending.route.is_none());

        let mut stale_resources = (*snapshot("host-b", "project-b", 8)).clone();
        stale_resources.host_resources_received_at_ms = Some(100_000 - 15_001);
        let mut stale_registry = EnvironmentRegistry::default();
        stale_registry.update(snapshot("host-a", "project-a", 2));
        stale_registry.update(Arc::new(stale_resources));
        let stale = stale_registry.evaluate_load_balancing(
            "host-a",
            "project-a",
            Driver::Codex,
            Some("codex"),
            "shared",
            &[],
            RuntimeMode::FullAccess,
            InteractionMode::Plan,
            &BTreeMap::new(),
            100_000,
        );
        assert!(stale.pending_resources);
        assert!(stale.route.is_none());

        let mut fallback_provider = provider.clone();
        fallback_provider.models = vec![Model {
            slug: "fallback".into(),
            name: "Fallback".into(),
            aliases: vec![],
            badge: None,
            is_default: true,
            is_legacy: false,
            option_descriptors: vec![agent_domain::OptionDescriptor::Select(
                agent_domain::SelectOption {
                    id: "effort".into(),
                    label: "Effort".into(),
                    description: None,
                    options: vec![agent_domain::OptionChoice {
                        id: "high".into(),
                        label: "High".into(),
                        description: None,
                        is_default: true,
                    }],
                    current_value: Some("high".into()),
                    prompt_injected_values: vec![],
                },
            )],
        }];
        let mut fallback_snapshot = (*snapshot("host-b", "project-b", 8)).clone();
        fallback_snapshot.providers = Some(vec![fallback_provider]);
        let mut fallback_registry = EnvironmentRegistry::default();
        fallback_registry.update(snapshot("host-a", "project-a", 2));
        fallback_registry.update(Arc::new(fallback_snapshot));
        let fallback = fallback_registry.evaluate_load_balancing(
            "host-a",
            "project-a",
            Driver::Codex,
            Some("codex"),
            "missing-model",
            &[
                ModelOption {
                    key: "effort".into(),
                    value: "invalid".into(),
                },
                ModelOption {
                    key: "old-option".into(),
                    value: "value".into(),
                },
            ],
            RuntimeMode::FullAccess,
            InteractionMode::Plan,
            &BTreeMap::new(),
            100_000,
        );
        assert_eq!(
            fallback.route.as_ref().map(|route| route.model.as_str()),
            Some("fallback")
        );
        assert_eq!(
            fallback.route.as_ref().map(|route| route.options.clone()),
            Some(vec![ModelOption {
                key: "effort".into(),
                value: "high".into(),
            }])
        );
        assert_eq!(
            fallback.route.as_ref().map(|route| route.runtime_mode),
            Some(RuntimeMode::ApprovalRequired)
        );
        assert_eq!(
            fallback.route.as_ref().map(|route| route.interaction_mode),
            Some(InteractionMode::Default)
        );
    }
}

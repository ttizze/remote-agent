//! Shared typed models. Unknown provider fields are ignored.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invitation {
    pub endpoint: String,
    pub invitation: uuid::Uuid,
    pub expires_at: u64,
    pub host_name: String,
    pub ai_recipients: Vec<String>,
    pub transcription_recipient: Option<String>,
}
impl std::fmt::Debug for Invitation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invitation")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteHost {
    pub id: String,
    pub name: String,
    pub ticket: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostStatus {
    pub node_id: String,
    pub name: String,
    pub devices: Vec<String>,
    #[serde(default)]
    #[serde(with = "crate::protocol::json")]
    pub provider_errors: Option<Map<String, Value>>,
}

/// The stable identity and capabilities of the Host serving a connection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentPlatform {
    pub os: String,
    pub arch: String,
    pub machine: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentFileAttachments {
    pub max_upload_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum EnvironmentInstallation {
    Npx,
    PnpmDlx,
    Bunx,
    NpmGlobal { prefix: String },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCapabilities {
    pub repository_identity: bool,
    pub connection_probe: bool,
    pub attachment_uploads: bool,
    pub question_attachments: bool,
    pub file_attachments: Option<EnvironmentFileAttachments>,
    pub pull_requests: bool,
    pub pull_request_checks: bool,
    pub inline_message_context: bool,
    pub required_worktree_bootstrap: bool,
    pub thread_settlement: bool,
    pub thread_auto_settlement: bool,
    pub thread_snooze: bool,
    pub storage_cleanup: bool,
    pub project_worktree_cleanup: bool,
    pub thread_restart_continuation: bool,
    pub project_settings_overrides: bool,
    pub environment_themes: bool,
    pub usage_limit_sources: bool,
    pub usage_price_overrides: bool,
    pub usage_model_aliases: bool,
    pub thread_pinning: bool,
    pub thread_pin_reorder: bool,
    pub thread_active_reorder: bool,
    pub thread_auto_settle_opt_out: bool,
    pub thread_title_regeneration: bool,
    pub thread_visited_tracking: bool,
    pub thread_pull_request_linking: bool,
    pub server_resolved_command_context: bool,
    pub thread_pull_requests: bool,
    pub thread_pull_request_watch: bool,
    pub pull_request_stack_actions: bool,
    pub server_self_update: Option<String>,
    pub server_installation: Option<EnvironmentInstallation>,
    pub server_self_update_progress: bool,
    pub server_update_thread_continuation: bool,
    pub project_clone_tracking: bool,
    pub environment_icon: bool,
    pub desktop_app_update: bool,
    pub agent_activity_publishing: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDescriptor {
    pub environment_id: String,
    pub label: String,
    pub platform: EnvironmentPlatform,
    pub server_version: String,
    pub orchestration_protocol_version: Option<u32>,
    pub capabilities: EnvironmentCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwarenessRegistration {
    pub device_id: String,
    pub label: String,
    pub platform: String,
    pub app_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwarenessRegistrationResult {
    pub accepted: bool,
    pub registered_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentActivityPhase {
    Starting,
    Running,
    WaitingApproval,
    WaitingInput,
    Completed,
    Failed,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwarenessActivity {
    pub environment_id: String,
    pub thread_id: String,
    pub project_title: String,
    pub thread_title: String,
    pub phase: AgentActivityPhase,
    pub headline: String,
    pub detail: Option<String>,
    pub model_title: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwarenessSnapshot {
    pub environment: EnvironmentDescriptor,
    pub activities: Vec<AwarenessActivity>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub roots: Vec<ProjectRoot>,
    #[serde(default)]
    pub scripts: Vec<ProjectScript>,
    /// Resolved from the root's Git remotes when the Host lists the project; never
    /// stored.
    #[serde(default)]
    pub repository_identity: Option<RepositoryIdentity>,
    /// The icon file the user chose, absolute or relative to the root. The icon
    /// itself comes from `host/project/favicon`.
    pub favicon_path: Option<String>,
    pub created_at: Option<agent_domain::Timestamp>,
    pub updated_at: Option<agent_domain::Timestamp>,
}

/// The saved icon path a project accepts: trimmed, non-empty, at most 1024
/// UTF-16 units, ending in an image extension.
pub fn project_favicon_path(value: &str) -> Result<String, String> {
    let js_space = |c: char| c == '\u{feff}' || (c != '\u{85}' && c.is_whitespace());
    let value = value.trim_matches(js_space);
    if value.is_empty() {
        return Err("project favicon path must not be empty".into());
    }
    if value.encode_utf16().count() > 1024 {
        return Err("project favicon path must be at most 1024 characters".into());
    }
    if image_mime_type(value).is_none() {
        return Err(
            "project favicon path must end in .avif, .gif, .ico, .jpg, .jpeg, .png, .svg or .webp"
                .into(),
        );
    }
    Ok(value.to_owned())
}

/// The image type of a path by its extension, ignoring ASCII case.
pub fn image_mime_type(path: &str) -> Option<&'static str> {
    [
        (".avif", "image/avif"),
        (".gif", "image/gif"),
        (".ico", "image/x-icon"),
        (".jpeg", "image/jpeg"),
        (".jpg", "image/jpeg"),
        (".png", "image/png"),
        (".svg", "image/svg+xml"),
        (".webp", "image/webp"),
    ]
    .into_iter()
    .find(|(extension, _)| {
        path.len() >= extension.len()
            && path.as_bytes()[path.len() - extension.len()..]
                .eq_ignore_ascii_case(extension.as_bytes())
    })
    .map(|(_, mime_type)| mime_type)
}

/// Asks for a project's icon; `known_hash` is the hash of the copy the client has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadProjectFavicon {
    pub project_id: String,
    pub known_hash: Option<String>,
}
/// A project's icon file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFavicon {
    /// SHA-256 of the file, in lowercase hex.
    pub hash: String,
    /// `v<hash>-<file name>`.
    pub file_name: String,
    pub mime_type: String,
    /// Absent when `hash` equals the request's `known_hash`.
    #[serde(with = "crate::protocol::optional_bytes")]
    pub data: Option<Vec<u8>>,
}
/// The repository a project's checkout belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentity {
    /// The normalized remote URL, shared by every clone.
    pub canonical_key: String,
    pub locator: RepositoryLocator,
    pub web_url: Option<String>,
    pub root_path: Option<String>,
    pub display_name: Option<String>,
    pub provider: Option<String>,
    pub owner: Option<String>,
    pub name: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryLocator {
    /// Always `git-remote`.
    pub source: String,
    pub remote_name: String,
    pub remote_url: String,
}
/// A project action. The first one that runs on worktree creation is the project's setup script.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectScript {
    pub id: String,
    pub name: String,
    pub command: String,
    pub icon: ProjectScriptIcon,
    pub run_on_worktree_create: bool,
    /// The agent starts while the setup runs unless this is `false`.
    #[serde(rename = "async", default)]
    pub run_async: Option<bool>,
    #[serde(default)]
    pub preview_url: Option<String>,
    #[serde(default)]
    pub auto_open_preview: Option<bool>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectScriptIcon {
    Play,
    Test,
    Lint,
    Configure,
    Build,
    Debug,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectRoot {
    pub path: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderStatus {
    Ready,
    Warning,
    Error,
    Disabled,
}

/// One provider instance as the composer and settings show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstance {
    pub instance: String,
    pub driver: agent_domain::Driver,
    pub display_name: String,
    /// `#rrggbb` when the instance is configured with one.
    pub accent_color: Option<String>,
    pub enabled: bool,
    pub installed: bool,
    pub version: Option<String>,
    pub status: ProviderStatus,
    /// Why the status is not ready, or advice such as an upgrade.
    pub message: Option<String>,
    /// Set when this Host cannot run the instance at all.
    pub unavailable_reason: Option<String>,
    pub show_interaction_mode_toggle: bool,
    pub reports_context_window: bool,
    /// The permission modes the instance offers; empty offers all of them.
    pub supported_runtime_modes: Vec<agent_domain::RuntimeMode>,
    pub models: Vec<Model>,
}

/// One model of a provider instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub slug: String,
    pub name: String,
    pub aliases: Vec<String>,
    /// `new` for a recently added model.
    pub badge: Option<String>,
    pub is_default: bool,
    pub is_legacy: bool,
    pub option_descriptors: Vec<agent_domain::OptionDescriptor>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileList {
    pub path: String,
    pub entries: Vec<FileEntry>,
    pub truncated: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub size: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    pub path: String,
    pub revision: String,
    pub text: String,
    pub size: u64,
}
/// When a project's threads settle on their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AutoSettle {
    Never,
    /// 1 to 90 days after the thread's last activity.
    AfterDays(u32),
}

/// One project's overrides; an absent value inherits the Host's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectConversationSettings {
    pub auto_settle: Option<AutoSettle>,
    /// Settle a thread when its linked pull request is merged.
    pub auto_settle_on_merge: Option<bool>,
    pub continue_after_restart: Option<bool>,
    pub new_worktrees_start_from_origin: Option<bool>,
    pub branch_naming_mode: Option<agent_domain::BranchNamingMode>,
    pub branch_name_prefix: Option<String>,
    pub branch_name_instructions: Option<String>,
}

/// One sparse project override edit, including an explicit return to inheritance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OverrideChange<T> {
    Inherit,
    Value(T),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectConversationSettingsPatch {
    pub auto_settle: Option<OverrideChange<AutoSettle>>,
    pub auto_settle_on_merge: Option<OverrideChange<bool>>,
    pub continue_after_restart: Option<OverrideChange<bool>>,
    pub new_worktrees_start_from_origin: Option<OverrideChange<bool>>,
    pub branch_naming_mode: Option<OverrideChange<agent_domain::BranchNamingMode>>,
    pub branch_name_prefix: Option<OverrideChange<String>>,
    pub branch_name_instructions: Option<OverrideChange<String>>,
}

fn patched_override<T: Clone>(
    current: &Option<T>,
    change: &Option<OverrideChange<T>>,
) -> Option<T> {
    match change {
        None => current.clone(),
        Some(OverrideChange::Inherit) => None,
        Some(OverrideChange::Value(value)) => Some(value.clone()),
    }
}
impl ProjectConversationSettings {
    fn patched(&self, patch: &ProjectConversationSettingsPatch) -> Self {
        Self {
            auto_settle: patched_override(&self.auto_settle, &patch.auto_settle),
            auto_settle_on_merge: patched_override(
                &self.auto_settle_on_merge,
                &patch.auto_settle_on_merge,
            ),
            continue_after_restart: patched_override(
                &self.continue_after_restart,
                &patch.continue_after_restart,
            ),
            new_worktrees_start_from_origin: patched_override(
                &self.new_worktrees_start_from_origin,
                &patch.new_worktrees_start_from_origin,
            ),
            branch_naming_mode: patched_override(
                &self.branch_naming_mode,
                &patch.branch_naming_mode,
            ),
            branch_name_prefix: patched_override(
                &self.branch_name_prefix,
                &patch.branch_name_prefix,
            ),
            branch_name_instructions: patched_override(
                &self.branch_name_instructions,
                &patch.branch_name_instructions,
            ),
        }
    }
}

/// Host conversation settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConversationSettings {
    pub auto_settle: AutoSettle,
    /// Settle a thread when its linked pull request is merged.
    pub auto_settle_on_merge: bool,
    /// Continue turns a Host restart cut.
    pub continue_after_restart: bool,
    pub snooze_limited_threads: bool,
    pub auto_resume_limited_threads: bool,
    /// A new worktree starts from the latest matching branch on origin.
    pub new_worktrees_start_from_origin: bool,
    /// How launches name the worktree branches they generate.
    pub branch_naming_mode: agent_domain::BranchNamingMode,
    pub branch_name_prefix: String,
    pub branch_name_instructions: String,
    /// How often the Git status fetches a subscribed checkout's upstream;
    /// zero fetches once and then only on explicit refreshes.
    pub source_control_auto_fetch_interval_seconds: u32,
    pub project_overrides: std::collections::BTreeMap<String, ProjectConversationSettings>,
}
impl Default for ConversationSettings {
    fn default() -> Self {
        let naming = agent_domain::BranchNaming::default();
        Self {
            auto_settle: AutoSettle::AfterDays(3),
            auto_settle_on_merge: true,
            continue_after_restart: false,
            snooze_limited_threads: false,
            auto_resume_limited_threads: false,
            new_worktrees_start_from_origin: true,
            branch_naming_mode: naming.mode,
            branch_name_prefix: naming.prefix,
            branch_name_instructions: naming.instructions,
            source_control_auto_fetch_interval_seconds: 30,
            project_overrides: Default::default(),
        }
    }
}
/// `host/conversation/settings/update`: the settings to change. The Host
/// merges it into its current settings and keeps every field it leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConversationSettingsPatch {
    pub auto_settle: Option<AutoSettle>,
    pub auto_settle_on_merge: Option<bool>,
    pub continue_after_restart: Option<bool>,
    pub new_worktrees_start_from_origin: Option<bool>,
    pub snooze_limited_threads: Option<bool>,
    pub auto_resume_limited_threads: Option<bool>,
    pub branch_naming_mode: Option<agent_domain::BranchNamingMode>,
    pub branch_name_prefix: Option<String>,
    pub branch_name_instructions: Option<String>,
    pub source_control_auto_fetch_interval_seconds: Option<u32>,
    /// Each entry edits only the supplied project fields; `None` removes all overrides.
    pub project_overrides:
        std::collections::BTreeMap<String, Option<ProjectConversationSettingsPatch>>,
}

impl ConversationSettings {
    /// These settings with the patch's fields replaced.
    pub fn patched(&self, patch: &ConversationSettingsPatch) -> Self {
        let mut next = self.clone();
        let ConversationSettingsPatch {
            auto_settle,
            auto_settle_on_merge,
            continue_after_restart,
            new_worktrees_start_from_origin,
            snooze_limited_threads,
            auto_resume_limited_threads,
            branch_naming_mode,
            branch_name_prefix,
            branch_name_instructions,
            source_control_auto_fetch_interval_seconds,
            project_overrides,
        } = patch.clone();
        if let Some(value) = auto_settle {
            next.auto_settle = value;
        }
        if let Some(value) = auto_settle_on_merge {
            next.auto_settle_on_merge = value;
        }
        if let Some(value) = continue_after_restart {
            next.continue_after_restart = value;
        }
        if let Some(value) = new_worktrees_start_from_origin {
            next.new_worktrees_start_from_origin = value;
        }
        if let Some(value) = snooze_limited_threads {
            next.snooze_limited_threads = value;
        }
        if let Some(value) = auto_resume_limited_threads {
            next.auto_resume_limited_threads = value;
        }
        if let Some(value) = branch_naming_mode {
            next.branch_naming_mode = value;
        }
        if let Some(value) = branch_name_prefix {
            next.branch_name_prefix = value;
        }
        if let Some(value) = branch_name_instructions {
            next.branch_name_instructions = value;
        }
        if let Some(value) = source_control_auto_fetch_interval_seconds {
            next.source_control_auto_fetch_interval_seconds = value;
        }
        for (project, overrides) in project_overrides {
            match overrides {
                Some(patch) => {
                    let overrides = next
                        .project_overrides
                        .get(&project)
                        .cloned()
                        .unwrap_or_default()
                        .patched(&patch);
                    if overrides == ProjectConversationSettings::default() {
                        next.project_overrides.remove(&project)
                    } else {
                        next.project_overrides.insert(project, overrides)
                    }
                }
                None => next.project_overrides.remove(&project),
            };
        }
        next
    }

    pub fn validate(&self) -> Result<(), String> {
        let days = |settle: &AutoSettle| match settle {
            AutoSettle::AfterDays(days) if !(1..=90).contains(days) => {
                Err("automatic settlement needs 1 to 90 days".to_owned())
            }
            _ => Ok(()),
        };
        days(&self.auto_settle)?;
        self.project_overrides
            .values()
            .filter_map(|project| project.auto_settle.as_ref())
            .try_for_each(days)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorktreeSettings {
    pub create_on_new_session: bool,
    pub copy_on_create: bool,
    pub copy_paths: Vec<String>,
    pub worktree_directory: String,
    pub delete_merged: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub path: String,
    pub project_path: String,
    pub branch: String,
    pub blocked_reason: Option<String>,
    pub threads: Vec<WorktreeThread>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeThread {
    pub id: agent_domain::ThreadId,
    pub name: String,
    pub active: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceReview {
    pub branch: String,
    pub additions: u64,
    pub deletions: u64,
    pub files: Vec<ChangedFile>,
    pub diff: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn worktree_status_distinguishes_pending_work_from_integrated_history(
            dirty in any::<bool>(),
            unmerged_changes in any::<bool>(),
            merged_history in any::<bool>(),
        ) {
            let status = worktree_branch_status(dirty, unmerged_changes, merged_history);
            let expected = match (dirty, unmerged_changes, merged_history) {
                (true, _, _) | (_, true, _) => Some(WorktreeStatus::Unmerged),
                (false, false, true) => Some(WorktreeStatus::Merged),
                _ => None,
            };
            prop_assert_eq!(status, expected);
        }
    }

    // contracts/project.ts ProjectFaviconPath.
    #[test]
    fn saved_favicon_paths_are_trimmed_image_paths() {
        assert_eq!(
            project_favicon_path("  brand/Logo.PNG\u{feff}").as_deref(),
            Ok("brand/Logo.PNG")
        );
        assert_eq!(
            project_favicon_path("/Users/me/Pictures/icon.jpeg").as_deref(),
            Ok("/Users/me/Pictures/icon.jpeg")
        );
        for extension in ["avif", "gif", "ico", "jpg", "JPEG", "png", "Svg", "webp"] {
            assert!(project_favicon_path(&format!("icon.{extension}")).is_ok());
        }
        for rejected in [
            "",
            " \n ",
            "icon.txt",
            "icon.svg.bak",
            "icon",
            "iconpng",
            "icon.\u{17f}vg",
        ] {
            assert!(project_favicon_path(rejected).is_err(), "{rejected:?}");
        }
        let longest = format!("{}.png", "a".repeat(1020));
        assert!(project_favicon_path(&longest).is_ok());
        assert!(project_favicon_path(&format!("a{longest}")).is_err());
        let wide = format!("{}.png", "\u{1f600}".repeat(510));
        assert!(project_favicon_path(&wide).is_ok());
        assert!(project_favicon_path(&format!("\u{1f600}{wide}")).is_err());
    }

    #[test]
    fn merged_settlement_setting_is_independent_of_inactivity_settlement() {
        let defaults = ConversationSettings::default();
        assert_eq!(defaults.auto_settle, AutoSettle::AfterDays(3));
        assert!(defaults.auto_settle_on_merge);

        let disabled = defaults.patched(&ConversationSettingsPatch {
            auto_settle: Some(AutoSettle::Never),
            auto_settle_on_merge: Some(false),
            ..Default::default()
        });
        assert_eq!(disabled.auto_settle, AutoSettle::Never);
        assert!(!disabled.auto_settle_on_merge);

        let enabled = defaults.patched(&ConversationSettingsPatch {
            auto_settle: Some(AutoSettle::Never),
            project_overrides: [(
                "project".into(),
                Some(ProjectConversationSettingsPatch {
                    auto_settle_on_merge: Some(OverrideChange::Value(true)),
                    ..Default::default()
                }),
            )]
            .into(),
            ..Default::default()
        });
        assert_eq!(enabled.auto_settle, AutoSettle::Never);
        assert_eq!(
            enabled.project_overrides["project"].auto_settle_on_merge,
            Some(true)
        );
    }
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct Empty {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferGrant {
    pub token: [u8; 32],
    pub size: u64,
    pub sha256: [u8; 32],
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadedFile {
    pub attachment: Option<agent_domain::Attachment>,
    pub path: String,
    pub size: u64,
    pub sha256: [u8; 32],
}

pub fn compact_title(value: &str) -> String {
    let line = value.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(120) {
        Some((end, _)) => format!("{}…", &line[..end]),
        None => line.to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeStatus {
    Unmerged,
    Merged,
}

/// Pending file changes take priority over previously integrated branch work.
pub fn worktree_branch_status(
    has_uncommitted_changes: bool,
    has_unmerged_changes: bool,
    has_merged_history: bool,
) -> Option<WorktreeStatus> {
    if has_uncommitted_changes || has_unmerged_changes {
        Some(WorktreeStatus::Unmerged)
    } else if has_merged_history {
        Some(WorktreeStatus::Merged)
    } else {
        None
    }
}

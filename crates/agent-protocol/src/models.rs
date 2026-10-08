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

/// The release train an updater follows. The Host owns the configured channel;
/// clients only request a change through the update RPC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Nightly,
    Preview,
    Stable,
}

/// The process or package an update transaction addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateTarget {
    Host,
    Desktop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateStatus {
    Disabled,
    Idle,
    Checking,
    Available,
    Downloading,
    Downloaded,
    Installing,
    UpToDate,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateErrorContext {
    Check,
    Download,
    Install,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseNoteGroup {
    pub title: String,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub enabled: bool,
    pub status: UpdateStatus,
    pub target: UpdateTarget,
    pub channel: UpdateChannel,
    pub current_version: String,
    pub available_version: Option<String>,
    pub downloaded_version: Option<String>,
    pub release_notes: Vec<ReleaseNoteGroup>,
    pub omitted_release_count: u32,
    pub download_percent: Option<u8>,
    pub checked_at: Option<String>,
    pub message: Option<String>,
    pub error_context: Option<UpdateErrorContext>,
    pub can_retry: bool,
    pub restart_required: bool,
    pub update_url: Option<String>,
    pub download_url: Option<String>,
    pub artifact_name: Option<String>,
}

impl UpdateState {
    pub fn initial(
        target: UpdateTarget,
        current_version: String,
        channel: UpdateChannel,
        enabled: bool,
    ) -> Self {
        Self {
            enabled,
            status: if enabled {
                UpdateStatus::Idle
            } else {
                UpdateStatus::Disabled
            },
            target,
            channel,
            current_version,
            available_version: None,
            downloaded_version: None,
            release_notes: Vec::new(),
            omitted_release_count: 0,
            download_percent: None,
            checked_at: None,
            message: None,
            error_context: None,
            can_retry: false,
            restart_required: false,
            update_url: None,
            download_url: None,
            artifact_name: None,
        }
    }

    /// A check starts without discarding a verified download. A downloaded
    /// package remains installable if the release endpoint is temporarily
    /// unavailable.
    pub fn check_started(&self, checked_at: Option<String>) -> Self {
        Self {
            status: UpdateStatus::Checking,
            checked_at: checked_at.or_else(|| self.checked_at.clone()),
            message: None,
            error_context: None,
            download_percent: self.downloaded_version.as_ref().map(|_| 100),
            can_retry: false,
            ..self.clone()
        }
    }

    pub fn check_failed(&self, message: String, checked_at: Option<String>) -> Self {
        if self.downloaded_version.is_some() {
            return Self {
                status: UpdateStatus::Downloaded,
                checked_at: checked_at.or_else(|| self.checked_at.clone()),
                message: None,
                error_context: None,
                download_percent: Some(100),
                can_retry: true,
                ..self.clone()
            };
        }
        Self {
            status: UpdateStatus::Error,
            checked_at: checked_at.or_else(|| self.checked_at.clone()),
            message: Some(message),
            error_context: Some(UpdateErrorContext::Check),
            download_percent: None,
            can_retry: true,
            ..self.clone()
        }
    }

    pub fn download_started(&self) -> Result<Self, String> {
        if self.available_version.is_none() {
            return Err("cannot download without an available update".into());
        }
        Ok(Self {
            status: UpdateStatus::Downloading,
            download_percent: Some(0),
            message: None,
            error_context: None,
            can_retry: false,
            ..self.clone()
        })
    }

    pub fn download_progress(&self, percent: u8) -> Result<Self, String> {
        if percent > 100 {
            return Err("download progress must be between 0 and 100".into());
        }
        Ok(Self {
            status: UpdateStatus::Downloading,
            download_percent: Some(percent),
            message: None,
            error_context: None,
            ..self.clone()
        })
    }

    pub fn download_finished(&self, version: String) -> Self {
        Self {
            status: UpdateStatus::Downloaded,
            available_version: Some(version.clone()),
            downloaded_version: Some(version),
            download_percent: Some(100),
            message: None,
            error_context: None,
            can_retry: true,
            ..self.clone()
        }
    }

    pub fn download_failed(&self, message: String) -> Self {
        Self {
            status: if self.available_version.is_some() {
                UpdateStatus::Available
            } else {
                UpdateStatus::Error
            },
            message: Some(message),
            error_context: Some(UpdateErrorContext::Download),
            can_retry: self.available_version.is_some(),
            download_percent: None,
            ..self.clone()
        }
    }

    pub fn install_started(&self) -> Result<Self, String> {
        if self.status != UpdateStatus::Downloaded
            || self.downloaded_version.is_none()
            || self.restart_required
        {
            return Err("cannot install without a downloaded update".into());
        }
        Ok(Self {
            status: UpdateStatus::Installing,
            message: None,
            error_context: None,
            can_retry: false,
            ..self.clone()
        })
    }

    pub fn install_finished(&self) -> Self {
        Self {
            status: UpdateStatus::Downloaded,
            restart_required: true,
            message: None,
            error_context: None,
            can_retry: true,
            ..self.clone()
        }
    }

    pub fn install_failed(&self, message: String) -> Self {
        Self {
            status: UpdateStatus::Downloaded,
            message: Some(message),
            error_context: Some(UpdateErrorContext::Install),
            can_retry: true,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatusRequest {
    pub target: UpdateTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCheckRequest {
    pub target: UpdateTarget,
    pub current_version: String,
    pub channel: UpdateChannel,
    pub platform: String,
    pub architecture: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateActionRequest {
    pub target: UpdateTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateChannelRequest {
    pub target: UpdateTarget,
    pub channel: UpdateChannel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NativeUpdatePlatform {
    Android,
    Ios,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeUpdateRequest {
    pub platform: NativeUpdatePlatform,
    pub current_version: String,
    pub channel: UpdateChannel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeUpdateState {
    pub platform: NativeUpdatePlatform,
    pub channel: UpdateChannel,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub store_url: Option<String>,
    pub release_notes: Vec<ReleaseNoteGroup>,
    pub checked_at: Option<String>,
    pub message: Option<String>,
}

#[cfg(test)]
mod update_tests {
    use super::*;

    #[test]
    fn a_download_survives_a_failed_recheck_and_install_requests_restart() {
        let state = UpdateState::initial(
            UpdateTarget::Host,
            "1.0.0-nightly.1".into(),
            UpdateChannel::Nightly,
            true,
        );
        let available = UpdateState {
            available_version: Some("1.0.0-nightly.2".into()),
            status: UpdateStatus::Available,
            ..state
        };
        let downloaded = available.download_finished("1.0.0-nightly.2".into());
        let retained = downloaded.check_failed("offline".into(), Some("now".into()));
        assert_eq!(retained.status, UpdateStatus::Downloaded);
        assert_eq!(
            retained.downloaded_version.as_deref(),
            Some("1.0.0-nightly.2")
        );
        let installing = retained.install_started().unwrap();
        assert_eq!(installing.status, UpdateStatus::Installing);
        assert!(installing.install_finished().restart_required);
    }

    #[test]
    fn download_progress_is_bounded() {
        let state = UpdateState::initial(
            UpdateTarget::Desktop,
            "1.0.0".into(),
            UpdateChannel::Stable,
            true,
        );
        assert!(state.download_progress(101).is_err());
    }
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
    pub project_overrides: std::collections::BTreeMap<String, ProjectConversationSettings>,
}
impl Default for ConversationSettings {
    fn default() -> Self {
        let naming = agent_domain::BranchNaming::default();
        Self {
            auto_settle: AutoSettle::AfterDays(3),
            continue_after_restart: false,
            snooze_limited_threads: false,
            auto_resume_limited_threads: false,
            new_worktrees_start_from_origin: true,
            branch_naming_mode: naming.mode,
            branch_name_prefix: naming.prefix,
            branch_name_instructions: naming.instructions,
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
    pub continue_after_restart: Option<bool>,
    pub new_worktrees_start_from_origin: Option<bool>,
    pub snooze_limited_threads: Option<bool>,
    pub auto_resume_limited_threads: Option<bool>,
    pub branch_naming_mode: Option<agent_domain::BranchNamingMode>,
    pub branch_name_prefix: Option<String>,
    pub branch_name_instructions: Option<String>,
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
            continue_after_restart,
            new_worktrees_start_from_origin,
            snooze_limited_threads,
            auto_resume_limited_threads,
            branch_naming_mode,
            branch_name_prefix,
            branch_name_instructions,
            project_overrides,
        } = patch.clone();
        if let Some(value) = auto_settle {
            next.auto_settle = value;
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

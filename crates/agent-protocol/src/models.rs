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
    /// The Host process' configured working directory. Diff previews can use
    /// this when a stale client checkout path is rejected by the VCS service.
    pub cwd: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderVersionAdvisoryStatus {
    Unknown,
    Current,
    BehindLatest,
}

/// The Host's update capability and version comparison for one provider
/// installation. The Host derives every action from the executable owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderVersionAdvisory {
    pub status: ProviderVersionAdvisoryStatus,
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_command: Option<String>,
    pub can_update: bool,
    pub can_install_version: bool,
    pub message: Option<String>,
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
    pub version_advisory: Option<ProviderVersionAdvisory>,
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

/// Host-owned configuration for one provider instance. The live provider
/// catalogue is still reported by [`ProviderInstance`]; this value contains
/// the settings that determine how the instance is launched and the models
/// the user adds to that catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProviderInstanceConfig {
    pub driver: agent_domain::Driver,
    #[serde(deserialize_with = "trimmed")]
    pub display_name: String,
    #[serde(deserialize_with = "trimmed")]
    pub accent_color: Option<String>,
    pub enabled: bool,
    /// A provider executable override. An absent value uses the Host's
    /// detected executable for the driver.
    #[serde(deserialize_with = "trimmed")]
    pub binary_path: Option<String>,
    /// Provider state/configuration directory override.
    #[serde(deserialize_with = "trimmed")]
    pub home_path: Option<String>,
    /// Environment values owned by this provider instance.
    pub environment: std::collections::BTreeMap<String, String>,
    /// Extra provider CLI arguments appended to the Host's launch arguments.
    pub launch_args: Vec<String>,
    /// Models not returned by the provider's live catalogue.
    pub custom_models: Vec<ProviderCustomModel>,
}

/// Maximum length accepted for a provider instance id.
pub const MAX_PROVIDER_INSTANCE_ID_LENGTH: usize = 64;
/// Maximum length accepted for a user-authored provider model slug.
pub const MAX_PROVIDER_CUSTOM_MODEL_SLUG_LENGTH: usize = 256;

impl Default for ProviderInstanceConfig {
    fn default() -> Self {
        Self {
            driver: agent_domain::Driver::Codex,
            display_name: String::new(),
            accent_color: None,
            enabled: true,
            binary_path: None,
            home_path: None,
            environment: Default::default(),
            launch_args: vec![],
            custom_models: vec![],
        }
    }
}

impl ProviderInstanceConfig {
    /// Validates the launch and catalogue settings for an instance id.
    ///
    /// The Host calls this while validating the complete settings document;
    /// native editors use the same rule through the core provider editor
    /// helpers so a value rejected in the UI cannot differ from a value
    /// rejected when persisted.
    pub fn validate(&self, instance: &str) -> Result<(), String> {
        let raw_instance = instance;
        let instance = raw_instance.trim();
        if raw_instance != instance {
            return Err("provider instance ids must not have surrounding whitespace".into());
        }
        if instance.is_empty()
            || instance.len() > MAX_PROVIDER_INSTANCE_ID_LENGTH
            || !instance.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric() || (index > 0 && (byte == b'_' || byte == b'-'))
            })
        {
            return Err(
                "provider instance ids must start with a letter and use letters, numbers, '-' or '_'".into(),
            );
        }
        if !instance.as_bytes()[0].is_ascii_alphabetic() {
            return Err("provider instance ids must start with a letter".into());
        }
        if (instance == "codex" && self.driver != agent_domain::Driver::Codex)
            || (instance == "claude" && self.driver != agent_domain::Driver::Claude)
        {
            return Err(format!(
                "built-in provider instance {instance} has the wrong driver"
            ));
        }
        if !self.display_name.trim().is_empty() && self.display_name.chars().count() > 128 {
            return Err("provider instance names must be at most 128 characters".into());
        }
        if let Some(color) = &self.accent_color
            && !(color.len() == 7
                && color.starts_with('#')
                && color[1..].bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err("provider accent colors must be #rrggbb".into());
        }
        if self
            .binary_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err("provider executable paths must not be empty".into());
        }
        if self
            .home_path
            .as_deref()
            .is_some_and(|path| path.trim().is_empty())
        {
            return Err("provider home paths must not be empty".into());
        }
        for name in self.environment.keys() {
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().enumerate().all(|(index, byte)| {
                    byte.is_ascii_alphanumeric() || (index > 0 && byte == b'_')
                })
                || !(name.as_bytes()[0].is_ascii_alphabetic() || name.as_bytes()[0] == b'_')
            {
                return Err(format!(
                    "invalid provider environment variable name: {name}"
                ));
            }
        }
        let mut slugs = std::collections::BTreeSet::new();
        for model in &self.custom_models {
            if model.slug.trim().is_empty() || model.name.trim().is_empty() {
                return Err("custom provider models need a slug and name".into());
            }
            if model.slug.chars().count() > MAX_PROVIDER_CUSTOM_MODEL_SLUG_LENGTH {
                return Err(format!(
                    "custom provider model slugs must be {MAX_PROVIDER_CUSTOM_MODEL_SLUG_LENGTH} characters or fewer"
                ));
            }
            if !slugs.insert(model.slug.trim().to_owned()) {
                return Err(format!(
                    "custom provider model {} is duplicated",
                    model.slug
                ));
            }
        }
        Ok(())
    }
}

/// A model descriptor saved with a provider instance. Defaults keep the
/// settings document small while the Host expands it into the wire `Model`
/// catalogue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderCustomModel {
    pub slug: String,
    pub name: String,
    pub aliases: Vec<String>,
    pub badge: Option<String>,
    pub is_default: bool,
    pub is_legacy: bool,
    pub option_descriptors: Vec<agent_domain::OptionDescriptor>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Serialize for ProviderCustomModel {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.name == self.slug
            && self.aliases.is_empty()
            && self.badge.is_none()
            && !self.is_default
            && !self.is_legacy
            && self.option_descriptors.is_empty()
        {
            return serializer.serialize_str(&self.slug);
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Fields<'a> {
            slug: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            name: Option<&'a str>,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            aliases: &'a Vec<String>,
            #[serde(skip_serializing_if = "Option::is_none")]
            badge: &'a Option<String>,
            #[serde(skip_serializing_if = "is_false")]
            is_default: bool,
            #[serde(skip_serializing_if = "is_false")]
            is_legacy: bool,
            #[serde(skip_serializing_if = "Option::is_none")]
            capabilities: Option<ProviderCustomModelCapabilities<'a>>,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ProviderCustomModelCapabilities<'a> {
            option_descriptors: &'a Vec<agent_domain::OptionDescriptor>,
        }
        Fields {
            slug: &self.slug,
            name: (self.name != self.slug).then_some(&self.name),
            aliases: &self.aliases,
            badge: &self.badge,
            is_default: self.is_default,
            is_legacy: self.is_legacy,
            capabilities: (!self.option_descriptors.is_empty()).then_some(
                ProviderCustomModelCapabilities {
                    option_descriptors: &self.option_descriptors,
                },
            ),
        }
        .serialize(serializer)
    }
}

#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ProviderCustomModelFields {
    #[serde(deserialize_with = "trimmed")]
    slug: String,
    #[serde(deserialize_with = "trimmed")]
    name: String,
    aliases: Vec<String>,
    badge: Option<String>,
    is_default: bool,
    is_legacy: bool,
    option_descriptors: Vec<agent_domain::OptionDescriptor>,
    capabilities: Option<ProviderCustomModelCapabilities>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ProviderCustomModelCapabilities {
    option_descriptors: Vec<agent_domain::OptionDescriptor>,
}
impl Default for ProviderCustomModelFields {
    fn default() -> Self {
        Self {
            slug: String::new(),
            name: String::new(),
            aliases: vec![],
            badge: None,
            is_default: false,
            is_legacy: false,
            option_descriptors: vec![],
            capabilities: None,
        }
    }
}

impl<'de> Deserialize<'de> for ProviderCustomModel {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Input {
            Slug(String),
            Fields(ProviderCustomModelFields),
        }
        Ok(match Input::deserialize(deserializer)? {
            Input::Slug(slug) => Self {
                name: slug.clone(),
                slug,
                ..Default::default()
            },
            Input::Fields(fields) => Self {
                name: if fields.name.is_empty() {
                    fields.slug.clone()
                } else {
                    fields.name
                },
                slug: fields.slug,
                aliases: fields.aliases,
                badge: fields.badge,
                is_default: fields.is_default,
                is_legacy: fields.is_legacy,
                option_descriptors: if fields.option_descriptors.is_empty() {
                    fields
                        .capabilities
                        .map(|capabilities| capabilities.option_descriptors)
                        .unwrap_or_default()
                } else {
                    fields.option_descriptors
                },
            },
        })
    }
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

/// A nullable setting's value, where `Null` is a real value: no default
/// model, no dedicated writer model, the device's last merge method. A
/// project override that is absent inherits instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Nullable<T> {
    Null,
    Value(T),
}
impl<T> From<Nullable<T>> for Option<T> {
    fn from(value: Nullable<T>) -> Self {
        match value {
            Nullable::Null => None,
            Nullable::Value(value) => Some(value),
        }
    }
}

/// How assistant text reaches clients while a turn runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseStreamingMode {
    /// The whole message once the turn finishes or pauses.
    Turn,
    /// Each finished paragraph or closed code block.
    #[default]
    Paragraph,
}

/// Where a new thread works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThreadEnvMode {
    Local,
    Worktree,
}

/// Which submodules a new worktree checks out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorktreeSubmodules {
    Recursive,
    TopLevel,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestMergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceControlWritingStyleMode {
    /// Follow the conventions of the repository's recent history.
    #[default]
    RepoConventions,
    ConventionalCommits,
    /// Follow `custom_instructions`.
    Custom,
}

/// How generated commit messages and pull request text are written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SourceControlWritingStyle {
    pub mode: SourceControlWritingStyleMode,
    #[serde(deserialize_with = "trimmed")]
    pub custom_instructions: String,
    /// Fill in the repository's pull request template.
    pub follow_change_request_templates: bool,
}
impl Default for SourceControlWritingStyle {
    fn default() -> Self {
        Self {
            mode: SourceControlWritingStyleMode::default(),
            custom_instructions: String::new(),
            follow_change_request_templates: true,
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SourceControlWritingStylePatch {
    pub mode: Option<SourceControlWritingStyleMode>,
    #[serde(deserialize_with = "trimmed")]
    pub custom_instructions: Option<String>,
    pub follow_change_request_templates: Option<bool>,
}
impl SourceControlWritingStyle {
    fn patched(&self, patch: &SourceControlWritingStylePatch) -> Self {
        Self {
            mode: patch.mode.unwrap_or(self.mode),
            custom_instructions: patch
                .custom_instructions
                .clone()
                .unwrap_or_else(|| self.custom_instructions.clone()),
            follow_change_request_templates: patch
                .follow_change_request_templates
                .unwrap_or(self.follow_change_request_templates),
        }
    }
}

/// The days a stored item is kept before cleanup removes it.
pub const MIN_RETENTION_DAYS: u32 = 1;
pub const MAX_RETENTION_DAYS: u32 = 3650;

/// What the Host's periodic cleanup removes; `None` days keep the item.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StorageCleanup {
    /// Worktrees whose threads have been inactive this long.
    pub worktree_after_days: Option<u32>,
    /// Worktrees whose branch was merged.
    pub worktree_on_merge: bool,
    /// Worktrees whose last thread was deleted.
    pub worktree_on_delete: bool,
    /// Worktrees without any change.
    pub worktree_unchanged: bool,
    pub browser_artifacts_after_days: Option<u32>,
    pub logs_after_days: Option<u32>,
}
impl StorageCleanup {
    /// The worktree rules, which a project's custom cleanup starts from.
    pub fn worktree_rules(&self) -> WorktreeCleanupRules {
        WorktreeCleanupRules {
            worktree_after_days: self.worktree_after_days,
            worktree_on_merge: self.worktree_on_merge,
            worktree_on_delete: self.worktree_on_delete,
            worktree_unchanged: self.worktree_unchanged,
        }
    }
    fn patched(&self, patch: &StorageCleanupPatch) -> Self {
        Self {
            worktree_after_days: nullable(self.worktree_after_days, patch.worktree_after_days),
            worktree_on_merge: patch.worktree_on_merge.unwrap_or(self.worktree_on_merge),
            worktree_on_delete: patch.worktree_on_delete.unwrap_or(self.worktree_on_delete),
            worktree_unchanged: patch.worktree_unchanged.unwrap_or(self.worktree_unchanged),
            browser_artifacts_after_days: nullable(
                self.browser_artifacts_after_days,
                patch.browser_artifacts_after_days,
            ),
            logs_after_days: nullable(self.logs_after_days, patch.logs_after_days),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StorageCleanupPatch {
    pub worktree_after_days: Option<Nullable<u32>>,
    pub worktree_on_merge: Option<bool>,
    pub worktree_on_delete: Option<bool>,
    pub worktree_unchanged: Option<bool>,
    pub browser_artifacts_after_days: Option<Nullable<u32>>,
    pub logs_after_days: Option<Nullable<u32>>,
}

fn nullable<T: Copy>(current: Option<T>, change: Option<Nullable<T>>) -> Option<T> {
    change.map_or(current, Into::into)
}

/// The worktree rules of a cleanup configured apart from the Host's; a
/// stored rule set is complete.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCleanupRules {
    pub worktree_after_days: Option<u32>,
    pub worktree_on_merge: bool,
    pub worktree_on_delete: bool,
    pub worktree_unchanged: bool,
}
impl WorktreeCleanupRules {
    fn patched(&self, patch: &WorktreeCleanupRulesPatch) -> Self {
        Self {
            worktree_after_days: nullable(self.worktree_after_days, patch.worktree_after_days),
            worktree_on_merge: patch.worktree_on_merge.unwrap_or(self.worktree_on_merge),
            worktree_on_delete: patch.worktree_on_delete.unwrap_or(self.worktree_on_delete),
            worktree_unchanged: patch.worktree_unchanged.unwrap_or(self.worktree_unchanged),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WorktreeCleanupRulesPatch {
    pub worktree_after_days: Option<Nullable<u32>>,
    pub worktree_on_merge: Option<bool>,
    pub worktree_on_delete: Option<bool>,
    pub worktree_unchanged: Option<bool>,
}
/// Worktree cleanup configured apart from `StorageCleanup`'s worktree rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeCleanup {
    Off,
    Custom { rules: WorktreeCleanupRules },
}
/// Custom rules left out keep their current value, else start from the
/// Host's worktree rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WorktreeCleanupPatch {
    Off,
    Custom { rules: WorktreeCleanupRulesPatch },
}

/// A preset balance between fresh information and background work.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundActivityProfile {
    #[default]
    Balanced,
    Performance,
    BatterySaver,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundActivityProfileSelection {
    #[default]
    Balanced,
    Performance,
    BatterySaver,
    /// `BackgroundActivity::overrides` over `base_profile`.
    Custom,
}
/// The intervals a custom selection sets, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackgroundActivityOverrides {
    pub automatic_git_fetch_interval_ms: Option<u64>,
    pub provider_health_refresh_interval_ms: Option<u64>,
}
/// How often the Host fetches remotes and checks providers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackgroundActivity {
    pub profile: BackgroundActivityProfileSelection,
    /// The profile a custom selection started from.
    pub base_profile: Option<BackgroundActivityProfile>,
    pub overrides: BackgroundActivityOverrides,
}
/// The intervals a background-activity selection resolves to, in
/// milliseconds; a zero interval turns the work off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedBackgroundActivity {
    pub profile: BackgroundActivityProfile,
    pub automatic_git_fetch_interval_ms: u64,
    pub provider_health_refresh_interval_ms: u64,
}
impl BackgroundActivityProfile {
    const ALL: [Self; 3] = [Self::Balanced, Self::Performance, Self::BatterySaver];
    pub fn preset(self) -> ResolvedBackgroundActivity {
        const SECOND: u64 = 1_000;
        const MINUTE: u64 = 60 * SECOND;
        let (fetch, health) = match self {
            Self::Performance => (15 * SECOND, MINUTE),
            Self::Balanced => (30 * SECOND, 5 * MINUTE),
            Self::BatterySaver => (0, 15 * MINUTE),
        };
        ResolvedBackgroundActivity {
            profile: self,
            automatic_git_fetch_interval_ms: fetch,
            provider_health_refresh_interval_ms: health,
        }
    }
}
impl From<BackgroundActivityProfile> for BackgroundActivityProfileSelection {
    fn from(profile: BackgroundActivityProfile) -> Self {
        match profile {
            BackgroundActivityProfile::Balanced => Self::Balanced,
            BackgroundActivityProfile::Performance => Self::Performance,
            BackgroundActivityProfile::BatterySaver => Self::BatterySaver,
        }
    }
}
impl BackgroundActivity {
    /// The preset the selection starts from.
    pub fn base_profile(&self) -> BackgroundActivityProfile {
        match self.profile {
            BackgroundActivityProfileSelection::Balanced => BackgroundActivityProfile::Balanced,
            BackgroundActivityProfileSelection::Performance => {
                BackgroundActivityProfile::Performance
            }
            BackgroundActivityProfileSelection::BatterySaver => {
                BackgroundActivityProfile::BatterySaver
            }
            BackgroundActivityProfileSelection::Custom => self.base_profile.unwrap_or_default(),
        }
    }
    pub fn resolved(&self) -> ResolvedBackgroundActivity {
        let preset = self.base_profile().preset();
        if self.profile != BackgroundActivityProfileSelection::Custom {
            return preset;
        }
        ResolvedBackgroundActivity {
            automatic_git_fetch_interval_ms: self
                .overrides
                .automatic_git_fetch_interval_ms
                .unwrap_or(preset.automatic_git_fetch_interval_ms),
            provider_health_refresh_interval_ms: self
                .overrides
                .provider_health_refresh_interval_ms
                .unwrap_or(preset.provider_health_refresh_interval_ms),
            ..preset
        }
    }
    /// A preset carries no overrides, and a custom selection whose intervals
    /// equal a preset's becomes that preset, trying its base profile first.
    fn normalized(&self) -> Self {
        let resolved = self.resolved();
        let base = self.base_profile();
        let preset = |profile: BackgroundActivityProfile| Self {
            profile: profile.into(),
            base_profile: None,
            overrides: BackgroundActivityOverrides::default(),
        };
        if self.profile != BackgroundActivityProfileSelection::Custom {
            return preset(base);
        }
        let same_intervals = |profile: &BackgroundActivityProfile| {
            ResolvedBackgroundActivity {
                profile: *profile,
                ..resolved
            } == profile.preset()
        };
        if let Some(profile) = std::iter::once(base)
            .chain(BackgroundActivityProfile::ALL)
            .find(same_intervals)
        {
            return preset(profile);
        }
        let differs = |value: u64, preset: u64| (value != preset).then_some(value);
        Self {
            profile: BackgroundActivityProfileSelection::Custom,
            base_profile: Some(base),
            overrides: BackgroundActivityOverrides {
                automatic_git_fetch_interval_ms: differs(
                    resolved.automatic_git_fetch_interval_ms,
                    base.preset().automatic_git_fetch_interval_ms,
                ),
                provider_health_refresh_interval_ms: differs(
                    resolved.provider_health_refresh_interval_ms,
                    base.preset().provider_health_refresh_interval_ms,
                ),
            },
        }
    }
    fn patched(&self, patch: &BackgroundActivityPatch) -> Self {
        Self {
            profile: patch.profile.unwrap_or(self.profile),
            base_profile: patch.base_profile.or(self.base_profile),
            overrides: patch.overrides.unwrap_or(self.overrides),
        }
        .normalized()
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BackgroundActivityPatch {
    pub profile: Option<BackgroundActivityProfileSelection>,
    pub base_profile: Option<BackgroundActivityProfile>,
    /// Replaces every override.
    pub overrides: Option<BackgroundActivityOverrides>,
}

pub const MIN_BROWSER_VIEWPORT_DIMENSION: u32 = 240;
pub const MAX_BROWSER_VIEWPORT_DIMENSION: u32 = 3840;
pub const MAX_BROWSER_VIEWPORT_AREA: u32 = 3840 * 2160;
/// The zoom steps the browser's zoom controls step through, in percent.
pub const BROWSER_ZOOM_PERCENTS: [u32; 17] = [
    25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300, 400, 500,
];
pub const BROWSER_VIEWPORT_PRESETS: [&str; 17] = [
    "iphone-se",
    "iphone-xr",
    "iphone-12-pro",
    "iphone-14-pro-max",
    "pixel-7",
    "samsung-galaxy-s8-plus",
    "samsung-galaxy-s20-ultra",
    "ipad-mini",
    "ipad-air",
    "ipad-pro",
    "surface-pro-7",
    "surface-duo",
    "galaxy-z-fold-5",
    "asus-zenbook-fold",
    "samsung-galaxy-a51-71",
    "nest-hub",
    "nest-hub-max",
];
/// The size a browser tab opens with when nothing names one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BrowserViewport {
    /// The panel's size.
    #[default]
    Fill,
    Freeform {
        width: u32,
        height: u32,
    },
    Preset {
        width: u32,
        height: u32,
        preset_id: String,
    },
}
/// The color scheme browser pages see.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserAppearance {
    #[default]
    System,
    Light,
    Dark,
}
/// Where a clicked link opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserLinkTarget {
    /// The OS default browser.
    #[default]
    System,
    /// A browser tab beside the thread.
    App,
}
/// Defaults for browser tabs opened without an explicit viewport, zoom or
/// appearance, by the user or by an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserDefaults {
    pub viewport: BrowserViewport,
    /// One of `BROWSER_ZOOM_PERCENTS`.
    pub zoom_percent: u32,
    pub appearance: BrowserAppearance,
    pub link_target: BrowserLinkTarget,
    /// Show the floating preview when an agent opens a page.
    pub auto_show_floating_preview: bool,
}
impl Default for BrowserDefaults {
    fn default() -> Self {
        Self {
            viewport: BrowserViewport::Fill,
            zoom_percent: 100,
            appearance: BrowserAppearance::System,
            link_target: BrowserLinkTarget::System,
            auto_show_floating_preview: true,
        }
    }
}
impl BrowserDefaults {
    fn patched(&self, patch: &BrowserDefaultsPatch) -> Self {
        Self {
            viewport: patch
                .viewport
                .clone()
                .unwrap_or_else(|| self.viewport.clone()),
            zoom_percent: patch.zoom_percent.unwrap_or(self.zoom_percent),
            appearance: patch.appearance.unwrap_or(self.appearance),
            link_target: patch.link_target.unwrap_or(self.link_target),
            auto_show_floating_preview: patch
                .auto_show_floating_preview
                .unwrap_or(self.auto_show_floating_preview),
        }
    }
    fn validate(&self) -> Result<(), String> {
        if !BROWSER_ZOOM_PERCENTS.contains(&self.zoom_percent) {
            return Err(format!("{}% is not a browser zoom step", self.zoom_percent));
        }
        let (width, height) = match &self.viewport {
            BrowserViewport::Fill => return Ok(()),
            BrowserViewport::Freeform { width, height } => (*width, *height),
            BrowserViewport::Preset {
                width,
                height,
                preset_id,
            } => {
                if !BROWSER_VIEWPORT_PRESETS.contains(&preset_id.as_str()) {
                    return Err(format!("{preset_id} is not a browser viewport preset"));
                }
                (*width, *height)
            }
        };
        let range = MIN_BROWSER_VIEWPORT_DIMENSION..=MAX_BROWSER_VIEWPORT_DIMENSION;
        if !range.contains(&width) || !range.contains(&height) {
            return Err(format!(
                "browser viewport sides need {MIN_BROWSER_VIEWPORT_DIMENSION} to {MAX_BROWSER_VIEWPORT_DIMENSION} pixels"
            ));
        }
        if width.saturating_mul(height) > MAX_BROWSER_VIEWPORT_AREA {
            return Err(format!(
                "browser viewport area must not exceed {MAX_BROWSER_VIEWPORT_AREA} pixels"
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BrowserDefaultsPatch {
    pub viewport: Option<BrowserViewport>,
    pub zoom_percent: Option<u32>,
    pub appearance: Option<BrowserAppearance>,
    pub link_target: Option<BrowserLinkTarget>,
    pub auto_show_floating_preview: Option<bool>,
}

/// A text setting, trimmed as it is read.
trait Trim {
    fn trimmed(self) -> Self;
}
impl Trim for String {
    fn trimmed(self) -> Self {
        self.trim().to_owned()
    }
}
impl<T: Trim> Trim for Option<T> {
    fn trimmed(self) -> Self {
        self.map(Trim::trimmed)
    }
}
impl<T: Trim> Trim for OverrideChange<T> {
    fn trimmed(self) -> Self {
        match self {
            Self::Inherit => Self::Inherit,
            Self::Value(value) => Self::Value(value.trimmed()),
        }
    }
}
fn trimmed<'de, D: serde::Deserializer<'de>, T: Trim + Deserialize<'de>>(
    deserializer: D,
) -> Result<T, D::Error> {
    T::deserialize(deserializer).map(Trim::trimmed)
}

/// One sparse project override edit, including an explicit return to inheritance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OverrideChange<T> {
    Inherit,
    Value(T),
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

/// The Host settings a project can override, as one project's overrides and
/// as the patch editing them. An absent override inherits the Host's value.
macro_rules! project_overrides {
    ($($(#[$meta:meta])* $field:ident: $value:ty),* $(,)?) => {
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(default, rename_all = "camelCase")]
        pub struct ProjectSettingsOverrides {
            $($(#[$meta])* pub $field: Option<$value>,)*
        }
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(default, rename_all = "camelCase")]
        pub struct ProjectSettingsOverridesPatch {
            $($(#[$meta])* pub $field: Option<OverrideChange<$value>>,)*
        }
        impl ProjectSettingsOverrides {
            fn patched(&self, patch: &ProjectSettingsOverridesPatch) -> Self {
                Self { $($field: patched_override(&self.$field, &patch.$field),)* }
            }
        }
    };
}
project_overrides! {
    worktree_cleanup: WorktreeCleanup,
    default_model_selection: Nullable<agent_domain::ModelSelection>,
    default_runtime_mode: agent_domain::RuntimeMode,
    default_thread_env_mode: ThreadEnvMode,
    new_worktrees_start_from_origin: bool,
    worktree_submodules: WorktreeSubmodules,
    default_auto_pull: bool,
    enable_agent_browser_access: bool,
    text_generation_model_selection: agent_domain::ModelSelection,
    source_control_writer_model_selection: Nullable<agent_domain::ModelSelection>,
    source_control_writing_style: SourceControlWritingStyle,
    branch_naming_mode: agent_domain::BranchNamingMode,
    #[serde(deserialize_with = "trimmed")]
    branch_name_prefix: String,
    #[serde(deserialize_with = "trimmed")]
    branch_name_instructions: String,
    pull_request_merge_method: Nullable<PullRequestMergeMethod>,
    auto_settle_on_merge: bool,
    auto_settle: AutoSettle,
    continue_after_restart: bool,
    response_streaming_mode: ResponseStreamingMode,
}

/// The Host's settings document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HostSettings {
    /// Worktree cleanup apart from `storage_cleanup`'s worktree rules;
    /// `None` follows them.
    pub worktree_cleanup: Option<WorktreeCleanup>,
    pub storage_cleanup: StorageCleanup,
    pub response_streaming_mode: ResponseStreamingMode,
    /// Notify when a provider CLI has an update.
    pub enable_provider_update_checks: bool,
    /// Continue turns a Host restart cut.
    pub continue_after_restart: bool,
    /// Agents may drive the browser.
    pub enable_agent_browser_access: bool,
    /// Pull the checkout before a new thread starts.
    pub default_auto_pull: bool,
    pub default_model_selection: Option<agent_domain::ModelSelection>,
    pub default_runtime_mode: agent_domain::RuntimeMode,
    pub auto_settle: AutoSettle,
    /// Settle a thread once its pull request is merged.
    pub auto_settle_on_merge: bool,
    pub snooze_limited_threads: bool,
    pub auto_resume_limited_threads: bool,
    pub background_activity: BackgroundActivity,
    /// `None` follows the repository's configuration, then the checkout.
    pub default_thread_env_mode: Option<ThreadEnvMode>,
    /// A new worktree starts from the latest matching branch on origin.
    pub new_worktrees_start_from_origin: bool,
    /// `None` follows the repository's configuration, then recursive.
    pub worktree_submodules: Option<WorktreeSubmodules>,
    /// Where the add-project folder browser starts; empty for the home
    /// directory.
    #[serde(deserialize_with = "trimmed")]
    pub add_project_base_directory: String,
    /// The model that names threads and branches; `None` for the Host's
    /// built-in choice.
    pub text_generation_model_selection: Option<agent_domain::ModelSelection>,
    /// How launches name the worktree branches they generate.
    pub branch_naming_mode: agent_domain::BranchNamingMode,
    #[serde(deserialize_with = "trimmed")]
    pub branch_name_prefix: String,
    #[serde(deserialize_with = "trimmed")]
    pub branch_name_instructions: String,
    pub source_control_writing_style: SourceControlWritingStyle,
    /// The model that writes commit messages and pull request text; `None`
    /// for the text-generation model.
    pub source_control_writer_model_selection: Option<agent_domain::ModelSelection>,
    /// The merge method pull requests start with; `None` reuses the method
    /// last chosen on the device.
    pub pull_request_merge_method: Option<PullRequestMergeMethod>,
    pub browser: BrowserDefaults,
    /// Configured provider instances, keyed by the instance id used in model
    /// selections and runtime launches. The built-in instances are used when
    /// a key is absent; entries here override their launch and catalogue
    /// settings or add another instance for the same driver.
    pub provider_instances: std::collections::BTreeMap<String, ProviderInstanceConfig>,
    pub project_overrides: std::collections::BTreeMap<String, ProjectSettingsOverrides>,
}
impl Default for HostSettings {
    fn default() -> Self {
        let naming = agent_domain::BranchNaming::default();
        Self {
            worktree_cleanup: None,
            storage_cleanup: StorageCleanup::default(),
            response_streaming_mode: ResponseStreamingMode::default(),
            enable_provider_update_checks: true,
            continue_after_restart: false,
            enable_agent_browser_access: true,
            default_auto_pull: false,
            default_model_selection: None,
            default_runtime_mode: agent_domain::RuntimeMode::FullAccess,
            auto_settle: AutoSettle::AfterDays(3),
            auto_settle_on_merge: true,
            snooze_limited_threads: false,
            auto_resume_limited_threads: false,
            background_activity: BackgroundActivity::default(),
            default_thread_env_mode: None,
            new_worktrees_start_from_origin: true,
            worktree_submodules: None,
            add_project_base_directory: String::new(),
            text_generation_model_selection: None,
            branch_naming_mode: naming.mode,
            branch_name_prefix: naming.prefix,
            branch_name_instructions: naming.instructions,
            source_control_writing_style: SourceControlWritingStyle::default(),
            source_control_writer_model_selection: None,
            pull_request_merge_method: None,
            browser: BrowserDefaults::default(),
            provider_instances: Default::default(),
            project_overrides: Default::default(),
        }
    }
}
/// `host/settings/update`: the settings to change. The Host merges it into
/// its current settings and keeps every field it leaves out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct HostSettingsPatch {
    pub worktree_cleanup: Option<Nullable<WorktreeCleanupPatch>>,
    pub storage_cleanup: Option<StorageCleanupPatch>,
    pub response_streaming_mode: Option<ResponseStreamingMode>,
    pub enable_provider_update_checks: Option<bool>,
    pub continue_after_restart: Option<bool>,
    pub enable_agent_browser_access: Option<bool>,
    pub default_auto_pull: Option<bool>,
    pub default_model_selection: Option<Nullable<agent_domain::ModelSelection>>,
    pub default_runtime_mode: Option<agent_domain::RuntimeMode>,
    pub auto_settle: Option<AutoSettle>,
    pub auto_settle_on_merge: Option<bool>,
    pub snooze_limited_threads: Option<bool>,
    pub auto_resume_limited_threads: Option<bool>,
    pub background_activity: Option<BackgroundActivityPatch>,
    pub default_thread_env_mode: Option<Nullable<ThreadEnvMode>>,
    pub new_worktrees_start_from_origin: Option<bool>,
    pub worktree_submodules: Option<Nullable<WorktreeSubmodules>>,
    #[serde(deserialize_with = "trimmed")]
    pub add_project_base_directory: Option<String>,
    pub text_generation_model_selection: Option<Nullable<agent_domain::ModelSelection>>,
    pub branch_naming_mode: Option<agent_domain::BranchNamingMode>,
    #[serde(deserialize_with = "trimmed")]
    pub branch_name_prefix: Option<String>,
    #[serde(deserialize_with = "trimmed")]
    pub branch_name_instructions: Option<String>,
    pub source_control_writing_style: Option<SourceControlWritingStylePatch>,
    pub source_control_writer_model_selection: Option<Nullable<agent_domain::ModelSelection>>,
    pub pull_request_merge_method: Option<Nullable<PullRequestMergeMethod>>,
    pub browser: Option<BrowserDefaultsPatch>,
    /// Replaces the configured provider instance map. The Host validates the
    /// complete map before persisting it so removing an instance cannot leave
    /// a half-updated launch configuration.
    pub provider_instances: Option<std::collections::BTreeMap<String, ProviderInstanceConfig>>,
    /// Each entry edits only the supplied project fields; `None` removes all overrides.
    pub project_overrides:
        std::collections::BTreeMap<String, Option<ProjectSettingsOverridesPatch>>,
}

fn set<T>(target: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *target = value;
    }
}
fn set_nullable<T>(target: &mut Option<T>, value: Option<Nullable<T>>) {
    if let Some(value) = value {
        *target = value.into();
    }
}

impl HostSettings {
    /// These settings with the patch's fields replaced.
    pub fn patched(&self, patch: &HostSettingsPatch) -> Self {
        let mut next = self.clone();
        let HostSettingsPatch {
            worktree_cleanup,
            storage_cleanup,
            response_streaming_mode,
            enable_provider_update_checks,
            continue_after_restart,
            enable_agent_browser_access,
            default_auto_pull,
            default_model_selection,
            default_runtime_mode,
            auto_settle,
            auto_settle_on_merge,
            snooze_limited_threads,
            auto_resume_limited_threads,
            background_activity,
            default_thread_env_mode,
            new_worktrees_start_from_origin,
            worktree_submodules,
            add_project_base_directory,
            text_generation_model_selection,
            branch_naming_mode,
            branch_name_prefix,
            branch_name_instructions,
            source_control_writing_style,
            source_control_writer_model_selection,
            pull_request_merge_method,
            browser,
            provider_instances,
            project_overrides,
        } = patch.clone();
        if let Some(patch) = storage_cleanup {
            next.storage_cleanup = next.storage_cleanup.patched(&patch);
        }
        if let Some(cleanup) = worktree_cleanup {
            next.worktree_cleanup = match cleanup {
                Nullable::Null => None,
                Nullable::Value(WorktreeCleanupPatch::Off) => Some(WorktreeCleanup::Off),
                Nullable::Value(WorktreeCleanupPatch::Custom { rules }) => {
                    let current = match &self.worktree_cleanup {
                        Some(WorktreeCleanup::Custom { rules }) => rules.clone(),
                        _ => next.storage_cleanup.worktree_rules(),
                    };
                    Some(WorktreeCleanup::Custom {
                        rules: current.patched(&rules),
                    })
                }
            };
        }
        set(&mut next.response_streaming_mode, response_streaming_mode);
        set(
            &mut next.enable_provider_update_checks,
            enable_provider_update_checks,
        );
        set(&mut next.continue_after_restart, continue_after_restart);
        set(
            &mut next.enable_agent_browser_access,
            enable_agent_browser_access,
        );
        set(&mut next.default_auto_pull, default_auto_pull);
        set_nullable(&mut next.default_model_selection, default_model_selection);
        set(&mut next.default_runtime_mode, default_runtime_mode);
        set(&mut next.auto_settle, auto_settle);
        set(&mut next.auto_settle_on_merge, auto_settle_on_merge);
        set(&mut next.snooze_limited_threads, snooze_limited_threads);
        set(
            &mut next.auto_resume_limited_threads,
            auto_resume_limited_threads,
        );
        if let Some(patch) = background_activity {
            next.background_activity = next.background_activity.patched(&patch);
        }
        set_nullable(&mut next.default_thread_env_mode, default_thread_env_mode);
        set(
            &mut next.new_worktrees_start_from_origin,
            new_worktrees_start_from_origin,
        );
        set_nullable(&mut next.worktree_submodules, worktree_submodules);
        set(
            &mut next.add_project_base_directory,
            add_project_base_directory,
        );
        set_nullable(
            &mut next.text_generation_model_selection,
            text_generation_model_selection,
        );
        set(&mut next.branch_naming_mode, branch_naming_mode);
        set(&mut next.branch_name_prefix, branch_name_prefix);
        set(&mut next.branch_name_instructions, branch_name_instructions);
        if let Some(patch) = source_control_writing_style {
            next.source_control_writing_style = next.source_control_writing_style.patched(&patch);
        }
        set_nullable(
            &mut next.source_control_writer_model_selection,
            source_control_writer_model_selection,
        );
        set_nullable(
            &mut next.pull_request_merge_method,
            pull_request_merge_method,
        );
        if let Some(patch) = browser {
            next.browser = next.browser.patched(&patch);
        }
        set(&mut next.provider_instances, provider_instances);
        for (project, overrides) in project_overrides {
            match overrides {
                Some(patch) => {
                    let overrides = next
                        .project_overrides
                        .get(&project)
                        .cloned()
                        .unwrap_or_default()
                        .patched(&patch);
                    if overrides == ProjectSettingsOverrides::default() {
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
        let retention = |days: Option<u32>| match days {
            Some(days) if !(MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS).contains(&days) => Err(
                format!("retention needs {MIN_RETENTION_DAYS} to {MAX_RETENTION_DAYS} days"),
            ),
            _ => Ok(()),
        };
        let cleanup = |cleanup: Option<&WorktreeCleanup>| match cleanup {
            Some(WorktreeCleanup::Custom { rules }) => retention(rules.worktree_after_days),
            _ => Ok(()),
        };
        days(&self.auto_settle)?;
        retention(self.storage_cleanup.worktree_after_days)?;
        retention(self.storage_cleanup.browser_artifacts_after_days)?;
        retention(self.storage_cleanup.logs_after_days)?;
        cleanup(self.worktree_cleanup.as_ref())?;
        self.browser.validate()?;
        for (instance, config) in &self.provider_instances {
            config.validate(instance)?;
        }
        self.project_overrides.values().try_for_each(|project| {
            project.auto_settle.as_ref().map_or(Ok(()), days)?;
            cleanup(project.worktree_cleanup.as_ref())
        })
    }
}

#[cfg(test)]
mod settings_tests {
    use super::*;
    use serde_json::json;

    fn decode(value: serde_json::Value) -> HostSettings {
        serde_json::from_value(value).unwrap()
    }
    fn decode_patch(value: serde_json::Value) -> HostSettingsPatch {
        serde_json::from_value(value).unwrap()
    }
    fn rejects(value: serde_json::Value) {
        assert!(
            serde_json::from_value::<HostSettings>(value.clone()).is_err(),
            "{value}"
        );
        assert!(
            serde_json::from_value::<HostSettingsPatch>(value.clone()).is_err(),
            "{value}"
        );
    }
    fn round_trips(value: serde_json::Value) {
        let settings = decode(value);
        let encoded = serde_json::to_value(&settings).unwrap();
        assert_eq!(decode(encoded), settings);
    }

    // contracts settings.test.ts "ServerSettings response streaming".
    #[test]
    fn response_streaming_defaults_to_paragraph_and_round_trips_as_a_project_override() {
        assert_eq!(
            HostSettings::default().response_streaming_mode,
            ResponseStreamingMode::Paragraph
        );
        for mode in ["turn", "paragraph"] {
            let input = json!({
                "responseStreamingMode": mode,
                "projectOverrides": { "project": { "responseStreamingMode": mode } },
            });
            round_trips(input.clone());
            let settings = decode(input.clone());
            assert_eq!(
                settings.project_overrides["project"].response_streaming_mode,
                Some(settings.response_streaming_mode)
            );
            assert_eq!(
                decode_patch(json!({ "responseStreamingMode": mode })).response_streaming_mode,
                Some(settings.response_streaming_mode)
            );
        }
        for mode in ["token", "unsupported"] {
            rejects(json!({ "responseStreamingMode": mode }));
            rejects(json!({
                "projectOverrides": { "project": { "responseStreamingMode": mode } },
            }));
        }
    }

    // contracts settings.test.ts "storage cleanup settings".
    #[test]
    fn storage_cleanup_stays_off_until_configured() {
        let settings = decode(json!({}));
        assert_eq!(settings.worktree_cleanup, None);
        assert_eq!(
            settings.storage_cleanup,
            StorageCleanup {
                worktree_after_days: None,
                worktree_on_merge: false,
                worktree_on_delete: false,
                worktree_unchanged: false,
                browser_artifacts_after_days: None,
                logs_after_days: None,
            }
        );
    }

    #[test]
    fn a_storage_cleanup_patch_changes_one_rule_without_resetting_others() {
        let settings = HostSettings::default().patched(&HostSettingsPatch {
            storage_cleanup: Some(StorageCleanupPatch {
                worktree_on_merge: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        });
        let eight = settings.patched(&HostSettingsPatch {
            storage_cleanup: Some(StorageCleanupPatch {
                worktree_after_days: Some(Nullable::Value(8)),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(eight.storage_cleanup.worktree_after_days, Some(8));
        assert!(eight.storage_cleanup.worktree_on_merge);
        let never = eight.patched(&HostSettingsPatch {
            storage_cleanup: Some(StorageCleanupPatch {
                worktree_after_days: Some(Nullable::Null),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(never.storage_cleanup.worktree_after_days, None);
        assert!(never.storage_cleanup.worktree_on_merge);
    }

    #[test]
    fn a_custom_worktree_cleanup_starts_from_the_host_rules_and_stored_rules_are_complete() {
        let host = HostSettings::default().patched(&HostSettingsPatch {
            storage_cleanup: Some(StorageCleanupPatch {
                worktree_on_merge: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        });
        let custom = |rules| HostSettingsPatch {
            worktree_cleanup: Some(Nullable::Value(WorktreeCleanupPatch::Custom { rules })),
            ..Default::default()
        };
        let eight = host.patched(&custom(WorktreeCleanupRulesPatch {
            worktree_after_days: Some(Nullable::Value(8)),
            ..Default::default()
        }));
        assert_eq!(
            eight.worktree_cleanup,
            Some(WorktreeCleanup::Custom {
                rules: WorktreeCleanupRules {
                    worktree_after_days: Some(8),
                    worktree_on_merge: true,
                    worktree_on_delete: false,
                    worktree_unchanged: false,
                }
            })
        );
        let deleted = eight.patched(&custom(WorktreeCleanupRulesPatch {
            worktree_on_delete: Some(true),
            ..Default::default()
        }));
        assert_eq!(
            deleted.worktree_cleanup,
            Some(WorktreeCleanup::Custom {
                rules: WorktreeCleanupRules {
                    worktree_after_days: Some(8),
                    worktree_on_merge: true,
                    worktree_on_delete: true,
                    worktree_unchanged: false,
                }
            })
        );
        let off = deleted.patched(&HostSettingsPatch {
            worktree_cleanup: Some(Nullable::Value(WorktreeCleanupPatch::Off)),
            ..Default::default()
        });
        assert_eq!(off.worktree_cleanup, Some(WorktreeCleanup::Off));
        let following = off.patched(&HostSettingsPatch {
            worktree_cleanup: Some(Nullable::Null),
            ..Default::default()
        });
        assert_eq!(following.worktree_cleanup, None);
        assert!(
            serde_json::from_value::<HostSettings>(json!({
                "projectOverrides": {
                    "project": { "worktreeCleanup": { "custom": { "rules": { "worktreeAfterDays": 8 } } } },
                },
            }))
            .is_err()
        );
    }

    #[test]
    fn retention_outside_one_to_3650_days_is_rejected() {
        for days in [0, 3651] {
            let settings = HostSettings {
                storage_cleanup: StorageCleanup {
                    browser_artifacts_after_days: Some(days),
                    ..Default::default()
                },
                ..Default::default()
            };
            assert!(settings.validate().is_err(), "{days}");
        }
        for days in [1, 3650] {
            let settings = HostSettings {
                storage_cleanup: StorageCleanup {
                    browser_artifacts_after_days: Some(days),
                    ..Default::default()
                },
                ..Default::default()
            };
            assert!(settings.validate().is_ok(), "{days}");
        }
        for days in [json!(-1), json!(1.5)] {
            rejects(json!({ "storageCleanup": { "browserArtifactsAfterDays": days } }));
        }
    }

    // contracts settings.test.ts "ServerSettings thread settlement".
    #[test]
    fn thread_settlement_defaults_to_merge_and_three_days_and_both_can_be_off() {
        let settings = decode(json!({}));
        assert_eq!(settings.auto_settle, AutoSettle::AfterDays(3));
        assert!(settings.auto_settle_on_merge);
        let off = settings.patched(&HostSettingsPatch {
            auto_settle: Some(AutoSettle::Never),
            auto_settle_on_merge: Some(false),
            ..Default::default()
        });
        assert_eq!(off.auto_settle, AutoSettle::Never);
        assert!(!off.auto_settle_on_merge);
        for days in [0, 91] {
            let host = HostSettings {
                auto_settle: AutoSettle::AfterDays(days),
                ..Default::default()
            };
            assert!(host.validate().is_err(), "{days}");
            let mut project = HostSettings::default();
            project.project_overrides.insert(
                "project".into(),
                ProjectSettingsOverrides {
                    auto_settle: Some(AutoSettle::AfterDays(days)),
                    ..Default::default()
                },
            );
            assert!(project.validate().is_err(), "{days}");
        }
    }

    // contracts settings.test.ts "ServerSettings worktree defaults".
    #[test]
    fn worktree_defaults_inherit_until_set() {
        assert_eq!(decode(json!({})).default_thread_env_mode, None);
        assert_eq!(
            decode(json!({ "defaultThreadEnvMode": "worktree" })).default_thread_env_mode,
            Some(ThreadEnvMode::Worktree)
        );
        assert_eq!(decode(json!({})).worktree_submodules, None);
        assert_eq!(
            decode(json!({ "worktreeSubmodules": "top-level" })).worktree_submodules,
            Some(WorktreeSubmodules::TopLevel)
        );
        let set = HostSettings::default().patched(&HostSettingsPatch {
            default_thread_env_mode: Some(Nullable::Value(ThreadEnvMode::Worktree)),
            worktree_submodules: Some(Nullable::Value(WorktreeSubmodules::None)),
            ..Default::default()
        });
        assert_eq!(set.default_thread_env_mode, Some(ThreadEnvMode::Worktree));
        assert_eq!(set.worktree_submodules, Some(WorktreeSubmodules::None));
        let inherited = set.patched(&HostSettingsPatch {
            default_thread_env_mode: Some(Nullable::Null),
            worktree_submodules: Some(Nullable::Null),
            ..Default::default()
        });
        assert_eq!(inherited.default_thread_env_mode, None);
        assert_eq!(inherited.worktree_submodules, None);
    }

    // contracts settings.test.ts "ServerSettings.sourceControlWritingStyle".
    #[test]
    fn source_control_writing_style_defaults_and_trims_partial_updates() {
        let settings = decode(json!({}));
        assert_eq!(
            settings.source_control_writing_style,
            SourceControlWritingStyle {
                mode: SourceControlWritingStyleMode::RepoConventions,
                custom_instructions: String::new(),
                follow_change_request_templates: true,
            }
        );
        assert_eq!(settings.source_control_writer_model_selection, None);
        let patch = decode_patch(json!({
            "sourceControlWritingStyle": {
                "mode": "custom",
                "customInstructions": "  Prefer concise wording.  ",
            },
        }));
        assert_eq!(
            patch.source_control_writing_style,
            Some(SourceControlWritingStylePatch {
                mode: Some(SourceControlWritingStyleMode::Custom),
                custom_instructions: Some("Prefer concise wording.".into()),
                follow_change_request_templates: None,
            })
        );
        let patched = settings.patched(&patch);
        assert_eq!(
            patched.source_control_writing_style,
            SourceControlWritingStyle {
                mode: SourceControlWritingStyleMode::Custom,
                custom_instructions: "Prefer concise wording.".into(),
                follow_change_request_templates: true,
            }
        );
    }

    // contracts settings.test.ts "ServerSettingsPatch string normalization".
    #[test]
    fn text_settings_are_trimmed_as_they_are_read() {
        let patch = decode_patch(json!({
            "addProjectBaseDirectory": "  ~/Development  ",
            "branchNamePrefix": " team/ ",
            "projectOverrides": {
                "project": { "branchNameInstructions": { "value": "  Include the issue ID.  " } },
            },
        }));
        assert_eq!(
            patch.add_project_base_directory.as_deref(),
            Some("~/Development")
        );
        assert_eq!(patch.branch_name_prefix.as_deref(), Some("team/"));
        assert_eq!(
            patch.project_overrides["project"]
                .as_ref()
                .unwrap()
                .branch_name_instructions,
            Some(OverrideChange::Value("Include the issue ID.".into()))
        );
        let settings = decode(json!({
            "addProjectBaseDirectory": "  ~/Development  ",
            "sourceControlWritingStyle": { "customInstructions": " Be brief. " },
            "projectOverrides": { "project": { "branchNamePrefix": " team/ " } },
        }));
        assert_eq!(settings.add_project_base_directory, "~/Development");
        assert_eq!(
            settings.source_control_writing_style.custom_instructions,
            "Be brief."
        );
        assert_eq!(
            settings.project_overrides["project"]
                .branch_name_prefix
                .as_deref(),
            Some("team/")
        );
    }

    // contracts settings.test.ts "branch naming settings".
    #[test]
    fn branch_naming_defaults_to_the_static_prefix_and_round_trips_each_mode() {
        let settings = decode(json!({}));
        assert_eq!(
            settings.branch_naming_mode,
            agent_domain::BranchNamingMode::Static
        );
        assert_eq!(
            settings.branch_name_prefix,
            agent_domain::DEFAULT_BRANCH_NAME_PREFIX
        );
        assert_eq!(settings.branch_name_instructions, "");
        for mode in ["static", "semantic", "custom"] {
            let naming = json!({
                "branchNamingMode": mode,
                "branchNamePrefix": "team/",
                "branchNameInstructions": "Include the issue ID.",
            });
            let mut input = naming.clone();
            input["projectOverrides"] = json!({ "project": naming });
            round_trips(input.clone());
            let settings = decode(input);
            let project = &settings.project_overrides["project"];
            assert_eq!(
                project.branch_naming_mode,
                Some(settings.branch_naming_mode)
            );
            assert_eq!(project.branch_name_prefix.as_deref(), Some("team/"));
            assert_eq!(
                project.branch_name_instructions.as_deref(),
                Some("Include the issue ID.")
            );
        }
    }

    #[test]
    fn a_null_override_is_kept_apart_from_an_absent_one() {
        let mut settings = HostSettings::default();
        settings.project_overrides.insert(
            "project".into(),
            ProjectSettingsOverrides {
                pull_request_merge_method: Some(Nullable::Null),
                ..Default::default()
            },
        );
        let encoded = serde_json::to_value(&settings).unwrap();
        assert_eq!(decode(encoded), settings);
        let inherited = settings.patched(&HostSettingsPatch {
            project_overrides: [(
                "project".into(),
                Some(ProjectSettingsOverridesPatch {
                    pull_request_merge_method: Some(OverrideChange::Inherit),
                    ..Default::default()
                }),
            )]
            .into(),
            ..Default::default()
        });
        assert!(inherited.project_overrides.is_empty());
    }

    #[test]
    fn browser_defaults_fill_the_panel_at_full_zoom_and_reject_odd_sizes() {
        let settings = decode(json!({}));
        assert_eq!(settings.browser, BrowserDefaults::default());
        assert_eq!(settings.browser.viewport, BrowserViewport::Fill);
        assert_eq!(settings.browser.zoom_percent, 100);
        assert!(settings.browser.auto_show_floating_preview);
        let with = |patch| {
            HostSettings::default().patched(&HostSettingsPatch {
                browser: Some(patch),
                ..Default::default()
            })
        };
        assert!(
            with(BrowserDefaultsPatch {
                zoom_percent: Some(33),
                ..Default::default()
            })
            .validate()
            .is_ok()
        );
        assert!(
            with(BrowserDefaultsPatch {
                zoom_percent: Some(101),
                ..Default::default()
            })
            .validate()
            .is_err()
        );
        let freeform = |width, height| {
            with(BrowserDefaultsPatch {
                viewport: Some(BrowserViewport::Freeform { width, height }),
                ..Default::default()
            })
            .validate()
        };
        assert!(freeform(3840, 2160).is_ok());
        assert!(freeform(239, 800).is_err());
        assert!(freeform(3840, 2161).is_err());
        let preset = |preset_id: &str| {
            with(BrowserDefaultsPatch {
                viewport: Some(BrowserViewport::Preset {
                    width: 390,
                    height: 844,
                    preset_id: preset_id.into(),
                }),
                ..Default::default()
            })
            .validate()
        };
        assert!(preset("iphone-12-pro").is_ok());
        assert!(preset("desktop-1920x1080").is_err());
    }

    // shared backgroundActivitySettings.ts resolve/normalize, intervals only.
    #[test]
    fn background_activity_resolves_presets_and_collapses_matching_custom_intervals() {
        let settings = decode(json!({}));
        assert_eq!(settings.background_activity, BackgroundActivity::default());
        assert_eq!(
            settings.background_activity.resolved(),
            ResolvedBackgroundActivity {
                profile: BackgroundActivityProfile::Balanced,
                automatic_git_fetch_interval_ms: 30_000,
                provider_health_refresh_interval_ms: 300_000,
            }
        );
        let custom = settings.patched(&HostSettingsPatch {
            background_activity: Some(BackgroundActivityPatch {
                profile: Some(BackgroundActivityProfileSelection::Custom),
                base_profile: Some(BackgroundActivityProfile::Performance),
                overrides: Some(BackgroundActivityOverrides {
                    automatic_git_fetch_interval_ms: Some(10_000),
                    provider_health_refresh_interval_ms: Some(60_000),
                }),
            }),
            ..Default::default()
        });
        assert_eq!(
            custom.background_activity,
            BackgroundActivity {
                profile: BackgroundActivityProfileSelection::Custom,
                base_profile: Some(BackgroundActivityProfile::Performance),
                overrides: BackgroundActivityOverrides {
                    automatic_git_fetch_interval_ms: Some(10_000),
                    provider_health_refresh_interval_ms: None,
                },
            }
        );
        assert_eq!(
            custom.background_activity.resolved(),
            ResolvedBackgroundActivity {
                profile: BackgroundActivityProfile::Performance,
                automatic_git_fetch_interval_ms: 10_000,
                provider_health_refresh_interval_ms: 60_000,
            }
        );
        let matching = custom.patched(&HostSettingsPatch {
            background_activity: Some(BackgroundActivityPatch {
                overrides: Some(BackgroundActivityOverrides {
                    automatic_git_fetch_interval_ms: Some(0),
                    provider_health_refresh_interval_ms: Some(900_000),
                }),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(
            matching.background_activity,
            BackgroundActivity {
                profile: BackgroundActivityProfileSelection::BatterySaver,
                ..Default::default()
            }
        );
        let balanced = custom.patched(&HostSettingsPatch {
            background_activity: Some(BackgroundActivityPatch {
                profile: Some(BackgroundActivityProfileSelection::Balanced),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert_eq!(balanced.background_activity, BackgroundActivity::default());
    }

    #[test]
    fn provider_instance_maps_replace_atomically_and_validate_models() {
        let decoded = decode(json!({
            "providerInstances": {
                "build": {
                    "driver": "codex",
                    "customModels": ["bare-model", {"slug": "build-model", "name": "Build model"}],
                },
            },
        }));
        assert_eq!(
            decoded.provider_instances["build"].custom_models[0].slug,
            "bare-model"
        );
        assert_eq!(
            decoded.provider_instances["build"].custom_models[1].slug,
            "build-model"
        );
        assert_eq!(
            decoded.provider_instances["build"].custom_models[0].name,
            "bare-model"
        );
        assert!(
            decoded.provider_instances["build"].custom_models[1]
                .aliases
                .is_empty()
        );
        let capabilities = decode(json!({
            "providerInstances": {
                "capabilities": {
                    "driver": "codex",
                    "customModels": [{
                        "slug": "reasoning",
                        "capabilities": {
                            "optionDescriptors": [{
                            "Select": {
                                    "id": "effort",
                                    "label": "Reasoning",
                                    "description": null,
                                    "options": [],
                                    "currentValue": null,
                                    "promptInjectedValues": []
                                }
                            }
                            ]
                        }
                    }]
                }
            }
        }));
        assert_eq!(
            capabilities.provider_instances["capabilities"].custom_models[0]
                .option_descriptors
                .len(),
            1
        );
        assert_eq!(
            serde_json::to_value(&decoded.provider_instances["build"].custom_models).unwrap(),
            json!([
                "bare-model",
                {"slug": "build-model", "name": "Build model"}
            ])
        );
        assert_eq!(
            serde_json::to_value(&capabilities.provider_instances["capabilities"].custom_models)
                .unwrap()[0]["capabilities"]["optionDescriptors"][0]["Select"]["id"],
            "effort"
        );
        let custom = ProviderInstanceConfig {
            driver: agent_domain::Driver::Codex,
            display_name: "Build Codex".into(),
            binary_path: Some(" /opt/codex ".into()),
            environment: std::collections::BTreeMap::from([(
                "PROVIDER_MODE".into(),
                "work".into(),
            )]),
            custom_models: vec![ProviderCustomModel {
                slug: "build-model".into(),
                name: "Build model".into(),
                aliases: vec![],
                badge: Some("custom".into()),
                is_default: false,
                is_legacy: false,
                option_descriptors: vec![],
            }],
            ..Default::default()
        };
        let settings = HostSettings::default().patched(&HostSettingsPatch {
            provider_instances: Some([(String::from("build"), custom.clone())].into()),
            ..Default::default()
        });
        assert_eq!(settings.provider_instances.get("build"), Some(&custom));
        assert!(settings.provider_instances.get("codex").is_none());
        assert!(settings.validate().is_ok());
        assert_eq!(
            settings.provider_instances["build"].environment["PROVIDER_MODE"],
            "work"
        );
        let invalid_environment = HostSettings::default().patched(&HostSettingsPatch {
            provider_instances: Some(
                [(
                    String::from("build"),
                    ProviderInstanceConfig {
                        environment: std::collections::BTreeMap::from([(
                            "bad-name".into(),
                            "x".into(),
                        )]),
                        ..Default::default()
                    },
                )]
                .into(),
            ),
            ..Default::default()
        });
        assert!(invalid_environment.validate().is_err());
        let duplicate = HostSettings::default().patched(&HostSettingsPatch {
            provider_instances: Some(
                [(
                    String::from("build"),
                    ProviderInstanceConfig {
                        custom_models: vec![
                            ProviderCustomModel {
                                slug: "same".into(),
                                name: "One".into(),
                                aliases: vec![],
                                badge: None,
                                is_default: false,
                                is_legacy: false,
                                option_descriptors: vec![],
                            },
                            ProviderCustomModel {
                                slug: "same".into(),
                                name: "Two".into(),
                                aliases: vec![],
                                badge: None,
                                is_default: false,
                                is_legacy: false,
                                option_descriptors: vec![],
                            },
                        ],
                        ..custom
                    },
                )]
                .into(),
            ),
            ..Default::default()
        });
        assert!(duplicate.validate().is_err());
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

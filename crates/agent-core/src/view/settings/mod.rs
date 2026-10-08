//! Settings: usage-limit handling, auto-settle, follow-ups, the clock
//! format, restart continuation and new-thread defaults, for the Host or one
//! project's overrides.
//!
//! Host values come from `HostSettings` (read with `ReadSettings`) and the
//! worktree settings; the follow-up behavior and new-thread draft are device
//! settings.
//!
//! Each section lives in its own file and registers itself in `registry`,
//! which assembles the page and routes a row's edits and resets to its owner.
mod agent;
mod auto_settle;
mod behavior;
mod beta;
mod browser;
mod capture;
#[cfg(test)]
mod fixtures;
mod follow_ups;
mod maintenance;
mod new_threads;
mod notifications;
mod patch;
mod registry;
mod source_control;
mod storage;
mod usage_limits;

pub use auto_settle::{
    AUTO_SETTLE_DEFAULT_DAYS, MAX_AUTO_SETTLE_DAYS, MIN_AUTO_SETTLE_DAYS, parse_auto_settle_days,
};
pub use behavior::timestamp_format_id;
pub use new_threads::{default_model_picker, runtime_mode_id};
pub use patch::{
    ProjectSettingKey, ResolvedSettings, SettingChange, clear_project_overrides,
    new_worktrees_start_from_origin, plan_settings_update, resolve_project_settings,
};
pub use registry::{setting_intent, setting_reset_intent, settings_view};

/// The Host's defaults, or one project's overrides of them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettingsScope {
    #[default]
    Host,
    Project {
        project_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettingId {
    AutoResumeLimitedThreads,
    SnoozeLimitedThreads,
    AutoSettleInactiveThreads,
    AutoSettleDays,
    FollowUpBehavior,
    TimeFormat,
    ContinueAfterRestart,
    WorkingSection,
    DefaultModel,
    DefaultPermissions,
    DefaultWorkspace,
    WorktreeSubmodules,
    StartFromOrigin,
    NotificationMode,
    InAppNotifications,
    LiveActivities,
    ProviderUpdateChecks,
    AgentBrowserAccess,
    ResponseStreaming,
    AutoSettleOnMerge,
    BackgroundActivity,
    DefaultAutoPull,
    BranchNaming,
    SourceControlWritingStyle,
    FollowChangeRequestTemplates,
    PullRequestMergeMethod,
    StorageWorktreeAfterDays,
    StorageWorktreeOnMerge,
    StorageWorktreeOnDelete,
    StorageWorktreeUnchanged,
    StorageBrowserArtifactsAfterDays,
    StorageLogsAfterDays,
    AddProjectBaseDirectory,
    LoadBalancing,
    SnapshotCapture,
    SnapshotIncludeAccessibility,
    SnapshotShortcut,
    SnapshotPlaySound,
    SnapshotSound,
    SnapshotFlash,
    SnapshotAnimations,
    BrowserDefaultViewport,
    BrowserDefaultViewportWidth,
    BrowserDefaultViewportHeight,
    BrowserDefaultZoom,
    BrowserDefaultAppearance,
    BrowserRecordingFrameRate,
    BrowserRecordingShowKeyPresses,
    BrowserRecordingShowMousePresses,
    BrowserLinkTarget,
    BrowserAutoShowFloatingPreview,
    BrowserDefaultProfile,
    BrowserProfiles,
}

/// Where a project page's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettingSource {
    Project,
    Host,
}

/// A project row can always clear its own override, even when the effective
/// value happens to equal the Host default. Host rows reset only when they
/// differ from their built-in value.
pub(super) const fn resettable_for(source: Option<SettingSource>, differs: bool) -> bool {
    matches!(source, Some(SettingSource::Project)) || (source.is_none() && differs)
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SettingChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettingControl {
    Switch {
        on: bool,
    },
    Choice {
        choices: Vec<SettingChoice>,
        selected: Option<String>,
    },
    /// An integer field that commits only values within the bounds.
    Number {
        value: u32,
        min: u32,
        max: u32,
    },
    /// Opens the model picker on the new-thread draft.
    Model {
        model_label: String,
        traits_label: Option<String>,
    },
    /// A trimmed path or path prefix used by the add-project folder picker.
    Text {
        value: String,
        placeholder: Option<String>,
    },
    /// Built-in and custom browser identities. Built-ins are rendered as
    /// read-only; native owners use the profile intents for custom edits.
    BrowserProfiles {
        profiles: Vec<crate::view::browser::BrowserProfile>,
        default_profile_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SettingsRow {
    pub id: SettingId,
    pub title: String,
    pub description: Option<String>,
    pub control: SettingControl,
    /// The value differs from the default, so a reset is offered.
    pub resettable: bool,
    /// Set on a project page.
    pub source: Option<SettingSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SettingsSection {
    pub id: String,
    pub title: String,
    pub rows: Vec<SettingsRow>,
    pub footer: Option<String>,
}

/// The project a project page edits.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ProjectOverridesHeader {
    pub project_id: String,
    pub label: String,
    /// "Use defaults" clears the project's overrides.
    pub has_overrides: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SettingsView {
    pub project: Option<ProjectOverridesHeader>,
    pub sections: Vec<SettingsSection>,
}

/// A control's new value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettingValue {
    Switch { on: bool },
    Choice { id: String },
    Number { value: u32 },
    Text { value: String },
}

fn row(
    id: SettingId,
    title: &str,
    description: Option<&str>,
    control: SettingControl,
) -> SettingsRow {
    SettingsRow {
        id,
        title: title.into(),
        description: description.map(Into::into),
        control,
        resettable: false,
        source: None,
    }
}

fn choice(id: &str, label: &str, description: Option<&str>) -> SettingChoice {
    SettingChoice {
        id: id.into(),
        label: label.into(),
        description: description.map(Into::into),
    }
}

fn section(id: &str, title: &str, rows: Vec<SettingsRow>, footer: Option<&str>) -> SettingsSection {
    SettingsSection {
        id: id.into(),
        title: title.into(),
        rows,
        footer: footer.map(Into::into),
    }
}

/// The edit of a Host setting on the page of `scope`.
fn update(scope: &SettingsScope, change: SettingChange) -> crate::state::Intent {
    crate::state::Intent::UpdateSettings {
        scope: scope.clone(),
        change,
    }
}

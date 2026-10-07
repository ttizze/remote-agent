//! Conversation settings: usage-limit handling, auto-settle, follow-ups, the
//! clock format, restart continuation and new-thread defaults, for the Host
//! or one project's overrides.
//!
//! Host values come from `ConversationSettings` (read with
//! `ReadConversationSettings`) and the worktree settings; the follow-up
//! behavior and new-thread draft are device settings.
use crate::{
    commands::build::FollowUpBehavior,
    models::{AutoSettle, ConversationSettings, ProjectConversationSettings},
    state::{Draft, Intent, Preferences, Snapshot},
    view::{
        models::{
            catalog,
            picker::{ModelPickerOptions, ModelPickerView, build_model_picker, trigger},
            traits::build_traits,
        },
        time::TimestampFormat,
    },
};
use agent_domain::RuntimeMode;

pub const MIN_AUTO_SETTLE_DAYS: u32 = 1;
pub const MAX_AUTO_SETTLE_DAYS: u32 = 90;
/// The days an inactive thread waits when auto-settle is turned on.
pub const AUTO_SETTLE_DEFAULT_DAYS: u32 = 3;

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
    StartFromOrigin,
}

/// Where a project page's value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettingSource {
    Project,
    Host,
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

/// A project's effective values and where each comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConversationSettings {
    pub auto_settle: (AutoSettle, SettingSource),
    pub continue_after_restart: (bool, SettingSource),
    pub new_worktrees_start_from_origin: (bool, SettingSource),
}

pub fn resolve_project_settings(
    host: &ConversationSettings,
    project_id: Option<&str>,
) -> ResolvedConversationSettings {
    fn pick<T>(value: Option<T>, fallback: T) -> (T, SettingSource) {
        value.map_or((fallback, SettingSource::Host), |value| {
            (value, SettingSource::Project)
        })
    }
    let overrides = project_id.and_then(|id| host.project_overrides.get(id));
    ResolvedConversationSettings {
        auto_settle: pick(
            overrides.and_then(|project| project.auto_settle),
            host.auto_settle,
        ),
        continue_after_restart: pick(
            overrides.and_then(|project| project.continue_after_restart),
            host.continue_after_restart,
        ),
        new_worktrees_start_from_origin: pick(
            overrides.and_then(|project| project.new_worktrees_start_from_origin),
            host.new_worktrees_start_from_origin,
        ),
    }
}

/// Whether a project's new worktrees start from origin; on until the Host's
/// settings are read.
pub fn new_worktrees_start_from_origin(
    host: Option<&ConversationSettings>,
    project_id: Option<&str>,
) -> bool {
    host.map_or(
        ConversationSettings::default().new_worktrees_start_from_origin,
        |host| {
            resolve_project_settings(host, project_id)
                .new_worktrees_start_from_origin
                .0
        },
    )
}

/// A Host conversation setting a project can override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ProjectSettingKey {
    AutoSettle,
    ContinueAfterRestart,
    NewWorktreesStartFromOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ConversationSettingChange {
    AutoResumeLimitedThreads {
        on: bool,
    },
    SnoozeLimitedThreads {
        on: bool,
    },
    /// `None` turns auto-settle off.
    AutoSettle {
        days: Option<u32>,
    },
    ContinueAfterRestart {
        on: bool,
    },
    NewWorktreesStartFromOrigin {
        on: bool,
    },
    /// A project follows the Host's value again.
    Inherit {
        key: ProjectSettingKey,
    },
}

fn auto_settle(days: Option<u32>) -> AutoSettle {
    days.map_or(AutoSettle::Never, AutoSettle::AfterDays)
}

fn clear(overrides: &mut ProjectConversationSettings, key: ProjectSettingKey) {
    match key {
        ProjectSettingKey::AutoSettle => overrides.auto_settle = None,
        ProjectSettingKey::ContinueAfterRestart => overrides.continue_after_restart = None,
        ProjectSettingKey::NewWorktreesStartFromOrigin => {
            overrides.new_worktrees_start_from_origin = None
        }
    }
}

fn store(
    host: &mut ConversationSettings,
    project_id: &str,
    overrides: ProjectConversationSettings,
) {
    if overrides == ProjectConversationSettings::default() {
        host.project_overrides.remove(project_id);
    } else {
        host.project_overrides.insert(project_id.into(), overrides);
    }
}

/// The settings to save after one change. A project page writes only that
/// project's override; usage-limit handling stays Host-wide, so a project
/// page cannot change it (`None`).
pub fn plan_conversation_settings_update(
    host: &ConversationSettings,
    scope: &SettingsScope,
    change: &ConversationSettingChange,
) -> Option<ConversationSettings> {
    let mut next = host.clone();
    match scope {
        SettingsScope::Host => match change {
            ConversationSettingChange::AutoResumeLimitedThreads { on } => {
                next.auto_resume_limited_threads = *on
            }
            ConversationSettingChange::SnoozeLimitedThreads { on } => {
                next.snooze_limited_threads = *on
            }
            ConversationSettingChange::AutoSettle { days } => next.auto_settle = auto_settle(*days),
            ConversationSettingChange::ContinueAfterRestart { on } => {
                next.continue_after_restart = *on
            }
            ConversationSettingChange::NewWorktreesStartFromOrigin { on } => {
                next.new_worktrees_start_from_origin = *on
            }
            ConversationSettingChange::Inherit { .. } => return None,
        },
        SettingsScope::Project { project_id } => {
            let mut overrides = next
                .project_overrides
                .get(project_id)
                .cloned()
                .unwrap_or_default();
            match change {
                ConversationSettingChange::AutoSettle { days } => {
                    overrides.auto_settle = Some(auto_settle(*days))
                }
                ConversationSettingChange::ContinueAfterRestart { on } => {
                    overrides.continue_after_restart = Some(*on)
                }
                ConversationSettingChange::NewWorktreesStartFromOrigin { on } => {
                    overrides.new_worktrees_start_from_origin = Some(*on)
                }
                ConversationSettingChange::Inherit { key } => clear(&mut overrides, *key),
                ConversationSettingChange::AutoResumeLimitedThreads { .. }
                | ConversationSettingChange::SnoozeLimitedThreads { .. } => return None,
            }
            store(&mut next, project_id, overrides);
        }
    }
    Some(next)
}

/// Clears the project's overrides of `keys`, keeping its others.
pub fn clear_project_overrides(
    host: &ConversationSettings,
    project_id: &str,
    keys: &[ProjectSettingKey],
) -> ConversationSettings {
    let mut next = host.clone();
    if let Some(mut overrides) = next.project_overrides.get(project_id).cloned() {
        for key in keys {
            clear(&mut overrides, *key);
        }
        store(&mut next, project_id, overrides);
    }
    next
}

/// The days the auto-settle field commits: an integer from 1 to 90.
pub fn parse_auto_settle_days(text: &str) -> Option<u32> {
    let value: f64 = text.trim().parse().ok()?;
    (value.fract() == 0.0
        && (f64::from(MIN_AUTO_SETTLE_DAYS)..=f64::from(MAX_AUTO_SETTLE_DAYS)).contains(&value))
    .then_some(value as u32)
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

fn auto_settle_rows(value: AutoSettle, source: Option<SettingSource>) -> Vec<SettingsRow> {
    let defaults = ConversationSettings::default();
    let days = match value {
        AutoSettle::AfterDays(days) => Some(days),
        AutoSettle::Never => None,
    };
    let mut rows = vec![SettingsRow {
        resettable: value != defaults.auto_settle,
        source,
        ..row(
            SettingId::AutoSettleInactiveThreads,
            "Auto-settle inactive threads",
            Some("Sidebar threads with no activity for this long settle automatically."),
            SettingControl::Switch { on: days.is_some() },
        )
    }];
    if let Some(days) = days {
        rows.push(SettingsRow {
            source,
            ..row(
                SettingId::AutoSettleDays,
                "Days of inactivity before auto-settle",
                Some("Any new activity un-settles a thread automatically."),
                SettingControl::Number {
                    value: days,
                    min: MIN_AUTO_SETTLE_DAYS,
                    max: MAX_AUTO_SETTLE_DAYS,
                },
            )
        });
    }
    rows
}

fn continue_row(on: bool, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: on != ConversationSettings::default().continue_after_restart,
        source,
        ..row(
            SettingId::ContinueAfterRestart,
            "Continue threads after restarts",
            Some(
                "Automatically resume interrupted threads after an update, crash, or machine restart on the selected environments.",
            ),
            SettingControl::Switch { on },
        )
    }
}

fn start_from_origin_row(on: bool, source: Option<SettingSource>) -> SettingsRow {
    SettingsRow {
        resettable: on != ConversationSettings::default().new_worktrees_start_from_origin,
        source,
        ..row(
            SettingId::StartFromOrigin,
            "Start from origin",
            Some(
                "Creates the worktree from the latest matching branch on origin instead of your local branch.",
            ),
            SettingControl::Switch { on },
        )
    }
}

fn runtime_mode_choices() -> Vec<SettingChoice> {
    [
        (
            RuntimeMode::ApprovalRequired,
            "Supervised",
            "Ask before commands and file changes.",
        ),
        (
            RuntimeMode::AutoAcceptEdits,
            "Auto-accept edits",
            "Auto-approve edits, ask before other actions.",
        ),
        (
            RuntimeMode::Auto,
            "Auto",
            "Supported providers approve routine actions; others still ask.",
        ),
        (
            RuntimeMode::FullAccess,
            "Full access",
            "Allow commands and edits without prompts.",
        ),
    ]
    .into_iter()
    .map(|(mode, label, description)| choice(runtime_mode_id(mode), label, Some(description)))
    .collect()
}

pub fn runtime_mode_id(mode: RuntimeMode) -> &'static str {
    match mode {
        RuntimeMode::ApprovalRequired => "approval-required",
        RuntimeMode::AutoAcceptEdits => "auto-accept-edits",
        RuntimeMode::Auto => "auto",
        RuntimeMode::FullAccess => "full-access",
    }
}

pub fn timestamp_format_id(format: TimestampFormat) -> &'static str {
    match format {
        TimestampFormat::Locale => "locale",
        TimestampFormat::TwelveHour => "12-hour",
        TimestampFormat::TwentyFourHour => "24-hour",
    }
}

fn host_sections(
    snapshot: &Snapshot,
    host: Option<&ConversationSettings>,
    timestamp_format: TimestampFormat,
) -> Vec<SettingsSection> {
    let mut sections = vec![];
    if let Some(host) = host {
        sections.push(section(
            "usage-limits",
            "Usage limits",
            vec![
                SettingsRow {
                    resettable: host.auto_resume_limited_threads,
                    ..row(
                        SettingId::AutoResumeLimitedThreads,
                        "Auto-resume limited threads",
                        Some("Resume usage-limit stops at the reported reset time. Each thread can cancel its scheduled continuation."),
                        SettingControl::Switch { on: host.auto_resume_limited_threads },
                    )
                },
                SettingsRow {
                    resettable: host.snooze_limited_threads,
                    ..row(
                        SettingId::SnoozeLimitedThreads,
                        "Snooze limited threads",
                        Some("Snooze usage-limit stops until the reported reset time. Combine with auto-resume to continue when they wake."),
                        SettingControl::Switch { on: host.snooze_limited_threads },
                    )
                },
            ],
            None,
        ));
        sections.push(section(
            "auto-settle",
            "Auto-settle",
            auto_settle_rows(host.auto_settle, None),
            None,
        ));
    }
    let follow_up = match snapshot.follow_up {
        FollowUpBehavior::Queue => Some("queue"),
        FollowUpBehavior::Steer => Some("steer"),
        FollowUpBehavior::Restart => None,
    };
    sections.push(section(
        "follow-ups",
        "While the agent is running",
        vec![SettingsRow {
            resettable: snapshot.follow_up != FollowUpBehavior::default(),
            ..row(
                SettingId::FollowUpBehavior,
                "Follow-up behavior",
                Some("Queue follow-ups while the agent runs or steer the current run."),
                SettingControl::Choice {
                    choices: vec![
                        choice("queue", "Queue", Some("Your message waits and runs after the current turn finishes.")),
                        choice("steer", "Steer", Some("Your message reaches the agent right away, changing what it is working on.")),
                    ],
                    selected: follow_up.map(Into::into),
                },
            )
        }],
        Some("Long-press the send button to use the other option for a single message. With a hardware keyboard, hold Command while sending."),
    ));
    let mut behavior = vec![SettingsRow {
        resettable: timestamp_format != TimestampFormat::default(),
        ..row(
            SettingId::TimeFormat,
            "Time format",
            Some("System default follows your browser or OS clock preference."),
            SettingControl::Choice {
                choices: vec![
                    choice("locale", "System default", None),
                    choice("12-hour", "12-hour", None),
                    choice("24-hour", "24-hour", None),
                ],
                selected: Some(timestamp_format_id(timestamp_format).into()),
            },
        )
    }];
    if let Some(host) = host {
        behavior.push(continue_row(host.continue_after_restart, None));
    }
    sections.push(section("behavior", "Behavior", behavior, None));
    sections.push(section(
        "beta",
        "Beta",
        vec![SettingsRow {
            resettable: snapshot.preferences.working_section,
            ..row(
                SettingId::WorkingSection,
                "Working section",
                Some("Fold working and monitoring threads into a Working section. They return to the top of the inbox when they need you."),
                SettingControl::Switch {
                    on: snapshot.preferences.working_section,
                },
            )
        }],
        None,
    ));

    let draft = &snapshot.default_draft;
    let models = catalog(snapshot);
    let traits = build_traits(&models, draft, false);
    let mut new_threads = vec![
        row(
            SettingId::DefaultModel,
            "Model",
            Some("Default model for new threads."),
            SettingControl::Model {
                model_label: trigger(&models, &draft.instance_id, &draft.model).label,
                traits_label: traits.visible.then_some(traits.trigger.label),
            },
        ),
        SettingsRow {
            resettable: draft.runtime_mode != RuntimeMode::FullAccess,
            ..row(
                SettingId::DefaultPermissions,
                "Permissions",
                Some("Default permissions for new threads."),
                SettingControl::Choice {
                    choices: runtime_mode_choices(),
                    selected: Some(runtime_mode_id(draft.runtime_mode).into()),
                },
            )
        },
    ];
    if let Some(worktrees) = &snapshot.workspace.worktree_settings {
        new_threads.push(row(
            SettingId::DefaultWorkspace,
            "Workspace",
            Some("Where new threads start."),
            SettingControl::Choice {
                choices: vec![
                    choice("local", "Current checkout", None),
                    choice("worktree", "New worktree", None),
                ],
                selected: Some(
                    if worktrees.create_on_new_session {
                        "worktree"
                    } else {
                        "local"
                    }
                    .into(),
                ),
            },
        ));
    }
    if let Some(host) = host {
        new_threads.push(start_from_origin_row(
            host.new_worktrees_start_from_origin,
            None,
        ));
    }
    sections.push(section("new-threads", "New threads", new_threads, None));
    sections
}

/// A control's new value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum SettingValue {
    Switch { on: bool },
    Choice { id: String },
    Number { value: u32 },
}

fn auto_settle_days(value: AutoSettle) -> Option<u32> {
    match value {
        AutoSettle::AfterDays(days) => Some(days),
        AutoSettle::Never => None,
    }
}

/// What giving setting `id` the new `value` on the page of `scope` does;
/// `None` when the value does not fit the setting.
pub fn setting_intent(
    snapshot: &Snapshot,
    scope: &SettingsScope,
    id: SettingId,
    value: &SettingValue,
) -> Option<Intent> {
    let update = |change| {
        Some(Intent::UpdateConversationSettings {
            scope: scope.clone(),
            change,
        })
    };
    match (id, value) {
        (SettingId::AutoResumeLimitedThreads, SettingValue::Switch { on }) => {
            update(ConversationSettingChange::AutoResumeLimitedThreads { on: *on })
        }
        (SettingId::SnoozeLimitedThreads, SettingValue::Switch { on }) => {
            update(ConversationSettingChange::SnoozeLimitedThreads { on: *on })
        }
        (SettingId::AutoSettleInactiveThreads, SettingValue::Switch { on }) => {
            update(ConversationSettingChange::AutoSettle {
                days: on.then_some(AUTO_SETTLE_DEFAULT_DAYS),
            })
        }
        (SettingId::AutoSettleDays, SettingValue::Number { value })
            if (MIN_AUTO_SETTLE_DAYS..=MAX_AUTO_SETTLE_DAYS).contains(value) =>
        {
            update(ConversationSettingChange::AutoSettle { days: Some(*value) })
        }
        (SettingId::ContinueAfterRestart, SettingValue::Switch { on }) => {
            update(ConversationSettingChange::ContinueAfterRestart { on: *on })
        }
        (SettingId::StartFromOrigin, SettingValue::Switch { on }) => {
            update(ConversationSettingChange::NewWorktreesStartFromOrigin { on: *on })
        }
        (SettingId::WorkingSection, SettingValue::Switch { on }) => {
            Some(Intent::SetWorkingSection { enabled: *on })
        }
        (SettingId::FollowUpBehavior, SettingValue::Choice { id }) => {
            let behavior = match id.as_str() {
                "queue" => FollowUpBehavior::Queue,
                "steer" => FollowUpBehavior::Steer,
                _ => return None,
            };
            Some(Intent::SetFollowUpBehavior { behavior })
        }
        (SettingId::TimeFormat, SettingValue::Choice { id }) => [
            TimestampFormat::Locale,
            TimestampFormat::TwelveHour,
            TimestampFormat::TwentyFourHour,
        ]
        .into_iter()
        .find(|format| timestamp_format_id(*format) == id)
        .map(|format| Intent::SetTimestampFormat { format }),
        (SettingId::DefaultPermissions, SettingValue::Choice { id }) => [
            RuntimeMode::ApprovalRequired,
            RuntimeMode::AutoAcceptEdits,
            RuntimeMode::Auto,
            RuntimeMode::FullAccess,
        ]
        .into_iter()
        .find(|mode| runtime_mode_id(*mode) == id)
        .map(|mode| Intent::SetDefaultRuntimeMode { mode }),
        (SettingId::DefaultWorkspace, SettingValue::Choice { id }) => {
            let create_on_new_session = match id.as_str() {
                "local" => false,
                "worktree" => true,
                _ => return None,
            };
            let settings = snapshot.workspace.worktree_settings.clone()?;
            Some(Intent::SaveWorktreeSettings {
                settings: crate::models::WorktreeSettings {
                    create_on_new_session,
                    ..settings
                },
            })
        }
        _ => None,
    }
}

/// What a row's reset arrow does: a project row follows the Host again, a
/// Host row returns to its default. `None` when the row offers no reset.
pub fn setting_reset_intent(scope: &SettingsScope, row: &SettingsRow) -> Option<Intent> {
    let update = |change| {
        Some(Intent::UpdateConversationSettings {
            scope: scope.clone(),
            change,
        })
    };
    if let SettingsScope::Project { .. } = scope {
        if row.source != Some(SettingSource::Project) {
            return None;
        }
        let key = match row.id {
            SettingId::AutoSettleInactiveThreads | SettingId::AutoSettleDays => {
                ProjectSettingKey::AutoSettle
            }
            SettingId::ContinueAfterRestart => ProjectSettingKey::ContinueAfterRestart,
            SettingId::StartFromOrigin => ProjectSettingKey::NewWorktreesStartFromOrigin,
            _ => return None,
        };
        return update(ConversationSettingChange::Inherit { key });
    }
    if !row.resettable {
        return None;
    }
    let defaults = ConversationSettings::default();
    match row.id {
        SettingId::AutoResumeLimitedThreads => {
            update(ConversationSettingChange::AutoResumeLimitedThreads {
                on: defaults.auto_resume_limited_threads,
            })
        }
        SettingId::SnoozeLimitedThreads => {
            update(ConversationSettingChange::SnoozeLimitedThreads {
                on: defaults.snooze_limited_threads,
            })
        }
        SettingId::AutoSettleInactiveThreads | SettingId::AutoSettleDays => {
            update(ConversationSettingChange::AutoSettle {
                days: auto_settle_days(defaults.auto_settle),
            })
        }
        SettingId::ContinueAfterRestart => {
            update(ConversationSettingChange::ContinueAfterRestart {
                on: defaults.continue_after_restart,
            })
        }
        SettingId::StartFromOrigin => {
            update(ConversationSettingChange::NewWorktreesStartFromOrigin {
                on: defaults.new_worktrees_start_from_origin,
            })
        }
        SettingId::WorkingSection => Some(Intent::SetWorkingSection {
            enabled: Preferences::default().working_section,
        }),
        SettingId::FollowUpBehavior => Some(Intent::SetFollowUpBehavior {
            behavior: FollowUpBehavior::default(),
        }),
        SettingId::TimeFormat => Some(Intent::SetTimestampFormat {
            format: TimestampFormat::default(),
        }),
        SettingId::DefaultPermissions => Some(Intent::SetDefaultRuntimeMode {
            mode: Draft::default().runtime_mode,
        }),
        SettingId::DefaultModel | SettingId::DefaultWorkspace => None,
    }
}

/// The picker the Model row opens: the new-thread default, which no open
/// thread locks or limits.
pub fn default_model_picker(snapshot: &Snapshot, options: &ModelPickerOptions) -> ModelPickerView {
    let draft = &snapshot.default_draft;
    build_model_picker(
        &catalog(snapshot),
        &draft.instance_id,
        &draft.model,
        None,
        &|_, _| None,
        options,
    )
}

/// The settings page. `host` is the Host's conversation settings once read;
/// rows backed by them are left out until then. A project page shows that
/// project's effective auto-settle and restart continuation.
pub fn settings_view(
    snapshot: &Snapshot,
    host: Option<&ConversationSettings>,
    scope: &SettingsScope,
    timestamp_format: TimestampFormat,
) -> SettingsView {
    match scope {
        SettingsScope::Host => SettingsView {
            project: None,
            sections: host_sections(snapshot, host, timestamp_format),
        },
        SettingsScope::Project { project_id } => {
            let label = snapshot
                .shell_projects()
                .iter()
                .find(|project| &project.id == project_id)
                .map_or_else(
                    || "Unavailable project".into(),
                    |project| project.name.clone(),
                );
            let Some(host) = host else {
                return SettingsView {
                    project: None,
                    sections: vec![],
                };
            };
            let resolved = resolve_project_settings(host, Some(project_id));
            SettingsView {
                project: Some(ProjectOverridesHeader {
                    project_id: project_id.clone(),
                    label,
                    has_overrides: host.project_overrides.get(project_id).is_some_and(
                        |overrides| overrides != &ProjectConversationSettings::default(),
                    ),
                }),
                sections: vec![
                    section(
                        "auto-settle",
                        "Auto-settle",
                        auto_settle_rows(resolved.auto_settle.0, Some(resolved.auto_settle.1)),
                        None,
                    ),
                    section(
                        "behavior",
                        "Behavior",
                        vec![continue_row(
                            resolved.continue_after_restart.0,
                            Some(resolved.continue_after_restart.1),
                        )],
                        None,
                    ),
                    section(
                        "new-threads",
                        "New threads",
                        vec![start_from_origin_row(
                            resolved.new_worktrees_start_from_origin.0,
                            Some(resolved.new_worktrees_start_from_origin.1),
                        )],
                        None,
                    ),
                ],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::WorktreeSettings;

    fn host_with(project: &str, overrides: ProjectConversationSettings) -> ConversationSettings {
        let mut host = ConversationSettings::default();
        host.project_overrides.insert(project.into(), overrides);
        host
    }

    fn project(id: &str) -> SettingsScope {
        SettingsScope::Project {
            project_id: id.into(),
        }
    }

    #[test]
    fn edits_a_project_override_without_changing_the_host_default() {
        let host = host_with(
            "first-project",
            ProjectConversationSettings {
                continue_after_restart: Some(true),
                ..Default::default()
            },
        );
        let next = plan_conversation_settings_update(
            &host,
            &project("first-project"),
            &ConversationSettingChange::AutoSettle { days: None },
        )
        .unwrap();
        assert_eq!(
            next.project_overrides["first-project"],
            ProjectConversationSettings {
                auto_settle: Some(AutoSettle::Never),
                continue_after_restart: Some(true),
                ..Default::default()
            }
        );
        assert_eq!(next.auto_settle, host.auto_settle);
        let second = plan_conversation_settings_update(
            &host,
            &project("second-project"),
            &ConversationSettingChange::ContinueAfterRestart { on: false },
        )
        .unwrap();
        assert_eq!(
            second.project_overrides["second-project"].continue_after_restart,
            Some(false)
        );
        assert!(!second.continue_after_restart);
    }

    #[test]
    fn inheriting_removes_only_that_project_override() {
        let host = host_with(
            "first-project",
            ProjectConversationSettings {
                auto_settle: Some(AutoSettle::AfterDays(7)),
                continue_after_restart: Some(true),
                ..Default::default()
            },
        );
        let next = plan_conversation_settings_update(
            &host,
            &project("first-project"),
            &ConversationSettingChange::Inherit {
                key: ProjectSettingKey::ContinueAfterRestart,
            },
        )
        .unwrap();
        assert_eq!(
            next.project_overrides["first-project"],
            ProjectConversationSettings {
                auto_settle: Some(AutoSettle::AfterDays(7)),
                continue_after_restart: None,
                ..Default::default()
            }
        );
        assert_eq!(
            plan_conversation_settings_update(
                &host,
                &SettingsScope::Host,
                &ConversationSettingChange::Inherit {
                    key: ProjectSettingKey::AutoSettle
                },
            ),
            None
        );
    }

    #[test]
    fn resets_only_the_selected_override_and_rejects_host_wide_writes_from_a_project() {
        let host = host_with(
            "first-project",
            ProjectConversationSettings {
                auto_settle: Some(AutoSettle::Never),
                continue_after_restart: Some(true),
                ..Default::default()
            },
        );
        let cleared =
            clear_project_overrides(&host, "first-project", &[ProjectSettingKey::AutoSettle]);
        assert_eq!(
            cleared.project_overrides["first-project"],
            ProjectConversationSettings {
                auto_settle: None,
                continue_after_restart: Some(true),
                ..Default::default()
            }
        );
        let all = clear_project_overrides(
            &host,
            "first-project",
            &[
                ProjectSettingKey::AutoSettle,
                ProjectSettingKey::ContinueAfterRestart,
            ],
        );
        assert!(all.project_overrides.is_empty());
        assert_eq!(
            plan_conversation_settings_update(
                &host,
                &project("first-project"),
                &ConversationSettingChange::SnoozeLimitedThreads { on: true },
            ),
            None
        );
    }

    #[test]
    fn host_wide_changes_write_the_host_value() {
        let next = plan_conversation_settings_update(
            &ConversationSettings::default(),
            &SettingsScope::Host,
            &ConversationSettingChange::AutoResumeLimitedThreads { on: true },
        )
        .unwrap();
        assert!(next.auto_resume_limited_threads);
        let next = plan_conversation_settings_update(
            &next,
            &SettingsScope::Host,
            &ConversationSettingChange::AutoSettle { days: Some(7) },
        )
        .unwrap();
        assert_eq!(next.auto_settle, AutoSettle::AfterDays(7));
    }

    // contracts settings.test.ts "defaults start-from-origin on" and "accepts
    // start-from-origin updates"; a project can override it like T3's
    // project-scoped server settings.
    #[test]
    fn start_from_origin_defaults_on_and_a_project_can_override_it() {
        let decoded: ConversationSettings = serde_json::from_str("{}").unwrap();
        assert!(decoded.new_worktrees_start_from_origin);
        assert!(new_worktrees_start_from_origin(None, Some("p")));
        let host = plan_conversation_settings_update(
            &decoded,
            &SettingsScope::Host,
            &ConversationSettingChange::NewWorktreesStartFromOrigin { on: false },
        )
        .unwrap();
        assert!(!new_worktrees_start_from_origin(Some(&host), Some("p")));
        let host = plan_conversation_settings_update(
            &host,
            &project("p"),
            &ConversationSettingChange::NewWorktreesStartFromOrigin { on: true },
        )
        .unwrap();
        assert!(new_worktrees_start_from_origin(Some(&host), Some("p")));
        assert!(!new_worktrees_start_from_origin(Some(&host), Some("q")));
        let view = settings_view(
            &Snapshot::default(),
            Some(&host),
            &project("p"),
            TimestampFormat::Locale,
        );
        let row = view
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .find(|row| row.id == SettingId::StartFromOrigin)
            .unwrap();
        assert_eq!(
            (&row.control, row.source),
            (
                &SettingControl::Switch { on: true },
                Some(SettingSource::Project)
            )
        );
        same(
            setting_reset_intent(&project("p"), row),
            Some(Intent::UpdateConversationSettings {
                scope: project("p"),
                change: ConversationSettingChange::Inherit {
                    key: ProjectSettingKey::NewWorktreesStartFromOrigin,
                },
            }),
        );
        let cleared = clear_project_overrides(
            &host,
            "p",
            &[ProjectSettingKey::NewWorktreesStartFromOrigin],
        );
        assert!(!cleared.project_overrides.contains_key("p"));
    }

    #[test]
    fn the_days_field_commits_only_whole_days_in_range() {
        assert_eq!(parse_auto_settle_days("7"), Some(7));
        assert_eq!(parse_auto_settle_days(" 90 "), Some(90));
        assert_eq!(parse_auto_settle_days("3.5"), None);
        assert_eq!(parse_auto_settle_days("0"), None);
        assert_eq!(parse_auto_settle_days("91"), None);
        assert_eq!(parse_auto_settle_days(""), None);
    }

    fn ids(view: &SettingsView) -> Vec<(String, Vec<SettingId>)> {
        view.sections
            .iter()
            .map(|section| {
                (
                    section.id.clone(),
                    section.rows.iter().map(|row| row.id).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn host_rows_wait_for_the_host_settings() {
        let snapshot = Snapshot::default();
        let view = settings_view(
            &snapshot,
            None,
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        assert_eq!(
            ids(&view),
            [
                ("follow-ups".into(), vec![SettingId::FollowUpBehavior]),
                ("behavior".into(), vec![SettingId::TimeFormat]),
                ("beta".into(), vec![SettingId::WorkingSection]),
                (
                    "new-threads".into(),
                    vec![SettingId::DefaultModel, SettingId::DefaultPermissions]
                ),
            ]
        );
    }

    #[test]
    fn the_host_page_shows_current_values_and_offers_resets() {
        let mut snapshot = Snapshot {
            follow_up: FollowUpBehavior::Steer,
            ..Snapshot::default()
        };
        snapshot.workspace.worktree_settings = Some(WorktreeSettings {
            create_on_new_session: true,
            ..Default::default()
        });
        let host = ConversationSettings {
            snooze_limited_threads: true,
            ..Default::default()
        };
        let view = settings_view(
            &snapshot,
            Some(&host),
            &SettingsScope::Host,
            TimestampFormat::TwentyFourHour,
        );
        assert_eq!(
            ids(&view),
            [
                (
                    "usage-limits".into(),
                    vec![
                        SettingId::AutoResumeLimitedThreads,
                        SettingId::SnoozeLimitedThreads
                    ]
                ),
                (
                    "auto-settle".into(),
                    vec![
                        SettingId::AutoSettleInactiveThreads,
                        SettingId::AutoSettleDays
                    ]
                ),
                ("follow-ups".into(), vec![SettingId::FollowUpBehavior]),
                (
                    "behavior".into(),
                    vec![SettingId::TimeFormat, SettingId::ContinueAfterRestart]
                ),
                ("beta".into(), vec![SettingId::WorkingSection]),
                (
                    "new-threads".into(),
                    vec![
                        SettingId::DefaultModel,
                        SettingId::DefaultPermissions,
                        SettingId::DefaultWorkspace,
                        SettingId::StartFromOrigin
                    ]
                ),
            ]
        );
        let rows: Vec<&SettingsRow> = view
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .collect();
        let find = |id| *rows.iter().find(|row| row.id == id).unwrap();
        assert!(find(SettingId::SnoozeLimitedThreads).resettable);
        assert!(!find(SettingId::AutoResumeLimitedThreads).resettable);
        assert_eq!(
            find(SettingId::AutoSettleDays).control,
            SettingControl::Number {
                value: 3,
                min: 1,
                max: 90
            }
        );
        assert!(matches!(&find(SettingId::FollowUpBehavior).control,
            SettingControl::Choice { selected: Some(selected), .. } if selected == "steer"));
        assert!(matches!(&find(SettingId::TimeFormat).control,
            SettingControl::Choice { selected: Some(selected), .. } if selected == "24-hour"));
        assert!(matches!(&find(SettingId::DefaultWorkspace).control,
            SettingControl::Choice { selected: Some(selected), .. } if selected == "worktree"));
        assert_eq!(
            find(SettingId::DefaultModel).control,
            SettingControl::Model {
                model_label: "Choose model".into(),
                traits_label: None
            }
        );
    }

    fn same(left: Option<Intent>, right: Option<Intent>) {
        assert_eq!(format!("{left:?}"), format!("{right:?}"));
    }

    #[test]
    fn each_control_value_becomes_its_intent() {
        let mut snapshot = Snapshot::default();
        let host = SettingsScope::Host;
        let on = SettingValue::Switch { on: true };
        same(
            setting_intent(&snapshot, &host, SettingId::AutoSettleInactiveThreads, &on),
            Some(Intent::UpdateConversationSettings {
                scope: SettingsScope::Host,
                change: ConversationSettingChange::AutoSettle { days: Some(3) },
            }),
        );
        same(
            setting_intent(
                &snapshot,
                &project("p"),
                SettingId::AutoSettleDays,
                &SettingValue::Number { value: 7 },
            ),
            Some(Intent::UpdateConversationSettings {
                scope: project("p"),
                change: ConversationSettingChange::AutoSettle { days: Some(7) },
            }),
        );
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::AutoSettleDays,
                &SettingValue::Number { value: 91 },
            ),
            None,
        );
        same(
            setting_intent(&snapshot, &host, SettingId::WorkingSection, &on),
            Some(Intent::SetWorkingSection { enabled: true }),
        );
        let choice = |id: &str| SettingValue::Choice { id: id.into() };
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::FollowUpBehavior,
                &choice("steer"),
            ),
            Some(Intent::SetFollowUpBehavior {
                behavior: FollowUpBehavior::Steer,
            }),
        );
        same(
            setting_intent(&snapshot, &host, SettingId::TimeFormat, &choice("24-hour")),
            Some(Intent::SetTimestampFormat {
                format: TimestampFormat::TwentyFourHour,
            }),
        );
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::DefaultPermissions,
                &choice("auto"),
            ),
            Some(Intent::SetDefaultRuntimeMode {
                mode: RuntimeMode::Auto,
            }),
        );
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::DefaultWorkspace,
                &choice("worktree"),
            ),
            None,
        );
        snapshot.workspace.worktree_settings = Some(crate::models::WorktreeSettings {
            worktree_directory: "/tmp/trees".into(),
            ..Default::default()
        });
        same(
            setting_intent(
                &snapshot,
                &host,
                SettingId::DefaultWorkspace,
                &choice("worktree"),
            ),
            Some(Intent::SaveWorktreeSettings {
                settings: crate::models::WorktreeSettings {
                    create_on_new_session: true,
                    worktree_directory: "/tmp/trees".into(),
                    ..Default::default()
                },
            }),
        );
        same(
            setting_intent(&snapshot, &host, SettingId::TimeFormat, &on),
            None,
        );
    }

    #[test]
    fn a_reset_returns_a_host_row_to_its_default_and_a_project_row_to_the_host() {
        let snapshot = Snapshot {
            follow_up: FollowUpBehavior::Steer,
            ..Snapshot::default()
        };
        let host = ConversationSettings {
            auto_settle: AutoSettle::Never,
            ..Default::default()
        };
        let view = settings_view(
            &snapshot,
            Some(&host),
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        let find = |view: &SettingsView, id| {
            view.sections
                .iter()
                .flat_map(|section| &section.rows)
                .find(|row| row.id == id)
                .cloned()
                .unwrap()
        };
        same(
            setting_reset_intent(
                &SettingsScope::Host,
                &find(&view, SettingId::AutoSettleInactiveThreads),
            ),
            Some(Intent::UpdateConversationSettings {
                scope: SettingsScope::Host,
                change: ConversationSettingChange::AutoSettle { days: Some(3) },
            }),
        );
        same(
            setting_reset_intent(
                &SettingsScope::Host,
                &find(&view, SettingId::FollowUpBehavior),
            ),
            Some(Intent::SetFollowUpBehavior {
                behavior: FollowUpBehavior::Queue,
            }),
        );
        same(
            setting_reset_intent(&SettingsScope::Host, &find(&view, SettingId::TimeFormat)),
            None,
        );
        let overridden = host_with(
            "p",
            ProjectConversationSettings {
                continue_after_restart: Some(true),
                ..Default::default()
            },
        );
        let page = settings_view(
            &snapshot,
            Some(&overridden),
            &project("p"),
            TimestampFormat::Locale,
        );
        same(
            setting_reset_intent(&project("p"), &find(&page, SettingId::ContinueAfterRestart)),
            Some(Intent::UpdateConversationSettings {
                scope: project("p"),
                change: ConversationSettingChange::Inherit {
                    key: ProjectSettingKey::ContinueAfterRestart,
                },
            }),
        );
        same(
            setting_reset_intent(
                &project("p"),
                &find(&page, SettingId::AutoSettleInactiveThreads),
            ),
            None,
        );
    }

    #[test]
    fn the_working_section_row_follows_the_device_preference() {
        let mut snapshot = Snapshot::default();
        snapshot.preferences.working_section = true;
        let view = settings_view(
            &snapshot,
            None,
            &SettingsScope::Host,
            TimestampFormat::Locale,
        );
        let row = view
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .find(|row| row.id == SettingId::WorkingSection)
            .unwrap();
        assert_eq!(row.control, SettingControl::Switch { on: true });
        assert!(row.resettable);
    }

    #[test]
    fn the_default_model_picker_reads_the_new_thread_default_not_the_open_thread() {
        use crate::view::models::fixtures::{host_instance, host_model};
        use agent_domain::Driver;
        let mut snapshot = Snapshot {
            providers: Some(vec![
                host_instance(
                    "codex",
                    Driver::Codex,
                    vec![host_model("gpt-5.4", "gpt-5.4")],
                ),
                host_instance(
                    "claude",
                    Driver::Claude,
                    vec![host_model("sonnet", "Sonnet")],
                ),
            ]),
            ..Snapshot::default()
        };
        snapshot.default_draft.instance_id = "claude".into();
        snapshot.default_draft.driver = agent_domain::Driver::Claude;
        snapshot.default_draft.model = "sonnet".into();
        snapshot.drafts.insert(
            "new:chats".into(),
            crate::state::Draft {
                instance_id: "codex".into(),
                model: "gpt-5.4".into(),
                ..Default::default()
            },
        );
        let picker = default_model_picker(&snapshot, &ModelPickerOptions::default());
        assert_eq!(picker.trigger.instance_id, "claude");
        assert_eq!(picker.locked_driver, None);
        let selected: Vec<_> = picker.rows.iter().filter(|row| row.selected).collect();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].slug, "sonnet");
        assert!(picker.rows.iter().all(|row| row.disabled_reason.is_none()));
    }

    #[test]
    fn a_project_page_shows_effective_values_with_their_source() {
        let host = host_with(
            "first-project",
            ProjectConversationSettings {
                auto_settle: Some(AutoSettle::Never),
                ..Default::default()
            },
        );
        let view = settings_view(
            &Snapshot::default(),
            Some(&host),
            &project("first-project"),
            TimestampFormat::Locale,
        );
        assert_eq!(
            view.project,
            Some(ProjectOverridesHeader {
                project_id: "first-project".into(),
                label: "Unavailable project".into(),
                has_overrides: true,
            })
        );
        assert_eq!(
            ids(&view),
            [
                (
                    "auto-settle".into(),
                    vec![SettingId::AutoSettleInactiveThreads]
                ),
                ("behavior".into(), vec![SettingId::ContinueAfterRestart]),
                ("new-threads".into(), vec![SettingId::StartFromOrigin]),
            ]
        );
        assert_eq!(
            view.sections[0].rows[0].source,
            Some(SettingSource::Project)
        );
        assert_eq!(
            view.sections[0].rows[0].control,
            SettingControl::Switch { on: false }
        );
        assert_eq!(view.sections[1].rows[0].source, Some(SettingSource::Host));
        let other = settings_view(
            &Snapshot::default(),
            Some(&host),
            &project("other"),
            TimestampFormat::Locale,
        );
        assert!(!other.project.unwrap().has_overrides);
    }
}

//! The per-thread action menu shared by the list row and the conversation
//! header, the unsent-draft row menu, and the confirmation texts of the
//! actions that ask first.
use super::snooze::{SnoozePreset, SnoozePresetId, can_snooze, effective_snoozed};
use super::thread_sort::MoveDirection;
use super::thread_summary::{SettledOverride, ThreadSummary};
use super::time::TimestampFormat;
use crate::state::{Snapshot, ThreadAction};
use chrono::{Local, TimeZone};

/// Item ids; `key()` is the stable string id (`snooze:<preset>` for presets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadMenuItemId {
    NewThreadOnBranch,
    FilterByProject,
    ProjectSettings,
    Pin,
    Unpin,
    Settle,
    Unsettle,
    AutoSettle,
    AutoSettleEnabled,
    AutoSettleDisabled,
    Snooze,
    SnoozePreset { preset: SnoozePresetId },
    SnoozeCustom,
    Unsnooze,
    Rename,
    RegenerateTitle,
    MarkUnread,
    Copy,
    CopyPath,
    CopyBranch,
    CopyThreadId,
    Archive,
    Delete,
    Arrange,
    MoveUp,
    MoveDown,
}
impl ThreadMenuItemId {
    pub fn key(self) -> String {
        match self {
            Self::NewThreadOnBranch => "new-thread-on-branch",
            Self::FilterByProject => "filter-by-project",
            Self::ProjectSettings => "project-settings",
            Self::Pin => "pin",
            Self::Unpin => "unpin",
            Self::Settle => "settle",
            Self::Unsettle => "unsettle",
            Self::AutoSettle => "auto-settle",
            Self::AutoSettleEnabled => "auto-settle:enabled",
            Self::AutoSettleDisabled => "auto-settle:disabled",
            Self::Snooze => "snooze",
            Self::SnoozePreset { preset } => return format!("snooze:{}", preset.key()),
            Self::SnoozeCustom => "snooze:custom",
            Self::Unsnooze => "unsnooze",
            Self::Rename => "rename",
            Self::RegenerateTitle => "regenerate-title",
            Self::MarkUnread => "mark-unread",
            Self::Copy => "copy",
            Self::CopyPath => "copy-path",
            Self::CopyBranch => "copy-branch",
            Self::CopyThreadId => "copy-thread-id",
            Self::Archive => "archive",
            Self::Delete => "delete",
            Self::Arrange => "arrange",
            Self::MoveUp => "move-up",
            Self::MoveDown => "move-down",
        }
        .into()
    }
}

/// What choosing an item does. `Thread` is `Intent::Thread { thread_id,
/// action }` and `FilterProject` is `Intent::FilterProject`; the rest are
/// client steps.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadMenuAction {
    Thread {
        action: ThreadAction,
    },
    /// `None` shows every project again.
    FilterProject {
        project_id: Option<String>,
    },
    /// A new thread on the branch: in the thread's worktree when it has one,
    /// otherwise on the local checkout. No intent carries a branch yet.
    NewThreadOnBranch {
        project_id: String,
        branch: String,
        worktree_path: Option<String>,
    },
    /// The client asks for a wake time (`resolve_custom_snooze`), then sends
    /// `ThreadAction::Snooze`.
    CustomSnooze,
    /// The client edits the title in place, then sends `ThreadAction::Rename`.
    StartRename,
    OpenProjectSettings {
        project_id: String,
    },
    /// `None`: the thread has no workspace path ("Path unavailable").
    CopyPath {
        path: Option<String>,
    },
    CopyBranch {
        branch: String,
    },
    CopyThreadId {
        thread_id: String,
    },
    /// Opens the arrangement sheet.
    Arrange,
    /// The client plans the swap with `thread_order` and sends the order key.
    Move {
        direction: MoveDirection,
    },
}

/// A question to confirm before the action runs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadMenuConfirmation {
    /// A dialog title, when the platform's dialog has one.
    pub title: Option<String>,
    pub message: String,
    pub destructive: bool,
}

/// A submenu entry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadMenuChild {
    pub id: ThreadMenuItemId,
    pub label: String,
    /// A secondary line, such as a preset's wake time.
    pub detail: Option<String>,
    pub icon: Option<String>,
    pub separator_before: bool,
    /// Present on option entries; marks the current one.
    pub checked: Option<bool>,
    pub action: ThreadMenuAction,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadMenuItem {
    pub id: ThreadMenuItemId,
    pub label: String,
    /// An icon keyword ("pin", "clock", "trash", ...).
    pub icon: Option<String>,
    pub enabled: bool,
    pub destructive: bool,
    pub separator_before: bool,
    /// `None` on a submenu parent.
    pub action: Option<ThreadMenuAction>,
    pub confirmation: Option<ThreadMenuConfirmation>,
    pub children: Vec<ThreadMenuChild>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadMenuView {
    pub thread_id: String,
    pub title: String,
    pub items: Vec<ThreadMenuItem>,
}

/// Where the menu opens. The list scopes by project and reads lifecycle from
/// the row's shelf; the header has no scoped list behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadMenuSurface {
    #[default]
    List,
    Header,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadMenuOptions {
    pub surface: ThreadMenuSurface,
    pub timestamp_format: TimestampFormat,
    pub confirm_unpin: bool,
    pub confirm_archive: bool,
    pub confirm_delete: bool,
}
impl Default for ThreadMenuOptions {
    fn default() -> Self {
        Self {
            surface: ThreadMenuSurface::List,
            timestamp_format: TimestampFormat::Locale,
            confirm_unpin: false,
            confirm_archive: false,
            confirm_delete: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFilter {
    pub label: String,
    /// The list is already scoped to this thread's project.
    pub is_active: bool,
}

/// The facts the menu reads.
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadMenuState {
    pub thread_id: String,
    pub title: String,
    pub project_id: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    /// The worktree, else the project root.
    pub workspace_path: Option<String>,
    /// `None` on surfaces without a scoped list behind the menu.
    pub project_filter: Option<ProjectFilter>,
    pub is_pinned: bool,
    pub is_settled: bool,
    pub auto_settle_enabled: bool,
    pub is_snoozed: bool,
    pub can_snooze_now: bool,
    pub is_regenerating_title: bool,
    /// Archive is refused while a provider is attached.
    pub is_running: bool,
    pub snooze_presets: Vec<SnoozePreset>,
    pub confirm_unpin: bool,
    pub confirm_archive: bool,
    pub confirm_delete: bool,
}

pub fn unpin_confirmation(title: &str) -> String {
    format!("Unpin thread \"{title}\"?\nThis will move the thread out of your pinned section.")
}
pub fn archive_confirmation(title: &str) -> String {
    format!("Archive thread \"{title}\"?")
}
pub fn delete_confirmation(title: &str) -> String {
    format!(
        "Delete thread \"{title}\"?\nThis permanently clears conversation history for this thread."
    )
}

fn confirm(enabled: bool, message: String, destructive: bool) -> Option<ThreadMenuConfirmation> {
    enabled.then_some(ThreadMenuConfirmation {
        title: None,
        message,
        destructive,
    })
}

pub(crate) fn entry(
    id: ThreadMenuItemId,
    label: impl Into<String>,
    action: Option<ThreadMenuAction>,
) -> ThreadMenuItem {
    ThreadMenuItem {
        id,
        label: label.into(),
        icon: None,
        enabled: true,
        destructive: false,
        separator_before: false,
        action,
        confirmation: None,
        children: vec![],
    }
}

fn item(id: ThreadMenuItemId, label: impl Into<String>, icon: &str) -> ThreadMenuItem {
    ThreadMenuItem {
        icon: Some(icon.into()),
        ..entry(id, label, None)
    }
}

pub(crate) fn child(
    id: ThreadMenuItemId,
    label: impl Into<String>,
    action: ThreadMenuAction,
) -> ThreadMenuChild {
    ThreadMenuChild {
        id,
        label: label.into(),
        detail: None,
        icon: None,
        separator_before: false,
        checked: None,
        action,
    }
}

pub(crate) fn thread_action(action: ThreadAction) -> Option<ThreadMenuAction> {
    Some(ThreadMenuAction::Thread { action })
}

/// One order and gating for every surface. Settle and snooze stay on pinned
/// threads: settling clears the pin, snoozing keeps it until the wake.
pub fn build_thread_menu(state: &ThreadMenuState) -> Vec<ThreadMenuItem> {
    use ThreadMenuItemId as Id;
    let mut items = vec![];
    if let Some(branch) = &state.branch {
        items.push(ThreadMenuItem {
            action: Some(ThreadMenuAction::NewThreadOnBranch {
                project_id: state.project_id.clone(),
                branch: branch.clone(),
                worktree_path: state.worktree_path.clone(),
            }),
            ..item(
                Id::NewThreadOnBranch,
                format!("New thread on {branch}"),
                "message-square-plus",
            )
        });
    }
    items.push(if state.is_pinned {
        ThreadMenuItem {
            action: thread_action(ThreadAction::Unpin),
            confirmation: confirm(state.confirm_unpin, unpin_confirmation(&state.title), false),
            ..item(Id::Unpin, "Unpin thread", "pin-off")
        }
    } else {
        ThreadMenuItem {
            action: thread_action(ThreadAction::Pin),
            ..item(Id::Pin, "Pin thread", "pin")
        }
    });
    items.push(if state.is_settled {
        ThreadMenuItem {
            action: thread_action(ThreadAction::Unsettle),
            ..item(Id::Unsettle, "Un-settle thread", "circle-check")
        }
    } else {
        ThreadMenuItem {
            action: thread_action(ThreadAction::Settle),
            ..item(Id::Settle, "Settle thread", "circle-check")
        }
    });
    items.push(if state.is_snoozed {
        ThreadMenuItem {
            action: thread_action(ThreadAction::Unsnooze),
            ..item(Id::Unsnooze, "Wake thread", "clock")
        }
    } else {
        let presets = state.snooze_presets.iter().map(|preset| {
            child(
                Id::SnoozePreset { preset: preset.id },
                format!("{} ({})", preset.label, preset.when_label),
                ThreadMenuAction::Thread {
                    action: ThreadAction::Snooze {
                        until: preset.snoozed_until.clone(),
                    },
                },
            )
        });
        let custom = ThreadMenuChild {
            separator_before: true,
            ..child(Id::SnoozeCustom, "Custom…", ThreadMenuAction::CustomSnooze)
        };
        ThreadMenuItem {
            enabled: state.can_snooze_now,
            children: presets.chain([custom]).collect(),
            ..item(Id::Snooze, "Snooze", "clock")
        }
    });
    items.push(ThreadMenuItem {
        separator_before: true,
        action: Some(ThreadMenuAction::StartRename),
        ..item(Id::Rename, "Rename thread", "pencil")
    });
    items.push(ThreadMenuItem {
        enabled: !state.is_regenerating_title,
        action: (!state.is_regenerating_title).then_some(ThreadMenuAction::Thread {
            action: ThreadAction::RegenerateTitle,
        }),
        ..item(
            Id::RegenerateTitle,
            if state.is_regenerating_title {
                "Regenerating…"
            } else {
                "Regenerate title"
            },
            "refresh-cw",
        )
    });
    items.push(ThreadMenuItem {
        action: thread_action(ThreadAction::MarkUnread),
        ..item(Id::MarkUnread, "Mark unread", "mail-open")
    });
    if let Some(filter) = &state.project_filter {
        items.push(ThreadMenuItem {
            action: Some(ThreadMenuAction::FilterProject {
                project_id: (!filter.is_active).then(|| state.project_id.clone()),
            }),
            ..item(
                Id::FilterByProject,
                if filter.is_active {
                    "Show all projects".into()
                } else {
                    format!("Filter by {}", filter.label)
                },
                "folder-tree",
            )
        });
    }
    // A setting rather than a lifecycle verb: a submenu with the current
    // option checked.
    let auto_settle = |id, label, enabled: bool| ThreadMenuChild {
        checked: Some(state.auto_settle_enabled == enabled),
        ..child(
            id,
            label,
            ThreadMenuAction::Thread {
                action: ThreadAction::AutoSettle { enabled },
            },
        )
    };
    items.push(ThreadMenuItem {
        children: vec![
            auto_settle(Id::AutoSettleEnabled, "Enabled", true),
            auto_settle(Id::AutoSettleDisabled, "Disabled", false),
        ],
        ..item(Id::AutoSettle, "Auto-settle behavior", "timer")
    });
    let copy_path = ThreadMenuChild {
        icon: Some("folder".into()),
        ..child(
            Id::CopyPath,
            "Path",
            ThreadMenuAction::CopyPath {
                path: state.workspace_path.clone(),
            },
        )
    };
    let copy_branch = state.branch.as_ref().map(|branch| ThreadMenuChild {
        icon: Some("git-branch".into()),
        ..child(
            Id::CopyBranch,
            "Branch",
            ThreadMenuAction::CopyBranch {
                branch: branch.clone(),
            },
        )
    });
    let copy_id = ThreadMenuChild {
        icon: Some("hash".into()),
        ..child(
            Id::CopyThreadId,
            "Thread ID",
            ThreadMenuAction::CopyThreadId {
                thread_id: state.thread_id.clone(),
            },
        )
    };
    items.push(ThreadMenuItem {
        separator_before: true,
        children: [Some(copy_path), copy_branch, Some(copy_id)]
            .into_iter()
            .flatten()
            .collect(),
        ..item(Id::Copy, "Copy", "copy")
    });
    items.push(ThreadMenuItem {
        action: Some(ThreadMenuAction::OpenProjectSettings {
            project_id: state.project_id.clone(),
        }),
        ..item(Id::ProjectSettings, "Project settings", "settings")
    });
    // Archive keeps the conversation under Archived threads, unlike Delete,
    // so it sits beside Delete without its destructive styling.
    items.push(ThreadMenuItem {
        enabled: !state.is_running,
        separator_before: true,
        action: thread_action(ThreadAction::Archive),
        confirmation: confirm(
            state.confirm_archive,
            archive_confirmation(&state.title),
            false,
        ),
        ..item(Id::Archive, "Archive thread", "archive")
    });
    items.push(ThreadMenuItem {
        destructive: true,
        action: thread_action(ThreadAction::Delete),
        confirmation: confirm(
            state.confirm_delete,
            delete_confirmation(&state.title),
            true,
        ),
        ..item(Id::Delete, "Delete", "trash")
    });
    items
}

/// The menu of an active-list thread at `now_ms`, with snooze presets in the
/// device's zone; `None` when the active list does not show the thread.
pub fn thread_menu(
    snapshot: &Snapshot,
    thread_id: &str,
    now_ms: i64,
    options: &ThreadMenuOptions,
) -> Option<ThreadMenuView> {
    let now = Local.timestamp_millis_opt(now_ms).single()?;
    thread_menu_at(snapshot, thread_id, &now, options)
}

pub(crate) fn thread_menu_at<Tz: TimeZone>(
    snapshot: &Snapshot,
    thread_id: &str,
    now: &chrono::DateTime<Tz>,
    options: &ThreadMenuOptions,
) -> Option<ThreadMenuView> {
    let now_ms = now.timestamp_millis();
    let shell = snapshot.shell_view()?;
    let row = shell
        .threads
        .iter()
        .find(|row| row.id.as_str() == thread_id && row.archived_at.is_none())?;
    let thread = ThreadSummary::from_shell(row);
    let project = shell.projects.iter().find(|p| p.id == thread.project);
    let is_snoozed = effective_snoozed(&thread, now_ms);
    let settled = thread.settled_override == Some(SettledOverride::Settled);
    let (is_settled, project_filter) = match options.surface {
        // The list's settled shelf: snooze outranks settlement until the wake.
        ThreadMenuSurface::List => (
            settled && !is_snoozed,
            project.map(|project| ProjectFilter {
                label: project.name.clone(),
                is_active: snapshot.selected_project.as_deref() == Some(project.id.as_str()),
            }),
        ),
        ThreadMenuSurface::Header => (settled, None),
    };
    let state = ThreadMenuState {
        thread_id: thread.id.clone(),
        title: thread.title.clone(),
        project_id: thread.project.clone(),
        branch: thread.branch.clone(),
        worktree_path: thread.worktree_path.clone(),
        workspace_path: thread.worktree_path.clone().or_else(|| {
            project
                .and_then(|project| project.roots.first())
                .map(|root| root.path.clone())
        }),
        project_filter,
        is_pinned: thread.pinned_at.is_some(),
        is_settled,
        auto_settle_enabled: !thread.auto_settle_disabled,
        is_snoozed,
        can_snooze_now: can_snooze(&thread, now_ms),
        is_regenerating_title: thread.title_regenerating,
        is_running: !thread.runtime_can_archive(),
        snooze_presets: super::snooze::resolve_snooze_presets(now, options.timestamp_format),
        confirm_unpin: options.confirm_unpin,
        confirm_archive: options.confirm_archive,
        confirm_delete: options.confirm_delete,
    };
    Some(ThreadMenuView {
        items: build_thread_menu(&state),
        thread_id: thread.id,
        title: thread.title,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum DraftMenuItemId {
    Copy,
    CopyPath,
    CopyBranch,
    ProjectSettings,
    Discard,
}
impl DraftMenuItemId {
    pub fn key(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::CopyPath => "copy-path",
            Self::CopyBranch => "copy-branch",
            Self::ProjectSettings => "project-settings",
            Self::Discard => "discard",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DraftMenuChild {
    pub id: DraftMenuItemId,
    pub label: String,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct DraftMenuItem {
    pub id: DraftMenuItemId,
    pub label: String,
    pub icon: Option<String>,
    pub enabled: bool,
    pub destructive: bool,
    pub separator_before: bool,
    pub children: Vec<DraftMenuChild>,
}

/// The menu of an unsent draft row: copy only the values the draft has.
pub fn build_draft_menu(has_path: bool, has_branch: bool, has_project: bool) -> Vec<DraftMenuItem> {
    let entry = |id, label: &str, icon: &str| DraftMenuItem {
        id,
        label: label.into(),
        icon: Some(icon.into()),
        enabled: true,
        destructive: false,
        separator_before: false,
        children: vec![],
    };
    let copy_child = |id, label: &str, icon: &str| DraftMenuChild {
        id,
        label: label.into(),
        icon: Some(icon.into()),
    };
    let mut items = vec![DraftMenuItem {
        enabled: has_path || has_branch,
        children: [
            has_path.then(|| copy_child(DraftMenuItemId::CopyPath, "Path", "folder")),
            has_branch.then(|| copy_child(DraftMenuItemId::CopyBranch, "Branch", "git-branch")),
        ]
        .into_iter()
        .flatten()
        .collect(),
        ..entry(DraftMenuItemId::Copy, "Copy", "copy")
    }];
    if has_project {
        items.push(entry(
            DraftMenuItemId::ProjectSettings,
            "Project settings",
            "settings",
        ));
    }
    items.push(DraftMenuItem {
        destructive: true,
        separator_before: true,
        ..entry(DraftMenuItemId::Discard, "Discard draft", "trash")
    });
    items
}

#[cfg(test)]
mod tests;

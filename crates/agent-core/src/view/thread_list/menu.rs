//! A row's long-press menu as records: lifecycle first, then arrangement,
//! title and auto-settle, with Delete last.
use super::RowVariant;
use crate::view::snooze::{SnoozePreset, SnoozePresetId};
use crate::view::thread_summary::ThreadSummary;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadMenuAction {
    NewThreadOnBranch,
    CopyThreadId,
    Settle,
    Unsettle,
    /// Wake a snoozed thread now.
    Unsnooze,
    /// Opens the snooze choices.
    Snooze,
    /// Resolve with `resolve_snooze_menu_selection` when picked.
    SnoozePreset {
        preset: SnoozePresetId,
    },
    /// Opens the custom date and time picker.
    SnoozeCustom,
    /// Opens the arrangement sheet.
    Arrange,
    MoveUp,
    MoveDown,
    Pin,
    Unpin,
    Rename,
    RegenerateTitle,
    /// Opens the auto-settle choices.
    AutoSettle,
    SetAutoSettle {
        enabled: bool,
    },
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadMenuOption {
    pub action: ThreadMenuAction,
    pub label: String,
    /// A secondary line, such as a preset's wake time.
    pub detail: Option<String>,
    pub checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ThreadMenuItem {
    pub action: ThreadMenuAction,
    pub label: String,
    pub enabled: bool,
    pub destructive: bool,
    /// A submenu's choices.
    pub options: Vec<ThreadMenuOption>,
}

fn entry(action: ThreadMenuAction, label: &str) -> ThreadMenuItem {
    ThreadMenuItem {
        action,
        label: label.into(),
        enabled: true,
        destructive: false,
        options: vec![],
    }
}

fn option(action: ThreadMenuAction, label: &str, checked: bool) -> ThreadMenuOption {
    ThreadMenuOption {
        action,
        label: label.into(),
        detail: None,
        checked,
    }
}

/// The presets, each with its wake time, then "Custom…".
pub fn snooze_menu_options(presets: &[SnoozePreset]) -> Vec<ThreadMenuOption> {
    presets
        .iter()
        .map(|preset| ThreadMenuOption {
            detail: Some(preset.when_label.clone()),
            ..option(
                ThreadMenuAction::SnoozePreset { preset: preset.id },
                &preset.label,
                false,
            )
        })
        .chain([option(ThreadMenuAction::SnoozeCustom, "Custom…", false)])
        .collect()
}

/// Disabled while a regeneration is running.
pub fn title_regeneration_menu_item(regenerating: bool) -> ThreadMenuItem {
    if regenerating {
        ThreadMenuItem {
            enabled: false,
            ..entry(ThreadMenuAction::RegenerateTitle, "Regenerating…")
        }
    } else {
        entry(ThreadMenuAction::RegenerateTitle, "Regenerate title")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowMenuContext<'a> {
    pub variant: RowVariant,
    pub snoozed: bool,
    /// The row's section follows a saved arrangement.
    pub reorderable: bool,
    pub can_move_up: bool,
    pub can_move_down: bool,
    /// Non-empty when the thread can be snoozed now.
    pub snooze_options: &'a [ThreadMenuOption],
}

/// Snoozed rows offer Wake, settled rows Un-settle, cards Settle and, when
/// allowed, Snooze. Moves appear on cards only; the pin item follows the
/// thread's pin even on a settled row.
pub fn thread_row_menu(thread: &ThreadSummary, context: &RowMenuContext) -> Vec<ThreadMenuItem> {
    let mut menu = vec![];
    if thread.branch.is_some() {
        menu.push(entry(
            ThreadMenuAction::NewThreadOnBranch,
            "New thread on branch",
        ));
    }
    menu.push(entry(ThreadMenuAction::CopyThreadId, "Copy thread ID"));
    let card = context.variant == RowVariant::Card;
    if context.snoozed {
        menu.push(entry(ThreadMenuAction::Unsnooze, "Wake thread"));
    } else if card {
        menu.push(entry(ThreadMenuAction::Settle, "Settle"));
        if !context.snooze_options.is_empty() {
            menu.push(ThreadMenuItem {
                options: context.snooze_options.to_vec(),
                ..entry(ThreadMenuAction::Snooze, "Snooze")
            });
        }
    } else {
        menu.push(entry(ThreadMenuAction::Unsettle, "Un-settle"));
    }
    if !context.snoozed {
        if context.reorderable {
            menu.push(entry(ThreadMenuAction::Arrange, "Arrange threads…"));
            if card {
                menu.push(ThreadMenuItem {
                    enabled: context.can_move_up,
                    ..entry(ThreadMenuAction::MoveUp, "Move up")
                });
                menu.push(ThreadMenuItem {
                    enabled: context.can_move_down,
                    ..entry(ThreadMenuAction::MoveDown, "Move down")
                });
            }
        }
        menu.push(if thread.pinned_at.is_some() {
            entry(ThreadMenuAction::Unpin, "Unpin")
        } else {
            entry(ThreadMenuAction::Pin, "Pin")
        });
    }
    menu.push(entry(ThreadMenuAction::Rename, "Rename"));
    menu.push(title_regeneration_menu_item(thread.title_regenerating));
    let enabled = !thread.auto_settle_disabled;
    menu.push(ThreadMenuItem {
        options: vec![
            option(
                ThreadMenuAction::SetAutoSettle { enabled: true },
                "Enabled",
                enabled,
            ),
            option(
                ThreadMenuAction::SetAutoSettle { enabled: false },
                "Disabled",
                !enabled,
            ),
        ],
        ..entry(ThreadMenuAction::AutoSettle, "Auto-settle behavior")
    });
    menu.push(ThreadMenuItem {
        destructive: true,
        ..entry(ThreadMenuAction::Delete, "Delete")
    });
    menu
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum TitleRename {
    Rename { title: String },
    Noop,
    RejectEmpty,
}

/// The trimmed title to save, or why nothing is saved.
pub fn resolve_thread_title_rename(title: &str, original: &str) -> TitleRename {
    let title = title.trim();
    if title.is_empty() {
        TitleRename::RejectEmpty
    } else if title == original {
        TitleRename::Noop
    } else {
        TitleRename::Rename {
            title: title.into(),
        }
    }
}

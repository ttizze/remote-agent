//! A row's long-press menu as records: lifecycle first, then arrangement,
//! title and auto-settle, with Delete last.
use super::RowVariant;
use crate::state::ThreadAction;
use crate::view::snooze::SnoozePreset;
use crate::view::thread_menu::{
    ThreadMenuAction, ThreadMenuChild, ThreadMenuItem, ThreadMenuItemId, child,
};
use crate::view::thread_sort::MoveDirection;
use crate::view::thread_summary::ThreadSummary;

fn entry(id: ThreadMenuItemId, label: &str, action: Option<ThreadMenuAction>) -> ThreadMenuItem {
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

fn thread(action: ThreadAction) -> Option<ThreadMenuAction> {
    Some(ThreadMenuAction::Thread { action })
}

/// The presets, each with its wake time, then "Custom…". A picked preset is
/// re-resolved with `resolve_snooze_menu_selection` by its id.
pub fn snooze_menu_options(presets: &[SnoozePreset]) -> Vec<ThreadMenuChild> {
    presets
        .iter()
        .map(|preset| ThreadMenuChild {
            detail: Some(preset.when_label.clone()),
            ..child(
                ThreadMenuItemId::SnoozePreset { preset: preset.id },
                &preset.label,
                ThreadMenuAction::Thread {
                    action: ThreadAction::Snooze {
                        until: preset.snoozed_until.clone(),
                    },
                },
            )
        })
        .chain([child(
            ThreadMenuItemId::SnoozeCustom,
            "Custom…",
            ThreadMenuAction::CustomSnooze,
        )])
        .collect()
}

/// Disabled while a regeneration is running.
pub fn title_regeneration_menu_item(regenerating: bool) -> ThreadMenuItem {
    if regenerating {
        ThreadMenuItem {
            enabled: false,
            ..entry(ThreadMenuItemId::RegenerateTitle, "Regenerating…", None)
        }
    } else {
        entry(
            ThreadMenuItemId::RegenerateTitle,
            "Regenerate title",
            thread(ThreadAction::RegenerateTitle),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RowMenuContext<'a> {
    pub variant: RowVariant,
    pub snoozed: bool,
    /// The row's section follows a saved arrangement.
    pub reorderable: bool,
    pub can_move_up: bool,
    pub can_move_down: bool,
    /// Non-empty when the thread can be snoozed now.
    pub snooze_options: &'a [ThreadMenuChild],
}

/// Snoozed rows offer Wake, settled rows Un-settle, cards Settle and, when
/// allowed, Snooze. Moves appear on cards only; the pin item follows the
/// thread's pin even on a settled row.
pub fn thread_row_menu(row: &ThreadSummary, context: &RowMenuContext) -> Vec<ThreadMenuItem> {
    use ThreadMenuItemId as Id;
    let mut menu = vec![];
    if let Some(branch) = &row.branch {
        menu.push(entry(
            Id::NewThreadOnBranch,
            "New thread on branch",
            Some(ThreadMenuAction::NewThreadOnBranch {
                project_id: row.project.clone(),
                branch: branch.clone(),
                worktree_path: row.worktree_path.clone(),
            }),
        ));
    }
    menu.push(entry(
        Id::CopyThreadId,
        "Copy thread ID",
        Some(ThreadMenuAction::CopyThreadId {
            thread_id: row.id.clone(),
        }),
    ));
    let card = context.variant == RowVariant::Card;
    if context.snoozed {
        menu.push(entry(
            Id::Unsnooze,
            "Wake thread",
            thread(ThreadAction::Unsnooze),
        ));
    } else if card {
        menu.push(entry(Id::Settle, "Settle", thread(ThreadAction::Settle)));
        if !context.snooze_options.is_empty() {
            menu.push(ThreadMenuItem {
                children: context.snooze_options.to_vec(),
                ..entry(Id::Snooze, "Snooze", None)
            });
        }
    } else {
        menu.push(entry(
            Id::Unsettle,
            "Un-settle",
            thread(ThreadAction::Unsettle),
        ));
    }
    if !context.snoozed {
        if context.reorderable {
            menu.push(entry(
                Id::Arrange,
                "Arrange threads…",
                Some(ThreadMenuAction::Arrange),
            ));
            if card {
                let step = |id, label, enabled, direction| ThreadMenuItem {
                    enabled,
                    ..entry(id, label, Some(ThreadMenuAction::Move { direction }))
                };
                menu.push(step(
                    Id::MoveUp,
                    "Move up",
                    context.can_move_up,
                    MoveDirection::Up,
                ));
                menu.push(step(
                    Id::MoveDown,
                    "Move down",
                    context.can_move_down,
                    MoveDirection::Down,
                ));
            }
        }
        menu.push(if row.pinned_at.is_some() {
            entry(Id::Unpin, "Unpin", thread(ThreadAction::Unpin))
        } else {
            entry(Id::Pin, "Pin", thread(ThreadAction::Pin))
        });
    }
    menu.push(entry(
        Id::Rename,
        "Rename",
        Some(ThreadMenuAction::StartRename),
    ));
    menu.push(title_regeneration_menu_item(row.title_regenerating));
    let enabled = !row.auto_settle_disabled;
    let choice = |id, label, value: bool| ThreadMenuChild {
        checked: Some(enabled == value),
        ..child(
            id,
            label,
            ThreadMenuAction::Thread {
                action: ThreadAction::AutoSettle { enabled: value },
            },
        )
    };
    menu.push(ThreadMenuItem {
        children: vec![
            choice(Id::AutoSettleEnabled, "Enabled", true),
            choice(Id::AutoSettleDisabled, "Disabled", false),
        ],
        ..entry(Id::AutoSettle, "Auto-settle behavior", None)
    });
    menu.push(ThreadMenuItem {
        destructive: true,
        ..entry(Id::Delete, "Delete", thread(ThreadAction::Delete))
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

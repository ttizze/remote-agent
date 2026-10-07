//! The queued-messages control: the composer's queue list on desktop and the
//! queue sheet on mobile.
use crate::commands::outbox::{Outbox, Request};
use crate::commands::workflows::queue_workflow;
use crate::view::quantity;
use agent_domain::{
    Attachment, AttachmentKind, Command, DispatchMode, RunId, State, context_references,
};

pub const QUEUE_TITLE: &str = "Queued";
pub const REMOVE_QUEUED_MESSAGE_ACCESSIBILITY_LABEL: &str = "Remove queued message";
pub const QUEUE_HELD_NOTICE: &str = "Queue held after restart";
pub const RESUME_QUEUE_LABEL: &str = "Resume queue";
pub const EMPTY_QUEUE_TEXT: &str = "No messages waiting in this queue.";
pub const EDITING_MARKER: &str = "Editing";
/// Thumbnails a compact (mobile) row shows before its overflow count.
pub const COMPACT_THUMBNAIL_LIMIT: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueRowControls {
    pub can_dismiss: bool,
    pub can_edit: bool,
    pub can_move_down: bool,
    pub can_move_up: bool,
    pub can_steer: bool,
    pub dismiss_accessibility_label: String,
    pub display_text: String,
    pub is_editing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueRowControlsInput<'a> {
    pub busy: bool,
    pub can_promote_to_steer: bool,
    pub can_reorder: bool,
    pub index: usize,
    /// This row's message is already open in the composer.
    pub is_editing: bool,
    pub queued_count: usize,
    pub text: &'a str,
}

pub fn queue_row_controls(input: QueueRowControlsInput<'_>) -> QueueRowControls {
    let enabled = !input.busy;
    QueueRowControls {
        can_dismiss: enabled,
        // Reopening the row already in the composer would reload it and lose
        // what was typed since.
        can_edit: enabled && !input.is_editing,
        can_move_down: enabled && input.can_reorder && input.index + 1 < input.queued_count,
        can_move_up: enabled && input.can_reorder && input.index > 0,
        can_steer: enabled && input.can_promote_to_steer && !input.is_editing,
        dismiss_accessibility_label: REMOVE_QUEUED_MESSAGE_ACCESSIBILITY_LABEL.into(),
        display_text: input.text.into(),
        is_editing: input.is_editing,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueThumbnail {
    pub attachment_id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueRowView {
    /// The run id, or the message id of a send the Host has not confirmed.
    pub key: String,
    /// `None` while the send is unconfirmed; such rows have no actions.
    pub run_id: Option<String>,
    pub message_id: String,
    pub pending: bool,
    /// The raw message text; editing loads it into the composer.
    pub text: String,
    /// Context links shown by their labels; image links are dropped when the
    /// row shows thumbnails.
    pub preview: String,
    /// The compact row's title: the text, else "Attachments" or "Queued message".
    pub title: String,
    /// Every image attachment; the compact row shows the first
    /// `COMPACT_THUMBNAIL_LIMIT`.
    pub thumbnails: Vec<QueueThumbnail>,
    /// The compact row's count of attachments without a shown thumbnail.
    pub compact_overflow: Option<String>,
    pub editing: bool,
    pub controls: QueueRowControls,
    pub edit_tooltip: String,
    pub steer_tooltip: String,
    pub actions_accessibility_label: String,
    pub steer_accessibility_label: String,
    pub reorder_accessibility_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueView {
    pub title: String,
    /// Rows including unconfirmed sends: the desktop header count.
    pub count: u32,
    /// Confirmed queued runs: the mobile queue badge.
    pub queued_count: u32,
    pub region_accessibility_label: String,
    pub active_run_id: Option<String>,
    /// Drag handles and arrow-key moves; the mobile sheet also needs two rows.
    pub can_reorder: bool,
    /// Rows offer Steer only when this holds.
    pub can_promote_to_steer: bool,
    /// A queue command from this device is waiting for the Host.
    pub busy: bool,
    pub held_notice: Option<String>,
    pub can_resume: bool,
    pub editing_run_id: Option<String>,
    /// The run the steer-next shortcut promotes.
    pub steer_next_run_id: Option<String>,
    /// The run the edit-latest shortcut opens.
    pub edit_latest_run_id: Option<String>,
    pub rows: Vec<QueueRowView>,
}

/// Keyboard shortcut labels the desktop appends to the row tooltips.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueShortcuts<'a> {
    pub steer: Option<&'a str>,
    pub edit: Option<&'a str>,
}

fn image_thumbnails(attachments: &[Attachment]) -> Vec<QueueThumbnail> {
    attachments
        .iter()
        .filter(|attachment| attachment.kind == AttachmentKind::Image)
        .map(|attachment| QueueThumbnail {
            attachment_id: attachment.id.clone(),
            name: attachment.name.clone(),
        })
        .collect()
}

pub fn queue_preview_text(text: &str, has_thumbnails: bool) -> String {
    let mut preview = String::new();
    let mut cursor = 0;
    for reference in context_references(text) {
        preview.push_str(&text[cursor..reference.start]);
        if !(reference.kind == "image" && has_thumbnails) {
            preview.push_str(&reference.label);
        }
        cursor = reference.end;
    }
    preview.push_str(&text[cursor..]);
    preview.trim().to_owned()
}

fn compact_title(text: &str, attachments: usize) -> String {
    if !text.is_empty() {
        text.into()
    } else if attachments > 0 {
        "Attachments".into()
    } else {
        "Queued message".into()
    }
}

fn compact_overflow(attachments: usize, images: usize) -> Option<String> {
    let shown = images.min(COMPACT_THUMBNAIL_LIMIT);
    let overflow = attachments - shown;
    (overflow > 0).then(|| {
        if shown == 0 {
            overflow.to_string()
        } else {
            format!("+{overflow}")
        }
    })
}

fn with_shortcut(text: &str, shortcut: Option<&str>) -> String {
    match shortcut {
        Some(shortcut) => format!("{text} ({shortcut})"),
        None => text.into(),
    }
}

/// Commands this device sent to the thread that the Host has not confirmed.
pub(crate) fn outbox_commands<'a>(
    state: &State,
    outbox: &'a Outbox,
) -> impl Iterator<Item = &'a Command> {
    let thread = state.thread.as_ref().map(|thread| thread.id.clone());
    outbox
        .entries
        .iter()
        .filter(move |entry| Some(&entry.thread) == thread.as_ref())
        .filter_map(|entry| match &entry.request {
            Request::Dispatch(dispatch) => Some(&dispatch.command),
            Request::Launch(_) => None,
        })
}

struct Row<'a> {
    run: Option<&'a RunId>,
    message_id: String,
    text: &'a str,
    attachments: &'a [Attachment],
}

pub fn queue_view(
    state: &State,
    outbox: &Outbox,
    editing_run: Option<&RunId>,
    shortcuts: QueueShortcuts<'_>,
) -> QueueView {
    let workflow = queue_workflow(state);
    let busy = outbox_commands(state, outbox).any(|command| {
        matches!(
            command,
            Command::ReorderQueued { .. }
                | Command::PromoteToSteer { .. }
                | Command::CancelQueued { .. }
        )
    });
    let resuming =
        outbox_commands(state, outbox).any(|command| matches!(command, Command::ResumeQueue));
    let unconfirmed = outbox_commands(state, outbox).filter_map(|command| match command {
        Command::Send(send)
            if send.mode == DispatchMode::QueueAfterActive && state.message(&send.id).is_none() =>
        {
            Some(Row {
                run: None,
                message_id: send.id.to_string(),
                text: &send.text,
                attachments: &send.attachments,
            })
        }
        _ => None,
    });
    let rows: Vec<Row> = workflow
        .queued
        .iter()
        .map(|queued| Row {
            run: Some(&queued.run),
            message_id: queued.message.to_string(),
            text: &queued.text,
            attachments: &queued.attachments,
        })
        .chain(unconfirmed)
        .collect();
    let queued_count = workflow.queued.len();
    let rows: Vec<QueueRowView> = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            let thumbnails = image_thumbnails(row.attachments);
            let editing = row.run.is_some() && row.run == editing_run;
            let controls = match row.run {
                Some(_) => queue_row_controls(QueueRowControlsInput {
                    busy,
                    can_promote_to_steer: workflow.can_promote_to_steer,
                    can_reorder: workflow.can_reorder,
                    index,
                    is_editing: editing,
                    queued_count,
                    text: row.text,
                }),
                None => QueueRowControls {
                    can_dismiss: false,
                    can_edit: false,
                    can_move_down: false,
                    can_move_up: false,
                    can_steer: false,
                    dismiss_accessibility_label: REMOVE_QUEUED_MESSAGE_ACCESSIBILITY_LABEL.into(),
                    display_text: row.text.into(),
                    is_editing: false,
                },
            };
            let title = compact_title(row.text, row.attachments.len());
            QueueRowView {
                key: row
                    .run
                    .map_or_else(|| row.message_id.clone(), ToString::to_string),
                run_id: row.run.map(ToString::to_string),
                message_id: row.message_id,
                pending: row.run.is_none(),
                text: row.text.into(),
                preview: queue_preview_text(row.text, !thumbnails.is_empty()),
                compact_overflow: compact_overflow(row.attachments.len(), thumbnails.len()),
                thumbnails,
                editing,
                controls,
                edit_tooltip: with_shortcut(
                    "Edit in the composer",
                    shortcuts.edit.filter(|_| index + 1 == queued_count),
                ),
                steer_tooltip: if workflow.active_run.is_none() {
                    "There is no active run to steer".into()
                } else {
                    with_shortcut(
                        "Send as a steer instead",
                        shortcuts.steer.filter(|_| index == 0 && row.run.is_some()),
                    )
                },
                actions_accessibility_label: format!("Actions for queued message {}", index + 1),
                steer_accessibility_label: format!("Steer with message {} now", index + 1),
                reorder_accessibility_label: format!("Reorder {title}"),
                title,
            }
        })
        .collect();
    let count = rows.len();
    QueueView {
        title: QUEUE_TITLE.into(),
        count: count as u32,
        queued_count: queued_count as u32,
        region_accessibility_label: quantity(count, "queued message"),
        active_run_id: workflow.active_run.as_ref().map(ToString::to_string),
        can_reorder: workflow.can_reorder,
        can_promote_to_steer: workflow.can_promote_to_steer,
        busy,
        held_notice: (workflow.held && queued_count > 0).then(|| QUEUE_HELD_NOTICE.into()),
        can_resume: workflow.held && queued_count > 0 && !resuming && !busy,
        editing_run_id: editing_run.map(ToString::to_string),
        steer_next_run_id: workflow
            .queued
            .first()
            .filter(|_| workflow.can_promote_to_steer)
            .map(|queued| queued.run.to_string()),
        edit_latest_run_id: workflow
            .queued
            .last()
            .filter(|_| editing_run.is_none() && !busy)
            .map(|queued| queued.run.to_string()),
        rows,
    }
}

/// A row's measured layout in the queue sheet, before any drag offset.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct QueueRowLayout {
    pub id: String,
    pub y: Option<f64>,
    pub height: Option<f64>,
}

/// Where a moved queued run goes: before another run or at the end.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum QueueDropTarget {
    Before { run_id: String },
    End,
}

/// The insertion anchor while a row is dragged, from the rows' original
/// layout; `None` while any row is unmeasured.
pub fn queue_drag_target(
    rows: &[QueueRowLayout],
    run_id: &str,
    translation_y: f64,
) -> Option<QueueDropTarget> {
    let source = rows.iter().find(|row| row.id == run_id)?;
    let center = |row: &QueueRowLayout| Some(row.y? + row.height? / 2.0);
    if rows
        .iter()
        .any(|row| row.y.is_none() || row.height.is_none())
    {
        return None;
    }
    let dragged = center(source)? + translation_y;
    Some(
        rows.iter()
            .filter(|row| row.id != run_id)
            .find(|row| center(row).is_some_and(|middle| dragged < middle))
            .map_or(QueueDropTarget::End, |row| QueueDropTarget::Before {
                run_id: row.id.clone(),
            }),
    )
}

/// The insertion anchor after a drag, or `None` when the order is unchanged.
pub fn queue_drop_target(
    rows: &[QueueRowLayout],
    run_id: &str,
    translation_y: f64,
) -> Option<QueueDropTarget> {
    let target = queue_drag_target(rows, run_id, translation_y)?;
    let index = rows.iter().position(|row| row.id == run_id)?;
    let unchanged = match rows.get(index + 1) {
        Some(next) => QueueDropTarget::Before {
            run_id: next.id.clone(),
        },
        None => QueueDropTarget::End,
    };
    (target != unchanged).then_some(target)
}

/// The anchor for moving a row one step up or down, as the arrow keys and the
/// row menu do; `None` at either end.
pub fn queue_step_target(run_ids: &[String], run_id: &str, up: bool) -> Option<QueueDropTarget> {
    let index = run_ids.iter().position(|id| id == run_id)?;
    let anchor = if up {
        Some(run_ids.get(index.checked_sub(1)?)?)
    } else {
        run_ids.get(index + 1)?;
        run_ids.get(index + 2)
    };
    Some(
        anchor.map_or(QueueDropTarget::End, |id| QueueDropTarget::Before {
            run_id: id.clone(),
        }),
    )
}

/// The anchor for dropping a dragged row at insertion index `insert_index`
/// (0 to the row count); `None` when it lands next to itself.
pub fn queue_insert_target(
    run_ids: &[String],
    run_id: &str,
    insert_index: usize,
) -> Option<QueueDropTarget> {
    let index = run_ids.iter().position(|id| id == run_id)?;
    if insert_index == index || insert_index == index + 1 {
        return None;
    }
    Some(
        run_ids
            .get(insert_index)
            .map_or(QueueDropTarget::End, |id| QueueDropTarget::Before {
                run_id: id.clone(),
            }),
    )
}

/// The queue order after moving `run_id` to `target`.
pub fn queue_order_after_move(
    run_ids: &[String],
    run_id: &str,
    target: &QueueDropTarget,
) -> Vec<String> {
    let mut order: Vec<String> = run_ids.iter().filter(|id| *id != run_id).cloned().collect();
    let at = match target {
        QueueDropTarget::Before { run_id: before } => order
            .iter()
            .position(|id| id == before)
            .unwrap_or(order.len()),
        QueueDropTarget::End => order.len(),
    };
    order.insert(at, run_id.into());
    order
}

#[cfg(test)]
mod tests;

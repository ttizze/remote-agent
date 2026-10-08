//! Desktop timeline scrolling: following appended calls, restoring an
//! expanded group's position, the follow band at the end, and the minimap.

use super::rows::{TimelineRow, TimelineRowKind};
use std::ops::Range;

/// The visible call of an expanded group and how far into it the view was.
#[derive(Debug, Clone, PartialEq)]
pub struct ExpandedGroupAnchor {
    pub entry_id: String,
    pub offset: f64,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkGroupScrollIndex {
    pub index: u32,
    pub view_offset: f64,
}

/// Restore a visible tool, including a position partway through its expanded output.
pub fn resolve_work_group_scroll_index(
    entry_ids: &[String],
    anchor: Option<&ExpandedGroupAnchor>,
) -> Option<WorkGroupScrollIndex> {
    let anchor = anchor?;
    let index = entry_ids.iter().position(|id| *id == anchor.entry_id)?;
    Some(WorkGroupScrollIndex {
        index: crate::view::count(index),
        view_offset: -anchor.offset,
    })
}

/// Only newly appended calls may follow the end, never status or output updates.
pub fn should_follow_work_group_append(
    previous: &[String],
    entry_ids: &[String],
    distance_from_end: f64,
) -> bool {
    !previous.is_empty()
        && entry_ids.len() > previous.len()
        && distance_from_end <= 1.0
        && previous
            .iter()
            .zip(entry_ids)
            .all(|(previous, next)| previous == next)
}

/// What the list reports about its scroll position.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TimelineEndState {
    pub is_at_end: Option<bool>,
    pub is_near_end: Option<bool>,
    pub content_length: Option<f64>,
    pub scroll: Option<f64>,
    pub scroll_length: Option<f64>,
}

/// The follow re-arm band: a strict at-end flag flickers false for a frame
/// while streaming content grows under the viewport, so following re-arms
/// within this distance of the real content bottom instead.
const TIMELINE_FOLLOW_REARM_THRESHOLD_PX: f64 = 40.0;

pub fn resolve_timeline_is_at_end(state: Option<&TimelineEndState>) -> Option<bool> {
    let state = state?;
    let (Some(content_length), Some(scroll), Some(scroll_length)) =
        (state.content_length, state.scroll, state.scroll_length)
    else {
        return state.is_at_end;
    };
    // The composer inset is part of the content length and hides the same
    // amount of viewport, so it cancels out of the gap below the last row.
    Some(content_length - scroll - scroll_length <= TIMELINE_FOLLOW_REARM_THRESHOLD_PX)
}

const TIMELINE_MINIMAP_PERSISTENT_GUTTER: f64 = 48.0;
const TIMELINE_MINIMAP_HIT_STRIP_LEFT: f64 = 12.0;
const TIMELINE_MINIMAP_HIT_STRIP_MAX_WIDTH: f64 = 40.0;
/// The prev/next buttons' hitbox reaches this far past the strip's left edge.
const TIMELINE_MINIMAP_NAVIGATION_REACH: f64 = 14.0;
/// A single turn is not useful as navigation, and matches the web timeline.
pub const TIMELINE_MINIMAP_MIN_ITEMS: usize = 2;
/// The source rail uses an 8px gap between adjacent turn markers.
const TIMELINE_MINIMAP_MARKER_SPACING: f64 = 8.0;
/// The source rail leaves room for the composer and surrounding timeline chrome.
const TIMELINE_MINIMAP_VIEWPORT_INSET: f64 = 288.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineMinimapItem {
    pub id: String,
    /// The zero-based index in `ThreadView.rows`; the desktop list adds its
    /// history header when converting this to a `ListOffset`.
    pub row_index: usize,
    pub user_text: Option<String>,
    pub assistant_text: Option<String>,
}

/// The compact projection shown when the pointer is over a turn marker.
pub fn resolve_timeline_minimap_preview(
    item: Option<&TimelineMinimapItem>,
) -> Option<TimelineMinimapItem> {
    let item = item?;
    Some(TimelineMinimapItem {
        id: item.id.clone(),
        row_index: item.row_index,
        user_text: compact_timeline_minimap_text(item.user_text.as_deref()),
        assistant_text: compact_timeline_minimap_text(item.assistant_text.as_deref()),
    })
}

fn compact_timeline_minimap_text(text: Option<&str>) -> Option<String> {
    let text = text?.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// Keep one marker for each user turn and attach the final assistant message
/// before the next user turn. This is deliberately projected from the rows
/// that the desktop already renders, so folds, expanded work and history use
/// the same row indexes as the jump target.
pub fn derive_timeline_minimap_items(rows: &[TimelineRow]) -> Vec<TimelineMinimapItem> {
    rows.iter()
        .enumerate()
        .filter_map(|(row_index, row)| {
            let TimelineRowKind::UserMessage(user) = &row.kind else {
                return None;
            };
            let mut assistant_text = None;
            for next in rows.iter().skip(row_index + 1) {
                match &next.kind {
                    TimelineRowKind::UserMessage(_) => break,
                    TimelineRowKind::AssistantMessage(assistant) => {
                        assistant_text = Some(assistant.text.clone())
                    }
                    _ => {}
                }
            }
            Some(TimelineMinimapItem {
                id: row.id.clone(),
                row_index,
                user_text: Some(user.text.clone()),
                assistant_text,
            })
        })
        .collect()
}

/// Resolve the current marker from the actual visible list range. The list's
/// first item is the history header, while minimap row indexes refer only to
/// `ThreadView.rows`.
pub fn resolve_timeline_minimap_current_index_for_visible_range(
    item_row_indices: &[usize],
    visible_range: Range<usize>,
    history_header_items: usize,
) -> Option<usize> {
    if item_row_indices.is_empty() {
        return None;
    }
    let start = visible_range.start.saturating_sub(history_header_items);
    let end = visible_range
        .end
        .saturating_sub(history_header_items)
        .max(start.saturating_add(1));
    let mut preceding = None;
    for (index, row_index) in item_row_indices.iter().copied().enumerate() {
        if row_index < end && row_index >= start {
            return Some(index);
        }
        if row_index <= start {
            preceding = Some(index);
        }
    }
    preceding.or(Some(0))
}

/// The native equivalent of the source `min(…, calc(100vh - 18rem))` rail.
/// The caller supplies the measured viewport height instead of a CSS string.
pub fn resolve_timeline_minimap_height(item_count: usize, viewport_height: f64) -> f64 {
    if item_count == 0 {
        return 0.0;
    }
    let natural =
        ((item_count.saturating_sub(1)) as f64 * TIMELINE_MINIMAP_MARKER_SPACING).max(1.0);
    let available = if viewport_height.is_finite() && viewport_height > 0.0 {
        (viewport_height - TIMELINE_MINIMAP_VIEWPORT_INSET).max(1.0)
    } else {
        natural
    };
    natural.min(available)
}

pub fn resolve_timeline_minimap_top_percent(index: usize, item_count: usize) -> f64 {
    if item_count <= 1 {
        return 0.0;
    }
    index.min(item_count - 1) as f64 / (item_count - 1) as f64 * 100.0
}

pub fn resolve_timeline_minimap_index_from_pointer(
    item_count: usize,
    rail_top: f64,
    rail_height: f64,
    pointer_y: f64,
) -> Option<usize> {
    if item_count == 0 || rail_height <= 0.0 {
        return None;
    }
    if item_count == 1 {
        return Some(0);
    }
    let progress = ((pointer_y - rail_top) / rail_height).clamp(0.0, 1.0);
    // JavaScript `Math.round` rounds halves up.
    let index = (progress * (item_count - 1) as f64 + 0.5).floor();
    Some((index.max(0.0) as usize).min(item_count - 1))
}

/// A minimap item's measured position; `None` while it is not measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinimapItemBounds {
    pub top: Option<f64>,
    pub height: Option<f64>,
}

/// The first marker in view, or the last one above the viewport.
pub fn resolve_timeline_minimap_current_index(
    scroll_top: f64,
    scroll_bottom: f64,
    item_bounds: &[MinimapItemBounds],
) -> Option<usize> {
    let mut preceding = None;
    for (index, item) in item_bounds.iter().enumerate() {
        let Some(top) = item.top else {
            continue;
        };
        if top < scroll_bottom && top + item.height.unwrap_or(1.0).max(1.0) > scroll_top {
            return Some(index);
        }
        if top <= scroll_top {
            preceding = Some(index);
        }
    }
    preceding
}

/// The gutter beside the centered content column; `content_width` is the
/// rendered column width, which follows the chat width setting.
fn resolve_timeline_side_gutter(viewport_width: f64, content_width: f64) -> f64 {
    if !viewport_width.is_finite() || viewport_width <= 0.0 || !content_width.is_finite() {
        return 0.0;
    }
    ((viewport_width - viewport_width.min(content_width)) / 2.0).max(0.0)
}

pub fn resolve_timeline_minimap_has_persistent_gutter(
    viewport_width: f64,
    content_width: f64,
) -> bool {
    resolve_timeline_side_gutter(viewport_width, content_width)
        >= TIMELINE_MINIMAP_PERSISTENT_GUTTER
}

/// The hover strip never reaches past the gutter into the message text; 0
/// disables it.
pub fn resolve_timeline_minimap_hit_strip_width(viewport_width: f64, content_width: f64) -> f64 {
    let side_gutter = resolve_timeline_side_gutter(viewport_width, content_width);
    (side_gutter.floor() - TIMELINE_MINIMAP_HIT_STRIP_LEFT)
        .clamp(0.0, TIMELINE_MINIMAP_HIT_STRIP_MAX_WIDTH)
}

/// The prev/next buttons take pointer input only when the gutter holds them;
/// keyboard focus still reaches them.
pub fn resolve_timeline_minimap_navigation_interactive(collapsed_width: f64) -> bool {
    collapsed_width >= TIMELINE_MINIMAP_NAVIGATION_REACH
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::timeline::rows::{
        AssistantMessageRow, TimelineRow, UserMessageDecorations, UserMessageRow,
    };
    use agent_domain::MessageId;

    fn timeline_row(id: &str, kind: TimelineRowKind) -> TimelineRow {
        TimelineRow {
            id: id.into(),
            created_at: None,
            continues_work_log: false,
            kind,
        }
    }

    fn user_row(id: &str, text: &str) -> TimelineRow {
        timeline_row(
            id,
            TimelineRowKind::UserMessage(Box::new(UserMessageRow {
                message: MessageId::new(id).unwrap(),
                text: text.into(),
                attachments: vec![],
                context: None,
                decorations: UserMessageDecorations {
                    attribution: None,
                    intent: None,
                    collapsible: false,
                    status_chip: None,
                    copy: None,
                    edit_from_here: None,
                },
                badge: None,
            })),
        )
    }

    fn assistant_row(id: &str, text: &str) -> TimelineRow {
        timeline_row(
            id,
            TimelineRowKind::AssistantMessage(Box::new(AssistantMessageRow {
                message: MessageId::new(id).unwrap(),
                text: text.into(),
                attachments: vec![],
                streaming: false,
                preserve_line_breaks: false,
                duration_start: None,
                meta: None,
                changed_files: None,
            })),
        )
    }

    #[test]
    fn rearms_follow_within_the_band_above_the_content_bottom() {
        let state = |is_at_end, scroll| TimelineEndState {
            is_at_end,
            content_length: Some(1_000.0),
            scroll,
            scroll_length: Some(500.0),
            ..TimelineEndState::default()
        };
        assert_eq!(resolve_timeline_is_at_end(None), None);
        assert_eq!(
            resolve_timeline_is_at_end(Some(&state(Some(false), Some(460.0)))),
            Some(true)
        );
        assert_eq!(
            resolve_timeline_is_at_end(Some(&state(Some(true), Some(459.0)))),
            Some(false)
        );
        assert_eq!(
            resolve_timeline_is_at_end(Some(&state(Some(true), None))),
            Some(true)
        );
    }

    #[test]
    fn maps_minimap_markers_pointers_and_gutters() {
        assert_eq!(resolve_timeline_minimap_top_percent(0, 1), 0.0);
        assert_eq!(resolve_timeline_minimap_top_percent(5, 3), 100.0);
        assert_eq!(resolve_timeline_minimap_top_percent(1, 3), 50.0);
        assert_eq!(
            resolve_timeline_minimap_index_from_pointer(0, 0.0, 10.0, 0.0),
            None
        );
        assert_eq!(
            resolve_timeline_minimap_index_from_pointer(3, 100.0, 100.0, 150.0),
            Some(1)
        );
        assert_eq!(
            resolve_timeline_minimap_index_from_pointer(3, 100.0, 100.0, 500.0),
            Some(2)
        );
        let bounds = |top, height| MinimapItemBounds { top, height };
        assert_eq!(
            resolve_timeline_minimap_current_index(
                100.0,
                200.0,
                &[
                    bounds(Some(0.0), Some(10.0)),
                    bounds(None, None),
                    bounds(Some(150.0), None)
                ]
            ),
            Some(2)
        );
        assert_eq!(
            resolve_timeline_minimap_current_index(100.0, 200.0, &[bounds(Some(0.0), Some(10.0))]),
            Some(0)
        );
        assert!(!resolve_timeline_minimap_has_persistent_gutter(
            1_000.0, 920.0
        ));
        assert!(resolve_timeline_minimap_has_persistent_gutter(
            1_000.0, 800.0
        ));
        assert_eq!(
            resolve_timeline_minimap_hit_strip_width(1_000.0, 800.0),
            40.0
        );
        assert_eq!(
            resolve_timeline_minimap_hit_strip_width(1_000.0, 960.0),
            8.0
        );
        assert_eq!(
            resolve_timeline_minimap_hit_strip_width(f64::NAN, 960.0),
            0.0
        );
        assert!(!resolve_timeline_minimap_navigation_interactive(8.0));
        assert!(resolve_timeline_minimap_navigation_interactive(14.0));
    }

    #[test]
    fn projects_user_turns_and_the_last_answer_before_the_next_turn() {
        let rows = vec![
            user_row("user-1", "  Inspect\n this  "),
            assistant_row("assistant-1", "Working"),
            assistant_row("assistant-2", " Done\t now "),
            timeline_row("fold", TimelineRowKind::Working),
            user_row("user-2", "Next"),
            assistant_row("assistant-3", "Second answer"),
        ];
        let items = derive_timeline_minimap_items(&rows);
        assert_eq!(
            items,
            vec![
                TimelineMinimapItem {
                    id: "user-1".into(),
                    row_index: 0,
                    user_text: Some("  Inspect\n this  ".into()),
                    assistant_text: Some(" Done\t now ".into()),
                },
                TimelineMinimapItem {
                    id: "user-2".into(),
                    row_index: 4,
                    user_text: Some("Next".into()),
                    assistant_text: Some("Second answer".into()),
                },
            ]
        );
        assert_eq!(
            resolve_timeline_minimap_preview(items.first()),
            Some(TimelineMinimapItem {
                id: "user-1".into(),
                row_index: 0,
                user_text: Some("Inspect this".into()),
                assistant_text: Some("Done now".into()),
            })
        );
        assert_eq!(resolve_timeline_minimap_preview(None), None);
    }

    #[test]
    fn maps_visible_list_ranges_after_the_history_header() {
        let rows = [0, 4, 10];
        assert_eq!(
            resolve_timeline_minimap_current_index_for_visible_range(&rows, 1..2, 1),
            Some(0)
        );
        assert_eq!(
            resolve_timeline_minimap_current_index_for_visible_range(&rows, 5..8, 1),
            Some(1)
        );
        assert_eq!(
            resolve_timeline_minimap_current_index_for_visible_range(&rows, 11..12, 1),
            Some(2)
        );
        assert_eq!(resolve_timeline_minimap_height(5, 320.0), 32.0);
        assert_eq!(resolve_timeline_minimap_height(100, 320.0), 32.0);
    }
}

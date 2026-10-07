//! Desktop timeline scrolling: following appended calls, restoring an
//! expanded group's position, the follow band at the end, and the minimap.

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
}

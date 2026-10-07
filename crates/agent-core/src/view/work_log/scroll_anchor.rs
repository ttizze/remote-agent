//! The row a scrolled tool group returns to after it re-renders.

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkGroupScrollAnchor {
    pub row_id: String,
    pub offset_within_row: f64,
    pub scroll_offset: f64,
}

/// Saves the row at the current offset, read from measured row tops; a
/// virtualizer's cached visible range can lag a fling.
pub fn resolve_work_group_scroll_anchor(
    row_ids: &[String],
    scroll: f64,
    position_at_index: impl Fn(usize) -> Option<f64>,
) -> Option<WorkGroupScrollAnchor> {
    if row_ids.is_empty() || !scroll.is_finite() {
        return None;
    }
    let scroll_offset = scroll.max(0.0);
    let measured = |index: usize| position_at_index(index).filter(|top| top.is_finite());
    let mut low: isize = 0;
    let mut high = row_ids.len() as isize - 1;
    let mut index = 0;
    let mut row_top = measured(0)?;

    while low <= high {
        let middle = (low + high) / 2;
        let top = measured(middle as usize)?;
        if top <= scroll_offset {
            index = middle as usize;
            row_top = top;
            low = middle + 1;
        } else {
            high = middle - 1;
        }
    }

    Some(WorkGroupScrollAnchor {
        row_id: row_ids[index].clone(),
        offset_within_row: (scroll_offset - row_top).max(0.0),
        scroll_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(count: usize) -> Vec<String> {
        (0..count).map(|index| format!("call-{index}")).collect()
    }

    fn anchor(row_id: &str, offset_within_row: f64, scroll_offset: f64) -> WorkGroupScrollAnchor {
        WorkGroupScrollAnchor {
            row_id: row_id.into(),
            offset_within_row,
            scroll_offset,
        }
    }

    fn uniform(index: usize) -> Option<f64> {
        Some(index as f64 * 33.0)
    }

    #[test]
    fn returns_to_the_top_after_a_fast_flick_even_when_the_cached_start_is_five_rows_behind() {
        assert_eq!(
            resolve_work_group_scroll_anchor(&rows(18), 0.0, uniform),
            Some(anchor("call-0", 0.0, 0.0))
        );
    }

    #[test]
    fn uses_the_measured_position_at_scroll_rather_than_cached_row_start() {
        for (scroll, row_id, offset_within_row) in [
            (302.0, "call-9", 5.0),
            (40.0, "call-1", 7.0),
            (165.0, "call-5", 0.0),
        ] {
            assert_eq!(
                resolve_work_group_scroll_anchor(&rows(18), scroll, uniform),
                Some(anchor(row_id, offset_within_row, scroll))
            );
        }
    }

    #[test]
    fn preserves_an_offset_within_expanded_output() {
        let positions = [0.0, 33.0, 600.0];
        let ids = ["first", "output", "last"].map(String::from);
        assert_eq!(
            resolve_work_group_scroll_anchor(&ids, 153.0, |index| positions.get(index).copied()),
            Some(anchor("output", 120.0, 153.0))
        );
    }

    #[test]
    fn does_not_preserve_overscroll_before_the_first_row() {
        assert_eq!(
            resolve_work_group_scroll_anchor(&rows(18), -20.0, uniform),
            Some(anchor("call-0", 0.0, 0.0))
        );
    }

    #[test]
    fn skips_incomplete_layout_snapshots_instead_of_saving_an_invalid_anchor() {
        assert_eq!(resolve_work_group_scroll_anchor(&[], 0.0, uniform), None);
        assert_eq!(
            resolve_work_group_scroll_anchor(&rows(18), f64::NAN, uniform),
            None
        );
        assert_eq!(
            resolve_work_group_scroll_anchor(&rows(18), 165.0, |_| None),
            None
        );
        assert_eq!(
            resolve_work_group_scroll_anchor(&rows(18), 165.0, |index| Some(if index == 0 {
                0.0
            } else {
                f64::NAN
            })),
            None
        );
    }
}

//! Fitting the composer footer's controls: trailing blocks (traits, then
//! the mode controls) move into the "More composer controls" menu until the
//! rest fits; below the model picker's minimum width the cluster hides.

/// The widths the footer measured, in pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct FooterMeasurement {
    pub gap: f32,
    /// The model picker (and anything else that never moves) at its natural width.
    pub natural_fixed_width: f32,
    /// The same at the picker's smallest readable width.
    pub minimum_fixed_width: f32,
    /// The blocks that can move into the menu, in order.
    pub block_widths: Vec<f32>,
    /// The menu's trigger.
    pub overflow_width: f32,
}

/// How many trailing blocks the menu holds, and whether the cluster shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FooterLayout {
    pub hidden_count: usize,
    pub visible: bool,
}

/// Restoring a block needs a pixel of slack so a width that jitters by a
/// fraction of a pixel cannot flip the layout back and forth.
const SLACK: f32 = 1.;

fn width(input: &FooterMeasurement, hidden: usize, fixed: f32) -> f32 {
    let visible = input.block_widths.len() - hidden;
    fixed
        + input.block_widths[..visible].iter().sum::<f32>()
        + if hidden > 0 { input.overflow_width } else { 0. }
        + input.gap * (visible + usize::from(hidden > 0)) as f32
}

/// The width the controls take with nothing in the menu.
pub fn natural_footer_width(input: &FooterMeasurement) -> f32 {
    width(input, 0, input.natural_fixed_width)
}

/// Moves trailing blocks into the menu until the rest fits `host_width`;
/// `previous` keeps a restored block from flickering at the boundary.
pub fn resolve_footer_layout(
    input: &FooterMeasurement,
    host_width: f32,
    previous: Option<FooterLayout>,
) -> FooterLayout {
    let blocks = input.block_widths.len();
    let previous_hidden = previous.map_or(0, |previous| previous.hidden_count.min(blocks));
    let mut hidden = 0;
    while hidden < blocks
        && width(input, hidden, input.natural_fixed_width)
            > host_width - if hidden < previous_hidden { SLACK } else { 0. }
    {
        hidden += 1;
    }
    let minimum = width(input, hidden, input.minimum_fixed_width);
    let visible = if previous.is_some_and(|previous| !previous.visible) {
        minimum <= host_width - SLACK
    } else {
        minimum <= host_width
    };
    FooterLayout {
        hidden_count: hidden,
        visible,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Picker 140 natural / 96 minimum, plus a 9px separator. Traits 60,
    // mode 140, overflow 24, gap 4.
    fn base() -> FooterMeasurement {
        FooterMeasurement {
            gap: 4.,
            natural_fixed_width: 149.,
            minimum_fixed_width: 105.,
            block_widths: vec![60., 140.],
            overflow_width: 24.,
        }
    }

    fn layout(hidden_count: usize, visible: bool) -> FooterLayout {
        FooterLayout {
            hidden_count,
            visible,
        }
    }

    #[test]
    fn shows_everything_when_the_host_has_room() {
        assert_eq!(resolve_footer_layout(&base(), 357., None), layout(0, true));
        assert_eq!(natural_footer_width(&base()), 357.);
    }

    #[test]
    fn moves_trailing_blocks_into_the_overflow_menu_until_the_rest_fits() {
        assert_eq!(resolve_footer_layout(&base(), 356., None), layout(1, true));
        assert_eq!(resolve_footer_layout(&base(), 241., None), layout(1, true));
        assert_eq!(resolve_footer_layout(&base(), 240., None), layout(2, true));
    }

    #[test]
    fn shrinks_the_picker_after_moving_every_trailing_block_into_overflow() {
        assert_eq!(resolve_footer_layout(&base(), 176., None), layout(2, true));
        assert_eq!(resolve_footer_layout(&base(), 133., None), layout(2, true));
    }

    #[test]
    fn hides_the_whole_cluster_below_the_pickers_minimum_readable_width() {
        assert_eq!(resolve_footer_layout(&base(), 132., None), layout(2, false));
        assert_eq!(resolve_footer_layout(&base(), 0., None), layout(2, false));
    }

    #[test]
    fn uses_the_same_thresholds_while_shrinking_and_growing() {
        assert_eq!(resolve_footer_layout(&base(), 240., None), layout(2, true));
        assert_eq!(resolve_footer_layout(&base(), 241., None), layout(1, true));
    }

    #[test]
    fn supports_a_single_leading_control_without_overflow_blocks() {
        let single = FooterMeasurement {
            natural_fixed_width: 140.,
            minimum_fixed_width: 140.,
            block_widths: vec![],
            overflow_width: 0.,
            ..base()
        };
        assert_eq!(resolve_footer_layout(&single, 139., None), layout(0, false));
    }

    #[test]
    fn keeps_a_block_in_overflow_when_re_showing_it_would_leave_no_slack() {
        assert_eq!(
            resolve_footer_layout(&base(), 357., Some(layout(1, true))),
            layout(1, true)
        );
    }

    #[test]
    fn re_shows_a_block_once_the_host_clears_the_slack_margin() {
        assert_eq!(
            resolve_footer_layout(&base(), 358., Some(layout(1, true))),
            layout(0, true)
        );
    }

    #[test]
    fn restores_the_blocks_that_fit_when_the_full_cluster_has_no_slack() {
        let partly = resolve_footer_layout(&base(), 357., Some(layout(2, true)));
        assert_eq!(partly, layout(1, true));
        assert_eq!(resolve_footer_layout(&base(), 357., Some(partly)), partly);
        assert_eq!(
            resolve_footer_layout(&base(), 358., Some(partly)),
            layout(0, true)
        );
    }

    #[test]
    fn requires_slack_before_partially_restoring_a_cluster() {
        let previous = layout(2, true);
        assert_eq!(
            resolve_footer_layout(&base(), 241., Some(previous)),
            previous
        );
        assert_eq!(
            resolve_footer_layout(&base(), 242., Some(previous)),
            layout(1, true)
        );
    }

    #[test]
    fn settles_when_the_measured_picker_width_jitters_below_a_pixel() {
        let mut jittered = base();
        jittered.natural_fixed_width = 149.4;
        let first = resolve_footer_layout(&jittered, 357., None);
        assert_eq!(first, layout(1, true));
        jittered.natural_fixed_width = 149.;
        assert_eq!(resolve_footer_layout(&jittered, 357., Some(first)), first);
    }
}

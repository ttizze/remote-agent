//! Whether a conversation follows its end as a turn streams in, as T3 Code's
//! timeline does (`liveFollowEnabled`, `maintainScrollAtEnd`): the reader
//! scrolling toward the start, however little, by wheel, trackpad or drag,
//! stops following until they come back to the end.

/// Distance beyond which the scroll-to-bottom button appears.
const SHOW_END_BUTTON_DISTANCE: f32 = 40.;

/// Where a scrolled conversation is, in points along its axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollPosition {
    /// How far the content is scrolled from its start.
    pub offset: f32,
    pub viewport: f32,
    pub content: f32,
}

impl ScrollPosition {
    fn end_offset(self) -> f32 {
        (self.content - self.viewport).max(0.)
    }

    fn distance_from_end(self) -> f32 {
        self.content - self.offset - self.viewport
    }
}

/// Whether the conversation follows its end once it moved from `previous` to
/// `now`. While the content can scroll, moving toward the start stops following,
/// even during a resize. Content that fits, resize clamping and elastic return
/// to the end keep following. Once stopped, reaching the end resumes following.
/// Moving closer alone does not: row measurements can also move the offset
/// toward the end. Without a `previous` position, a following conversation
/// was at its end.
pub fn follows_end(following: bool, previous: Option<ScrollPosition>, now: ScrollPosition) -> bool {
    if now.content <= now.viewport || now.distance_from_end() <= 0.5 {
        return true;
    }
    if !following {
        return false;
    }
    let previous = previous.unwrap_or(ScrollPosition {
        offset: now.end_offset(),
        ..now
    });
    // Compare within both scrollable ranges: resize clamping and elastic
    // return stay at the end, but a simultaneous upward scroll leaves it.
    now.offset
        >= previous
            .offset
            .min(previous.end_offset())
            .min(now.end_offset())
            - 0.5
}

/// Whether the reader is far enough from the end to show the scroll-to-bottom
/// button, as T3 Code does.
pub fn away_from_end(position: ScrollPosition) -> bool {
    position.distance_from_end() > SHOW_END_BUTTON_DISTANCE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(offset: f32, content: f32) -> ScrollPosition {
        ScrollPosition {
            offset,
            viewport: 500.,
            content,
        }
    }

    #[test]
    fn a_small_scroll_up_while_a_turn_streams_stops_following_until_the_end() {
        // At the end, following.
        let end = at(1500., 2000.);
        assert!(follows_end(true, None, end));
        // Four lines up stay within the button's threshold, yet stop following.
        let up = at(1470., 2000.);
        assert!(!follows_end(true, Some(end), up));
        // The turn streams on: the content grows under the reader, who stays.
        let grown = at(1470., 2400.);
        assert!(!follows_end(false, Some(up), grown));
        // Scrolling down short of the end keeps not following.
        let down = at(1700., 2400.);
        assert!(!follows_end(false, Some(grown), down));
        // Back at the end, it follows again.
        let back = at(1900., 2400.);
        assert!(follows_end(false, Some(down), back));
    }

    #[test]
    fn updates_near_the_end_do_not_resume_following_after_scrolling_up() {
        let up = at(1470., 2000.);
        assert!(!follows_end(true, Some(at(1500., 2000.)), up));
        // Repeated scroll reports, streaming and self-sizing must leave the
        // reader alone, even while they are still near the end.
        for now in [
            up,
            at(1470.5, 2000.),
            at(1470.5, 1999.),
            // An earlier row grows while a later row shrinks: the visible
            // content stays, but its offset rises and the end comes closer.
            at(1480., 1990.),
            at(1470., 2001.),
            at(1460., 1990.),
            at(1480., 2010.),
            at(1480., 2009.5),
            ScrollPosition {
                viewport: 510.,
                ..up
            },
            ScrollPosition {
                offset: 1480.,
                viewport: 490.,
                ..up
            },
        ] {
            assert!(!follows_end(false, Some(up), now), "{now:?}");
        }
        // Moving down short of the end also keeps the reader in control.
        assert!(!follows_end(false, Some(up), at(1480., 2000.)));
        assert!(follows_end(false, Some(up), at(1500., 2000.)));
        // Reaching the end during an update resumes following too.
        assert!(follows_end(false, Some(up), at(1510., 2010.)));
        assert!(follows_end(
            false,
            Some(up),
            ScrollPosition {
                offset: 1510.,
                viewport: 490.,
                ..up
            }
        ));
    }

    #[test]
    fn scrolling_up_stops_following_even_when_rows_shrink_in_the_same_update() {
        let end = at(1500., 2000.);
        let up = at(1470., 1990.);
        assert!(!follows_end(true, Some(end), up));
        assert!(!follows_end(false, Some(up), up));
        assert!(!follows_end(false, Some(up), at(1470., 1991.)));
        assert!(!follows_end(false, Some(up), at(1480., 1991.)));
        assert!(follows_end(false, Some(up), at(1491., 1991.)));
        // Resizing at the end and bouncing while content grows are not
        // upward departures from the end.
        assert!(follows_end(true, Some(end), at(1490., 1990.)));
        assert!(follows_end(true, Some(at(1530., 2000.)), at(1500., 2010.)));
    }

    #[test]
    fn following_scrolls_and_shrinking_content_do_not_stop_following() {
        // Following moves toward the end as the content grows.
        assert!(follows_end(true, Some(at(1500., 2000.)), at(1900., 2400.)));
        // A fold closing near the end pulls the offset back with the content.
        assert!(follows_end(true, Some(at(1900., 2400.)), at(1500., 2000.)));
        // Content that fits the viewport cannot leave the end, including
        // the boundary where its height equals the viewport.
        for content in [300., 500.] {
            assert!(follows_end(true, Some(at(10., content)), at(0., content)));
            assert!(follows_end(false, Some(at(1470., 2000.)), at(0., content)));
        }
    }

    #[test]
    fn bouncing_back_to_the_end_keeps_following_until_the_reader_leaves_it() {
        let end = at(1500., 2000.);
        let overscroll = at(1530., 2000.);
        for following in [false, true] {
            assert!(follows_end(following, Some(overscroll), end));
            assert!(follows_end(following, Some(overscroll), at(1499.5, 2000.)));
            assert!(!follows_end(following, Some(overscroll), at(1470., 2000.)));
        }
        assert!(follows_end(false, Some(at(1470., 2000.)), overscroll));
        assert!(follows_end(true, Some(end), at(1510., 2010.)));
    }

    #[test]
    fn the_way_back_shows_only_beyond_the_button_threshold() {
        assert!(!away_from_end(at(1500., 2000.)));
        assert!(!away_from_end(at(1460., 2000.)));
        assert!(away_from_end(at(1459., 2000.)));
        // Content that fits has no end to be away from.
        assert!(!away_from_end(at(0., 300.)));
    }

    #[test]
    fn the_first_scroll_of_a_following_conversation_leaves_its_end() {
        // Nothing scrolled the list since it opened at its end.
        assert!(!follows_end(true, None, at(1300., 2000.)));
        assert!(!follows_end(true, None, at(1490., 2000.)));
        assert!(follows_end(true, None, at(1500., 2000.)));
        // A reader who left the end keeps their place.
        assert!(!follows_end(false, None, at(1300., 2000.)));
    }

    proptest::proptest! {
        #[test]
        fn leaving_the_end_during_row_shrink_stops_following(
            content in 1000f32..8000.,
            shrink in 0f32..100.,
            gap in 1f32..40.,
        ) {
            let before = at(content - 500., content);
            let resized_end = at(content - shrink - 500., content - shrink);
            let up = at(resized_end.offset - gap, resized_end.content);
            proptest::prop_assert!(!follows_end(true, Some(before), up));
            proptest::prop_assert!(follows_end(true, Some(before), resized_end));
        }

        #[test]
        fn the_end_follows_and_an_upward_scroll_never_does(
            following in proptest::bool::ANY,
            offset in 0f32..5000.,
            delta in 1f32..2000.,
            content in 600f32..8000.,
        ) {
            let end = at(content - 500., content);
            // A real upward scroll leaves the end; elastic bounce at the
            // end itself must keep following instead.
            let now = at(offset.min(end.offset - 1.).max(0.), content);
            let below = at(now.offset + delta, content);
            proptest::prop_assert!(follows_end(following, None, end));
            proptest::prop_assert!(follows_end(following, Some(at(end.offset + delta, content)), end));
            proptest::prop_assert!(!follows_end(false, None, now));
            // No previous event while following means it opened at the end.
            // Even near the end, the first upward scroll leaves it.
            let first_up = at((end.offset - delta).max(0.), content);
            proptest::prop_assert!(!follows_end(true, None, first_up));
            proptest::prop_assert!(!follows_end(following, Some(below), now));
        }
    }
}

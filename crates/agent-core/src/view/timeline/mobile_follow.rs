//! When the mobile feed and its tool groups follow new content, and where a
//! re-rendered tool group resumes scrolling.
use agent_domain::MessageId;

/// Where a tool group's scrolling stood: its visible row and the offset in it.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkGroupScrollPosition {
    pub row_id: String,
    pub offset_within_row: f64,
    pub scroll_offset: f64,
    pub content_height: f64,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct WorkGroupInitialScroll {
    pub index: u32,
    pub view_offset: f64,
}

/// Restores the visible row and its detail offset rather than stale pixels.
pub fn work_group_initial_scroll(
    row_ids: &[String],
    position: Option<&WorkGroupScrollPosition>,
) -> Option<WorkGroupInitialScroll> {
    let position = position?;
    let index = row_ids.iter().position(|id| id == &position.row_id)?;
    Some(WorkGroupInitialScroll {
        index: crate::view::count(index),
        view_offset: -position.offset_within_row,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkGroupAppend {
    pub previous_rows: Vec<String>,
    pub rows: Vec<String>,
    pub previous_content_height: f64,
    pub content_height: f64,
    pub viewport_height: f64,
    pub scroll_offset: f64,
    pub details_changed: bool,
    pub user_scrolling: bool,
}

/// Follows new calls appended at the end, never a detail toggle, a growing
/// result or the reader's own scrolling.
pub fn should_follow_work_group_append(append: &WorkGroupAppend) -> bool {
    !append.details_changed
        && !append.user_scrolling
        && append.content_height > append.previous_content_height
        && append.rows.len() > append.previous_rows.len()
        && append
            .previous_rows
            .iter()
            .zip(&append.rows)
            .all(|(previous, row)| previous == row)
        && append.previous_content_height - append.viewport_height - append.scroll_offset <= 1.0
}

/// The message the feed keeps anchored after a submission: only the first
/// message of a thread that has not started a turn.
pub fn feed_submission_anchor(
    current_anchor: Option<&MessageId>,
    submitted: &MessageId,
    has_started_turn: bool,
    has_user_message: bool,
    queued_message_count: usize,
) -> Option<MessageId> {
    if has_started_turn || has_user_message {
        return None;
    }
    if let Some(anchor) = current_anchor {
        return Some(anchor.clone());
    }
    (queued_message_count == 0).then(|| submitted.clone())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum LiveFollowEvent {
    Reset,
    UserScrollBegin,
    UserScrollEnd {
        at_end: bool,
        user_scroll_session_active: bool,
    },
    Scroll {
        at_end: bool,
        user_scroll_session_active: bool,
    },
    DisclosureSettled {
        at_end: bool,
        user_scroll_session_active: bool,
    },
}

/// Whether the feed keeps following its end after `event`.
pub fn feed_live_follow(current: bool, event: LiveFollowEvent) -> bool {
    match event {
        LiveFollowEvent::Reset => true,
        LiveFollowEvent::UserScrollBegin => false,
        LiveFollowEvent::UserScrollEnd {
            at_end,
            user_scroll_session_active,
        } => {
            if user_scroll_session_active {
                at_end
            } else {
                current
            }
        }
        LiveFollowEvent::DisclosureSettled {
            at_end,
            user_scroll_session_active,
        } => !user_scroll_session_active && at_end,
        LiveFollowEvent::Scroll {
            at_end,
            user_scroll_session_active,
        } => !user_scroll_session_active && (at_end || current),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn position() -> WorkGroupScrollPosition {
        WorkGroupScrollPosition {
            row_id: "read-output".into(),
            offset_within_row: 80.0,
            scroll_offset: 600.0,
            content_height: 1_000.0,
        }
    }

    #[test]
    fn restores_the_visible_row_and_its_detail_offset_rather_than_stale_absolute_pixels() {
        let before = ids(&["first", "read-output", "last"]);
        let after = ids(&["older", "first", "read-output", "last"]);
        assert_eq!(
            work_group_initial_scroll(&before, Some(&position())),
            Some(WorkGroupInitialScroll {
                index: 1,
                view_offset: -80.0
            })
        );
        assert_eq!(
            work_group_initial_scroll(&after, Some(&position())),
            Some(WorkGroupInitialScroll {
                index: 2,
                view_offset: -80.0
            })
        );
    }

    #[test]
    fn starts_normally_when_the_saved_row_no_longer_belongs_to_the_group() {
        assert_eq!(
            work_group_initial_scroll(&ids(&["other"]), Some(&position())),
            None
        );
        assert_eq!(
            work_group_initial_scroll(&ids(&["read-output"]), None),
            None
        );
    }

    fn previous_rows() -> Vec<String> {
        (0..10).map(|index| format!("call-{index}")).collect()
    }

    fn at_end() -> WorkGroupAppend {
        let mut rows = previous_rows();
        rows.push("new-call".into());
        WorkGroupAppend {
            previous_rows: previous_rows(),
            rows,
            previous_content_height: 289.0,
            content_height: 318.0,
            viewport_height: 256.0,
            scroll_offset: 33.0,
            details_changed: false,
            user_scrolling: false,
        }
    }

    #[test]
    fn follows_a_new_call_when_the_reader_was_at_the_end() {
        assert!(should_follow_work_group_append(&at_end()));
    }

    #[test]
    fn follows_the_first_overflowing_append_as_a_short_group_reaches_its_height_cap() {
        assert!(should_follow_work_group_append(&WorkGroupAppend {
            previous_rows: previous_rows()[..8].to_vec(),
            rows: previous_rows()[..9].to_vec(),
            previous_content_height: 231.0,
            content_height: 260.0,
            viewport_height: 231.0,
            scroll_offset: 0.0,
            ..at_end()
        }));
    }

    #[rstest]
    #[case::reading_earlier_calls(WorkGroupAppend { scroll_offset: 20.0, ..at_end() })]
    #[case::dragging_before_leaving_the_edge(WorkGroupAppend { user_scrolling: true, ..at_end() })]
    #[case::opening_detail_during_an_append(WorkGroupAppend { details_changed: true, ..at_end() })]
    #[case::streaming_a_result(WorkGroupAppend {
        rows: previous_rows(),
        content_height: 600.0,
        ..at_end()
    })]
    #[case::updating_a_lifecycle_label(WorkGroupAppend {
        rows: previous_rows(),
        content_height: 289.0,
        ..at_end()
    })]
    #[case::prepending_old_calls(WorkGroupAppend {
        rows: [vec!["older".to_owned()], previous_rows()].concat(),
        ..at_end()
    })]
    #[case::replacing_a_call_while_appending(WorkGroupAppend {
        rows: [vec!["replacement".to_owned()], at_end().rows[1..].to_vec()].concat(),
        ..at_end()
    })]
    fn does_not_steal_the_readers_position(#[case] append: WorkGroupAppend) {
        assert!(!should_follow_work_group_append(&append));
    }

    fn message(id: &str) -> MessageId {
        MessageId::new(id).unwrap()
    }

    #[test]
    fn anchors_the_first_user_message_in_a_thread() {
        assert_eq!(
            feed_submission_anchor(None, &message("first-message"), false, false, 0),
            Some(message("first-message"))
        );
    }

    #[test]
    fn preserves_the_first_message_anchor_when_another_message_is_queued() {
        assert_eq!(
            feed_submission_anchor(
                Some(&message("first-message")),
                &message("second-message"),
                false,
                false,
                1
            ),
            Some(message("first-message"))
        );
    }

    #[test]
    fn preserves_the_first_message_anchor_after_its_outbox_entry_drains() {
        assert_eq!(
            feed_submission_anchor(
                Some(&message("first-message")),
                &message("second-message"),
                false,
                false,
                0
            ),
            Some(message("first-message"))
        );
    }

    #[test]
    fn does_not_anchor_a_follow_up_after_a_user_message_appears() {
        assert_eq!(
            feed_submission_anchor(
                Some(&message("first-message")),
                &message("second-message"),
                false,
                true,
                0
            ),
            None
        );
    }

    #[test]
    fn does_not_anchor_a_thread_that_has_already_started_a_turn() {
        assert_eq!(
            feed_submission_anchor(None, &message("second-message"), true, false, 0),
            None
        );
    }

    fn scroll(at_end: bool, user_scroll_session_active: bool) -> LiveFollowEvent {
        LiveFollowEvent::Scroll {
            at_end,
            user_scroll_session_active,
        }
    }

    #[test]
    fn pauses_immediately_when_the_user_starts_scrolling() {
        assert!(!feed_live_follow(true, LiveFollowEvent::UserScrollBegin));
    }

    #[test]
    fn stays_paused_away_from_the_actual_end() {
        assert!(!feed_live_follow(false, scroll(false, true)));
    }

    #[test]
    fn does_not_mistake_programmatic_layout_compensation_for_a_user_scroll() {
        assert!(feed_live_follow(true, scroll(false, false)));
    }

    #[test]
    fn does_not_re_arm_at_the_end_while_a_user_scroll_session_is_active() {
        assert!(!feed_live_follow(false, scroll(true, true)));
    }

    #[rstest]
    #[case(false, false, false)]
    #[case(true, false, true)]
    #[case(false, true, false)]
    #[case(true, true, false)]
    fn reconciles_follow_after_a_disclosure_settles(
        #[case] at_end: bool,
        #[case] user_scroll_session_active: bool,
        #[case] expected: bool,
    ) {
        assert_eq!(
            feed_live_follow(
                !expected,
                LiveFollowEvent::DisclosureSettled {
                    at_end,
                    user_scroll_session_active
                }
            ),
            expected
        );
    }

    #[test]
    fn re_arms_at_the_actual_end_only_after_the_user_scroll_session_ends() {
        let end = |at_end| LiveFollowEvent::UserScrollEnd {
            at_end,
            user_scroll_session_active: true,
        };
        assert!(feed_live_follow(false, end(true)));
        assert!(!feed_live_follow(false, end(false)));
    }

    #[test]
    fn ignores_momentum_end_events_from_programmatic_scrolling() {
        assert!(feed_live_follow(
            true,
            LiveFollowEvent::UserScrollEnd {
                at_end: false,
                user_scroll_session_active: false
            }
        ));
    }

    #[test]
    fn re_arms_after_an_explicit_reset() {
        assert!(feed_live_follow(false, LiveFollowEvent::Reset));
    }
}

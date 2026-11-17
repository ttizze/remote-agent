//! Timeline spacing follows MessagesTimeline's message padding and tight work lists.
use super::{ConversationRowContent, TurnRow, rows::ConversationRows};

#[derive(Clone, Copy, PartialEq, Eq)]
enum TimelineRowLayout {
    Message { assistant_without_actions: bool },
    Header,
    Group { open: bool },
    Work,
    Live,
    Error,
    Commentary,
    Interrupted,
    Subagent,
    Footer,
    Other,
}

impl TimelineRowLayout {
    fn work_log(self) -> bool {
        matches!(
            self,
            Self::Group { .. } | Self::Work | Self::Live | Self::Error
        )
    }
}

/// A work group owns its consecutive tools; any other row ends membership.
fn work_members(rows: &[TimelineRowLayout]) -> Vec<bool> {
    let mut in_group = false;
    rows.iter()
        .map(|&row| {
            let member = in_group && row == TimelineRowLayout::Work;
            in_group = member || matches!(row, TimelineRowLayout::Group { open: true });
            member
        })
        .collect()
}

/// Only consecutive work rows belong to an open group; a message or header ends it.
fn timeline_row_gaps(rows: &[TimelineRowLayout]) -> Vec<f32> {
    rows.iter()
        .zip(work_members(rows))
        .enumerate()
        .map(|(index, (&row, member))| {
            let next = rows.get(index + 1).copied();
            match row {
                TimelineRowLayout::Header => 6.,
                TimelineRowLayout::Group { open: true } => 0.,
                TimelineRowLayout::Work
                    if member && !next.is_some_and(TimelineRowLayout::work_log) =>
                {
                    4.
                }
                row if row.work_log() => {
                    if next.is_some_and(TimelineRowLayout::work_log) {
                        0.
                    } else {
                        8.
                    }
                }
                TimelineRowLayout::Commentary
                | TimelineRowLayout::Interrupted
                | TimelineRowLayout::Footer => 8.,
                TimelineRowLayout::Subagent => 0.,
                TimelineRowLayout::Message {
                    assistant_without_actions: true,
                } => 8.,
                _ => 16.,
            }
        })
        .collect()
}

#[derive(Clone)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct ConversationWorkGroupRange {
    pub start: u32,
    pub end: u32,
}

/// A group owns only its contiguous work rows. Reasoning is rendered as a trace,
/// which the reference deliberately leaves outside the bounded tool-call scroller.
fn work_group_ranges(rows: &[(TimelineRowLayout, bool)]) -> Vec<ConversationWorkGroupRange> {
    let mut groups = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        if rows[index] == (TimelineRowLayout::Group { open: true }, true) {
            let start = index;
            index += 1;
            while rows
                .get(index)
                .is_some_and(|(layout, _)| *layout == TimelineRowLayout::Work)
            {
                index += 1;
            }
            if index > start + 1 {
                groups.push(ConversationWorkGroupRange {
                    start: start as u32,
                    end: index as u32,
                });
            }
        } else {
            index += 1;
        }
    }
    groups
}

fn row_layout(content: &ConversationRowContent, timeline: Option<&TurnRow>) -> TimelineRowLayout {
    match timeline {
        Some(TurnRow::Fold { .. } | TurnRow::Working) => TimelineRowLayout::Header,
        Some(
            TurnRow::Group { open, .. }
            | TurnRow::Live { open, .. }
            | TurnRow::Subagents { open, .. },
        ) => TimelineRowLayout::Group { open: *open },
        Some(TurnRow::Work { .. }) => TimelineRowLayout::Work,
        Some(TurnRow::Thinking | TurnRow::Requested { .. } | TurnRow::Retrying { .. }) => {
            TimelineRowLayout::Live
        }
        Some(TurnRow::Commentary(_)) => TimelineRowLayout::Commentary,
        Some(TurnRow::Interrupted) => TimelineRowLayout::Interrupted,
        Some(TurnRow::Subagent { .. }) => TimelineRowLayout::Subagent,
        Some(TurnRow::Actions(_)) => TimelineRowLayout::Other,
        _ => match content {
            ConversationRowContent::User { .. } => TimelineRowLayout::Message {
                assistant_without_actions: false,
            },
            ConversationRowContent::Response { show_actions, .. } => TimelineRowLayout::Message {
                assistant_without_actions: !show_actions,
            },
            ConversationRowContent::Error { .. } => TimelineRowLayout::Error,
            ConversationRowContent::PendingRequest { .. } => TimelineRowLayout::Live,
            ConversationRowContent::Background { .. } => TimelineRowLayout::Footer,
            _ => TimelineRowLayout::Other,
        },
    }
}

impl ConversationRows {
    pub fn work_members(&self) -> Vec<bool> {
        work_members(
            &self
                .rows
                .iter()
                .map(|row| row_layout(&row.data.content, row.timeline().as_ref()))
                .collect::<Vec<_>>(),
        )
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
impl ConversationRows {
    pub fn work_groups(&self) -> Vec<ConversationWorkGroupRange> {
        work_group_ranges(&self.rows.iter().map(|row| {
            let timeline = row.timeline();
            let bounded = matches!(&timeline,
                Some(TurnRow::Group { kind, .. } | TurnRow::Live { kind, .. }) if kind != "reasoning");
            (row_layout(&row.data.content, timeline.as_ref()), bounded)
        }).collect::<Vec<_>>())
    }
    pub fn gaps(&self) -> Vec<f32> {
        timeline_row_gaps(
            &self
                .rows
                .iter()
                .map(|row| row_layout(&row.data.content, row.timeline().as_ref()))
                .collect::<Vec<_>>(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{TimelineRowLayout::*, *};

    #[test]
    fn group_membership_ends_at_a_message_and_closed_groups_do_not_own_tools() {
        assert_eq!(
            work_members(&[
                Group { open: true },
                Work,
                Work,
                Commentary,
                Work,
                Group { open: false },
                Work
            ]),
            [false, true, true, false, false, false, false]
        );
    }

    #[test]
    fn tight_work_lists_keep_message_and_turn_boundaries() {
        // Reference CSS: message pb-4, header pb-1.5, work list gap-0 and pb-1.
        assert_eq!(
            timeline_row_gaps(&[
                Message {
                    assistant_without_actions: false
                },
                Header,
                Group { open: true },
                Work,
                Work,
                Message {
                    assistant_without_actions: true
                },
                Group { open: false },
                Work,
                Live,
                Other,
                Commentary,
                Interrupted,
                Subagent,
            ]),
            [16., 6., 0., 0., 4., 8., 0., 0., 8., 16., 8., 8., 0.]
        );
        // A new header breaks group membership; standalone tools use the work-list padding.
        assert_eq!(
            timeline_row_gaps(&[Group { open: true }, Work, Header, Work, Error]),
            [0., 4., 6., 0., 8.]
        );
        assert!(timeline_row_gaps(&[]).is_empty());
        assert_eq!(
            timeline_row_gaps(&[Group { open: true }, Work, Header, Work, Other]),
            [0., 4., 6., 8., 16.]
        );
        assert_eq!(timeline_row_gaps(&[Group { open: true }, Other]), [0., 16.]);
        assert_eq!(
            timeline_row_gaps(&[Group { open: true }, Live, Work, Other]),
            [0., 0., 8., 16.]
        );
        assert_eq!(
            timeline_row_gaps(&[Group { open: false }, Work, Other]),
            [0., 8., 16.]
        );
    }

    #[test]
    fn bounded_groups_stop_before_commentary_other_groups_and_reasoning_traces() {
        let groups = work_group_ranges(&[
            (Other, false),
            (Group { open: true }, true),
            (Work, false),
            (Work, false),
            (Commentary, false),
            (Work, false),
            (Group { open: true }, false),
            (Work, false),
            (Group { open: false }, true),
            (Work, false),
            (Group { open: true }, true),
            (Group { open: true }, true),
            (Work, false),
        ]);
        assert_eq!(
            groups
                .iter()
                .map(|group| (group.start, group.end))
                .collect::<Vec<_>>(),
            [(1, 4), (11, 13)]
        );
        assert!(work_group_ranges(&[]).is_empty());
    }
}

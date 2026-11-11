//! Status priority and accessible descriptions shared by the conversation lists.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadActivity {
    Waiting,
    Running,
    /// Answered, with background work that will wake the agent, as T3 Code's
    /// Waiting: not the user's turn yet, and not a false Done.
    Background,
    Unread,
}

/// What a conversation holds for its user, as T3 Code's list badges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadAttention {
    Approval,
    Input,
    Failed,
}

impl From<agent_protocol::live_activity::Attention> for ThreadAttention {
    fn from(attention: agent_protocol::live_activity::Attention) -> Self {
        use agent_protocol::live_activity::Attention;
        match attention {
            Attention::Approval => Self::Approval,
            Attention::Input => Self::Input,
            Attention::Failed => Self::Failed,
        }
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn thread_activity(
    waiting: bool,
    active: bool,
    background: bool,
    unread: bool,
) -> Option<ThreadActivity> {
    if waiting {
        Some(ThreadActivity::Waiting)
    } else if active {
        Some(ThreadActivity::Running)
    } else if background {
        Some(ThreadActivity::Background)
    } else if unread {
        Some(ThreadActivity::Unread)
    } else {
        None
    }
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn thread_activity_label(activity: ThreadActivity) -> String {
    match activity {
        ThreadActivity::Waiting => "確認待ち",
        ThreadActivity::Running => "実行中",
        ThreadActivity::Background => "待機中",
        ThreadActivity::Unread => "完了・未確認",
    }
    .into()
}

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn thread_attention_label(attention: ThreadAttention) -> String {
    match attention {
        ThreadAttention::Approval => "承認を待っています",
        ThreadAttention::Input => "入力",
        ThreadAttention::Failed => "失敗",
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_work_reads_waiting_below_running_and_above_unread() {
        assert_eq!(
            thread_activity(false, false, true, true),
            Some(ThreadActivity::Background)
        );
        assert_eq!(
            thread_activity(false, true, true, false),
            Some(ThreadActivity::Running)
        );
        assert_eq!(
            thread_activity(true, false, true, false),
            Some(ThreadActivity::Waiting)
        );
        assert_eq!(thread_activity_label(ThreadActivity::Background), "待機中");
    }

    #[test]
    fn waiting_precedes_running_and_running_precedes_unread() {
        for (waiting, active, unread, expected) in [
            (false, false, false, None),
            (false, false, true, Some(ThreadActivity::Unread)),
            (false, true, false, Some(ThreadActivity::Running)),
            (false, true, true, Some(ThreadActivity::Running)),
            (true, false, false, Some(ThreadActivity::Waiting)),
            (true, false, true, Some(ThreadActivity::Waiting)),
            (true, true, false, Some(ThreadActivity::Waiting)),
            (true, true, true, Some(ThreadActivity::Waiting)),
        ] {
            assert_eq!(thread_activity(waiting, active, false, unread), expected);
        }
        assert_eq!(thread_activity_label(ThreadActivity::Waiting), "確認待ち");
        assert_eq!(thread_activity_label(ThreadActivity::Running), "実行中");
        assert_eq!(
            thread_activity_label(ThreadActivity::Unread),
            "完了・未確認"
        );
    }
}

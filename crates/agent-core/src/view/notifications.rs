//! Pure notification policy shared by desktop and mobile clients.
//!
//! Delivery is intentionally left to each client. This module decides whether
//! a state transition merits an in-app notice, an operating-system notice, a
//! sound, or a badge. It never asks for permission or talks to a transport.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{
    state::Snapshot,
    view::thread_summary::{ThreadListStatus, ThreadSummary, thread_list_status},
};
use agent_domain::ThreadRelationship;

/// How a device presents thread attention and completion events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum NotificationMode {
    #[default]
    Off,
    Notifications,
    Sound,
    NotificationsAndSound,
}

impl NotificationMode {
    pub const fn has_notifications(self) -> bool {
        matches!(self, Self::Notifications | Self::NotificationsAndSound)
    }

    pub const fn has_sound(self) -> bool {
        matches!(self, Self::Sound | Self::NotificationsAndSound)
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Notifications => "notifications",
            Self::Sound => "sound",
            Self::NotificationsAndSound => "notifications-and-sound",
        }
    }
}

/// A thread state that can produce a local notification when it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum ThreadNotificationStatus {
    Idle,
    Input,
    Approval,
    Failed,
    Limited,
    Completed,
}

/// The reason a client may present a notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum NotificationEventKind {
    Input,
    Approval,
    Failed,
    Limited,
    Completion,
}

/// The delivery choices for one newly observed event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NotificationDecision {
    pub kind: NotificationEventKind,
    pub in_app: bool,
    pub operating_system: bool,
    pub sound: bool,
    pub badge: bool,
}

/// One attention event folded from two consecutive snapshots. The delivery
/// flags come from [`decide`]; clients only choose the platform surface for
/// each flag and never infer a second notification policy.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NotificationEvent {
    pub thread_id: String,
    pub title: String,
    pub body: String,
    pub in_app: bool,
    pub operating_system: bool,
    pub sound: bool,
    pub badge: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotificationThread {
    id: String,
    title: String,
    status: ThreadNotificationStatus,
    completed_at: Option<i64>,
}

fn notification_status(thread: &ThreadSummary) -> ThreadNotificationStatus {
    match thread_list_status(thread) {
        ThreadListStatus::Input => ThreadNotificationStatus::Input,
        ThreadListStatus::Approval => ThreadNotificationStatus::Approval,
        ThreadListStatus::Failed => ThreadNotificationStatus::Failed,
        ThreadListStatus::Limited => ThreadNotificationStatus::Limited,
        ThreadListStatus::Ready
            if thread
                .latest_run
                .as_ref()
                .is_some_and(|run| run.completed_at.is_some()) =>
        {
            ThreadNotificationStatus::Completed
        }
        ThreadListStatus::Working | ThreadListStatus::Waiting | ThreadListStatus::Ready => {
            ThreadNotificationStatus::Idle
        }
    }
}

fn notification_threads(snapshot: &Snapshot) -> BTreeMap<String, NotificationThread> {
    let mut rows = BTreeMap::new();
    let Some(shell) = snapshot.shell.snapshot.as_ref() else {
        return rows;
    };
    for shell_thread in &shell.threads {
        if shell_thread.archived_at.is_some()
            || shell_thread.relationship_to_parent == Some(ThreadRelationship::Subagent)
        {
            continue;
        }
        let summary = ThreadSummary::from_shell(shell_thread);
        let status = notification_status(&summary);
        let completed_at = summary.latest_run.as_ref().and_then(|run| run.completed_at);
        rows.insert(
            summary.id.clone(),
            NotificationThread {
                id: summary.id,
                title: summary.title,
                status,
                completed_at,
            },
        );
    }
    rows
}

fn notification_title(status: ThreadNotificationStatus) -> &'static str {
    match status {
        ThreadNotificationStatus::Input => "Input needed",
        ThreadNotificationStatus::Approval => "Approval needed",
        ThreadNotificationStatus::Failed => "Thread failed",
        ThreadNotificationStatus::Limited => "Usage limit reached",
        ThreadNotificationStatus::Completed => "Thread completed",
        ThreadNotificationStatus::Idle => "",
    }
}

/// Folds one snapshot transition into local attention events.
///
/// A different store is a new session and establishes the initial baseline;
/// loading a cached Host must not notify for every already-pending row.
pub fn between(
    previous: &Snapshot,
    current: &Snapshot,
    app_visible: bool,
    app_focused: bool,
) -> Vec<NotificationEvent> {
    if previous.store_id != current.store_id {
        return vec![];
    }
    let previous_threads = notification_threads(previous);
    let selected_thread = current.selected_thread.as_ref().map(ToString::to_string);
    let mut events = vec![];
    for (id, thread) in notification_threads(current) {
        let previous_thread = previous_threads.get(&id);
        let previous_status = previous_thread.map(|thread| thread.status);
        let completion_changed = thread.completed_at.is_some()
            && previous_thread.and_then(|thread| thread.completed_at) != thread.completed_at;
        let Some(decision) = decide(
            current.preferences.notification_mode,
            current.preferences.in_app_notifications_enabled,
            app_visible,
            app_focused,
            selected_thread.as_deref() == Some(id.as_str()),
            previous_status,
            thread.status,
            completion_changed,
        ) else {
            continue;
        };
        events.push(NotificationEvent {
            thread_id: thread.id,
            title: notification_title(thread.status).to_owned(),
            body: if thread.title.trim().is_empty() {
                "Untitled thread".to_owned()
            } else {
                thread.title
            },
            in_app: decision.in_app,
            operating_system: decision.operating_system,
            sound: decision.sound,
            badge: decision.badge,
        });
    }
    events
}

/// Decides whether a thread transition should be delivered locally.
///
/// A repeated status is ignored. A completion is keyed by the caller's
/// `completion_changed` bit because a completed thread can receive unrelated
/// shell updates. Operating-system notices are suppressed while the app is
/// in the foreground; in-app notices are limited to that same foreground
/// state and are also suppressed for the selected thread.
pub fn decide(
    mode: NotificationMode,
    in_app_notifications_enabled: bool,
    app_visible: bool,
    app_focused: bool,
    selected_thread: bool,
    previous: Option<ThreadNotificationStatus>,
    current: ThreadNotificationStatus,
    completion_changed: bool,
) -> Option<NotificationDecision> {
    let kind = match current {
        ThreadNotificationStatus::Input if previous != Some(current) => {
            NotificationEventKind::Input
        }
        ThreadNotificationStatus::Approval if previous != Some(current) => {
            NotificationEventKind::Approval
        }
        ThreadNotificationStatus::Failed if previous != Some(current) => {
            NotificationEventKind::Failed
        }
        ThreadNotificationStatus::Limited if previous != Some(current) => {
            NotificationEventKind::Limited
        }
        ThreadNotificationStatus::Completed if completion_changed => {
            NotificationEventKind::Completion
        }
        _ => return None,
    };
    let foreground = app_visible && app_focused;
    let in_app = in_app_notifications_enabled && foreground && !selected_thread;
    let operating_system = mode.has_notifications() && !foreground;
    let sound = mode.has_sound();
    (in_app || operating_system || sound).then_some(NotificationDecision {
        kind,
        in_app,
        operating_system,
        sound,
        badge: operating_system,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(
        id: &str,
        status: ThreadNotificationStatus,
        completed_at: Option<i64>,
    ) -> NotificationThread {
        NotificationThread {
            id: id.into(),
            title: id.into(),
            status,
            completed_at,
        }
    }

    fn fold(
        previous: &[NotificationThread],
        current: &[NotificationThread],
        mode: NotificationMode,
        visible: bool,
        focused: bool,
        selected: Option<&str>,
    ) -> Vec<NotificationEvent> {
        let previous = previous
            .iter()
            .map(|thread| (thread.id.clone(), thread))
            .collect::<BTreeMap<_, _>>();
        current
            .iter()
            .filter_map(|thread| {
                let prior = previous.get(&thread.id).copied();
                let decision = decide(
                    mode,
                    true,
                    visible,
                    focused,
                    selected == Some(thread.id.as_str()),
                    prior.map(|thread| thread.status),
                    thread.status,
                    thread.completed_at.is_some()
                        && prior.and_then(|thread| thread.completed_at) != thread.completed_at,
                )?;
                Some(NotificationEvent {
                    thread_id: thread.id.clone(),
                    title: thread.title.clone(),
                    body: thread.title.clone(),
                    in_app: decision.in_app,
                    operating_system: decision.operating_system,
                    sound: decision.sound,
                    badge: decision.badge,
                })
            })
            .collect()
    }

    #[test]
    fn mode_flags_match_the_four_user_choices() {
        assert!(!NotificationMode::Off.has_notifications());
        assert!(!NotificationMode::Off.has_sound());
        assert!(NotificationMode::Notifications.has_notifications());
        assert!(!NotificationMode::Notifications.has_sound());
        assert!(!NotificationMode::Sound.has_notifications());
        assert!(NotificationMode::Sound.has_sound());
        assert!(NotificationMode::NotificationsAndSound.has_notifications());
        assert!(NotificationMode::NotificationsAndSound.has_sound());
    }

    #[test]
    fn attention_is_deduplicated_and_foreground_delivery_is_local() {
        assert_eq!(
            decide(
                NotificationMode::NotificationsAndSound,
                true,
                true,
                true,
                false,
                Some(ThreadNotificationStatus::Input),
                ThreadNotificationStatus::Input,
                false,
            ),
            None
        );
        let decision = decide(
            NotificationMode::NotificationsAndSound,
            true,
            true,
            true,
            false,
            Some(ThreadNotificationStatus::Idle),
            ThreadNotificationStatus::Input,
            false,
        )
        .unwrap();
        assert!(decision.in_app && !decision.operating_system && decision.sound);
        assert!(!decision.badge);
    }

    #[test]
    fn background_attention_adds_an_os_badge_and_selected_threads_are_suppressed() {
        let decision = decide(
            NotificationMode::Notifications,
            true,
            false,
            false,
            false,
            Some(ThreadNotificationStatus::Idle),
            ThreadNotificationStatus::Failed,
            false,
        )
        .unwrap();
        assert!(!decision.in_app && decision.operating_system && decision.badge);
        assert_eq!(
            decide(
                NotificationMode::Notifications,
                true,
                true,
                true,
                true,
                Some(ThreadNotificationStatus::Idle),
                ThreadNotificationStatus::Failed,
                false,
            ),
            None
        );
    }

    #[test]
    fn completion_requires_a_new_completion_timestamp() {
        assert!(
            decide(
                NotificationMode::Notifications,
                true,
                false,
                false,
                false,
                Some(ThreadNotificationStatus::Idle),
                ThreadNotificationStatus::Completed,
                false,
            )
            .is_none()
        );
        assert!(
            decide(
                NotificationMode::Notifications,
                true,
                false,
                false,
                false,
                Some(ThreadNotificationStatus::Idle),
                ThreadNotificationStatus::Completed,
                true,
            )
            .is_some()
        );
        assert!(
            decide(
                NotificationMode::Notifications,
                true,
                false,
                false,
                false,
                Some(ThreadNotificationStatus::Completed),
                ThreadNotificationStatus::Completed,
                true,
            )
            .is_some()
        );
    }

    #[test]
    fn fold_deduplicates_attention_and_keeps_completion_edges() {
        let previous = [thread("input", ThreadNotificationStatus::Input, None)];
        let current = [
            thread("input", ThreadNotificationStatus::Input, None),
            thread("done", ThreadNotificationStatus::Completed, Some(7)),
        ];
        let events = fold(
            &previous,
            &current,
            NotificationMode::NotificationsAndSound,
            false,
            false,
            None,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].thread_id, "done");
        assert!(events[0].operating_system && events[0].sound && events[0].badge);
        assert!(fold(
            &current,
            &current,
            NotificationMode::NotificationsAndSound,
            false,
            false,
            None,
        )
        .is_empty());
    }

    #[test]
    fn fold_suppresses_selected_foreground_rows_but_keeps_in_app_for_others() {
        let previous = [
            thread("selected", ThreadNotificationStatus::Idle, None),
            thread("other", ThreadNotificationStatus::Idle, None),
        ];
        let current = [
            thread("selected", ThreadNotificationStatus::Approval, None),
            thread("other", ThreadNotificationStatus::Input, None),
        ];
        let events = fold(
            &previous,
            &current,
            NotificationMode::Notifications,
            true,
            true,
            Some("selected"),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].thread_id, "other");
        assert!(events[0].in_app);
        assert!(!events[0].operating_system);
    }
}

//! Pure notification policy shared by desktop and mobile clients.
//!
//! Delivery is intentionally left to each client. This module decides whether
//! a state transition merits an in-app notice, an operating-system notice, a
//! sound, or a badge. It never asks for permission or talks to a transport.
use serde::{Deserialize, Serialize};

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
}

//! Pure notification policy shared by desktop and mobile clients.
//!
//! Delivery is intentionally left to each client. This module decides whether
//! a state transition merits an in-app notice, an operating-system notice, or
//! a sound. It never asks for permission or talks to a transport.
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

/// The sound a client should play for an event. Keeping this separate from
/// the delivery flags lets each native client choose its own sound asset
/// without re-deriving the notification policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum NotificationSoundKind {
    Input,
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
}

/// One attention event folded from two consecutive snapshots. The delivery
/// flags come from [`decide`]; clients only choose the platform surface for
/// each flag and never infer a second notification policy.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct NotificationEvent {
    /// The Host environment that owns this thread, used by clients to route
    /// a click back through the selected profile instead of another Host.
    pub environment_id: Option<String>,
    /// A fully qualified client route; native surfaces pass this through
    /// without reconstructing environment identity.
    pub deep_link: String,
    pub thread_id: String,
    pub title: String,
    pub body: String,
    pub kind: NotificationEventKind,
    pub sound_kind: NotificationSoundKind,
    pub in_app: bool,
    pub operating_system: bool,
    pub sound: bool,
}

pub fn notification_deep_link(environment_id: Option<&str>, thread_id: &str) -> String {
    environment_id.map_or_else(
        || agent_domain::ACTIVITY_OVERVIEW_DEEP_LINK.to_owned(),
        |environment| agent_domain::activity_thread_deep_link(environment, thread_id),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NotificationThread {
    id: String,
    title: String,
    status: ThreadNotificationStatus,
    attention_key: Option<String>,
    completed_at: Option<i64>,
}

fn notification_status(thread: &ThreadSummary) -> ThreadNotificationStatus {
    match thread_list_status(thread) {
        ThreadListStatus::Input => ThreadNotificationStatus::Input,
        ThreadListStatus::Approval => ThreadNotificationStatus::Approval,
        ThreadListStatus::Failed => ThreadNotificationStatus::Failed,
        ThreadListStatus::Limited => ThreadNotificationStatus::Limited,
        ThreadListStatus::Ready if completed_run(thread).is_some() => {
            ThreadNotificationStatus::Completed
        }
        ThreadListStatus::Working | ThreadListStatus::Waiting | ThreadListStatus::Ready => {
            ThreadNotificationStatus::Idle
        }
    }
}

fn completed_run(thread: &ThreadSummary) -> Option<&crate::view::thread_summary::RunSummary> {
    thread.latest_run.as_ref().filter(|run| {
        run.status == crate::view::thread_summary::RuntimeStatus::Completed
            && run.completed_at.is_some()
    })
}

fn attention_key(thread: &ThreadSummary, status: ThreadNotificationStatus) -> Option<String> {
    matches!(
        status,
        ThreadNotificationStatus::Input
            | ThreadNotificationStatus::Approval
            | ThreadNotificationStatus::Failed
            | ThreadNotificationStatus::Limited
    )
    .then(|| {
        format!(
            "{}:{}",
            thread
                .latest_run
                .as_ref()
                .map(|run| run.id.as_str())
                .unwrap_or_default(),
            notification_status_id(status),
        )
    })
}

fn notification_status_id(status: ThreadNotificationStatus) -> &'static str {
    match status {
        ThreadNotificationStatus::Idle => "idle",
        ThreadNotificationStatus::Input => "input",
        ThreadNotificationStatus::Approval => "approval",
        ThreadNotificationStatus::Failed => "failed",
        ThreadNotificationStatus::Limited => "limited",
        ThreadNotificationStatus::Completed => "completed",
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
        let completed_at = completed_run(&summary).and_then(|run| run.completed_at);
        let attention_key = attention_key(&summary, status);
        rows.insert(
            summary.id.clone(),
            NotificationThread {
                id: summary.id,
                title: summary.title,
                status,
                attention_key,
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
    let environment_id = current.context_environment_id().map(str::to_owned);
    let selected_thread = current.selected_thread.as_ref().map(ToString::to_string);
    let mut events = vec![];
    for (id, thread) in notification_threads(current) {
        let previous_thread = previous_threads.get(&id);
        let Some(previous_thread) = previous_thread else {
            // The first observation establishes the baseline. A cached Host
            // can already contain pending work, which must not be replayed as
            // a newly generated local event.
            continue;
        };
        let completion_changed = thread.completed_at.is_some()
            && previous_thread
                .completed_at
                .map_or(true, |previous| thread.completed_at > Some(previous));
        let Some(decision) = decide(
            current.preferences.notification_mode,
            current.preferences.in_app_notifications_enabled,
            app_visible,
            app_focused,
            selected_thread.as_deref() == Some(id.as_str()),
            previous_thread.attention_key.as_deref(),
            thread.attention_key.as_deref(),
            thread.status,
            completion_changed,
        ) else {
            continue;
        };
        events.push(NotificationEvent {
            environment_id: environment_id.clone(),
            deep_link: notification_deep_link(environment_id.as_deref(), &thread.id),
            thread_id: thread.id,
            title: notification_title(thread.status).to_owned(),
            body: if thread.title.trim().is_empty() {
                "Untitled thread".to_owned()
            } else {
                thread.title
            },
            kind: decision.kind,
            sound_kind: sound_kind(decision.kind),
            in_app: decision.in_app,
            operating_system: decision.operating_system,
            sound: decision.sound,
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
    previous_attention_key: Option<&str>,
    attention_key: Option<&str>,
    current: ThreadNotificationStatus,
    completion_changed: bool,
) -> Option<NotificationDecision> {
    let kind = match current {
        ThreadNotificationStatus::Input
            if attention_key.is_some() && attention_key != previous_attention_key =>
        {
            NotificationEventKind::Input
        }
        ThreadNotificationStatus::Approval
            if attention_key.is_some() && attention_key != previous_attention_key =>
        {
            NotificationEventKind::Approval
        }
        ThreadNotificationStatus::Failed
            if attention_key.is_some() && attention_key != previous_attention_key =>
        {
            NotificationEventKind::Failed
        }
        ThreadNotificationStatus::Limited
            if attention_key.is_some() && attention_key != previous_attention_key =>
        {
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
    })
}

fn sound_kind(kind: NotificationEventKind) -> NotificationSoundKind {
    match kind {
        NotificationEventKind::Completion => NotificationSoundKind::Completion,
        NotificationEventKind::Input
        | NotificationEventKind::Approval
        | NotificationEventKind::Failed
        | NotificationEventKind::Limited => NotificationSoundKind::Input,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::search::fixtures;
    use agent_domain::{RunId, RunStatus, ThreadId, Timestamp};

    fn thread(
        id: &str,
        status: ThreadNotificationStatus,
        completed_at: Option<i64>,
    ) -> NotificationThread {
        NotificationThread {
            id: id.into(),
            title: id.into(),
            status,
            attention_key: matches!(
                status,
                ThreadNotificationStatus::Input
                    | ThreadNotificationStatus::Approval
                    | ThreadNotificationStatus::Failed
                    | ThreadNotificationStatus::Limited
            )
            .then(|| format!("{id}:{}", notification_status_id(status))),
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
                    prior.and_then(|thread| thread.attention_key.as_deref()),
                    thread.attention_key.as_deref(),
                    thread.status,
                    thread.completed_at.is_some()
                        && prior
                            .and_then(|thread| thread.completed_at)
                            .map_or(true, |previous| thread.completed_at > Some(previous)),
                )?;
                Some(NotificationEvent {
                    environment_id: None,
                    deep_link: notification_deep_link(None, &thread.id),
                    thread_id: thread.id.clone(),
                    title: thread.title.clone(),
                    body: thread.title.clone(),
                    kind: decision.kind,
                    sound_kind: sound_kind(decision.kind),
                    in_app: decision.in_app,
                    operating_system: decision.operating_system,
                    sound: decision.sound,
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
    fn deep_links_keep_the_owning_environment() {
        assert_eq!(
            notification_deep_link(Some("host-a"), "thread-1"),
            "remoteagent://threads/host-a/thread-1"
        );
        assert_eq!(
            notification_deep_link(None, "thread-1"),
            agent_domain::ACTIVITY_OVERVIEW_DEEP_LINK
        );
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
                Some("input:input"),
                Some("input:input"),
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
            None,
            Some("input:input"),
            ThreadNotificationStatus::Input,
            false,
        )
        .unwrap();
        assert!(decision.in_app && !decision.operating_system && decision.sound);
    }

    #[test]
    fn background_attention_adds_an_os_notice_and_selected_threads_are_suppressed() {
        let decision = decide(
            NotificationMode::Notifications,
            true,
            false,
            false,
            false,
            None,
            Some("failed:failed"),
            ThreadNotificationStatus::Failed,
            false,
        )
        .unwrap();
        assert!(!decision.in_app && decision.operating_system);
        assert_eq!(
            decide(
                NotificationMode::Notifications,
                true,
                true,
                true,
                true,
                None,
                Some("failed:failed"),
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
                None,
                None,
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
                None,
                None,
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
                None,
                None,
                ThreadNotificationStatus::Completed,
                true,
            )
            .is_some()
        );
    }

    #[test]
    fn fold_deduplicates_attention_and_keeps_completion_edges() {
        let previous = [
            thread("input", ThreadNotificationStatus::Input, None),
            thread("done", ThreadNotificationStatus::Completed, Some(6)),
        ];
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
        assert_eq!(events[0].kind, NotificationEventKind::Completion);
        assert_eq!(events[0].sound_kind, NotificationSoundKind::Completion);
        assert!(events[0].operating_system && events[0].sound);
        assert!(
            fold(
                &current,
                &current,
                NotificationMode::NotificationsAndSound,
                false,
                false,
                None,
            )
            .is_empty()
        );
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

    #[test]
    fn a_new_run_with_the_same_attention_status_is_a_new_event() {
        let old = Some("run-1:input");
        let new = Some("run-2:input");
        assert!(
            decide(
                NotificationMode::Notifications,
                true,
                false,
                false,
                false,
                old,
                new,
                ThreadNotificationStatus::Input,
                false,
            )
            .is_some()
        );
    }

    #[test]
    fn first_seen_attention_is_a_baseline() {
        let previous = fixtures::snapshot(vec![], vec![]);
        let mut row = fixtures::row("thread", "project", "needs input");
        row.latest_run = Some(RunId::new("run-1").unwrap());
        row.status = Some(RunStatus::Failed);
        let mut current = fixtures::snapshot(vec![], vec![row]);
        current.preferences.notification_mode = NotificationMode::Notifications;
        assert!(between(&previous, &current, false, false).is_empty());
    }

    #[test]
    fn emitted_routes_keep_the_current_host_identity() {
        let mut previous_row = fixtures::row("thread", "project", "needs input");
        previous_row.latest_run = Some(RunId::new("run-1").unwrap());
        let mut previous = fixtures::snapshot(vec![], vec![previous_row]);
        previous.preferences.notification_mode = NotificationMode::Notifications;

        let mut current_row = fixtures::row("thread", "project", "needs input");
        current_row.latest_run = Some(RunId::new("run-1").unwrap());
        current_row.status = Some(RunStatus::Failed);
        let mut current = fixtures::snapshot(vec![], vec![current_row]);
        current.preferences.notification_mode = NotificationMode::Notifications;
        current.environment = Some(agent_protocol::models::EnvironmentDescriptor {
            environment_id: "host-a".into(),
            ..Default::default()
        });

        let events = between(&previous, &current, false, false);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].environment_id.as_deref(), Some("host-a"));
        assert_eq!(events[0].deep_link, "remoteagent://threads/host-a/thread");
    }

    #[test]
    fn cancelled_runs_with_completion_stamps_do_not_become_completion_events() {
        let mut row = fixtures::row("thread", "project", "cancelled");
        row.latest_run = Some(RunId::new("run-1").unwrap());
        row.status = Some(RunStatus::Cancelled);
        row.latest_run_completed_at = Some(Timestamp::parse("2026-06-20T00:00:01Z").unwrap());
        let mut previous = fixtures::snapshot(vec![], vec![row.clone()]);
        let mut current = fixtures::snapshot(vec![], vec![row]);
        previous.preferences.notification_mode = NotificationMode::Notifications;
        current.preferences.notification_mode = NotificationMode::Notifications;
        assert!(between(&previous, &current, false, false).is_empty());
    }
}

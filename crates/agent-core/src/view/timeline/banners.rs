//! The thread error banner and the usage-limit recovery banner. The failure
//! itself (`last_error`, `last_error_class`, `usage_limit_reset_at` of the
//! presented run) comes from `agent_domain::shell`.
use agent_domain::{LimitRecoveryUpdate, RunId, RunStatus, ThreadShell, Timestamp};
use std::collections::BTreeSet;

pub fn thread_error_banner_key(thread_key: &str, error: Option<&str>) -> Option<String> {
    error.map(|error| format!("{thread_key}\u{0}{error}"))
}

pub fn should_show_thread_error_banner(
    thread_key: &str,
    error: Option<&str>,
    is_dismissed: bool,
) -> bool {
    thread_error_banner_key(thread_key, error).is_some() && !is_dismissed
}

/// Dismissals for the app session, per thread and message: another thread or
/// another error on the same thread shows again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThreadErrorDismissals {
    keys: BTreeSet<String>,
}

impl ThreadErrorDismissals {
    pub fn dismiss(&mut self, banner_key: Option<&str>) {
        if let Some(key) = banner_key {
            self.keys.insert(key.to_owned());
        }
    }

    pub fn is_dismissed(&self, banner_key: Option<&str>) -> bool {
        banner_key.is_some_and(|key| self.keys.contains(key))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BannerVariant {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadErrorBanner {
    pub text: String,
    pub variant: BannerVariant,
    /// Passed to `ThreadErrorDismissals::dismiss` with the local error cleared.
    pub dismiss_key: String,
    pub dismiss_label: String,
}

fn runtime_failed(shell: &ThreadShell) -> bool {
    shell.activity_run_status.or(shell.status) == Some(RunStatus::Failed)
}

fn usage_limited(shell: &ThreadShell) -> bool {
    runtime_failed(shell)
        && shell.last_error_class.as_deref() == Some("usage_limit")
        && shell.latest_run.is_some()
}

/// The banner over the timeline. `local_error` is a failure this client saw
/// that the thread does not record; it wins over the thread's failure. A
/// usage-limit failure shows as the recovery banner instead.
pub fn thread_error_banner(
    thread_key: &str,
    local_error: Option<&str>,
    shell: Option<&ThreadShell>,
    dismissals: &ThreadErrorDismissals,
) -> Option<ThreadErrorBanner> {
    let thread_error = shell.and_then(|shell| shell.last_error.as_deref());
    let error = local_error.or(thread_error);
    let key = thread_error_banner_key(thread_key, error);
    if !should_show_thread_error_banner(thread_key, error, dismissals.is_dismissed(key.as_deref()))
    {
        return None;
    }
    let from_thread = local_error.is_none() && error == thread_error;
    if from_thread && shell.is_some_and(usage_limited) {
        return None;
    }
    let class = shell
        .filter(|_| from_thread)
        .and_then(|shell| shell.last_error_class.as_deref());
    Some(ThreadErrorBanner {
        text: error?.to_owned(),
        variant: if class == Some("usage_limit") {
            BannerVariant::Warning
        } else {
            BannerVariant::Error
        },
        dismiss_key: key?,
        dismiss_label: "Dismiss error".into(),
    })
}

pub const USAGE_LIMIT_TITLE: &str = "Usage limit reached";
/// With a reset time, desktop reads "Resets <local time>" and mobile
/// "Usage limit resets <local time>.".
pub const RESET_UNAVAILABLE_DESKTOP: &str = "Reset time unavailable; retry manually";
pub const RESET_UNAVAILABLE_MOBILE: &str =
    "The provider did not report a reset time. Retry manually when your limit is available.";
pub const LIMIT_RECOVERY_SAVING: &str = "Saving...";
pub const LIMIT_RECOVERY_FAILED: &str = "Could not change limit recovery.";
pub const LIMIT_RESET_PASSED: &str = "The reset time has passed. Retry the thread manually.";

/// What the recovery banner offers for a run stopped by a usage limit.
/// Clients evaluate it again at `reset_at`, when snoozing stops being possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageLimitRecovery {
    pub id: String,
    pub run: RunId,
    pub title: String,
    pub reset_at: Option<Timestamp>,
    /// The reset lies after the run stopped, so the actions apply.
    pub can_schedule: bool,
    pub scheduled: bool,
    pub snoozed: bool,
    pub resume_label: String,
    /// Desktop hides the snooze action once snoozed; mobile offers "Wake now".
    pub snooze_label: String,
    pub snooze_enabled: bool,
}

pub fn usage_limit_recovery(shell: &ThreadShell, now_ms: i64) -> Option<UsageLimitRecovery> {
    if !usage_limited(shell) {
        return None;
    }
    let run = shell.latest_run.clone()?;
    let reset_at = shell.usage_limit_reset_at.clone();
    let stopped_at = shell
        .latest_run_completed_at
        .as_ref()
        .unwrap_or(&shell.updated_at);
    let recovery = shell
        .limit_recovery
        .as_ref()
        .filter(|recovery| recovery.run == run && Some(&recovery.reset_at) == reset_at.as_ref());
    let scheduled = recovery.is_some_and(|recovery| recovery.auto_resume);
    let snoozed = recovery.is_some_and(|recovery| {
        recovery.snooze && shell.snoozed_until.as_ref() == Some(&recovery.reset_at)
    });
    let reset_ms = reset_at.as_ref().map(Timestamp::millis);
    Some(UsageLimitRecovery {
        id: format!("usage-limit-recovery:{run}"),
        title: USAGE_LIMIT_TITLE.into(),
        can_schedule: reset_ms.is_some_and(|reset| reset > stopped_at.millis()),
        scheduled,
        snoozed,
        resume_label: if scheduled {
            "Cancel auto-resume"
        } else {
            "Resume at reset"
        }
        .into(),
        snooze_label: if snoozed {
            "Wake now"
        } else {
            "Snooze until reset"
        }
        .into(),
        snooze_enabled: snoozed || reset_ms.is_some_and(|reset| reset > now_ms),
        run,
        reset_at,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecoveryAction {
    Resume,
    Snooze,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryToggle {
    Ignored,
    /// Shown under the actions.
    Rejected(String),
    Send(LimitRecoveryUpdate),
}

/// The metadata update one recovery action sends.
pub fn toggle_limit_recovery(
    recovery: &UsageLimitRecovery,
    action: RecoveryAction,
    now_ms: i64,
) -> RecoveryToggle {
    let Some(reset_at) = recovery.reset_at.clone().filter(|_| recovery.can_schedule) else {
        return RecoveryToggle::Ignored;
    };
    if action == RecoveryAction::Snooze && !recovery.snoozed && reset_at.millis() <= now_ms {
        return RecoveryToggle::Rejected(LIMIT_RESET_PASSED.into());
    }
    RecoveryToggle::Send(LimitRecoveryUpdate {
        run: recovery.run.clone(),
        reset_at,
        auto_resume: (action == RecoveryAction::Resume).then_some(!recovery.scheduled),
        snooze: (action == RecoveryAction::Snooze).then_some(!recovery.snoozed),
    })
}

/// Waking a thread the recovery snoozed clears the recovery's snooze;
/// `None` means the ordinary unsnooze.
pub fn wake_limit_recovery_update(shell: &ThreadShell) -> Option<LimitRecoveryUpdate> {
    let recovery = shell.limit_recovery.as_ref()?;
    (recovery.snooze
        && shell.latest_run.as_ref() == Some(&recovery.run)
        && shell.snoozed_until.as_ref() == Some(&recovery.reset_at))
    .then(|| LimitRecoveryUpdate {
        run: recovery.run.clone(),
        reset_at: recovery.reset_at.clone(),
        auto_resume: None,
        snooze: Some(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_domain::{
        Driver, InteractionMode, LimitRecovery, MessageAuthor, ModelSelection, RuntimeMode,
        ThreadId,
    };
    use std::collections::BTreeMap;

    fn at(value: &str) -> Timestamp {
        Timestamp::parse(value).unwrap()
    }
    fn ms(value: &str) -> i64 {
        at(value).millis()
    }
    fn shell() -> ThreadShell {
        ThreadShell {
            id: ThreadId::new("thread").unwrap(),
            project: "project".into(),
            title: "Thread".into(),
            selection: ModelSelection {
                instance: "claude".into(),
                driver: Driver::Claude,
                model: "fable".into(),
                options: BTreeMap::new(),
            },
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: None,
            parent: None,
            fork_boundary: None,
            imported: false,
            created_by: MessageAuthor::User,
            creation_source: "desktop".into(),
            created_at: at("2026-10-01T10:00:00Z"),
            updated_at: at("2026-10-01T10:05:00Z"),
            archived_at: None,
            deleted_at: None,
            settled: None,
            settled_at: None,
            snoozed_until: None,
            snoozed_at: None,
            pinned_at: None,
            pin_order: None,
            active_order: None,
            last_visited_at: None,
            auto_settle: false,
            title_regenerating: false,
            latest_run: Some(RunId::new("run-1").unwrap()),
            latest_run_requested_at: None,
            latest_run_started_at: None,
            latest_run_completed_at: Some(at("2026-10-01T10:01:00Z")),
            status: Some(RunStatus::Failed),
            active_run: None,
            activity_run_status: None,
            activity_run_started_at: None,
            last_error: Some("Usage limit reached".into()),
            last_error_class: Some("usage_limit".into()),
            usage_limit_reset_at: Some(at("2026-10-01T15:00:00Z")),
            limit_recovery: None,
            linked_pull_request: None,
            pending_request: None,
            latest_user_message_at: None,
            latest_user_authored_message_at: None,
            has_actionable_proposed_plan: false,
            pending_background_work: vec![],
            provider_instance_history: vec![],
            item_count: 0,
            visible_item_count: 0,
        }
    }
    fn recovery(auto_resume: bool, snooze: bool) -> LimitRecovery {
        LimitRecovery {
            request: None,
            run: RunId::new("run-1").unwrap(),
            reset_at: at("2026-10-01T15:00:00Z"),
            auto_resume,
            snooze,
        }
    }

    #[test]
    fn stays_hidden_after_its_current_error_is_dismissed() {
        let mut dismissals = ThreadErrorDismissals::default();
        let banner_key = thread_error_banner_key("env:thread-a", Some("Aborted"));
        dismissals.dismiss(banner_key.as_deref());
        assert!(!should_show_thread_error_banner(
            "env:thread-a",
            Some("Aborted"),
            dismissals.is_dismissed(banner_key.as_deref()),
        ));
    }

    #[test]
    fn reappears_when_a_new_error_arrives_on_the_same_thread() {
        let mut dismissals = ThreadErrorDismissals::default();
        dismissals.dismiss(thread_error_banner_key("env:thread-b", Some("Turn failed")).as_deref());
        let new_error_key = thread_error_banner_key("env:thread-b", Some("Provider crashed"));
        assert!(!dismissals.is_dismissed(new_error_key.as_deref()));
        assert!(should_show_thread_error_banner(
            "env:thread-b",
            Some("Provider crashed"),
            dismissals.is_dismissed(new_error_key.as_deref()),
        ));
    }

    #[test]
    fn scopes_dismissals_to_the_thread_that_dismissed_them() {
        let mut dismissals = ThreadErrorDismissals::default();
        dismissals.dismiss(thread_error_banner_key("env:thread-c", Some("Aborted")).as_deref());
        let other_thread_key = thread_error_banner_key("env:other-thread", Some("Aborted"));
        assert!(!dismissals.is_dismissed(other_thread_key.as_deref()));
        assert!(should_show_thread_error_banner(
            "env:other-thread",
            Some("Aborted"),
            dismissals.is_dismissed(other_thread_key.as_deref()),
        ));
    }

    #[test]
    fn keeps_a_dismissal_across_visiting_threads_with_no_error() {
        let mut dismissals = ThreadErrorDismissals::default();
        let banner_key = thread_error_banner_key("env:thread-d", Some("Aborted"));
        dismissals.dismiss(banner_key.as_deref());
        assert!(!should_show_thread_error_banner(
            "env:thread-d",
            None,
            false
        ));
        assert!(dismissals.is_dismissed(banner_key.as_deref()));
        assert!(!should_show_thread_error_banner(
            "env:thread-d",
            Some("Aborted"),
            dismissals.is_dismissed(banner_key.as_deref()),
        ));
    }

    #[test]
    fn never_shows_a_null_error() {
        assert!(!should_show_thread_error_banner(
            "env:thread-e",
            None,
            false
        ));
    }

    #[test]
    fn hands_a_usage_limit_failure_to_the_recovery_banner() {
        let dismissals = ThreadErrorDismissals::default();
        let limited = shell();
        assert_eq!(
            thread_error_banner("env:thread", None, Some(&limited), &dismissals),
            None
        );
        let local = thread_error_banner(
            "env:thread",
            Some("Could not send"),
            Some(&limited),
            &dismissals,
        )
        .unwrap();
        assert_eq!(
            (local.text.as_str(), local.variant),
            ("Could not send", BannerVariant::Error)
        );
        let mut failed = shell();
        failed.last_error = Some("Provider crashed".into());
        failed.last_error_class = Some("provider_error".into());
        let banner = thread_error_banner("env:thread", None, Some(&failed), &dismissals).unwrap();
        assert_eq!(banner.variant, BannerVariant::Error);
        assert_eq!(banner.dismiss_label, "Dismiss error");
        let mut dismissed = ThreadErrorDismissals::default();
        dismissed.dismiss(Some(&banner.dismiss_key));
        assert_eq!(
            thread_error_banner("env:thread", None, Some(&failed), &dismissed),
            None
        );
        let mut no_run = shell();
        no_run.latest_run = None;
        let warning = thread_error_banner("env:thread", None, Some(&no_run), &dismissals).unwrap();
        assert_eq!(warning.variant, BannerVariant::Warning);
    }

    #[test]
    fn offers_resume_and_snooze_until_the_reset() {
        let now = ms("2026-10-01T12:00:00Z");
        let banner = usage_limit_recovery(&shell(), now).unwrap();
        assert_eq!(banner.id, "usage-limit-recovery:run-1");
        assert_eq!(banner.title, "Usage limit reached");
        assert!(banner.can_schedule && banner.snooze_enabled);
        assert_eq!(
            (banner.resume_label.as_str(), banner.snooze_label.as_str()),
            ("Resume at reset", "Snooze until reset")
        );
        assert_eq!(
            toggle_limit_recovery(&banner, RecoveryAction::Resume, now),
            RecoveryToggle::Send(LimitRecoveryUpdate {
                run: RunId::new("run-1").unwrap(),
                reset_at: at("2026-10-01T15:00:00Z"),
                auto_resume: Some(true),
                snooze: None,
            })
        );
        assert_eq!(
            toggle_limit_recovery(&banner, RecoveryAction::Snooze, now),
            RecoveryToggle::Send(LimitRecoveryUpdate {
                run: RunId::new("run-1").unwrap(),
                reset_at: at("2026-10-01T15:00:00Z"),
                auto_resume: None,
                snooze: Some(true),
            })
        );
        let later = ms("2026-10-01T16:00:00Z");
        let passed = usage_limit_recovery(&shell(), later).unwrap();
        assert!(!passed.snooze_enabled);
        assert_eq!(
            toggle_limit_recovery(&passed, RecoveryAction::Snooze, later),
            RecoveryToggle::Rejected(LIMIT_RESET_PASSED.into())
        );
    }

    #[test]
    fn reflects_a_scheduled_and_snoozed_recovery_of_the_same_run_and_reset() {
        let now = ms("2026-10-01T12:00:00Z");
        let mut thread = shell();
        thread.limit_recovery = Some(recovery(true, true));
        thread.snoozed_until = Some(at("2026-10-01T15:00:00Z"));
        let banner = usage_limit_recovery(&thread, now).unwrap();
        assert!(banner.scheduled && banner.snoozed);
        assert_eq!(
            (banner.resume_label.as_str(), banner.snooze_label.as_str()),
            ("Cancel auto-resume", "Wake now")
        );
        assert_eq!(
            toggle_limit_recovery(&banner, RecoveryAction::Resume, now),
            RecoveryToggle::Send(LimitRecoveryUpdate {
                run: RunId::new("run-1").unwrap(),
                reset_at: at("2026-10-01T15:00:00Z"),
                auto_resume: Some(false),
                snooze: None,
            })
        );
        assert_eq!(
            wake_limit_recovery_update(&thread),
            Some(LimitRecoveryUpdate {
                run: RunId::new("run-1").unwrap(),
                reset_at: at("2026-10-01T15:00:00Z"),
                auto_resume: None,
                snooze: Some(false),
            })
        );
        thread.snoozed_until = Some(at("2026-10-02T00:00:00Z"));
        assert!(!usage_limit_recovery(&thread, now).unwrap().snoozed);
        assert_eq!(wake_limit_recovery_update(&thread), None);
        let mut stale = shell();
        stale.limit_recovery = Some(recovery(true, false));
        stale.usage_limit_reset_at = Some(at("2026-10-01T18:00:00Z"));
        assert!(!usage_limit_recovery(&stale, now).unwrap().scheduled);
    }

    #[test]
    fn shows_no_actions_without_a_reset_after_the_stop() {
        let now = ms("2026-10-01T12:00:00Z");
        let mut unknown = shell();
        unknown.usage_limit_reset_at = None;
        let banner = usage_limit_recovery(&unknown, now).unwrap();
        assert!(!banner.can_schedule && !banner.snooze_enabled);
        assert_eq!(
            toggle_limit_recovery(&banner, RecoveryAction::Resume, now),
            RecoveryToggle::Ignored
        );
        let mut early = shell();
        early.usage_limit_reset_at = Some(at("2026-10-01T10:00:30Z"));
        assert!(!usage_limit_recovery(&early, now).unwrap().can_schedule);
        let mut running = shell();
        running.activity_run_status = Some(RunStatus::Running);
        assert_eq!(usage_limit_recovery(&running, now), None);
        let mut other = shell();
        other.last_error_class = Some("provider_error".into());
        assert_eq!(usage_limit_recovery(&other, now), None);
    }
}

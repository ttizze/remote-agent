//! Host sweep decisions over a thread's list row: automatic settlement (T3
//! ThreadSettlementService.ts) and usage-limit recovery (T3
//! UsageLimitRecoveryWorker.ts). The Host runs them on a schedule and
//! dispatches the command they return.
use crate::*;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
pub const QUEUED_TURN_START_GRACE_MS: i64 = 2 * 60 * 1000;

fn millis(at: &Option<Timestamp>) -> Option<i64> {
    at.as_ref().map(Timestamp::millis)
}

/// A recent user message stays queued until a run adopts its timestamp.
pub fn thread_has_queued_turn_start(thread: &ThreadShell, now_ms: i64) -> bool {
    let Some(message_at) = millis(&thread.latest_user_message_at) else {
        return false;
    };
    if thread.status == Some(RunStatus::Failed)
        || (now_ms - message_at).abs() > QUEUED_TURN_START_GRACE_MS
    {
        return false;
    }
    if thread.latest_run.is_none() {
        return true;
    }
    [
        &thread.latest_run_requested_at,
        &thread.latest_run_started_at,
        &thread.latest_run_completed_at,
    ]
    .into_iter()
    .all(|at| millis(at).is_none_or(|at| at < message_at))
}

/// T3 isAutoSettlementCandidate: cheap checks before any source control lookup.
pub fn is_auto_settlement_candidate(thread: &ThreadShell, now_ms: i64) -> bool {
    if thread.archived_at.is_some()
        || thread.settled.is_some()
        || thread.pinned_at.is_some()
        || !thread.auto_settle
        || thread.pending_request.is_some()
        || thread.activity_run_status.is_some()
        || thread
            .pending_background_work
            .iter()
            .any(|work| work.kind != BackgroundKind::Command)
        || thread_has_queued_turn_start(thread, now_ms)
    {
        return false;
    }
    if millis(&thread.snoozed_until).is_none_or(|until| until <= now_ms) {
        return true;
    }
    // A snoozed thread that woke early (an error or completed work) can settle.
    let snoozed_at = millis(&thread.snoozed_at);
    let completed_at = millis(&thread.latest_run_completed_at);
    let after_snooze = snoozed_at.zip(completed_at).is_some_and(|(s, c)| c > s);
    let woke_on_error = thread.status == Some(RunStatus::Failed)
        && (snoozed_at.is_none() || completed_at.is_some() && after_snooze);
    woke_on_error || after_snooze
}

/// T3 resolveAutoSettlementAt for a thread without pull request state: the
/// latest activity, once it is older than `after_days`.
pub fn auto_settlement_at(
    thread: &ThreadShell,
    now_ms: i64,
    after_days: Option<u64>,
) -> Option<Timestamp> {
    if !is_auto_settlement_candidate(thread, now_ms) {
        return None;
    }
    let activity = [
        &thread.latest_user_message_at,
        &thread.latest_run_requested_at,
        &thread.latest_run_started_at,
        &thread.latest_run_completed_at,
    ]
    .into_iter()
    .filter_map(millis)
    .max()?;
    let days = i64::try_from(after_days?).ok()?;
    (activity < now_ms - days * DAY_MS)
        .then(|| Timestamp::from_millis(activity).ok())
        .flatten()
}

/// T3 limitRecoveryCommand: arm a recovery for a fresh usage-limit stop, or
/// resume the limited run once its armed reset has passed.
pub fn limit_recovery_command(
    thread: &ThreadShell,
    auto_resume: bool,
    snooze: bool,
    now_ms: i64,
) -> Option<(CommandId, Command)> {
    if thread.status != Some(RunStatus::Failed)
        || thread.last_error_class.as_deref() != Some("usage_limit")
        || thread.archived_at.is_some()
        || thread.settled == Some(true)
        || thread.pending_request.is_some()
    {
        return None;
    }
    let run = thread.latest_run.clone()?;
    let reset_at = thread.usage_limit_reset_at.clone()?;
    let reset_ms = reset_at.millis();
    if reset_ms <= millis(&thread.latest_run_completed_at).unwrap_or(thread.updated_at.millis()) {
        return None;
    }
    let identity = format!("{}:{run}:{reset_ms}", thread.id);
    let recovery = thread
        .limit_recovery
        .as_ref()
        .filter(|recovery| recovery.run == run && recovery.reset_at == reset_at);
    let Some(recovery) = recovery else {
        if !auto_resume && (!snooze || reset_ms <= now_ms) {
            return None;
        }
        return Some((
            CommandId::new(format!("limit-arm:{identity}")).ok()?,
            Command::UpdateMetadata {
                title: None,
                regenerate_title: None,
                branch: None,
                worktree_path: None,
                expected_worktree_path: None,
                expected_empty: false,
                limit_recovery: Some(Some(LimitRecoveryUpdate {
                    run,
                    reset_at,
                    auto_resume: Some(auto_resume),
                    snooze: Some(snooze && reset_ms > now_ms),
                })),
                linked_pull_request: None,
                project_root: None,
            },
        ));
    };
    if !recovery.auto_resume
        || reset_ms > now_ms
        || millis(&thread.snoozed_until).is_some_and(|until| until > now_ms)
    {
        return None;
    }
    let delivery = format!(
        "{identity}:{}",
        recovery
            .request
            .as_ref()
            .map_or("legacy", |request| request.as_str())
    );
    Some((
        CommandId::new(format!("limit-resume:{delivery}")).ok()?,
        Command::Send(SendMessage {
            context: None,
            created_by: MessageAuthor::User,
            creation_source: "server".into(),
            id: MessageId::new(format!("limit-resume:{delivery}")).ok()?,
            text: "Continue where you left off.".into(),
            attachments: vec![],
            selection: None,
            mode: DispatchMode::StartImmediately,
            intent: None,
            source_plan: None,
            resolved_plan: None,
            continuation: Some(Continuation::UsageLimit {
                run,
                recovery: recovery.request.clone(),
            }),
            title_seed: None,
        }),
    ))
}

// T3 ThreadSettlementService.test.ts.
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    const NOW_MS: i64 = 1_781_092_800_000;
    fn at(offset: i64) -> Option<Timestamp> {
        Some(Timestamp::from_millis(NOW_MS + offset).unwrap())
    }
    fn shell() -> ThreadShell {
        let mut state = State::default();
        apply(
            &mut state,
            &Fact {
                at: at(-30 * DAY_MS).unwrap(),
                body: FactBody::ThreadCreated {
                    id: ThreadId::new("thread-1").unwrap(),
                    project: "project-1".into(),
                    title: "Thread".into(),
                    selection: ModelSelection {
                        instance: "codex".into(),
                        driver: Driver::Codex,
                        model: "gpt-5.4".into(),
                        options: BTreeMap::new(),
                    },
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: InteractionMode::Default,
                },
            },
        )
        .unwrap();
        crate::shell(&state).unwrap()
    }
    fn with(change: impl FnOnce(&mut ThreadShell)) -> ThreadShell {
        let mut thread = shell();
        change(&mut thread);
        thread
    }
    fn candidate(thread: ThreadShell) -> bool {
        is_auto_settlement_candidate(&thread, NOW_MS)
    }

    #[test]
    fn excludes_overridden_pinned_blocked_and_working_threads() {
        assert!(candidate(shell()));
        assert!(!candidate(with(|t| t.archived_at = at(-1))));
        assert!(!candidate(with(|t| t.settled = Some(true))));
        assert!(!candidate(with(|t| t.settled = Some(false))));
        assert!(!candidate(with(|t| t.pinned_at = at(-1))));
        assert!(!candidate(with(|t| t.auto_settle = false)));
        assert!(!candidate(with(|t| {
            t.activity_run_status = Some(RunStatus::Running)
        })));
        assert!(!candidate(with(|t| {
            t.pending_request = Some(PendingRequestSummary {
                id: RuntimeRequestId::new("request").unwrap(),
                kind: "approval".into(),
                created_at: at(-1).unwrap(),
            })
        })));
        assert!(!candidate(with(|t| {
            t.pending_background_work = vec![PendingBackgroundSummary {
                key: "review".into(),
                kind: BackgroundKind::Subagent,
                description: String::new(),
            }]
        })));
    }

    #[test]
    fn settles_a_thread_whose_only_background_work_is_a_command_left_running() {
        assert!(candidate(with(|t| {
            t.pending_background_work = vec![PendingBackgroundSummary {
                key: "dev".into(),
                kind: BackgroundKind::Command,
                description: "vp run dev --share".into(),
            }]
        })));
    }

    #[test]
    fn keeps_snoozed_threads_parked_until_they_wake_early_on_error_or_completion() {
        let snoozed = || {
            with(|t| {
                t.snoozed_until = at(60 * 60 * 1000);
                t.snoozed_at = at(-60 * 60 * 1000);
            })
        };
        let failed = |completed: Option<Timestamp>| {
            let mut t = snoozed();
            t.status = Some(RunStatus::Failed);
            t.latest_run_completed_at = completed;
            t
        };
        assert!(!candidate(snoozed()));
        assert!(candidate(failed(at(-30 * 60 * 1000))));
        let mut completed = snoozed();
        completed.latest_run_completed_at = at(-30 * 60 * 1000);
        assert!(candidate(completed));
        assert!(!candidate(failed(at(-2 * 60 * 60 * 1000))));
        assert!(!candidate(failed(at(-60 * 60 * 1000))));
        assert!(!candidate(failed(None)));
        let mut expired = snoozed();
        expired.snoozed_until = at(-1);
        assert!(candidate(expired));
    }

    #[test]
    fn holds_a_fresh_unadopted_user_message_inside_the_grace_window_only() {
        let queued = |change: &dyn Fn(&mut ThreadShell)| {
            let mut t = shell();
            change(&mut t);
            thread_has_queued_turn_start(&t, NOW_MS)
        };
        assert!(queued(&|t| t.latest_user_message_at = at(-1000)));
        assert!(!queued(&|t| {
            t.latest_user_message_at = at(-1000);
            t.latest_run = Some(RunId::new("run-1").unwrap());
            t.latest_run_requested_at = at(-500);
        }));
        assert!(!queued(&|t| {
            t.latest_user_message_at = at(-QUEUED_TURN_START_GRACE_MS - 1)
        }));
        assert!(!queued(&|t| {
            t.latest_user_message_at = at(QUEUED_TURN_START_GRACE_MS + 1)
        }));
        assert!(!queued(&|t| {
            t.latest_user_message_at = at(-1000);
            t.status = Some(RunStatus::Failed);
        }));
    }

    #[test]
    fn uses_the_latest_activity_time_when_the_inactivity_window_elapses() {
        let idle = with(|t| {
            t.latest_user_message_at = at(-4 * DAY_MS);
            t.latest_run_requested_at = at(-4 * DAY_MS);
            t.latest_run_started_at = at(-4 * DAY_MS);
            t.latest_run_completed_at = at(-3 * DAY_MS);
        });
        assert_eq!(auto_settlement_at(&idle, NOW_MS, Some(2)), at(-3 * DAY_MS));
        assert_eq!(auto_settlement_at(&idle, NOW_MS, Some(5)), None);
        assert_eq!(auto_settlement_at(&idle, NOW_MS, None), None);
        assert_eq!(auto_settlement_at(&shell(), NOW_MS, Some(2)), None);
    }
}

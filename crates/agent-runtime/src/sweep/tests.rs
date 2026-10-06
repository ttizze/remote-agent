//! Worker cases ported from T3 `ThreadSettlementService.test.ts` (without pull
//! request state) and the sweep of `UsageLimitRecoveryWorker.ts`.
use super::*;
use crate::store::tests::{selection, temp_store};
use crate::{ActorContext, ActorHandle};
use agent_domain::{
    Fact, FactBody, InteractionMode, LimitRecovery, RunId, RunStatus, RuntimeMode, State,
    Timestamp, apply, shell,
};

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn make_thread(id: &str, project: &str, change: impl FnOnce(&mut ThreadShell)) -> ThreadShell {
    let mut state = State::default();
    apply(
        &mut state,
        &Fact {
            at: at("2026-08-01T00:00:00.000Z"),
            body: FactBody::ThreadCreated {
                id: ThreadId::new(id).unwrap(),
                project: project.into(),
                title: id.into(),
                selection: selection(),
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: InteractionMode::Default,
            },
        },
    )
    .unwrap();
    let mut thread = shell(&state).unwrap();
    thread.updated_at = at("2026-08-20T00:00:00.000Z");
    thread.latest_user_message_at = Some(at("2026-08-20T00:00:00.000Z"));
    change(&mut thread);
    thread
}

const NOW: &str = "2026-08-28T12:00:00.000Z";

fn days(after: Option<u64>) -> ConversationSettings {
    ConversationSettings {
        auto_settle_after_days: after,
        ..ConversationSettings::default()
    }
}

#[test]
fn settles_only_the_project_opted_in_while_environment_settlement_is_disabled() {
    let thread = make_thread("project-opt-in", "project-1", |t| {
        t.latest_run_completed_at = Some(at("2026-08-25T00:00:00.000Z"))
    });
    let other = make_thread("environment-off", "other-project", |_| {});
    let commands = settlement_commands(
        &[thread.clone(), other],
        |project| days((project == "project-1").then_some(2)),
        at(NOW).millis(),
    );
    let threads: Vec<_> = commands.iter().map(|(thread, ..)| thread).collect();
    assert_eq!(threads, [&thread.id]);
}

#[test]
fn dispatches_the_last_activity_time_with_the_snapshot_guard() {
    let thread = make_thread("inactive-open-pr", "project-1", |t| {
        t.latest_run_completed_at = Some(at("2026-08-25T00:00:00.000Z"))
    });
    let commands = settlement_commands(
        std::slice::from_ref(&thread),
        |_| days(Some(2)),
        at(NOW).millis(),
    );
    let [
        (
            _,
            _,
            Command::SettleAutomatically {
                snapshot_at,
                settled_at,
            },
        ),
    ] = &commands[..]
    else {
        panic!("{commands:?}")
    };
    assert_eq!(settled_at, &thread.latest_run_completed_at);
    assert_eq!(snapshot_at, &thread.updated_at);
}

#[test]
fn settles_after_the_default_three_days_of_inactivity() {
    let recent = make_thread("recent", "project-1", |t| {
        t.latest_user_message_at = Some(at("2026-08-26T12:00:00.000Z"))
    });
    let idle = make_thread("idle", "project-1", |_| {});
    let commands = settlement_commands(
        &[recent, idle.clone()],
        |_| ConversationSettings::default(),
        at(NOW).millis(),
    );
    let threads: Vec<_> = commands.iter().map(|(thread, ..)| thread).collect();
    assert_eq!(threads, [&idle.id]);
    assert!(settlement_commands(&[idle], |_| days(None), at(NOW).millis()).is_empty());
}

#[test]
fn recovers_usage_limits_only_when_the_settings_opt_in() {
    let limited = make_thread("limited", "project-1", |t| {
        t.status = Some(RunStatus::Failed);
        t.last_error_class = Some("usage_limit".into());
        t.latest_run = Some(RunId::new("run-1").unwrap());
        t.latest_run_completed_at = Some(at("2026-08-28T11:00:00.000Z"));
        t.usage_limit_reset_at = Some(at("2026-08-28T13:00:00.000Z"));
    });
    let now = at(NOW).millis();
    assert!(
        limit_recovery_commands(
            std::slice::from_ref(&limited),
            |_| ConversationSettings::default(),
            now
        )
        .is_empty()
    );
    let resume = |_: &str| ConversationSettings {
        auto_resume_limited_threads: true,
        ..ConversationSettings::default()
    };
    let armed = limit_recovery_commands(std::slice::from_ref(&limited), resume, now);
    assert!(
        matches!(&armed[..], [(_, id, Command::UpdateMetadata { .. })] if id.as_str().starts_with("limit-arm:"))
    );

    let mut ready = limited;
    ready.limit_recovery = Some(LimitRecovery {
        request: None,
        run: RunId::new("run-1").unwrap(),
        reset_at: at("2026-08-28T13:00:00.000Z"),
        auto_resume: true,
        snooze: false,
    });
    let later = at("2026-08-28T13:00:01.000Z").millis();
    let resumed = limit_recovery_commands(&[ready], resume, later);
    assert!(
        matches!(&resumed[..], [(_, id, Command::Send(_))] if id.as_str().starts_with("limit-resume:"))
    );
}

#[tokio::test]
async fn sweeps_read_live_unarchived_rows() {
    let (_dir, store) = temp_store();
    let context = ActorContext::new(store.clone());
    for id in ["thread-a", "thread-b"] {
        let thread = ThreadId::new(id).unwrap();
        let handle = ActorHandle::spawn(context.clone(), thread.clone())
            .await
            .unwrap();
        handle
            .dispatch(
                CommandId::new(format!("create:{id}")).unwrap(),
                Command::Create {
                    workspace: None,
                    thread,
                    project: "project".into(),
                    title: id.into(),
                    selection: selection(),
                    runtime_mode: RuntimeMode::FullAccess,
                    interaction_mode: InteractionMode::Default,
                },
                CommandOrigin::Client,
            )
            .await
            .unwrap();
        if id == "thread-b" {
            handle
                .dispatch(
                    CommandId::new("archive").unwrap(),
                    Command::Archive { archived: true },
                    CommandOrigin::Client,
                )
                .await
                .unwrap();
        }
    }
    let rows = store.sweep_candidates(false).unwrap();
    let ids: Vec<_> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["thread-a"]);
    assert!(store.sweep_candidates(true).unwrap().is_empty());
}

//! ScheduledTaskService.test.ts, ScheduledTaskService.schedule.test.ts and the
//! scheduler integration over the store-backed rig.
use super::*;
use crate::executor::tests::{Rig, codex, rig, root_workspace, tid};
use crate::session::tests::fake::Gate;
use agent_domain::{CommandId, MessageAuthor, Role};
use chrono::{Local, TimeZone};
use rusqlite::params;
use std::sync::Arc;

const NOW: &str = "2026-09-09T12:00:00.000Z";

fn at(text: &str) -> Timestamp {
    Timestamp::parse(text).unwrap()
}

fn input(id: Option<&str>, project: &str, schedule: Schedule) -> ScheduledTaskInput {
    ScheduledTaskInput {
        id: id.map(str::to_owned),
        require_existing: false,
        command_id: None,
        title: "Review".into(),
        prompt: "Review the open pull requests.".into(),
        enabled: true,
        schedule,
        project: project.into(),
        thread: None,
        workspace: WorkspaceStrategy::Root { branch: None },
        selection: codex(),
        runtime_mode: RuntimeMode::FullAccess,
        interaction_mode: InteractionMode::Default,
        created_by: MessageAuthor::User,
        creation_source: "desktop".into(),
    }
}

fn minutely() -> Schedule {
    Schedule::Interval { every_ms: 60_000 }
}

fn fixed(time_of_day: &str) -> Schedule {
    Schedule::FixedTime {
        time_of_day: time_of_day.into(),
        weekdays: vec![],
    }
}

struct Harness {
    rig: Rig,
    tasks: Arc<ScheduledTasks>,
}

fn harness() -> Harness {
    let rig = rig();
    rig.clock.set(&at(NOW));
    let tasks = ScheduledTasks::new(
        rig.context.clone(),
        Arc::new(Preparations::default()),
        rig.clock.clone(),
    );
    Harness { rig, tasks }
}

impl Harness {
    /// Puts a saved task into the given run state, as a previous process or
    /// an earlier run would have left it.
    async fn set_run_state(&self, id: &str, next_run_at: Option<&str>, status: &str) {
        let (id, next, status) = (
            id.to_owned(),
            next_run_at.map(str::to_owned),
            status.to_owned(),
        );
        self.rig
            .store
            .write(move |tx| {
                tx.execute(
                    "UPDATE scheduled_tasks SET next_run_at = ?2, last_run_status = ?3
                     WHERE task_id = ?1",
                    params![id, next, status],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    async fn task(&self, id: &str) -> ScheduledTask {
        self.rig.store.scheduled_task(id).unwrap().unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rejects_a_stale_form_save_after_deletion_while_preserving_explicit_id_creates() {
    let h = harness();
    let create = input(
        Some("scheduled-task:edit-after-delete"),
        "project",
        minutely(),
    );
    let created = h.tasks.upsert(create.clone()).await.unwrap();
    let edit = ScheduledTaskInput {
        require_existing: true,
        title: "Edited".into(),
        ..create.clone()
    };
    assert_eq!(h.tasks.upsert(edit.clone()).await.unwrap().title, "Edited");
    h.tasks.delete(&created.id).await.unwrap();

    let failure = h.tasks.upsert(edit).await.unwrap_err();
    assert_eq!(failure.to_string(), "Schedule task not found.");
    assert!(h.tasks.list().await.unwrap().is_empty());

    assert_eq!(h.tasks.upsert(create).await.unwrap().id, created.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_retry_with_the_same_command_id_keeps_one_task_and_history() {
    let h = harness();
    let mut create = input(None, "project", minutely());
    create.command_id = Some(CommandId::new("command:scheduled-create").unwrap());
    let first = h.tasks.upsert(create.clone()).await.unwrap();
    let second = h.tasks.upsert(create).await.unwrap();
    assert_eq!(first.id, "scheduled-task:command:scheduled-create");
    assert_eq!(second.id, first.id);
    assert_eq!(h.tasks.list().await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn preserves_a_due_run_when_a_save_only_pads_the_scheduled_hour() {
    let h = harness();
    let due = Local.with_ymd_and_hms(2026, 7, 1, 9, 0, 0).unwrap();
    let due_at = Timestamp::from_millis(due.timestamp_millis()).unwrap();
    h.rig
        .clock
        .set(&Timestamp::from_millis(due.timestamp_millis() - 1_000).unwrap());
    let created = h
        .tasks
        .upsert(input(None, "project", fixed("9:00")))
        .await
        .unwrap();
    assert_eq!(created.next_run_at.as_ref(), Some(&due_at));

    // Cross the due time before the sweep's next tick.
    h.rig
        .clock
        .set(&Timestamp::from_millis(due.timestamp_millis() + 1_000).unwrap());
    let update = ScheduledTaskInput {
        id: Some(created.id.clone()),
        ..input(None, "project", fixed("09:00"))
    };
    let updated = h.tasks.upsert(update.clone()).await.unwrap();
    assert_eq!(updated.next_run_at.as_ref(), Some(&due_at));
    assert_eq!(
        h.tasks.list().await.unwrap()[0].next_run_at.as_ref(),
        Some(&due_at)
    );

    let rescheduled = h
        .tasks
        .upsert(ScheduledTaskInput {
            schedule: fixed("09:30"),
            ..update
        })
        .await
        .unwrap();
    assert_eq!(
        rescheduled.next_run_at,
        Some(Timestamp::from_millis(due.timestamp_millis() + 30 * 60_000).unwrap())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_only_due_tasks_earliest_first_without_settled_ones() {
    let h = harness();
    for (id, next, enabled, status) in [
        ("due-now", Some(NOW), true, "never"),
        (
            "due-earlier",
            Some("2026-09-09T11:00:00.000Z"),
            true,
            "failed",
        ),
        ("disabled", Some(NOW), false, "never"),
        ("future", Some("2026-09-09T12:00:00.001Z"), true, "never"),
        ("unscheduled", None, true, "never"),
        ("running", Some(NOW), true, "running"),
    ] {
        h.tasks
            .upsert(ScheduledTaskInput {
                enabled,
                ..input(Some(id), "project", minutely())
            })
            .await
            .unwrap();
        h.set_run_state(id, next, status).await;
    }
    let due: Vec<String> = h
        .rig
        .store
        .due_scheduled_tasks(&at(NOW))
        .unwrap()
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert_eq!(due, ["due-earlier", "due-now"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn due_poll_skips_a_corrupt_schedule_without_defecting_healthy_tasks() {
    let h = harness();
    for id in ["due-healthy", "due-corrupt"] {
        h.tasks
            .upsert(input(Some(id), "project", minutely()))
            .await
            .unwrap();
        h.set_run_state(id, Some(NOW), "never").await;
    }
    h.rig
        .store
        .write(|tx| {
            let mut definition: serde_json::Value = tx.query_row(
                "SELECT definition FROM scheduled_tasks WHERE task_id = 'due-corrupt'",
                [],
                |row| row.get(0),
            )?;
            definition["schedule"] = serde_json::json!({
                "type": "fixed_time",
                "timeOfDay": "25:00",
                "weekdays": [],
            });
            tx.execute(
                "UPDATE scheduled_tasks SET definition = ?1 WHERE task_id = 'due-corrupt'",
                [serde_json::to_string(&definition)?],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let due = h
        .rig
        .store
        .due_scheduled_tasks(&at(NOW))
        .unwrap();
    assert_eq!(due.into_iter().map(|task| task.id).collect::<Vec<_>>(), ["due-healthy"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn releases_interrupted_runs_on_startup_and_still_executes_due_tasks() {
    let h = harness();
    let huge = Schedule::Interval {
        every_ms: 9_000_000_000_000_000,
    };
    for (id, schedule, status) in [
        ("stuck-valid", minutely(), "running"),
        ("stuck-huge", huge.clone(), "running"),
        ("due-healthy", minutely(), "never"),
        ("due-huge", huge, "never"),
    ] {
        h.tasks
            .upsert(input(Some(id), "project", schedule))
            .await
            .unwrap();
        h.set_run_state(id, Some(NOW), status).await;
    }
    // A task whose project is gone cannot launch; its run records the failure.
    h.tasks
        .upsert(input(Some("due-failing"), "missing", minutely()))
        .await
        .unwrap();
    h.set_run_state("due-failing", Some(NOW), "never").await;
    h.tasks
        .upsert(input(Some("stuck-corrupt"), "project", minutely()))
        .await
        .unwrap();
    h.rig
        .store
        .write(|tx| {
            tx.execute(
                "UPDATE scheduled_tasks SET definition = '{' , last_run_status = 'running'
                 WHERE task_id = 'stuck-corrupt'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    h.rig.clock.advance(1_000);
    h.tasks.release_interrupted().await.unwrap();
    h.tasks.run_due().await.unwrap();

    // The decodable stuck run is aimed at its next occurrence; the one beyond
    // the representable range is released without a next run.
    let stuck = h.task("stuck-valid").await;
    assert_eq!(stuck.last_run_status, ScheduledTaskRunStatus::Failed);
    assert!(stuck.next_run_at.is_some());
    let stuck_huge = h.task("stuck-huge").await;
    assert_eq!(stuck_huge.last_run_status, ScheduledTaskRunStatus::Failed);
    assert_eq!(stuck_huge.next_run_at, None);
    let corrupt = h
        .rig
        .store
        .read(|c| {
            c.query_row(
                "SELECT last_run_status, last_run_error, run_count
                 FROM scheduled_tasks WHERE task_id = 'stuck-corrupt'",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
        })
        .unwrap();
    assert_eq!(
        corrupt,
        (
            "failed".into(),
            Some("Run was interrupted by a server restart.".into()),
            1
        )
    );
    for id in ["stuck-valid", "stuck-huge"] {
        let task = h.task(id).await;
        assert_eq!(
            task.last_run_error.as_deref(),
            Some("Run was interrupted by a server restart.")
        );
        assert_eq!(task.run_count, 1);
    }
    // The healthy due tasks launched threads and recorded their runs.
    for id in ["due-healthy", "due-huge"] {
        let task = h.task(id).await;
        assert_eq!(
            task.last_run_status,
            ScheduledTaskRunStatus::Succeeded,
            "{id}"
        );
        assert_eq!(task.last_run_error, None);
        assert_eq!(task.run_count, 1);
    }
    assert_eq!(h.task("due-huge").await.next_run_at, None);
    assert_eq!(
        h.task("due-healthy").await.next_run_at,
        Some(at("2026-09-09T12:01:01.000Z"))
    );
    let failing = h.task("due-failing").await;
    assert_eq!(failing.last_run_status, ScheduledTaskRunStatus::Failed);
    assert!(
        failing
            .last_run_error
            .as_deref()
            .is_some_and(|error| error.contains("Project not found")),
        "{failing:?}"
    );
    assert_eq!(failing.run_count, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_launched_as_a_new_thread_carries_the_task_on_its_first_message() {
    let h = harness();
    let task = h
        .tasks
        .upsert(input(Some("daily"), "project", minutely()))
        .await
        .unwrap();
    let ran = h.tasks.run_now(&task.id).await.unwrap();
    assert_eq!(ran.last_run_status, ScheduledTaskRunStatus::Succeeded);
    assert_eq!(ran.run_count, 1);
    assert_eq!(ran.last_run_at, Some(at(NOW)));

    let threads = h.rig.store.sweep_candidates(false).unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].title, "Review");
    let state = h.rig.state(&threads[0].id).await;
    let message = state
        .messages
        .iter()
        .find(|message| message.role == Role::User)
        .unwrap();
    assert_eq!(message.text, "Review the open pull requests.");
    assert_eq!(message.scheduled_task.as_deref(), Some("daily"));
    assert_eq!(message.creation_source, "desktop");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_run_into_a_bound_thread_sends_the_prompt_as_the_task() {
    let h = harness();
    let thread = tid("bound");
    h.rig.create(&thread, Some(root_workspace("/repo"))).await;
    let task = h
        .tasks
        .upsert(ScheduledTaskInput {
            thread: Some(thread.clone()),
            ..input(Some("into-thread"), "project", minutely())
        })
        .await
        .unwrap();
    let ran = h.tasks.run_now(&task.id).await.unwrap();
    assert_eq!(ran.last_run_status, ScheduledTaskRunStatus::Succeeded);
    let state = h.rig.state(&thread).await;
    let message = state.messages.last().unwrap();
    assert_eq!(message.scheduled_task.as_deref(), Some("into-thread"));
    assert_eq!(message.text, "Review the open pull requests.");
    assert_eq!(state.runs.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manual_run_of_a_task_already_running_is_refused() {
    let h = harness();
    let gate = Arc::new(Gate::default());
    let hook_gate = gate.clone();
    *h.rig.ops.folder.lock().unwrap() = Some(Arc::new(move |_| {
        let gate = hook_gate.clone();
        Box::pin(async move {
            gate.pass().await;
            Ok(None)
        })
    }));
    let task = h
        .tasks
        .upsert(input(Some("slow"), "project", minutely()))
        .await
        .unwrap();
    let tasks = h.tasks.clone();
    let first = tokio::spawn(async move { tasks.run_now("slow").await });
    gate.until_arrived(1).await;
    assert_eq!(
        h.task(&task.id).await.last_run_status,
        ScheduledTaskRunStatus::Running
    );
    let refused = h.tasks.run_now("slow").await.unwrap_err();
    assert_eq!(refused.to_string(), "Schedule task is already running.");
    gate.release();
    assert_eq!(
        first.await.unwrap().unwrap().last_run_status,
        ScheduledTaskRunStatus::Succeeded
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_held_dispatch_releases_the_durable_run() {
    let h = harness();
    let gate = Arc::new(Gate::default());
    let hook_gate = gate.clone();
    *h.rig.ops.folder.lock().unwrap() = Some(Arc::new(move |_| {
        let gate = hook_gate.clone();
        Box::pin(async move {
            gate.pass().await;
            Ok(None)
        })
    }));
    let task = h
        .tasks
        .upsert(input(Some("cancelled"), "project", minutely()))
        .await
        .unwrap();
    let tasks = h.tasks.clone();
    let run = tokio::spawn(async move { tasks.run_now(&task.id).await });
    gate.until_arrived(1).await;
    assert_eq!(h.task("cancelled").await.last_run_status, ScheduledTaskRunStatus::Running);

    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    gate.release();

    for _ in 0..100 {
        let after = h.task("cancelled").await;
        if after.last_run_status != ScheduledTaskRunStatus::Running {
            assert_eq!(after.last_run_status, ScheduledTaskRunStatus::Failed);
            assert_eq!(after.last_run_error.as_deref(), Some("Run was cancelled."));
            assert_eq!(after.run_count, 1);
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("cancelled scheduled task remained running");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missed_reschedule_does_not_overwrite_a_newer_due_time() {
    let h = harness();
    let task = h
        .tasks
        .upsert(input(Some("stale-missed"), "project", fixed("09:00")))
        .await
        .unwrap();
    let newer = at("2026-09-10T09:00:00.000Z");
    h.rig
        .store
        .write({
            let id = task.id.clone();
            let newer = newer.clone();
            move |tx| {
                tx.execute(
                    "UPDATE scheduled_tasks SET next_run_at = ?2 WHERE task_id = ?1",
                    params![id, newer.as_str()],
                )?;
                Ok(())
            }
        })
        .await
        .unwrap();
    let changed = h
        .rig
        .store
        .write({
            let id = task.id.clone();
            let expected = task.next_run_at.clone();
            let replacement = at("2026-09-11T09:00:00.000Z");
            let updated_at = at(NOW);
            move |tx| store::reschedule(tx, &id, expected, Some(replacement), &updated_at)
        })
        .await
        .unwrap();
    assert!(!changed);
    assert_eq!(h.task("stale-missed").await.next_run_at, Some(newer));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_and_recreated_task_is_not_stamped_by_the_old_run() {
    let h = harness();
    let gate = Arc::new(Gate::default());
    let hook_gate = gate.clone();
    *h.rig.ops.folder.lock().unwrap() = Some(Arc::new(move |_| {
        let gate = hook_gate.clone();
        Box::pin(async move {
            gate.pass().await;
            Ok(None)
        })
    }));
    let task = h
        .tasks
        .upsert(input(Some("replace"), "project", minutely()))
        .await
        .unwrap();
    let tasks = h.tasks.clone();
    let first = tokio::spawn(async move { tasks.run_now(&task.id).await });
    gate.until_arrived(1).await;
    assert_eq!(h.task("replace").await.last_run_status, ScheduledTaskRunStatus::Running);

    h.tasks.delete("replace").await.unwrap();
    let replacement = h
        .tasks
        .upsert(ScheduledTaskInput {
            title: "Replacement".into(),
            ..input(Some("replace"), "project", minutely())
        })
        .await
        .unwrap();
    gate.release();

    let result = first.await.unwrap().unwrap();
    assert_eq!(result, replacement);
    assert_eq!(h.task("replace").await, replacement);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_postponed_before_its_run_is_read_again_is_not_fired() {
    let h = harness();
    for id in ["run-a", "run-b-victim", "run-c"] {
        h.tasks
            .upsert(input(Some(id), "project", minutely()))
            .await
            .unwrap();
        h.set_run_state(id, Some(NOW), "never").await;
    }
    // While the first task launches, the second is postponed by an edit.
    let tasks = h.tasks.clone();
    *h.rig.ops.folder.lock().unwrap() = Some(Arc::new(move |_| {
        let tasks = tasks.clone();
        Box::pin(async move {
            if let Ok(victim) = tasks.load("run-b-victim").await
                && victim.run_count == 0
            {
                tasks
                    .upsert(ScheduledTaskInput {
                        id: Some(victim.id),
                        schedule: Schedule::Interval { every_ms: 120_000 },
                        ..input(None, "project", minutely())
                    })
                    .await
                    .unwrap();
            }
            Ok(None)
        })
    }));
    h.rig.clock.advance(1_000);
    h.tasks.run_due().await.unwrap();
    let victim = h.task("run-b-victim").await;
    assert_eq!(victim.last_run_status, ScheduledTaskRunStatus::Never);
    assert_eq!(victim.run_count, 0);
    for id in ["run-a", "run-c"] {
        assert_eq!(h.task(id).await.run_count, 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn fixed_time_runs_missed_by_more_than_the_grace_window_are_rescheduled() {
    let h = harness();
    let due = Local.with_ymd_and_hms(2026, 7, 1, 9, 0, 0).unwrap();
    h.rig
        .clock
        .set(&Timestamp::from_millis(due.timestamp_millis() - 60_000).unwrap());
    let task = h
        .tasks
        .upsert(input(Some("morning"), "project", fixed("09:00")))
        .await
        .unwrap();
    // The Host wakes six hours late.
    h.rig
        .clock
        .set(&Timestamp::from_millis(due.timestamp_millis() + 6 * 3_600_000).unwrap());
    h.tasks.run_due().await.unwrap();
    let after = h.task(&task.id).await;
    assert_eq!(after.run_count, 0);
    assert_eq!(after.last_run_status, ScheduledTaskRunStatus::Never);
    let next_day = Local.with_ymd_and_hms(2026, 7, 2, 9, 0, 0).unwrap();
    assert_eq!(
        after.next_run_at,
        Some(Timestamp::from_millis(next_day.timestamp_millis()).unwrap())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn pausing_clears_the_next_run_and_resuming_restarts_the_clock() {
    let h = harness();
    let task = h
        .tasks
        .upsert(input(Some("toggle"), "project", minutely()))
        .await
        .unwrap();
    assert_eq!(task.next_run_at, Some(at("2026-09-09T12:01:00.000Z")));
    let paused = h.tasks.set_enabled("toggle", false).await.unwrap();
    assert!(!paused.enabled);
    assert_eq!(paused.next_run_at, None);
    h.rig.clock.advance(30_000);
    let resumed = h.tasks.set_enabled("toggle", true).await.unwrap();
    assert_eq!(resumed.next_run_at, Some(at("2026-09-09T12:01:30.000Z")));
    assert_eq!(h.task("toggle").await, resumed);
    h.tasks.delete("toggle").await.unwrap();
    assert_eq!(
        h.tasks
            .set_enabled("toggle", false)
            .await
            .unwrap_err()
            .to_string(),
        "Schedule task not found."
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn every_change_wakes_the_list_subscribers() {
    let h = harness();
    let mut changes = h.tasks.subscribe();
    let before = *changes.borrow_and_update();
    h.tasks
        .upsert(input(Some("watched"), "project", minutely()))
        .await
        .unwrap();
    changes.changed().await.unwrap();
    assert!(*changes.borrow_and_update() > before);
    let listed = h.tasks.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "watched");
}

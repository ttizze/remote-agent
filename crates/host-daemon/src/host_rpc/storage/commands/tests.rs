use super::*;

const NOW: &str = "2026-11-14T00:00:00.000Z";
fn command(id: &str) -> Command<'_> {
    Command {
        id,
        thread_id: "thread",
        kind: "message.dispatch",
        now: NOW,
        item_ids: &[],
    }
}
use super::super::test_support::{database, thread};

#[test]
fn committed_admission_is_atomic_and_repeating_its_command_cannot_reexecute_the_decision() {
    let mut db = database();
    let seed = Uuid::from_u128(1);
    let id = seed.to_string();
    let first = execute(&mut db, command(&id), Some(&thread("thread")), |state| {
        model::admit(
            state,
            model::Input {
                message_id: "message".into(),
                created_by: model::Actor::User,
                creation_source: model::CreationSource::Mobile,
                text: "hello".into(),
                attachments: Vec::new(),
                context: None,
                model_selection: state.thread.model_selection.clone(),
                driver: "codex".into(),
                workspace_preparation: None,
                fork_origin: None,
            },
            model::Admission::Queue,
            model::Context {
                command_id: &id,
                now: NOW,
                next_run_ordinal: 1,
                can_steer: false,
            },
        )
    })
    .unwrap();
    assert!(first.receipt.error.is_none());
    let events = event_store::read(&db, None, 0, 100, 100).unwrap();
    assert!(!events.is_empty());
    let saved = projections::load(&db, "thread", &[]).unwrap();
    assert_eq!(saved.runs.len(), 1);
    assert_eq!(saved.run_attempts.len(), 1);
    assert_eq!(saved.turn_items.len(), 1);
    assert_eq!(saved.nodes.len(), 1);
    assert_eq!(saved.effects.len(), 1);
    let repeated = execute(&mut db, command(&id), None, |_| {
        panic!("receipted decision must not run")
    })
    .unwrap();
    assert_eq!(first.receipt, repeated.receipt);
    assert!(repeated.events.is_empty());
    assert_eq!(event_store::read(&db, None, 0, 100, 100).unwrap(), events);
    assert_eq!(projections::load(&db, "thread", &[]).unwrap(), saved);
    assert!(receipt(&db, &id, "other", "message.dispatch").is_err());
    assert!(receipt(&db, &id, "thread", "thread.stop").is_err());
}

#[test]
fn a_rejection_is_durable_and_a_failed_commit_leaves_no_receipt_or_partial_projection() {
    let mut db = database();
    execute(&mut db, command("create"), Some(&thread("thread")), |_| {
        Ok(model::Decision::default())
    })
    .unwrap();
    let rejected = execute(&mut db, command("rejected"), None, |_| {
        Err("input is invalid".into())
    })
    .unwrap();
    assert_eq!(rejected.receipt.error.as_deref(), Some("input is invalid"));
    let repeated = execute(&mut db, command("rejected"), None, |_| {
        panic!("rejection must not be recomputed")
    })
    .unwrap();
    assert_eq!(repeated.receipt, rejected.receipt);
    assert_eq!(event_store::head(&db, None).unwrap(), 1);
    let before = projections::read::<model::Thread>(&db, "thread")
        .unwrap()
        .unwrap();
    let failed = execute(&mut db, command("invalid-ref"), None, |state| {
        Ok(model::Decision {
            changes: vec![model::Change::Thread(model::Thread {
                title: "must roll back".into(),
                active_provider_thread_id: Some("absent".into()),
                ..state.thread.clone()
            })],
            effects: Vec::new(),
        })
    });
    assert!(failed.is_err());
    assert!(
        receipt(&db, "invalid-ref", "thread", "message.dispatch")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        projections::read::<model::Thread>(&db, "thread").unwrap(),
        Some(before)
    );
    let events = event_store::read(&db, None, 0, 100, 100).unwrap();
    assert_eq!(events.len(), 1);
}

fn input_decision(
    state: &model::State,
    id: &str,
    message: &str,
    provider: &str,
) -> Result<model::Decision, String> {
    model::admit(
        state,
        model::Input {
            message_id: message.into(),
            created_by: model::Actor::User,
            creation_source: model::CreationSource::Mobile,
            text: message.into(),
            attachments: vec![],
            context: None,
            model_selection: model::ModelSelection {
                instance_id: provider.into(),
                model: "example".into(),
                options: vec![],
            },
            driver: provider.into(),
            workspace_preparation: None,
            fork_origin: None,
        },
        model::Admission::Queue,
        context(id, state),
    )
}

fn context<'a>(id: &'a str, state: &model::State) -> model::Context<'a> {
    model::Context {
        command_id: id,
        now: NOW,
        next_run_ordinal: state
            .runs
            .values()
            .map(|run| run.ordinal)
            .max()
            .unwrap_or(0)
            + 1,
        can_steer: false,
    }
}

fn seed_threads(db: &mut Connection) {
    for name in ["source", "thread"] {
        execute(
            db,
            Command {
                thread_id: name,
                ..command(name)
            },
            Some(&thread(name)),
            |_| Ok(model::Decision::default()),
        )
        .unwrap();
    }
}

fn append(db: &mut Connection, decision: model::Decision) {
    let state = projections::load(db, "thread", &[]).unwrap();
    let tx = db.transaction().unwrap();
    append_decision(&tx, &state, decision, None, NOW).unwrap();
    tx.commit().unwrap();
}

fn dangling_fork() -> model::Change {
    // A valid JSON row can still refer to source execution data that cannot
    // be read. This fault must not roll back another execution's completion.
    model::Change::ContextTransfer(serde_json::from_value(serde_json::json!({
        "id":"unavailable-source","type":"fork","sourceThreadId":"source","targetThreadId":"thread",
        "sourcePoint":{"threadId":"source","runId":"missing"},"basePoint":null,
        "sourceProviderInstanceId":"codex","targetProviderInstanceId":null,"targetRunId":null,
        "status":"pending","resolution":null,"createdBy":"user","error":null,
        "createdAt":NOW,"updatedAt":NOW,"consumedAt":null
    })).unwrap())
}

#[test]
fn a_context_preparation_rejection_is_durable_without_partial_admission() {
    let mut db = database();
    seed_threads(&mut db);
    append(
        &mut db,
        model::Decision {
            changes: vec![dangling_fork()],
            effects: vec![],
        },
    );
    let before = event_store::head(&db, Some("thread")).unwrap();
    let id = Uuid::new_v4().to_string();
    let ack = execute(&mut db, command(&id), None, |state| {
        input_decision(state, &id, "new-input", "codex")
    })
    .expect("a logical context rejection is an acknowledged command result");
    assert!(ack.receipt.error.is_some());
    let repeated = execute(&mut db, command(&id), None, |_| {
        panic!("a rejected operation cannot re-run its decision")
    })
    .unwrap();
    assert_eq!(repeated, ack);
    let saved = projections::load(&db, "thread", &[]).unwrap();
    assert!(saved.runs.is_empty() && saved.turn_items.is_empty() && saved.effects.is_empty());
    assert_eq!(event_store::head(&db, Some("thread")).unwrap(), before);
}

#[test]
fn queued_context_failure_preserves_the_preceding_native_completion() {
    let mut db = database();
    seed_threads(&mut db);
    let id = Uuid::new_v4().to_string();
    execute(&mut db, command(&id), None, |state| {
        input_decision(state, &id, "first", "codex")
    })
    .unwrap();
    super::super::outbox::claim(&db, "worker", NOW)
        .unwrap()
        .unwrap();
    let state = projections::load(&db, "thread", &[]).unwrap();
    let run = state.active_run().unwrap().clone();
    let attempt = run.active_attempt_id.as_deref().unwrap();
    let provider_thread = state.provider_threads[run.provider_thread_id.as_ref().unwrap()].clone();
    let session:model::ProviderSession=serde_json::from_value(serde_json::json!({"id":"session","driver":"codex","providerInstanceId":"codex","status":"ready","cwd":"/project","model":null,"capabilities":model::ProviderCapabilities::default(),"createdAt":NOW,"updatedAt":NOW,"lastError":null})).unwrap();
    let native = model::ProviderRef {
        driver: "codex".into(),
        native_id: Some("native-thread".into()),
        strength: model::NativeRefStrength::Strong,
        fingerprint: None,
        ordinal: None,
    };
    let mut bound = model::Decision {
        changes: vec![
            model::Change::ProviderSessionAttached(session),
            model::Change::ProviderThread(model::ProviderThread {
                provider_session_id: Some("session".into()),
                native_thread_ref: Some(native.clone()),
                ..provider_thread
            }),
        ],
        effects: vec![],
    };
    let current = state.apply(&bound);
    let started = model::started(
        &current,
        attempt,
        "session",
        model::ProviderRef {
            native_id: Some("native-turn".into()),
            ..native
        },
        context("native-started", &current),
    )
    .unwrap();
    bound.changes.extend(started.changes);
    bound.effects.extend(started.effects);
    append(&mut db, bound);
    append(
        &mut db,
        model::Decision {
            changes: vec![dangling_fork()],
            effects: vec![],
        },
    );
    let queued_id = Uuid::new_v4().to_string();
    execute(&mut db, command(&queued_id), None, |state| {
        input_decision(state, &queued_id, "queued", "claude")
    })
    .unwrap();
    let state = projections::load(&db, "thread", &[]).unwrap();
    let queued = state
        .runs
        .values()
        .find(|run| run.status == model::RunStatus::Queued)
        .unwrap()
        .id
        .clone();
    let completed = model::complete(
        &state,
        attempt,
        "session",
        model::Completion::Completed,
        context("native-completed", &state),
    )
    .unwrap();
    let effect=completed.effects.iter().find(|effect|matches!(&effect.request,model::EffectRequest::ProviderTurnStart {run_id} if run_id==&queued)).unwrap().id.clone();
    let result = {
        let tx = db.transaction().unwrap();
        append_decision(&tx, &state, completed, None, NOW)
            .and_then(|_| tx.commit().map_err(failure))
    };
    assert!(
        result.is_ok(),
        "the preceding completion survives failure of its queued successor: {result:?}"
    );
    let saved = projections::load(&db, "thread", &[]).unwrap();
    assert_eq!(saved.runs[&run.id].status, model::RunStatus::Completed);
    assert_eq!(saved.runs[&queued].status, model::RunStatus::Failed);
    assert!(db.query_row("SELECT count(*) FROM orchestration_v2_projection_turn_items WHERE run_id=?1 AND type='error'",[&queued],|row|row.get::<_,i64>(0)).unwrap()>0);

    assert_eq!(
        db.query_row(
            "SELECT status FROM orchestration_v2_effect_outbox WHERE effect_id=?1",
            [effect],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "failed"
    );
}

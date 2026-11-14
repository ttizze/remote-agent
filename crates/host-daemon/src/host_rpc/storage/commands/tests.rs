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
    assert!(first.error.is_none());
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
    assert_eq!(first, repeated);
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
    assert_eq!(rejected.error.as_deref(), Some("input is invalid"));
    let repeated = execute(&mut db, command("rejected"), None, |_| {
        panic!("rejection must not be recomputed")
    })
    .unwrap();
    assert_eq!(repeated, rejected);
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

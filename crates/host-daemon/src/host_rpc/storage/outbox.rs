//! T3's per-thread FIFO effect lane. A running native write is never reclaimed
//! on a timer: process recovery must distinguish replay-safe work from writes
//! whose delivery is unknown.
use super::*;
use agent_protocol::orchestration as model;

pub(super) fn claim(
    connection: &Connection,
    worker_id: &str,
    now: &str,
) -> Result<Option<model::Effect>, String> {
    let mut query = connection.prepare(&format!(
        "UPDATE orchestration_v2_effect_outbox
         SET status='running',attempt_count=attempt_count+1,lease_owner=?1,updated_at=?2,last_error=NULL
         WHERE effect_id=(
           SELECT candidate.effect_id FROM orchestration_v2_effect_outbox AS candidate
           WHERE candidate.status='pending' AND candidate.available_at<=?2
             AND NOT EXISTS (
               SELECT 1 FROM orchestration_v2_effect_outbox AS active
               WHERE active.thread_id=candidate.thread_id
                 AND (active.status='running' OR (active.status='pending' AND active.rowid<candidate.rowid))
             )
           ORDER BY candidate.available_at,candidate.created_at,candidate.effect_id LIMIT 1
         ) RETURNING {}", event_store::EFFECT_COLUMNS)).map_err(failure)?;
    let mut rows = query.query(params![worker_id, now]).map_err(failure)?;
    rows.next()
        .map_err(failure)?
        .map(event_store::effect)
        .transpose()
}

pub(in crate::host_rpc) enum Settlement<'a> {
    Succeeded,
    Failed(&'a str),
    Retry {
        available_at: &'a str,
        error: &'a str,
    },
    /// Keep the lane occupied until native evidence or explicit resolution.
    DeliveryUnknown(&'a str),
}

impl Storage {
    pub(in crate::host_rpc) fn next_effect_at(&self) -> Result<Option<String>, String> {
        self.with(|connection|connection.query_row("SELECT min(candidate.available_at) FROM orchestration_v2_effect_outbox candidate WHERE candidate.status='pending' AND NOT EXISTS (SELECT 1 FROM orchestration_v2_effect_outbox active WHERE active.thread_id=candidate.thread_id AND (active.status='running' OR (active.status='pending' AND active.rowid<candidate.rowid)))",[],|row|row.get(0)).map_err(failure))
    }
    pub(in crate::host_rpc) fn recover_effects(&self, now: &str) -> Result<(), String> {
        self.with(|connection| reconcile_after_process_loss(connection, now))
    }

    pub(in crate::host_rpc) fn observe_effect(
        &self,
        effect: &model::Effect,
        worker: &str,
        now: &str,
        decide: impl FnOnce(&model::State) -> Result<model::Decision, String>,
    ) -> Result<Vec<model::StoredEvent>, String> {
        self.observe_thread(&effect.thread_id, &[], now, |state| {
            if !state.effects.get(&effect.id).is_some_and(|saved| {
                saved.status == model::EffectStatus::Running
                    && saved.lease_owner.as_deref() == Some(worker)
            }) {
                return Ok(model::Decision::default());
            }
            decide(state)
        })
    }

    pub(in crate::host_rpc) fn claim_effect(
        &self,
        worker: &str,
        now: &str,
    ) -> Result<Option<model::Effect>, String> {
        self.with(|connection| claim(connection, worker, now))
    }

    pub(in crate::host_rpc) fn finish_effect(
        &self,
        id: &str,
        worker: Option<&str>,
        now: &str,
        result: Settlement<'_>,
    ) -> Result<bool, String> {
        self.with(|connection| finish(connection, id, worker, now, result))
    }

    pub(in crate::host_rpc) fn effect_state(
        &self,
        effect: &model::Effect,
        worker: &str,
    ) -> Result<Option<model::State>, String> {
        self.with(|connection| {
            let state = projections::load(connection, &effect.thread_id, &[])?;
            Ok(state
                .effects
                .get(&effect.id)
                .is_some_and(|effect| {
                    effect.status == model::EffectStatus::Running
                        && effect.lease_owner.as_deref() == Some(worker)
                })
                .then_some(state))
        })
    }

    pub(in crate::host_rpc) fn thread_state(
        &self,
        thread_id: &str,
        item_ids: &[String],
    ) -> Result<model::State, String> {
        self.with(|connection| projections::load(connection, thread_id, item_ids))
    }

    pub(in crate::host_rpc) fn attempt_owner(
        &self,
        attempt_id: &str,
    ) -> Result<Option<SessionRef>, String> {
        self.with(|connection|connection.query_row("SELECT thread_id FROM orchestration_v2_projection_run_attempts WHERE attempt_id=?1",[attempt_id],|row|row.get::<_,String>(0))
            .optional().map_err(failure)?.map(|id|SessionRef::new(None,id).map_err(str::to_owned)).transpose())
    }
}

pub(super) fn finish(
    connection: &Connection,
    effect_id: &str,
    worker_id: Option<&str>,
    now: &str,
    result: Settlement<'_>,
) -> std::result::Result<bool, String> {
    let (status, error, available_at) = match result {
        Settlement::Succeeded => ("succeeded", None, None),
        Settlement::Failed(error) => ("failed", Some(error), None),
        Settlement::Retry {
            available_at,
            error,
        } => ("pending", Some(error), Some(available_at)),
        Settlement::DeliveryUnknown(error) => ("running", Some(error), None),
    };
    connection
        .execute(
            "UPDATE orchestration_v2_effect_outbox
         SET status=?4,last_error=?5,available_at=coalesce(?6,available_at),updated_at=?3,
             lease_owner=NULL,
             completed_at=CASE WHEN ?4 IN ('succeeded','failed') THEN ?3 ELSE NULL END
         WHERE effect_id=?1 AND status='running' AND lease_owner IS ?2",
            params![effect_id, worker_id, now, status, error, available_at],
        )
        .map(|count| count == 1)
        .map_err(failure)
}

/// Called once by the process owner, before workers start. T3 cancels process
/// bound work and requeues replay-safe effects. Writes possibly delivered keep
/// the lane blocked until evidence or an explicit user resolution fences them.
pub(super) fn reconcile_after_process_loss(
    connection: &mut Connection,
    now: &str,
) -> Result<(), String> {
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(failure)?;
    let uncertain = {
        let mut query=tx.prepare("SELECT effect_id,thread_id FROM orchestration_v2_effect_outbox WHERE status='running' AND lease_owner IS NOT NULL AND effect_type IN ('provider-turn.start','provider-turn.steer','provider-turn.restart') ORDER BY rowid").map_err(failure)?;
        query
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(failure)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(failure)?
    };
    for (effect_id, thread_id) in uncertain {
        let state = projections::load(&tx, &thread_id, &[])?;
        match model::uncertain_delivery(
            &state,
            &effect_id,
            "Delivery not confirmed after the previous server process ended.",
            now,
        ) {
            Ok(decision) => {
                commands::append_decision(&tx, &state, decision, None, now)?;
            }
            Err(error) => {
                tracing::error!(target:"agent",operation="outbox.recover-input",effect=%effect_id,%error);
                finish(
                    &tx,
                    &effect_id,
                    state.effects[&effect_id].lease_owner.as_deref(),
                    now,
                    Settlement::Failed(&error),
                )?;
            }
        }
    }
    tx.execute("UPDATE orchestration_v2_effect_outbox SET status='cancelled',lease_owner=NULL,completed_at=?1,updated_at=?1,last_error='Cancelled because the server process ended before the effect completed.'
        WHERE (status='pending' OR (status='running' AND lease_owner IS NOT NULL))
          AND effect_type IN ('provider-session.apply-model-selection','provider-turn.start','provider-turn.interrupt','provider-turn.steer','provider-turn.restart','runtime-request.respond')",[now]).map_err(failure)?;
    tx.execute("UPDATE orchestration_v2_effect_outbox SET status='pending',lease_owner=NULL,available_at=?1,updated_at=?1,last_error='Requeued after the previous server process ended.'
        WHERE status='running' AND effect_type IN ('provider-runtime.continue','provider-session.detach','provider-thread.rollback','checkpoint.capture')",[now]).map_err(failure)?;
    tx.commit().map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: &str = "2026-11-14T00:00:00.000Z";
    const LATER: &str = "2026-11-14T00:00:01.000Z";

    fn database() -> Connection {
        let mut db = super::super::test_support::database();
        let tx = db.transaction().unwrap();
        for id in ["thread", "other"] {
            projections::save(&tx, None, &super::super::test_support::thread(id)).unwrap();
        }
        tx.commit().unwrap();
        db
    }

    fn enqueue(db: &mut Connection, id: &str, thread: &str) {
        let tx = db.transaction().unwrap();
        event_store::save_effect(
            &tx,
            &model::Effect {
                id: id.into(),
                command_id: format!("command:{id}"),
                thread_id: thread.into(),
                request: model::EffectRequest::ProviderTurnStart {
                    run_id: format!("run:{id}"),
                },
                status: model::EffectStatus::Pending,
                attempt_count: 0,
                available_at: NOW.into(),
                lease_owner: None,
                lease_expires_at: None,
                created_at: NOW.into(),
                updated_at: NOW.into(),
                completed_at: None,
                last_error: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn each_thread_is_serial_and_a_backing_off_head_blocks_later_work() {
        let mut db = database();
        enqueue(&mut db, "first", "thread");
        enqueue(&mut db, "second", "thread");
        enqueue(&mut db, "other", "other");
        let first = claim(&db, "worker", NOW).unwrap().unwrap();
        assert_eq!(first.id, "first");
        assert_eq!(first.attempt_count, 1);
        assert_eq!(first.lease_expires_at, None);
        assert_eq!(
            claim(&db, "other-worker", NOW).unwrap().unwrap().id,
            "other"
        );
        assert!(claim(&db, "worker", LATER).unwrap().is_none());
        assert!(
            finish(
                &db,
                "first",
                Some("worker"),
                NOW,
                Settlement::Retry {
                    available_at: LATER,
                    error: "temporary"
                }
            )
            .unwrap()
        );
        assert!(claim(&db, "worker", NOW).unwrap().is_none());
        let retry = claim(&db, "worker", LATER).unwrap().unwrap();
        assert_eq!(retry.id, "first");
        assert_eq!(retry.attempt_count, 2);
        assert_eq!(retry.last_error, None);
        assert!(finish(&db, "first", Some("worker"), LATER, Settlement::Succeeded).unwrap());
        assert_eq!(claim(&db, "worker", LATER).unwrap().unwrap().id, "second");
    }

    #[test]
    fn only_the_claiming_worker_can_settle_and_unknown_delivery_never_releases_the_lane() {
        let mut db = database();
        enqueue(&mut db, "first", "thread");
        enqueue(&mut db, "second", "thread");
        claim(&db, "owner", NOW).unwrap().unwrap();
        for result in [
            Settlement::Succeeded,
            Settlement::Failed("failure"),
            Settlement::Retry {
                available_at: NOW,
                error: "retry",
            },
            Settlement::DeliveryUnknown("unknown"),
        ] {
            assert!(!finish(&db, "first", Some("stale"), NOW, result).unwrap());
        }
        assert!(
            finish(
                &db,
                "first",
                Some("owner"),
                NOW,
                Settlement::DeliveryUnknown("write acknowledgement lost")
            )
            .unwrap()
        );
        assert!(claim(&db, "new-process", LATER).unwrap().is_none());
        assert!(!finish(&db, "first", Some("owner"), LATER, Settlement::Succeeded).unwrap());
        let pending = event_store::effects(&db, "thread").unwrap();
        assert_eq!(pending["first"].status, model::EffectStatus::Running);
        assert_eq!(
            pending["first"].last_error.as_deref(),
            Some("write acknowledgement lost")
        );
        assert!(finish(&db, "first", None, LATER, Settlement::Failed("resolved")).unwrap());
        assert!(!finish(&db, "first", Some("owner"), LATER, Settlement::Succeeded).unwrap());
        assert_eq!(
            claim(&db, "new-process", LATER).unwrap().unwrap().id,
            "second"
        );
    }

    #[test]
    fn repeated_registration_cannot_reset_a_lease_or_reexecute_a_completed_effect() {
        let mut db = database();
        enqueue(&mut db, "first", "thread");
        claim(&db, "owner", NOW).unwrap().unwrap();
        enqueue(&mut db, "first", "thread");
        let running = event_store::effects(&db, "thread").unwrap();
        assert_eq!(running["first"].status, model::EffectStatus::Running);
        assert_eq!(running["first"].lease_owner.as_deref(), Some("owner"));
        assert_eq!(running["first"].attempt_count, 1);
        assert!(finish(&db, "first", Some("owner"), LATER, Settlement::Succeeded).unwrap());
        enqueue(&mut db, "first", "thread");
        assert!(claim(&db, "another", LATER).unwrap().is_none());
    }

    #[test]
    fn process_loss_requeues_safe_work_cancels_unsent_work_and_leaves_native_writes_for_resolution()
    {
        let mut db = database();
        let tx = db.transaction().unwrap();
        let state = projections::load(&tx, "thread", &[]).unwrap();
        let mut admitted = model::admit(
            &state,
            model::Input {
                message_id: "first-input".into(),
                created_by: model::Actor::User,
                creation_source: model::CreationSource::Mobile,
                text: "hello".into(),
                attachments: vec![],
                context: None,
                model_selection: state.thread.model_selection.clone(),
                driver: "codex".into(),
                workspace_preparation: None,
            },
            model::Admission::Queue,
            model::Context {
                command_id: "first",
                now: NOW,
                next_run_ordinal: 1,
                can_steer: false,
            },
        )
        .unwrap();
        admitted.effects[0].id = "first".into();
        commands::append_decision(&tx, &state, admitted, None, NOW).unwrap();
        tx.commit().unwrap();
        enqueue(&mut db, "unsent", "thread");
        let saved = event_store::effects(&db, "thread").unwrap()["first"].clone();
        let tx = db.transaction().unwrap();
        event_store::save_effect(
            &tx,
            &model::Effect {
                id: "checkpoint".into(),
                command_id: "checkpoint".into(),
                thread_id: "other".into(),
                request: model::EffectRequest::CheckpointCapture {
                    run_id: "run".into(),
                    scope_id: "scope".into(),
                },
                ..saved
            },
        )
        .unwrap();
        tx.commit().unwrap();
        claim(&db, "old", NOW).unwrap().unwrap();
        claim(&db, "old", NOW).unwrap().unwrap();
        reconcile_after_process_loss(&mut db, LATER).unwrap();
        let unknown = event_store::effects(&db, "thread").unwrap();
        assert_eq!(unknown["first"].status, model::EffectStatus::Running);
        assert_eq!(unknown["first"].lease_owner, None);
        let state = projections::load(&db, "thread", &[]).unwrap();
        assert!(state.turn_items.values().any(|item|matches!(&item.body,model::TurnItemBody::Error {failure,..} if failure.class=="delivery_uncertain")));
        let cancelled = model::cancel_uncertain(
            &state,
            "first-input",
            model::Context {
                command_id: "cancel",
                now: LATER,
                next_run_ordinal: 2,
                can_steer: false,
            },
        )
        .unwrap();
        assert!(!state.apply(&cancelled).has_uncertain_input());
        assert!(!unknown.contains_key("unsent"));
        assert_eq!(claim(&db, "new", LATER).unwrap().unwrap().id, "checkpoint");
        assert!(claim(&db, "new", LATER).unwrap().is_none());
        assert!(!finish(&db, "first", Some("old"), LATER, Settlement::Succeeded).unwrap());
        assert!(finish(&db, "first", None, LATER, Settlement::Succeeded).unwrap());
    }
}

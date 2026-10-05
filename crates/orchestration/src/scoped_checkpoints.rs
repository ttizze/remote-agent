//! Generic scope registration and durable captures do not finalize root application runs.
use super::*;
use crate::checkpoint_scope;

impl Store {
    pub fn ensure_checkpoint_scope(
        &self,
        mut scope: CheckpointScope,
        now: &Timestamp,
    ) -> Result<Commit> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let p = load_projection(&tx, &scope.thread_id)?.ok_or(StoreError::ThreadNotFound)?;
        let foreign:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM orchestration_v2_projection_checkpoint_scopes WHERE scope_id=?1 AND thread_id!=?2)",params![scope.id.as_str(),scope.thread_id.as_str()],|row|row.get(0))?;
        if foreign {
            return Err(StoreError::InvalidEvent(
                "scope id belongs to another thread".into(),
            ));
        }
        if p.thread.deleted_at.is_some() || p.thread.rollback_request_id.is_some() {
            return Err(StoreError::InvalidEvent(
                "checkpoint scope is unavailable".into(),
            ));
        }
        checkpoint_scope::validate(
            &scope,
            &p.thread.id,
            &p.nodes,
            &p.runs,
            &p.provider_threads,
            &p.checkpoint_scopes,
        )?;
        if let Some(existing) = p.checkpoint_scopes.iter().find(|s| s.id == scope.id) {
            scope.created_at = existing.created_at.clone();
        }
        let mut node = p
            .nodes
            .iter()
            .find(|n| n.id == scope.node_id)
            .expect("validated node")
            .clone();
        node.checkpoint_scope_id = Some(scope.id.clone());
        let payloads = if p.checkpoint_scopes.iter().any(|s| s == &scope)
            && p.nodes.iter().any(|n| n == &node)
        {
            vec![]
        } else {
            vec![
                EventPayload::CheckpointScopeCreated(scope.clone()),
                EventPayload::NodeUpdated(node),
            ]
        };
        let sequence = latest_sequence(&tx, None)?;
        let events = commit_decision(
            &tx,
            Decision {
                events: crate::events(
                    &scope.thread_id,
                    &format!("scope:{}:{sequence}", scope.id),
                    payloads,
                    now,
                ),
                ..Decision::default()
            },
            None,
            now,
        )?;
        let sequence = latest_sequence(&tx, None)?;
        tx.commit()?;
        self.publish(&events);
        Ok(Commit {
            sequence,
            events,
            replayed: false,
        })
    }
    pub fn ingest_scope_baseline(&self, checkpoint: Checkpoint, now: &Timestamp) -> Result<Commit> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let p = load_projection(&tx, &checkpoint.thread_id)?.ok_or(StoreError::ThreadNotFound)?;
        let scope = p
            .checkpoint_scopes
            .iter()
            .find(|s| s.id == checkpoint.scope_id)
            .ok_or_else(|| StoreError::InvalidEvent("scope missing".into()))?;
        checkpoint_scope::validate(
            scope,
            &p.thread.id,
            &p.nodes,
            &p.runs,
            &p.provider_threads,
            &p.checkpoint_scopes,
        )?;
        if checkpoint.id
            != crate::checkpoint::scope_ordinal_id(&scope.id, checkpoint.ordinal_within_scope)
            || checkpoint.run_id.is_some()
            || checkpoint.node_id != scope.node_id
            || checkpoint.app_run_ordinal
                != (scope.advances_app_run_count && checkpoint.ordinal_within_scope == 0)
                    .then_some(0)
            || checkpoint.parent_checkpoint_id.is_some()
            || p.thread.deleted_at.is_some()
            || p.thread.rollback_request_id.is_some()
        {
            return Err(StoreError::InvalidEvent(
                "scope baseline ownership is invalid".into(),
            ));
        }
        let ready = p
            .checkpoints
            .iter()
            .any(|c| c.id == checkpoint.id && c.status == CheckpointStatus::Ready);
        let events = commit_decision(
            &tx,
            Decision {
                events: if ready {
                    vec![]
                } else {
                    crate::events(
                        &checkpoint.thread_id.clone(),
                        &format!(
                            "baseline:{}:{}",
                            checkpoint.scope_id,
                            latest_sequence(&tx, None)?
                        ),
                        vec![EventPayload::CheckpointCaptured(checkpoint)],
                        now,
                    )
                },
                ..Decision::default()
            },
            None,
            now,
        )?;
        let sequence = latest_sequence(&tx, None)?;
        tx.commit()?;
        self.publish(&events);
        Ok(Commit {
            sequence,
            events,
            replayed: ready,
        })
    }
    pub fn schedule_scope_capture(
        &self,
        thread: &ThreadId,
        mut capture: CheckpointCapture,
        now: &Timestamp,
    ) -> Result<Commit> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let p = load_projection(&tx, thread)?.ok_or(StoreError::ThreadNotFound)?;
        let scope = p
            .checkpoint_scopes
            .iter()
            .find(|s| s.id == capture.scope_id)
            .ok_or_else(|| StoreError::InvalidEvent("scope missing".into()))?;
        checkpoint_scope::validate(
            scope,
            thread,
            &p.nodes,
            &p.runs,
            &p.provider_threads,
            &p.checkpoint_scopes,
        )?;
        if capture.parent_checkpoint_id.is_none() {
            capture.parent_checkpoint_id = p
                .checkpoints
                .iter()
                .filter(|c| {
                    c.scope_id == scope.id
                        && c.status == CheckpointStatus::Ready
                        && c.ordinal_within_scope < capture.ordinal_within_scope
                })
                .max_by_key(|c| c.ordinal_within_scope)
                .map(|c| c.id.clone());
        }
        if p.thread.deleted_at.is_some()
            || p.thread.rollback_request_id.is_some()
            || !checkpoint_scope::capture_owned(&capture, scope, &p.runs, &p.nodes, &p.checkpoints)
        {
            return Err(StoreError::InvalidEvent(
                "scoped capture ownership is invalid".into(),
            ));
        }
        let id = format!(
            "scoped-capture:{}:{}",
            capture.scope_id, capture.ordinal_within_scope
        );
        let pending: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM orchestration_v2_effect_outbox WHERE effect_id=?1",
                [&id],
                |r| r.get(0),
            )
            .optional()?;
        let effect = Effect {
            id,
            thread_id: thread.clone(),
            body: EffectBody::CaptureScopedCheckpoint {
                capture: capture.into(),
            },
        };
        if let Some(existing) = &pending
            && serde_json::from_str::<Effect>(existing)? != effect
        {
            return Err(StoreError::InvalidEvent(
                "capture ordinal is already reserved".into(),
            ));
        }
        let events = commit_decision(
            &tx,
            Decision {
                effects: if pending.is_some() {
                    vec![]
                } else {
                    vec![effect]
                },
                ..Decision::default()
            },
            None,
            now,
        )?;
        let sequence = latest_sequence(&tx, None)?;
        tx.commit()?;
        self.publish(&events);
        Ok(Commit {
            sequence,
            events,
            replayed: pending.is_some(),
        })
    }
    pub fn ingest_scoped_checkpoint(
        &self,
        events: Vec<DomainEvent>,
        capture: &CheckpointCapture,
        now: &Timestamp,
    ) -> Result<Commit> {
        let mut connection = self.lock()?;
        let tx = connection.transaction()?;
        let thread = events
            .first()
            .ok_or_else(|| StoreError::InvalidEvent("empty capture".into()))?
            .thread_id
            .clone();
        let p = load_projection(&tx, &thread)?.ok_or(StoreError::ThreadNotFound)?;
        let reserved: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM orchestration_v2_effect_outbox WHERE effect_id=?1",
                [format!(
                    "scoped-capture:{}:{}",
                    capture.scope_id, capture.ordinal_within_scope
                )],
                |r| r.get(0),
            )
            .optional()?;
        if !reserved.as_deref().map(serde_json::from_str::<Effect>).transpose()?.is_some_and(|e|e.thread_id==thread && matches!(e.body,EffectBody::CaptureScopedCheckpoint{capture:expected} if *expected==*capture)) {
            return Err(StoreError::InvalidEvent("scoped capture has no matching reservation".into()));
        }
        let owned = p
            .checkpoint_scopes
            .iter()
            .find(|s| s.id == capture.scope_id)
            .is_some_and(|s| {
                checkpoint_scope::capture_owned(capture, s, &p.runs, &p.nodes, &p.checkpoints)
            });
        if owned && events.iter().any(|e| e.thread_id != thread || !matches!(&e.payload,EventPayload::CheckpointCaptured(c) if c.id==crate::checkpoint::scope_ordinal_id(&capture.scope_id,capture.ordinal_within_scope) && c.thread_id==thread && c.scope_id==capture.scope_id && c.run_id==capture.run_id && c.node_id==capture.node_id && c.ordinal_within_scope==capture.ordinal_within_scope && c.app_run_ordinal==capture.app_run_ordinal && c.parent_checkpoint_id==capture.parent_checkpoint_id)) {
            return Err(StoreError::InvalidEvent("scoped capture does not match its reservation".into()));
        }
        let already_captured = p.checkpoints.iter().any(|c| {
            c.scope_id == capture.scope_id
                && c.ordinal_within_scope == capture.ordinal_within_scope
                && c.status != CheckpointStatus::Stale
        });
        let accept = owned
            && !already_captured
            && p.thread.deleted_at.is_none()
            && p.thread.rollback_request_id.is_none();
        let events = commit_decision(
            &tx,
            Decision {
                events: if accept { events } else { vec![] },
                ..Decision::default()
            },
            None,
            now,
        )?;
        let sequence = latest_sequence(&tx, None)?;
        tx.commit()?;
        self.publish(&events);
        Ok(Commit {
            sequence,
            events,
            replayed: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    fn setup() -> (Store, ThreadProjection, CheckpointScope) {
        let store = Store::memory().unwrap();
        let mut p = running();
        let mut payloads = vec![EventPayload::ThreadCreated(p.thread.clone())];
        payloads.extend(p.runs.iter().cloned().map(EventPayload::RunUpdated));
        payloads.extend(
            p.attempts
                .iter()
                .cloned()
                .map(EventPayload::RunAttemptUpdated),
        );
        payloads.extend(p.nodes.iter().cloned().map(EventPayload::NodeUpdated));
        payloads.extend(
            p.provider_threads
                .iter()
                .cloned()
                .map(EventPayload::ProviderThreadUpdated),
        );
        let mut child = p.nodes[0].clone();
        child.id = NodeId::new("child").unwrap();
        child.parent_node_id = Some(p.nodes[0].id.clone());
        child.kind = NodeKind::ToolCall;
        child.counts_for_run = false;
        payloads.push(EventPayload::NodeUpdated(child.clone()));
        p.nodes.push(child.clone());
        store
            .ingest(
                crate::events(&p.thread.id, "seed", payloads, &now()),
                None,
                &now(),
            )
            .unwrap();
        let root = checkpoint_scope(&p.runs[0]);
        store.ensure_checkpoint_scope(root.clone(), &now()).unwrap();
        let scope = CheckpointScope {
            id: CheckpointScopeId::new("nested").unwrap(),
            node_id: child.id,
            parent_scope_id: Some(root.id),
            kind: ScopeKind::Tool,
            advances_app_run_count: false,
            cwd: "/workspace/nested".into(),
            ..root
        };
        store
            .ensure_checkpoint_scope(scope.clone(), &now())
            .unwrap();
        let mut baseline = checkpoint(&p.runs[0], CheckpointStatus::Ready);
        baseline.id = crate::checkpoint::scope_ordinal_id(&scope.id, 0);
        baseline.scope_id = scope.id.clone();
        baseline.node_id = scope.node_id.clone();
        baseline.run_id = None;
        baseline.app_run_ordinal = None;
        baseline.ordinal_within_scope = 0;
        store.ingest_scope_baseline(baseline, &now()).unwrap();
        (store, p, scope)
    }
    fn capture(p: &ThreadProjection, scope: &CheckpointScope) -> CheckpointCapture {
        CheckpointCapture {
            scope_id: scope.id.clone(),
            run_id: scope.run_id.clone(),
            attempt_id: p.runs[0].active_attempt_id.clone(),
            node_id: scope.node_id.clone(),
            ordinal_within_scope: 1,
            app_run_ordinal: None,
            parent_checkpoint_id: None,
        }
    }
    #[test]
    fn durable_nested_capture_is_idempotent_and_does_not_finalize_root() {
        let (store, p, scope) = setup();
        let c = capture(&p, &scope);
        assert!(
            !store
                .schedule_scope_capture(&p.thread.id, c.clone(), &now())
                .unwrap()
                .replayed
        );
        assert!(
            store
                .schedule_scope_capture(&p.thread.id, c, &now())
                .unwrap()
                .replayed
        );
        // Complete the root provider-start effect, then the independent scope effect.
        let claim = store.claim_effect("worker", 0).unwrap().unwrap();
        if !matches!(
            claim.effect.body,
            EffectBody::CaptureScopedCheckpoint { .. }
        ) {
            store.finish_effect(&claim, None, 0).unwrap();
        }
        let effects = read_effects(&store.lock().unwrap()).unwrap();
        let EffectBody::CaptureScopedCheckpoint { capture } = &effects
            .iter()
            .find(|e| matches!(e.body, EffectBody::CaptureScopedCheckpoint { .. }))
            .unwrap()
            .body
        else {
            panic!()
        };
        let mut checkpoint = crate::test_support::checkpoint(&p.runs[0], CheckpointStatus::Ready);
        checkpoint.id = crate::checkpoint::scope_ordinal_id(&scope.id, 1);
        checkpoint.scope_id = scope.id;
        checkpoint.node_id = scope.node_id;
        checkpoint.app_run_ordinal = None;
        checkpoint.parent_checkpoint_id = capture.parent_checkpoint_id.clone();
        let events = crate::events(
            &p.thread.id,
            "capture",
            vec![EventPayload::CheckpointCaptured(checkpoint)],
            &now(),
        );
        assert_eq!(
            store
                .ingest_scoped_checkpoint(events.clone(), capture, &now())
                .unwrap()
                .events
                .len(),
            1
        );
        assert!(
            store
                .ingest_scoped_checkpoint(events, capture, &now())
                .unwrap()
                .events
                .is_empty()
        );
        let current = store.projection(&p.thread.id).unwrap();
        assert_eq!(current.runs, p.runs);
        assert_eq!(current.attempts, p.attempts);
        assert_eq!(
            current
                .checkpoints
                .iter()
                .filter(|c| c.status == CheckpointStatus::Ready)
                .count(),
            2
        );
        let mut forged = capture.as_ref().clone();
        forged.app_run_ordinal = Some(1);
        assert!(
            store
                .schedule_scope_capture(&p.thread.id, forged, &now())
                .is_err()
        );
        store.recover(&now()).unwrap();
        assert!(
            read_effects(&store.lock().unwrap())
                .unwrap()
                .iter()
                .any(|e| matches!(e.body, EffectBody::CaptureScopedCheckpoint { .. }))
        );
    }
    #[test]
    fn scoped_capture_rejects_forged_result_and_drops_a_previous_attempt_result() {
        let (store, p, scope) = setup();
        store
            .schedule_scope_capture(&p.thread.id, capture(&p, &scope), &now())
            .unwrap();
        let effects = read_effects(&store.lock().unwrap()).unwrap();
        let EffectBody::CaptureScopedCheckpoint { capture } = &effects
            .iter()
            .find(|e| matches!(e.body, EffectBody::CaptureScopedCheckpoint { .. }))
            .unwrap()
            .body
        else {
            panic!()
        };
        let mut cp = checkpoint(&p.runs[0], CheckpointStatus::Ready);
        cp.id = crate::checkpoint::scope_ordinal_id(&scope.id, 1);
        cp.scope_id = scope.id;
        cp.node_id = scope.node_id;
        cp.app_run_ordinal = None;
        cp.parent_checkpoint_id = capture.parent_checkpoint_id.clone();
        let write = |key: &str, cp: Checkpoint| {
            crate::events(
                &p.thread.id,
                key,
                vec![EventPayload::CheckpointCaptured(cp)],
                &now(),
            )
        };
        let mut forged = cp.clone();
        forged.app_run_ordinal = Some(p.runs[0].ordinal);
        assert!(
            store
                .ingest_scoped_checkpoint(write("forged", forged), capture, &now())
                .is_err()
        );
        let mut run = p.runs[0].clone();
        run.active_attempt_id = Some(RunAttemptId::new("new-attempt").unwrap());
        store
            .ingest(
                crate::events(
                    &p.thread.id,
                    "attempt",
                    vec![EventPayload::RunUpdated(run)],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        assert!(
            store
                .ingest_scoped_checkpoint(write("late", cp), capture, &now())
                .unwrap()
                .events
                .is_empty()
        );
        assert_eq!(store.projection(&p.thread.id).unwrap().checkpoints.len(), 1);
    }
    #[test]
    fn restart_rebinds_root_scope_without_replacing_baseline() {
        let (store, p, _) = setup();
        let mut scope = checkpoint_scope(&p.runs[0]);
        let mut root = p.nodes[0].clone();
        root.id = NodeId::new("restarted-root").unwrap();
        root.root_node_id = root.id.clone();
        let mut run = p.runs[0].clone();
        run.root_node_id = Some(root.id.clone());
        store
            .ingest(
                crate::events(
                    &p.thread.id,
                    "restart",
                    vec![
                        EventPayload::RunUpdated(run.clone()),
                        EventPayload::NodeUpdated(root.clone()),
                    ],
                    &now(),
                ),
                None,
                &now(),
            )
            .unwrap();
        scope.node_id = root.id.clone();
        scope.created_at = Timestamp::from_millis(10).unwrap();
        store
            .ensure_checkpoint_scope(scope.clone(), &now())
            .unwrap();
        let current = store.projection(&p.thread.id).unwrap();
        let actual = current
            .checkpoint_scopes
            .iter()
            .find(|s| s.id == scope.id)
            .unwrap();
        assert_eq!(actual.node_id, root.id);
        assert_eq!(actual.created_at, now());
        assert_eq!(
            current
                .nodes
                .iter()
                .find(|n| n.id == root.id)
                .unwrap()
                .checkpoint_scope_id
                .as_ref(),
            Some(&scope.id)
        );
        scope.node_id = p.nodes[0].id.clone();
        assert!(store.ensure_checkpoint_scope(scope, &now()).is_err());
    }
}

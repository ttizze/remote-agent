//! Pending provider requests: native identity, ownership and answer claims.
use super::*;

pub(super) fn native_key(instance: uuid::Uuid, id: &Value) -> (uuid::Uuid, String) {
    (
        instance,
        serde_json::to_string(id).expect("native ID serializes"),
    )
}
fn request_target_is_live(
    target: &agent_protocol::requests::RequestTarget,
    turns: Option<&[Arc<agent_protocol::models::Turn>]>,
) -> bool {
    match target {
        agent_protocol::requests::RequestTarget::Session => true,
        agent_protocol::requests::RequestTarget::Turn { turn_id, .. } => {
            turns.into_iter().flatten().any(|turn| {
                &turn.id == turn_id && turn.status == agent_protocol::execution::TurnStatus::Running
            })
        }
    }
}

impl SessionRouter {
    pub(crate) fn request(
        &self,
        target: SessionRef,
        origin: RequestOrigin,
        request: agent_protocol::requests::Request,
    ) -> Result<(), String> {
        if target.provider != origin.provider {
            return Err("request provider does not match session".into());
        }
        if !origin.source.is_alive() {
            return Err("request source is closed".into());
        }
        if serde_json::to_vec(&request)
            .map_err(|e| e.to_string())?
            .len()
            > 64 * 1024
        {
            return Err("pending request capacity reached".into());
        }
        let native = native_key(origin.instance, &origin.native_id);
        let lease = self.retain_execution(target.clone())?;
        let actor = &lease.actor;
        let mut owned = lock_state(actor);
        let mut state = lock_state(&self.state);
        if state.native_requests.contains_key(&native) {
            return Ok(());
        }
        if state.requests.len() >= 256 || owned.timeline.requests.len() >= 32 {
            return Err("pending request capacity reached".into());
        }
        if !request_target_is_live(&request.target, owned.timeline.turns.as_deref()) {
            return Err("request has no owned live turn".into());
        }
        state.requests.insert(request.id.clone(), target.clone());
        state.native_requests.insert(native, request.id.clone());
        drop(state);
        owned.request_origins.insert(request.id.clone(), origin);
        let mut failed = Vec::new();
        self.change_locked(
            &mut owned,
            &target,
            &SessionChange::Request { request },
            &mut failed,
        );
        drop(owned);
        self.close_failed(failed);
        Ok(())
    }
    pub(crate) fn resolve_native_request(&self, instance: uuid::Uuid, native: &Value) {
        let resolved = {
            let state = lock_state(&self.state);
            state
                .native_requests
                .get(&native_key(instance, native))
                .and_then(|id| {
                    state
                        .requests
                        .get(id)
                        .map(|target| (target.clone(), id.clone()))
                })
        };
        if let Some((target, request_id)) = resolved {
            self.session_change(&target, SessionChange::ResolveRequest { request_id });
        }
    }
    pub(crate) fn close_request_source(&self, instance: uuid::Uuid) {
        let requests: Vec<_> = {
            let state = lock_state(&self.state);
            state
                .native_requests
                .iter()
                .filter(|((source, _), _)| *source == instance)
                .filter_map(|(_, id)| {
                    state
                        .requests
                        .get(id)
                        .map(|target| (target.clone(), id.clone()))
                })
                .collect()
        };
        for (target, request_id) in requests {
            self.session_change(&target, SessionChange::ResolveRequest { request_id });
        }
    }
    pub(in crate::host_rpc) fn request_session(&self, id: &str) -> Option<SessionRef> {
        lock_state(&self.state).requests.get(id).cloned()
    }
    pub(crate) fn claim_response(
        &self,
        session: SessionId,
        id: &str,
        answer: &agent_protocol::requests::Answer,
    ) -> Result<(RequestOrigin, agent_protocol::requests::RequestBody), String> {
        let target = self
            .request_session(id)
            .ok_or("request was already answered or its execution has ended")?;
        let actor = self
            .existing(&target)
            .ok_or("request execution has ended")?;
        let mut owned = lock_state(&actor);
        let request = owned
            .timeline
            .requests
            .get(id)
            .ok_or("request was already answered or its execution has ended")?;
        let origin = owned
            .request_origins
            .get(id)
            .ok_or("request source has ended")?;
        if request.delivery != agent_protocol::session::RequestDelivery::Awaiting
            || !origin.source.is_alive()
        {
            return Err("request was already answered or its source has ended".into());
        }
        if !request_target_is_live(&request.target, owned.timeline.turns.as_deref()) {
            return Err("request execution has ended".into());
        }
        agent_protocol::requests::validate_answer(&request.body, answer)?;
        let body = request.body.clone();
        let origin = origin.clone();
        let state = lock_state(&self.state);
        if !state
            .sessions
            .get(&session)
            .is_some_and(|output| output.alive.load(Relaxed))
        {
            return Err("connection is closed".into());
        }
        drop(state);
        let mut failed = Vec::new();
        self.change_locked(
            &mut owned,
            &target,
            &SessionChange::RequestDelivery {
                request_id: id.into(),
                state: agent_protocol::session::RequestDelivery::Sending,
            },
            &mut failed,
        );
        drop(owned);
        self.close_failed(failed);
        Ok((origin, body))
    }
    pub(crate) fn response_delivery(
        &self,
        id: &str,
        delivery: agent_protocol::session::RequestDelivery,
    ) {
        let Some(target) = self.request_session(id) else {
            return;
        };
        let Some(actor) = self.existing(&target) else {
            return;
        };
        let mut owned = lock_state(&actor);
        let mut failed = Vec::new();
        if owned.timeline.requests.get(id).is_some_and(|request| {
            request.delivery == agent_protocol::session::RequestDelivery::Sending
        }) {
            self.change_locked(
                &mut owned,
                &target,
                &SessionChange::RequestDelivery {
                    request_id: id.into(),
                    state: delivery,
                },
                &mut failed,
            );
        }
        drop(owned);
        self.close_failed(failed);
        prune(&self.state, &target, &actor);
    }
}

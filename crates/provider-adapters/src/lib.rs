//! Codex app-server and Claude stream-json adapters for orchestration-v2.
pub mod capabilities;
pub mod claude;
pub mod codex;
pub mod normalize;
use orchestration::*;
use std::time::{SystemTime, UNIX_EPOCH};

/// The Host ingests these under the run/attempt ownership guard. Adapters do
/// not own the orchestration store or mutate client projections.
#[derive(Debug)]
pub struct ProviderBatch {
    pub thread_id: ThreadId,
    pub run_id: RunId,
    pub attempt_id: RunAttemptId,
    pub events: Vec<DomainEvent>,
    pub occurred_at: Timestamp,
    pub acknowledged: Option<tokio::sync::oneshot::Sender<bool>>,
}
pub(crate) fn now() -> Timestamp {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64;
    Timestamp::from_millis(millis).expect("system time in RFC 3339 range")
}
pub(crate) fn error(message: impl std::fmt::Display) -> orchestration::worker::AdapterError {
    orchestration::worker::AdapterError {
        message: message.to_string(),
        retryable: false,
        turn_completed: false,
    }
}

pub(crate) fn turn_completed() -> orchestration::worker::AdapterError {
    orchestration::worker::AdapterError {
        message: "turn completed".into(),
        retryable: false,
        turn_completed: true,
    }
}

/// Resume/fork failure starts a fresh native session, with durable portable context.
async fn portable_fallback(
    output: &tokio::sync::mpsc::Sender<ProviderBatch>,
    projection: &ThreadProjection,
    run: &Run,
) -> Result<ThreadProjection, orchestration::worker::AdapterError> {
    let timestamp = now();
    let mut provider = projection
        .provider_threads
        .iter()
        .find(|p| Some(&p.id) == run.provider_thread_id.as_ref())
        .ok_or_else(|| error("provider missing"))?
        .clone();
    provider.native_thread_ref = None;
    provider.native_conversation_head_ref = None;
    provider.forked_from = None;
    provider.first_run_ordinal = None;
    provider.updated_at = timestamp.clone();
    let mut payloads = vec![EventPayload::ProviderThreadUpdated(provider)];
    let transfers: Vec<_> = projection
        .context_transfers
        .iter()
        .filter(|t| {
            t.target_run_id.as_ref() == Some(&run.id) && t.status == TransferStatus::ResolvedNative
        })
        .collect();
    if transfers.is_empty() {
        if !projection.context_handoffs.iter().any(|h| {
            h.target_run_id == run.id
                && h.strategy == HandoffStrategy::FullThreadSummary
                && h.transfer_id.is_none()
        }) {
            payloads.extend(context::portable(
                projection,
                projection,
                run,
                None,
                HandoffStrategy::FullThreadSummary,
                1,
                run.ordinal.saturating_sub(1),
                &timestamp,
            ));
        }
    } else {
        for transfer in transfers {
            payloads.extend(context::portable(
                projection,
                projection,
                run,
                Some(transfer),
                HandoffStrategy::FullThreadSummary,
                1,
                run.ordinal.saturating_sub(1),
                &timestamp,
            ));
        }
    }
    let events = events(
        &projection.thread.id,
        &format!("resume-fallback:{}", run.id),
        payloads,
        &timestamp,
    );
    let mut result = Some(projection.clone());
    for event in &events {
        result = projector::apply(
            result.as_ref(),
            event,
            projector::ProjectionOptions::default(),
        );
    }
    let (ack, received) = tokio::sync::oneshot::channel();
    output
        .send(ProviderBatch {
            thread_id: projection.thread.id.clone(),
            run_id: run.id.clone(),
            attempt_id: run
                .active_attempt_id
                .clone()
                .ok_or_else(|| error("attempt missing"))?,
            events,
            occurred_at: timestamp,
            acknowledged: Some(ack),
        })
        .await
        .map_err(error)?;
    if !received.await.map_err(error)? {
        return Err(error("resume fallback superseded"));
    }
    result.ok_or_else(|| error("fallback lost thread"))
}

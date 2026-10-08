//! Native request/answer conversion. Native identities and effects stay private.
use agent_protocol::requests::{Answer, RequestBody};
use serde_json::Value;

/// The Host keeps normalized requests and source identity. The adapter owns
/// answer mappings and the resource to which an answer can be written.
#[async_trait::async_trait]
pub(crate) trait AnswerSource: Send + Sync {
    fn is_alive(&self) -> bool;
    async fn prepare(
        &self,
        native_id: &Value,
        body: &RequestBody,
        answer: &Answer,
    ) -> Result<super::agent::AnswerWrite, super::service::Failure>;
}
#[derive(Clone)]
pub(crate) struct RequestOrigin {
    pub instance: uuid::Uuid,
    pub native_id: Value,
    pub provider: agent_protocol::session::ProviderKind,
    pub source: std::sync::Arc<dyn AnswerSource>,
}
#[cfg(test)]
pub(crate) fn unavailable_origin(
    instance: uuid::Uuid,
    native_id: Value,
    stopped: tokio_util::sync::CancellationToken,
) -> RequestOrigin {
    struct UnavailableSource(tokio_util::sync::CancellationToken);
    #[async_trait::async_trait]
    impl AnswerSource for UnavailableSource {
        fn is_alive(&self) -> bool {
            !self.0.is_cancelled()
        }
        async fn prepare(
            &self,
            _: &Value,
            _: &RequestBody,
            _: &Answer,
        ) -> Result<super::agent::AnswerWrite, super::service::Failure> {
            Err(super::service::Failure::new(
                "answer_not_sent",
                "request source has changed",
            ))
        }
    }
    RequestOrigin {
        instance,
        native_id,
        provider: agent_protocol::session::ProviderKind::Codex,
        source: std::sync::Arc::new(UnavailableSource(stopped)),
    }
}

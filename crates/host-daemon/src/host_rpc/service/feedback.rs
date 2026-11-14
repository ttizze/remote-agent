use super::*;
use agent_protocol::feedback::{FeedbackReceipt, UploadFeedback};

impl HostRpcService {
    pub(super) async fn upload_feedback(
        &self,
        params: &UploadFeedback,
    ) -> Result<FeedbackReceipt, Failure> {
        params
            .validate()
            .map_err(|message| Failure::new("invalid_params", message))?;
        if params.storage_scope
            != self
                .storage_scope()
                .map_err(|error| Failure::new("storage_unavailable", error))?
        {
            return Err(Failure::new(
                "storage_changed",
                "接続先の会話データが変わりました。会話を開き直してお試しください。",
            ));
        }
        self.require_not_updating()?;
        let target = self
            .inner
            .conversation
            .storage
            .active_source(&params.thread_id)
            .map_err(super::super::runtime::storage_failure)?
            .unwrap_or_else(|| params.thread_id.clone());
        let provider = target.native_provider().map_err(|_| {
            Failure::new(
                "feedback_unavailable",
                "最初の発言が実行されてからフィードバックを送信してください。",
            )
        })?;
        self.require_provider_enabled(provider)?;
        let agent = self.inner.providers.get(provider)?;
        if !agent.capabilities().feedback {
            return Err(Failure::new(
                "unsupported",
                "このエージェントはフィードバック送信に対応していません。",
            ));
        }
        agent
            .upload_feedback(&target.id, params.reason.as_deref(), params.include_logs)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_rpc::runtime::native_fixture::{NativeAgent, own};
    use agent_protocol::{
        models::{Thread, ThreadResponse},
        session::{ProviderKind, SessionRef},
    };

    #[tokio::test]
    async fn feedback_resolves_the_public_conversation_and_checks_scope_before_native_effects() {
        let directory = tempfile::tempdir().unwrap();
        let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
        let service = HostRpcService::new(
            [codex.backend(), claude.backend()],
            directory.path().join("projects.json"),
        );
        let public = SessionRef::new(None, uuid::Uuid::new_v4().to_string()).unwrap();
        own(
            &service,
            &public,
            &ThreadResponse {
                thread: Thread {
                    id: Some(
                        SessionRef::new(ProviderKind::Codex, "native-feedback".into()).unwrap(),
                    ),
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let mut request = UploadFeedback {
            id: uuid::Uuid::new_v4().to_string(),
            storage_scope: service.storage_scope().unwrap(),
            thread_id: public,
            reason: Some("display issue".into()),
            include_logs: false,
        };
        for include_logs in [false, true] {
            request.include_logs = include_logs;
            let receipt = service.upload_feedback(&request).await.unwrap();
            assert_eq!(receipt.feedback_id, "feedback:native-feedback");
        }
        assert_eq!(
            *codex.feedback_reports.lock().unwrap(),
            vec![
                (
                    "native-feedback".into(),
                    Some("display issue".into()),
                    false
                ),
                ("native-feedback".into(), Some("display issue".into()), true)
            ]
        );
        request.storage_scope = "different-storage".into();
        assert_eq!(
            service.upload_feedback(&request).await.unwrap_err().code,
            "storage_changed"
        );
        request.storage_scope = service.storage_scope().unwrap();
        request.reason = Some("😀".repeat(4001));
        assert_eq!(
            service.upload_feedback(&request).await.unwrap_err().code,
            "invalid_params"
        );
        request.reason = None;
        request.thread_id = SessionRef::new(ProviderKind::Claude, "unsupported".into()).unwrap();
        assert_eq!(
            service.upload_feedback(&request).await.unwrap_err().code,
            "unsupported"
        );
        assert_eq!(codex.feedback_reports.lock().unwrap().len(), 2);
        assert!(claude.feedback_reports.lock().unwrap().is_empty());
        service.shutdown_owned_processes().await;
    }
}

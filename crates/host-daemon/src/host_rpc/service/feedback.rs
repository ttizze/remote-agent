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
        if self
            .inner
            .updating
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(Failure::new(
                "host_updating",
                "Bexを更新しています。再接続後にお試しください。",
            ));
        }
        let target = if let Some(row) = self.inner.conversations.get(&params.thread_id)? {
            if row.startup.is_some() {
                return Err(Failure::new(
                    "feedback_unavailable",
                    "最初の発言が実行されてからフィードバックを送信してください。",
                ));
            }
            row.active
        } else {
            params.thread_id.clone()
        };
        self.require_provider_enabled(target.provider)?;
        let agent = self.inner.providers.get(target.provider)?;
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
    use super::super::conversations::tests::{NativeAgent, call};
    use super::*;
    use crate::host_rpc::conversations::{Binding, Conversation};
    use agent_protocol::{models::Thread, session::SessionRef};
    #[tokio::test]
    async fn feedback_uses_the_active_native_binding_and_requires_explicit_logs() {
        let directory = tempfile::tempdir().unwrap();
        let codex = NativeAgent::new(ProviderKind::Codex, directory.path().join("codex"));
        let claude = NativeAgent::new(ProviderKind::Claude, directory.path().join("claude"));
        let host = HostRpcService::new(
            [codex.backend(), claude.backend()],
            ProjectStore::new(directory.path().join("worktrees.json")),
        );
        let client = host.open_session();
        let root = SessionRef::new(ProviderKind::Codex, "public-conversation".into()).unwrap();
        let native = SessionRef::new(ProviderKind::Codex, "native-codex".into()).unwrap();
        let mut row = Conversation::new(&Thread {
            id: Some(native.clone()),
            cwd: Some(directory.path().to_str().unwrap().into()),
            ..Default::default()
        })
        .unwrap();
        row.id = root.clone();
        host.inner.conversations.save(row.clone()).unwrap();
        let request = |include_logs, reason| UploadFeedback {
            id: uuid::Uuid::new_v4().to_string(),
            storage_scope: host.storage_scope().unwrap(),
            thread_id: root.clone(),
            reason,
            include_logs,
        };
        for include_logs in [false, true] {
            let receipt: FeedbackReceipt = call(
                &host,
                client.id(),
                Call::UploadFeedback(request(include_logs, Some("A bug".into()))),
            )
            .await
            .unwrap();
            assert_eq!(receipt.feedback_id, "feedback:native-codex");
            assert_eq!(
                codex
                    .feedback_reports
                    .lock()
                    .unwrap()
                    .last()
                    .cloned()
                    .unwrap(),
                (native.id.clone(), Some("A bug".into()), include_logs)
            );
        }
        let invalid = request(false, Some("x".repeat(8001)));
        assert_eq!(
            call::<FeedbackReceipt>(&host, client.id(), Call::UploadFeedback(invalid))
                .await
                .unwrap_err()
                .code,
            "invalid_params"
        );
        assert_eq!(codex.feedback_reports.lock().unwrap().len(), 2);
        let mut foreign = request(false, None);
        foreign.storage_scope = "another-host".into();
        assert_eq!(
            call::<FeedbackReceipt>(&host, client.id(), Call::UploadFeedback(foreign))
                .await
                .unwrap_err()
                .code,
            "storage_changed"
        );
        assert_eq!(codex.feedback_reports.lock().unwrap().len(), 2);
        row.active = SessionRef::new(ProviderKind::Claude, "native-claude".into()).unwrap();
        row.bindings.push(Binding {
            context_start: 0,
            session: row.active.clone(),
            covered: Some(0),
        });
        host.inner.conversations.save(row).unwrap();
        assert_eq!(
            call::<FeedbackReceipt>(
                &host,
                client.id(),
                Call::UploadFeedback(request(false, None))
            )
            .await
            .unwrap_err()
            .code,
            "unsupported"
        );
        assert!(claude.feedback_reports.lock().unwrap().is_empty());
        assert_eq!(codex.feedback_reports.lock().unwrap().len(), 2);
    }
}

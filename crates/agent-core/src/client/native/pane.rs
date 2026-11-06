//! ABI conversion only; the core Pane owns selection, observation and lifetime.
use super::*;
use crate::presentation::view::Presentation;

#[derive(uniffi::Object)]
pub struct AgentPane {
    pub(super) pane: Arc<crate::store::Pane>,
}

#[uniffi::export(async_runtime = "tokio")]
impl AgentPane {
    pub fn id(&self) -> String {
        self.pane.id().into()
    }
    pub fn presentation(&self) -> Result<Arc<Presentation>, AgentError> {
        self.pane.presentation().map_err(error)
    }
    pub fn dispatch(&self, intent: Intent) -> Result<Arc<Receipt>, AgentError> {
        let receipt = self.pane.dispatch(intent);
        Ok(Arc::new(Receipt {
            result: Mutex::new(Some(Box::pin(async move { receipt.await.map_err(error) }))),
        }))
    }
    pub async fn next_presentation(
        &self,
        previous: Arc<Presentation>,
    ) -> Result<Arc<Presentation>, AgentError> {
        let mut updates = self.pane.subscribe();
        loop {
            updates.borrow_and_update();
            let current = self.presentation()?;
            if current.changes(previous.clone()).changed {
                return Ok(current);
            }
            updates.changed().await.map_err(error)?;
        }
    }
    pub fn prepare_dictation(&self) -> Arc<crate::client::DictationPreparation> {
        Arc::new(self.pane.prepare_dictation())
    }
    pub async fn prepare_side_chat(
        &self,
        provider: Option<crate::session::ProviderKind>,
        text: String,
    ) -> Result<(), AgentError> {
        self.pane.prepare_side_chat(provider, text).await
    }
    pub fn retire(&self) {
        self.pane.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn next_presentation_waits_after_an_immediate_draft_acknowledgment() {
        let store = AgentStore::offline(Vec::new(), None, "fixture".into())
            .await
            .unwrap();
        let pane = store.open_pane("/fixture".into()).await.unwrap();
        let key = pane
            .presentation()
            .unwrap()
            .snapshot
            .pane
            .navigation
            .draft_key
            .clone();
        pane.dispatch(Intent::SetDraftText {
            thread_id: key.clone(),
            text: "acknowledged".into(),
        })
        .unwrap()
        .wait()
        .await
        .unwrap();
        let acknowledged = pane.presentation().unwrap();
        let waiting = pane.next_presentation(acknowledged);
        tokio::pin!(waiting);
        assert!(futures_util::poll!(waiting.as_mut()).is_pending());
        pane.dispatch(Intent::SetDraftText {
            thread_id: key,
            text: "next".into(),
        })
        .unwrap()
        .wait()
        .await
        .unwrap();
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.composer.draft.text, "next");
        pane.retire();
        assert!(pane.presentation().is_err());
        store.shutdown().await.unwrap();
    }
}

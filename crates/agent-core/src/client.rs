//! Peripheral client workflows retained independently of conversation state.
use agent_transport::client::Client;
#[cfg_attr(feature = "bindings", derive(uniffi::Object))]
pub struct DictationPreparation {
    pub(crate) id: String,
    pub(crate) _cancel: tokio_util::sync::DropGuard,
}
#[cfg_attr(feature = "bindings", uniffi::export)]
impl DictationPreparation {
    pub fn id(&self) -> String {
        self.id.clone()
    }
}
pub(crate) async fn prepare_dictation(
    peer: std::sync::Arc<Client>,
    id: String,
    cancel: tokio_util::sync::CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }
    let params = agent_protocol::operations::DictationPreparation { id };
    if peer
        .request::<crate::models::Empty>(&crate::protocol::Call::PrepareDictation(params.clone()))
        .await
        .is_ok()
    {
        cancel.cancelled().await;
    }
    let _ = peer
        .request::<crate::models::Empty>(&crate::protocol::Call::CancelDictation(params))
        .await;
}
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn terminal_handle(cwd: String) -> String {
    format!(
        "bex-terminal-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, cwd.as_bytes())
    )
}

pub async fn models(
    peer: &Client,
) -> Result<agent_protocol::operations::ModelPage, crate::peer::PeerError> {
    let mut data = vec![];
    let mut cursor = None;
    let mut seen = std::collections::HashSet::new();
    let mut errors = serde_json::Map::new();
    loop {
        let page = peer
            .call(&agent_protocol::operations::ListModels { limit: 100, cursor })
            .await?;
        data.extend(page.data);
        if let Some(provider_errors) = page.provider_errors {
            errors.extend(provider_errors)
        }
        cursor = page.next_cursor;
        if let Some(next) = &cursor {
            if !seen.insert(next.clone()) {
                return Err(crate::peer::PeerError::InvalidMessage(
                    "Model cursor repeated".into(),
                ));
            }
        } else {
            return Ok(agent_protocol::operations::ModelPage {
                data,
                next_cursor: None,
                provider_errors: (!errors.is_empty()).then_some(errors),
            });
        }
    }
}

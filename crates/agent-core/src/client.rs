//! Client workflows built on the shared transport and protocol.
use crate::peer::PeerError;
use agent_protocol::operations::*;
use agent_protocol::requests::{Answer, Request, validate_answer};
use agent_transport::client::{Client, Updates};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, thiserror::Error)]
#[cfg_attr(feature = "bindings", derive(uniffi::Error))]
pub enum AgentError {
    #[error("{reason}")]
    Failed {
        /// Safe error source, formatted by the UI in its current language.
        reason: String,
    },
}

pub(crate) fn error(reason: impl std::fmt::Display) -> AgentError {
    AgentError::Failed {
        reason: crate::presentation::error::error_source(&reason.to_string()),
    }
}

pub(crate) fn command_result(
    reply: agent_protocol::conversation::records::Acknowledged,
) -> Result<u64, PeerError> {
    reply.accepted().map_err(|error| PeerError::Remote {
        delivery: error.delivery,
        error: serde_json::to_string(&error).expect("command failure serializes"),
        sequence: None,
    })
}

pub(crate) async fn resolve_hydration(
    response: agent_protocol::conversation::records::Hydrated,
    expected: &agent_protocol::ids::ItemId,
    session: Option<&crate::transport::Session>,
) -> Result<Option<agent_protocol::orchestration::TurnItem>, PeerError> {
    let item = if let Some(grant) = response.transfer {
        Some(
            tokio::time::timeout(
                std::time::Duration::from_secs(120),
                crate::transfers::resolve_item(grant, expected, session),
            )
            .await
            .unwrap_or_else(|_| {
                Err(PeerError::RequestTimeout {
                    method: "item body transfer".into(),
                })
            })?,
        )
    } else {
        response.item
    };
    if item
        .as_ref()
        .is_some_and(|item| item.id != expected.as_str())
    {
        return Err(PeerError::InvalidMessage(
            "item detail identity changed".into(),
        ));
    }
    Ok(item)
}

async fn gallery_frame(
    snapshot: &mut agent_protocol::orchestration::projection::ThreadSnapshot,
    stream: &mut agent_transport::framing::Reader,
) -> Result<(), PeerError> {
    let message = stream
        .read::<agent_protocol::orchestration::projection::ThreadStreamItem>()
        .await
        .map_err(|error| PeerError::ConnectionClosed(error.to_string()))?
        .ok_or_else(|| PeerError::ConnectionClosed("gallery subscription ended".into()))?;
    *snapshot =
        crate::orchestration::receive(&snapshot.projection.thread.id, Some(snapshot), message)
            .map_err(|error| PeerError::InvalidMessage(error.into()))?
            .ok_or_else(|| PeerError::InvalidMessage("gallery snapshot is missing".into()))?;
    Ok(())
}

// Read the one state stream during RPCs and transfers so its byte budget stays
// available. History RPCs return pages; they never acknowledge live events.
async fn gallery_request<T>(
    snapshot: &mut agent_protocol::orchestration::projection::ThreadSnapshot,
    stream: &mut agent_transport::framing::Reader,
    request: impl std::future::Future<Output = Result<T, PeerError>>,
) -> Result<T, PeerError> {
    tokio::pin!(request);
    loop {
        tokio::select! {
            result=&mut request=>return result,
            result=gallery_frame(snapshot,stream)=>{result?;},
        }
    }
}

/// A recording owns its prepared connection until transcription finishes or
/// capture is cancelled. The Store worker orders preparation before cancellation.
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
    peer: &Client,
    id: String,
    cancel: tokio_util::sync::CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }
    let preparation = agent_protocol::operations::DictationPreparation { id };
    if peer
        .request::<crate::models::Empty>(&crate::protocol::Call::PrepareDictation(
            preparation.clone(),
        ))
        .await
        .is_ok()
    {
        cancel.cancelled().await;
    }
    // A lost preparation reply may still leave a connection on Host. Cancel it
    // after that request completes, including failures, rather than racing start.
    let _ = peer
        .request::<crate::models::Empty>(&crate::protocol::Call::CancelDictation(preparation))
        .await;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SessionImage {
    pub source: String,
    pub encoded: bool,
}

/// One resumable terminal per device and working directory. Host scopes this ID to the
/// authenticated device, so reopening a view or application can attach safely.
#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn terminal_handle(cwd: String) -> String {
    format!(
        "agent-terminal-{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, cwd.as_bytes())
    )
}

#[derive(Clone, Copy)]
pub enum SubscriptionKind {
    List,
    Conversation,
    Output,
}

#[allow(async_fn_in_trait)]
pub trait ClientExt {
    async fn open_subscription<O: RpcMethod>(
        &self,
        operation: &O,
    ) -> Result<Option<(O::Output, Updates, uuid::Uuid, SubscriptionKind)>, PeerError>;
    async fn models(&self, provider: crate::session::ProviderKind) -> Result<ModelPage, PeerError>;
    async fn respond(
        &self,
        session: &crate::session::SessionRef,
        request: &Request,
        answer: &Answer,
    ) -> Result<(), PeerError>;
    async fn session_images(
        &self,
        thread_id: &crate::session::SessionRef,
        _session: Option<&crate::transport::Session>,
    ) -> Result<Vec<SessionImage>, PeerError>;
}
impl ClientExt for Client {
    async fn open_subscription<O: RpcMethod>(
        &self,
        operation: &O,
    ) -> Result<Option<(O::Output, Updates, uuid::Uuid, SubscriptionKind)>, PeerError> {
        let id = uuid::Uuid::new_v4();
        let mut request = operation.request()?;
        let (subscription, kind) = match &mut request {
            crate::protocol::Call::SubscribeList(params) => {
                (&mut params.subscription, SubscriptionKind::List)
            }
            crate::protocol::Call::SubscribeConversation(params) => {
                (&mut params.subscription, SubscriptionKind::Conversation)
            }
            crate::protocol::Call::SubscribeCommandOutput(params) => {
                (&mut params.subscription, SubscriptionKind::Output)
            }
            crate::protocol::Call::CreateConversation(params) => {
                (&mut params.subscription, SubscriptionKind::Conversation)
            }
            _ => return Ok(None),
        };
        *subscription = id;
        let (mut output, updates) = self.request_stream::<O::Output>(&request).await?;
        validate_output(operation, &output)?;
        O::subscription(&mut output, id);
        Ok(Some((output, updates, id, kind)))
    }

    async fn models(&self, provider: crate::session::ProviderKind) -> Result<ModelPage, PeerError> {
        let mut provider_errors = Map::<String, Value>::new();
        let mut models: Vec<crate::models::Model> = Vec::new();
        let mut positions = std::collections::HashMap::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self
                .call(&ListModels {
                    provider,
                    limit: 100,
                    cursor: cursor.clone(),
                })
                .await?;
            if page
                .data
                .iter()
                .any(|model| model.model.provider != provider)
                || page
                    .provider_errors
                    .as_ref()
                    .is_some_and(|errors| errors.keys().any(|key| key != provider.key()))
            {
                return Err(PeerError::InvalidMessage(
                    "model catalog belongs to another provider".into(),
                ));
            }
            if let Some(errors) = page.provider_errors {
                provider_errors.extend(errors);
            }
            for model in page.data {
                let index = *positions.entry(model.id.clone()).or_insert(models.len());
                if index == models.len() {
                    models.push(model);
                } else {
                    models[index] = model;
                }
            }
            cursor = page.next_cursor;
            let Some(next) = &cursor else {
                return Ok(ModelPage {
                    data: models,
                    next_cursor: None,
                    provider_errors: (!provider_errors.is_empty()).then_some(provider_errors),
                });
            };
            if !seen.insert(next.clone()) {
                return Err(PeerError::InvalidMessage(
                    "model cursor did not advance".into(),
                ));
            }
        }
    }

    async fn respond(
        &self,
        session: &crate::session::SessionRef,
        request: &Request,
        answer: &Answer,
    ) -> Result<(), PeerError> {
        validate_answer(&request.body, answer).map_err(PeerError::InvalidMessage)?;
        let reply: agent_protocol::conversation::records::Acknowledged = self
            .call(&agent_protocol::conversation::records::Change {
                session: session.clone(),
                operation: uuid::Uuid::new_v4(),
                command: agent_protocol::conversation::Command::Answer {
                    id: request.id.clone(),
                    answer: answer.clone(),
                },
            })
            .await?;
        command_result(reply).map(|_| ())
    }

    async fn session_images(
        &self,
        thread_id: &crate::session::SessionRef,
        _session: Option<&crate::transport::Session>,
    ) -> Result<Vec<SessionImage>, PeerError> {
        use agent_protocol::{
            conversation::records,
            orchestration::{self as model},
        };
        let (opened, mut stream) = self
            .request_stream::<records::Opened>(&crate::protocol::Call::SubscribeConversation(
                records::Subscribe {
                    session: thread_id.clone(),
                    subscription: uuid::Uuid::new_v4(),
                    after_sequence: None,
                },
            ))
            .await?;
        if &opened.session != thread_id {
            return Err(PeerError::InvalidMessage(
                "gallery conversation identity changed".into(),
            ));
        }
        let mut snapshot = None;
        for message in opened.initial {
            snapshot = crate::orchestration::receive(
                &thread_id.application_id(),
                snapshot.as_ref(),
                message,
            )
            .map_err(|error| PeerError::InvalidMessage(error.into()))?;
        }
        let mut snapshot = snapshot
            .ok_or_else(|| PeerError::InvalidMessage("gallery snapshot is missing".into()))?;
        while let Some(cursor) = snapshot.history_cursor.clone() {
            let page = gallery_request(
                &mut snapshot,
                &mut stream,
                self.call(&records::Extend {
                    session: thread_id.clone(),
                    cursor: cursor.clone(),
                }),
            )
            .await?;
            if page.has_more_history && page.next_cursor.as_ref() == Some(&cursor) {
                return Err(PeerError::InvalidMessage(
                    "history cursor did not advance".into(),
                ));
            }
            if let Some(next) = crate::orchestration::receive_history(&snapshot, &cursor, page) {
                snapshot = next;
            }
        }
        let mut images = Vec::new();
        let mut sources = std::collections::HashSet::new();
        for row in snapshot.projection.visible_turn_items.iter().rev() {
            if let model::TurnItemBody::AssistantMessage { attachments, .. } = &row.item.body {
                for attachment in attachments.iter().rev() {
                    if attachment.get("type").and_then(Value::as_str) != Some("image") {
                        continue;
                    }
                    let id = attachment
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            PeerError::InvalidMessage("image attachment ID is missing".into())
                        })?;
                    let extension = agent_protocol::paths::attachment_extension(
                        attachment
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("image"),
                        attachment.get("mimeType").and_then(Value::as_str),
                    );
                    let source = format!(
                        "{}{id}.{extension}",
                        agent_protocol::paths::ATTACHMENT_SCHEME
                    );
                    if sources.insert(source.clone()) {
                        images.push(SessionImage {
                            source,
                            encoded: false,
                        });
                    }
                }
            }
        }
        if snapshot.has_more_history {
            return Err(PeerError::InvalidMessage("履歴が部分取得のため、画像一覧を完全には取得できません。会話内の画像から個別に開いてください。".into()));
        }

        Ok(images)
    }
}

#[cfg(feature = "bindings")]
pub mod native;
pub mod persistence;
#[cfg(feature = "bindings")]
pub mod session;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        models::{Model, ModelRef},
        session::ProviderKind,
        state::Snapshot,
    };
    use serde_json::json;
    use std::time::Duration;

    use crate::test_host as host_fixture;

    #[tokio::test]
    async fn model_pages_reject_payloads_owned_by_another_provider() {
        for result in [
            json!({"data":[{"id":"wrong","model":{"provider":"claude","id":"wrong"},"displayName":"Wrong"}]}),
            json!({"data":[],"providerErrors":{"claude":{"message":"wrong provider"}}}),
        ] {
            let ((client, _events), mut reader, writer) =
                host_fixture::connect(&Snapshot::default()).await;
            let serving = async {
                let request = reader.read_request().await.unwrap().unwrap();
                assert_eq!(request["params"]["provider"], "codex");
                writer
                    .reply(&request, json!({"result":result}))
                    .await
                    .unwrap();
            };
            let (result, ()) = tokio::join!(client.models(ProviderKind::Codex), serving);
            assert!(matches!(result, Err(PeerError::InvalidMessage(_))));
            client.close().await;
        }
    }

    #[rstest::rstest]
    #[case(1)]
    #[case(16)]
    #[case(127)]
    #[case(128)]
    #[case(129)]
    #[case(160)]
    #[tokio::test]
    async fn model_pages_keep_alias_identity_first_position_and_latest_payload(
        #[case] count: usize,
    ) {
        let original: Vec<_> = (0..count)
            .map(|index| Model {
                id: format!("alias-{index}"),
                model: ModelRef {
                    provider: ProviderKind::Codex,
                    id: "same-native-model".into(),
                },
                display_name: format!("Original {index}"),
                credential_route: None,
                settings: None,
                settings_discovery: false,
                is_default: Some(false),
            })
            .collect();
        let mut tail = original[count - 1].clone();
        tail.display_name = "Updated tail".into();
        let mut alias = original[0].clone();
        alias.id = "another-alias".into();
        let mut latest = original[0].clone();
        latest.display_name = "Latest first entry".into();
        latest.model.id = "replaced-native-model".into();
        latest.settings_discovery = true;
        latest.is_default = Some(true);
        let mut pages: Vec<_> = original.chunks(100).map(<[Model]>::to_vec).collect();
        pages.push(vec![tail.clone(), alias.clone(), latest.clone()]);
        let mut expected = original;
        expected[count - 1] = tail;
        expected[0] = latest;
        expected.push(alias);
        let ((client, _events), mut reader, writer) =
            host_fixture::connect(&Snapshot::default()).await;
        let page_count = pages.len();
        let serving = async {
            for (index, data) in pages.into_iter().enumerate() {
                let request = reader.read_request().await.unwrap().unwrap();
                assert_eq!(request["method"], "host/model/list");
                assert_eq!(request["params"]["limit"], 100);
                assert_eq!(
                    request["params"]["cursor"],
                    if index == 0 {
                        Value::Null
                    } else {
                        json!(format!("page-{index}"))
                    }
                );
                let next = (index + 1 < page_count).then(|| format!("page-{}", index + 1));
                let errors = if next.is_none() {
                    json!({"codex":"latest"})
                } else {
                    json!({"codex":format!("page-{index}")})
                };
                writer
                    .reply(
                        &request,
                        json!({"result":{"data":data,"nextCursor":next,"providerErrors":errors}}),
                    )
                    .await
                    .unwrap();
            }
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                client.models(agent_protocol::session::ProviderKind::Codex),
                serving
            )
        })
        .await
        .unwrap();
        let result = result.unwrap();
        assert_eq!(result.data, expected);
        assert_eq!(result.next_cursor, None);
        assert_eq!(
            result.provider_errors,
            json!({"codex":"latest"}).as_object().cloned()
        );
        client.close().await;
    }
}

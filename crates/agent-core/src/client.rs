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
) -> Result<agent_protocol::sync::Cursor, PeerError> {
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
) -> Result<Option<(u64, crate::models::Item)>, PeerError> {
    let Some(grant) = response.transfer else {
        return Ok(None);
    };
    // Consume one-use grants even when the requesting subscription became obsolete.
    let resolved = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        crate::transfers::resolve_item(
            agent_protocol::operations::ItemResponse {
                item: crate::models::Item::new(
                    expected.clone(),
                    Default::default(),
                    crate::models::ItemBody::Sleep {},
                ),
                transfer: Some(grant),
            },
            session,
        ),
    )
    .await
    .unwrap_or_else(|_| {
        Err(PeerError::RequestTimeout {
            method: "item body transfer".into(),
        })
    })?;
    if resolved.item.id != *expected || resolved.item.is_deferred() {
        return Err(PeerError::InvalidMessage("hydration item changed".into()));
    }
    let version = response
        .version
        .ok_or_else(|| PeerError::InvalidMessage("hydration body version is missing".into()))?;
    Ok(Some((version, resolved.item)))
}

async fn gallery_frame(
    replica: &mut agent_protocol::conversation::records::Replica,
    stream: &mut agent_transport::framing::Reader,
) -> Result<(), PeerError> {
    let message = stream
        .read::<agent_protocol::conversation::records::Message>()
        .await
        .map_err(|error| PeerError::ConnectionClosed(error.to_string()))?
        .ok_or_else(|| PeerError::ConnectionClosed("gallery subscription ended".into()))?;
    agent_protocol::conversation::records::validate_message(&message)
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    *replica = replica
        .apply(1, message)
        .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    Ok(())
}

// Keep consuming the one state stream while an RPC or large transfer is in
// flight; both otherwise share the connection's finite queued-byte budget.
async fn gallery_request<T>(
    replica: &mut agent_protocol::conversation::records::Replica,
    stream: &mut agent_transport::framing::Reader,
    request: impl std::future::Future<Output = Result<T, PeerError>>,
) -> Result<T, PeerError> {
    tokio::pin!(request);
    loop {
        tokio::select! {
            result = &mut request => return result,
            result = gallery_frame(replica, stream) => result?,
        }
    }
}

async fn gallery_item(
    client: &Client,
    request: agent_protocol::conversation::records::Hydrate,
    replica: &mut agent_protocol::conversation::records::Replica,
    stream: &mut agent_transport::framing::Reader,
    session: Option<&crate::transport::Session>,
) -> Result<std::sync::Arc<crate::models::Item>, PeerError> {
    use agent_protocol::conversation::records;
    let response = gallery_request(replica, stream, client.call(&request)).await?;
    let cursor = response.cursor;
    let version = response.version;
    let transferred = response.transfer.is_some();
    let body = gallery_request(
        replica,
        stream,
        resolve_hydration(response, &request.item, session),
    )
    .await?;
    let key = agent_protocol::sync::Key::Item {
        thread: request.session.clone(),
        id: request.item.clone(),
    };
    loop {
        let caught_up = replica.cursor().is_some_and(|current| {
            current.store == cursor.store && current.sequence >= cursor.sequence
        });
        let ready = replica.rows().get(&key).is_none_or(|row| {
                                let (agent_protocol::sync::Content::Full(value) | agent_protocol::sync::Content::Summary(value)) = &row.content;
                                transferred || Some(row.version) != version || matches!(value.as_ref(), records::Record::Item(item) if !item.content.is_deferred())
                            });
        if caught_up && ready {
            break;
        }
        gallery_frame(replica, stream).await?;
    }
    if let Some((version, item)) = body
        && let Some(row) = records::hydrate_body(replica, version, item)
    {
        *replica = replica
            .apply(
                1,
                records::Message::Hydrate {
                    cursor: replica.cursor().expect("gallery cursor"),
                    row,
                },
            )
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
    }
    let row = replica
        .rows()
        .get(&agent_protocol::sync::Key::Item {
            thread: request.session.clone(),
            id: request.item.clone(),
        })
        .ok_or_else(|| PeerError::InvalidMessage("gallery item is no longer available".into()))?;
    let value = match &row.content {
        agent_protocol::sync::Content::Full(value)
        | agent_protocol::sync::Content::Summary(value) => value,
    };
    let records::Record::Item(value) = value.as_ref() else {
        return Err(PeerError::InvalidMessage(
            "gallery hydration is not an item".into(),
        ));
    };
    if value.content.is_deferred() {
        return Err(PeerError::InvalidMessage(
            "gallery item changed while reading its body".into(),
        ));
    }
    Ok(value.content.clone())
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
        session: Option<&crate::transport::Session>,
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
        session: Option<&crate::transport::Session>,
    ) -> Result<Vec<SessionImage>, PeerError> {
        // Gallery reads follow bounded history pages independently of the
        // Store's visible window, retaining their subscription until paging ends.
        use agent_protocol::{conversation::records, sync::Scope};
        let subscription = uuid::Uuid::new_v4();
        let (opened, mut stream) = self
            .request_stream::<records::Opened>(&crate::protocol::Call::SubscribeConversation(
                records::Subscribe {
                    session: thread_id.clone(),
                    subscription,
                    cached: None,
                    window: None,
                },
            ))
            .await?;
        if &opened.session != thread_id {
            return Err(PeerError::InvalidMessage(
                "gallery conversation identity changed".into(),
            ));
        }
        records::validate_message(&opened.initial)
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
        let mut replica = records::Replica::new(Scope::Conversation(thread_id.clone()), 1)
            .apply(1, opened.initial)
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
        loop {
            let previous_window = replica.window();
            if replica
                .window()
                .is_none_or(|window| window.turn == 0 && window.item == 0)
            {
                break;
            }
            gallery_request(
                &mut replica,
                &mut stream,
                self.call(&records::Extend {
                    session: thread_id.clone(),
                    subscription,
                }),
            )
            .await?;
            while replica.window().map(|window| (window.turn, window.item))
                >= previous_window.map(|window| (window.turn, window.item))
            {
                gallery_frame(&mut replica, &mut stream).await?;
            }
        }
        let thread = records::project(&replica)
            .ok_or_else(|| PeerError::InvalidMessage("gallery conversation disappeared".into()))?
            .thread;
        let mut images = Vec::new();
        let mut sources = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for turn in thread.turns.as_deref().unwrap_or_default().iter().rev() {
            for item in turn.items.as_deref().unwrap_or_default().iter().rev() {
                if !matches!(item.body(), crate::models::ItemBody::ImageGeneration { .. }) {
                    continue;
                }
                let detail;
                let item = if item.is_deferred() {
                    detail = gallery_item(
                        self,
                        records::Hydrate {
                            session: thread_id.clone(),
                            subscription,
                            item: item.id.clone(),
                        },
                        &mut replica,
                        &mut stream,
                        session,
                    )
                    .await?;
                    detail.as_ref()
                } else {
                    item.as_ref()
                };
                let crate::models::ItemBody::ImageGeneration {
                    saved_path, data, ..
                } = item.body()
                else {
                    continue;
                };
                let image = saved_path
                    .as_deref()
                    .filter(|path| !path.trim().is_empty())
                    .map(|path| (path, false))
                    .or_else(|| {
                        data.as_deref()
                            .filter(|data| !data.trim().is_empty())
                            .map(|data| (data, true))
                    });
                if let Some((source, encoded)) = image
                    && sources.insert(source.to_owned())
                {
                    bytes = bytes.saturating_add(source.len());
                    if bytes > 16 * 1024 * 1024 {
                        return Err(PeerError::InvalidMessage("画像一覧が16 MiBの上限に達しました。会話内の画像から個別に開いてください。".into()));
                    }
                    images.push(SessionImage {
                        source: source.into(),
                        encoded,
                    });
                }
            }
        }
        if thread.history_has_more == Some(true)
            || thread
                .history_read_state
                .as_ref()
                .is_some_and(|state| state.kind != crate::session::HistoryReadKind::Complete)
        {
            return Err(PeerError::InvalidMessage("履歴が部分取得のため、画像一覧を完全には取得できません。会話内の画像から個別に開いてください。".into()));
        }
        images.reverse();
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

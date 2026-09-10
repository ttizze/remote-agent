//! Typed RPC operations. This module has no transport or UI dependencies.
use crate::state::operations::{ReadOlder, ReadThread};
use crate::{
    models::ThreadResponse,
    peer::{PeerError, Reply, Request, RpcPeer},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use std::sync::Arc;

/// The method, parameters and result are one contract.
pub trait RpcMethod: Serialize {
    type Output: DeserializeOwned + Serialize;
    const METHOD: &'static str;
    fn method(&self) -> &'static str {
        Self::METHOD
    }
    /// Serialize the RPC parameters without local Store policy fields.
    fn serialize_params<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.serialize(serializer)
    }
    fn validate(&self, _output: &Self::Output) -> Result<(), &'static str> {
        Ok(())
    }
}
struct Params<'a, O>(&'a O);
impl<O: RpcMethod> Serialize for Params<'_, O> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize_params(serializer)
    }
}

#[derive(Serialize)]
pub struct Pair {
    pub invitation: uuid::Uuid,
}
impl RpcMethod for Pair {
    type Output = Map<String, Value>;
    const METHOD: &'static str = "host/pair";
}

#[derive(Debug, Serialize)]
pub struct ReadHostStatus {}
impl RpcMethod for ReadHostStatus {
    type Output = crate::models::HostStatus;
    const METHOD: &'static str = "host/status";
}

#[derive(Debug, Serialize)]
pub struct ListRemoteHosts {}
impl RpcMethod for ListRemoteHosts {
    type Output = Vec<crate::models::RemoteHost>;
    const METHOD: &'static str = "host/listRemotes";
}
#[derive(Debug, Serialize)]
pub struct RegisterRemoteHost<'a> {
    pub ticket: &'a str,
    pub name: &'a str,
}
impl RpcMethod for RegisterRemoteHost<'_> {
    type Output = crate::models::RemoteHost;
    const METHOD: &'static str = "host/registerRemote";
}

pub struct Client {
    peer: Arc<RpcPeer>,
}
impl Client {
    pub fn new(peer: Arc<RpcPeer>) -> Self {
        Self { peer }
    }
    pub fn call<'a, O: RpcMethod>(
        &'a self,
        operation: &'a O,
    ) -> Request<impl std::future::Future<Output = Result<Reply<O::Output>, PeerError>> + use<'a, O>>
    {
        let request = self.peer.request(operation.method(), &Params(operation));
        Request {
            wire_id: request.wire_id(),
            response: async move {
                let reply = request.await?;
                operation
                    .validate(&reply.value)
                    .map_err(|reason| PeerError::InvalidResponse {
                        method: operation.method().into(),
                        reason: reason.into(),
                        sequence: Some(reply.sequence),
                        raw: serde_json::to_string(&reply.value).expect("wire output serializes"),
                    })?;
                Ok(reply)
            },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Input<'a> {
    Text {
        text: &'a str,
        text_elements: &'a [Value],
    },
    LocalImage {
        path: &'a str,
    },
    Mention {
        path: &'a str,
        name: &'a str,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartTurn<'a> {
    pub thread_id: &'a str,
    pub client_user_message_id: &'a str,
    pub input: &'a [Input<'a>],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<&'a str>,
    #[serde(rename = "serviceTierForTurn", skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<&'a str>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StartedTurn {
    Turn {
        turn: TurnIdentity,
    },
    Id {
        #[serde(rename = "turnId")]
        turn_id: String,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub struct TurnIdentity {
    pub id: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl RpcMethod for StartTurn<'_> {
    type Output = StartedTurn;
    const METHOD: &'static str = "turn/start";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        let id = match output {
            StartedTurn::Turn { turn } => &turn.id,
            StartedTurn::Id { turn_id } => turn_id,
        };
        if id.trim().is_empty() {
            Err("turn ID is missing")
        } else {
            Ok(())
        }
    }
}

pub(crate) fn validate_thread(
    output: &ThreadResponse,
    expected: Option<&str>,
) -> Result<(), &'static str> {
    let id = output
        .thread
        .id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or("thread ID is missing")?;
    if expected.is_some_and(|expected| id != expected) {
        Err("thread ID does not match")
    } else {
        Ok(())
    }
}

macro_rules! operation {
    ($name:ident, $output:ty, $method:literal) => {
        impl RpcMethod for $name<'_> {
            type Output = $output;
            const METHOD: &'static str = $method;
        }
    };
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ItemResponse {
    pub item: crate::models::Item,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeThread<'a> {
    pub thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<&'a str>,
}
operation!(ResumeThread, Map<String, Value>, "thread/resume");
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerTurn<'a> {
    pub thread_id: &'a str,
    pub client_user_message_id: &'a str,
    pub input: &'a [Input<'a>],
    pub expected_turn_id: &'a str,
}
operation!(SteerTurn, Map<String, Value>, "turn/steer");
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueTurn<'a> {
    pub thread_id: &'a str,
    pub client_user_message_id: &'a str,
    pub input: &'a [Input<'a>],
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedTurn {
    pub queued_submission: TurnIdentity,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl RpcMethod for QueueTurn<'_> {
    type Output = QueuedTurn;
    const METHOD: &'static str = "thread/queue/add";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.queued_submission.id.trim().is_empty() {
            Err("queued submission ID is missing")
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ListModels<'a> {
    pub limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPage {
    pub data: Vec<crate::models::Model>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
operation!(ListModels, ModelPage, "model/list");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcribe<T = String> {
    pub audio: T,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Transcription {
    pub text: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl<T: Serialize> RpcMethod for Transcribe<T> {
    type Output = Transcription;
    const METHOD: &'static str = "host/dictation/transcribe";
    fn validate(&self, output: &Self::Output) -> Result<(), &'static str> {
        if output.text.trim().is_empty() {
            Err("transcription is blank")
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Serialize)]
pub struct WriteFile<'a> {
    pub path: &'a str,
    pub revision: &'a str,
    pub text: &'a str,
}
operation!(WriteFile, crate::models::FileContent, "host/file/write");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Account {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct Accounts {
    pub accounts: Vec<Account>,
    pub selected_id: Option<String>,
    pub error: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelection {
    pub selected_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence_error: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AccountLogin {
    pub login_id: String,
    pub user_code: String,
    pub verification_url: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct AccountLoginStatus {
    pub completed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, PartialEq)]
pub enum SubmissionTarget<'a> {
    Steer(&'a str),
    Queue,
    Start { cwd: &'a str, resume: bool },
}
pub fn submission_target<'a>(
    snapshot: Option<&'a crate::models::Thread>,
    listed: Option<&'a crate::models::Thread>,
    active: Option<bool>,
) -> Result<SubmissionTarget<'a>, PeerError> {
    if let Some(turn) = snapshot
        .and_then(|thread| thread.turns.as_ref())
        .and_then(|turns| {
            turns.iter().rev().find(|turn| {
                turn.status.as_deref() == Some("inProgress") && !turn.id.trim().is_empty()
            })
        })
    {
        return Ok(SubmissionTarget::Steer(&turn.id));
    }
    if active.unwrap_or_else(|| {
        [snapshot, listed].into_iter().flatten().any(|thread| {
            thread
                .status
                .as_ref()
                .is_some_and(|status| status.kind == "active")
        })
    }) {
        return Ok(SubmissionTarget::Queue);
    }
    let cwd = [snapshot, listed]
        .into_iter()
        .flatten()
        .filter_map(|thread| thread.cwd.as_deref())
        .find(|cwd| !cwd.trim().is_empty())
        .ok_or_else(|| PeerError::InvalidMessage("thread working directory is unknown".into()))?;
    let resume = snapshot.is_none_or(|thread| {
        thread
            .status
            .as_ref()
            .is_some_and(|status| status.kind == "notLoaded")
    });
    Ok(SubmissionTarget::Start { cwd, resume })
}

pub struct Submission<'a> {
    pub thread_id: &'a str,
    pub client_user_message_id: &'a str,
    pub input: &'a [Input<'a>],
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub service_tier: Option<&'a str>,
}
impl Client {
    pub async fn submit(
        &self,
        submission: &Submission<'_>,
        target: SubmissionTarget<'_>,
    ) -> Result<Reply<Option<String>>, PeerError> {
        let Submission {
            thread_id,
            client_user_message_id,
            input,
            model,
            effort,
            service_tier,
        } = *submission;
        match target {
            SubmissionTarget::Steer(turn_id) => {
                let reply = self
                    .call(&SteerTurn {
                        thread_id,
                        client_user_message_id,
                        input,
                        expected_turn_id: turn_id,
                    })
                    .await?;
                Ok(Reply {
                    sequence: reply.sequence,
                    value: Some(turn_id.into()),
                })
            }
            SubmissionTarget::Queue => {
                let reply = self
                    .call(&QueueTurn {
                        thread_id,
                        client_user_message_id,
                        input,
                    })
                    .await?;
                Ok(Reply {
                    sequence: reply.sequence,
                    value: None,
                })
            }
            SubmissionTarget::Start { cwd, resume } => {
                if resume {
                    self.call(&ResumeThread {
                        thread_id,
                        cwd: Some(cwd),
                    })
                    .await?;
                }
                let reply = self
                    .call(&StartTurn {
                        thread_id,
                        client_user_message_id,
                        input,
                        model,
                        effort,
                        service_tier,
                    })
                    .await?;
                let id = match reply.value {
                    StartedTurn::Turn { turn } => turn.id,
                    StartedTurn::Id { turn_id } => turn_id,
                };
                Ok(Reply {
                    sequence: reply.sequence,
                    value: Some(id),
                })
            }
        }
    }

    pub async fn models(&self) -> Result<Vec<crate::models::Model>, PeerError> {
        let mut models: Vec<crate::models::Model> = Vec::new();
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let page = self
                .call(&ListModels {
                    limit: 100,
                    cursor: cursor.as_deref(),
                })
                .await?
                .value;
            for model in page.data {
                if let Some(index) = models.iter().position(|previous| previous.id == model.id) {
                    models[index] = model;
                } else {
                    models.push(model);
                }
            }
            cursor = page.next_cursor;
            let Some(next) = &cursor else {
                return Ok(models);
            };
            if !seen.insert(next.clone()) {
                return Err(PeerError::InvalidMessage(
                    "model cursor did not advance".into(),
                ));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerRequest {
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Map<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Enum))]
pub enum Answer {
    Decision {
        index: u32,
    },
    Permissions {
        allow: bool,
    },
    Questions {
        answers: std::collections::BTreeMap<String, String>,
    },
    Raw {
        value: Value,
    },
}
/// The UI and response validator use the same ordered choices.
pub fn approval_decisions(request: &ServerRequest) -> &[Value] {
    static DEFAULTS: std::sync::LazyLock<[Value; 4]> = std::sync::LazyLock::new(|| {
        ["accept", "acceptForSession", "decline", "cancel"].map(|value| Value::String(value.into()))
    });
    request
        .params
        .get("availableDecisions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(DEFAULTS.as_slice())
}
/// Validates a response before any bytes are queued. Unknown payloads remain intact.
pub fn answer_result(request: &ServerRequest, answer: &Answer) -> Result<Value, PeerError> {
    use serde_json::json;
    match answer {
        Answer::Decision { index } => {
            let choices = approval_decisions(request);
            let decision = choices
                .get(*index as usize)
                .ok_or_else(|| PeerError::InvalidMessage("invalid approval choice".into()))?;
            Ok(json!({"decision":decision}))
        }
        Answer::Permissions { allow } => {
            let permissions = if *allow {
                request
                    .params
                    .get("permissions")
                    .filter(|value| value.is_object())
                    .cloned()
                    .ok_or_else(|| {
                        PeerError::InvalidMessage("permissions object is missing".into())
                    })?
            } else {
                json!({})
            };
            Ok(json!({"permissions":permissions,"scope":"turn"}))
        }
        Answer::Questions { answers } => {
            #[derive(Deserialize)]
            struct Question {
                id: String,
            }
            let questions: Vec<Question> = serde_json::from_value(
                request
                    .params
                    .get("questions")
                    .cloned()
                    .unwrap_or(Value::Null),
            )
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            let mut result = Map::new();
            for question in questions {
                let text = answers
                    .get(&question.id)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or_else(|| {
                        PeerError::InvalidMessage("every question requires an answer".into())
                    })?;
                result.insert(question.id, json!({"answers":[text]}));
            }
            Ok(json!({"answers":result}))
        }
        Answer::Raw { value } => Ok(value.clone()),
    }
}
impl Client {
    pub async fn respond(&self, request: &ServerRequest, answer: &Answer) -> Result<(), PeerError> {
        let result = answer_result(request, answer)?;
        self.peer
            .respond_raw(&request.id.to_string(), "result", &result.to_string())
            .await
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SessionImage {
    pub source: String,
    pub encoded: bool,
}
impl Client {
    pub async fn session_images(&self, thread_id: &str) -> Result<Vec<SessionImage>, PeerError> {
        let mut thread = self
            .call(&ReadThread::new(thread_id.to_owned()))
            .await?
            .value
            .thread;
        let mut visited = std::collections::HashSet::new();
        loop {
            let item_page = thread
                .turns
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|turn| turn.items_has_more == Some(true))
                .map(|turn| {
                    (
                        turn.id.clone(),
                        turn.items_next_cursor
                            .as_ref()
                            .and_then(|cursor| cursor.clone()),
                    )
                });
            let (turn_id, cursor) = if let Some((turn_id, cursor)) = item_page {
                (Some(turn_id), cursor)
            } else if let Some(Some(cursor)) = &thread.history_cursor {
                (None, Some(cursor.clone()))
            } else {
                break;
            };
            if !visited.insert((turn_id.clone(), cursor.clone())) {
                return Err(PeerError::InvalidMessage(
                    "history cursor did not advance".into(),
                ));
            }
            let operation = ReadOlder::new(thread_id.to_owned(), turn_id, cursor);
            let page = self.call(&operation).await?.value.thread;
            thread = crate::state::older(
                &thread,
                &page,
                operation.turn_id.as_deref(),
                operation.cursor.as_deref(),
            )
            .map_err(PeerError::InvalidMessage)?;
        }
        let mut images = Vec::new();
        let mut sources = std::collections::HashSet::new();
        for item in thread
            .turns
            .as_deref()
            .unwrap_or_default()
            .iter()
            .rev()
            .flat_map(|turn| turn.items.as_deref().unwrap_or_default().iter().rev())
        {
            if item.kind.as_deref() != Some("imageGeneration") {
                continue;
            }
            let image = item
                .saved_path
                .as_deref()
                .filter(|path| !path.trim().is_empty())
                .map(|path| (path, false))
                .or_else(|| {
                    item.result
                        .as_ref()
                        .and_then(Value::as_str)
                        .filter(|data| !data.trim().is_empty())
                        .map(|data| (data, true))
                });
            if let Some((source, encoded)) = image
                && sources.insert(source)
            {
                images.push(SessionImage {
                    source: source.into(),
                    encoded,
                });
            }
        }
        images.reverse();
        Ok(images)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

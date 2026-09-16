//! Typed RPC operations and deferred payload reads. No UI dependencies.
use crate::state::operations::ReadThread;
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
macro_rules! rpc_method {
    ($name:ty, $output:ty, $method:expr) => {
        impl $crate::client::RpcMethod for $name {
            type Output = $output;
            const METHOD: &'static str = $method;
        }
    };
}
pub(crate) use rpc_method;

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
rpc_method!(Pair, Map<String, Value>, "host/pair");

#[derive(Debug, Serialize)]
pub struct ReadHostStatus {}
rpc_method!(ReadHostStatus, crate::models::HostStatus, "host/status");

#[derive(Debug, Serialize)]
pub struct ListRemoteHosts {}
rpc_method!(
    ListRemoteHosts,
    Vec<crate::models::RemoteHost>,
    "host/listRemotes"
);
#[derive(Debug, Serialize)]
pub struct RegisterRemoteHost<'a> {
    pub ticket: &'a str,
    pub name: &'a str,
}
rpc_method!(
    RegisterRemoteHost<'_>,
    crate::models::RemoteHost,
    "host/registerRemote"
);

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemResponse {
    pub item: crate::models::Item,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transfer: Option<crate::models::TransferGrant>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl ItemResponse {
    pub async fn resolve(
        mut self,
        session: Option<&crate::transport::Session>,
    ) -> Result<Self, PeerError> {
        if let Some(grant) = self.transfer.take() {
            let session = session.ok_or_else(|| {
                PeerError::InvalidMessage("item transfer requires an iroh session".into())
            })?;
            let bytes = crate::transfers::download_bytes(grant, || async {
                session.open_stream().await.map_err(std::io::Error::other)
            })
            .await
            .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            let item: crate::models::Item = serde_json::from_slice(&bytes)
                .map_err(|error| PeerError::InvalidMessage(error.to_string()))?;
            if item.id != self.item.id {
                return Err(PeerError::InvalidMessage(
                    "transferred item ID does not match".into(),
                ));
            }
            self.item = item;
        }
        Ok(self)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResumeThread<'a> {
    pub thread_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<&'a str>,
}
rpc_method!(ResumeThread<'_>, Map<String, Value>, "thread/resume");
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerTurn<'a> {
    pub thread_id: &'a str,
    pub client_user_message_id: &'a str,
    pub input: &'a [Input<'a>],
    pub expected_turn_id: &'a str,
}
rpc_method!(SteerTurn<'_>, Map<String, Value>, "turn/steer");
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
rpc_method!(ListModels<'_>, ModelPage, "model/list");

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
}

#[derive(Debug, Serialize)]
pub struct WriteFile<'a> {
    pub path: &'a str,
    pub revision: &'a str,
    pub text: &'a str,
}
rpc_method!(WriteFile<'_>, crate::models::FileContent, "host/file/write");

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
                .is_some_and(|status| status.kind == crate::models::ThreadStatusKind::Active)
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
            .is_some_and(|status| status.kind == crate::models::ThreadStatusKind::NotLoaded)
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

    pub async fn models(&self) -> Result<ModelPage, PeerError> {
        let mut extra = Map::<String, Value>::new();
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
            for (key, value) in page.extra {
                if key == "providerErrors" {
                    let Value::Object(errors) = value else {
                        return Err(PeerError::InvalidMessage("invalid provider errors".into()));
                    };
                    extra
                        .entry(key)
                        .or_insert_with(|| serde_json::json!({}))
                        .as_object_mut()
                        .expect("provider errors are an object")
                        .extend(errors);
                } else {
                    extra.insert(key, value);
                }
            }
            for model in page.data {
                if let Some(index) = models.iter().position(|previous| previous.id == model.id) {
                    models[index] = model;
                } else {
                    models.push(model);
                }
            }
            cursor = page.next_cursor;
            let Some(next) = &cursor else {
                return Ok(ModelPage {
                    data: models,
                    next_cursor: None,
                    extra,
                });
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
    #[serde(
        default,
        rename = "deliveryState",
        skip_serializing_if = "Option::is_none"
    )]
    pub delivery_state: Option<crate::session::RequestDelivery>,
    #[serde(
        default,
        rename = "nativeRequestId",
        skip_serializing_if = "Option::is_none"
    )]
    pub native_request_id: Option<Value>,
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
/// Validate the complete wire answer before claiming a shared pending request.
pub fn validate_answer(request: &ServerRequest, result: &Value) -> Result<(), String> {
    if request.method == "item/tool/requestUserInput" {
        let questions = request
            .params
            .get("questions")
            .and_then(Value::as_array)
            .ok_or("questions are unavailable")?;
        let answers = result["answers"]
            .as_object()
            .ok_or("answers are required")?;
        if answers.len() != questions.len() {
            return Err("every question requires one answer".into());
        }
        for question in questions {
            let id = question["id"].as_str().ok_or("question ID is missing")?;
            let values = answers
                .get(id)
                .and_then(|answer| answer["answers"].as_array())
                .ok_or("question answer is missing")?;
            if values.is_empty()
                || values
                    .iter()
                    .any(|value| value.as_str().is_none_or(|value| value.trim().is_empty()))
            {
                return Err("question answer is invalid".into());
            }
        }
    } else if request.method == "item/permissions/requestApproval" {
        let requested = request
            .params
            .get("permissions")
            .and_then(Value::as_object)
            .ok_or("permissions are unavailable")?;
        let granted = result["permissions"]
            .as_object()
            .ok_or("permissions are required")?;
        if result["scope"] != "turn"
            || granted
                .iter()
                .any(|(key, value)| requested.get(key) != Some(value))
        {
            return Err("answer grants unrequested permissions".into());
        }
    } else if matches!(
        request.method.as_str(),
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "claude/tool/requestApproval"
    ) {
        if !approval_decisions(request).contains(&result["decision"]) {
            return Err("invalid approval decision".into());
        }
    } else if request.method == "mcpServer/elicitation/request" {
        match result["action"].as_str() {
            Some("decline" | "cancel") if result["content"].is_null() => {}
            Some("accept")
                if matches!(
                    request.params.get("mode").and_then(Value::as_str),
                    Some("form" | "openai/form")
                ) && result["content"].is_object() => {}
            Some("accept")
                if request.params.get("mode").and_then(Value::as_str) == Some("url")
                    && result["content"].is_null() => {}
            _ => return Err("invalid MCP elicitation answer".into()),
        }
    } else if request.method == "item/tool/call" {
        if !result["success"].is_boolean()
            || result["contentItems"].as_array().is_none_or(|items| {
                items.iter().any(|item| match item["type"].as_str() {
                    Some("inputText") => !item["text"].is_string(),
                    Some("inputImage") => item["imageUrl"]
                        .as_str()
                        .is_none_or(|url| !url.starts_with("data:image/")),
                    _ => true,
                })
            })
        {
            return Err("invalid dynamic tool response".into());
        }
    } else {
        return Err("unsupported provider request cannot be approved".into());
    }
    Ok(())
}

impl Client {
    pub async fn respond(&self, request: &ServerRequest, answer: &Answer) -> Result<(), PeerError> {
        let result = answer_result(request, answer)?;
        validate_answer(request, &result).map_err(PeerError::InvalidMessage)?;
        self.peer
            .request::<_, Value>(
                "host/session/answer",
                &serde_json::json!({"requestId":request.id,"result":result}),
            )
            .await
            .map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct SessionImage {
    pub source: String,
    pub encoded: bool,
}
impl Client {
    pub async fn session_images(
        &self,
        thread_id: &str,
        session: Option<&crate::transport::Session>,
    ) -> Result<Vec<SessionImage>, PeerError> {
        // One bounded provider view; close this transient subscription before
        // returning images so it never replaces the Store's visible session.
        let opened = self
            .call(&ReadThread {
                limit: 1000,
                ..ReadThread::new(thread_id.to_owned())
            })
            .await?
            .value;
        let thread = opened.response.thread;
        self.call(&crate::state::operations::CloseSubscription {
            subscription_id: opened.subscription_id.to_string(),
        })
        .await?;
        let mut images = Vec::new();
        let mut sources = std::collections::HashSet::new();
        let mut native_items = std::collections::HashSet::new();
        let mut bytes = 0usize;
        for turn in thread.turns.as_deref().unwrap_or_default().iter().rev() {
            for item in turn.items.as_deref().unwrap_or_default().iter().rev() {
                if item.kind.as_deref() != Some("imageGeneration") || !native_items.insert(&item.id)
                {
                    continue;
                }
                let detail;
                let item = if turn
                    .deferred_item_ids
                    .as_ref()
                    .is_some_and(|ids| ids.contains(&item.id))
                {
                    detail = self
                        .call(&crate::state::operations::ReadItem {
                            thread_id: thread_id.into(),
                            turn_id: turn.id.clone(),
                            item_id: item.id.clone(),
                        })
                        .await?
                        .value
                        .resolve(session)
                        .await?;
                    &detail.item
                } else {
                    item.as_ref()
                };
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
        if thread.extra.get("historyHasMore") == Some(&Value::Bool(true))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
}

// Shared Host/Client request records; Store behavior lives in state::operations.
#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListThreads {
    #[serde(flatten)]
    pub query: crate::models::ListQuery,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadItem {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub request_id: Value,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartTerminal {
    #[serde(rename = "processHandle")]
    pub handle: String,
    pub cwd: String,
    pub size: TerminalSize,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewWorkspace {
    pub cwd: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoveWorktree {
    pub path: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectAccount {
    #[serde(rename = "accountId")]
    pub id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogoutAccount {
    #[serde(rename = "accountId")]
    pub id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAccountLogin {
    #[serde(rename = "loginId")]
    pub id: String,
}

#[cfg_attr(feature = "bindings", derive(uniffi::Record))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadVisualization {
    pub path: String,
    pub cwd: String,
}
